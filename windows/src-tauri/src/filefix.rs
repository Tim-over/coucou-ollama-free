// "Correct it" on a plain-text file (txt, md, code…): the model proofreads the
// human-language text and Coucou writes "<name> (corrigé).<ext>" next to the
// original. The original is never modified.
//
// .docx has its own path (docfix.rs, Word tracked changes). This one is for
// everything read as text. Line structure is preserved: the file is corrected in
// line-aligned chunks and reassembled, and a chunk whose answer doesn't look
// like a correction (a rewrite, a reply, a wrong line count) is kept as it was,
// so the user never gets a mangled file.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter};

use crate::docfix::{DocFixResult, Outputs, Progress};
use crate::engine;
use crate::ollama::LocalChat;
use crate::settings::Settings;
use crate::{attach, textdiff};

/// Prose files get a plain proofreading prompt; code files a strict one that
/// must not touch anything the machine reads.
const PROSE_PROMPT: &str = "You are a meticulous proofreader. You receive numbered lines from one text file. \
Correct spelling, grammar, conjugation, agreement, punctuation and typography, in the text's own language. \
Do not rephrase, do not reorder, do not translate, do not change the meaning or tone. \
Output exactly the same lines, in the same order, each as [[n]] followed by the corrected line. \
Keep empty lines (output [[n]] with nothing after it). If a line has no mistake, output it unchanged. Output nothing else.";

const CODE_PROMPT: &str = "You are a meticulous proofreader working on a source code file. \
You receive numbered lines. Correct ONLY spelling and grammar mistakes in human-language text: comments and user-facing strings. \
NEVER change code: keep every identifier, keyword, operator, number, symbol, indentation and the structure exactly as they are. \
Never translate. Never touch a line that is pure code. \
Output exactly the same lines, in the same order, each as [[n]] followed by the line. \
Keep empty lines (output [[n]] with nothing after it). Output nothing else.";

/// Extensions treated as source code (strict prompt, higher safety bar).
const CODE_EXT: &[&str] = &[
    "py", "rs", "js", "ts", "jsx", "tsx", "c", "h", "cpp", "cc", "hpp", "cs", "java", "kt", "go", "rb", "php",
    "swift", "sh", "bash", "ps1", "sql", "html", "css", "scss", "vue", "svelte", "toml", "yaml", "yml", "json",
    "xml", "ini", "lua", "r", "m", "pl", "dart", "scala", "clj", "ex", "exs",
];

#[allow(dead_code)]
pub fn is_correctable(path: &str) -> bool {
    matches!(attach::kind_of(path), attach::Kind::Text)
}

fn is_code(path: &str) -> bool {
    CODE_EXT.contains(&attach::extension(path).as_str())
}

pub async fn fix_file(
    app: &AppHandle,
    settings: &Settings,
    local: &LocalChat,
    outputs: &Outputs,
    name: &str,
    inbox_path: &str,
    source_path: &str,
    stream_id: u64,
) -> Result<DocFixResult, String> {
    let text = attach::read_text(inbox_path)
        .ok_or_else(|| format!("I can't read {name} as text (it may be binary or too large)."))?;

    let code = is_code(inbox_path);
    let prompt = if code { CODE_PROMPT } else { PROSE_PROMPT };

    // Windows files are CRLF; remember it so the corrected copy keeps the same.
    let crlf = text.contains("\r\n");
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();

    // Line-aligned batches within the engine's budget.
    let budget = engine::input_budget(settings);
    let mut batches: Vec<(usize, usize)> = Vec::new(); // [start, end) into `lines`
    let (mut start, mut size) = (0usize, 0usize);
    for (i, l) in lines.iter().enumerate() {
        let cost = l.chars().count() + 10;
        if size + cost > budget && i > start {
            batches.push((start, i));
            start = i;
            size = 0;
        }
        size += cost;
    }
    batches.push((start, lines.len()));

    let total = batches.len();
    let emit = |done: usize| {
        let _ = app.emit("docfix-progress", Progress { stream_id, done, total, name: name.to_string() });
    };
    emit(0);

    let mut out_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let mut corrections = 0usize;
    let mut changed_lines = 0usize;
    let mut rejected = 0usize;

    for (b, &(from, to)) in batches.iter().enumerate() {
        // A batch of only blank lines has nothing to correct.
        if lines[from..to].iter().all(|l| l.trim().is_empty()) {
            emit(b + 1);
            continue;
        }
        let numbered: String = (from..to).map(|i| format!("[[{}]] {}\n", i - from + 1, lines[i])).collect();
        let answer = engine::complete(settings, local, prompt, &numbered).await?;
        let parsed = crate::docfix::parse_numbered(&answer);

        for i in from..to {
            let Some(new) = parsed.get(&(i - from + 1)) else { continue };
            let old = lines[i];
            if new == old {
                continue;
            }
            // Per-line safety: keep the line unless the change reads as a real
            // correction (guards against a model that rewrites or explains).
            if !textdiff::looks_like_correction(old, new) {
                rejected += 1;
                continue;
            }
            let n = textdiff::change_count(&textdiff::diff(old, new));
            if n == 0 {
                continue;
            }
            out_lines[i] = new.clone();
            corrections += n;
            changed_lines += 1;
        }
        emit(b + 1);
    }

    let mut body = out_lines.join("\n");
    if crlf {
        body = body.replace('\n', "\r\n");
    }

    let dest = output_path(name, source_path);
    let tmp = dest.with_extension(format!("{}.part", attach::extension(&dest.to_string_lossy())));
    std::fs::write(&tmp, body.as_bytes()).map_err(|e| format!("Cannot write {}: {e}", dest.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    outputs.0.lock().unwrap().insert(dest.clone());
    crate::log::line(format!(
        "filefix {name}: {corrections} correction(s) on {changed_lines} line(s), {rejected} refused → {}",
        dest.display()
    ));

    Ok(DocFixResult {
        output_name: dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        output: dest.to_string_lossy().to_string(),
        changed_paragraphs: changed_lines,
        corrections,
        checked: lines.len(),
        skipped_complex: 0,
        rejected,
    })
}

/// "<stem> (corrigé).<ext>" next to the original, or in Documents when that
/// folder isn't writable; never overwrites anything.
fn output_path(name: &str, source: &str) -> PathBuf {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "fichier".into());
    let ext = attach::extension(name);
    let dot_ext = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    let inbox = crate::files::inbox_dir();
    let dirs: Vec<PathBuf> = [
        Path::new(source).parent().map(Path::to_path_buf),
        std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Documents")),
        Some(inbox.clone()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let last = dirs.len().saturating_sub(1);
    for (k, dir) in dirs.into_iter().enumerate() {
        // A file that lives only in the inbox (dropped without a path) gets its
        // corrected copy in Documents, where the user will find it.
        if k < last && dir == inbox {
            continue;
        }
        if !dir.is_dir() || !writable(&dir) {
            continue;
        }
        for i in 1..1000 {
            let file = if i == 1 {
                format!("{stem} (corrigé){dot_ext}")
            } else {
                format!("{stem} (corrigé {i}){dot_ext}")
            };
            let candidate = dir.join(file);
            if !candidate.exists() {
                return candidate;
            }
        }
    }
    inbox.join(format!("{stem} (corrigé){dot_ext}"))
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".coucou-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}
