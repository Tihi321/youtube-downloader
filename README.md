# YouTube Downloader

A small Windows tray app (Tauri v2) to download YouTube **videos**, **music (audio)** and **transcripts** from a
single video or a whole playlist. It lives in the system tray and shows a progress bar per job.

## Features
- Paste a video or playlist URL; choose any combination of **Video**, **Audio**, **Transcript**.
- Video: Best / 1080p / 720p / 480p, merged to MP4.
- Audio: MP3 (320k, with thumbnail + metadata) or M4A (original stream, no re-encode).
- Transcript: TXT, timestamped TXT, SRT, VTT or JSON, in a language of your choice (default `en`).
  Manual captions are preferred over auto-generated ones.
- Playlists: optional "Download entire playlist" (on by default for `/playlist?` URLs, off for `watch?v=…&list=…`).
  One failing item does not abort the playlist.
- Job queue (sequential) with progress, speed/ETA, "item n/N", cancel, and a toast when a job finishes.
- Tray icon: Show window, Download URL from clipboard (uses last-used options), Open downloads folder, Quit.
  Closing the window hides it to the tray; launching the app twice focuses the running instance.
- Output naming: `<out>/<title> [<id>].<ext>` for single videos,
  `<out>/<playlist title>/<NNN> - <title>.<ext>` for playlists. Transcripts use the same name stem plus
  `.<lang>.<ext>`.
- Footer shows the yt-dlp version with an **Update yt-dlp** button (`yt-dlp -U`), so YouTube changes rarely need a rebuild.

## First run
`yt-dlp.exe` and `ffmpeg.exe`/`ffprobe.exe` are **not bundled**. On first launch they are downloaded (with progress) into
`%LOCALAPPDATA%\YTDownloader\bin\`:
- https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe
- https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip

Settings are stored in `%APPDATA%\com.tihi321.ytdownloader\settings.json`.

## Build
Requirements: Windows 10/11, Node.js + npm, Rust (MSVC toolchain, rustc 1.89+) and the
**Visual Studio Build Tools with the "Desktop development with C++" workload** (MSVC `link.exe` + Windows SDK).
WebView2 is preinstalled on Windows 11.

```
npm install
npm run dev      # run in development
npm run build    # release exe + NSIS installer
```
Outputs: `src-tauri/target/release/yt-downloader.exe` (portable) and
`src-tauri/target/release/bundle/nsis/yt-downloader_0.1.0_x64-setup.exe`.

`src-tauri/.cargo/config.toml` enables Cargo's MSRV-aware resolver so dependencies resolve to versions compatible with
the installed rustc. Run `cargo test` in `src-tauri` for the transcript unit tests; the network smoke test is ignored by
default (`cargo test -- --ignored`).

The icon is generated from `src-tauri/app-icon.png` with `npx tauri icon src-tauri/app-icon.png`.

## Legal
For personal use only. Download only content you have the right to, and respect YouTube's Terms of Service and the
copyright of creators.
