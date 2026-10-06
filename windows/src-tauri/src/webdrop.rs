// File drops, the WebView2 way.
//
// Earlier builds let wry's OLE drop target handle drops and revoked the one
// WebView2 registers on its render widget, so that wry's would win. On recent
// WebView2 runtimes that revoke no longer takes (the widget belongs to the
// browser process), WebView2's own target keeps winning, and every drop was
// refused with the "no drop" cursor — nothing ever reached the app.
//
// So the island now accepts drops like any web page (dragDropEnabled: false in
// tauri.conf.json leaves WebView2's handling alone), and hands the dropped
// File objects to us with `chrome.webview.postMessageWithAdditionalObjects`.
// WebView2 turns each one into an ICoreWebView2File carrying the full path —
// folders included — which we emit to the island as `files-dropped`.
//
// If that API is missing (very old runtime), the page falls back to sending the
// bytes of each file to `ingest_bytes`.

use tauri::{Emitter, Manager, WebviewWindow};

/// The message the page posts along with the files.
pub const DROP_MESSAGE: &str = "__coucou_drop__";

#[cfg(windows)]
pub fn install(win: &WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2File, ICoreWebView2WebMessageReceivedEventArgs2,
    };
    use webview2_com::{take_pwstr, WebMessageReceivedEventHandler};
    use windows_core::{Interface, PWSTR};

    let app = win.app_handle().clone();
    let label = win.label().to_string();
    let result = win.with_webview(move |platform| unsafe {
        let core = match platform.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                crate::log::line(format!("webdrop: no CoreWebView2 ({e})"));
                return;
            }
        };
        let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut raw = PWSTR::null();
            if args.TryGetWebMessageAsString(&mut raw).is_err() {
                return Ok(()); // not a string: not ours
            }
            if take_pwstr(raw) != DROP_MESSAGE {
                return Ok(());
            }
            let mut paths: Vec<String> = Vec::new();
            if let Ok(args2) = args.cast::<ICoreWebView2WebMessageReceivedEventArgs2>() {
                if let Ok(objects) = args2.AdditionalObjects() {
                    let mut count = 0u32;
                    let _ = objects.Count(&mut count);
                    for i in 0..count {
                        let Ok(obj) = objects.GetValueAtIndex(i) else { continue };
                        let Ok(file) = obj.cast::<ICoreWebView2File>() else { continue };
                        let mut p = PWSTR::null();
                        if file.Path(&mut p).is_ok() {
                            let path = take_pwstr(p);
                            if !path.is_empty() {
                                paths.push(path);
                            }
                        }
                    }
                }
            }
            crate::log::line(format!("drop: {} path(s) from WebView2", paths.len()));
            let _ = app.emit_to(label.as_str(), "files-dropped", paths);
            Ok(())
        }));
        let mut token = 0i64;
        match core.add_WebMessageReceived(&handler, &mut token) {
            Ok(()) => crate::log::line("webdrop: drop handler installed".to_string()),
            Err(e) => crate::log::line(format!("webdrop: add_WebMessageReceived failed ({e})")),
        }
    });
    if let Err(e) = result {
        crate::log::line(format!("webdrop: with_webview failed ({e})"));
    }
}

#[cfg(not(windows))]
pub fn install(_win: &WebviewWindow) {}

/// Percent-decoding for the file name header (headers are ASCII-only, so the
/// page sends `encodeURIComponent(name)`).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn decodes_names() {
        assert_eq!(percent_decode("rapport%20%C3%A9t%C3%A9.docx"), "rapport été.docx");
        assert_eq!(percent_decode("plain.txt"), "plain.txt");
        assert_eq!(percent_decode("bad%2"), "bad%2");
        assert_eq!(percent_decode("a%zz"), "a%zz");
    }
}
