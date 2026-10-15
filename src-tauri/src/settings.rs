//! Persistent user settings stored as `settings.json` in the app config dir.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub out_dir: String,
    pub video: bool,
    pub audio: bool,
    pub transcript: bool,
    /// "best" | "1080" | "720" | "480"
    pub video_quality: String,
    /// "mp3" | "m4a"
    pub audio_format: String,
    /// "txt" | "timestamped" | "srt" | "vtt" | "json"
    pub transcript_format: String,
    pub lang: String,
}

pub fn default_out_dir() -> String {
    let base = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Downloads")
        .join("YouTube")
        .to_string_lossy()
        .into_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            out_dir: default_out_dir(),
            video: true,
            audio: false,
            transcript: false,
            video_quality: "best".into(),
            audio_format: "mp3".into(),
            transcript_format: "txt".into(),
            lang: "en".into(),
        }
    }
}

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    settings_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .map(|mut s| {
            if s.out_dir.trim().is_empty() {
                s.out_dir = default_out_dir();
            }
            s
        })
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, settings: &Settings) -> anyhow::Result<()> {
    let path = settings_path(app).ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}
