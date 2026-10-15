# YouTube Downloader (video / music / transcript) — Windows tray app

> On implementation start, copy this plan to `.claude/tickets/youtube-downloader-app/plan.md` (per global CLAUDE.md) and run it through the `implement` skill.

## Context
`C:\projects\Personal\youtube-downloader` is an empty repo (only `README.md`). The user wants a Windows desktop app:
paste a YouTube URL (single video **or playlist**) → download **video**, **music (audio)**, and/or **transcript**;
lives in the **system tray**; ships as an **.exe**; progress bar is nice-to-have.

The reference repo `Tihi321/youtube-transcription-downloader` (Node, no deps) fetches captions via the InnerTube
`player` endpoint (Android client), parses caption XML into `{start, dur, text}` segments, and renders them with
`src/formats.js` into **txt / timestamped txt / srt / vtt / json**. We reuse that segment model and port the
`formats.js` renderers (`clock`, `short`, `end` helpers) 1:1, but source the captions through yt-dlp so playlists
and YouTube's frequent anti-bot changes are handled by one maintained tool.

## Tech choice
- **Tauri v2** (Rust backend, vanilla HTML/CSS/JS frontend, no bundler) — small native exe, built-in tray API,
  WebView2 is already on Windows 11. Rust/cargo is installed.
- **yt-dlp.exe** does all YouTube work (videos, audio, playlists, subtitles). **ffmpeg** is needed for merging
  video+audio and mp3 conversion.
- Both binaries are **downloaded on first run** into `%LOCALAPPDATA%\YTDownloader\bin\` (not bundled), so the exe
  stays small and yt-dlp can self-update (`yt-dlp -U`) when YouTube breaks things — no rebuild needed.
  - yt-dlp: `https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe`
  - ffmpeg: `https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip` (extract `ffmpeg.exe` + `ffprobe.exe`)

## Project layout
```
package.json                 # only @tauri-apps/cli devDep + scripts: dev, build
src/                         # frontend (static, served via frontendDist)
  index.html  styles.css  main.js
src-tauri/
  Cargo.toml  tauri.conf.json  build.rs  capabilities/default.json
  icons/                     # generated with `tauri icon` from one source PNG
  src/
    main.rs                  # app setup, tray, close-to-tray, single-instance
    tools.rs                 # locate/download/update yt-dlp + ffmpeg, emits setup progress
    downloader.rs            # job queue, builds yt-dlp args, spawns process, parses progress, cancel
    transcript.rs            # json3 -> segments; port of formats.js renderers
    settings.rs              # load/save settings.json in app config dir
```
Crates: `tauri` (feature `tray-icon`), `tauri-plugin-dialog` (folder picker), `tauri-plugin-opener` (open folder),
`tauri-plugin-notification` (job done toast), `tauri-plugin-single-instance`, `tokio` (process, sync),
`reqwest` (rustls, stream), `zip`, `serde`/`serde_json`, `regex`, `anyhow`.

## Features / behaviour
**UI (single window, ~520×640):**
- URL box + "Paste" button (also auto-paste clipboard YouTube URL when window is shown and box is empty).
- Mode toggles (checkboxes, any combination): **Video**, **Audio**, **Transcript**.
  - Video quality: Best / 1080p / 720p / 480p → mp4.
  - Audio format: MP3 (320k) / M4A (original, no re-encode).
  - Transcript: format (TXT, Timestamped TXT, SRT, VTT, JSON) + language code (default `en`).
- Playlist: if URL contains `list=`, show checkbox "Download entire playlist" (default on for `/playlist?` URLs,
  off for `watch?v=…&list=…`). Maps to `--yes-playlist` / `--no-playlist`.
- Output folder (default `%USERPROFILE%\Downloads\YouTube`) with Browse + Open buttons. Settings persisted.
- Job list: title, "item 3/12" for playlists, progress bar + % + speed/ETA, status (queued / downloading /
  processing / done / error), Cancel button, "Open folder" on done. Jobs run sequentially.
- Footer: yt-dlp version + "Update yt-dlp" button.

**Tray:** icon with menu — Show window, Download URL from clipboard (uses last-used options), Open downloads
folder, Quit. Left-click toggles window. Closing the window hides it to tray (`on_window_event` →
`CloseRequested` → `prevent_close` + `hide`). Toast notification when a job finishes/fails. Single instance:
second launch just focuses the existing window.

**Output naming:** single video → `<out>/<title> [<id>].<ext>`; playlist →
`<out>/<playlist_title>/<NNN> - <title>.<ext>` (`%(playlist_index)03d`).

## yt-dlp invocation (downloader.rs)
Common args: `--newline --no-colors --ffmpeg-location <bin> --windows-filenames -o <template>`
`--progress-template "download:PROG|%(info.playlist_index)s|%(info.n_entries)s|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s"`
`--print "before_dl:ITEM|%(playlist_index)s|%(n_entries)s|%(title)s"` (switches the job's current title).
- Video: `-f "bv*[height<=H][ext=mp4]+ba[ext=m4a]/bv*[height<=H]+ba/b" --merge-output-format mp4`
- Audio MP3: `-f ba -x --audio-format mp3 --audio-quality 0 --embed-thumbnail --embed-metadata`
- Audio M4A: `-f "ba[ext=m4a]/ba" -x --audio-format m4a`
- Video+Audio both selected → run two passes (one per output) within the same job, sequentially.
- Transcript: `--skip-download --write-subs --write-auto-subs --sub-langs "<lang>,<lang>-orig,<lang>.*" --sub-format json3`
  into a temp dir; yt-dlp prefers manual subs over auto for the same language. Then `transcript.rs` parses each
  `.json3` (`events[].segs[].utf8`, `tStartMs`, `dDurationMs`) into segments, renders the chosen format with
  the ported `formats.js` logic, writes `<title>.<lang>.<ext>` next to the media, and deletes the temp file.
  Videos with no captions → per-item warning, job continues.
- Spawn with `tokio::process::Command` + `CREATE_NO_WINDOW` (0x08000000) so no console flashes; stream stdout
  lines, parse `PROG|`/`ITEM|`, emit `job-progress` events to the frontend; stderr `ERROR:` lines become the
  item error. Cancel = kill child + mark cancelled.

## Tauri commands (frontend ↔ backend)
`get_settings`, `save_settings`, `ensure_tools` (emits `setup-progress`), `update_ytdlp`, `tools_version`,
`enqueue(JobRequest) -> job_id`, `cancel(job_id)`, `pick_folder` (via dialog plugin on JS side), `open_path`.
Events: `job-progress {id, index, total, title, percent, speed, eta, status, message}`, `setup-progress`.

## Packaging
- `npm run build` → `tauri build` produces `src-tauri/target/release/yt-downloader.exe` (standalone portable exe)
  plus an NSIS installer under `target/release/bundle/nsis/`. Set `bundle.targets = ["nsis"]`,
  `windows.webviewInstallMode = downloadBootstrapper`.
- Release profile: `opt-level="s"`, `lto=true`, `strip=true`. `#![windows_subsystem = "windows"]` in main.rs.
- App icon + tray icon generated from one PNG with `npx tauri icon`.
- README: features, build steps, first-run tool download note, legal note (personal use, respect YouTube ToS).

## Verification
1. `cargo check` / `cargo clippy` in `src-tauri`; unit tests in `transcript.rs` comparing rendered
   txt/srt/vtt output against a small fixture json3 (mirrors formats.js output).
2. `npm run dev`: first run downloads yt-dlp + ffmpeg with progress; footer shows yt-dlp version.
3. Manual runs against real URLs:
   - single video → Video 720p mp4 plays with audio; Audio MP3 has metadata/thumbnail;
   - Transcript SRT + TXT for a video with manual captions and one with only auto captions; a video with no captions shows a warning;
   - short public playlist (3–5 items) with Audio → subfolder with numbered files, progress shows "item n/N";
   - `watch?v=…&list=…` with playlist checkbox off downloads only that video;
   - Cancel mid-download stops the process and leaves no running yt-dlp.exe (Task Manager).
4. Tray: close window → app stays in tray; tray "Download URL from clipboard" works; Quit exits; launching the exe twice focuses the first instance.
5. `npm run build` → run `target/release/yt-downloader.exe` from a different folder and repeat a quick video + transcript download.

## Implementation
- Branch: `youtube-downloader-app` (from `origin/master`)


### Implementation status (implementation agent)
**BLOCKED on build verification: this machine has no MSVC Build Tools / Windows SDK.** `cargo check` fails at the first build script with `linker link.exe not found` (in Git Bash, `link` resolves to coreutils `C:/Program Files/Git/usr/bin/link.exe`; in PowerShell it is absent). No Visual Studio, no `Windows Kits`, no `rc.exe`, not admin, so the workload could not be installed. Therefore NO Rust code has been compiled, linted, or tested. Install "Visual Studio Build Tools -> Desktop development with C++" and run the verification below.

- [x] Project layout / package.json / .gitignore / tauri.conf.json / capabilities (written; not built)
- [x] Icon generated (src-tauri/app-icon.png, pure-Python PNG) and `npx tauri icon` run -> src-tauri/icons (android/ios dirs removed)
- [x] Frontend src/index.html, styles.css, main.js (written; `node --check` passes; not run in the app)
- [x] settings.rs, tools.rs, downloader.rs, transcript.rs, main.rs (written, UNCOMPILED)
- [x] README
- [x] Cargo dependency resolution succeeded under rustc 1.89 (Cargo.lock generated using `src-tauri/.cargo/config.toml` `incompatible-rust-versions = "fallback"`, because latest tauri 2.12 needs rustc 1.90)
- [ ] `cargo check` / `cargo clippy -- -D warnings` (blocked: no linker)
- [ ] `cargo test` incl. transcript.rs fixtures (tests written; not run)
- [ ] `npm run build` exe + NSIS (blocked)
- [ ] Ignored network smoke test `cargo test -- --ignored --nocapture transcript_pipeline_smoke` (written; not run)
- [ ] Manual checks (tray, real downloads via UI, cancel, playlist, single-instance): not run

Deviations / notes:
- Transcripts: yt-dlp writes subtitle json3 straight into the media folder using the media name stem (`<stem>.<lang>.json3`); an `infojson:` template into a per-job temp dir records `requested_subtitles[].filepath` + title, which `finish_transcripts` uses to convert json3 to the chosen format and delete the json3. Unverified assumptions: yt-dlp fills `filepath` in `requested_subtitles` before writing the info json under `--skip-download`; `before_dl:` print fires with `--skip-download`.
- Transcript filename uses the media stem (`<title> [<id>].<lang>.<ext>`) per the task note, not `<title>.<lang>.<ext>`.
- Added `rust-version = "1.89"` and `.cargo/config.toml` (MSRV-aware resolver).
- Not handled: recent yt-dlp may need a JS runtime (e.g. Deno) for some YouTube formats.

- Deviation (user request): package manager switched from npm to Yarn 4.13 (`packageManager` field, `.yarnrc.yml` nodeLinker: node-modules). Use `yarn install` / `yarn dev` / `yarn build`.
- Deviation (user request): project pinned to Rust 1.99.0 via `rust-toolchain.toml`; MSRV fallback resolver config removed.

