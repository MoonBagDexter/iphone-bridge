//! HTTP handlers for the Files API, plus the PIN auth middleware. All routes
//! live under `/api/...` and require the `x-bridge-pin` header. Errors return
//! an appropriate status and `{"error":"<human message>"}`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Multipart, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::files::config::{self, NamedRoot};
use crate::files::features;
use crate::files::pathsafe;
use crate::files::pshell;
use crate::files::sessions;
use crate::files::shortcuts;
use crate::files::trash;
use crate::logging::log_both;
use crate::net::server::AppState;

/// Per-request cap for `/api/upload` multipart bodies (200 MB).
pub const UPLOAD_BODY_LIMIT: usize = 200 * 1024 * 1024;
/// Refuse to stream files larger than this via `/api/download` (512 MB).
const DOWNLOAD_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Constant-time byte comparison of two strings. Avoids leaking the PIN via
/// early-exit timing. Length mismatch still returns false but in constant time
/// over the shorter buffer plus a length check.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// JSON error response helper.
fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

/// PIN auth middleware for `/api/*`. Checks the `x-bridge-pin` header against
/// the configured PIN in constant time.
pub async fn require_pin(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let provided = req
        .headers()
        .get("x-bridge-pin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (ok, pin_len) = {
        let cfg = state.config.read().unwrap();
        (
            constant_time_eq(provided.as_bytes(), cfg.pin.as_bytes()),
            cfg.pin.chars().count(),
        )
    };
    if ok {
        next.run(req).await
    } else {
        // pinLength lets the phone's passcode pad draw the right number of
        // dots and auto-submit; leaking the length is an accepted trade-off.
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized", "pinLength": pin_len })),
        )
            .into_response()
    }
}

// --- GET /api/roots ---

pub async fn roots(State(state): State<AppState>) -> Response {
    let cfg = state.config.read().unwrap();
    let scope = match cfg.scope {
        config::Scope::Roots => "roots",
        config::Scope::Profile => "profile",
        config::Scope::Drives => "drives",
    };
    let configured = cfg.configured_named_roots();
    let effective = cfg.effective_named_roots();
    drop(cfg);
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    let shortcuts = shortcuts::resolve_shortcuts(&profile, shortcuts::path_exists);
    Json(json!({
        "scope": scope,
        "roots": named_roots_json(&configured),
        "effectiveRoots": named_roots_with_space_json(&effective),
        "shortcuts": shortcuts,
    }))
    .into_response()
}

fn named_roots_json(roots: &[NamedRoot]) -> Vec<serde_json::Value> {
    roots
        .iter()
        .map(|r| json!({ "name": r.name, "path": r.path }))
        .collect()
}

/// Like `named_roots_json` but each entry also carries `freeBytes`/`totalBytes`
/// (null on failure) via `GetDiskFreeSpaceExW`. Disk-space probing never errors
/// the request.
fn named_roots_with_space_json(roots: &[NamedRoot]) -> Vec<serde_json::Value> {
    roots
        .iter()
        .map(|r| {
            let (free, total) = disk_space(&r.path);
            json!({
                "name": r.name,
                "path": r.path,
                "freeBytes": free,
                "totalBytes": total,
            })
        })
        .collect()
}

/// Free / total bytes for the volume containing `path`, via
/// `GetDiskFreeSpaceExW`. Returns `(None, None)` on any failure.
fn disk_space(path: &str) -> (Option<u64>, Option<u64>) {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free_to_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 {
        (None, None)
    } else {
        (Some(free_to_caller), Some(total))
    }
}

// --- POST /api/scope ---

#[derive(Deserialize)]
pub struct ScopeBody {
    scope: String,
}

/// Change how much of the filesystem the Files tab may browse. Mirrors the
/// tray's Folder-access submenu so the scope can be switched from the phone.
pub async fn set_scope(State(state): State<AppState>, Json(body): Json<ScopeBody>) -> Response {
    let scope = match body.scope.as_str() {
        "roots" => config::Scope::Roots,
        "profile" => config::Scope::Profile,
        "drives" => config::Scope::Drives,
        other => return err(StatusCode::BAD_REQUEST, &format!("unknown scope: {other}")),
    };
    let snapshot = {
        let mut cfg = state.config.write().unwrap();
        cfg.scope = scope;
        cfg.clone()
    };
    config::save(&snapshot);
    log_both(&format!("[files] scope -> {scope:?} (via phone)"));
    // Echo back the new effective roots so the client can re-render immediately.
    let effective = snapshot.effective_named_roots();
    Json(json!({
        "scope": body.scope,
        "effectiveRoots": named_roots_with_space_json(&effective),
    }))
    .into_response()
}

// --- GET /api/ls?path=<abs> ---

#[derive(Deserialize)]
pub struct PathQuery {
    path: String,
}

pub async fn ls(State(state): State<AppState>, Query(q): Query<PathQuery>) -> Response {
    let (canon, canon_roots) = match resolve_existing(&state, &q.path) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };

    let read = match std::fs::read_dir(&canon) {
        Ok(rd) => rd,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("cannot read directory: {e}")),
    };

    // First pass: raw directory entries. `.lnk` targets are filled in below.
    struct Row {
        name: String,
        path: String,
        is_dir: bool,
        is_repo: bool,
        mtime_ms: u64,
        is_shortcut: bool,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut lnk_paths: Vec<String> = Vec::new();
    for de in read.flatten() {
        let p = de.path();
        let name = de.file_name().to_string_lossy().into_owned();
        let meta = de.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let mtime_ms = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let is_repo = is_dir && p.join(".git").is_dir();
        let path_str = p.to_string_lossy().into_owned();
        if !is_dir && crate::files::lnk::is_lnk(&name) {
            lnk_paths.push(path_str.clone());
        }
        rows.push(Row {
            name,
            path: path_str,
            is_dir,
            is_repo,
            mtime_ms,
            is_shortcut: false,
        });
    }

    // Resolve `.lnk` files that point at a *folder inside the current scope* into
    // navigable folder rows (path repointed at the target). Shortcuts to files,
    // exes, or folders outside the scope are left as plain downloadable `.lnk`s.
    if !lnk_paths.is_empty() {
        let resolved =
            tokio::task::spawn_blocking(move || crate::files::lnk::resolve_targets(&lnk_paths))
                .await
                .unwrap_or_default();
        for row in rows.iter_mut() {
            if row.is_dir {
                continue;
            }
            let Some(r) = resolved.get(&row.path.to_lowercase()) else {
                continue;
            };
            if !r.is_dir || r.target.is_empty() {
                continue;
            }
            let target = Path::new(&r.target);
            let Ok(canon_target) = pathsafe::canonicalize(target) else {
                continue;
            };
            if !pathsafe::is_path_allowed(&canon_target, &canon_roots) {
                continue;
            }
            row.name = crate::files::lnk::strip_lnk(&row.name);
            row.path = canon_target.to_string_lossy().into_owned();
            row.is_dir = true;
            row.is_repo = canon_target.join(".git").is_dir();
            row.is_shortcut = true;
        }
    }

    let mut entries: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "name": r.name,
                "path": r.path,
                "isDir": r.is_dir,
                "isRepo": r.is_repo,
                "mtimeMs": r.mtime_ms,
                "isShortcut": r.is_shortcut,
            })
        })
        .collect();

    // Dirs first, then files; each group alphabetical, case-insensitive.
    entries.sort_by(|a, b| {
        let ad = a["isDir"].as_bool().unwrap_or(false);
        let bd = b["isDir"].as_bool().unwrap_or(false);
        bd.cmp(&ad).then_with(|| {
            let an = a["name"].as_str().unwrap_or("").to_lowercase();
            let bn = b["name"].as_str().unwrap_or("").to_lowercase();
            an.cmp(&bn)
        })
    });

    // parent is null when this path IS an effective root.
    let parent = if pathsafe::is_effective_root(&canon, &canon_roots) {
        serde_json::Value::Null
    } else {
        match canon.parent() {
            Some(p) if pathsafe::is_path_allowed(p, &canon_roots) => {
                json!(p.to_string_lossy())
            }
            _ => serde_json::Value::Null,
        }
    };

    Json(json!({
        "path": canon.to_string_lossy(),
        "parent": parent,
        "entries": entries,
    }))
    .into_response()
}

// --- GET /api/gitstatus?path=<abs> ---

pub async fn gitstatus(State(state): State<AppState>, Query(q): Query<PathQuery>) -> Response {
    let (canon, _roots) = match resolve_existing(&state, &q.path) {
        Ok(v) => v,
        // Outside roots -> treat as not dirty rather than leaking existence.
        Err(_) => return Json(json!({ "dirty": false })).into_response(),
    };
    let dirty = git_dirty(&canon).await;
    Json(json!({ "dirty": dirty })).into_response()
}

/// `git -C <path> status --porcelain` with a 5s timeout; anything non-clean
/// (non-repo, git missing, timeout, error) reports `dirty: false`.
async fn git_dirty(path: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let path = path.to_path_buf();
    let handle = tokio::task::spawn_blocking(move || {
        let mut child = match std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["status", "--porcelain"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        // Poll for up to 5s, then kill.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return false;
                    }
                    use std::io::Read;
                    let mut out = String::new();
                    if let Some(mut so) = child.stdout.take() {
                        let _ = so.read_to_string(&mut out);
                    }
                    return !out.trim().is_empty();
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return false;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(_) => return false,
            }
        }
    });
    handle.await.unwrap_or(false)
}

// --- POST /api/mkdir ---

#[derive(Deserialize)]
pub struct MkdirBody {
    parent: String,
    name: String,
}

pub async fn mkdir(State(state): State<AppState>, Json(body): Json<MkdirBody>) -> Response {
    if let Err(e) = pathsafe::validate_name(&body.name) {
        return err(StatusCode::BAD_REQUEST, &e);
    }
    let (parent_canon, _roots) = match resolve_existing(&state, &body.parent) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };
    let new_path = parent_canon.join(&body.name);
    match std::fs::create_dir(&new_path) {
        Ok(()) => Json(json!({ "path": new_path.to_string_lossy() })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &format!("could not create folder: {e}")),
    }
}

// --- POST /api/rename ---

#[derive(Deserialize)]
pub struct RenameBody {
    path: String,
    #[serde(rename = "newName")]
    new_name: String,
}

pub async fn rename(State(state): State<AppState>, Json(body): Json<RenameBody>) -> Response {
    if let Err(e) = pathsafe::validate_name(&body.new_name) {
        return err(StatusCode::BAD_REQUEST, &e);
    }
    let (canon, canon_roots) = match resolve_existing(&state, &body.path) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };
    // Refuse to rename an effective root itself.
    if pathsafe::is_effective_root(&canon, &canon_roots) {
        return err(StatusCode::FORBIDDEN, "cannot rename a root folder");
    }
    let Some(parent) = canon.parent() else {
        return err(StatusCode::BAD_REQUEST, "path has no parent");
    };
    let new_path = parent.join(&body.new_name);
    match std::fs::rename(&canon, &new_path) {
        Ok(()) => Json(json!({ "path": new_path.to_string_lossy() })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &format!("could not rename: {e}")),
    }
}

// --- POST /api/delete ---

#[derive(Deserialize)]
pub struct DeleteBody {
    path: String,
}

pub async fn delete(State(state): State<AppState>, Json(body): Json<DeleteBody>) -> Response {
    let (canon, canon_roots) = match resolve_existing(&state, &body.path) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };
    if pathsafe::is_effective_root(&canon, &canon_roots) {
        return err(StatusCode::FORBIDDEN, "cannot delete a root folder");
    }
    match crate::files::delete::to_recycle_bin(&canon) {
        Ok(()) => {
            log_both(&format!("[files] delete->recycle {}", canon.to_string_lossy()));
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

// --- POST /api/spawn ---

#[derive(Deserialize)]
pub struct SpawnBody {
    path: String,
    #[serde(default)]
    name: Option<String>,
    /// "auto" | "visible" | "hidden" (default "auto" if absent). Replaces the
    /// old `hidden` bool.
    #[serde(default)]
    mode: Option<String>,
}

pub async fn spawn(State(state): State<AppState>, Json(body): Json<SpawnBody>) -> Response {
    let (canon, _roots) = match resolve_existing(&state, &body.path) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };
    let canon_str = canon.to_string_lossy().into_owned();

    let mode = features::SpawnMode::parse(body.mode.as_deref());
    let first_time = {
        let cfg = state.config.read().unwrap();
        !cfg.has_spawned_dir(&canon_str)
    };
    let (hidden, effective) = features::decide_spawn(mode, first_time);
    log_both(&format!(
        "[files] spawn decision mode={mode:?} firstTime={first_time} -> {effective} path={canon_str}"
    ));

    let name = body.name.as_deref();
    match sessions::spawn(&state.sessions, &canon, name, hidden) {
        Ok(s) => {
            // Remember this dir so future auto spawns go hidden; persist only if
            // the set actually changed.
            let changed = {
                let mut cfg = state.config.write().unwrap();
                cfg.remember_spawned_dir(&canon_str)
            };
            if changed {
                let snapshot = state.config.read().unwrap().clone();
                config::save(&snapshot);
                log_both(&format!("[files] recorded spawned dir {canon_str}"));
            }
            Json(json!({
                "id": s.id,
                "pid": s.pid,
                "mode": effective,
                "firstTime": first_time,
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// --- GET /api/sessions ---

pub async fn list_sessions(State(state): State<AppState>) -> Response {
    let views = state.sessions.snapshot_and_prune();
    Json(json!({ "sessions": views })).into_response()
}

// --- POST /api/kill ---

#[derive(Deserialize)]
pub struct KillBody {
    id: String,
}

pub async fn kill(State(state): State<AppState>, Json(body): Json<KillBody>) -> Response {
    match sessions::kill(&state.sessions, &body.id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

// --- GET /api/trash ---

pub async fn list_trash(State(_state): State<AppState>) -> Response {
    match tokio::task::spawn_blocking(trash::list).await {
        Ok(Ok(items)) => Json(json!({ "items": items })).into_response(),
        Ok(Err(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, &e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task join error: {e}")),
    }
}

// --- POST /api/trash/restore ---

#[derive(Deserialize)]
pub struct TrashRestoreBody {
    id: String,
}

pub async fn restore_trash(
    State(_state): State<AppState>,
    Json(body): Json<TrashRestoreBody>,
) -> Response {
    let id = body.id;
    match tokio::task::spawn_blocking(move || trash::restore(&id)).await {
        Ok(Ok(restored_to)) => Json(json!({ "ok": true, "restoredTo": restored_to })).into_response(),
        Ok(Err(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, &e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task join error: {e}")),
    }
}

// --- GET /api/search?root=<abs>&q=<string> ---

#[derive(Deserialize)]
pub struct SearchQuery {
    root: String,
    q: String,
}

pub async fn search(State(state): State<AppState>, Query(q): Query<SearchQuery>) -> Response {
    if q.q.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "query cannot be empty");
    }
    let (root_canon, _roots) = match resolve_existing(&state, &q.root) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };
    let needle = q.q.clone();
    let result = tokio::task::spawn_blocking(move || search_walk(&root_canon, &needle)).await;
    match result {
        Ok((results, truncated, took_ms)) => {
            log_both(&format!(
                "[files] search q={:?} -> {} hit(s) truncated={truncated} {took_ms}ms",
                q.q,
                results.len()
            ));
            Json(json!({
                "results": results,
                "truncated": truncated,
                "tookMs": took_ms,
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task join error: {e}")),
    }
}

/// BFS over the tree under `root`, matching the case-insensitive substring
/// `needle` against entry names. Caps: 100 results, 10s wall clock, depth 12,
/// skipping the well-known heavy dirs. Returns (results, truncated, tookMs).
fn search_walk(root: &Path, needle: &str) -> (Vec<serde_json::Value>, bool, u64) {
    const MAX_RESULTS: usize = 100;
    const MAX_DEPTH: usize = 12;
    let wall = Duration::from_secs(10);
    let start = Instant::now();

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut truncated = false;
    // Queue of (dir, depth). BFS so shallow hits come first.
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));

    'outer: while let Some((dir, depth)) = queue.pop_front() {
        if start.elapsed() >= wall {
            truncated = true;
            break;
        }
        let read = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for de in read.flatten() {
            if start.elapsed() >= wall {
                truncated = true;
                break 'outer;
            }
            let name = de.file_name().to_string_lossy().into_owned();
            let is_dir = de.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if features::name_matches(&name, needle) {
                let p = de.path();
                let parent = p
                    .parent()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default();
                results.push(json!({
                    "name": name,
                    "path": p.to_string_lossy(),
                    "isDir": is_dir,
                    "parent": parent,
                }));
                if results.len() >= MAX_RESULTS {
                    truncated = true;
                    break 'outer;
                }
            }
            // Descend into non-skipped subdirectories within the depth cap.
            if is_dir && depth < MAX_DEPTH && !features::is_skipped_dir(&name) {
                queue.push_back((de.path(), depth + 1));
            }
        }
    }

    (results, truncated, start.elapsed().as_millis() as u64)
}

// --- GET /api/download?path=<abs>&disposition=inline|attachment ---

#[derive(Deserialize)]
pub struct DownloadQuery {
    path: String,
    #[serde(default)]
    disposition: Option<String>,
}

pub async fn download(State(state): State<AppState>, Query(q): Query<DownloadQuery>) -> Response {
    let (canon, _roots) = match resolve_existing(&state, &q.path) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };

    let meta = match tokio::fs::metadata(&canon).await {
        Ok(m) => m,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("cannot stat file: {e}")),
    };
    if meta.is_dir() {
        return err(StatusCode::BAD_REQUEST, "path is a directory, not a file");
    }
    if meta.len() > DOWNLOAD_MAX_BYTES {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "file exceeds 512 MB download cap");
    }

    let filename = canon
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    let disposition = q.disposition.as_deref().unwrap_or("attachment");

    stream_file_response(&canon, &filename, disposition, meta.len()).await
}

/// Open `path` and build a streaming response with the right Content-Type,
/// Content-Length, and Content-Disposition. Shared by /api/download and /api/zip.
async fn stream_file_response(
    path: &Path,
    download_name: &str,
    disposition: &str,
    len: u64,
) -> Response {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("cannot open file: {e}")),
    };
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = Body::from_stream(stream);
    let ctype = features::content_type_for(download_name);
    let cdisp = features::content_disposition(disposition, download_name);
    log_both(&format!(
        "[files] download {} ({} bytes) type={ctype}",
        path.to_string_lossy(),
        len
    ));
    (
        [
            (header::CONTENT_TYPE, ctype.to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_DISPOSITION, cdisp),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    )
        .into_response()
}

// --- POST /api/upload?dir=<abs> (multipart, field "files") ---

#[derive(Deserialize)]
pub struct UploadQuery {
    dir: String,
}

pub async fn upload(
    State(state): State<AppState>,
    Query(q): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Response {
    let (dir_canon, _roots) = match resolve_existing(&state, &q.dir) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };
    if !dir_canon.is_dir() {
        return err(StatusCode::BAD_REQUEST, "upload target is not a directory");
    }

    let mut saved: Vec<String> = Vec::new();
    let mut rejected: Vec<serde_json::Value> = Vec::new();

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return err(StatusCode::BAD_REQUEST, &format!("malformed multipart: {e}"))
            }
        };
        // Only the `files` field carries uploads; skip anything else.
        if field.name() != Some("files") {
            continue;
        }
        let client_name = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_default();
        if client_name.is_empty() {
            rejected.push(json!({ "name": "", "reason": "missing filename" }));
            // Still must drain the field body before advancing.
            let _ = field.bytes().await;
            continue;
        }
        if let Err(reason) = pathsafe::validate_name(&client_name) {
            rejected.push(json!({ "name": client_name, "reason": reason }));
            let _ = field.bytes().await;
            continue;
        }
        let bytes = match field.bytes().await {
            Ok(b) => b,
            Err(e) => {
                rejected.push(json!({ "name": client_name, "reason": format!("read failed: {e}") }));
                continue;
            }
        };
        // Pick a non-overwriting final name against the live directory.
        let final_name = features::collision_free_name(&client_name, |cand| {
            dir_canon.join(cand).exists()
        });
        let dest = dir_canon.join(&final_name);
        match tokio::fs::write(&dest, &bytes).await {
            Ok(()) => {
                log_both(&format!(
                    "[files] upload saved {} ({} bytes)",
                    dest.to_string_lossy(),
                    bytes.len()
                ));
                saved.push(final_name);
            }
            Err(e) => {
                rejected.push(json!({ "name": client_name, "reason": format!("write failed: {e}") }));
            }
        }
    }

    Json(json!({ "saved": saved, "rejected": rejected })).into_response()
}

// --- GET /api/gitchanges?path=<abs> ---

pub async fn gitchanges(State(state): State<AppState>, Query(q): Query<PathQuery>) -> Response {
    let (canon, _roots) = match resolve_existing(&state, &q.path) {
        Ok(v) => v,
        Err(_) => return Json(json!({ "files": [], "truncated": false })).into_response(),
    };
    let (files, truncated) = git_changes(&canon).await;
    log_both(&format!(
        "[files] gitchanges {} -> {} file(s) truncated={truncated}",
        canon.to_string_lossy(),
        files.len()
    ));
    Json(json!({ "files": files, "truncated": truncated })).into_response()
}

/// `git -C <path> status --porcelain` with a 5s timeout; parses each line into
/// `{status, path}`, capped at 200. On any failure returns an empty list.
async fn git_changes(path: &Path) -> (Vec<serde_json::Value>, bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const MAX_LINES: usize = 200;
    let path = path.to_path_buf();
    let handle = tokio::task::spawn_blocking(move || {
        let mut child = match std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["status", "--porcelain"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return (Vec::new(), false),
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return (Vec::new(), false);
                    }
                    use std::io::Read;
                    let mut out = String::new();
                    if let Some(mut so) = child.stdout.take() {
                        let _ = so.read_to_string(&mut out);
                    }
                    let mut files = Vec::new();
                    let mut truncated = false;
                    for line in out.lines() {
                        if files.len() >= MAX_LINES {
                            truncated = true;
                            break;
                        }
                        if let Some(c) = features::parse_porcelain_line(line) {
                            files.push(json!({ "status": c.status, "path": c.path }));
                        }
                    }
                    return (files, truncated);
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return (Vec::new(), false);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => return (Vec::new(), false),
            }
        }
    });
    handle.await.unwrap_or((Vec::new(), false))
}

// --- GET /api/session-peek?id=<session id> ---

#[derive(Deserialize)]
pub struct SessionPeekQuery {
    id: String,
}

pub async fn session_peek(
    State(state): State<AppState>,
    Query(q): Query<SessionPeekQuery>,
) -> Response {
    // Find the session in the store to learn its cwd + start time.
    let session = state
        .sessions
        .snapshot_and_prune()
        .into_iter()
        .find(|s| s.id == q.id);
    let Some(session) = session else {
        return Json(json!({ "lines": [], "file": serde_json::Value::Null })).into_response();
    };

    let cwd = session.path.clone();
    let started = session.started_at_ms;
    let result = tokio::task::spawn_blocking(move || peek_transcript(&cwd, started)).await;
    match result {
        Ok((lines, file)) => {
            log_both(&format!(
                "[files] session-peek id={} -> {} line(s) file={:?}",
                q.id,
                lines.len(),
                file
            ));
            Json(json!({ "lines": lines, "file": file })).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task join error: {e}")),
    }
}

/// Convert a `SystemTime` to epoch milliseconds, clamping to 0 for any
/// pre-epoch time (shouldn't happen on Windows, but keeps this infallible).
fn to_epoch_ms(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Best-effort transcript peek: derive the slug from `cwd`, pick the `.jsonl`
/// under `%USERPROFILE%\.claude\projects\<slug>` that belongs to this session
/// (see `features::select_peek_file` -- prefers creation time so an unrelated,
/// still-active session in the same folder isn't mistaken for this one), read
/// its last 40 lines, and extract a compact `{role,text}` per message line.
/// Returns (lines, file basename|null).
fn peek_transcript(cwd: &str, started_ms: u64) -> (Vec<serde_json::Value>, serde_json::Value) {
    let none = (Vec::new(), serde_json::Value::Null);
    let profile = match std::env::var("USERPROFILE") {
        Ok(p) => p,
        Err(_) => return none,
    };
    let slug = features::project_slug(cwd);
    let dir = PathBuf::from(profile)
        .join(".claude")
        .join("projects")
        .join(&slug);

    let read = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(_) => return none,
    };

    let mut candidates = Vec::new();
    for de in read.flatten() {
        let p = de.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = de.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        // `created()` is reliably populated on Windows (real file birth
        // time); fall back to `modified` if it's ever unavailable so the
        // candidate still participates in the mtime fallback path.
        let created = meta.created().unwrap_or(modified);
        let Some(name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        candidates.push(features::PeekCandidate {
            name,
            created_ms: to_epoch_ms(created),
            modified_ms: to_epoch_ms(modified),
        });
    }

    let Some(name) = features::select_peek_file(&candidates, started_ms) else {
        return none;
    };
    let file = dir.join(&name);
    let content = match std::fs::read_to_string(&file) {
        Ok(c) => c,
        Err(_) => return none,
    };
    let all: Vec<&str> = content.lines().collect();
    let tail = if all.len() > 40 { &all[all.len() - 40..] } else { &all[..] };

    let mut lines = Vec::new();
    for raw in tail {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        if let Some(pl) = features::peek_line_from_json(&v) {
            lines.push(json!({ "role": pl.role, "text": pl.text }));
        }
    }

    let basename = file
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    (lines, basename)
}

// --- GET /api/zip?path=<abs dir> ---

pub async fn zip(State(state): State<AppState>, Query(q): Query<PathQuery>) -> Response {
    let (canon, _roots) = match resolve_existing(&state, &q.path) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::FORBIDDEN, &e),
    };
    if !canon.is_dir() {
        return err(StatusCode::BAD_REQUEST, "zip target must be a directory");
    }
    let dir_name = canon
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_string());

    let dir_for_zip = canon.clone();
    let build = tokio::task::spawn_blocking(move || build_zip(&dir_for_zip)).await;
    let zip_path = match build {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, &e),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task join error: {e}")),
    };

    let meta = match tokio::fs::metadata(&zip_path).await {
        Ok(m) => m,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("zip missing: {e}")),
    };
    let download_name = format!("{dir_name}.zip");
    let resp = stream_file_response(&zip_path, &download_name, "attachment", meta.len()).await;

    // Best-effort cleanup: schedule deletion of THIS temp zip once the response
    // has been sent. We can't hook "after response" cheaply, so delete on a
    // short delay; the file has already been opened for streaming.
    let to_delete = zip_path.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(120)).await;
        let _ = tokio::fs::remove_file(&to_delete).await;
    });

    resp
}

/// Zip `dir/*` into `%TEMP%\iphone-bridge-zip-<id>.zip` via PowerShell's
/// `Compress-Archive`, using the shared `-EncodedCommand` runner. Also sweeps
/// stale `iphone-bridge-zip-*` files older than 1h first. An empty dir still
/// produces a valid (empty) zip. Returns the created zip path.
fn build_zip(dir: &Path) -> Result<PathBuf, String> {
    let temp = std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .unwrap_or_else(|_| r"C:\Windows\Temp".to_string());
    sweep_stale_zips(&temp);

    let id = format!(
        "{:016x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0) as u64
    );
    let zip_path = PathBuf::from(&temp).join(format!("iphone-bridge-zip-{id}.zip"));

    // Pass the source dir and destination as base64 (UTF-16LE) so nothing is
    // string-interpolated raw into the script.
    let src_b64 = pshell::b64_utf16(&dir.to_string_lossy());
    let dst_b64 = pshell::b64_utf16(&zip_path.to_string_lossy());
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$src = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{src_b64}'))
$dst = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{dst_b64}'))
if (Test-Path $dst) {{ Remove-Item -LiteralPath $dst -Force }}
$items = Get-ChildItem -LiteralPath $src -Force
if ($items.Count -eq 0) {{
    # Compress-Archive refuses an empty -Path; hand it an empty file set by
    # creating a valid empty zip via .NET instead.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $fs = [System.IO.Compression.ZipFile]::Open($dst, 'Create')
    $fs.Dispose()
}} else {{
    Compress-Archive -Path (Join-Path $src '*') -DestinationPath $dst -Force
}}
Write-Output 'ok'
"#
    );

    pshell::run_powershell("zip", &script, Duration::from_secs(120))?;
    if !zip_path.exists() {
        return Err("zip creation reported success but file is missing".to_string());
    }
    log_both(&format!("[files] zip built {}", zip_path.to_string_lossy()));
    Ok(zip_path)
}

/// Delete leftover `iphone-bridge-zip-*` temp files older than 1 hour.
fn sweep_stale_zips(temp: &str) {
    let Ok(read) = std::fs::read_dir(temp) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for de in read.flatten() {
        let name = de.file_name().to_string_lossy().into_owned();
        if !name.starts_with("iphone-bridge-zip-") {
            continue;
        }
        if let Ok(meta) = de.metadata() {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = now.duration_since(modified) {
                    if age > Duration::from_secs(3600) {
                        let _ = std::fs::remove_file(de.path());
                    }
                }
            }
        }
    }
}

// --- shared resolution helpers ---

/// Resolve `raw` to a canonical, allowed, existing path for the current scope,
/// returning it alongside the canonicalized effective roots (so callers can
/// reuse the roots for root/parent checks).
fn resolve_existing(
    state: &AppState,
    raw: &str,
) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let effective = {
        let cfg = state.config.read().unwrap();
        cfg.effective_root_paths()
    };
    let canon_roots = pathsafe::canonicalize_roots(&effective);
    let candidate = PathBuf::from(raw);
    let canon = pathsafe::resolve_allowed(&candidate, &effective)?;
    Ok((canon, canon_roots))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"123456", b"123456"));
        assert!(!constant_time_eq(b"123456", b"123457"));
        assert!(!constant_time_eq(b"123456", b"12345"));
        assert!(!constant_time_eq(b"", b"1"));
        assert!(constant_time_eq(b"", b""));
    }
}
