//! Remembered search fields and options, in `%APPDATA%\Spotter\settings.toml`.

use crate::ignore::DEFAULT_RULES;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub name: String,
    pub name_case: bool,
    pub text: String,
    pub text_case: bool,
    /// Folders to search, separated by `;`.
    pub look_in: String,
    pub subfolders: bool,
    pub skip_ignored: bool,
    pub folders: bool,
    pub ignore_rules: String,
    pub max_text_mb: u64,
    /// Recent values of each field, newest first.
    pub name_history: Vec<String>,
    pub text_history: Vec<String>,
    pub look_in_history: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            name: String::new(),
            name_case: false,
            text: String::new(),
            text_case: false,
            look_in: String::new(),
            subfolders: true,
            skip_ignored: true,
            folders: true,
            ignore_rules: DEFAULT_RULES.to_string(),
            max_text_mb: 50,
            name_history: Vec::new(),
            text_history: Vec::new(),
            look_in_history: Vec::new(),
        }
    }
}

const HISTORY: usize = 20;

pub fn remember(list: &mut Vec<String>, value: &str) {
    let v = value.trim();
    if v.is_empty() {
        return;
    }
    list.retain(|x| x != v);
    list.insert(0, v.to_string());
    list.truncate(HISTORY);
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Spotter").join("settings.toml"))
}

impl Settings {
    pub fn load() -> Settings {
        path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string(self) {
            let _ = std::fs::write(p, s);
        }
    }
}
