// Clipboard reaction: a light background watch on the clipboard so Mochi can
// offer to help the moment you copy a chunk of text or a link anywhere.
//
// It polls the clipboard sequence number (cheap, no clipboard open) every ~700 ms
// and only reads the text when it actually changed. Coucou's own copy/paste is
// skipped (quickfix is running, or the text matches what we just handled), and a
// disabled setting parks the whole thing. The island decides whether to show the
// prompt — it ignores the event while it is open or busy.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::Shared;

/// Shorter than this isn't worth interrupting for (a word, a number copied to paste).
const MIN_CHARS: usize = 12;
/// Longer than this belongs in a file drop, where the answer can be reviewed.
const MAX_CHARS: usize = 8_000;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ClipboardEvent {
    /// "url" when the whole clipboard is a single link, else "text".
    kind: &'static str,
    /// First line / start of the text, for the prompt.
    preview: String,
    chars: usize,
    /// The full copied text, so the chosen action can use it without a round-trip.
    text: String,
}

#[cfg(windows)]
pub fn spawn_watcher(app: AppHandle) {
    use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;

    std::thread::spawn(move || {
        let mut last_seq = unsafe { GetClipboardSequenceNumber() };
        let mut last_text = String::new();
        loop {
            std::thread::sleep(Duration::from_millis(700));

            let enabled = app
                .try_state::<Shared>()
                .map(|s| s.settings.lock().unwrap().clipboard_reaction)
                .unwrap_or(false);
            if !enabled {
                // Keep the sequence current so re-enabling doesn't fire on old content.
                last_seq = unsafe { GetClipboardSequenceNumber() };
                continue;
            }

            let seq = unsafe { GetClipboardSequenceNumber() };
            if seq == last_seq {
                continue;
            }
            last_seq = seq;

            // Our own correction copies/pastes: not worth reacting to.
            if crate::quickfix::is_running() {
                continue;
            }

            let Some(text) = clip_get() else { continue };
            let trimmed = text.trim();
            let chars = trimmed.chars().count();
            if chars < MIN_CHARS || chars > MAX_CHARS {
                continue;
            }
            if trimmed == last_text {
                continue; // same thing copied again
            }
            last_text = trimmed.to_string();

            let kind = if is_url(trimmed) { "url" } else { "text" };
            let preview: String = trimmed.chars().take(90).collect();
            let _ = app.emit(
                "clipboard",
                ClipboardEvent { kind, preview, chars, text: trimmed.to_string() },
            );
        }
    });
}

#[cfg(not(windows))]
pub fn spawn_watcher(_app: AppHandle) {}

fn clip_get() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// True when the whole clipboard is a single http(s) URL (no spaces).
fn is_url(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("http://") || s.starts_with("https://")) && !s.contains(char::is_whitespace)
}
