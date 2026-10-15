#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod downloader;
mod settings;
mod tools;
mod transcript;

use std::sync::OnceLock;

use downloader::{JobRequest, Queue};
use regex::Regex;
use serde::Serialize;
use settings::Settings;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WindowEvent,
};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

fn youtube_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^https?://(www\.|m\.|music\.)?(youtube\.com|youtu\.be)/\S+$").expect("regex")
    })
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn toggle_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false) {
            let _ = w.hide();
        } else {
            show_main(app);
        }
    }
}

#[derive(Serialize)]
struct ToolsInfo {
    ready: bool,
    ytdlp_version: Option<String>,
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Settings {
    settings::load(&app)
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    settings::save(&app, &settings).map_err(|e| e.to_string())
}

#[tauri::command]
async fn ensure_tools(app: AppHandle) -> Result<ToolsInfo, String> {
    tools::ensure(&app).await.map_err(|e| format!("{e:#}"))?;
    Ok(ToolsInfo {
        ready: tools::is_ready(),
        ytdlp_version: tools::version().await,
    })
}

#[tauri::command]
async fn tools_version() -> ToolsInfo {
    ToolsInfo {
        ready: tools::is_ready(),
        ytdlp_version: tools::version().await,
    }
}

#[tauri::command]
async fn update_ytdlp(app: AppHandle) -> Result<String, String> {
    tools::update_ytdlp(&app).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn enqueue(queue: State<'_, Queue>, mut req: JobRequest) -> Result<u64, String> {
    req.url = req.url.trim().to_string();
    if !req.url.starts_with("http://") && !req.url.starts_with("https://") {
        return Err("Enter a valid URL".into());
    }
    if !(req.video || req.audio || req.transcript) {
        return Err("Select at least one of Video, Audio or Transcript".into());
    }
    if req.lang.trim().is_empty() {
        req.lang = "en".into();
    }
    Ok(queue.enqueue(req))
}

#[tauri::command]
fn cancel(queue: State<'_, Queue>, id: u64) {
    queue.cancel(id);
}

#[tauri::command]
fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    let _ = std::fs::create_dir_all(&path);
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

fn request_from_settings(s: &Settings, url: String) -> JobRequest {
    JobRequest {
        playlist: url.contains("/playlist?"),
        url,
        video: s.video,
        audio: s.audio,
        transcript: s.transcript,
        video_quality: s.video_quality.clone(),
        audio_format: s.audio_format.clone(),
        transcript_format: s.transcript_format.clone(),
        lang: s.lang.clone(),
        out_dir: s.out_dir.clone(),
    }
}

fn download_from_clipboard(app: &AppHandle) {
    let text = app.clipboard().read_text().unwrap_or_default();
    let url = text.trim().to_string();
    if !youtube_url_re().is_match(&url) {
        let _ = app
            .notification()
            .builder()
            .title("YouTube Downloader")
            .body("No YouTube URL found in the clipboard.")
            .show();
        return;
    }
    let s = settings::load(app);
    let mut req = request_from_settings(&s, url);
    if !(req.video || req.audio || req.transcript) {
        req.video = true;
    }
    let queue = app.state::<Queue>();
    queue.enqueue(req);
    show_main(app);
    let _ = app
        .notification()
        .builder()
        .title("YouTube Downloader")
        .body("Download queued from clipboard.")
        .show();
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItemBuilder::with_id("show", "Show window").build(app)?;
    let clip = MenuItemBuilder::with_id("clipboard", "Download URL from clipboard").build(app)?;
    let folder = MenuItemBuilder::with_id("folder", "Open downloads folder").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&show, &clip, &folder, &quit])
        .build()?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("YouTube Downloader")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "clipboard" => download_from_clipboard(app),
            "folder" => {
                let dir = settings::load(app).out_dir;
                let _ = open_path(app.clone(), dir);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(Queue::start(handle.clone()));
            setup_tray(&handle)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            ensure_tools,
            tools_version,
            update_ytdlp,
            enqueue,
            cancel,
            open_path
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
