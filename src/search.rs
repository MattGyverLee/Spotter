//! Parallel file search.
//!
//! - Folders matched by the ignore list are not descended into (the folder itself can still be found by name).
//! - Never follows symlinks or junctions (name-surrogate reparse points), so pnpm stores and the like cannot loop.
//! - Cloud placeholders (OneDrive / Dropbox / Google Drive "online-only" files and folders) are matched by
//!   name but never opened or listed, so a search never triggers a download.

use crate::ignore::IgnoreList;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

const ATTR_REPARSE_POINT: u32 = 0x400;
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x40000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
/// Only this many errors are kept for the summary; the count keeps going.
const MAX_ERRORS: usize = 500;
/// A NUL byte in this much of the start of a file marks it as binary.
const BINARY_SNIFF: usize = 8000;

fn is_placeholder(attrs: u32) -> bool {
    attrs & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
}

/// `DirEntry::file_type()` reports name-surrogate reparse points (symlinks AND junctions) as symlinks;
/// cloud-file reparse points are not, so synced folders are still searched.
fn is_link(attrs: u32, ft: &fs::FileType) -> bool {
    attrs & ATTR_REPARSE_POINT != 0 && ft.is_symlink()
}

/// `*` and `?` wildcard match of the whole of `text`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// File name patterns separated by `;`. A pattern without `*` or `?` matches anywhere in the name.
#[derive(Clone, Debug, Default)]
pub struct NameMatcher {
    patterns: Vec<String>,
    case: bool,
}

impl NameMatcher {
    pub fn new(text: &str, case: bool) -> NameMatcher {
        let patterns = text
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| if case { s.to_string() } else { s.to_lowercase() })
            .collect();
        NameMatcher { patterns, case }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        if self.patterns.is_empty() {
            return true;
        }
        let lower;
        let name = if self.case {
            name
        } else {
            lower = name.to_lowercase();
            &lower
        };
        self.patterns.iter().any(|p| if p.contains(['*', '?']) { glob_match(p, name) } else { name.contains(p.as_str()) })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub roots: Vec<PathBuf>,
    pub name: NameMatcher,
    /// Text to look for inside files; empty for a name-only search.
    pub text: String,
    pub text_case: bool,
    pub subfolders: bool,
    /// Skip folders matched by `ignore`.
    pub skip_ignored: bool,
    pub ignore: IgnoreList,
    /// Files larger than this are not read for a text search.
    pub max_text_bytes: u64,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Unix seconds.
    pub after: Option<i64>,
    pub before: Option<i64>,
    /// Also list folders whose name matches (only for searches without text or size filters).
    pub folders: bool,
}

impl Query {
    fn folders_can_match(&self) -> bool {
        self.folders && self.text.is_empty() && self.min_size.is_none() && self.max_size.is_none()
    }

    fn date_ok(&self, modified: i64) -> bool {
        self.after.is_none_or(|a| modified >= a) && self.before.is_none_or(|b| modified < b)
    }

    fn size_ok(&self, size: u64) -> bool {
        self.min_size.is_none_or(|m| size > m) && self.max_size.is_none_or(|m| size < m)
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds.
    pub modified: i64,
    /// Text matches in the file (0 for a name-only search).
    pub hits: u32,
}

impl Hit {
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn location(&self) -> String {
        self.path.parent().map(|p| p.display().to_string()).unwrap_or_default()
    }
}

#[derive(Default)]
pub struct Progress {
    /// Files and folders looked at.
    pub checked: AtomicU64,
    pub dirs: AtomicU64,
    pub found: AtomicU64,
    /// Folders skipped by the ignore list.
    pub ignored: AtomicU64,
    pub links: AtomicU64,
    /// Cloud folders that are not on this machine, so not opened.
    pub online_only: AtomicU64,
    /// Cloud placeholder files not read for a text search, since reading would download them.
    pub online_files: AtomicU64,
    /// Files not read for a text search: binary or too large.
    pub not_read: AtomicU64,
    /// Folders and files this account may not read (mostly system and vendor folders that need administrator).
    pub denied: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    /// Skipped folders per ignore rule.
    pub ignored_by_rule: Mutex<HashMap<String, u64>>,
    /// Paths behind `denied`, at most MAX_ERRORS of them.
    pub denied_list: Mutex<Vec<String>>,
    /// (path, message), at most MAX_ERRORS of them.
    pub error_list: Mutex<Vec<(String, String)>>,
}

impl Progress {
    fn io_error(&self, path: &Path, e: &std::io::Error) {
        if e.kind() != std::io::ErrorKind::PermissionDenied {
            return self.error(path, e);
        }
        self.denied.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut v) = self.denied_list.lock()
            && v.len() < MAX_ERRORS
        {
            v.push(path.display().to_string());
        }
    }

    fn error(&self, path: &Path, e: impl std::fmt::Display) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut v) = self.error_list.lock()
            && v.len() < MAX_ERRORS
        {
            v.push((path.display().to_string(), e.to_string()));
        }
    }

    fn ignored(&self, label: &str) {
        self.ignored.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut m) = self.ignored_by_rule.lock() {
            *m.entry(label.to_string()).or_default() += 1;
        }
    }
}

fn unix_secs(md: &fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Run `q`, sending every hit to `tx`. Returns when the search is done or cancelled.
pub fn search(q: &Query, prog: &Progress, tx: &Sender<Hit>) {
    q.roots.par_iter().for_each(|root| match fs::metadata(root) {
        Ok(md) if md.is_dir() => walk(root, q, prog, tx),
        Ok(_) => prog.error(root, "not a folder"),
        Err(e) => prog.io_error(root, &e),
    });
}

struct Entry {
    path: PathBuf,
    name: String,
    md: fs::Metadata,
    link: bool,
}

fn walk(dir: &Path, q: &Query, prog: &Progress, tx: &Sender<Hit>) {
    if prog.cancel.load(Ordering::Relaxed) {
        return;
    }
    prog.dirs.fetch_add(1, Ordering::Relaxed);
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => return prog.io_error(dir, &e),
    };
    let mut entries = Vec::new();
    for e in rd {
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                prog.io_error(dir, &err);
                continue;
            }
        };
        match (e.metadata(), e.file_type()) {
            (Ok(md), Ok(ft)) => {
                let link = is_link(md.file_attributes(), &ft);
                entries.push(Entry { path: e.path(), name: e.file_name().to_string_lossy().into_owned(), md, link });
            }
            (Err(err), _) | (_, Err(err)) => prog.io_error(&e.path(), &err),
        }
    }
    prog.checked.fetch_add(entries.len() as u64, Ordering::Relaxed);

    let siblings: Vec<String> = if q.skip_ignored { entries.iter().map(|e| e.name.to_lowercase()).collect() } else { Vec::new() };
    let send = |e: &Entry, hits: u32| {
        prog.found.fetch_add(1, Ordering::Relaxed);
        let _ = tx.send(Hit { path: e.path.clone(), is_dir: e.md.is_dir(), size: if e.md.is_dir() { 0 } else { e.md.len() }, modified: unix_secs(&e.md), hits });
    };

    let mut subdirs = Vec::new();
    let mut texts = Vec::new();
    for e in &entries {
        if e.md.is_dir() {
            if q.folders_can_match() && q.name.matches(&e.name) && q.date_ok(unix_secs(&e.md)) {
                send(e, 0);
            }
            if e.link {
                prog.links.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            // Listing an online-only folder makes the sync client download its contents.
            if is_placeholder(e.md.file_attributes()) {
                prog.online_only.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if q.skip_ignored
                && let Some(rule) = q.ignore.matches(&e.path, &e.name.to_lowercase(), &siblings)
            {
                prog.ignored(&rule.label);
                continue;
            }
            if q.subfolders {
                subdirs.push(&e.path);
            }
            continue;
        }
        if e.link || !q.name.matches(&e.name) || !q.size_ok(e.md.len()) || !q.date_ok(unix_secs(&e.md)) {
            continue;
        }
        if q.text.is_empty() {
            send(e, 0);
        } else if is_placeholder(e.md.file_attributes()) {
            prog.online_files.fetch_add(1, Ordering::Relaxed);
        } else if e.md.len() > q.max_text_bytes {
            prog.not_read.fetch_add(1, Ordering::Relaxed);
        } else {
            texts.push(e);
        }
    }

    texts.par_iter().for_each(|e| {
        if prog.cancel.load(Ordering::Relaxed) {
            return;
        }
        match read_text(&e.path) {
            Ok(Some(t)) => {
                let n = count_matches(&t, &q.text, q.text_case);
                if n > 0 {
                    send(e, n.min(u32::MAX as usize) as u32);
                }
            }
            Ok(None) => drop(prog.not_read.fetch_add(1, Ordering::Relaxed)),
            Err(err) => prog.io_error(&e.path, &err),
        }
    });

    subdirs.par_iter().for_each(|d| walk(d, q, prog, tx));
}

/// File contents as text: UTF-16 with a byte order mark, else UTF-8 (lossy). `None` for binary files.
pub fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    let b = fs::read(path)?;
    let utf16 = |be: bool| {
        let units: Vec<u16> = b[2..].chunks_exact(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    Ok(match b.as_slice() {
        [0xFF, 0xFE, ..] => Some(utf16(false)),
        [0xFE, 0xFF, ..] => Some(utf16(true)),
        _ if b[..b.len().min(BINARY_SNIFF)].contains(&0) => None,
        [0xEF, 0xBB, 0xBF, rest @ ..] => Some(String::from_utf8_lossy(rest).into_owned()),
        _ => Some(String::from_utf8_lossy(&b).into_owned()),
    })
}

pub fn count_matches(haystack: &str, needle: &str, case: bool) -> usize {
    if case { haystack.matches(needle).count() } else { haystack.to_lowercase().matches(&needle.to_lowercase()).count() }
}

/// Lines of `path` containing `needle`: (1-based line number, line), at most `limit` of them.
pub fn matching_lines(path: &Path, needle: &str, case: bool, limit: usize) -> Vec<(usize, String)> {
    let Ok(Some(text)) = read_text(path) else { return Vec::new() };
    let lower = needle.to_lowercase();
    text.lines()
        .enumerate()
        .filter(|(_, l)| if case { l.contains(needle) } else { l.to_lowercase().contains(&lower) })
        .take(limit)
        .map(|(i, l)| (i + 1, l.trim_end().chars().take(400).collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.rsx"));
        assert!(glob_match("a?c*", "abcdef"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("?", ""));
        assert!(glob_match("*fluff*", "diskfluff-0a1"));
    }

    #[test]
    fn names() {
        let m = NameMatcher::new("fluff", false);
        assert!(m.matches("DiskFluff"));
        let m = NameMatcher::new("*.toml; *.rs", false);
        assert!(m.matches("Cargo.TOML") && m.matches("lib.rs") && !m.matches("rs.txt"));
        let m = NameMatcher::new("Fluff", true);
        assert!(!m.matches("diskfluff"));
        assert!(NameMatcher::new(" ; ", false).is_empty());
    }

    #[test]
    fn counting() {
        assert_eq!(count_matches("Foo foo FOO", "foo", false), 3);
        assert_eq!(count_matches("Foo foo FOO", "foo", true), 1);
    }

    #[test]
    fn skips_ignored_folders() {
        let root = std::env::temp_dir().join(format!("spotter-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["src", "node_modules/pkg", "target/debug"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["src/needle.txt", "node_modules/pkg/needle.txt", "target/debug/needle.txt", "Cargo.toml"] {
            fs::write(root.join(f), "a needle here").unwrap();
        }
        let run = |skip: bool, text: &str| {
            let q = Query {
                roots: vec![root.clone()],
                name: NameMatcher::new("needle", false),
                text: text.into(),
                subfolders: true,
                skip_ignored: skip,
                ignore: IgnoreList::defaults(),
                max_text_bytes: 1 << 20,
                folders: true,
                ..Default::default()
            };
            let (tx, rx) = std::sync::mpsc::channel();
            search(&q, &Progress::default(), &tx);
            drop(tx);
            let mut v: Vec<_> = rx.iter().map(|h| (h.path.strip_prefix(&root).unwrap().display().to_string(), h.hits)).collect();
            v.sort();
            v
        };
        assert_eq!(run(true, ""), vec![(r"src\needle.txt".to_string(), 0)]);
        assert_eq!(run(false, "").len(), 3);
        assert_eq!(run(true, "NEEDLE"), vec![(r"src\needle.txt".to_string(), 1)]);
        fs::remove_dir_all(&root).unwrap();
    }
}
