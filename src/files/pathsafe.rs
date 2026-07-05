//! Pure path-safety + name-validation helpers for the Files API.
//!
//! Every endpoint that accepts a caller-supplied path must confirm the path
//! resolves *inside* one of the effective roots for the current scope. These
//! functions are deliberately pure (no I/O beyond the explicit canonicalize
//! wrapper) so they can be unit-tested exhaustively against traversal,
//! slash-direction, case, short-name and junction-escape tricks.

use std::path::{Path, PathBuf};

/// Windows reserved device names that can never be used as a file/folder name,
/// with or without an extension (`CON`, `CON.txt`, ...). Compared case-insensitively.
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Characters that are illegal in a Windows file/folder name. Includes both
/// path separators (a name must never contain them) and the reserved set.
const ILLEGAL_NAME_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Validate a single path component the user wants to create/rename to.
/// Returns `Ok(())` if the name is a legal, single-level Windows folder name.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    if name.len() > 255 {
        return Err("name too long".to_string());
    }
    // "." and ".." are never valid new folder names.
    if name == "." || name == ".." {
        return Err("name cannot be '.' or '..'".to_string());
    }
    for c in name.chars() {
        if (c as u32) < 0x20 {
            return Err("name contains a control character".to_string());
        }
        if ILLEGAL_NAME_CHARS.contains(&c) {
            return Err(format!("name contains an illegal character: {c:?}"));
        }
    }
    // Windows forbids trailing space or dot on a name.
    if name.ends_with(' ') || name.ends_with('.') {
        return Err("name cannot end with a space or dot".to_string());
    }
    // Reserved device names, with or without an extension.
    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED_DEVICE_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
    {
        return Err(format!("'{name}' is a reserved device name"));
    }
    Ok(())
}

/// Normalize a path for prefix comparison: canonical-ish lowercase string with
/// forward slashes stripped of any trailing separator. This is only used for
/// the *already-canonicalized* absolute paths, so it does NOT resolve `..` --
/// that must have happened during canonicalization.
fn normalize_for_compare(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.replace('/', "\\");
    let trimmed = s.trim_end_matches('\\');
    trimmed.to_lowercase()
}

/// Is `candidate` the same as, or nested inside, `root`?
/// Both paths must already be canonicalized (real, absolute). The comparison is
/// case-insensitive (Windows) and boundary-correct: `...\Work2` does NOT count
/// as inside `...\Work`.
fn is_within(candidate: &Path, root: &Path) -> bool {
    let cand = normalize_for_compare(candidate);
    let root = normalize_for_compare(root);
    if cand == root {
        return true;
    }
    // Nested: candidate must start with root + separator, so a shared prefix
    // like Work / Work2 is rejected.
    let root_with_sep = format!("{root}\\");
    cand.starts_with(&root_with_sep)
}

/// Pure allow-check over already-canonicalized paths. `candidate` is allowed if
/// it equals, or is nested inside, any of `effective_roots`.
pub fn is_path_allowed(candidate: &Path, effective_roots: &[PathBuf]) -> bool {
    effective_roots.iter().any(|r| is_within(candidate, r))
}

/// Canonicalize a path to its real on-disk form using `dunce` (which avoids the
/// `\\?\` verbatim prefix that breaks naive string comparisons). Resolves `..`,
/// short (8.3) names, and symlink/junction targets. Fails for paths that don't
/// exist.
pub fn canonicalize(p: &Path) -> std::io::Result<PathBuf> {
    dunce::canonicalize(p)
}

/// Full check for an *existing* path: canonicalize it and both the roots, then
/// verify containment. Returns the canonical candidate on success.
///
/// `effective_roots` are the raw (config) root paths; they are canonicalized
/// here too so that a root given with a short name / trailing slash still
/// matches. Roots that fail to canonicalize (e.g. a removed drive) are skipped.
pub fn resolve_allowed(
    candidate: &Path,
    effective_roots: &[PathBuf],
) -> Result<PathBuf, String> {
    let canon = canonicalize(candidate).map_err(|_| "path does not exist".to_string())?;
    let canon_roots = canonicalize_roots(effective_roots);
    if is_path_allowed(&canon, &canon_roots) {
        Ok(canon)
    } else {
        Err("path is outside the allowed folders".to_string())
    }
}

/// Canonicalize each root, dropping any that don't resolve (removed drive,
/// deleted folder). Callers compare against the returned canonical set.
pub fn canonicalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|r| canonicalize(r).ok())
        .collect()
}

/// Is `candidate` (canonicalized) *equal to* one of the effective roots? Used
/// to refuse deleting a root, and to null out `parent` when at a root.
pub fn is_effective_root(candidate: &Path, canon_roots: &[PathBuf]) -> bool {
    let cand = normalize_for_compare(candidate);
    canon_roots
        .iter()
        .any(|r| normalize_for_compare(r) == cand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn roots() -> Vec<PathBuf> {
        vec![
            PathBuf::from(r"C:\Users\thedi\Desktop\Work"),
            PathBuf::from(r"C:\Users\thedi\Desktop"),
        ]
    }

    // --- is_path_allowed: pure, over already-canonical paths ---

    #[test]
    fn root_itself_is_allowed() {
        assert!(is_path_allowed(
            &PathBuf::from(r"C:\Users\thedi\Desktop\Work"),
            &roots()
        ));
    }

    #[test]
    fn nested_path_is_allowed() {
        assert!(is_path_allowed(
            &PathBuf::from(r"C:\Users\thedi\Desktop\Work\project\src"),
            &roots()
        ));
    }

    #[test]
    fn sibling_with_shared_prefix_is_rejected() {
        // Work2 must NOT match root Work.
        assert!(!is_path_allowed(
            &PathBuf::from(r"C:\Users\thedi\Desktop\Work2"),
            &[PathBuf::from(r"C:\Users\thedi\Desktop\Work")]
        ));
    }

    #[test]
    fn path_above_root_is_rejected() {
        assert!(!is_path_allowed(
            &PathBuf::from(r"C:\Users\thedi"),
            &[PathBuf::from(r"C:\Users\thedi\Desktop\Work")]
        ));
    }

    #[test]
    fn unrelated_drive_is_rejected() {
        assert!(!is_path_allowed(
            &PathBuf::from(r"D:\secrets"),
            &roots()
        ));
    }

    #[test]
    fn case_insensitive_match() {
        assert!(is_path_allowed(
            &PathBuf::from(r"c:\users\THEDI\desktop\WORK\thing"),
            &roots()
        ));
    }

    #[test]
    fn forward_slashes_are_normalized() {
        assert!(is_path_allowed(
            &PathBuf::from("C:/Users/thedi/Desktop/Work/x"),
            &roots()
        ));
    }

    #[test]
    fn trailing_separator_on_root_still_matches() {
        assert!(is_path_allowed(
            &PathBuf::from(r"C:\Users\thedi\Desktop\Work\"),
            &roots()
        ));
    }

    #[test]
    fn empty_roots_allow_nothing() {
        assert!(!is_path_allowed(
            &PathBuf::from(r"C:\Users\thedi\Desktop\Work"),
            &[]
        ));
    }

    // --- is_effective_root ---

    #[test]
    fn effective_root_detection() {
        let r = roots();
        assert!(is_effective_root(
            &PathBuf::from(r"C:\Users\thedi\Desktop\Work"),
            &r
        ));
        assert!(!is_effective_root(
            &PathBuf::from(r"C:\Users\thedi\Desktop\Work\sub"),
            &r
        ));
    }

    // --- validate_name ---

    #[test]
    fn valid_names_pass() {
        for n in ["project", "my-folder", "a.b.c", "New Folder", "日本語", "v1.2.3"] {
            assert!(validate_name(n).is_ok(), "expected {n:?} to be valid");
        }
    }

    #[test]
    fn empty_name_rejected() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn dot_names_rejected() {
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
    }

    #[test]
    fn path_separators_rejected() {
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("..\\escape").is_err());
    }

    #[test]
    fn illegal_chars_rejected() {
        for n in ["a:b", "a<b", "a>b", "a\"b", "a|b", "a?b", "a*b"] {
            assert!(validate_name(n).is_err(), "expected {n:?} to be rejected");
        }
    }

    #[test]
    fn control_chars_rejected() {
        assert!(validate_name("a\u{0007}b").is_err());
    }

    #[test]
    fn trailing_space_or_dot_rejected() {
        assert!(validate_name("name ").is_err());
        assert!(validate_name("name.").is_err());
    }

    #[test]
    fn reserved_device_names_rejected() {
        for n in ["CON", "con", "NUL", "nul.txt", "COM1", "lpt9", "AUX", "PRN"] {
            assert!(validate_name(n).is_err(), "expected {n:?} to be rejected");
        }
    }

    #[test]
    fn reserved_name_as_substring_is_ok() {
        // CONSOLE is not reserved; only the exact stem CON is.
        assert!(validate_name("CONSOLE").is_ok());
        assert!(validate_name("COM10").is_ok());
    }
}
