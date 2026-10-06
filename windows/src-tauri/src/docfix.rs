// "Correct it" on a Word document: the model proofreads it paragraph by
// paragraph, and Coucou writes "<name> (corrigé).docx" next to the original
// with every correction as a Word tracked change (author "Mochi"), so the user
// reviews them in Word with Review → Accept / Reject.
//
// The original is never modified. Only word/document.xml is rewritten in the
// copy, and only the paragraphs that actually changed: everything else —
// styles, images, tables, headers — is copied byte for byte.
//
// A changed paragraph is rebuilt with its own paragraph properties and the
// formatting of its first run. Paragraphs whose structure a rebuild could
// damage (fields, links, images, comments, existing revisions, bookmarks,
// line breaks, tabs, mixed formatting) are left as they are.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::engine;
use crate::ollama::LocalChat;
use crate::settings::Settings;
use crate::textdiff::{self, Op};

const SYSTEM_PROMPT: &str = "You are a meticulous proofreader. You receive numbered paragraphs from one document. \
Correct spelling, grammar, conjugation, agreement, punctuation and typography, in each paragraph's own language. \
Do not rephrase, do not reorder, do not translate, do not change the meaning, the tone, names, numbers or technical terms. \
Output every paragraph, in the same order and in exactly the same format: the tag [[n]] followed by the corrected paragraph on a single line. \
If a paragraph has no mistake, output it unchanged. Output nothing else — no title, no explanation, no comment.";

/// Paths this module wrote, the only ones `open_output` agrees to open.
#[derive(Default)]
pub struct Outputs(pub Mutex<HashSet<PathBuf>>);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub stream_id: u64,
    pub done: usize,
    pub total: usize,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocFixResult {
    pub output: String,
    pub output_name: String,
    /// Paragraphs that received at least one correction.
    pub changed_paragraphs: usize,
    /// Individual corrections (tracked changes).
    pub corrections: usize,
    /// Paragraphs sent to the model.
    pub checked: usize,
    /// Paragraphs left alone because a rebuild could damage their structure.
    pub skipped_complex: usize,
    /// Model answers refused because they rewrote instead of correcting.
    pub rejected: usize,
}

/// A paragraph of document.xml we may rewrite.
struct Para {
    start: usize,
    end: usize,
    ppr: String,
    rpr: String,
    text: String,
}

pub async fn fix_docx(
    app: &AppHandle,
    settings: &Settings,
    local: &LocalChat,
    outputs: &Outputs,
    name: &str,
    inbox_path: &str,
    source_path: &str,
    stream_id: u64,
) -> Result<DocFixResult, String> {
    if crate::attach::extension(inbox_path) != "docx" {
        return Err("Only Word .docx files can be corrected in place.".into());
    }
    let xml = read_entry(inbox_path, "word/document.xml")?;
    let (paras, skipped_complex) = paragraphs(&xml);
    let candidates: Vec<usize> = paras
        .iter()
        .enumerate()
        .filter(|(_, p)| p.text.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        .map(|(i, _)| i)
        .collect();
    if candidates.is_empty() {
        return Err(format!("I found no text I can safely correct in {name}."));
    }

    // Batches that fit the engine's window.
    let budget = engine::input_budget(settings);
    let mut batches: Vec<Vec<usize>> = vec![Vec::new()];
    let mut size = 0;
    for &i in &candidates {
        let len = paras[i].text.chars().count() + 8;
        if size + len > budget && !batches.last().unwrap().is_empty() {
            batches.push(Vec::new());
            size = 0;
        }
        batches.last_mut().unwrap().push(i);
        size += len;
    }

    let total = batches.len();
    let emit = |done: usize| {
        let _ = app.emit("docfix-progress", Progress { stream_id, done, total, name: name.to_string() });
    };
    emit(0);

    let mut corrected: HashMap<usize, String> = HashMap::new();
    for (b, batch) in batches.iter().enumerate() {
        let prompt: String = batch
            .iter()
            .enumerate()
            .map(|(k, &i)| format!("[[{}]] {}\n", k + 1, paras[i].text))
            .collect();
        let answer = engine::complete(settings, local, SYSTEM_PROMPT, &prompt).await?;
        let parsed = parse_numbered(&answer);
        for (k, &i) in batch.iter().enumerate() {
            if let Some(t) = parsed.get(&(k + 1)) {
                corrected.insert(i, t.clone());
            }
        }
        emit(b + 1);
    }

    // Rebuild document.xml from the end so earlier offsets stay valid.
    let mut out = xml.clone();
    let mut next_id: u32 = 90_000;
    let date = iso_now();
    let mut changed_paragraphs = 0;
    let mut corrections = 0;
    let mut rejected = 0;
    for &i in candidates.iter().rev() {
        let p = &paras[i];
        let Some(new_text) = corrected.get(&i) else { continue };
        let new_text = new_text.trim();
        // Keep the original's surrounding spaces: models trim them.
        let lead = &p.text[..p.text.len() - p.text.trim_start().len()];
        let trail = &p.text[p.text.trim_end().len()..];
        let new_text = format!("{lead}{new_text}{trail}");
        if new_text == p.text {
            continue;
        }
        if !textdiff::looks_like_correction(&p.text, &new_text) {
            rejected += 1;
            continue;
        }
        let ops = textdiff::diff(&p.text, &new_text);
        let n = textdiff::change_count(&ops);
        if n == 0 {
            continue;
        }
        let rebuilt = build_paragraph(p, &ops, &mut next_id, &date);
        out.replace_range(p.start..p.end, &rebuilt);
        changed_paragraphs += 1;
        corrections += n;
    }

    let dest = output_path(name, source_path);
    write_copy(inbox_path, &dest, &out)?;
    outputs.0.lock().unwrap().insert(dest.clone());
    crate::log::line(format!(
        "docfix {name}: {corrections} correction(s) in {changed_paragraphs} paragraph(s), {rejected} refused, {skipped_complex} skipped → {}",
        dest.display()
    ));

    Ok(DocFixResult {
        output_name: dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        output: dest.to_string_lossy().to_string(),
        changed_paragraphs,
        corrections,
        checked: candidates.len(),
        skipped_complex,
        rejected,
    })
}

// ── Reading document.xml ─────────────────────────────────────────────────────

fn read_entry(path: &str, entry: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "This isn't a valid Word document.".to_string())?;
    let mut xml = String::new();
    zip.by_name(entry)
        .map_err(|_| "This isn't a valid Word document.".to_string())?
        .take(50_000_000)
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    Ok(xml)
}

/// Markup a rebuild would lose or break. Paragraphs containing any of it are
/// never touched.
const COMPLEX: &[&str] = &[
    "<w:fldChar", "<w:instrText", "<w:fldSimple", "<w:hyperlink", "<w:drawing", "<w:pict", "<w:object",
    "<w:footnoteReference", "<w:endnoteReference", "<w:commentReference", "<w:commentRangeStart",
    "<w:ins ", "<w:ins>", "<w:del ", "<w:del>", "<w:moveFrom", "<w:moveTo", "<w:sdt", "<mc:AlternateContent",
    "<w:tab/>", "<w:tab ", "<w:br", "<w:cr", "<w:sym", "<w:bookmarkStart", "<w:smartTag", "<w:customXml",
    "<m:oMath", "<w:ruby", "<w:noBreakHyphen", "<w:softHyphen", "<w:ptab",
];

/// Leaf paragraphs (no paragraph nested inside, as text boxes do) that are
/// simple enough to rebuild, plus the count of those skipped as complex.
fn paragraphs(xml: &str) -> (Vec<Para>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    // Stack of (start offset, has_child_paragraph).
    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut pos = 0;
    while let Some(rel) = xml[pos..].find('<') {
        let at = pos + rel;
        let rest = &xml[at..];
        if rest.starts_with("<w:p>") || rest.starts_with("<w:p ") {
            if let Some(parent) = stack.last_mut() {
                parent.1 = true;
            }
            // Self-closing empty paragraph: nothing to correct.
            let close = rest.find('>').unwrap_or(0);
            if rest[..=close].ends_with("/>") {
                pos = at + close + 1;
                continue;
            }
            stack.push((at, false));
            pos = at + 4;
        } else if rest.starts_with("</w:p>") {
            let end = at + "</w:p>".len();
            if let Some((start, has_child)) = stack.pop() {
                if !has_child {
                    match simple_paragraph(&xml[start..end]) {
                        Some((ppr, rpr, text)) => out.push(Para { start, end, ppr, rpr, text }),
                        None => {
                            if !plain_text(&xml[start..end]).trim().is_empty() {
                                skipped += 1;
                            }
                        }
                    }
                }
            }
            pos = end;
        } else {
            pos = at + 1;
        }
    }
    (out, skipped)
}

/// (paragraph properties, first run properties, text) — or None when the
/// paragraph holds anything a rebuild could damage.
fn simple_paragraph(p: &str) -> Option<(String, String, String)> {
    if COMPLEX.iter().any(|m| p.contains(m)) {
        return None;
    }
    let ppr = element(p, "w:pPr").unwrap_or_default();
    let body = if ppr.is_empty() { p } else { &p[p.find(&ppr)? + ppr.len()..] };

    // Every run must carry the same formatting: bold words inside a sentence
    // would otherwise be flattened.
    let mut rprs: Vec<String> = Vec::new();
    let mut search = body;
    while let Some(i) = find_tag(search, "w:r") {
        let run_end = search[i..].find("</w:r>").map(|e| i + e + "</w:r>".len())?;
        let run = &search[i..run_end];
        rprs.push(strip_proofing(&element(run, "w:rPr").unwrap_or_default()));
        search = &search[run_end..];
    }
    if rprs.is_empty() {
        return None;
    }
    if rprs.iter().any(|r| r != &rprs[0]) {
        return None;
    }
    let text = plain_text(body);
    if text.trim().is_empty() {
        return None;
    }
    // The rebuild keeps the first run's properties as they are, language
    // included, so Word keeps spell-checking in the right language.
    let first_run = &body[find_tag(body, "w:r")?..];
    let first_run = &first_run[..first_run.find("</w:r>")?];
    let rpr = element(first_run, "w:rPr").unwrap_or_default();
    Some((ppr, rpr, text))
}

/// Proofing marks (spelling/grammar squiggles) differ between runs of the same
/// formatting; they must not count as a formatting difference.
fn strip_proofing(rpr: &str) -> String {
    let mut s = rpr.to_string();
    for tag in ["<w:noProof/>", "<w:lang "] {
        while let Some(i) = s.find(tag) {
            let end = s[i..].find("/>").map(|e| i + e + 2).unwrap_or(s.len());
            s.replace_range(i..end, "");
        }
    }
    if s == "<w:rPr></w:rPr>" {
        String::new()
    } else {
        s
    }
}

/// Position of `<tag>` or `<tag ` (not `<tagSomething`).
fn find_tag(s: &str, tag: &str) -> Option<usize> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = s[from..].find(&open) {
        let at = from + i;
        match s[at + open.len()..].chars().next() {
            Some('>') | Some(' ') | Some('/') => return Some(at),
            _ => from = at + open.len(),
        }
    }
    None
}

/// First `<tag …>…</tag>` (or `<tag/>`) as a string.
fn element(s: &str, tag: &str) -> Option<String> {
    let start = find_tag(s, tag)?;
    let head_end = start + s[start..].find('>')?;
    if s[..=head_end].ends_with("/>") {
        return Some(s[start..=head_end].to_string());
    }
    let close = format!("</{tag}>");
    let end = head_end + s[head_end..].find(&close)? + close.len();
    Some(s[start..end].to_string())
}

/// Concatenated <w:t> contents, unescaped.
fn plain_text(p: &str) -> String {
    let mut out = String::new();
    let mut rest = p;
    while let Some(i) = find_tag(rest, "w:t") {
        let Some(h) = rest[i..].find('>') else { break };
        let head_end = i + h;
        if rest[..=head_end].ends_with("/>") {
            rest = &rest[head_end + 1..];
            continue;
        }
        let Some(c) = rest[head_end..].find("</w:t>") else { break };
        out.push_str(&unescape(&rest[head_end + 1..head_end + c]));
        rest = &rest[head_end + c..];
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// ── Model answer ─────────────────────────────────────────────────────────────

/// "[[1]] text\n[[2]] text" → {1: text, 2: text}. Tolerates missing line
/// breaks, extra blank lines and a stray preamble.
pub fn parse_numbered(answer: &str) -> HashMap<usize, String> {
    let mut out = HashMap::new();
    let mut marks: Vec<(usize, usize, usize)> = Vec::new(); // (tag start, text start, number)
    let mut from = 0;
    while let Some(i) = answer[from..].find("[[") {
        let at = from + i;
        let digits: String = answer[at + 2..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let close = at + 2 + digits.len();
        if !digits.is_empty() && answer[close..].starts_with("]]") {
            marks.push((at, close + 2, digits.parse().unwrap_or(0)));
            from = close + 2;
        } else {
            from = at + 2;
        }
    }
    for (k, &(_, text_start, n)) in marks.iter().enumerate() {
        let text_end = marks.get(k + 1).map(|m| m.0).unwrap_or(answer.len());
        let text = answer[text_start..text_end].trim().replace('\n', " ");
        out.entry(n).or_insert(text);
    }
    out
}

// ── Writing ──────────────────────────────────────────────────────────────────

fn build_paragraph(p: &Para, ops: &[(Op, String)], next_id: &mut u32, date: &str) -> String {
    let run = |t: &str| format!("<w:r>{}<w:t xml:space=\"preserve\">{}</w:t></w:r>", p.rpr, escape(t));
    let del_run = |t: &str| format!("<w:r>{}<w:delText xml:space=\"preserve\">{}</w:delText></w:r>", p.rpr, escape(t));
    let mut s = String::from("<w:p>");
    s.push_str(&p.ppr);
    for (op, text) in ops {
        match op {
            Op::Equal => s.push_str(&run(text)),
            Op::Delete => {
                s.push_str(&format!("<w:del w:id=\"{}\" w:author=\"Mochi\" w:date=\"{date}\">{}</w:del>", next_id, del_run(text)));
                *next_id += 1;
            }
            Op::Insert => {
                s.push_str(&format!("<w:ins w:id=\"{}\" w:author=\"Mochi\" w:date=\"{date}\">{}</w:ins>", next_id, run(text)));
                *next_id += 1;
            }
        }
    }
    s.push_str("</w:p>");
    s
}

/// "<stem> (corrigé).docx" next to the original, or in Documents when that
/// folder isn't writable; never overwrites anything.
fn output_path(name: &str, source: &str) -> PathBuf {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "document".into());
    let dirs: Vec<PathBuf> = [
        Path::new(source).parent().map(Path::to_path_buf),
        std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Documents")),
        Some(crate::files::inbox_dir()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let inbox = crate::files::inbox_dir();
    let last = dirs.len().saturating_sub(1);
    for (k, dir) in dirs.into_iter().enumerate() {
        // A file that only exists in the inbox (dropped without a path) gets
        // its corrected copy in Documents, where the user will find it.
        if k < last && dir == inbox {
            continue;
        }
        if !dir.is_dir() || !writable(&dir) {
            continue;
        }
        for i in 1..1000 {
            let file = if i == 1 { format!("{stem} (corrigé).docx") } else { format!("{stem} (corrigé {i}).docx") };
            let candidate = dir.join(file);
            if !candidate.exists() {
                return candidate;
            }
        }
    }
    crate::files::inbox_dir().join(format!("{stem} (corrigé).docx"))
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

/// Copies every entry of the original archive, replacing document.xml.
fn write_copy(src: &str, dest: &Path, document_xml: &str) -> Result<(), String> {
    let file = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut zin = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let tmp = dest.with_extension("docx.part");
    let out = std::fs::File::create(&tmp).map_err(|e| format!("Cannot write {}: {e}", dest.display()))?;
    let mut zout = zip::ZipWriter::new(out);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let result = (|| -> Result<(), String> {
        for i in 0..zin.len() {
            let mut entry = zin.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().to_string();
            if entry.is_dir() {
                zout.add_directory(name, opts).map_err(|e| e.to_string())?;
                continue;
            }
            zout.start_file(name.clone(), opts).map_err(|e| e.to_string())?;
            if name == "word/document.xml" {
                zout.write_all(document_xml.as_bytes()).map_err(|e| e.to_string())?;
            } else {
                std::io::copy(&mut entry, &mut zout).map_err(|e| e.to_string())?;
            }
        }
        zout.finish().map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

/// UTC timestamp in the form Word expects (2026-09-30T18:04:00Z).
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = concat!(
        r#"<w:document><w:body>"#,
        r#"<w:p><w:pPr><w:pStyle w:val="Titre"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Un titre</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Il sont </w:t></w:r><w:proofErr w:type="spellStart"/><w:r><w:t>venu</w:t></w:r><w:r><w:t xml:space="preserve"> hier &amp; aujourd'hui.</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Avec </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>gras</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Lien</w:t></w:r><w:hyperlink r:id="x"><w:r><w:t>ici</w:t></w:r></w:hyperlink></w:p>"#,
        r#"<w:p/>"#,
        r#"</w:body></w:document>"#
    );

    #[test]
    fn finds_simple_paragraphs_only() {
        let (paras, skipped) = paragraphs(DOC);
        let texts: Vec<&str> = paras.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["Un titre", "Il sont venu hier & aujourd'hui."]);
        assert_eq!(skipped, 2); // mixed formatting + hyperlink
        assert_eq!(paras[0].ppr, r#"<w:pPr><w:pStyle w:val="Titre"/></w:pPr>"#);
        assert_eq!(paras[0].rpr, "<w:rPr><w:b/></w:rPr>");
    }

    #[test]
    fn numbered_answers_parse() {
        let a = "Voici :\n[[1]] Un titre\n\n[[2]] Ils sont venus hier.\n[[3]]Sans espace";
        let m = parse_numbered(a);
        assert_eq!(m[&1], "Un titre");
        assert_eq!(m[&2], "Ils sont venus hier.");
        assert_eq!(m[&3], "Sans espace");
    }

    #[test]
    fn rebuilt_paragraph_has_tracked_changes() {
        let (paras, _) = paragraphs(DOC);
        let p = &paras[1];
        let ops = textdiff::diff(&p.text, "Ils sont venus hier & aujourd'hui.");
        let mut id = 1;
        let xml = build_paragraph(p, &ops, &mut id, "2026-01-01T00:00:00Z");
        assert!(xml.contains(r#"<w:del w:id="1" w:author="Mochi""#));
        assert!(xml.contains("<w:delText xml:space=\"preserve\">Il</w:delText>"));
        assert!(xml.contains("<w:t xml:space=\"preserve\">Ils</w:t></w:r></w:ins>"));
        assert!(xml.contains("&amp; aujourd"));
        assert_eq!(plain_text(&xml.replace("w:delText", "w:x")), "Ils sont venus hier & aujourd'hui.");
    }

    #[test]
    fn timestamp_format() {
        let t = iso_now();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }
}
