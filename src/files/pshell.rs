//! Shared PowerShell subprocess runner, generalized out of `trash.rs`.
//!
//! PowerShell scripts are handed in via `-EncodedCommand` (base64 UTF-16LE):
//! piping a multi-line script to `-Command -` on stdin silently executes
//! nothing (verified live), and `-EncodedCommand` also sidesteps every quoting
//! hazard. Every subprocess runs CREATE_NO_WINDOW with a caller-chosen timeout,
//! killed on expiry.

use std::os::windows::process::CommandExt;
use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use crate::logging::log_both;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Base64-encode (UTF-16LE, matching `[Text.Encoding]::Unicode`) a string for
/// safe embedding into a PowerShell script or the `-EncodedCommand` argument.
pub fn b64_utf16(s: &str) -> String {
    let wide: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(wide)
}

/// Run a PowerShell `script` with `timeout` (killed on expiry), CREATE_NO_WINDOW,
/// capturing stdout as a raw string. `tag` is a short log label (e.g. "trash",
/// "zip"). Returns trimmed stdout on success, or an error string (also logged).
pub fn run_powershell(tag: &str, script: &str, timeout: Duration) -> Result<String, String> {
    let encoded = b64_utf16(script);
    let mut child = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            let msg = format!("failed to spawn powershell.exe: {e}");
            log_both(&format!("[{tag}] {msg}"));
            msg
        })?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    log_both(&format!(
                        "[{tag}] powershell.exe timed out after {}s; killed",
                        timeout.as_secs()
                    ));
                    return Err("powershell operation timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                log_both(&format!("[{tag}] error waiting on powershell.exe: {e}"));
                return Err(format!("failed waiting on powershell.exe: {e}"));
            }
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to collect powershell output: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log_both(&format!(
            "[{tag}] powershell.exe exited with {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
        return Err(format!("powershell operation failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Like `run_powershell` but parses the stdout as JSON. Empty output -> `Null`
/// (never an error, matching the recycle-bin "no items" case).
pub fn run_powershell_json(tag: &str, script: &str, timeout: Duration) -> Result<Value, String> {
    let trimmed = run_powershell(tag, script, timeout)?;
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&trimmed).map_err(|e| {
        log_both(&format!(
            "[{tag}] failed to parse powershell JSON output: {e}; raw: {trimmed}"
        ));
        format!("failed to parse powershell data: {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_utf16_round_trips_via_manual_decode() {
        let s = r"C:\$Recycle.Bin\S-1-5-21\$RABC123.txt";
        let encoded = b64_utf16(s);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .unwrap();
        let wide: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let decoded = String::from_utf16(&wide).unwrap();
        assert_eq!(decoded, s);
    }
}
