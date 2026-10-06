// Coucou for Windows — app wiring and the commands the island calls.

mod attach;
mod claude;
mod clipboard;
mod docfix;
mod engine;
mod filefix;
mod files;
mod gitpush;
mod hooks;
mod integrations;
mod island;
mod log;
mod ollama;
mod pipe;
mod quickfix;
mod scan;
mod secrets;
mod settings;
mod textdiff;
mod tray;
mod webdrop;
mod winurl;
mod win_user;

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};

use claude::{Chat, ChatContext, ChatReply};
use docfix::{DocFixResult, Outputs};
use ollama::LocalChat;
use files::DroppedFile;
use hooks::{HookPreview, HookStatus};
use island::{PollGate, ScreenInfo};
use pipe::Pending;
use settings::Settings;

/// Keeps spawned helpers from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub gate: Arc<PollGate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfo {
    settings: Settings,
    screen: ScreenInfo,
    version: String,
    hook_path: String,
}

#[tauri::command]
fn boot(app: AppHandle, shared: State<Shared>) -> BootInfo {
    let mut settings = shared.settings.lock().unwrap().clone();
    // The real state of ~/.claude/settings.json wins over whatever we stored.
    settings.hooks_installed = hooks::status().installed;
    let screen = island::screen_info(&app, &settings.screen);
    BootInfo {
        settings,
        screen,
        version: env!("CARGO_PKG_VERSION").to_string(),
        hook_path: settings::hook_exe_path().to_string_lossy().to_string(),
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<Shared>, settings: Settings) {
    let (screen_changed, autostart_changed, shortcut_changed) = {
        let mut current = shared.settings.lock().unwrap();
        let screen_changed = current.screen != settings.screen;
        let autostart_changed = current.autostart != settings.autostart;
        let shortcut_changed = current.quickfix_shortcut != settings.quickfix_shortcut;
        *current = settings.clone();
        (screen_changed, autostart_changed, shortcut_changed)
    };
    if let Err(err) = settings::save(&settings) {
        eprintln!("[coucou] could not save settings: {err}");
    }
    if autostart_changed {
        let manager = app.autolaunch();
        let result = if settings.autostart { manager.enable() } else { manager.disable() };
        if let Err(err) = result {
            eprintln!("[coucou] autostart: {err}");
        }
    }
    if shortcut_changed {
        quickfix::register(&app, &settings.quickfix_shortcut);
    }
    if screen_changed {
        let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
        island::apply_geometry(&app, &settings.screen, collapsed);
    }
    // Keep the other window in step (island ⇄ settings window).
    let _ = app.emit("settings-changed", settings);
}

/// Hidden island → shrink the window to the invisible wake strip and park the
/// cursor poll; anything else → full panel and 60 Hz polling.
#[tauri::command]
fn set_collapsed(app: AppHandle, shared: State<Shared>, collapsed: bool) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    shared.gate.collapsed.store(collapsed, Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed);
    // The wake strip must always take the mouse, and a resize invalidates the flag.
    island::set_ignore_cursor(&app, false);
    shared.gate.forget_ignore_state();
    shared.gate.set_active(!collapsed);
}

/// The front end pushes the island shape; Rust decides click-through from it.
#[tauri::command]
fn set_island_rect(shared: State<Shared>, x: f64, y: f64, width: f64, height: f64) {
    shared.gate.set_rect(island::IslandRect { x, y, w: width, h: height });
}

#[tauri::command]
fn focus_window(app: AppHandle, focused: bool) {
    let Some(win) = island::window(&app) else { return };
    island::set_activating(&win, focused);
    if focused {
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn reposition(app: AppHandle, shared: State<Shared>) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed);
}

#[tauri::command]
fn open_url(url: String) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    let _ = Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", &url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// "Open terminal" opens the working folder in VS Code when `code` is on PATH,
/// and falls back to Explorer otherwise.
#[tauri::command]
fn open_in_vscode(path: Option<String>) -> bool {
    // No `cmd /C` anywhere near this. The path is a project folder chosen by
    // whoever is using Claude Code, and cmd would happily read `&`, `^` and `%`
    // in a folder name as syntax. Finding the launcher ourselves and handing the
    // path over as a separate argument keeps it a path.
    if let Some(code) = find_on_path("code") {
        let mut cmd = Command::new(code);
        if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
            cmd.arg(p);
        }
        if cmd.creation_flags(CREATE_NO_WINDOW).spawn().is_ok() {
            return true;
        }
    }
    if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
        let _ = Command::new("explorer").arg(p).spawn();
    }
    false
}

/// Our own `where`: walks %PATH% against %PATHEXT%, no shell involved.
/// Rust quotes arguments correctly for `.cmd`/`.bat` targets since 1.77, so
/// spawning `code.cmd` directly is safe.
fn find_on_path(stem: &str) -> Option<std::path::PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let dirs = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&dirs) {
        for ext in exts.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{stem}{}", ext.to_lowercase()));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// Tray → Pause. Paused means paused: the pollers stop talking to the network,
/// not just the island stopping showing things.
#[tauri::command]
fn set_paused(paused: bool) {
    integrations::set_paused(paused);
}

// ── Claude Code hooks ─────────────────────────────────────────────────────────

#[tauri::command]
fn hooks_status() -> HookStatus {
    hooks::status()
}

/// Returns the diff the user has to look at before anything is written.
#[tauri::command]
fn hooks_preview(install: bool) -> Result<HookPreview, String> {
    hooks::preview(install)
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn hooks_apply(
    app: AppHandle,
    shared: State<Shared>,
    install: bool,
    fingerprint: String,
) -> Result<String, String> {
    // The fingerprint comes from the preview the user actually looked at, so a
    // settings.json that changed in between is refused rather than overwritten.
    let backup = hooks::write(install, &fingerprint)?;
    let updated = {
        let mut current = shared.settings.lock().unwrap();
        current.hooks_installed = install;
        let _ = settings::save(&current);
        current.clone()
    };
    let _ = app.emit("settings-changed", updated);
    Ok(backup)
}

#[tauri::command]
fn approval_decision(app: AppHandle, request_id: String, decision: String) {
    pipe::answer(&app, &request_id, &decision);
}

/// The island has the card on screen, so the long wait for a human may begin.
/// Until this arrives the relay only waits a few hundred milliseconds, which is
/// what stops a paused or unresponsive island from freezing Claude Code.
#[tauri::command]
fn approval_ack(app: AppHandle, request_id: String) {
    pipe::acknowledge(&app, &request_id);
}

/// Nobody can act on this request — the island is paused, or another card is
/// already up. Claude Code falls back to asking in the terminal immediately.
#[tauri::command]
fn approval_decline(app: AppHandle, request_id: String) {
    pipe::decline(&app, &request_id);
}

// ── Chat, files and secrets ───────────────────────────────────────────────────

/// One chat turn, answered by Claude or by the local Ollama model depending on
/// the settings. The API key and any file bytes stay on the Rust side.
/// `stream_id` tags the `chat-chunk` events of a streamed (local) answer.
#[tauri::command]
async fn chat_send(
    app: AppHandle,
    shared: State<'_, Shared>,
    chat: State<'_, Chat>,
    local: State<'_, LocalChat>,
    query: String,
    context: Option<ChatContext>,
    stream_id: Option<u64>,
) -> Result<ChatReply, String> {
    let s = shared.settings.lock().unwrap().clone();
    let result = if s.provider == "ollama" {
        let cfg = engine::ollama_config(&s);
        ollama::send(&app, &local, cfg, query, context, stream_id.unwrap_or(0)).await
    } else {
        claude::send(&chat, &s.model, query, context).await
    };
    if let Err(err) = &result {
        log::line(format!("chat ({}) failed: {err}", s.provider));
    }
    result
}

#[tauri::command]
fn chat_reset(chat: State<Chat>, local: State<LocalChat>) {
    chat.reset();
    local.reset();
}

/// Settings → AI engine: is Ollama running, and which models are installed.
#[tauri::command]
async fn ollama_status(local: State<'_, LocalChat>, url: String) -> Result<ollama::Status, String> {
    Ok(ollama::status(&local, &url).await)
}

/// Copies a dropped file into the inbox and reports its name back.
#[tauri::command]
fn ingest_file(path: String) -> Result<DroppedFile, String> {
    files::ingest(&path)
}

/// Several dropped files and folders at once (folders are walked).
#[tauri::command]
async fn ingest_paths(paths: Vec<String>) -> Result<Vec<DroppedFile>, String> {
    tauri::async_runtime::spawn_blocking(move || files::ingest_many(&paths))
        .await
        .map_err(|e| e.to_string())?
}

/// Fallback drop path: raw bytes of one file, its name in the `x-name` header.
#[tauri::command]
fn ingest_bytes(request: tauri::ipc::Request) -> Result<DroppedFile, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file's bytes".into());
    };
    if bytes.len() > 100 * 1024 * 1024 {
        return Err("That file is too big to drop (100 MB max).".into());
    }
    let name = request
        .headers()
        .get("x-name")
        .and_then(|v| v.to_str().ok())
        .map(webdrop::percent_decode)
        .unwrap_or_else(|| "file".into());
    log::line(format!("drop: {} byte(s) for {name} (fallback)", bytes.len()));
    files::ingest_bytes(&name, bytes)
}

/// "Correct it" on a .docx: writes "<name> (corrigé).docx" with tracked changes.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn docfix(
    app: AppHandle,
    shared: State<'_, Shared>,
    local: State<'_, LocalChat>,
    outputs: State<'_, Outputs>,
    name: String,
    path: String,
    source: String,
    stream_id: u64,
) -> Result<DocFixResult, String> {
    let s = shared.settings.lock().unwrap().clone();
    let result = docfix::fix_docx(&app, &s, &local, &outputs, &name, &path, &source, stream_id).await;
    if let Err(e) = &result {
        log::line(format!("docfix {name} failed: {e}"));
    }
    result
}

/// "Correct it" on a text / code file: writes "<name> (corrigé).<ext>".
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn filefix(
    app: AppHandle,
    shared: State<'_, Shared>,
    local: State<'_, LocalChat>,
    outputs: State<'_, Outputs>,
    name: String,
    path: String,
    source: String,
    stream_id: u64,
) -> Result<DocFixResult, String> {
    let s = shared.settings.lock().unwrap().clone();
    let result = filefix::fix_file(&app, &s, &local, &outputs, &name, &path, &source, stream_id).await;
    if let Err(e) = &result {
        log::line(format!("filefix {name} failed: {e}"));
    }
    result
}

/// Opens (or shows in Explorer) a file Coucou itself wrote — nothing else.
#[tauri::command]
fn open_output(outputs: State<Outputs>, path: String, reveal: bool) -> bool {
    let p = std::path::PathBuf::from(&path);
    if !outputs.0.lock().unwrap().contains(&p) || !p.is_file() {
        return false;
    }
    let mut cmd = Command::new("explorer.exe");
    if reveal {
        // explorer parses "/select,<path>" as one argument.
        cmd.raw_arg(format!("/select,\"{}\"", p.display()));
    } else {
        cmd.arg(&p);
    }
    cmd.spawn().is_ok()
}

/// The island may only ask whether a key exists — never read it.
#[tauri::command]
fn secret_present(key: String) -> bool {
    secrets::present(&key)
}

#[tauri::command]
fn secret_set(key: String, value: String) -> Result<(), String> {
    secrets::set(&key, &value)
}

#[tauri::command]
fn secret_clear(key: String) -> Result<(), String> {
    secrets::clear(&key)
}

/// Opens the configured n8n instance — the URL lives in the Credential Manager.
#[tauri::command]
fn open_n8n() {
    if let Some(url) = secrets::get("n8n-url") {
        open_url(url);
    }
}

/// Refresh buttons in the integration cards.
#[tauri::command]
async fn refresh_integration(app: AppHandle, id: String) {
    integrations::poll_once(app, &id).await;
}

/// Lets the island write to the same log as the Rust side.
#[tauri::command]
fn log_line(message: String) {
    log::line(format!("ui  {message}"));
}

// ── GitHub push ───────────────────────────────────────────────────────────────

/// Stage, commit and push a local project to GitHub with the stored token.
#[tauri::command]
async fn git_push(path: String, message: String) -> Result<gitpush::PushResult, String> {
    let result = tauri::async_runtime::spawn_blocking(move || gitpush::push_project(&path, &message))
        .await
        .map_err(|e| e.to_string())?;
    if let Err(e) = &result {
        log::line(format!("git push failed: {e}"));
    }
    result
}

/// Creates a new GitHub repo from a local folder, then commits and pushes it.
#[tauri::command]
async fn git_create_repo(path: String, message: String, is_public: bool) -> Result<gitpush::PushResult, String> {
    let token = secrets::get("github-token")
        .ok_or_else(|| "No GitHub token. Settings… → Integrations → GitHub, then paste a token with 'repo' scope.".to_string())?;

    let state = {
        let p = path.clone();
        tauri::async_runtime::spawn_blocking(move || gitpush::repo_state(&p)).await.map_err(|e| e.to_string())??
    };
    if state.has_origin {
        // Already linked — just push instead of creating a duplicate.
        let p = path.clone();
        return tauri::async_runtime::spawn_blocking(move || gitpush::push_project(&p, &message))
            .await
            .map_err(|e| e.to_string())?;
    }
    let name = state.suggested_name;

    // Create the repository on the user's account.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post("https://api.github.com/user/repos")
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "Coucou")
        .json(&serde_json::json!({ "name": name, "private": !is_public, "auto_init": false }))
        .send()
        .await
        .map_err(|e| format!("Couldn't reach GitHub: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let msg = body
            .get("errors")
            .and_then(|e| e.get(0))
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| body.get("message").and_then(|m| m.as_str()))
            .unwrap_or("unknown error");
        if status.as_u16() == 422 && msg.to_lowercase().contains("already exists") {
            return Err(format!("You already have a repository named '{name}'. Rename the folder, or push to the existing one."));
        }
        if status.as_u16() == 403 || status.as_u16() == 401 {
            return Err("GitHub refused: the token lacks the 'repo' scope (needed to create repositories).".into());
        }
        return Err(format!("GitHub couldn't create the repo: {msg}"));
    }
    let owner = body
        .get("owner")
        .and_then(|o| o.get("login"))
        .and_then(|l| l.as_str())
        .ok_or("GitHub didn't return the repo owner.")?
        .to_string();
    let real_name = body.get("name").and_then(|n| n.as_str()).unwrap_or(&name).to_string();

    let p = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || gitpush::create_and_push(&p, &message, &owner, &real_name))
        .await
        .map_err(|e| e.to_string())?;
    if let Err(e) = &result {
        log::line(format!("git create+push failed: {e}"));
    }
    result
}

/// Whether a folder is a repo and already has an origin — picks push vs create.
#[tauri::command]
async fn git_repo_state(path: String) -> Result<gitpush::RepoState, String> {
    tauri::async_runtime::spawn_blocking(move || gitpush::repo_state(&path))
        .await
        .map_err(|e| e.to_string())?
}

// ── Throw Mochi at a window → scan a repo / page ────────────────────────────────

/// Start watching the throw gesture: when the button is released, Rust reads the
/// window under the cursor and emits `throw-url`.
#[tauri::command]
fn begin_throw(app: AppHandle, shared: State<Shared>) {
    island::watch_throw(app, shared.gate.clone());
}

/// Fetch and summarise a repo or web page into a context file the chat attaches.
#[tauri::command]
async fn scan_url(url: String) -> Result<scan::Scanned, String> {
    let result = scan::scan_url(&url).await;
    if let Err(e) = &result {
        log::line(format!("scan {url} failed: {e}"));
    }
    result
}

// ── Settings window ───────────────────────────────────────────────────────────

/// WebView2 allows exactly one browser environment per app, and its options are
/// fixed by whichever webview is created first. Every window must therefore ask
/// for the *same* arguments as the island (see `additionalBrowserArgs` in
/// tauri.conf.json) — a mismatch makes the second window come up blank, with no
/// error anywhere.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// In a dev build the pages are served by Vite, so the second window needs the
/// absolute dev URL; a bundled build resolves it inside the app bundle.
fn settings_page_url(app: &AppHandle) -> WebviewUrl {
    #[cfg(dev)]
    if let Some(mut base) = app.config().build.dev_url.clone() {
        base.set_path("/settings.html");
        return WebviewUrl::External(base);
    }
    let _ = app;
    WebviewUrl::App("settings.html".into())
}

/// The settings window is created hidden at launch and only ever shown and
/// hidden afterwards. A WebView2 window created later — on the main thread or
/// not — silently comes up blank in this app, so the window that works is the
/// one that exists before the island's webview does.
fn create_settings_window(app: &AppHandle) {
    let url = settings_page_url(app);
    match WebviewWindowBuilder::new(app, "settings", url)
        .additional_browser_args(BROWSER_ARGS)
        .title("Settings — Coucou")
        .inner_size(560.0, 680.0)
        .min_inner_size(460.0, 480.0)
        .resizable(true)
        .visible(false)
        .center()
        .build()
    {
        Ok(win) => {
            // Closing it must only hide it, or it could never be reopened.
            let hidden = win.clone();
            win.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = hidden.hide();
                }
            });
        }
        Err(err) => log::line(format!("settings window failed: {err}")),
    }
}

/// Same dance as the settings window: created hidden at launch so WebView2 is
/// happy, then only shown / hidden / moved afterwards.
fn ghost_page_url(app: &AppHandle) -> WebviewUrl {
    #[cfg(dev)]
    if let Some(mut base) = app.config().build.dev_url.clone() {
        base.set_path("/ghost.html");
        return WebviewUrl::External(base);
    }
    let _ = app;
    WebviewUrl::App("ghost.html".into())
}

/// The Mochi that follows the cursor during a "throw" — a small, transparent,
/// click-through, always-on-top window. Click-through matters twice over: it
/// keeps the ghost out of the way, and it keeps WindowFromPoint (used to read
/// the window under the drop) from ever returning the ghost itself.
fn create_ghost_window(app: &AppHandle) {
    let url = ghost_page_url(app);
    match WebviewWindowBuilder::new(app, "ghost", url)
        .additional_browser_args(BROWSER_ARGS)
        .title("Coucou")
        .inner_size(64.0, 98.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .build()
    {
        Ok(win) => {
            island::make_non_activating(&win);
            let _ = win.set_ignore_cursor_events(true);
        }
        Err(err) => log::line(format!("ghost window failed: {err}")),
    }
}

pub fn show_settings_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("settings") else {
        log::line("settings window missing");
        return;
    };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn open_settings_window(app: AppHandle) {
    show_settings_window(&app);
}

pub fn run() {
    let loaded = settings::load();
    let gate = Arc::new(PollGate::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            let _ = app.emit_to(island::WINDOW_LABEL, "tray", "open".to_string());
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .manage(Shared {
            settings: Mutex::new(loaded.clone()),
            gate: gate.clone(),
        })
        .manage(Pending::default())
        .manage(Chat::default())
        .manage(LocalChat::default())
        .manage(Outputs::default())
        .invoke_handler(tauri::generate_handler![
            boot,
            save_settings,
            set_collapsed,
            set_island_rect,
            focus_window,
            reposition,
            open_url,
            open_in_vscode,
            quit_app,
            hooks_status,
            hooks_preview,
            hooks_apply,
            approval_decision,
            approval_ack,
            approval_decline,
            log_line,
            chat_send,
            chat_reset,
            ollama_status,
            ingest_file,
            ingest_paths,
            ingest_bytes,
            docfix,
            filefix,
            open_output,
            git_push,
            git_create_repo,
            git_repo_state,
            begin_throw,
            scan_url,
            secret_present,
            secret_set,
            secret_clear,
            refresh_integration,
            open_n8n,
            open_settings_window,
            set_paused,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::build(&handle)?;
            // Before the island: see create_settings_window.
            create_settings_window(&handle);
            create_ghost_window(&handle);

            if let Some(win) = island::window(&handle) {
                webdrop::install(&win);
                island::make_non_activating(&win);
                island::apply_geometry(&handle, &loaded.screen, false);
                let _ = win.show();
            }
            gate.collapsed.store(false, Ordering::Relaxed);
            gate.set_active(true);
            island::spawn_cursor_poll(handle.clone(), gate.clone());

            log::line(format!("--- Coucou {} started ---", env!("CARGO_PKG_VERSION")));
            hooks::ensure_hook_exe(&handle);
            pipe::start(handle.clone());
            quickfix::register(&handle, &loaded.quickfix_shortcut);
            integrations::start(handle.clone());
            clipboard::spawn_watcher(handle.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Coucou");
}
