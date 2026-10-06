// Global shortcut: correct the text selected in any application, in place.
//
//   1. remember the window the user is typing in, and what is on the clipboard
//   2. wait for the shortcut's keys to be released, send Ctrl+C
//   3. ask the chosen engine for the corrected text only
//   4. check the answer is a correction and not a rewrite or a reply
//   5. put it on the clipboard, send Ctrl+V into the same window
//   6. give the user their clipboard back
//
// The island never takes focus (it is a non-activating window), so the user's
// window is still the foreground one when we paste. If it isn't — the user
// clicked elsewhere meanwhile — nothing is pasted: the correction is left on the
// clipboard and the island says so. Ctrl+Z in the app undoes the paste.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::engine;
use crate::ollama::LocalChat;
use crate::textdiff;
use crate::Shared;

const SYSTEM_PROMPT: &str = "You correct text. Fix spelling, grammar, conjugation, agreement, punctuation and typography, in the text's own language. \
Keep the meaning, the tone, the wording, the line breaks, the formatting (markdown, lists, emojis) and the length. Do not translate. \
Reply with the corrected text only — no comment, no explanation, no quotes, no preamble. If there is no mistake, reply with the text unchanged.";

/// Longer selections belong in a file drop, where the answer can be reviewed.
const MAX_CHARS: usize = 8_000;

static RUNNING: AtomicBool = AtomicBool::new(false);

/// True while a correction is touching the clipboard, so the clipboard watcher
/// doesn't react to Coucou's own copy/paste.
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// "working" | "nothing" | "toolong" | "nochange" | "done" | "rewrite" |
    /// "focus" | "error". The island words the message; `message` carries the
    /// error text.
    pub phase: &'static str,
    pub message: String,
    pub corrections: usize,
}

fn emit(app: &AppHandle, phase: &'static str, message: impl Into<String>, corrections: usize) {
    let _ = app.emit("quickfix", Event { phase, message: message.into(), corrections });
}

/// Registers (or re-registers) the shortcut from the settings. An empty or
/// "off" value just unregisters it.
pub fn register(app: &AppHandle, shortcut: &str) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let shortcut = shortcut.trim();
    if shortcut.is_empty() || shortcut.eq_ignore_ascii_case("off") {
        return;
    }
    let result = gs.on_shortcut(shortcut, |app, _sc, event| {
        if event.state() == ShortcutState::Pressed {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { run(app).await });
        }
    });
    match result {
        Ok(()) => crate::log::line(format!("quickfix shortcut {shortcut} registered")),
        Err(e) => crate::log::line(format!("quickfix shortcut {shortcut} failed: {e}")),
    }
}

async fn run(app: AppHandle) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return; // one correction at a time
    }
    let result = correct_selection(&app).await;
    if let Err(e) = result {
        emit(&app, "error", e, 0);
    }
    RUNNING.store(false, Ordering::SeqCst);
}

async fn correct_selection(app: &AppHandle) -> Result<(), String> {
    let target = win::foreground();
    let saved = clipboard_text();

    // Ctrl+C while the user still holds Alt/Shift would be a different chord.
    tauri::async_runtime::spawn_blocking(|| win::wait_modifiers_released(Duration::from_millis(1500)))
        .await
        .map_err(|e| e.to_string())?;

    let before = win::clipboard_seq();
    win::send_chord(win::VK_C);
    let copied = wait_for(Duration::from_millis(900), || win::clipboard_seq() != before).await;
    if !copied {
        emit(app, "nothing", "", 0);
        return Ok(());
    }
    let text = clipboard_text().unwrap_or_default();
    if text.trim().is_empty() {
        restore(saved);
        emit(app, "nothing", "", 0);
        return Ok(());
    }
    if text.chars().count() > MAX_CHARS {
        restore(saved);
        emit(app, "toolong", "", 0);
        return Ok(());
    }

    emit(app, "working", "", 0);
    let settings = app.state::<Shared>().settings.lock().unwrap().clone();
    let local = app.state::<LocalChat>();
    let answer = match engine::complete(&settings, &local, SYSTEM_PROMPT, &text).await {
        Ok(a) => a,
        Err(e) => {
            restore(saved);
            return Err(e);
        }
    };

    // Keep the selection's own surrounding whitespace (a trailing newline when
    // a whole line was selected, for instance).
    let core = engine::clean_answer(&answer);
    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];
    let corrected = format!("{lead}{core}{trail}");

    if corrected == text {
        restore(saved);
        emit(app, "nochange", "", 0);
        return Ok(());
    }
    if !textdiff::looks_like_correction(&text, &corrected) {
        set_clipboard(&corrected);
        emit(app, "rewrite", "", 0);
        return Ok(());
    }
    let n = textdiff::change_count(&textdiff::diff(&text, &corrected));

    if win::foreground() != target {
        set_clipboard(&corrected);
        emit(app, "focus", "", n);
        return Ok(());
    }

    set_clipboard(&corrected);
    tokio::time::sleep(Duration::from_millis(60)).await;
    win::send_chord(win::VK_V);
    // Let the target read the clipboard before handing it back.
    tokio::time::sleep(Duration::from_millis(500)).await;
    restore(saved);

    emit(app, "done", "", n);
    Ok(())
}

async fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    done()
}

fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn set_clipboard(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}

/// Only text can be put back; an image that was on the clipboard is lost,
/// which is why we only touch the clipboard once a selection was copied.
fn restore(saved: Option<String>) {
    if let Some(t) = saved {
        set_clipboard(&t);
    }
}

#[cfg(windows)]
mod win {
    use std::time::{Duration, Instant};

    use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    pub const VK_C: u16 = 0x43;
    pub const VK_V: u16 = 0x56;

    pub fn foreground() -> isize {
        unsafe { GetForegroundWindow().0 as isize }
    }

    pub fn clipboard_seq() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    pub fn wait_modifiers_released(limit: Duration) {
        let start = Instant::now();
        let held = || unsafe {
            [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
                .iter()
                .any(|k| (GetAsyncKeyState(k.0 as i32) as u16 & 0x8000) != 0)
        };
        while held() && start.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(15));
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    fn key(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// Ctrl + key, pressed and released.
    pub fn send_chord(vk: u16) {
        let inputs = [key(VK_CONTROL.0, false), key(vk, false), key(vk, true), key(VK_CONTROL.0, true)];
        unsafe {
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }
}

#[cfg(not(windows))]
mod win {
    use std::time::Duration;
    pub const VK_C: u16 = 0x43;
    pub const VK_V: u16 = 0x56;
    pub fn foreground() -> isize {
        0
    }
    pub fn clipboard_seq() -> u32 {
        0
    }
    pub fn wait_modifiers_released(_limit: Duration) {}
    pub fn send_chord(_vk: u16) {}
}
