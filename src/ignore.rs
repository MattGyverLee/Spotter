//! Folders that are skipped while searching: dependency stores, caches and build output.
//!
//! One rule per line:
//!   `node_modules`                skip every folder with this name (`*` and `?` allowed)
//!   `target if ../Cargo.toml`     skip only when the parent folder has a matching file
//!   `venv if pyvenv.cfg`          skip only when the folder itself contains a matching file
//!   `Windows/WinSxS`              a rule with `/` matches the end of the folder's path
//!   `C:/Windows/WinSxS`           ... or the whole path, when it has a drive
//! `#` starts a comment. Names are compared case-insensitively.

use crate::search::glob_match;
use std::path::Path;

pub const DEFAULT_RULES: &str = "\
# JavaScript
node_modules
bower_components
jspm_packages
.pnpm-store
.yarn
.next
.nuxt
.svelte-kit
.parcel-cache
.turbo
.angular
# Python
site-packages
dist-packages
__pycache__
.venv
venv if pyvenv.cfg
env if pyvenv.cfg
.tox
.nox
.mypy_cache
.pytest_cache
.ruff_cache
.ipynb_checkpoints
*.egg-info
# Version control
.git
.svn
.hg
# Build output
target if ../Cargo.toml
bin if ../*.csproj
obj if ../*.csproj
.vs
.gradle
.terraform
# Windows system-managed
$Recycle.Bin
System Volume Information
$Extend
$WinREAgent
$SysReset
$Windows.~BT
$Windows.~WS
Config.Msi
Windows/WinSxS
Windows/Installer
Windows/servicing
Windows/SoftwareDistribution
Windows/assembly
Windows/Prefetch
Windows/CSC
Windows/System32/DriverStore
Windows/SystemTemp
Program Files/WindowsApps
ProgramData/Package Cache
";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Marker {
    None,
    /// A file next to the folder (`../name`).
    Sibling(String),
    /// A file inside the folder.
    Inside(String),
}

#[derive(Clone, Debug)]
pub struct Rule {
    /// The rule as written, used as its label in the summary.
    pub label: String,
    /// Lowercase; for a path rule, `/`-separated and anchored with a leading `*/` unless it has a drive.
    name: String,
    path_rule: bool,
    marker: Marker,
}

#[derive(Clone, Debug, Default)]
pub struct IgnoreList {
    pub rules: Vec<Rule>,
}

impl IgnoreList {
    pub fn parse(text: &str) -> IgnoreList {
        let rules = text
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (name, marker) = match l.split_once(" if ") {
                    Some((n, m)) => {
                        let m = m.trim().replace('\\', "/").to_lowercase();
                        let marker = match m.strip_prefix("../") {
                            Some(s) => Marker::Sibling(s.to_string()),
                            None => Marker::Inside(m),
                        };
                        (n.trim(), marker)
                    }
                    None => (l, Marker::None),
                };
                let lower = name.replace('\\', "/").trim_end_matches('/').to_lowercase();
                let path_rule = lower.contains('/');
                let name = if path_rule && !lower.contains(':') { format!("*/{lower}") } else { lower };
                Rule { label: l.to_string(), name, path_rule, marker }
            })
            .collect();
        IgnoreList { rules }
    }

    pub fn defaults() -> IgnoreList {
        IgnoreList::parse(DEFAULT_RULES)
    }

    /// The rule that makes `dir` (named `lower_name`, lowercased) skipped, if any.
    /// `siblings` are the lowercased names of every entry in the parent folder.
    pub fn matches(&self, dir: &Path, lower_name: &str, siblings: &[String]) -> Option<&Rule> {
        let mut lower_path = None;
        self.rules.iter().find(|r| {
            let target = if r.path_rule { lower_path.get_or_insert_with(|| dir.to_string_lossy().replace('\\', "/").to_lowercase()).as_str() } else { lower_name };
            glob_match(&r.name, target)
                && match &r.marker {
                    Marker::None => true,
                    Marker::Sibling(m) => siblings.iter().any(|s| glob_match(m, s)),
                    Marker::Inside(m) => inside_has(dir, m),
                }
        })
    }
}

fn inside_has(dir: &Path, pattern: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return dir.join(pattern).exists();
    }
    std::fs::read_dir(dir).is_ok_and(|rd| rd.flatten().any(|e| glob_match(pattern, &e.file_name().to_string_lossy().to_lowercase())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_marker_rules() {
        let l = IgnoreList::defaults();
        let p = Path::new("C:/nowhere");
        assert!(l.matches(p, "node_modules", &[]).is_some());
        assert!(l.matches(p, "foo.egg-info", &[]).is_some());
        assert!(l.matches(p, "target", &[]).is_none());
        assert!(l.matches(p, "target", &["cargo.toml".into()]).is_some());
        assert!(l.matches(p, "bin", &["app.csproj".into()]).is_some());
        assert!(l.matches(p, "bin", &["readme.md".into()]).is_none());
        assert!(l.matches(p, "src", &[]).is_none());
    }

    #[test]
    fn path_rules() {
        let l = IgnoreList::defaults();
        assert!(l.matches(Path::new(r"C:\Windows\WinSxS"), "winsxs", &[]).is_some());
        assert!(l.matches(Path::new(r"D:\Images\Windows\Installer"), "installer", &[]).is_some());
        assert!(l.matches(Path::new(r"C:\Tools\Installer"), "installer", &[]).is_none());
        assert!(l.matches(Path::new(r"C:\Windows"), "windows", &[]).is_none());
        let l = IgnoreList::parse(r"C:\Data\Big");
        assert!(l.matches(Path::new(r"C:\Data\Big"), "big", &[]).is_some());
        assert!(l.matches(Path::new(r"D:\Data\Big"), "big", &[]).is_none());
    }
}
