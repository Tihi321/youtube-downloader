"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const dialog = window.__TAURI__.dialog;
const clipboard = window.__TAURI__.clipboardManager;

const $ = (id) => document.getElementById(id);
const YT_RE = /^https?:\/\/(www\.|m\.|music\.)?(youtube\.com|youtu\.be)\/\S+$/i;

const els = {
  url: $("url"), paste: $("paste"), playlistRow: $("playlist-row"), playlist: $("playlist"),
  mVideo: $("m-video"), mAudio: $("m-audio"), mTranscript: $("m-transcript"),
  optVideo: $("opt-video"), optAudio: $("opt-audio"), optTranscript: $("opt-transcript"),
  videoQuality: $("video-quality"), audioFormat: $("audio-format"),
  transcriptFormat: $("transcript-format"), lang: $("lang"),
  outDir: $("out-dir"), browse: $("browse"), openOut: $("open-out"),
  download: $("download"), formError: $("form-error"),
  jobs: $("jobs"), jobsEmpty: $("jobs-empty"),
  version: $("ytdlp-version"), update: $("update"),
  setup: $("setup"), setupTitle: $("setup-title"), setupBar: $("setup-bar"),
  setupMsg: $("setup-msg"), setupError: $("setup-error"), setupRetry: $("setup-retry"),
};

let settings = null;
const jobs = new Map(); // id -> { el, refs }

// ---------- settings ----------
function applySettings(s) {
  els.mVideo.checked = s.video;
  els.mAudio.checked = s.audio;
  els.mTranscript.checked = s.transcript;
  els.videoQuality.value = s.video_quality;
  els.audioFormat.value = s.audio_format;
  els.transcriptFormat.value = s.transcript_format;
  els.lang.value = s.lang;
  els.outDir.value = s.out_dir;
  updateOptionVisibility();
}

function collectSettings() {
  return {
    out_dir: els.outDir.value,
    video: els.mVideo.checked,
    audio: els.mAudio.checked,
    transcript: els.mTranscript.checked,
    video_quality: els.videoQuality.value,
    audio_format: els.audioFormat.value,
    transcript_format: els.transcriptFormat.value,
    lang: els.lang.value.trim() || "en",
  };
}

async function persist() {
  settings = collectSettings();
  try { await invoke("save_settings", { settings }); } catch (e) { console.error(e); }
}

function updateOptionVisibility() {
  els.optVideo.classList.toggle("hidden", !els.mVideo.checked);
  els.optAudio.classList.toggle("hidden", !els.mAudio.checked);
  els.optTranscript.classList.toggle("hidden", !els.mTranscript.checked);
}

// ---------- URL handling ----------
function updatePlaylistRow() {
  const v = els.url.value.trim();
  const hasList = /[?&]list=/.test(v);
  els.playlistRow.classList.toggle("hidden", !hasList);
  if (hasList && els.url.dataset.listFor !== v) {
    els.url.dataset.listFor = v;
    els.playlist.checked = /\/playlist\?/.test(v);
  }
}

async function readClipboardUrl() {
  try {
    const text = ((await clipboard.readText()) || "").trim();
    return YT_RE.test(text) ? text : "";
  } catch (e) {
    return "";
  }
}

async function pasteUrl(manual) {
  const text = manual ? ((await clipboard.readText().catch(() => "")) || "").trim() : await readClipboardUrl();
  if (text) {
    els.url.value = text;
    updatePlaylistRow();
  }
}

async function autoPaste() {
  if (els.url.value.trim()) return;
  await pasteUrl(false);
}

// ---------- jobs ----------
function makeJobEl(id) {
  const el = document.createElement("div");
  el.className = "job queued";
  el.innerHTML = `
    <div class="job-head"><span class="job-title"></span><span class="job-status"></span></div>
    <div class="bar"><div class="bar-fill"></div></div>
    <div class="job-meta"><span class="left"></span><span class="right"></span></div>
    <div class="job-msg"></div>
    <div class="job-actions"></div>`;
  const refs = {
    title: el.querySelector(".job-title"), status: el.querySelector(".job-status"),
    fill: el.querySelector(".bar-fill"), left: el.querySelector(".left"),
    right: el.querySelector(".right"), msg: el.querySelector(".job-msg"),
    actions: el.querySelector(".job-actions"),
  };
  els.jobs.prepend(el);
  els.jobsEmpty.classList.add("hidden");
  const j = { el, refs, id, outDir: "" };
  jobs.set(id, j);
  return j;
}

function button(label, onClick) {
  const b = document.createElement("button");
  b.textContent = label;
  b.addEventListener("click", onClick);
  return b;
}

function renderJob(ev) {
  const j = jobs.get(ev.id) || makeJobEl(ev.id);
  const { el, refs } = j;
  j.outDir = ev.out_dir;
  el.className = `job ${ev.status}`;
  refs.title.textContent = ev.title || ev.url;
  refs.title.title = ev.title || ev.url;
  refs.status.textContent = ev.status;
  const pct = Math.max(0, Math.min(100, ev.percent || 0));
  const indeterminate = ev.status === "processing" || ev.status === "queued";
  refs.fill.style.width = indeterminate ? "" : `${pct}%`;
  if (ev.status === "done") refs.fill.style.width = "100%";
  if (ev.status === "error" || ev.status === "cancelled") refs.fill.style.width = `${pct}%`;

  const left = [];
  if (ev.pass) left.push(ev.pass);
  if (ev.total && ev.total > 1 && ev.index) left.push(`item ${ev.index}/${ev.total}`);
  refs.left.textContent = left.join(" · ");
  const right = [];
  if (ev.status === "downloading") {
    right.push(`${pct.toFixed(1)}%`);
    if (ev.speed) right.push(ev.speed);
    if (ev.eta) right.push(`ETA ${ev.eta}`);
  }
  refs.right.textContent = right.join(" · ");
  refs.msg.textContent = ev.message || "";

  refs.actions.replaceChildren();
  if (ev.status === "queued" || ev.status === "downloading" || ev.status === "processing") {
    refs.actions.append(button("Cancel", () => invoke("cancel", { id: ev.id })));
  } else {
    if (ev.status === "done") {
      refs.actions.append(button("Open folder", () => invoke("open_path", { path: j.outDir })));
    }
    refs.actions.append(button("Dismiss", () => {
      el.remove();
      jobs.delete(ev.id);
      if (jobs.size === 0) els.jobsEmpty.classList.remove("hidden");
    }));
  }
}

// ---------- actions ----------
async function startDownload() {
  els.formError.classList.add("hidden");
  const url = els.url.value.trim();
  const s = collectSettings();
  let err = "";
  if (!/^https?:\/\//i.test(url)) err = "Enter a valid URL.";
  else if (!(s.video || s.audio || s.transcript)) err = "Select Video, Audio and/or Transcript.";
  else if (!s.out_dir) err = "Choose an output folder.";
  if (err) {
    els.formError.textContent = err;
    els.formError.classList.remove("hidden");
    return;
  }
  await persist();
  try {
    await invoke("enqueue", { req: { ...s, url, playlist: els.playlist.checked && !els.playlistRow.classList.contains("hidden") } });
    els.url.value = "";
    delete els.url.dataset.listFor;
    updatePlaylistRow();
  } catch (e) {
    els.formError.textContent = String(e);
    els.formError.classList.remove("hidden");
  }
}

async function browse() {
  try {
    const dir = await dialog.open({ directory: true, multiple: false, defaultPath: els.outDir.value || undefined });
    if (dir) {
      els.outDir.value = dir;
      persist();
    }
  } catch (e) {
    console.error(e);
  }
}

async function refreshVersion() {
  try {
    const info = await invoke("tools_version");
    els.version.textContent = `yt-dlp: ${info.ytdlp_version || "not installed"}`;
  } catch (e) {
    els.version.textContent = "yt-dlp: unknown";
  }
}

async function updateYtdlp() {
  els.update.disabled = true;
  els.version.textContent = "yt-dlp: updating…";
  try {
    const v = await invoke("update_ytdlp");
    els.version.textContent = `yt-dlp: ${v}`;
  } catch (e) {
    els.version.textContent = `yt-dlp: update failed (${String(e).slice(0, 80)})`;
  } finally {
    els.update.disabled = false;
  }
}

// ---------- first-run setup ----------
async function runSetup() {
  els.setupError.classList.add("hidden");
  els.setupRetry.classList.add("hidden");
  els.setupTitle.textContent = "Setting up tools…";
  try {
    const info = await invoke("tools_version");
    if (info.ready) {
      els.version.textContent = `yt-dlp: ${info.ytdlp_version || "?"}`;
      return;
    }
  } catch (e) { /* fall through to ensure */ }
  els.setup.classList.remove("hidden");
  els.setupBar.style.width = "0";
  els.setupMsg.textContent = "Downloading yt-dlp and ffmpeg (first run only)…";
  try {
    const info = await invoke("ensure_tools");
    els.version.textContent = `yt-dlp: ${info.ytdlp_version || "?"}`;
    els.setup.classList.add("hidden");
  } catch (e) {
    els.setupTitle.textContent = "Setup failed";
    els.setupError.textContent = String(e);
    els.setupError.classList.remove("hidden");
    els.setupRetry.classList.remove("hidden");
  }
}

// ---------- wiring ----------
async function init() {
  settings = await invoke("get_settings");
  applySettings(settings);

  els.url.addEventListener("input", updatePlaylistRow);
  els.url.addEventListener("keydown", (e) => { if (e.key === "Enter") startDownload(); });
  els.paste.addEventListener("click", () => pasteUrl(true));
  els.download.addEventListener("click", startDownload);
  els.browse.addEventListener("click", browse);
  els.openOut.addEventListener("click", () => invoke("open_path", { path: els.outDir.value }));
  els.update.addEventListener("click", updateYtdlp);
  els.setupRetry.addEventListener("click", runSetup);
  for (const el of [els.mVideo, els.mAudio, els.mTranscript]) {
    el.addEventListener("change", () => { updateOptionVisibility(); persist(); });
  }
  for (const el of [els.videoQuality, els.audioFormat, els.transcriptFormat, els.lang]) {
    el.addEventListener("change", persist);
  }

  await listen("job-progress", (e) => renderJob(e.payload));
  await listen("setup-progress", (e) => {
    const p = e.payload;
    if (p.done) return;
    els.setup.classList.remove("hidden");
    els.setupMsg.textContent = p.message;
    els.setupBar.style.width = p.percent == null ? "100%" : `${p.percent}%`;
  });

  window.addEventListener("focus", autoPaste);
  await autoPaste();
  await runSetup();
}

init().catch((e) => {
  document.body.textContent = `Failed to start: ${e}`;
});
