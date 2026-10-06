// Dropped files are copied into %LOCALAPPDATA%\Coucou\inbox so the original is
// never touched and the copy survives the drag source going away.
// The inbox is swept of anything older than a week, as on macOS.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::settings;

const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFile {
    pub name: String,
    /// The copy in the inbox — what the models read.
    pub path: String,
    pub size: u64,
    /// Where the user dragged it from — where a corrected copy is written.
    pub source: String,
}

/// A folder drop reads at most this many files…
const MAX_FILES: usize = 25;
/// …this deep…
const MAX_DEPTH: usize = 4;
/// …and skips anything bigger than this.
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Folders nobody wants read: dependencies, build output, VCS metadata.
const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", ".svn", ".hg", "target", "dist", "build", "out", "bin", "obj", "__pycache__",
    ".venv", "venv", ".next", ".cache", ".idea", ".vs", ".vscode",
];

/// Files that can't be read as text, a document or an image.
const SKIP_EXT: &[&str] = &[
    "exe", "dll", "msi", "sys", "bin", "iso", "img", "zip", "7z", "rar", "gz", "tar", "xz", "bz2", "mp3", "wav",
    "flac", "ogg", "m4a", "mp4", "mkv", "avi", "mov", "webm", "psd", "ai", "blend", "fbx", "obj", "glb", "ttf",
    "otf", "woff", "woff2", "ico", "db", "sqlite", "lock", "pdb", "class", "jar", "pyc", "o", "a", "lib", "so",
];

/// Several dropped items at once. Folders are walked (breadth-first, so the
/// top-level files come first); files that can't be read are skipped.
pub fn ingest_many(sources: &[String]) -> Result<Vec<DroppedFile>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut truncated = false;
    for s in sources {
        let p = PathBuf::from(s);
        if p.is_dir() {
            truncated |= walk(&p, &mut files);
        } else {
            files.push(p);
        }
        if files.len() >= MAX_FILES {
            truncated |= files.len() > MAX_FILES;
            files.truncate(MAX_FILES);
            break;
        }
    }
    if files.is_empty() {
        return Err("Nothing I can read in there (empty folder, or only binaries).".into());
    }
    let mut out = Vec::new();
    let mut last_err = None;
    for f in &files {
        match ingest(&f.to_string_lossy()) {
            Ok(d) => out.push(d),
            Err(e) => last_err = Some(e),
        }
    }
    if out.is_empty() {
        return Err(last_err.unwrap_or_else(|| "Could not read the dropped files.".into()));
    }
    if truncated {
        crate::log::line(format!("folder drop truncated to {MAX_FILES} files"));
    }
    Ok(out)
}

/// Fallback drop path: the page sends a file's bytes (no path available).
/// Stored in the inbox like any other drop; `source` is the inbox copy.
pub fn ingest_bytes(name: &str, bytes: &[u8]) -> Result<DroppedFile, String> {
    // Keep only a file name, without anything Windows refuses in one.
    let clean: String = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let clean = if clean.trim().is_empty() { "file".to_string() } else { clean };
    let dir = inbox_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let src = Path::new(&clean);
    let mut dest = dir.join(&clean);
    if dest.exists() {
        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = src.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
        for i in 2..1000 {
            let candidate = dir.join(format!("{stem} ({i}){ext}"));
            if !candidate.exists() {
                dest = candidate;
                break;
            }
        }
    }
    std::fs::write(&dest, bytes).map_err(|e| format!("cannot save {clean}: {e}"))?;
    sweep(&dir);
    let path = dest.to_string_lossy().to_string();
    Ok(DroppedFile { name: clean, path: path.clone(), size: bytes.len() as u64, source: path })
}

/// Returns true when files were left out.
fn walk(root: &Path, out: &mut Vec<PathBuf>) -> bool {
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.starts_with('~') {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth + 1 < MAX_DEPTH && !SKIP_DIRS.contains(&name.to_lowercase().as_str()) {
                    queue.push_back((path, depth + 1));
                }
                continue;
            }
            let ext = crate::attach::extension(&name);
            if meta.len() == 0 || meta.len() > MAX_FILE_SIZE || SKIP_EXT.contains(&ext.as_str()) {
                continue;
            }
            if out.len() >= MAX_FILES {
                return true;
            }
            out.push(path);
        }
    }
    false
}

pub fn inbox_dir() -> PathBuf {
    settings::local_dir().join("inbox")
}

pub fn ingest(source: &str) -> Result<DroppedFile, String> {
    let src = Path::new(source);
    let meta = std::fs::metadata(src).map_err(|e| format!("cannot read {source}: {e}"))?;
    if meta.is_dir() {
        return Err("Folders can't be dropped yet.".into());
    }

    let dir = inbox_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());

    let mut dest = dir.join(&name);
    if dest.exists() {
        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = src.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
        for i in 2..1000 {
            let candidate = dir.join(format!("{stem} ({i}){ext}"));
            if !candidate.exists() {
                dest = candidate;
                break;
            }
        }
    }

    std::fs::copy(src, &dest).map_err(|e| format!("cannot copy: {e}"))?;
    // CopyFileEx carries the source's timestamps across, so a file last edited
    // three years ago would arrive already older than the sweep window and be
    // deleted on the spot. The inbox ages from when *we* copied it.
    if let Ok(file) = std::fs::File::options().write(true).open(&dest) {
        let _ = file.set_modified(SystemTime::now());
    }
    sweep(&dir);

    Ok(DroppedFile {
        name,
        path: dest.to_string_lossy().to_string(),
        size: meta.len(),
        source: source.to_string(),
    })
}

/// Drops anything copied here more than a week ago. `ingest` stamps every copy
/// with the time it landed, so this really is the age of the copy and not the
/// age of whatever the user happened to drag in.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(copied) = meta.modified() else { continue };
        if now.duration_since(copied).map(|age| age > KEEP_FOR).unwrap_or(false) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_copies_and_never_overwrites() {
        let tmp = std::env::temp_dir().join(format!("coucou-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("note.txt");
        std::fs::write(&source, b"hello").unwrap();

        let first = ingest(source.to_str().unwrap()).unwrap();
        assert_eq!(first.name, "note.txt");
        assert_eq!(std::fs::read(&first.path).unwrap(), b"hello");

        // A second drop of the same name must not clobber the first copy.
        std::fs::write(&source, b"second").unwrap();
        let second = ingest(source.to_str().unwrap()).unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(std::fs::read(&first.path).unwrap(), b"hello");
        assert_eq!(std::fs::read(&second.path).unwrap(), b"second");

        // A single folder is refused by `ingest`; `ingest_many` walks it.
        assert!(ingest(tmp.to_str().unwrap()).is_err());
        std::fs::create_dir_all(tmp.join("node_modules")).unwrap();
        std::fs::write(tmp.join("node_modules").join("dep.js"), b"x").unwrap();
        std::fs::write(tmp.join("tool.exe"), b"MZ").unwrap();
        let many = ingest_many(&[tmp.to_string_lossy().to_string()]).unwrap();
        let names: Vec<&str> = many.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"note.txt"), "{names:?}");
        assert!(!names.contains(&"dep.js") && !names.contains(&"tool.exe"), "{names:?}");
        for f in &many {
            let _ = std::fs::remove_file(&f.path);
        }

        // An ancient source must not arrive already older than the sweep window.
        let old_source = tmp.join("ancient.txt");
        std::fs::write(&old_source, b"old").unwrap();
        let long_ago = SystemTime::now() - KEEP_FOR - Duration::from_secs(60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&old_source)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let aged = ingest(old_source.to_str().unwrap()).unwrap();
        assert!(
            Path::new(&aged.path).exists(),
            "a file copied just now was swept as if it were a week old"
        );
        let _ = std::fs::remove_file(&aged.path);

        let _ = std::fs::remove_file(&first.path);
        let _ = std::fs::remove_file(&second.path);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
