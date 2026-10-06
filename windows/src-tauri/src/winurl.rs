// Reads the URL of the browser window under a screen point — the "throw Mochi at
// a window" gesture.
//
// Primary method (reliable on every browser): bring that window to the front and
// simulate Ctrl+L then Ctrl+C, which selects and copies the active tab's URL in
// Chrome, Edge, Firefox, Opera, Opera GX, Brave, Vivaldi… Coucou reads the
// clipboard, then puts the user's clipboard back. If that yields nothing, it
// falls back to reading the address bar through UI Automation.

#[derive(Debug, Clone)]
pub struct UnderPoint {
    pub url: Option<String>,
    pub over_self: bool,
    pub title: String,
    /// Owning application's pretty name (e.g. "Chrome", "Visual Studio Code").
    pub app: String,
}

impl UnderPoint {
    fn none() -> Self {
        UnderPoint { url: None, over_self: false, title: String::new(), app: String::new() }
    }
}

#[cfg(windows)]
pub fn url_under_point(x: i32, y: i32) -> UnderPoint {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};

    let pt = POINT { x, y };
    let hwnd = unsafe { WindowFromPoint(pt) };
    if hwnd.0.is_null() {
        return UnderPoint::none();
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let root = if root.0.is_null() { hwnd } else { root };
    let title = window_title(root);
    let app = window_app_name(root);

    // 1) Clipboard method — focus the window, Ctrl+L, Ctrl+C, read, restore.
    let (clip_url, own) = clipboard_url(root);
    if own {
        return UnderPoint { url: None, over_self: true, title, app };
    }
    if let Some(url) = clip_url.as_deref().and_then(url_like) {
        crate::log::line(format!("winurl: window=\"{title}\" app=\"{app}\" via=clipboard url=yes"));
        return UnderPoint { url: Some(url), over_self: false, title, app };
    }

    // 2) Fallback — read the address bar through UI Automation.
    match uia_url(root) {
        Ok(UiaOutcome::OwnWebview) => UnderPoint { url: None, over_self: true, title, app },
        Ok(UiaOutcome::Url(u)) => {
            crate::log::line(format!("winurl: window=\"{title}\" app=\"{app}\" via=uia url=yes"));
            UnderPoint { url: Some(u), over_self: false, title, app }
        }
        Ok(UiaOutcome::None { fields, sample }) => {
            crate::log::line(format!(
                "winurl: window=\"{title}\" app=\"{app}\" via=none clip={:?} fields={fields} [{sample}]",
                clip_url.unwrap_or_default().chars().take(40).collect::<String>()
            ));
            UnderPoint { url: None, over_self: false, title, app }
        }
        Err(_) => UnderPoint { url: None, over_self: false, title, app },
    }
}

#[cfg(not(windows))]
pub fn url_under_point(_x: i32, _y: i32) -> UnderPoint {
    UnderPoint::none()
}

// ── Clipboard method ────────────────────────────────────────────────────────

/// Returns (url_text, is_our_own_window). Focuses `root`, copies its address
/// bar via Ctrl+L / Ctrl+C, reads the clipboard and restores it.
#[cfg(windows)]
fn clipboard_url(root: windows::Win32::Foundation::HWND) -> (Option<String>, bool) {
    use std::time::{Duration, Instant};
    use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

    let saved = clip_get();
    let before = unsafe { GetClipboardSequenceNumber() };

    // Bring the target to the front so the keystrokes reach it.
    unsafe {
        let _ = SetForegroundWindow(root);
    }
    std::thread::sleep(Duration::from_millis(80));
    let fg = unsafe { GetForegroundWindow() };
    // If the front window is still ours (the drop landed on Coucou), bail as self.
    if !fg.0.is_null() && window_is_ours(fg) {
        return (None, true);
    }

    // Ctrl+L focuses + selects the address bar; Ctrl+C copies it.
    send_ctrl(0x4C); // 'L'
    std::thread::sleep(Duration::from_millis(70));
    send_ctrl(0x43); // 'C'

    // Wait for the clipboard to actually change.
    let start = Instant::now();
    let mut changed = false;
    while start.elapsed() < Duration::from_millis(700) {
        if unsafe { GetClipboardSequenceNumber() } != before {
            changed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let copied = if changed { clip_get() } else { None };
    // Give the address bar its normal look back, then restore the clipboard.
    send_key(0x1B); // Escape
    if let Some(prev) = saved {
        clip_set(&prev);
    }

    let own = copied.as_deref().map(is_own_webview).unwrap_or(false);
    (copied, own)
}

#[cfg(windows)]
fn clip_get() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

#[cfg(windows)]
fn clip_set(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}

/// Ctrl + `vk`, pressed and released, via SendInput.
#[cfg(windows)]
fn send_ctrl(vk: u16) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL};
    key_events(&[(VK_CONTROL.0, false), (vk, false), (vk, true), (VK_CONTROL.0, true)]);
}

#[cfg(windows)]
fn send_key(vk: u16) {
    key_events(&[(vk, false), (vk, true)]);
}

#[cfg(windows)]
fn key_events(events: &[(u16, bool)]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    let inputs: Vec<INPUT> = events
        .iter()
        .map(|&(vk, up)| INPUT {
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
        })
        .collect();
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// True when the window belongs to this process (so a throw onto Coucou cancels).
#[cfg(windows)]
fn window_is_ours(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid != 0 && pid == unsafe { GetCurrentProcessId() }
}

// ── UI Automation fallback ──────────────────────────────────────────────────

#[cfg(windows)]
enum UiaOutcome {
    Url(String),
    OwnWebview,
    None { fields: usize, sample: String },
}

#[cfg(windows)]
fn uia_url(root: windows::Win32::Foundation::HWND) -> windows::core::Result<UiaOutcome> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::Variant::{VARIANT, VT_I4};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern, TreeScope_Descendants,
        UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
        UIA_ValuePatternId,
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let automation: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let element = unsafe { automation.ElementFromHandle(root)? };

    let value_of = |el: &IUIAutomationElement| -> Option<String> {
        let pattern = unsafe { el.GetCurrentPattern(UIA_ValuePatternId) }.ok()?;
        let vp = pattern.cast::<IUIAutomationValuePattern>().ok()?;
        let v = unsafe { vp.CurrentValue() }.ok()?;
        Some(v.to_string())
    };

    let mut raw_values: Vec<String> = Vec::new();
    for control_type in [UIA_EditControlTypeId.0, UIA_ComboBoxControlTypeId.0, UIA_DocumentControlTypeId.0] {
        let mut cv = VARIANT::default();
        unsafe {
            let inner = &mut *cv.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = control_type;
        }
        let Ok(cond) = (unsafe { automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &cv) }) else { continue };
        let Ok(found) = (unsafe { element.FindAll(TreeScope_Descendants, &cond) }) else { continue };
        let n = unsafe { found.Length() }.unwrap_or(0);
        for i in 0..n {
            if let Ok(el) = unsafe { found.GetElement(i) } {
                if let Some(v) = value_of(&el) {
                    if !v.trim().is_empty() {
                        raw_values.push(v);
                    }
                }
            }
        }
    }
    if let Ok(focused) = unsafe { automation.GetFocusedElement() } {
        if let Some(v) = value_of(&focused) {
            if !v.trim().is_empty() {
                raw_values.push(v);
            }
        }
    }

    if raw_values.iter().any(|v| is_own_webview(v)) {
        return Ok(UiaOutcome::OwnWebview);
    }
    let best = raw_values
        .iter()
        .filter_map(|v| url_like(v))
        .max_by_key(|u| (u.trim_end_matches('/').matches('/').count() >= 3) as usize * 1000 + u.len());
    match best {
        Some(u) => Ok(UiaOutcome::Url(u)),
        None => {
            let sample: Vec<String> = raw_values
                .iter()
                .take(6)
                .map(|v| v.split(['?', '#']).next().unwrap_or("").chars().take(50).collect())
                .collect();
            Ok(UiaOutcome::None { fields: raw_values.len(), sample: sample.join(" | ") })
        }
    }
}

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Pretty application name from the window's owning process executable.
#[cfg(windows)]
fn window_app_name(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return String::new();
    }
    let exe = unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    };
    // Last path component, without .exe, title-cased-ish.
    let file = exe.rsplit(['\\', '/']).next().unwrap_or(&exe);
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    prettify_app(stem)
}

/// Turns an exe stem into a friendly name (known browsers/editors, else capitalised).
fn prettify_app(stem: &str) -> String {
    let low = stem.to_lowercase();
    let known = [
        ("chrome", "Chrome"), ("msedge", "Edge"), ("firefox", "Firefox"), ("opera_gx", "Opera GX"),
        ("opera", "Opera"), ("brave", "Brave"), ("vivaldi", "Vivaldi"), ("code", "VS Code"),
        ("cursor", "Cursor"), ("windowsterminal", "Terminal"), ("explorer", "Explorer"),
        ("notepad", "Notepad"), ("winword", "Word"), ("excel", "Excel"), ("powerpnt", "PowerPoint"),
        ("acrobat", "Acrobat"), ("photoshop", "Photoshop"), ("blender", "Blender"),
    ];
    for (k, v) in known {
        if low.contains(k) {
            return v.to_string();
        }
    }
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(windows)]
fn window_title(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n as usize])
}

/// True for Coucou's own webview address, so its windows are never scanned.
fn is_own_webview(v: &str) -> bool {
    let low = v.to_lowercase();
    low.contains("tauri.localhost") || low.contains("://localhost") || low.starts_with("localhost")
}

/// Turns an address-bar string into a real URL, or None when it isn't one.
fn url_like(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.contains(char::is_whitespace) || is_own_webview(s) {
        return None;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return Some(s.to_string());
    }
    let host = s.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains('.') && !host.contains('@') && host.len() >= 4 && host.split('.').all(|p| !p.is_empty()) {
        return Some(format!("https://{s}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{is_own_webview, url_like};

    #[test]
    fn recognises_urls() {
        assert_eq!(url_like("https://github.com/a/b"), Some("https://github.com/a/b".into()));
        assert_eq!(url_like("github.com/anthropics/claude"), Some("https://github.com/anthropics/claude".into()));
        assert_eq!(url_like("  example.org  "), Some("https://example.org".into()));
        assert_eq!(url_like("search terms here"), None);
        assert_eq!(url_like(""), None);
        assert_eq!(url_like("http://tauri.localhost/"), None);
    }

    #[test]
    fn spots_own_webview() {
        assert!(is_own_webview("http://tauri.localhost/"));
        assert!(is_own_webview("https://localhost:1420/"));
        assert!(!is_own_webview("https://github.com/x"));
    }
}
