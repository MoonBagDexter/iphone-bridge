//! Pure, unit-tested helpers for the batch-2 Files endpoints (search, download,
//! upload, roots disk-space, gitchanges, session-peek, zip, spawn redesign).
//! Every function here is I/O-free except where it takes an explicit predicate,
//! so the tricky logic can be exercised exhaustively without a live machine.

use serde_json::Value;

// --- #1 search: directory skip-list ---

/// Directory names never descended into during a `/api/search` walk. Compared
/// case-insensitively against the entry's file name.
pub const SEARCH_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "__pycache__",
    "venv",
    ".venv",
    "$RECYCLE.BIN",
    "System Volume Information",
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "AppData",
];

/// Should the walker skip descending into a directory with this name?
pub fn is_skipped_dir(name: &str) -> bool {
    SEARCH_SKIP_DIRS
        .iter()
        .any(|s| s.eq_ignore_ascii_case(name))
}

/// Case-insensitive substring match on an entry name (the search predicate).
pub fn name_matches(entry_name: &str, query: &str) -> bool {
    entry_name.to_lowercase().contains(&query.to_lowercase())
}

// --- #2 download: content-type guess + RFC 5987 filename encoding ---

/// Guess a Content-Type from a file extension. Mirrors the endpoint contract:
/// listed text types -> text/plain (json/html get their own), images -> image/*,
/// pdf -> application/pdf, everything else -> application/octet-stream.
pub fn content_type_for(name: &str) -> &'static str {
    let ext = name
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    // A dotfile like ".gitignore" has no extension via rsplit (it yields
    // "gitignore"); treat the bare name that way too.
    let ext = if name.starts_with('.') && !name[1..].contains('.') {
        name[1..].to_ascii_lowercase()
    } else {
        ext
    };
    match ext.as_str() {
        "json" => "application/json",
        "html" | "htm" => "text/html; charset=utf-8",
        "txt" | "md" | "log" | "js" | "mjs" | "ts" | "rs" | "py" | "toml" | "yaml" | "yml"
        | "css" | "csv" | "sql" | "sh" | "ps1" | "bat" | "xml" | "ini" | "cfg" | "conf"
        | "lock" | "gitignore" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Build a `Content-Disposition` header value. ASCII-only filenames use the
/// simple `filename="..."` form; anything with non-ASCII adds the RFC 5987
/// `filename*=UTF-8''...` form (percent-encoded) so phones render the real name.
pub fn content_disposition(disposition: &str, filename: &str) -> String {
    let disp = if disposition == "inline" {
        "inline"
    } else {
        "attachment"
    };
    if filename.is_ascii() && !filename.contains('"') && !filename.contains('\\') {
        format!("{disp}; filename=\"{filename}\"")
    } else {
        let encoded = rfc5987_encode(filename);
        // Provide an ASCII fallback too (best-effort transliteration = '_').
        let ascii_fallback: String = filename
            .chars()
            .map(|c| if c.is_ascii() && c != '"' && c != '\\' { c } else { '_' })
            .collect();
        format!("{disp}; filename=\"{ascii_fallback}\"; filename*=UTF-8''{encoded}")
    }
}

/// Percent-encode per RFC 5987 (attr-char set kept literal, everything else
/// %HH over its UTF-8 bytes).
pub fn rfc5987_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

// --- #3 upload: non-overwriting collision suffix ---

/// Given a desired file `name` and a predicate telling whether a candidate name
/// already exists in the target dir, return the final name to use. On collision
/// insert ` (2)`, ` (3)`, ... before the extension: `a.txt` -> `a (2).txt`,
/// `README` -> `README (2)`. Dotfiles (`.gitignore`) suffix at the end.
pub fn collision_free_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = split_stem_ext(name);
    let mut n = 2u32;
    loop {
        let candidate = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
        if n > 100_000 {
            // Pathological; give up with a timestamp-ish unique-enough suffix.
            return match &ext {
                Some(e) => format!("{stem} ({n}).{e}"),
                None => format!("{stem} ({n})"),
            };
        }
    }
}

/// Split "name.ext" into ("name", Some("ext")). A leading-dot file with no
/// other dot (".gitignore") has no extension -> ("gitignore-with-dot", None) is
/// wrong; we keep the whole thing as the stem so the suffix lands at the end.
fn split_stem_ext(name: &str) -> (String, Option<String>) {
    // Dotfile with a single leading dot and no further dot: no extension.
    if name.starts_with('.') && !name[1..].contains('.') {
        return (name.to_string(), None);
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => {
            (name[..i].to_string(), Some(name[i + 1..].to_string()))
        }
        _ => (name.to_string(), None),
    }
}

// --- #5 gitchanges: porcelain XY status parsing ---

/// One parsed `git status --porcelain` line.
#[derive(Debug, PartialEq, Eq)]
pub struct GitChange {
    pub status: String,
    pub path: String,
}

/// Parse one porcelain v1 line into a single trimmed display status + path.
/// " M file" -> M, "?? file" -> ??, "A  file" -> A, "R  old -> new" -> R (new).
/// Returns None for blank/too-short lines.
pub fn parse_porcelain_line(line: &str) -> Option<GitChange> {
    // Porcelain v1: XY<space>PATH, where XY is exactly 2 columns.
    if line.len() < 4 {
        return None;
    }
    let xy = &line[..2];
    let rest = line[3..].trim_end_matches(['\r', '\n']);
    if rest.is_empty() {
        return None;
    }

    // Untracked / ignored keep their doubled code as the display code.
    let status = if xy == "??" {
        "??".to_string()
    } else if xy == "!!" {
        "!!".to_string()
    } else {
        // Prefer the staged (X) column, else the worktree (Y) column.
        let x = xy.chars().next().unwrap();
        let y = xy.chars().nth(1).unwrap();
        let pick = if x != ' ' { x } else { y };
        pick.to_string()
    };

    // Rename/copy lines are "old -> new"; display the new path.
    let path = if let Some(idx) = rest.find(" -> ") {
        rest[idx + 4..].to_string()
    } else {
        rest.to_string()
    };

    Some(GitChange { status, path })
}

// --- #6 session-peek: cwd -> project slug + JSONL entry extraction ---

/// Derive Claude Code's `.claude/projects/<slug>` directory name from an
/// absolute cwd. Every character that is not ASCII-alphanumeric is replaced by
/// a single `-` (verified against the live projects directory: `C:\Users\thedi`
/// -> `C--Users-thedi`, `C:\...\Let's go` -> `C--...-Let-s-go`, `police AI` ->
/// `police-AI`; case and existing hyphens are preserved, and separators do NOT
/// collapse).
pub fn project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// One candidate transcript file: its name plus creation and modification
/// times in epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeekCandidate {
    pub name: String,
    pub created_ms: u64,
    pub modified_ms: u64,
}

/// Slack window before `started_at_ms` a file's timestamp may still count as
/// "at session start" (clock skew / launch latency).
const PEEK_CREATED_SLACK_MS: u64 = 15_000;
const PEEK_MODIFIED_SLACK_MS: u64 = 60_000;

/// Pick the transcript file that belongs to a spawned session, given every
/// candidate `.jsonl` in the project-slug directory and the session's
/// `started_at_ms`.
///
/// Prefers file CREATION time: among candidates created at or after
/// `started_at_ms - 15s`, pick the earliest-created one (the session's own
/// transcript appears right at launch, before any of its own writes land).
/// This avoids picking another, unrelated Claude session's transcript that
/// happens to have a newer *modified* time because it's still actively being
/// written to.
///
/// Falls back to the old mtime heuristic (newest-modified file with mtime >=
/// started_at_ms - 60s) only when no candidate qualifies by creation time --
/// e.g. on a filesystem/copy where creation time isn't reliable.
pub fn select_peek_file(candidates: &[PeekCandidate], started_at_ms: u64) -> Option<String> {
    let created_floor = started_at_ms.saturating_sub(PEEK_CREATED_SLACK_MS);
    let mut best_by_creation: Option<&PeekCandidate> = None;
    for c in candidates {
        if c.created_ms < created_floor {
            continue;
        }
        match best_by_creation {
            Some(b) if b.created_ms <= c.created_ms => {}
            _ => best_by_creation = Some(c),
        }
    }
    if let Some(c) = best_by_creation {
        return Some(c.name.clone());
    }

    // Fallback: newest-modified file within the old mtime slack window.
    let modified_floor = started_at_ms.saturating_sub(PEEK_MODIFIED_SLACK_MS);
    let mut best_by_mtime: Option<&PeekCandidate> = None;
    for c in candidates {
        if c.modified_ms < modified_floor {
            continue;
        }
        match best_by_mtime {
            Some(b) if b.modified_ms >= c.modified_ms => {}
            _ => best_by_mtime = Some(c),
        }
    }
    best_by_mtime.map(|c| c.name.clone())
}

/// A compact display line extracted from one transcript JSONL entry.
#[derive(Debug, PartialEq, Eq)]
pub struct PeekLine {
    pub role: String,
    pub text: String,
}

/// Extract a compact `{role, text}` from one parsed JSONL entry, or None for
/// non-message lines (summaries, queue ops, progress, entries with no
/// `message.role`). Assistant tool_use blocks render as `[tool: <name>]`; text
/// blocks are truncated to 200 chars.
pub fn peek_line_from_json(v: &Value) -> Option<PeekLine> {
    // Only top-level user/assistant message entries carry a display line.
    let ty = v.get("type").and_then(|x| x.as_str())?;
    if ty != "user" && ty != "assistant" {
        return None;
    }
    let message = v.get("message")?;
    let role = message.get("role").and_then(|x| x.as_str())?.to_string();

    let content = message.get("content")?;
    let text = match content {
        // User messages can be a bare string.
        Value::String(s) => truncate_chars(s.trim(), 200),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            for b in blocks {
                let bt = b.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match bt {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                            let t = t.trim();
                            if !t.is_empty() {
                                parts.push(truncate_chars(t, 200));
                            }
                        }
                    }
                    "tool_use" => {
                        let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("tool");
                        parts.push(format!("[tool: {name}]"));
                    }
                    "tool_result" => {
                        parts.push("[tool result]".to_string());
                    }
                    // thinking / image / other blocks contribute nothing.
                    _ => {}
                }
            }
            parts.join(" ")
        }
        _ => String::new(),
    };

    if text.is_empty() {
        // A message with only thinking/empty content: skip rather than emit a
        // blank row.
        return None;
    }
    Some(PeekLine { role, text })
}

/// Truncate a string to at most `max` characters (not bytes), appending an
/// ellipsis when it was cut.
fn truncate_chars(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

// --- #8 spawn redesign: effective-mode decision ---

/// Requested spawn mode from the client body.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SpawnMode {
    Auto,
    Visible,
    Hidden,
}

impl SpawnMode {
    /// Parse the `mode` string; unknown/empty -> Auto (the documented default).
    pub fn parse(s: Option<&str>) -> SpawnMode {
        match s.map(|x| x.to_ascii_lowercase()).as_deref() {
            Some("visible") => SpawnMode::Visible,
            Some("hidden") => SpawnMode::Hidden,
            _ => SpawnMode::Auto,
        }
    }
}

/// Decide the effective (visible|hidden) spawn given the requested mode and
/// whether this canonical path has been spawned before. auto -> visible the
/// FIRST time (so the user can answer claude's one-time trust prompt in a real
/// console), hidden thereafter. Explicit visible/hidden are honored as-is.
/// Returns `(hidden, effective_label)`.
pub fn decide_spawn(mode: SpawnMode, first_time: bool) -> (bool, &'static str) {
    match mode {
        SpawnMode::Visible => (false, "visible"),
        SpawnMode::Hidden => (true, "hidden"),
        SpawnMode::Auto => {
            if first_time {
                (false, "visible")
            } else {
                (true, "hidden")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- #1 search ---

    #[test]
    fn skip_dirs_case_insensitive() {
        assert!(is_skipped_dir("node_modules"));
        assert!(is_skipped_dir("Node_Modules"));
        assert!(is_skipped_dir(".git"));
        assert!(is_skipped_dir("$RECYCLE.BIN"));
        assert!(is_skipped_dir("system volume information"));
        assert!(!is_skipped_dir("src"));
        assert!(!is_skipped_dir("node_modules_backup"));
    }

    #[test]
    fn name_matches_is_case_insensitive_substring() {
        assert!(name_matches("ReadMe.md", "readme"));
        assert!(name_matches("app.js", "PP"));
        assert!(!name_matches("main.rs", "xyz"));
    }

    // --- #2 download ---

    #[test]
    fn content_type_text_family() {
        assert_eq!(content_type_for("a.txt"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for("a.rs"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for("a.PS1"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for(".gitignore"), "text/plain; charset=utf-8");
    }

    #[test]
    fn content_type_json_and_html_special() {
        assert_eq!(content_type_for("pkg.json"), "application/json");
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("index.htm"), "text/html; charset=utf-8");
    }

    #[test]
    fn content_type_images_and_pdf_and_default() {
        assert_eq!(content_type_for("a.png"), "image/png");
        assert_eq!(content_type_for("a.JPG"), "image/jpeg");
        assert_eq!(content_type_for("a.jpeg"), "image/jpeg");
        assert_eq!(content_type_for("a.svg"), "image/svg+xml");
        assert_eq!(content_type_for("a.ico"), "image/x-icon");
        assert_eq!(content_type_for("a.pdf"), "application/pdf");
        assert_eq!(content_type_for("a.bin"), "application/octet-stream");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
    }

    #[test]
    fn disposition_ascii_simple() {
        assert_eq!(
            content_disposition("attachment", "report.pdf"),
            "attachment; filename=\"report.pdf\""
        );
        assert_eq!(
            content_disposition("inline", "a.txt"),
            "inline; filename=\"a.txt\""
        );
    }

    #[test]
    fn disposition_non_ascii_uses_rfc5987() {
        let d = content_disposition("attachment", "café.pdf");
        assert!(d.contains("filename*=UTF-8''"));
        assert!(d.contains("caf%C3%A9.pdf"));
        // ASCII fallback present with '_' for the non-ascii char.
        assert!(d.contains("filename=\"caf_.pdf\""));
    }

    #[test]
    fn rfc5987_encodes_space_and_unicode() {
        assert_eq!(rfc5987_encode("a b"), "a%20b");
        assert_eq!(rfc5987_encode("é"), "%C3%A9");
        assert_eq!(rfc5987_encode("file-1.txt"), "file-1.txt");
    }

    // --- #3 upload collision ---

    #[test]
    fn no_collision_returns_original() {
        assert_eq!(collision_free_name("a.txt", |_| false), "a.txt");
    }

    #[test]
    fn collision_appends_suffix_before_ext() {
        let existing = |n: &str| n == "a.txt";
        assert_eq!(collision_free_name("a.txt", existing), "a (2).txt");
    }

    #[test]
    fn collision_walks_upward() {
        let existing = |n: &str| matches!(n, "a.txt" | "a (2).txt" | "a (3).txt");
        assert_eq!(collision_free_name("a.txt", existing), "a (4).txt");
    }

    #[test]
    fn collision_no_extension() {
        let existing = |n: &str| n == "README";
        assert_eq!(collision_free_name("README", existing), "README (2)");
    }

    #[test]
    fn collision_dotfile_suffixes_at_end() {
        let existing = |n: &str| n == ".gitignore";
        assert_eq!(collision_free_name(".gitignore", existing), ".gitignore (2)");
    }

    #[test]
    fn collision_multi_dot_uses_last_ext() {
        let existing = |n: &str| n == "archive.tar.gz";
        assert_eq!(
            collision_free_name("archive.tar.gz", existing),
            "archive.tar (2).gz"
        );
    }

    // --- #5 gitchanges ---

    #[test]
    fn porcelain_modified_worktree() {
        let c = parse_porcelain_line(" M src/main.rs").unwrap();
        assert_eq!(c.status, "M");
        assert_eq!(c.path, "src/main.rs");
    }

    #[test]
    fn porcelain_untracked() {
        let c = parse_porcelain_line("?? new.txt").unwrap();
        assert_eq!(c.status, "??");
        assert_eq!(c.path, "new.txt");
    }

    #[test]
    fn porcelain_added_staged() {
        let c = parse_porcelain_line("A  staged.rs").unwrap();
        assert_eq!(c.status, "A");
        assert_eq!(c.path, "staged.rs");
    }

    #[test]
    fn porcelain_rename_shows_new_path() {
        let c = parse_porcelain_line("R  old.rs -> new.rs").unwrap();
        assert_eq!(c.status, "R");
        assert_eq!(c.path, "new.rs");
    }

    #[test]
    fn porcelain_deleted() {
        let c = parse_porcelain_line(" D gone.rs").unwrap();
        assert_eq!(c.status, "D");
    }

    #[test]
    fn porcelain_blank_or_short_is_none() {
        assert!(parse_porcelain_line("").is_none());
        assert!(parse_porcelain_line(" M ").is_none());
    }

    // --- #6 slug + peek ---

    #[test]
    fn slug_matches_live_convention() {
        assert_eq!(project_slug(r"C:\Users\thedi"), "C--Users-thedi");
        assert_eq!(
            project_slug(r"C:\Users\thedi\Desktop\Work\Tools\MetaGrid"),
            "C--Users-thedi-Desktop-Work-Tools-MetaGrid"
        );
        // apostrophe + space each -> one dash
        assert_eq!(
            project_slug(r"C:\Users\thedi\Desktop\Let's go"),
            "C--Users-thedi-Desktop-Let-s-go"
        );
        // space -> dash, case preserved
        assert_eq!(
            project_slug(r"C:\Users\thedi\Desktop\police AI"),
            "C--Users-thedi-Desktop-police-AI"
        );
        assert_eq!(project_slug(r"D:\Quantifull"), "D--Quantifull");
    }

    #[test]
    fn peek_user_string_content() {
        let v = json!({"type":"user","message":{"role":"user","content":"yo"}});
        let p = peek_line_from_json(&v).unwrap();
        assert_eq!(p.role, "user");
        assert_eq!(p.text, "yo");
    }

    #[test]
    fn peek_assistant_text_block() {
        let v = json!({
            "type":"assistant",
            "message":{"role":"assistant","content":[
                {"type":"thinking","thinking":"..."},
                {"type":"text","text":"Here is the answer."}
            ]}
        });
        let p = peek_line_from_json(&v).unwrap();
        assert_eq!(p.role, "assistant");
        assert_eq!(p.text, "Here is the answer.");
    }

    #[test]
    fn peek_assistant_tool_use() {
        let v = json!({
            "type":"assistant",
            "message":{"role":"assistant","content":[
                {"type":"tool_use","name":"Bash","input":{}}
            ]}
        });
        let p = peek_line_from_json(&v).unwrap();
        assert_eq!(p.text, "[tool: Bash]");
    }

    #[test]
    fn peek_skips_non_message_lines() {
        assert!(peek_line_from_json(&json!({"type":"queue-operation"})).is_none());
        assert!(peek_line_from_json(&json!({"type":"summary","summary":"x"})).is_none());
        assert!(peek_line_from_json(&json!({"type":"progress"})).is_none());
        // assistant with only thinking -> no display line
        assert!(peek_line_from_json(&json!({
            "type":"assistant",
            "message":{"role":"assistant","content":[{"type":"thinking","thinking":"x"}]}
        }))
        .is_none());
    }

    #[test]
    fn peek_truncates_long_text() {
        let long = "a".repeat(500);
        let v = json!({"type":"user","message":{"role":"user","content":long}});
        let p = peek_line_from_json(&v).unwrap();
        // 200 chars + ellipsis
        assert_eq!(p.text.chars().count(), 201);
        assert!(p.text.ends_with('…'));
    }

    // --- #6b select_peek_file ---

    #[test]
    fn own_file_created_at_start_chosen_over_older_but_recently_modified() {
        // A different, unrelated session's transcript keeps getting its
        // mtime bumped by ongoing writes, but it was created well before this
        // session started. Our own file was created right at launch. Own file
        // must win even though the other file's mtime is newer.
        let started = 1_700_000_100_000u64;
        let candidates = vec![
            PeekCandidate {
                name: "other-session.jsonl".to_string(),
                created_ms: 1_699_000_000_000, // created long before this session
                modified_ms: 1_700_000_500_000, // but still being actively written
            },
            PeekCandidate {
                name: "own-session.jsonl".to_string(),
                created_ms: 1_700_000_101_000, // created just after session start
                modified_ms: 1_700_000_101_000,
            },
        ];
        assert_eq!(
            select_peek_file(&candidates, started),
            Some("own-session.jsonl".to_string())
        );
    }

    #[test]
    fn multiple_candidates_after_start_picks_earliest_created() {
        let started = 1_700_000_000_000u64;
        let candidates = vec![
            PeekCandidate {
                name: "later.jsonl".to_string(),
                created_ms: 1_700_000_050_000,
                modified_ms: 1_700_000_050_000,
            },
            PeekCandidate {
                name: "earliest.jsonl".to_string(),
                created_ms: 1_700_000_010_000,
                modified_ms: 1_700_000_010_000,
            },
        ];
        assert_eq!(
            select_peek_file(&candidates, started),
            Some("earliest.jsonl".to_string())
        );
    }

    #[test]
    fn within_creation_slack_window_counts_as_after_start() {
        // Created 10s before startedAtMs -- within the 15s slack -- still
        // counts as "the session's own file".
        let started = 1_700_000_100_000u64;
        let candidates = vec![PeekCandidate {
            name: "just-before.jsonl".to_string(),
            created_ms: 1_700_000_090_000, // 10s before start
            modified_ms: 1_700_000_090_000,
        }];
        assert_eq!(
            select_peek_file(&candidates, started),
            Some("just-before.jsonl".to_string())
        );
    }

    #[test]
    fn none_created_after_start_falls_back_to_mtime_heuristic() {
        let started = 1_700_000_100_000u64;
        let candidates = vec![
            PeekCandidate {
                name: "old-created-old-modified.jsonl".to_string(),
                created_ms: 1_000_000_000_000,
                modified_ms: 1_000_000_000_000, // outside mtime slack too -> excluded
            },
            PeekCandidate {
                name: "old-created-recently-modified.jsonl".to_string(),
                created_ms: 1_000_000_000_000, // created long before start (and slack)
                modified_ms: 1_700_000_090_000, // modified within 60s mtime slack
            },
        ];
        assert_eq!(
            select_peek_file(&candidates, started),
            Some("old-created-recently-modified.jsonl".to_string())
        );
    }

    #[test]
    fn empty_candidate_list_returns_none() {
        assert_eq!(select_peek_file(&[], 1_700_000_000_000), None);
    }

    #[test]
    fn no_candidate_qualifies_by_creation_or_mtime_returns_none() {
        let started = 1_700_000_100_000u64;
        let candidates = vec![PeekCandidate {
            name: "ancient.jsonl".to_string(),
            created_ms: 1_000_000_000_000,
            modified_ms: 1_000_000_000_000,
        }];
        assert_eq!(select_peek_file(&candidates, started), None);
    }

    // --- #8 spawn decision ---

    #[test]
    fn spawn_mode_parse_defaults_to_auto() {
        assert_eq!(SpawnMode::parse(None), SpawnMode::Auto);
        assert_eq!(SpawnMode::parse(Some("")), SpawnMode::Auto);
        assert_eq!(SpawnMode::parse(Some("bogus")), SpawnMode::Auto);
        assert_eq!(SpawnMode::parse(Some("Visible")), SpawnMode::Visible);
        assert_eq!(SpawnMode::parse(Some("HIDDEN")), SpawnMode::Hidden);
    }

    #[test]
    fn auto_first_time_is_visible_then_hidden() {
        assert_eq!(decide_spawn(SpawnMode::Auto, true), (false, "visible"));
        assert_eq!(decide_spawn(SpawnMode::Auto, false), (true, "hidden"));
    }

    #[test]
    fn explicit_modes_honored_regardless_of_first_time() {
        assert_eq!(decide_spawn(SpawnMode::Visible, false), (false, "visible"));
        assert_eq!(decide_spawn(SpawnMode::Visible, true), (false, "visible"));
        assert_eq!(decide_spawn(SpawnMode::Hidden, true), (true, "hidden"));
        assert_eq!(decide_spawn(SpawnMode::Hidden, false), (true, "hidden"));
    }
}
