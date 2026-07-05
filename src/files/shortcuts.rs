//! Fixed quick-access shortcuts surfaced in `/api/roots`. The candidate list is
//! static (relative to `%USERPROFILE%`); only entries that actually exist on
//! disk are returned. Filtering is pure (takes an `exists` predicate) so it's
//! fully unit-testable without touching the real filesystem.

use serde::Serialize;

/// One shortcut entry as sent to the client.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Shortcut {
    pub name: String,
    pub path: String,
}

/// Fixed candidate list, relative to the profile root. Order is preserved in
/// the output (existing entries only, in this order).
fn candidates(profile: &str) -> Vec<Shortcut> {
    let join = |suffix: &str| -> String {
        if suffix.is_empty() {
            profile.to_string()
        } else {
            format!("{}\\{}", profile.trim_end_matches('\\'), suffix)
        }
    };
    vec![
        Shortcut { name: "Desktop".to_string(), path: join("Desktop") },
        Shortcut { name: "Downloads".to_string(), path: join("Downloads") },
        Shortcut { name: "Documents".to_string(), path: join("Documents") },
        Shortcut { name: "Work".to_string(), path: join(r"Desktop\Work") },
        Shortcut { name: "Projects".to_string(), path: join(r"Desktop\Work\Projects") },
        Shortcut { name: "Tools".to_string(), path: join(r"Desktop\Work\Tools") },
        Shortcut { name: "MetaGrid".to_string(), path: join(r"Desktop\Work\Tools\MetaGrid") },
        Shortcut { name: "Mic".to_string(), path: join(r"Desktop\Mic") },
    ]
}

/// Build the shortcut list for `profile`, keeping only entries for which
/// `exists` returns true. Pure: `exists` is injected so this is testable
/// without disk I/O.
pub fn resolve_shortcuts(profile: &str, exists: impl Fn(&str) -> bool) -> Vec<Shortcut> {
    candidates(profile)
        .into_iter()
        .filter(|s| exists(&s.path))
        .collect()
}

/// Real filesystem existence check (directory must exist), for production use.
pub fn path_exists(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_present_returns_all_in_order() {
        let out = resolve_shortcuts(r"C:\Users\thedi", |_| true);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Desktop", "Downloads", "Documents", "Work", "Projects", "Tools", "MetaGrid", "Mic"]
        );
        assert_eq!(out[0].path, r"C:\Users\thedi\Desktop");
        assert_eq!(out[6].path, r"C:\Users\thedi\Desktop\Work\Tools\MetaGrid");
    }

    #[test]
    fn none_present_returns_empty() {
        let out = resolve_shortcuts(r"C:\Users\thedi", |_| false);
        assert!(out.is_empty());
    }

    #[test]
    fn only_existing_entries_are_kept() {
        let existing: HashSet<&str> = [r"C:\Users\thedi\Desktop", r"C:\Users\thedi\Downloads"]
            .into_iter()
            .collect();
        let out = resolve_shortcuts(r"C:\Users\thedi", |p| existing.contains(p));
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Desktop", "Downloads"]);
    }

    #[test]
    fn profile_trailing_backslash_is_normalized() {
        let out = resolve_shortcuts(r"C:\Users\thedi\", |_| true);
        assert_eq!(out[0].path, r"C:\Users\thedi\Desktop");
    }

    #[test]
    fn nested_paths_use_correct_separators() {
        let out = resolve_shortcuts(r"C:\Users\thedi", |_| true);
        let mic = out.iter().find(|s| s.name == "Mic").unwrap();
        assert_eq!(mic.path, r"C:\Users\thedi\Desktop\Mic");
        let projects = out.iter().find(|s| s.name == "Projects").unwrap();
        assert_eq!(projects.path, r"C:\Users\thedi\Desktop\Work\Projects");
    }
}
