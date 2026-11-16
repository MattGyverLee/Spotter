//! Command-line search, using the same engine and settings as the Spotter window.

use spotter::drives;
use spotter::fmt::{self, count, human};
use spotter::ignore::IgnoreList;
use spotter::search::{self, NameMatcher, Progress, Query};
use spotter::settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

const USAGE: &str = "\
spotter-cli <folder[;folder...] | local> [name patterns] [options]

  local               every local drive (not network or cloud drives)

  name patterns       e.g. report  or  *.rs;*.toml  (substring unless it has * or ?)
  --text TEXT         only files containing TEXT
  --case              name and text are case-sensitive
  --no-skip           also search ignored folders (node_modules, WinSxS ...)
  --top               do not search subfolders
  --files             files only (no folders)
  --after DATE        modified on or after (YYYY-MM-DD or 7d)
  --before DATE       modified before
  --min-kb N          larger than N KB
  --max-kb N          smaller than N KB
  --quiet             summary only
  --errors            list every error and access-denied path
";

fn main() {
    if let Err(e) = run() {
        eprintln!("[ERROR] {e}");
        eprintln!();
        eprint!("{USAGE}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let settings = Settings::load();
    let mut args = std::env::args().skip(1);
    let mut positional = Vec::new();
    let mut text = String::new();
    let (mut case, mut skip, mut subfolders, mut folders, mut quiet, mut all_errors) = (false, true, true, true, false, false);
    let (mut after, mut before, mut min_size, mut max_size) = (None, None, None, None);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--text" => text = value()?,
            "--case" => case = true,
            "--no-skip" => skip = false,
            "--top" => subfolders = false,
            "--files" => folders = false,
            "--quiet" => quiet = true,
            "--errors" => all_errors = true,
            "--after" => after = fmt::parse_date(&value()?)?,
            "--before" => before = fmt::parse_date(&value()?)?,
            "--min-kb" => min_size = fmt::parse_size(&value()?, 1024)?,
            "--max-kb" => max_size = fmt::parse_size(&value()?, 1024)?,
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => positional.push(a),
        }
    }
    let Some(look_in) = positional.first() else { return Err("give a folder to search".into()) };
    let look_in = if look_in.eq_ignore_ascii_case("local") { drives::local_roots(&drives::list()) } else { look_in.clone() };
    let q = Query {
        roots: look_in.split(';').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect(),
        name: NameMatcher::new(positional.get(1).map_or("", |s| s.as_str()), case),
        text,
        text_case: case,
        subfolders,
        skip_ignored: skip,
        ignore: IgnoreList::parse(&settings.ignore_rules),
        max_text_bytes: settings.max_text_mb * 1024 * 1024,
        min_size,
        max_size,
        after,
        before,
        folders,
    };

    let prog = Progress::default();
    let (tx, rx) = std::sync::mpsc::channel::<search::Hit>();
    let start = Instant::now();
    let printer = std::thread::spawn(move || {
        for h in rx {
            if !quiet {
                let size = if h.is_dir { "<folder>".to_string() } else { human(h.size) };
                let hits = if h.hits > 0 { format!("  [{} hits]", h.hits) } else { String::new() };
                println!("{}  {size}  {}{hits}", fmt::date(h.modified), h.path.display());
            }
        }
    });
    search::search(&q, &prog, &tx);
    drop(tx);
    let _ = printer.join();

    let n = |a: &std::sync::atomic::AtomicU64| count(a.load(Ordering::Relaxed));
    println!();
    println!("[DONE] Found {} in {:.2} s; checked {} items in {} folders", n(&prog.found), start.elapsed().as_secs_f64(), n(&prog.checked), n(&prog.dirs));
    let ignored = prog.ignored_by_rule.lock().map(|m| m.clone()).unwrap_or_default();
    if !ignored.is_empty() {
        let mut v: Vec<_> = ignored.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let list = v.iter().map(|(r, c)| format!("{r} {}", count(*c))).collect::<Vec<_>>().join(", ");
        println!("[INFO] Skipped {} folders: {list}", n(&prog.ignored));
    }
    if prog.online_only.load(Ordering::Relaxed) > 0 {
        println!("[INFO] {} online-only cloud folders not opened", n(&prog.online_only));
    }
    if prog.online_files.load(Ordering::Relaxed) > 0 {
        println!("[INFO] {} online-only cloud files not read", n(&prog.online_files));
    }
    if prog.not_read.load(Ordering::Relaxed) > 0 {
        println!("[INFO] {} files not read (binary or too large)", n(&prog.not_read));
    }
    if prog.denied.load(Ordering::Relaxed) > 0 {
        println!("[INFO] {} folders or files not searched: access denied (need administrator)", n(&prog.denied));
        if all_errors {
            for p in prog.denied_list.lock().map(|v| v.clone()).unwrap_or_default() {
                println!("  {p}");
            }
        }
    }
    if prog.errors.load(Ordering::Relaxed) > 0 {
        println!("[WARN] {} errors, first:", n(&prog.errors));
        for (p, e) in prog.error_list.lock().map(|v| v.clone()).unwrap_or_default().iter().take(if all_errors { usize::MAX } else { 5 }) {
            println!("  {p}: {e}");
        }
    }
    Ok(())
}
