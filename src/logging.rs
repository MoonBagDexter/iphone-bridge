//! Minimal file logger + panic capture, kept dependency-light (std only).
//!
//! The release build runs with `windows_subsystem = "windows"`, so stdout and
//! stderr have nowhere to go. Every `eprintln!`/`println!` call site is being
//! left as-is (matches existing style); this module gives them a second home
//! by mirroring lines into `%LOCALAPPDATA%\iphone-bridge\bridge.log`, and
//! installs a panic hook so a crash leaves a message behind instead of
//! silently exiting (see the May 13 bridge.log: exit 0xffffffff, nothing
//! logged).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn log_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
    PathBuf::from(base).join("iphone-bridge").join("bridge.log")
}

/// Open (truncating if oversized) the log file and install the panic hook.
/// Call this once at the very start of `main`.
pub fn init() {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    // Rotate by truncation if the existing log has grown too large; a single
    // rolled-over backup is enough for a hobby bridge app, not worth more.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::remove_file(&path);
        }
    }

    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    let _ = LOG_FILE.set(Mutex::new(file));

    log_line("[log] bridge.log opened for this run");
    install_panic_hook();
}

/// Log to both stderr and the log file. The release build has nowhere for
/// stderr to go, but keeping the `eprintln!` matches existing style and helps
/// during `cargo run` / debug builds.
pub fn log_both(msg: &str) {
    eprintln!("{msg}");
    log_line(msg);
}

/// Append a line to the log file (best-effort; never panics itself).
pub fn log_line(msg: &str) {
    if let Some(mutex) = LOG_FILE.get() {
        if let Ok(mut guard) = mutex.lock() {
            if let Some(file) = guard.as_mut() {
                let now = now_string();
                let _ = writeln!(file, "[{now}] {msg}");
                let _ = file.flush();
            }
        }
    }
}

fn now_string() -> String {
    // Avoid pulling in a datetime crate just for a log timestamp: use
    // seconds-since-epoch, which is enough to correlate against Windows
    // Event Log / other timestamps when debugging a crash.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch+{secs}")
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log_line(&format!("[panic] {info}\n{backtrace}"));
        default_hook(info);
    }));
}
