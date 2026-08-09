//! Resolve Windows `.lnk` shell shortcuts to their targets so the Files listing
//! can present a shortcut that points at a folder *as* that folder (tap to
//! navigate in, rather than downloading the tiny `.lnk` blob).
//!
//! A `.lnk` is a regular file the shell interprets -- not a symlink/junction --
//! so `dunce::canonicalize` won't follow it. We resolve targets via
//! `WScript.Shell.CreateShortcut(...).TargetPath` in a single batched PowerShell
//! call (see `pshell`), matching how `trash`/`zip` already shell out. The pure
//! JSON parsing is split out so it's unit-testable without touching disk.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::files::pshell;
use crate::logging::log_both;

/// Case-insensitive `.lnk` extension test.
pub fn is_lnk(name: &str) -> bool {
    name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".lnk")
}

/// Strip a trailing `.lnk` (case-insensitive) for display. `Work Mode.lnk` ->
/// `Work Mode`. Names without the extension are returned unchanged.
pub fn strip_lnk(name: &str) -> String {
    if is_lnk(name) {
        name[..name.len() - 4].to_string()
    } else {
        name.to_string()
    }
}

/// One resolved shortcut target, as emitted by the PowerShell helper.
#[derive(Debug, Clone, Deserialize)]
pub struct Resolved {
    pub lnk: String,
    #[serde(default)]
    pub target: String,
    #[serde(rename = "isDir", default)]
    pub is_dir: bool,
}

/// Parse the PowerShell JSON (always a top-level array, see `SCRIPT`) into a
/// map keyed by the `.lnk` path (lowercased) for case-insensitive lookup.
pub fn parse_resolved(json: &str) -> HashMap<String, Resolved> {
    let mut map = HashMap::new();
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return map;
    }
    match serde_json::from_str::<Vec<Resolved>>(trimmed) {
        Ok(list) => {
            for r in list {
                map.insert(r.lnk.to_lowercase(), r);
            }
        }
        Err(e) => log_both(&format!("[lnk] could not parse resolver output: {e}; raw: {trimmed}")),
    }
    map
}

/// Cap on how many shortcuts we resolve per listing, so a Start-Menu-sized
/// folder can't stall a directory read behind a giant COM loop.
const MAX_RESOLVE: usize = 128;

/// Resolve every path in `lnk_paths` (absolute `.lnk` files) to its target in a
/// single PowerShell call. Returns a map keyed by lowercased `.lnk` path. Any
/// failure (spawn error, timeout, bad JSON) yields an empty map -- the caller
/// then just leaves those entries as plain files.
pub fn resolve_targets(lnk_paths: &[String]) -> HashMap<String, Resolved> {
    if lnk_paths.is_empty() {
        return HashMap::new();
    }
    let slice = if lnk_paths.len() > MAX_RESOLVE {
        &lnk_paths[..MAX_RESOLVE]
    } else {
        lnk_paths
    };
    // Pass the paths as one base64 (UTF-16LE) newline-joined blob so nothing is
    // string-interpolated raw into the script.
    let joined = slice.join("\n");
    let b64 = pshell::b64_utf16(&joined);
    let script = format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
$blob = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{b64}'))
$paths = $blob -split "`n" | Where-Object {{ $_ -ne '' }}
$sh = New-Object -ComObject WScript.Shell
$parts = foreach ($p in $paths) {{
    $target = ''
    $isDir = $false
    try {{
        $lnk = $sh.CreateShortcut($p)
        $target = [string]$lnk.TargetPath
        if ($target -and (Test-Path -LiteralPath $target)) {{
            $isDir = [bool](Get-Item -LiteralPath $target -Force).PSIsContainer
        }}
    }} catch {{}}
    ([pscustomobject]@{{ lnk = $p; target = $target; isDir = $isDir }} | ConvertTo-Json -Compress)
}}
# Always emit a JSON array, even for a single element (ConvertTo-Json would
# otherwise collapse one object to a non-array).
'[' + ($parts -join ',') + ']'
"#
    );

    match pshell::run_powershell("lnk", &script, Duration::from_secs(12)) {
        Ok(out) => parse_resolved(&out),
        Err(e) => {
            log_both(&format!("[lnk] resolve failed: {e}"));
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_lnk_is_case_insensitive_and_needs_a_stem() {
        assert!(is_lnk("Work Mode.lnk"));
        assert!(is_lnk("thing.LNK"));
        assert!(is_lnk("a.Lnk"));
        assert!(!is_lnk(".lnk")); // extension only, no stem
        assert!(!is_lnk("notes.txt"));
        assert!(!is_lnk("lnk"));
    }

    #[test]
    fn strip_lnk_removes_only_the_extension() {
        assert_eq!(strip_lnk("Work Mode.lnk"), "Work Mode");
        assert_eq!(strip_lnk("Hermes.LNK"), "Hermes");
        assert_eq!(strip_lnk("plain.txt"), "plain.txt");
    }

    #[test]
    fn parse_resolved_keys_case_insensitively() {
        let json = r#"[
            {"lnk":"C:\\Users\\me\\Work Mode.lnk","target":"D:\\Work","isDir":true},
            {"lnk":"C:\\Users\\me\\Hermes.lnk","target":"C:\\apps\\hermes.exe","isDir":false}
        ]"#;
        let map = parse_resolved(json);
        assert_eq!(map.len(), 2);
        let w = map.get(&r"c:\users\me\work mode.lnk".to_string()).unwrap();
        assert_eq!(w.target, r"D:\Work");
        assert!(w.is_dir);
        let h = map.get(&r"c:\users\me\hermes.lnk".to_string()).unwrap();
        assert!(!h.is_dir);
    }

    #[test]
    fn parse_resolved_tolerates_empty_and_garbage() {
        assert!(parse_resolved("").is_empty());
        assert!(parse_resolved("   ").is_empty());
        assert!(parse_resolved("not json").is_empty());
        assert!(parse_resolved("[]").is_empty());
    }

    #[test]
    fn parse_resolved_defaults_missing_fields() {
        // A broken shortcut may resolve to an empty target.
        let json = r#"[{"lnk":"C:\\x\\broken.lnk","target":"","isDir":false}]"#;
        let map = parse_resolved(json);
        let r = map.get(&r"c:\x\broken.lnk".to_string()).unwrap();
        assert_eq!(r.target, "");
        assert!(!r.is_dir);
    }
}
