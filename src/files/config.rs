//! Files-feature configuration: PIN, folder-access scope, and configured roots.
//! Persisted as `%LOCALAPPDATA%\iphone-bridge\config.json`; loaded into
//! `AppState` behind an `Arc<RwLock<_>>` and re-saved on every tray change.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::logging::log_both;

/// How much of the filesystem the Files tab is allowed to browse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Only the explicitly-configured `roots`.
    Roots,
    /// The whole user profile (`C:\Users\<user>`) as a single root.
    Profile,
    /// Every ready fixed/removable drive as its own root.
    Drives,
}

impl Default for Scope {
    fn default() -> Self {
        Scope::Roots
    }
}

/// A named root exposed to the client. `name` is the display label (the folder
/// basename, drive letter, or "profile"); `path` is the absolute path.
#[derive(Clone, Debug, Serialize)]
pub struct NamedRoot {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub pin: String,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub roots: Vec<String>,
    /// Canonical paths a Claude session has been spawned in at least once.
    /// Drives the `mode: "auto"` first-time-visible decision (see `/api/spawn`).
    /// `serde(default)` so pre-existing config.json files (which lack this key)
    /// load cleanly.
    #[serde(default)]
    pub spawned_dirs: Vec<String>,
}

impl Config {
    /// Record that `canon` (an already-canonicalized path) has now been spawned
    /// in. Returns true if it was newly added (caller should persist).
    pub fn remember_spawned_dir(&mut self, canon: &str) -> bool {
        if self
            .spawned_dirs
            .iter()
            .any(|d| d.eq_ignore_ascii_case(canon))
        {
            false
        } else {
            self.spawned_dirs.push(canon.to_string());
            true
        }
    }

    /// Has a session been spawned in `canon` (already canonicalized) before?
    pub fn has_spawned_dir(&self, canon: &str) -> bool {
        self.spawned_dirs
            .iter()
            .any(|d| d.eq_ignore_ascii_case(canon))
    }
}

impl Config {
    /// Default first-run config: a fresh random 6-digit PIN, scope "roots", and
    /// the two default desktop roots.
    fn first_run() -> Self {
        Config {
            pin: random_pin(),
            scope: Scope::Roots,
            roots: vec![
                r"C:\Users\thedi\Desktop\Work".to_string(),
                r"C:\Users\thedi\Desktop".to_string(),
                r"C:\Users\thedi\Downloads".to_string(),
                r"C:\Users\thedi\Documents".to_string(),
            ],
            spawned_dirs: Vec::new(),
        }
    }

    /// Configured roots as `NamedRoot`s (name = folder basename).
    pub fn configured_named_roots(&self) -> Vec<NamedRoot> {
        self.roots
            .iter()
            .map(|p| NamedRoot {
                name: basename_of(p),
                path: p.clone(),
            })
            .collect()
    }

    /// What the *current scope* actually exposes.
    pub fn effective_named_roots(&self) -> Vec<NamedRoot> {
        match self.scope {
            Scope::Roots => self.configured_named_roots(),
            Scope::Profile => {
                let p = user_profile_dir();
                vec![NamedRoot {
                    name: "profile".to_string(),
                    path: p,
                }]
            }
            Scope::Drives => ready_drives()
                .into_iter()
                .map(|d| NamedRoot {
                    name: d.trim_end_matches('\\').to_string(),
                    path: d,
                })
                .collect(),
        }
    }

    /// Effective roots as raw `PathBuf`s, for the path-safety check.
    pub fn effective_root_paths(&self) -> Vec<PathBuf> {
        self.effective_named_roots()
            .into_iter()
            .map(|r| PathBuf::from(r.path))
            .collect()
    }
}

/// Path to the config file.
pub fn config_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
    PathBuf::from(base)
        .join("iphone-bridge")
        .join("config.json")
}

/// Load the config, creating a first-run file (with a fresh PIN) if none
/// exists or the existing one is unreadable/corrupt.
pub fn load_or_init() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Config>(&s) {
            Ok(cfg) => {
                log_both(&format!(
                    "[files] loaded config: scope={:?}, {} root(s)",
                    cfg.scope,
                    cfg.roots.len()
                ));
                cfg
            }
            Err(e) => {
                log_both(&format!(
                    "[files] config.json unreadable ({e}); regenerating with a new PIN"
                ));
                let cfg = Config::first_run();
                save(&cfg);
                cfg
            }
        },
        Err(_) => {
            let cfg = Config::first_run();
            log_both(&format!(
                "[files] no config.json; generated PIN {} and default roots",
                cfg.pin
            ));
            save(&cfg);
            cfg
        }
    }
}

/// Persist the config to disk (best-effort; logs on failure).
pub fn save(cfg: &Config) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(cfg) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&path, s) {
                log_both(&format!("[files] failed to write config.json: {e}"));
            }
        }
        Err(e) => log_both(&format!("[files] failed to serialize config: {e}")),
    }
}

/// Generate a random 6-digit PIN (000000-999999) without pulling a crypto
/// crate: seed a small LCG from the system clock. Non-cryptographic, which is
/// fine for a LAN/Tailscale device PIN.
fn random_pin() -> String {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64
        ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15);
    // xorshift64* -> a value in [0, 999999].
    let mut x = seed | 1;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let n = (x.wrapping_mul(0x2545F4914F6CDD1D) >> 40) % 1_000_000;
    format!("{n:06}")
}

/// The current user's profile directory (`C:\Users\<user>`).
fn user_profile_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users".to_string())
}

/// Basename of an absolute path, for display (falls back to the whole string,
/// e.g. for a bare drive root like `C:\`).
fn basename_of(p: &str) -> String {
    let trimmed = p.trim_end_matches(['\\', '/']);
    match trimmed.rsplit(['\\', '/']).next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => p.to_string(),
    }
}

/// Enumerate ready drive roots (`C:\`, `D:\`, ...) by probing each letter and
/// keeping the ones whose root directory can be read.
fn ready_drives() -> Vec<String> {
    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        if std::fs::metadata(&root).is_ok() {
            out.push(root);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_without_spawned_dirs_loads() {
        // A config.json written before batch-2 has no `spawned_dirs` key.
        let legacy = r#"{ "pin": "123456", "scope": "roots", "roots": ["C:\\x"] }"#;
        let cfg: Config = serde_json::from_str(legacy).expect("legacy config must load");
        assert_eq!(cfg.pin, "123456");
        assert_eq!(cfg.scope, Scope::Roots);
        assert_eq!(cfg.roots, vec!["C:\\x".to_string()]);
        assert!(cfg.spawned_dirs.is_empty());
    }

    #[test]
    fn remember_spawned_dir_dedups_case_insensitively() {
        let mut cfg = Config::first_run();
        assert!(cfg.remember_spawned_dir(r"C:\Users\thedi\proj"));
        // Same path, different case -> not added again.
        assert!(!cfg.remember_spawned_dir(r"c:\users\thedi\PROJ"));
        assert_eq!(cfg.spawned_dirs.len(), 1);
        assert!(cfg.has_spawned_dir(r"C:\Users\thedi\proj"));
        assert!(cfg.has_spawned_dir(r"C:\USERS\THEDI\PROJ"));
        assert!(!cfg.has_spawned_dir(r"C:\Users\thedi\other"));
    }

    #[test]
    fn config_roundtrips_with_spawned_dirs() {
        let mut cfg = Config::first_run();
        cfg.remember_spawned_dir(r"C:\a");
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.spawned_dirs, vec![r"C:\a".to_string()]);
    }
}
