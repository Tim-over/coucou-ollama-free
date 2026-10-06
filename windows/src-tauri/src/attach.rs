// Turns a dropped file into something a model can read, for both engines.
//
// Claude takes PDFs and images natively. A local Ollama model only takes text
// and (for vision models) JPEG/PNG images, so this module also:
//   - pulls the text out of PDFs, Word (.docx) and OpenDocument (.odt) files,
//   - renders the pages of a scanned PDF to images with Windows' own PDF engine,
//   - re-encodes and shrinks images so a 24 MP photo doesn't choke a local model.
//
// Everything stays on this machine: nothing here touches the network.

use std::io::Read;
use std::path::Path;

/// Text and code files are inlined up to this size, as on macOS.
pub const MAX_INLINE_TEXT: u64 = 200_000;
/// Longest side of an image handed to a local model. Vision encoders downscale
/// anyway; sending more only costs time and memory.
const MAX_IMAGE_SIDE: u32 = 1600;
/// Pages rendered when a PDF has no text layer (a scan).
const MAX_SCANNED_PAGES: u32 = 6;
/// Below this many characters of extracted text a PDF is treated as a scan.
const SCAN_THRESHOLD: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Image,
    Word,
    Text,
}

pub fn kind_of(path: &str) -> Kind {
    let ext = extension(path);
    match ext.as_str() {
        "pdf" => Kind::Pdf,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" => Kind::Image,
        "docx" | "odt" => Kind::Word,
        // Anything else is tried as UTF-8 text; binary files fail that test.
        _ => Kind::Text,
    }
}

pub fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

/// What a local model gets for one file.
pub struct Prepared {
    /// Text to put in front of the question (may be empty for a plain image).
    pub text: String,
    /// Base64 JPEG/PNG images, for vision models.
    pub images: Vec<String>,
    /// A short note for the user when something was left out or cut.
    pub note: Option<String>,
}

/// Prepares a file for a local model. `char_budget` bounds the inlined text so
/// the document still fits in the model's context window.
pub fn prepare_for_local(name: &str, path: &str, char_budget: usize) -> Result<Prepared, String> {
    match kind_of(path) {
        Kind::Image => {
            let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {name}: {e}"))?;
            let jpeg = shrink_image(&bytes).ok_or_else(|| {
                format!("{name} isn't an image format I can read (try PNG or JPEG).")
            })?;
            Ok(Prepared {
                text: format!("[Image attached: {name}]"),
                images: vec![crate::claude::base64_for(&jpeg)],
                note: None,
            })
        }
        Kind::Pdf => {
            let text = pdf_text(path).unwrap_or_default();
            if text.trim().chars().count() >= SCAN_THRESHOLD {
                let (text, cut) = clip(&text, char_budget);
                return Ok(Prepared {
                    text: wrap_document(name, &text, cut),
                    images: Vec::new(),
                    note: cut.then(|| "The PDF was long, so only its beginning was read.".into()),
                });
            }
            // No text layer: a scan or a photo of a page. Render the pages so a
            // vision model can read them.
            let pages = render_pdf_pages(path, MAX_SCANNED_PAGES)?;
            if pages.is_empty() {
                return Err(format!("{name} looks empty — no text and no pages to read."));
            }
            let images: Vec<String> = pages
                .iter()
                .map(|png| shrink_image(png).unwrap_or_else(|| png.clone()))
                .map(|b| crate::claude::base64_for(&b))
                .collect();
            Ok(Prepared {
                text: format!(
                    "[Scanned PDF attached: {name} — {} page image(s). Read the text in the images.]",
                    images.len()
                ),
                images,
                note: None,
            })
        }
        Kind::Word => {
            let text = word_text(path).map_err(|e| format!("Cannot read {name}: {e}"))?;
            let (text, cut) = clip(&text, char_budget);
            Ok(Prepared {
                text: wrap_document(name, &text, cut),
                images: Vec::new(),
                note: cut.then(|| "The document was long, so only its beginning was read.".into()),
            })
        }
        Kind::Text => {
            let text = read_text(path).ok_or_else(|| {
                format!("{name} isn't a format I can read yet (PDF, images, Word, text and code work).")
            })?;
            let (text, cut) = clip(&text, char_budget);
            Ok(Prepared {
                text: wrap_document(name, &text, cut),
                images: Vec::new(),
                note: cut.then(|| "The file was long, so only its beginning was read.".into()),
            })
        }
    }
}

fn wrap_document(name: &str, text: &str, cut: bool) -> String {
    let mut out = format!("File: {name}\n<<<\n{text}\n>>>");
    if cut {
        out.push_str("\n(The file was truncated to fit; only the beginning is shown.)");
    }
    out
}

/// Cuts on a char boundary, preferring the end of a line.
fn clip(text: &str, budget: usize) -> (String, bool) {
    if text.chars().count() <= budget {
        return (text.to_string(), false);
    }
    let mut end = text.char_indices().nth(budget).map(|(i, _)| i).unwrap_or(text.len());
    if let Some(nl) = text[..end].rfind('\n') {
        if nl > end / 2 {
            end = nl;
        }
    }
    (text[..end].to_string(), true)
}

/// UTF-8 text files (with or without BOM) up to MAX_INLINE_TEXT. UTF-16 files,
/// which Notepad still produces, are decoded too.
pub fn read_text(path: &str) -> Option<String> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_INLINE_TEXT {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16(&units).ok();
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let text = std::str::from_utf8(body).ok()?;
    // A NUL byte means binary data that happens to be valid UTF-8.
    if text.contains('\0') {
        return None;
    }
    Some(text.to_string())
}

// ── PDF ───────────────────────────────────────────────────────────────────────

/// Text layer of a PDF. pdf-extract can panic on exotic files, so it runs
/// behind catch_unwind: a broken PDF must never take the app down.
pub fn pdf_text(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let result = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&bytes));
    match result {
        Ok(Ok(text)) => Some(tidy(&text)),
        Ok(Err(err)) => {
            crate::log::line(format!("pdf text extraction failed: {err}"));
            None
        }
        Err(_) => {
            crate::log::line("pdf text extraction panicked".to_string());
            None
        }
    }
}

/// Collapses the runs of blank lines PDF extraction tends to produce.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Renders the first pages of a PDF to PNG with Windows.Data.Pdf — the engine
/// Edge uses, already on every Windows 10/11 machine, so no extra dependency.
#[cfg(windows)]
pub fn render_pdf_pages(path: &str, max_pages: u32) -> Result<Vec<Vec<u8>>, String> {
    use windows::core::HSTRING;
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

    // Called from a blocking worker thread; S_FALSE / RPC_E_CHANGED_MODE just
    // mean the apartment is already set up.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }

    let run = || -> windows::core::Result<Vec<Vec<u8>>> {
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))?.get()?;
        let doc = PdfDocument::LoadFromFileAsync(&file)?.get()?;
        let count = doc.PageCount()?.min(max_pages);
        let mut pages = Vec::with_capacity(count as usize);
        for i in 0..count {
            let page = doc.GetPage(i)?;
            let size = page.Size()?;
            // ~1400 px wide: legible for OCR-ish reading, small enough to be quick.
            let scale = 1400.0 / size.Width.max(1.0);
            let options = PdfPageRenderOptions::new()?;
            options.SetDestinationWidth((size.Width * scale).round().max(1.0) as u32)?;
            options.SetDestinationHeight((size.Height * scale).round().max(1.0) as u32)?;
            let stream = InMemoryRandomAccessStream::new()?;
            page.RenderWithOptionsToStreamAsync(&stream, &options)?.get()?;
            let len = stream.Size()? as u32;
            let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
            reader.LoadAsync(len)?.get()?;
            let mut buf = vec![0u8; len as usize];
            reader.ReadBytes(&mut buf)?;
            pages.push(buf);
        }
        Ok(pages)
    };
    run().map_err(|e| format!("Windows could not render this PDF: {}", e.message()))
}

#[cfg(not(windows))]
pub fn render_pdf_pages(_path: &str, _max_pages: u32) -> Result<Vec<Vec<u8>>, String> {
    Err("This PDF has no text layer, and page rendering is Windows-only.".into())
}

// ── Word / OpenDocument ───────────────────────────────────────────────────────

/// Plain text of a .docx (word/document.xml) or .odt (content.xml).
pub fn word_text(path: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| "not a valid document".to_string())?;
    let (entry, para_end) = if archive.by_name("word/document.xml").is_ok() {
        ("word/document.xml", "</w:p>")
    } else {
        ("content.xml", "</text:p>")
    };
    let mut xml = String::new();
    archive
        .by_name(entry)
        .map_err(|_| "no text found in this document".to_string())?
        .take(20_000_000)
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    Ok(xml_to_text(&xml, para_end))
}

/// Strips tags, turning paragraph ends, line breaks and tabs into whitespace.
fn xml_to_text(xml: &str, para_end: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 4);
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        out.push_str(&unescape(&rest[..start]));
        let Some(end) = rest[start..].find('>') else { break };
        let tag = &rest[start..start + end + 1];
        if tag == para_end || tag.starts_with("<w:br") || tag.starts_with("<text:line-break") {
            out.push('\n');
        } else if tag.starts_with("<w:tab") || tag.starts_with("<text:tab") {
            out.push('\t');
        } else if tag == "</text:h>" {
            // .odt headings are their own element, not a paragraph.
            out.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    tidy(&out)
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

// ── Images ────────────────────────────────────────────────────────────────────

/// Decodes any supported image, shrinks it to MAX_IMAGE_SIDE and re-encodes it
/// as JPEG, which every Ollama vision model accepts.
pub fn shrink_image(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width().max(img.height()) > MAX_IMAGE_SIDE {
        img.resize(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
    encoder.encode_image(&rgb).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_respects_budget_and_char_boundaries() {
        let (t, cut) = clip("héllo wörld", 5);
        assert!(cut);
        assert_eq!(t, "héllo");
        let (t, cut) = clip("short", 100);
        assert!(!cut);
        assert_eq!(t, "short");
    }

    #[test]
    fn docx_xml_becomes_paragraphs() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Bonjour</w:t></w:r><w:r><w:tab/><w:t>tout le monde &amp; toi</w:t></w:r></w:p><w:p><w:r><w:t>Ligne 2</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(xml_to_text(xml, "</w:p>"), "Bonjour\ttout le monde & toi\nLigne 2");
    }

    #[test]
    fn images_are_shrunk_to_jpeg() {
        let big = image::RgbImage::from_pixel(3000, 1000, image::Rgb([200, 30, 30]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(big)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let jpeg = shrink_image(&png).unwrap();
        let back = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(back.width(), MAX_IMAGE_SIDE);
        assert!(jpeg.starts_with(&[0xFF, 0xD8]));
    }

    #[test]
    fn text_files_utf8_bom_and_utf16() {
        let dir = std::env::temp_dir().join(format!("coucou-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        std::fs::write(&a, b"\xEF\xBB\xBFsalut").unwrap();
        assert_eq!(read_text(a.to_str().unwrap()).unwrap(), "salut");
        let b = dir.join("b.txt");
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "été".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&b, utf16).unwrap();
        assert_eq!(read_text(b.to_str().unwrap()).unwrap(), "été");
        let c = dir.join("c.bin");
        std::fs::write(&c, b"ab\0cd").unwrap();
        assert!(read_text(c.to_str().unwrap()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
