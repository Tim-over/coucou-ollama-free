// Local chat through Ollama (https://ollama.com) — the private, offline twin of
// claude.rs. Same shape: multi-turn history kept here, files read here, and the
// island only ever sees text.
//
// Replies are streamed: every piece of text is emitted as a `chat-chunk` event
// so the answer appears word by word in the chat, which matters a lot for a
// model running on a laptop GPU.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::attach;
use crate::claude::{ChatContext, ChatReply, ChatStats};

pub const DEFAULT_URL: &str = "http://localhost:11434";
pub const DEFAULT_CTX: u32 = 8192;

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You run locally on the user's computer through Ollama, so you have no internet access: say so plainly if a question needs live information. \
You can read the files, PDFs and images the user drops on you. \
Always respond in the user's language. Be thorough and precise. \
When asked to correct or proofread a text, first give the full corrected text, then a short list of the changes you made and why. \
You may use light Markdown (bold, bullet or numbered lists, headings, > quotes, and ``` code blocks) when it helps readability.";

#[derive(Default)]
pub struct LocalChat {
    messages: Mutex<Vec<Value>>,
    /// Paths of the files already sent in this conversation, so each rides
    /// along once and not with every turn.
    attached: Mutex<std::collections::HashSet<String>>,
    /// Model name → capabilities reported by /api/show.
    caps: Mutex<HashMap<String, Vec<String>>>,
}

impl LocalChat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
        self.attached.lock().unwrap().clear();
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Chunk {
    stream_id: u64,
    text: String,
}

pub struct Config {
    pub url: String,
    pub model: String,
    pub ctx: u32,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        // Ollama is on this machine (or the LAN): never route it through a proxy.
        .no_proxy()
        .connect_timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| e.to_string())
}

pub fn normalise_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let url = if url.is_empty() { DEFAULT_URL } else { url };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("The Ollama address must start with http:// or https://".into());
    }
    Ok(url.to_string())
}

fn unreachable(url: &str) -> String {
    format!("Ollama isn't answering at {url}. Start Ollama (it lives in the notification area) or check the address in Settings.")
}

// ── Status & models ───────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
    pub size: u64,
    pub parameter_size: Option<String>,
    pub vision: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub running: bool,
    pub version: Option<String>,
    pub models: Vec<ModelInfo>,
    pub error: Option<String>,
}

pub async fn status(chat: &LocalChat, url: &str) -> Status {
    let url = match normalise_url(url) {
        Ok(u) => u,
        Err(e) => return Status { running: false, version: None, models: vec![], error: Some(e) },
    };
    let Ok(http) = client() else {
        return Status { running: false, version: None, models: vec![], error: Some("HTTP client failed".into()) };
    };

    let version = match http
        .get(format!("{url}/api/version"))
        .timeout(Duration::from_secs(4))
        .send()
        .await
    {
        Ok(r) => r.json::<Value>().await.ok().and_then(|v| v["version"].as_str().map(str::to_string)),
        Err(_) => {
            return Status { running: false, version: None, models: vec![], error: Some(unreachable(&url)) }
        }
    };

    let tags: Value = match http.get(format!("{url}/api/tags")).timeout(Duration::from_secs(6)).send().await {
        Ok(r) => r.json().await.unwrap_or(Value::Null),
        Err(e) => {
            return Status { running: true, version, models: vec![], error: Some(e.to_string()) }
        }
    };

    let mut models = Vec::new();
    for m in tags["models"].as_array().cloned().unwrap_or_default() {
        let Some(name) = m["name"].as_str().map(str::to_string) else { continue };
        let caps = capabilities(chat, &http, &url, &name).await;
        models.push(ModelInfo {
            vision: caps.iter().any(|c| c == "vision"),
            size: m["size"].as_u64().unwrap_or(0),
            parameter_size: m["details"]["parameter_size"].as_str().map(str::to_string),
            name,
        });
    }
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Status { running: true, version, models, error: None }
}

/// "completion", "vision", "thinking", "tools"… — cached per model.
async fn capabilities(chat: &LocalChat, http: &reqwest::Client, url: &str, model: &str) -> Vec<String> {
    if let Some(c) = chat.caps.lock().unwrap().get(model) {
        return c.clone();
    }
    let caps: Vec<String> = match http
        .post(format!("{url}/api/show"))
        .timeout(Duration::from_secs(6))
        .json(&json!({ "model": model }))
        .send()
        .await
    {
        Ok(r) => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["capabilities"].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| c.as_str().map(str::to_string))
            .collect(),
        Err(_) => return Vec::new(), // not cached: Ollama may just be starting
    };
    chat.caps.lock().unwrap().insert(model.to_string(), caps.clone());
    caps
}

// ── Chat ──────────────────────────────────────────────────────────────────────

/// One chat turn against the local model, streamed into `chat-chunk` events.
pub async fn send(
    app: &AppHandle,
    chat: &LocalChat,
    cfg: Config,
    query: String,
    context: Option<ChatContext>,
    stream_id: u64,
) -> Result<ChatReply, String> {
    let url = normalise_url(&cfg.url)?;
    if cfg.model.trim().is_empty() {
        return Err("No local model chosen. Open Settings → AI engine and pick an Ollama model.".into());
    }
    let http = client()?;
    let caps = capabilities(chat, &http, &url, &cfg.model).await;
    let vision = caps.iter().any(|c| c == "vision");

    // ~3 characters per token, keeping a third of the window for the question,
    // the history and the answer.
    let char_budget = (cfg.ctx as usize * 3 * 2 / 3).max(4_000);

    let mut text_parts: Vec<String> = Vec::new();
    let mut images: Vec<String> = Vec::new();
    let mut note: Option<String> = None;

    let files: Vec<_> = context
        .as_ref()
        .map(ChatContext::files)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !chat.attached.lock().unwrap().contains(&f.path))
        .collect();
    let mut attached_now: Vec<String> = Vec::new();
    if !files.is_empty() {
        // Text files share the window; images don't count against it.
        let texty = files.iter().filter(|f| attach::kind_of(&f.path) != attach::Kind::Image).count().max(1);
        let per_file = (char_budget / texty).max(1_500);
        let mut notes: Vec<String> = Vec::new();
        for f in files {
            let (name_c, path_c) = (f.name.clone(), f.path.clone());
            // PDF rendering and image decoding are CPU work: keep them off
            // the async runtime.
            let prepared = tauri::async_runtime::spawn_blocking(move || {
                attach::prepare_for_local(&name_c, &path_c, per_file)
            })
            .await
            .map_err(|e| e.to_string())?;
            let prepared = match prepared {
                Ok(p) => p,
                Err(e) => {
                    notes.push(e);
                    continue;
                }
            };
            if !prepared.images.is_empty() && !vision {
                notes.push(format!("{}: {} can't see images (try: ollama pull gemma3).", f.name, cfg.model));
                continue;
            }
            text_parts.push(prepared.text);
            images.extend(prepared.images);
            if let Some(n) = prepared.note {
                notes.push(format!("{}: {n}", f.name));
            }
            attached_now.push(f.path);
        }
        if attached_now.is_empty() {
            return Err(notes.join("\n"));
        }
        if !notes.is_empty() {
            note = Some(notes.join(" "));
        }
        chat.attached.lock().unwrap().extend(attached_now.iter().cloned());
    }
    if let Some(ChatContext::Window { app_name, title, url }) = &context {
        if chat.messages.lock().unwrap().is_empty() {
            let mut t = format!("Context — App: {app_name}, Window: {title}");
            if let Some(u) = url {
                t.push_str(&format!(", URL: {u}"));
            }
            text_parts.push(t);
        }
    }
    text_parts.push(query);

    let mut user = json!({ "role": "user", "content": text_parts.join("\n\n") });
    if !images.is_empty() {
        user["images"] = json!(images);
    }
    chat.messages.lock().unwrap().push(user);

    let mut messages = vec![json!({ "role": "system", "content": SYSTEM_PROMPT })];
    messages.extend(chat.messages.lock().unwrap().iter().cloned());

    let mut body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "keep_alive": "15m",
        "options": { "num_ctx": cfg.ctx },
    });
    // Reasoning models (qwen3, deepseek-r1…) would otherwise print pages of
    // "thinking" before answering. Only sent to models that understand it.
    if caps.iter().any(|c| c == "thinking") {
        body["think"] = json!(false);
    }

    let result = stream_reply(app, &http, &url, &body, stream_id).await;
    let (text, stats) = match result {
        Ok(t) => t,
        Err(e) => {
            // Keep the history consistent with what the model actually saw.
            chat.messages.lock().unwrap().pop();
            let mut set = chat.attached.lock().unwrap();
            for p in &attached_now {
                set.remove(p);
            }
            return Err(e);
        }
    };

    let text = strip_think(&text);
    if text.is_empty() {
        chat.messages.lock().unwrap().pop();
        return Err("The model returned an empty answer.".into());
    }
    chat.messages.lock().unwrap().push(json!({ "role": "assistant", "content": text }));

    let text = match note {
        Some(n) => format!("{text}\n\n({n})"),
        None => text,
    };
    Ok(ChatReply { text, stats })
}

/// One-shot request outside the chat history (proofreading jobs).
pub async fn complete(chat: &LocalChat, cfg: &Config, system: &str, user: &str) -> Result<String, String> {
    let url = normalise_url(&cfg.url)?;
    if cfg.model.trim().is_empty() {
        return Err("No local model chosen. Open Settings → AI engine and pick an Ollama model.".into());
    }
    let http = client()?;
    let caps = capabilities(chat, &http, &url, &cfg.model).await;
    let mut body = json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": false,
        "keep_alive": "15m",
        // Low temperature: a proofreader should be boring.
        "options": { "num_ctx": cfg.ctx, "temperature": 0.1 },
    });
    if caps.iter().any(|c| c == "thinking") {
        body["think"] = json!(false);
    }
    let response = http
        .post(format!("{url}/api/chat"))
        .timeout(Duration::from_secs(600))
        .json(&body)
        .send()
        .await
        .map_err(|e| if e.is_connect() { unreachable(&url) } else { format!("Ollama: {e}") })?;
    let status = response.status();
    let v: Value = response.json().await.map_err(|e| format!("Ollama: {e}"))?;
    if let Some(err) = v["error"].as_str() {
        if status.as_u16() == 404 {
            return Err(format!("{err}. Download it with: ollama pull {}", cfg.model));
        }
        return Err(format!("Ollama: {err}"));
    }
    let text = strip_think(v["message"]["content"].as_str().unwrap_or(""));
    if text.is_empty() {
        return Err("The model returned an empty answer.".into());
    }
    Ok(text)
}

async fn stream_reply(
    app: &AppHandle,
    http: &reqwest::Client,
    url: &str,
    body: &Value,
    stream_id: u64,
) -> Result<(String, Option<ChatStats>), String> {
    let mut response = http
        .post(format!("{url}/api/chat"))
        .json(body)
        .send()
        .await
        .map_err(|e| if e.is_connect() { unreachable(url) } else { format!("Ollama: {e}") })?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| text.chars().take(200).collect());
        if status.as_u16() == 404 && detail.contains("not found") {
            return Err(format!("{detail}. Download it with: ollama pull {}", body["model"].as_str().unwrap_or("")));
        }
        return Err(format!("Ollama {status}: {detail}"));
    }

    let mut full = String::new();
    let mut stats: Option<ChatStats> = None;
    let mut pending: Vec<u8> = Vec::new();
    // No overall timeout (a long answer on a CPU can take minutes), but a
    // stream that goes silent for this long is dead.
    let idle = Duration::from_secs(300);
    loop {
        let chunk = match tokio::time::timeout(idle, response.chunk()).await {
            Err(_) => return Err("Ollama stopped answering (5 min without output).".into()),
            Ok(Err(e)) => return Err(format!("Ollama connection lost: {e}")),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(bytes))) => bytes,
        };
        pending.extend_from_slice(&chunk);
        // NDJSON: one JSON object per line.
        while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=nl).collect();
            if let Some(done) = handle_line(app, &line, stream_id, &mut full, &mut stats)? {
                if done {
                    return Ok((full, stats));
                }
            }
        }
    }
    if !pending.is_empty() {
        let line = std::mem::take(&mut pending);
        handle_line(app, &line, stream_id, &mut full, &mut stats)?;
    }
    Ok((full, stats))
}

/// Returns Some(true) on the final line, Some(false) on a content line.
fn handle_line(
    app: &AppHandle,
    line: &[u8],
    stream_id: u64,
    full: &mut String,
    stats: &mut Option<ChatStats>,
) -> Result<Option<bool>, String> {
    let line = std::str::from_utf8(line).unwrap_or("").trim();
    if line.is_empty() {
        return Ok(None);
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Ok(None) };
    if let Some(err) = v["error"].as_str() {
        return Err(format!("Ollama: {err}"));
    }
    if let Some(piece) = v["message"]["content"].as_str() {
        if !piece.is_empty() {
            full.push_str(piece);
            let _ = app.emit("chat-chunk", Chunk { stream_id, text: piece.to_string() });
        }
    }
    let done = v["done"].as_bool().unwrap_or(false);
    // The final line carries Ollama's timing counters (nanoseconds).
    if done {
        let eval_count = v["eval_count"].as_u64().unwrap_or(0);
        let eval_ns = v["eval_duration"].as_u64().unwrap_or(0);
        let prompt_count = v["prompt_eval_count"].as_u64().unwrap_or(0);
        let total_ns = v["total_duration"].as_u64().unwrap_or(0);
        let tokens_per_sec = if eval_ns > 0 { eval_count as f64 / (eval_ns as f64 / 1e9) } else { 0.0 };
        if eval_count > 0 || total_ns > 0 {
            *stats = Some(ChatStats {
                tokens_per_sec,
                eval_count,
                prompt_count,
                total_ms: total_ns / 1_000_000,
            });
        }
    }
    Ok(Some(done))
}

/// Older reasoning models inline their thoughts in <think>…</think>.
fn strip_think(text: &str) -> String {
    let mut out = text.to_string();
    while let Some(start) = out.find("<think>") {
        match out[start..].find("</think>") {
            Some(end) => out.replace_range(start..start + end + "</think>".len(), ""),
            None => {
                out.truncate(start);
                break;
            }
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_blocks_are_removed() {
        assert_eq!(strip_think("<think>hmm</think>\nBonjour"), "Bonjour");
        assert_eq!(strip_think("Salut"), "Salut");
        assert_eq!(strip_think("A<think>unfinished"), "A");
    }

    #[test]
    fn urls_are_normalised() {
        assert_eq!(normalise_url("").unwrap(), DEFAULT_URL);
        assert_eq!(normalise_url("http://192.168.1.5:11434/").unwrap(), "http://192.168.1.5:11434");
        assert!(normalise_url("file:///c:/x").is_err());
    }
}
