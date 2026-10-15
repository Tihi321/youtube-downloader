//! Job queue: builds yt-dlp argument lists, runs them one job at a time,
//! parses progress output and emits `job-progress` events.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    sync::{mpsc, Notify},
};

use crate::{
    tools,
    transcript::{self, Format},
};

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct JobRequest {
    pub url: String,
    pub video: bool,
    pub audio: bool,
    pub transcript: bool,
    pub video_quality: String,
    pub audio_format: String,
    pub transcript_format: String,
    pub lang: String,
    pub playlist: bool,
    pub out_dir: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct JobEvent {
    pub id: u64,
    pub url: String,
    pub index: Option<u32>,
    pub total: Option<u32>,
    pub title: Option<String>,
    pub percent: f64,
    pub speed: Option<String>,
    pub eta: Option<String>,
    /// queued | downloading | processing | done | error | cancelled
    pub status: String,
    pub message: Option<String>,
    pub pass: Option<String>,
    pub out_dir: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PassKind {
    Video,
    Audio,
    Transcript,
}

impl PassKind {
    fn label(self) -> &'static str {
        match self {
            PassKind::Video => "Video",
            PassKind::Audio => "Audio",
            PassKind::Transcript => "Transcript",
        }
    }
}

pub struct JobCtl {
    cancelled: AtomicBool,
    notify: Notify,
}

type Ctls = Arc<Mutex<HashMap<u64, Arc<JobCtl>>>>;

pub struct Queue {
    tx: mpsc::UnboundedSender<(u64, JobRequest, Arc<JobCtl>)>,
    ctls: Ctls,
    next_id: AtomicU64,
    app: AppHandle,
}

impl Queue {
    pub fn start(app: AppHandle) -> Queue {
        let (tx, mut rx) = mpsc::unbounded_channel::<(u64, JobRequest, Arc<JobCtl>)>();
        let ctls: Ctls = Arc::new(Mutex::new(HashMap::new()));
        let worker_app = app.clone();
        let worker_ctls = ctls.clone();
        tauri::async_runtime::spawn(async move {
            while let Some((id, req, ctl)) = rx.recv().await {
                run_job(&worker_app, id, req, ctl).await;
                if let Ok(mut m) = worker_ctls.lock() {
                    m.remove(&id);
                }
            }
        });
        Queue {
            tx,
            ctls,
            next_id: AtomicU64::new(1),
            app,
        }
    }

    pub fn enqueue(&self, req: JobRequest) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let ctl = Arc::new(JobCtl {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        });
        if let Ok(mut m) = self.ctls.lock() {
            m.insert(id, ctl.clone());
        }
        let mut rep = Reporter::new(self.app.clone(), id, &req);
        rep.ev.status = "queued".into();
        rep.emit();
        let _ = self.tx.send((id, req, ctl));
        id
    }

    pub fn cancel(&self, id: u64) {
        let ctl = self.ctls.lock().ok().and_then(|m| m.get(&id).cloned());
        if let Some(ctl) = ctl {
            ctl.cancelled.store(true, Ordering::SeqCst);
            ctl.notify.notify_one();
        }
    }
}

struct Reporter {
    app: AppHandle,
    ev: JobEvent,
}

impl Reporter {
    fn new(app: AppHandle, id: u64, req: &JobRequest) -> Self {
        Reporter {
            app,
            ev: JobEvent {
                id,
                url: req.url.clone(),
                index: None,
                total: None,
                title: None,
                percent: 0.0,
                speed: None,
                eta: None,
                status: "queued".into(),
                message: None,
                pass: None,
                out_dir: req.out_dir.clone(),
            },
        }
    }

    fn emit(&self) {
        let _ = self.app.emit("job-progress", &self.ev);
    }

    fn set(&mut self, status: &str, message: Option<String>) {
        self.ev.status = status.into();
        self.ev.message = message;
        self.emit();
    }
}

// ---------------------------------------------------------------------------
// Argument building
// ---------------------------------------------------------------------------

pub fn is_playlist(req: &JobRequest) -> bool {
    req.playlist && req.url.contains("list=")
}

fn passes(req: &JobRequest) -> Vec<PassKind> {
    let mut v = Vec::new();
    if req.video {
        v.push(PassKind::Video);
    }
    if req.audio {
        v.push(PassKind::Audio);
    }
    if req.transcript {
        v.push(PassKind::Transcript);
    }
    v
}

fn output_template(req: &JobRequest) -> String {
    let out = req.out_dir.trim_end_matches(['\\', '/']).replace('%', "%%");
    if is_playlist(req) {
        format!("{out}/%(playlist_title)s/%(playlist_index)03d - %(title)s.%(ext)s")
    } else {
        format!("{out}/%(title)s [%(id)s].%(ext)s")
    }
}

/// Build the full yt-dlp argument list for one pass. `tmp` is only used by
/// the transcript pass (info-json mapping files).
pub fn build_args(req: &JobRequest, kind: PassKind, tmp: &Path) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |s: &str| a.push(s.to_string());
    push("--newline");
    push("--no-colors");
    push("--progress");
    push("--encoding");
    push("utf-8");
    push("--windows-filenames");
    push("--concurrent-fragments");
    push("4");
    push("--ffmpeg-location");
    a.push(tools::bin_dir().to_string_lossy().into_owned());
    a.push("--js-runtimes".into());
    a.push(format!("deno:{}", tools::deno_path().to_string_lossy()));
    let mut push = |s: &str| a.push(s.to_string());
    if is_playlist(req) {
        push("--yes-playlist");
        push("--ignore-errors");
    } else {
        push("--no-playlist");
    }
    push("-o");
    a.push(output_template(req));
    let mut push = |s: &str| a.push(s.to_string());
    push("--progress-template");
    push("download:PROG|%(info.playlist_index)s|%(info.n_entries)s|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s");
    push("--print");
    push("before_dl:ITEM|%(playlist_index)s|%(n_entries)s|%(title)s");

    match kind {
        PassKind::Video => {
            let h = match req.video_quality.as_str() {
                "1080" | "720" | "480" => format!("[height<={}]", req.video_quality),
                _ => String::new(),
            };
            push("-f");
            a.push(format!(
                "bv*{h}[ext=mp4]+ba[ext=m4a]/bv*{h}+ba/b{h}"
            ));
            a.push("--merge-output-format".into());
            a.push("mp4".into());
        }
        PassKind::Audio => {
            if req.audio_format == "m4a" {
                for s in ["-f", "ba[ext=m4a]/ba", "-x", "--audio-format", "m4a"] {
                    a.push(s.into());
                }
            } else {
                for s in [
                    "-f",
                    "ba",
                    "-x",
                    "--audio-format",
                    "mp3",
                    "--audio-quality",
                    "0",
                    "--embed-thumbnail",
                    "--embed-metadata",
                ] {
                    a.push(s.into());
                }
            }
        }
        PassKind::Transcript => {
            let l = req.lang.trim();
            for s in ["--skip-download", "--write-subs", "--write-auto-subs"] {
                a.push(s.into());
            }
            a.push("--sub-langs".into());
            a.push(format!("{l},{l}-orig,{l}.*"));
            a.push("--sub-format".into());
            a.push("json3".into());
            a.push("--write-info-json".into());
            a.push("--no-write-playlist-metafiles".into());
            a.push("-o".into());
            let t = tmp.to_string_lossy().replace('\\', "/").replace('%', "%%");
            a.push(format!("infojson:{t}/%(playlist_index|0)03d.%(id)s.%(ext)s"));
        }
    }
    a.push(req.url.clone());
    a
}

// ---------------------------------------------------------------------------
// Transcript post-processing
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct TranscriptOutcome {
    pub title: String,
    pub written: Option<PathBuf>,
    pub warning: Option<String>,
}

fn pick_subtitle_key(subs: &serde_json::Map<String, serde_json::Value>, lang: &str) -> Option<String> {
    if subs.contains_key(lang) {
        return Some(lang.to_string());
    }
    let orig = format!("{lang}-orig");
    if subs.contains_key(&orig) {
        return Some(orig);
    }
    subs.keys()
        .find(|k| k.starts_with(lang))
        .or_else(|| subs.keys().next())
        .cloned()
}

/// Convert every downloaded json3 referenced from `*.info.json` files in `tmp`
/// into the chosen format, next to where yt-dlp wrote the json3 (the media
/// folder, same name stem). Removes the json3 + info files.
pub fn finish_transcripts(tmp: &Path, lang: &str, fmt: Format) -> Vec<TranscriptOutcome> {
    let mut outcomes = Vec::new();
    let mut infos: Vec<PathBuf> = std::fs::read_dir(tmp)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".info.json"))
                })
                .collect()
        })
        .unwrap_or_default();
    infos.sort();
    for info_path in infos {
        let info: serde_json::Value = match std::fs::read_to_string(&info_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(v) => v,
            None => continue,
        };
        let _ = std::fs::remove_file(&info_path);
        let title = info
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("(unknown)")
            .to_string();
        let subs = info
            .get("requested_subtitles")
            .and_then(|s| s.as_object())
            .filter(|m| !m.is_empty());
        let Some(subs) = subs else {
            outcomes.push(TranscriptOutcome {
                title,
                written: None,
                warning: Some(format!("No captions available for \"{lang}\"")),
            });
            continue;
        };
        let key = pick_subtitle_key(subs, lang).unwrap_or_default();
        // Convert the preferred one, delete the rest.
        let mut result = TranscriptOutcome {
            title: title.clone(),
            written: None,
            warning: Some("Caption file not found".into()),
        };
        for (k, v) in subs {
            let Some(fp) = v.get("filepath").and_then(|f| f.as_str()) else {
                continue;
            };
            let fp = PathBuf::from(fp);
            if *k == key {
                match convert_one(&fp, k, lang, fmt) {
                    Ok(p) => {
                        result.written = Some(p);
                        result.warning = None;
                    }
                    Err(e) => result.warning = Some(format!("Transcript failed: {e}")),
                }
            }
            let _ = std::fs::remove_file(&fp);
        }
        outcomes.push(result);
    }
    outcomes
}

fn convert_one(json3: &Path, key: &str, lang: &str, fmt: Format) -> anyhow::Result<PathBuf> {
    let raw = std::fs::read_to_string(json3)?;
    let segs = transcript::parse_json3(&raw)?;
    if segs.is_empty() {
        anyhow::bail!("caption track is empty");
    }
    let name = json3
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("transcript.json3");
    let suffix = format!(".{key}.json3");
    let stem = name.strip_suffix(&suffix).unwrap_or_else(|| name.trim_end_matches(".json3"));
    let out = json3.with_file_name(format!("{stem}.{lang}.{}", fmt.ext()));
    std::fs::write(&out, transcript::render(fmt, &segs))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Running yt-dlp
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PassResult {
    cancelled: bool,
    spawn_error: Option<String>,
    errors: Vec<String>,
    warnings: Vec<String>,
    items_seen: u32,
    success: bool,
}

async fn read_line_lossy<R: AsyncRead + Unpin>(r: &mut BufReader<R>, buf: &mut Vec<u8>) -> Option<String> {
    buf.clear();
    match r.read_until(b'\n', buf).await {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(String::from_utf8_lossy(buf).trim_end().to_string()),
    }
}

fn kill_tree(pid: u32) {
    let mut c = std::process::Command::new("taskkill");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let _ = c
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn parse_u32(s: &str) -> Option<u32> {
    s.trim().parse().ok()
}

fn handle_line(rep: &mut Reporter, res: &mut PassResult, item_re: &Regex, line: &str, kind: PassKind) {
    if let Some(rest) = line.strip_prefix("PROG|") {
        let f: Vec<&str> = rest.split('|').collect();
        if f.len() >= 5 {
            if let Some(i) = parse_u32(f[0]) {
                rep.ev.index = Some(i);
            }
            if let Some(n) = parse_u32(f[1]) {
                rep.ev.total = Some(n);
            }
            if let Ok(p) = f[2].trim().trim_end_matches('%').trim().parse::<f64>() {
                rep.ev.percent = p;
            }
            let clean = |s: &str| {
                let t = s.trim();
                if t.is_empty() || t.contains("N/A") || t == "NA" || t.starts_with("Unknown") {
                    None
                } else {
                    Some(t.to_string())
                }
            };
            rep.ev.speed = clean(f[3]);
            rep.ev.eta = clean(f[4]);
            rep.ev.status = "downloading".into();
            rep.ev.message = None;
            rep.emit();
        }
    } else if let Some(rest) = line.strip_prefix("ITEM|") {
        let f: Vec<&str> = rest.splitn(3, '|').collect();
        if f.len() == 3 {
            res.items_seen += 1;
            rep.ev.index = parse_u32(f[0]);
            rep.ev.total = parse_u32(f[1]);
            rep.ev.title = Some(f[2].trim().to_string());
            rep.ev.percent = 0.0;
            rep.ev.speed = None;
            rep.ev.eta = None;
            if kind == PassKind::Transcript {
                rep.ev.status = "processing".into();
                rep.ev.message = Some("Fetching captions…".into());
            } else {
                rep.ev.status = "downloading".into();
                rep.ev.message = None;
            }
            rep.emit();
        }
    } else if let Some(c) = item_re.captures(line) {
        rep.ev.index = c.get(1).and_then(|m| parse_u32(m.as_str()));
        rep.ev.total = c.get(2).and_then(|m| parse_u32(m.as_str()));
        rep.emit();
    } else if line.starts_with("[Merger]")
        || line.starts_with("[ExtractAudio]")
        || line.starts_with("[EmbedThumbnail]")
        || line.starts_with("[Metadata]")
        || line.starts_with("[ThumbnailsConvertor]")
        || line.starts_with("[VideoConvertor]")
        || line.starts_with("[Fixup")
        || line.starts_with("[MoveFiles]")
    {
        rep.ev.status = "processing".into();
        rep.ev.percent = 100.0;
        rep.ev.speed = None;
        rep.ev.eta = None;
        rep.ev.message = Some("Processing…".into());
        rep.emit();
    }
}

async fn run_pass(rep: &mut Reporter, ctl: &JobCtl, req: &JobRequest, kind: PassKind, tmp: &Path) -> PassResult {
    let mut res = PassResult::default();
    if ctl.cancelled.load(Ordering::SeqCst) {
        res.cancelled = true;
        return res;
    }
    let args = build_args(req, kind, tmp);
    let mut cmd = tools::hidden_command(tools::ytdlp_path());
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            res.spawn_error = Some(format!("Could not start yt-dlp: {e}"));
            return res;
        }
    };
    let pid = child.id();
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        res.spawn_error = Some("Could not capture yt-dlp output".into());
        return res;
    };
    let err_task = tokio::spawn(async move {
        let mut r = BufReader::new(stderr);
        let mut buf = Vec::new();
        let mut lines = Vec::new();
        while let Some(l) = read_line_lossy(&mut r, &mut buf).await {
            lines.push(l);
        }
        lines
    });

    let item_re = Regex::new(r"Downloading (?:item|video) (\d+) of (\d+)").expect("regex");
    let mut out = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        tokio::select! {
            _ = ctl.notify.notified() => {
                res.cancelled = true;
                if let Some(pid) = pid { kill_tree(pid); }
                let _ = child.kill().await;
                break;
            }
            line = read_line_lossy(&mut out, &mut buf) => {
                match line {
                    Some(l) => handle_line(rep, &mut res, &item_re, &l, kind),
                    None => break,
                }
            }
        }
    }
    let status = child.wait().await;
    res.success = status.map(|s| s.success()).unwrap_or(false);
    let stderr_lines = if res.cancelled {
        tokio::time::timeout(Duration::from_secs(3), err_task)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default()
    } else {
        err_task.await.unwrap_or_default()
    };
    for l in stderr_lines {
        if let Some(e) = l.strip_prefix("ERROR:") {
            res.errors.push(e.trim().to_string());
        } else if l.starts_with("WARNING:") {
            eprintln!("[yt-dlp] {l}");
        }
    }
    res
}

fn short_err(e: &str) -> String {
    let e = e.trim();
    if e.chars().count() > 220 {
        format!("{}…", e.chars().take(220).collect::<String>())
    } else {
        e.to_string()
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

async fn run_job(app: &AppHandle, id: u64, req: JobRequest, ctl: Arc<JobCtl>) {
    let mut rep = Reporter::new(app.clone(), id, &req);
    if ctl.cancelled.load(Ordering::SeqCst) {
        rep.set("cancelled", Some("Cancelled".into()));
        return;
    }
    rep.set("processing", Some("Preparing tools…".into()));
    if !tools::is_ready() {
        if let Err(e) = tools::ensure(app).await {
            rep.set("error", Some(format!("Tool setup failed: {e}")));
            notify(app, "Download failed", &format!("Tool setup failed: {e}"));
            return;
        }
    }
    if let Err(e) = tokio::fs::create_dir_all(&req.out_dir).await {
        rep.set("error", Some(format!("Cannot create output folder: {e}")));
        return;
    }

    let list = passes(&req);
    if list.is_empty() {
        rep.set("error", Some("Nothing selected to download".into()));
        return;
    }
    let tmp = std::env::temp_dir()
        .join("YTDownloader")
        .join(format!("job-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut any_success = false;
    let mut cancelled = false;

    for (i, kind) in list.iter().enumerate() {
        rep.ev.pass = Some(if list.len() > 1 {
            format!("{} ({}/{})", kind.label(), i + 1, list.len())
        } else {
            kind.label().to_string()
        });
        rep.ev.percent = 0.0;
        rep.ev.speed = None;
        rep.ev.eta = None;
        rep.ev.status = if *kind == PassKind::Transcript { "processing" } else { "downloading" }.into();
        rep.ev.message = if *kind == PassKind::Transcript {
            Some("Fetching captions…".into())
        } else {
            None
        };
        rep.emit();

        let res = run_pass(&mut rep, &ctl, &req, *kind, &tmp).await;
        if res.cancelled {
            cancelled = true;
            break;
        }
        if let Some(e) = res.spawn_error {
            errors.push(e);
            continue;
        }
        let mut pass_ok = res.success || (is_playlist(&req) && res.errors.len() < res.items_seen as usize);
        errors.extend(res.errors.iter().map(|e| short_err(e)));
        warnings.extend(res.warnings);

        if *kind == PassKind::Transcript {
            rep.ev.status = "processing".into();
            rep.ev.message = Some("Converting transcripts…".into());
            rep.emit();
            let fmt = Format::parse(&req.transcript_format);
            let outcomes = finish_transcripts(&tmp, req.lang.trim(), fmt);
            let mut wrote = 0;
            for o in outcomes {
                match (o.written, o.warning) {
                    (Some(_), _) => wrote += 1,
                    (None, Some(w)) => warnings.push(format!("{}: {w}", o.title)),
                    _ => {}
                }
            }
            // A video without captions is a warning, not a failure.
            pass_ok = pass_ok || wrote > 0 || res.errors.is_empty();
        }
        any_success |= pass_ok;
    }
    let _ = std::fs::remove_dir_all(&tmp);

    if cancelled {
        rep.ev.speed = None;
        rep.ev.eta = None;
        rep.set("cancelled", Some("Cancelled".into()));
        return;
    }
    let title = rep.ev.title.clone().unwrap_or_else(|| req.url.clone());
    if !any_success {
        let msg = errors
            .first()
            .cloned()
            .unwrap_or_else(|| "Download failed".into());
        rep.ev.speed = None;
        rep.ev.eta = None;
        rep.set("error", Some(msg.clone()));
        notify(app, "Download failed", &format!("{title}\n{msg}"));
        return;
    }
    rep.ev.percent = 100.0;
    rep.ev.speed = None;
    rep.ev.eta = None;
    let mut notes: Vec<String> = Vec::new();
    notes.extend(warnings.iter().take(3).cloned());
    if !errors.is_empty() {
        notes.push(format!("{} item error(s): {}", errors.len(), errors[0]));
    }
    let msg = if notes.is_empty() {
        None
    } else {
        Some(notes.join(" | "))
    };
    rep.set("done", msg);
    notify(app, "Download complete", &title);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str) -> JobRequest {
        JobRequest {
            url: url.into(),
            video: false,
            audio: false,
            transcript: true,
            video_quality: "best".into(),
            audio_format: "mp3".into(),
            transcript_format: "srt".into(),
            lang: "en".into(),
            playlist: false,
            out_dir: String::new(),
        }
    }

    #[test]
    fn playlist_flag_requires_list_param() {
        let mut r = req("https://www.youtube.com/watch?v=abc");
        r.playlist = true;
        assert!(!is_playlist(&r));
        r.url = "https://www.youtube.com/playlist?list=PL123".into();
        assert!(is_playlist(&r));
    }

    #[test]
    fn video_args_contain_quality_filter() {
        let mut r = req("https://youtu.be/abc");
        r.video_quality = "720".into();
        let a = build_args(&r, PassKind::Video, Path::new("x"));
        assert!(a.iter().any(|s| s.contains("height<=720")));
        assert!(a.contains(&"--no-playlist".to_string()));
    }

    /// Network smoke test: `cargo test -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn transcript_pipeline_smoke() {
        tools::ensure_with(&|p| eprintln!("setup: {} {:?}", p.message, p.percent))
            .await
            .expect("tool setup");
        let base = std::env::temp_dir().join("YTDownloader-smoke");
        let _ = std::fs::remove_dir_all(&base);
        let out = base.join("out");
        let info = base.join("info");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&info).unwrap();
        let mut r = req("https://www.youtube.com/watch?v=jNQXAC9IVRw");
        r.out_dir = out.to_string_lossy().into_owned();
        let args = build_args(&r, PassKind::Transcript, &info);
        eprintln!("args: {args:?}");
        let o = tools::hidden_command(tools::ytdlp_path())
            .args(&args)
            .output()
            .await
            .expect("run yt-dlp");
        eprintln!("stdout:\n{}", String::from_utf8_lossy(&o.stdout));
        eprintln!("stderr:\n{}", String::from_utf8_lossy(&o.stderr));
        let outcomes = finish_transcripts(&info, "en", Format::Srt);
        eprintln!("{outcomes:?}");
        let written = outcomes
            .iter()
            .find_map(|o| o.written.clone())
            .expect("a transcript was written");
        let text = std::fs::read_to_string(&written).unwrap();
        eprintln!("{}: {}", written.display(), &text[..text.len().min(300)]);
        assert!(text.starts_with("1\n00:00:"));
    }
}
