// Whichever engine the user picked, for one-shot jobs (proofreading a Word
// document, correcting a selection): no chat history involved.

use crate::claude;
use crate::ollama::{self, LocalChat};
use crate::settings::Settings;

pub async fn complete(settings: &Settings, local: &LocalChat, system: &str, user: &str) -> Result<String, String> {
    if settings.provider == "ollama" {
        let cfg = ollama_config(settings);
        ollama::complete(local, &cfg, system, user).await
    } else {
        claude::complete(&settings.model, system, user).await
    }
}

pub fn ollama_config(s: &Settings) -> ollama::Config {
    ollama::Config {
        url: s.ollama_url.clone(),
        model: s.ollama_model.clone(),
        ctx: s.ollama_ctx.clamp(2048, 131_072),
    }
}

/// Rough number of characters of input one request may carry, leaving room for
/// an answer of the same size (a corrected text is as long as the original).
pub fn input_budget(s: &Settings) -> usize {
    if s.provider == "ollama" {
        // ~3 chars per token, input + output + instructions in one window.
        (s.ollama_ctx.clamp(2048, 131_072) as usize * 3 / 2 - 600).max(1500)
    } else {
        // Claude's output cap (8k tokens) is the limit, not its context.
        12_000
    }
}

/// Models like to wrap answers in quotes, code fences or a "Here is…" line.
pub fn clean_answer(text: &str) -> String {
    let mut t = text.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // drop an optional language tag on the fence line
        let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or(rest);
        t = rest.trim_end().strip_suffix("```").unwrap_or(rest).trim();
    }
    for (open, close) in [("\"", "\""), ("«", "»"), ("“", "”")] {
        if t.len() > 2 && t.starts_with(open) && t.ends_with(close) && t.matches(open).count() == 1 {
            t = t[open.len()..t.len() - close.len()].trim();
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::clean_answer;

    #[test]
    fn wrappers_are_removed() {
        assert_eq!(clean_answer("```text\nBonjour.\n```"), "Bonjour.");
        assert_eq!(clean_answer("« Bonjour. »"), "Bonjour.");
        assert_eq!(clean_answer("  Bonjour.  "), "Bonjour.");
        assert_eq!(clean_answer("\"a\" et \"b\""), "\"a\" et \"b\"");
    }
}
