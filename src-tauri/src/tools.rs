//! Locate / download / update yt-dlp and ffmpeg in `%LOCALAPPDATA%\YTDownloader\bin`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const FFMPEG_URL: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";
/// yt-dlp needs a JS runtime to solve YouTube's player challenges; without it
/// many videos (e.g. "made for kids") report "This video is not available".
const DENO_URL: &str = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static ENSURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize, Clone, Debug)]
pub struct SetupProgress {
    pub stage: String,
    pub percent: Option<f64>,
    pub message: String,
    pub done: bool,
}

pub fn bin_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("YTDownloader")
        .join("bin")
}

pub fn ytdlp_path() -> PathBuf {
    bin_dir().join("yt-dlp.exe")
}

pub fn ffmpeg_path() -> PathBuf {
    bin_dir().join("ffmpeg.exe")
}

pub fn deno_path() -> PathBuf {
    bin_dir().join("deno.exe")
}

pub fn is_ready() -> bool {
    ytdlp_path().exists()
        && ffmpeg_path().exists()
        && bin_dir().join("ffprobe.exe").exists()
        && deno_path().exists()
}

/// A hidden-window command with UTF-8 forced for Python's stdio.
pub fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .env("PYTHONIOENCODING", "utf-8")
        .kill_on_drop(true);
    cmd
}

async fn download_file<F>(url: &str, dest: &Path, stage: &str, emit: &F) -> Result<()>
where
    F: Fn(SetupProgress),
{
    let client = reqwest::Client::builder()
        .user_agent("yt-downloader")
        .build()?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("request failed: {url}"))?
        .error_for_status()?;
    let total = resp.content_length();
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = resp.bytes_stream();
    let mut got: u64 = 0;
    let mut last_pct: i64 = -1;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        got += chunk.len() as u64;
        let pct = total.map(|t| got as f64 * 100.0 / t as f64);
        let key = pct.map(|p| p as i64).unwrap_or(got as i64 / 1_000_000);
        if key != last_pct {
            last_pct = key;
            emit(SetupProgress {
                stage: stage.into(),
                percent: pct,
                message: format!("Downloading {stage}… {:.1} MB", got as f64 / 1_048_576.0),
                done: false,
            });
        }
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

fn extract_ffmpeg(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut found = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        let target = if name.ends_with("/bin/ffmpeg.exe") || name == "ffmpeg.exe" {
            "ffmpeg.exe"
        } else if name.ends_with("/bin/ffprobe.exe") || name == "ffprobe.exe" {
            "ffprobe.exe"
        } else {
            continue;
        };
        let out_path = dest_dir.join(target);
        let tmp = out_path.with_extension("exe.part");
        let mut out = std::fs::File::create(&tmp)?;
        std::io::copy(&mut entry, &mut out)?;
        drop(out);
        std::fs::rename(&tmp, &out_path)?;
        found += 1;
    }
    if found < 2 {
        return Err(anyhow!("ffmpeg archive did not contain ffmpeg.exe and ffprobe.exe"));
    }
    Ok(())
}

fn extract_deno(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entry = archive
        .by_name("deno.exe")
        .map_err(|_| anyhow!("deno archive did not contain deno.exe"))?;
    let tmp = dest.with_extension("exe.part");
    let mut out = std::fs::File::create(&tmp)?;
    std::io::copy(&mut entry, &mut out)?;
    drop(out);
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Download whatever is missing. Safe to call repeatedly / concurrently.
pub async fn ensure_with<F>(emit: &F) -> Result<()>
where
    F: Fn(SetupProgress),
{
    let _guard = ENSURE_LOCK.lock().await;
    let dir = bin_dir();
    tokio::fs::create_dir_all(&dir).await?;

    if !ytdlp_path().exists() {
        download_file(YTDLP_URL, &ytdlp_path(), "yt-dlp", emit).await?;
    }
    if !ffmpeg_path().exists() || !dir.join("ffprobe.exe").exists() {
        let zip_path = dir.join("ffmpeg.zip");
        download_file(FFMPEG_URL, &zip_path, "ffmpeg", emit).await?;
        emit(SetupProgress {
            stage: "ffmpeg".into(),
            percent: None,
            message: "Extracting ffmpeg…".into(),
            done: false,
        });
        let zp = zip_path.clone();
        let d = dir.clone();
        tokio::task::spawn_blocking(move || extract_ffmpeg(&zp, &d)).await??;
        let _ = tokio::fs::remove_file(&zip_path).await;
    }
    if !deno_path().exists() {
        let zip_path = dir.join("deno.zip");
        download_file(DENO_URL, &zip_path, "deno", emit).await?;
        emit(SetupProgress {
            stage: "deno".into(),
            percent: None,
            message: "Extracting deno…".into(),
            done: false,
        });
        let zp = zip_path.clone();
        tokio::task::spawn_blocking(move || extract_deno(&zp, &deno_path())).await??;
        let _ = tokio::fs::remove_file(&zip_path).await;
    }
    emit(SetupProgress {
        stage: "ready".into(),
        percent: Some(100.0),
        message: "Tools ready".into(),
        done: true,
    });
    Ok(())
}

pub async fn ensure(app: &AppHandle) -> Result<()> {
    let app = app.clone();
    ensure_with(&move |p| {
        let _ = app.emit("setup-progress", p);
    })
    .await
}

pub async fn version() -> Option<String> {
    if !ytdlp_path().exists() {
        return None;
    }
    let out = hidden_command(ytdlp_path())
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run `yt-dlp -U` and return the new version.
pub async fn update_ytdlp(app: &AppHandle) -> Result<String> {
    ensure(app).await?;
    let out = hidden_command(ytdlp_path())
        .arg("-U")
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err
            .lines()
            .find(|l| l.contains("ERROR"))
            .unwrap_or("yt-dlp update failed")
            .to_string();
        return Err(anyhow!(msg));
    }
    version().await.ok_or_else(|| anyhow!("could not read yt-dlp version"))
}
