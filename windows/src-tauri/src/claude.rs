// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as document/image/text blocks.
//
// Everything happens here rather than in the island: the API key never leaves
// the Credential Manager, and file bytes never cross the IPC boundary.

use std::collections::HashSet;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::secrets;

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Server-side fallback: on a policy decline the API retries the same request on
/// a fallback model inside the same call, so the island never shows a dead end.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 8192;

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
You may use light Markdown (bold, bullet or numbered lists, headings, > quotes, and ``` code blocks) when it helps readability.";

#[derive(Default)]
pub struct Chat {
    /// Full multi-turn history, including tool_use / tool_result blocks.
    messages: Mutex<Vec<Value>>,
    /// Paths of the files already sent in this conversation, so each rides
    /// along once and not with every turn.
    attached: Mutex<HashSet<String>>,
}

impl Chat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
        self.attached.lock().unwrap().clear();
    }

    fn is_empty(&self) -> bool {
        self.messages.lock().unwrap().is_empty()
    }

    fn push(&self, message: Value) {
        self.messages.lock().unwrap().push(message);
    }

    fn pop(&self) {
        self.messages.lock().unwrap().pop();
    }

    fn snapshot(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    /// Several dropped files (or the contents of a dropped folder).
    Files { files: Vec<FileRef> },
    File {
        name: String,
        path: String,
    },
    Window {
        app_name: String,
        title: String,
        url: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileRef {
    pub name: String,
    pub path: String,
}

impl ChatContext {
    /// The files this context carries, whatever its shape.
    pub fn files(&self) -> Vec<FileRef> {
        match self {
            ChatContext::File { name, path } => vec![FileRef { name: name.clone(), path: path.clone() }],
            ChatContext::Files { files } => files.clone(),
            ChatContext::Window { .. } => Vec::new(),
        }
    }
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatStats {
    /// Generation speed (output tokens per second). 0 when unknown.
    pub tokens_per_sec: f64,
    /// Output (answer) tokens.
    pub eval_count: u64,
    /// Prompt (input) tokens.
    pub prompt_count: u64,
    /// Total round-trip time in milliseconds.
    pub total_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
    /// Timing/usage for the turn (filled for Ollama; None for Claude).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<ChatStats>,
}

/// One chat turn. Returns the assistant's text, or a message the island shows
/// in the note view.
pub async fn send(chat: &Chat, model: &str, query: String, context: Option<ChatContext>) -> Result<ChatReply, String> {
    let key = secrets::get("anthropic-api-key").ok_or_else(|| "API key missing. Open settings.".to_string())?;

    let mut content: Vec<Value> = Vec::new();

    // Each file rides along once per conversation — on whichever turn it first
    // appears. Window context only on the first message, like ClaudeService.
    let mut attached_now: Vec<String> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();
    let files = context.as_ref().map(ChatContext::files).unwrap_or_default();
    for f in files {
        if chat.attached.lock().unwrap().contains(&f.path) {
            continue;
        }
        let path_c = f.path.clone();
        let block = tauri::async_runtime::spawn_blocking(move || file_block(&path_c))
            .await
            .map_err(|e| e.to_string())?;
        match block {
            Some(block) => {
                content.push(block);
                content.push(json!({ "type": "text", "text": format!("File: {}", f.name) }));
                chat.attached.lock().unwrap().insert(f.path.clone());
                attached_now.push(f.path);
            }
            None => unreadable.push(f.name),
        }
    }
    if content.is_empty() && !unreadable.is_empty() {
        return Err(format!(
            "{} isn't a format I can read yet (PDF, images, Word, text and code work).",
            unreadable.join(", ")
        ));
    }
    if !unreadable.is_empty() {
        content.push(json!({ "type": "text", "text": format!("(Could not read: {})", unreadable.join(", ")) }));
    }
    if let Some(ChatContext::Window { app_name, title, url }) = &context {
        if chat.is_empty() {
            let mut text = format!("Context — App: {app_name}, Window: {title}");
            if let Some(url) = url {
                text.push_str(&format!(", URL: {url}"));
            }
            content.push(json!({ "type": "text", "text": text }));
        }
    }
    content.push(json!({ "type": "text", "text": query }));

    chat.push(json!({ "role": "user", "content": content }));

    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM_PROMPT,
        "tools": [{ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }],
        "fallbacks": "default",
        "messages": chat.snapshot(),
    });

    let forget_file = || {
        let mut set = chat.attached.lock().unwrap();
        for p in &attached_now {
            set.remove(p);
        }
    };

    let response = match call(&key, &body).await {
        Ok(v) => v,
        Err(err) => {
            chat.pop(); // keep the history consistent with what the model saw
            forget_file();
            return Err(err);
        }
    };

    // A policy decline comes back as HTTP 200 with stop_reason "refusal".
    if response.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        chat.pop();
        forget_file();
        let why = response
            .get("stop_details")
            .and_then(|d| d.get("explanation"))
            .and_then(Value::as_str)
            .unwrap_or("Claude declined this one.");
        return Err(why.to_string());
    }

    let Some(blocks) = response.get("content").and_then(Value::as_array).cloned() else {
        chat.pop();
        forget_file();
        return Err("Unexpected API response.".into());
    };

    // Store the whole content — tool_use / tool_result blocks included — so the
    // next turn has the right context.
    chat.push(json!({ "role": "assistant", "content": blocks.clone() }));

    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err("No response text.".into());
    }
    Ok(ChatReply { text, stats: None })
}

/// One-shot request outside the chat history (proofreading jobs): no tools, no
/// memory, just the text back.
pub async fn complete(model: &str, system: &str, user: &str) -> Result<String, String> {
    let key = secrets::get("anthropic-api-key").ok_or_else(|| "API key missing. Open settings.".to_string())?;
    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "fallbacks": "default",
        "messages": [{ "role": "user", "content": user }],
    });
    let response = call(&key, &body).await?;
    if response.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return Err("Claude declined this one.".into());
    }
    let text = response
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err("No response text.".into());
    }
    Ok(text)
}

async fn call(key: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(ENDPOINT)
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        // Surface the API's own message, which is what makes a bad key obvious.
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("Claude API {status}: {detail}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))
}

/// PDF → document block, image → image block, Word → extracted text,
/// text/code → inline text. Mirrors readFileAsBlock() in ClaudeService.swift.
fn file_block(path: &str) -> Option<Value> {
    use crate::attach::{self, Kind};

    let native = match attach::extension(path).as_str() {
        "pdf" => Some(("document", "application/pdf")),
        "jpg" | "jpeg" => Some(("image", "image/jpeg")),
        "png" => Some(("image", "image/png")),
        "gif" => Some(("image", "image/gif")),
        "webp" => Some(("image", "image/webp")),
        _ => None,
    };

    if let Some((block_type, media)) = native {
        let bytes = std::fs::read(path).ok()?;
        // The API caps images at ~5 MB; big photos are shrunk instead of refused.
        if block_type == "image" && bytes.len() > 4_500_000 {
            let jpeg = attach::shrink_image(&bytes)?;
            return Some(image_block("image/jpeg", &jpeg));
        }
        return Some(json!({
            "type": block_type,
            "source": { "type": "base64", "media_type": media, "data": base64(&bytes) },
        }));
    }

    match attach::kind_of(path) {
        // BMP, TIFF… → JPEG.
        Kind::Image => {
            let jpeg = attach::shrink_image(&std::fs::read(path).ok()?)?;
            Some(image_block("image/jpeg", &jpeg))
        }
        Kind::Word => {
            let text = attach::word_text(path).ok()?;
            Some(json!({ "type": "text", "text": format!("File contents:\n{text}") }))
        }
        _ => {
            let text = attach::read_text(path)?;
            Some(json!({ "type": "text", "text": format!("File contents:\n{text}") }))
        }
    }
}

fn image_block(media: &str, bytes: &[u8]) -> Value {
    json!({
        "type": "image",
        "source": { "type": "base64", "media_type": media, "data": base64(bytes) },
    })
}

/// Small standalone base64 encoder — not worth another dependency.
/// Also used for Stripe's basic auth.
pub(crate) fn base64_for(bytes: &[u8]) -> String {
    base64(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
