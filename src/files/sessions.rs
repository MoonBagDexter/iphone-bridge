//! Claude Code session tracking: spawn `claude remote-control` in a chosen
//! folder (visible console or hidden+logged), track live PIDs, report liveness,
//! and kill the whole tree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::logging::log_both;

/// One tracked session. `id` is our handle; `pid` is the spawned `cmd.exe` PID
/// (killed as a tree so the child `claude` dies with it).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub pid: u32,
    pub path: String,
    pub name: String,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: u64,
    pub hidden: bool,
}

#[derive(Default)]
pub struct SessionStore {
    inner: Mutex<HashMap<String, Session>>,
}

impl SessionStore {
    /// Load the persisted session list (if any), re-validate each entry
    /// against the live process table, and return a store containing only
    /// the survivors. Logs a one-line summary. Best-effort: any read/parse
    /// failure just yields an empty store.
    pub fn load_or_default() -> Self {
        let path = sessions_path();
        let persisted: Vec<Session> = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        let total = persisted.len();
        let mut kept = HashMap::new();
        for s in persisted {
            let alive = is_pid_alive(s.pid);
            let name_match = query_process_image_name(s.pid)
                .map(|image| process_name_matches_cmd(&image));
            if restore_decision(alive, name_match) {
                kept.insert(s.id.clone(), s);
            }
        }
        let dropped = total - kept.len();
        log_both(&format!(
            "[files] restored {} tracked session(s), dropped {dropped}",
            kept.len()
        ));

        let store = SessionStore {
            inner: Mutex::new(kept),
        };
        store.persist();
        store
    }

    fn insert(&self, s: Session) {
        self.inner.lock().unwrap().insert(s.id.clone(), s);
        self.persist();
    }

    fn get(&self, id: &str) -> Option<Session> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    fn remove(&self, id: &str) -> Option<Session> {
        let removed = self.inner.lock().unwrap().remove(id);
        self.persist();
        removed
    }

    /// Write the current session list to `sessions.json` (best-effort).
    fn persist(&self) {
        let list: Vec<Session> = self.inner.lock().unwrap().values().cloned().collect();
        save_sessions(&list);
    }

    /// Snapshot every session with a computed `alive` flag, pruning dead
    /// entries that have been dead-and-idle for over an hour.
    pub fn snapshot_and_prune(&self) -> Vec<SessionView> {
        let now = now_ms();
        let mut guard = self.inner.lock().unwrap();
        let mut views = Vec::new();
        let mut to_remove = Vec::new();
        for (id, s) in guard.iter() {
            let alive = is_pid_alive(s.pid);
            if !alive && now.saturating_sub(s.started_at_ms) > 60 * 60 * 1000 {
                to_remove.push(id.clone());
                continue;
            }
            views.push(SessionView {
                id: s.id.clone(),
                pid: s.pid,
                path: s.path.clone(),
                name: s.name.clone(),
                started_at_ms: s.started_at_ms,
                hidden: s.hidden,
                alive,
            });
        }
        let pruned_any = !to_remove.is_empty();
        for id in to_remove {
            guard.remove(&id);
        }
        if pruned_any {
            let list: Vec<Session> = guard.values().cloned().collect();
            drop(guard);
            save_sessions(&list);
        }
        views.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
        views
    }
}

/// Serialized session view including liveness (the `/api/sessions` shape).
#[derive(Clone, Debug, Serialize)]
pub struct SessionView {
    pub id: String,
    pub pid: u32,
    pub path: String,
    pub name: String,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: u64,
    pub hidden: bool,
    pub alive: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A short pseudo-random id (not a real UUID, but unique enough as a session
/// handle). Hex of clock nanos xored with pid.
fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64;
    let mixed = nanos ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15);
    format!("{mixed:016x}")
}

/// Resolve the `claude` launcher once and cache it. It's normally on PATH but
/// the bridge may run in a service-ish environment; `where claude` finds the
/// `.cmd`/`.exe` shim. We still invoke through `cmd.exe /k` so a `.cmd` shim
/// runs correctly. Falls back to the literal `claude`.
fn claude_command() -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let out = std::process::Command::new("cmd")
                .args(["/c", "where", "claude"])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let s = String::from_utf8_lossy(&o.stdout);
                    let first = s.lines().next().map(str::trim).unwrap_or("");
                    if first.is_empty() {
                        log_both("[files] `where claude` empty; falling back to literal 'claude'");
                        "claude".to_string()
                    } else {
                        log_both(&format!("[files] resolved claude -> {first}"));
                        first.to_string()
                    }
                }
                _ => {
                    log_both("[files] `where claude` failed; falling back to literal 'claude'");
                    "claude".to_string()
                }
            }
        })
        .clone()
}

/// Spawn a Claude Code session in `path`. A user-chosen `name` is passed to
/// the CLI; otherwise the CLI gets no name so the Claude app auto-titles the
/// session, and the folder basename is used only as OUR panel's display name.
/// Returns the tracked `Session` on success.
///
/// Visible: `cmd.exe /k claude --remote-control [..]` in a new console
/// (CREATE_NEW_CONSOLE). Hidden: the same command with CREATE_NO_WINDOW.
pub fn spawn(
    store: &SessionStore,
    path: &Path,
    name: Option<&str>,
    hidden: bool,
) -> Result<Session, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // A non-empty name is a deliberate user choice ("Custom name…") and goes
    // to the CLI. Otherwise the CLI gets none (Claude app auto-titles) and the
    // folder basename only labels the row in our sessions panel.
    let custom_name = match name {
        Some(n) if !n.trim().is_empty() => Some(n.trim().to_string()),
        _ => None,
    };
    let display_name = custom_name.clone().unwrap_or_else(|| {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "claude".to_string())
    });

    let id = new_id();
    let claude = claude_command();

    let inner = build_spawn_cmdline(&claude, custom_name.as_deref());

    let mut cmd = std::process::Command::new("cmd.exe");
    // Rust's default arg quoting escapes embedded quotes MSVC-style (\"),
    // which cmd.exe does not parse — names with spaces arrived mangled. Hand
    // cmd.exe the exact line via raw_arg, wrapped in one outer quote pair
    // that /s makes it strip verbatim.
    cmd.raw_arg("/s");
    cmd.raw_arg(if hidden { "/c" } else { "/k" });
    cmd.raw_arg(format!("\"{inner}\""));
    cmd.current_dir(path);

    if hidden {
        // No stdio redirect: an interactive claude session needs a real
        // (invisible) console to render its TUI; piping stdout would make it
        // drop out of interactive mode. CREATE_NO_WINDOW still allocates a
        // console host — it's just never shown.
        cmd.creation_flags(CREATE_NO_WINDOW);
    } else {
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }

    let child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;
    let pid = child.id();
    // We don't keep the Child handle: for a visible new console we can't hold
    // it meaningfully, and we track/kill by PID uniformly. Dropping the Child
    // does not kill the process on Windows.
    std::mem::forget(child);

    let session = Session {
        id: id.clone(),
        pid,
        path: path.to_string_lossy().into_owned(),
        name: display_name,
        started_at_ms: now_ms(),
        hidden,
    };
    store.insert(session.clone());
    log_both(&format!(
        "[files] spawn session id={id} pid={pid} hidden={hidden} path={}",
        session.path
    ));
    Ok(session)
}

/// Kill a tracked session's whole process tree and drop it from the store.
pub fn kill(store: &SessionStore, id: &str) -> Result<(), String> {
    let Some(s) = store.get(id) else {
        return Err("unknown session id".to_string());
    };
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &s.pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    store.remove(id);
    log_both(&format!("[files] kill session id={id} pid={}", s.pid));
    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("taskkill failed: {e}")),
    }
}

/// Command line run inside `cmd.exe /s /c "<line>"`. Pure so the quoting is
/// testable: claude path and session name each get their own quote pair, and
/// embedded quotes are stripped from the name (cmd.exe cannot escape them).
fn build_spawn_cmdline(claude: &str, session_name: Option<&str>) -> String {
    // Interactive form, NOT `claude remote-control` server mode: server mode
    // registers a cloud-looking "environment" the app spawns fresh sessions
    // into, while `--remote-control` mirrors a normal LOCAL session (user's
    // scripts, CLAUDE.md, MCP config) into the app as-is.
    //
    // The name after --remote-control is an optional positional: omitted, the
    // Claude app auto-titles the session from the first prompt, so only a
    // user-chosen custom name is passed through.
    //
    // `--permission-mode auto`: phone-driven sessions can't be babysat, so
    // start in Auto mode (runs without routine prompts; a background safety
    // classifier still blocks escalations) rather than manual default.
    match session_name {
        Some(n) => {
            let name: String = n.chars().filter(|c| *c != '"').collect();
            format!("\"{claude}\" --remote-control \"{name}\" --permission-mode auto")
        }
        None => format!("\"{claude}\" --remote-control --permission-mode auto"),
    }
}

/// Does a process with this PID currently exist? Uses `OpenProcess` with the
/// cheapest access right; a non-null handle means it's alive.
fn is_pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            false
        } else {
            CloseHandle(handle);
            true
        }
    }
}

/// Best-effort executable name for a PID via `QueryFullProcessImageNameW`.
/// Returns `None` if the process is gone or the query fails (e.g. access
/// denied) -- callers treat that as "can't verify" rather than "mismatch".
fn query_process_image_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Does a full image path's file name match `cmd.exe` (case-insensitive)?
/// Pure so it's directly testable without a live process.
fn process_name_matches_cmd(image_path: &str) -> bool {
    image_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image_path)
        .eq_ignore_ascii_case("cmd.exe")
}

/// Should a persisted session entry be kept across a restart? `alive` is
/// whether the PID currently exists; `name_match` is `Some(true/false)` when
/// we could verify the process's image name is `cmd.exe`, or `None` when the
/// check couldn't be performed at all (e.g. access denied) -- in that case we
/// fall back to trusting the liveness check alone, since the UI's `alive`
/// flag already handles staleness.
///
/// Dead PIDs are always dropped. Alive PIDs are kept unless we positively
/// verified the image name does NOT match `cmd.exe` (a recycled-PID false
/// positive).
fn restore_decision(alive: bool, name_match: Option<bool>) -> bool {
    if !alive {
        return false;
    }
    match name_match {
        Some(matches) => matches,
        None => true,
    }
}

/// Path to the persisted session list.
fn sessions_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
    PathBuf::from(base).join("iphone-bridge").join("sessions.json")
}

/// Persist `sessions` to disk via write-temp-then-rename (same atomicity
/// approach recommended for config.rs): a crash or concurrent read mid-write
/// never observes a truncated file. Best-effort; logs on failure.
fn save_sessions(sessions: &[Session]) {
    let path = sessions_path();
    let Some(dir) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(dir) {
        log_both(&format!("[files] failed to create sessions dir: {e}"));
        return;
    }
    let json = match serde_json::to_string_pretty(sessions) {
        Ok(s) => s,
        Err(e) => {
            log_both(&format!("[files] failed to serialize sessions: {e}"));
            return;
        }
    };
    let tmp_path = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp_path, &json) {
        log_both(&format!("[files] failed to write sessions.json.tmp: {e}"));
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        log_both(&format!("[files] failed to rename sessions.json.tmp: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_quotes_name_with_spaces() {
        assert_eq!(
            build_spawn_cmdline("claude", Some("MetaGrid (test)")),
            "\"claude\" --remote-control \"MetaGrid (test)\" --permission-mode auto"
        );
    }

    #[test]
    fn cmdline_quotes_claude_path_with_spaces() {
        assert_eq!(
            build_spawn_cmdline("C:\\Program Files\\node\\claude.cmd", Some("x")),
            "\"C:\\Program Files\\node\\claude.cmd\" --remote-control \"x\" --permission-mode auto"
        );
    }

    #[test]
    fn cmdline_strips_embedded_quotes_from_name() {
        // cmd.exe has no way to escape a quote inside a quoted arg; embedded
        // quotes must be dropped or they break out of the quoting entirely.
        assert_eq!(
            build_spawn_cmdline("claude", Some("evil\" & calc & \"")),
            "\"claude\" --remote-control \"evil & calc & \" --permission-mode auto"
        );
    }

    #[test]
    fn cmdline_omits_name_for_auto_titling() {
        // No explicit name -> bare --remote-control so Claude Code's automatic
        // session naming takes over (title follows the first prompt).
        assert_eq!(
            build_spawn_cmdline("claude", None),
            "\"claude\" --remote-control --permission-mode auto"
        );
    }

    // --- restore-decision matrix ---

    #[test]
    fn dead_pid_is_always_dropped() {
        assert!(!restore_decision(false, None));
        assert!(!restore_decision(false, Some(true)));
        assert!(!restore_decision(false, Some(false)));
    }

    #[test]
    fn alive_and_name_matches_is_kept() {
        assert!(restore_decision(true, Some(true)));
    }

    #[test]
    fn alive_and_name_mismatch_is_dropped() {
        // Recycled PID now belongs to some other process -> drop it.
        assert!(!restore_decision(true, Some(false)));
    }

    #[test]
    fn alive_and_unverifiable_name_falls_back_to_keep() {
        // Couldn't check the image name (e.g. access denied) -- alive flag in
        // the UI already handles staleness, so keep it rather than losing a
        // legitimate session.
        assert!(restore_decision(true, None));
    }

    // --- process-name matching ---

    #[test]
    fn process_name_matches_cmd_exe_case_insensitive() {
        assert!(process_name_matches_cmd(r"C:\Windows\System32\cmd.exe"));
        assert!(process_name_matches_cmd(r"C:\Windows\System32\CMD.EXE"));
        assert!(!process_name_matches_cmd(r"C:\Windows\System32\notepad.exe"));
        assert!(!process_name_matches_cmd(
            r"C:\Program Files\node\claude.exe"
        ));
    }

    // --- (de)serialization round-trip of the session list ---

    #[test]
    fn session_list_json_roundtrips() {
        let sessions = vec![
            Session {
                id: "abc123".to_string(),
                pid: 4242,
                path: r"C:\Users\thedi\proj".to_string(),
                name: "proj".to_string(),
                started_at_ms: 1_700_000_000_000,
                hidden: true,
            },
            Session {
                id: "def456".to_string(),
                pid: 99,
                path: r"C:\Users\thedi\other (copy)".to_string(),
                name: "other (copy)".to_string(),
                started_at_ms: 1_700_000_001_234,
                hidden: false,
            },
        ];
        let json = serde_json::to_string_pretty(&sessions).unwrap();
        let back: Vec<Session> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].id, "abc123");
        assert_eq!(back[0].pid, 4242);
        assert_eq!(back[0].path, r"C:\Users\thedi\proj");
        assert_eq!(back[0].name, "proj");
        assert_eq!(back[0].started_at_ms, 1_700_000_000_000);
        assert!(back[0].hidden);
        assert_eq!(back[1].id, "def456");
        assert!(!back[1].hidden);
    }

    #[test]
    fn empty_session_list_roundtrips() {
        let sessions: Vec<Session> = Vec::new();
        let json = serde_json::to_string_pretty(&sessions).unwrap();
        let back: Vec<Session> = serde_json::from_str(&json).unwrap();
        assert!(back.is_empty());
    }
}
