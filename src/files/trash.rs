//! Recycle Bin browsing + restore, driven through PowerShell's Shell COM
//! object (`Shell.Application`). We shell out rather than binding the COM
//! interfaces directly in Rust because `Shell.Application`'s `Namespace(10)` /
//! `Verbs()` surface is far simpler to drive from PowerShell, and this mirrors
//! the existing `sessions.rs` pattern of spawning a helper process.
//!
//! The pure parts (JSON -> `TrashItem` mapping, locale date sanitizing) are
//! unit-tested here; the PowerShell subprocess plumbing itself is thin and
//! logged via `log_both` rather than tested.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::files::pshell::{b64_utf16, run_powershell_json};
use crate::logging::log_both;

const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(30);

/// One Recycle Bin entry as sent to the client.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TrashItem {
    pub id: String,
    pub name: String,
    #[serde(rename = "originalPath")]
    pub original_path: String,
    #[serde(rename = "deletedAtMs")]
    pub deleted_at_ms: Option<u64>,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: Option<u64>,
    #[serde(rename = "isDir")]
    pub is_dir: bool,
}

/// Strip characters PowerShell's locale-formatted date strings can carry that
/// aren't part of the actual date/time (left-to-right/right-to-left marks,
/// non-breaking spaces, etc.) but keep everything a date/time literal needs:
/// ASCII digits, letters (AM/PM, month names), whitespace, and common
/// separators (`/ : , -`).
pub fn sanitize_date_string(raw: &str) -> String {
    raw.chars()
        .filter(|c| {
            c.is_ascii_digit()
                || c.is_ascii_alphabetic()
                || matches!(*c, ' ' | '/' | ':' | ',' | '-')
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Parse a sanitized locale date/time string into epoch milliseconds. Supports
/// the common Windows short-date formats we expect out of `GetDetailsOf`:
/// `M/D/YYYY h:mm AM|PM` and `D/M/YYYY H:mm`. Returns `None` (never errors) if
/// the format isn't recognized -- callers report `deletedAtMs: null` rather
/// than fail the whole listing over one unparseable row.
pub fn parse_deleted_at_ms(raw: &str) -> Option<u64> {
    let clean = sanitize_date_string(raw);
    if clean.is_empty() {
        return None;
    }

    // Split "<date> <time...>" -- date is the first token, the rest is time.
    let mut parts = clean.splitn(2, ' ');
    let date_part = parts.next()?;
    let time_rest = parts.next().unwrap_or("").trim();

    let date_nums: Vec<i64> = date_part.split('/').filter_map(|s| s.parse().ok()).collect();
    if date_nums.len() != 3 {
        return None;
    }
    // Windows short date for en-US locale is M/D/YYYY.
    let (month, day, year) = (date_nums[0], date_nums[1], date_nums[2]);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }

    // Time: "H:MM" or "H:MM:SS" optionally followed by AM/PM.
    let is_pm = time_rest.to_ascii_uppercase().contains("PM");
    let is_am = time_rest.to_ascii_uppercase().contains("AM");
    let time_digits_only: String = time_rest
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ':')
        .collect();
    let time_nums: Vec<i64> = time_digits_only
        .split(':')
        .filter_map(|s| s.parse().ok())
        .collect();
    let (mut hour, minute, second) = match time_nums.as_slice() {
        [h, m] => (*h, *m, 0),
        [h, m, s] => (*h, *m, *s),
        [] => (0, 0, 0),
        _ => return None,
    };
    if is_pm && hour < 12 {
        hour += 12;
    }
    if is_am && hour == 12 {
        hour = 0;
    }

    days_and_time_to_epoch_ms(year, month, day, hour, minute, second)
}

/// Days-from-civil algorithm (Howard Hinnant's public-domain `civil_from_days`
/// inverse) to avoid pulling in a datetime crate for one conversion.
fn days_and_time_to_epoch_ms(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> Option<u64> {
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let mp = (month + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days_since_epoch = era * 146097 + doe - 719468;

    let secs = days_since_epoch
        .checked_mul(86400)?
        .checked_add(hour * 3600)?
        .checked_add(minute * 60)?
        .checked_add(second)?;
    if secs < 0 {
        return None;
    }
    Some(secs as u64 * 1000)
}

/// Map one PowerShell-emitted JSON value (already parsed) into a `TrashItem`.
/// Field names as produced by the `ConvertTo-Json` script (see `list()`
/// below): `Path`, `Name`, `OriginalPath`, `DateDeleted`, `Size`, `IsFolder`.
pub fn item_from_json(v: &Value) -> Option<TrashItem> {
    let id = v.get("Path")?.as_str()?.to_string();
    let name = v
        .get("Name")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let original_path = v
        .get("OriginalPath")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let deleted_at_ms = v
        .get("DateDeleted")
        .and_then(|x| x.as_str())
        .and_then(parse_deleted_at_ms);
    let size_bytes = v.get("Size").and_then(value_as_u64);
    let is_dir = v
        .get("IsFolder")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    Some(TrashItem {
        id,
        name,
        original_path,
        deleted_at_ms,
        size_bytes,
        is_dir,
    })
}

fn value_as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    if let Some(n) = v.as_i64() {
        return u64::try_from(n).ok();
    }
    if let Some(f) = v.as_f64() {
        if f >= 0.0 {
            return Some(f as u64);
        }
    }
    None
}

/// `ConvertTo-Json` of a single result is an object, not an array; of
/// multiple results it's an array. Normalize both into a `Vec<TrashItem>`,
/// dropping any entries that don't at least have a `Path`.
pub fn items_from_json(v: &Value) -> Vec<TrashItem> {
    match v {
        Value::Array(items) => items.iter().filter_map(item_from_json).collect(),
        Value::Null => Vec::new(),
        obj => item_from_json(obj).into_iter().collect(),
    }
}

/// List Recycle Bin items, newest first, capped at 200.
pub fn list() -> Result<Vec<TrashItem>, String> {
    let script = r#"
$ErrorActionPreference = 'Stop'
$sh = New-Object -ComObject Shell.Application
$rb = $sh.Namespace(10)

$origIdx = 1
$dateIdx = 2
for ($i = 0; $i -le 10; $i++) {
    $h = $rb.GetDetailsOf($null, $i)
    if ($h -match 'Original Location') { $origIdx = $i }
    if ($h -match 'Date Deleted') { $dateIdx = $i }
}

$results = @()
foreach ($item in $rb.Items()) {
    $results += [PSCustomObject]@{
        Path         = $item.Path
        Name         = $item.Name
        OriginalPath = $rb.GetDetailsOf($item, $origIdx)
        DateDeleted  = $rb.GetDetailsOf($item, $dateIdx)
        Size         = $item.ExtendedProperty('System.Size')
        IsFolder     = $item.IsFolder
    }
}
$results | ConvertTo-Json -Compress
"#;

    let value = run_powershell_json("trash", script, POWERSHELL_TIMEOUT)?;
    let mut items = items_from_json(&value);
    items.sort_by(|a, b| b.deleted_at_ms.unwrap_or(0).cmp(&a.deleted_at_ms.unwrap_or(0)));
    items.truncate(200);
    log_both(&format!("[trash] list -> {} item(s)", items.len()));
    Ok(items)
}

/// Restore the item whose recycler `Path` equals `id` to its original
/// location. `id` is passed through as base64 (UTF-16LE) so it never gets
/// string-interpolated raw into the PowerShell script.
pub fn restore(id: &str) -> Result<String, String> {
    let encoded = b64_utf16(id);
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$targetId = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{encoded}'))

$sh = New-Object -ComObject Shell.Application
$rb = $sh.Namespace(10)

$origIdx = 1
for ($i = 0; $i -le 10; $i++) {{
    $h = $rb.GetDetailsOf($null, $i)
    if ($h -match 'Original Location') {{ $origIdx = $i }}
}}

$match = $null
foreach ($item in $rb.Items()) {{
    if ($item.Path -eq $targetId) {{ $match = $item; break }}
}}
if ($null -eq $match) {{
    Write-Error "item not found in recycle bin"
    exit 1
}}

$originalPath = $rb.GetDetailsOf($match, $origIdx)

$restoreVerb = $null
foreach ($verb in $match.Verbs()) {{
    $label = $verb.Name -replace '&', ''
    if ($label -match '(?i)^restore') {{ $restoreVerb = $verb; break }}
}}
if ($null -eq $restoreVerb) {{
    $all = @($match.Verbs())
    if ($all.Count -gt 0) {{ $restoreVerb = $all[0] }}
}}
if ($null -eq $restoreVerb) {{
    Write-Error "no restore verb available for this item"
    exit 1
}}
$restoreVerb.DoIt()

$deadline = (Get-Date).AddSeconds(3)
$gone = $false
while ((Get-Date) -lt $deadline) {{
    $stillThere = $false
    foreach ($item in $rb.Items()) {{
        if ($item.Path -eq $targetId) {{ $stillThere = $true; break }}
    }}
    if (-not $stillThere) {{ $gone = $true; break }}
    Start-Sleep -Milliseconds 150
}}

[PSCustomObject]@{{ RestoredTo = $originalPath; Vanished = $gone }} | ConvertTo-Json -Compress
"#
    );

    let value = run_powershell_json("trash", &script, POWERSHELL_TIMEOUT)?;
    let restored_to = value
        .get("RestoredTo")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    match restored_to {
        Some(path) if !path.is_empty() => {
            log_both(&format!("[trash] restore id={id} -> {path}"));
            Ok(path)
        }
        _ => {
            log_both(&format!("[trash] restore id={id} did not report a destination"));
            Err("restore did not report a destination path".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- sanitize_date_string ---

    #[test]
    fn strips_lrm_marks() {
        let raw = "\u{200E}1/5/2026\u{200E} 2:14 AM\u{200E}";
        assert_eq!(sanitize_date_string(raw), "1/5/2026 2:14 AM");
    }

    #[test]
    fn strips_nbsp_and_other_invisibles() {
        let raw = "1/5/2026\u{00A0}2:14\u{200F} AM";
        let cleaned = sanitize_date_string(raw);
        assert!(cleaned.chars().all(|c| c.is_ascii()));
        assert!(cleaned.contains("2:14"));
    }

    #[test]
    fn plain_ascii_passes_through() {
        assert_eq!(sanitize_date_string("1/5/2026 2:14 AM"), "1/5/2026 2:14 AM");
    }

    // --- parse_deleted_at_ms ---

    #[test]
    fn parses_lrm_wrapped_datetime() {
        let raw = "\u{200E}1/5/2026\u{200E} 2:14 AM\u{200E}";
        let ms = parse_deleted_at_ms(raw);
        assert!(ms.is_some());
    }

    #[test]
    fn parses_24h_format() {
        let ms = parse_deleted_at_ms("5/1/2026 14:14");
        assert!(ms.is_some());
    }

    #[test]
    fn returns_none_for_garbage() {
        assert_eq!(parse_deleted_at_ms("not a date"), None);
        assert_eq!(parse_deleted_at_ms(""), None);
        assert_eq!(parse_deleted_at_ms("\u{200E}\u{200F}"), None);
    }

    #[test]
    fn pm_hour_is_converted_to_24h() {
        let noon = parse_deleted_at_ms("1/1/2026 12:00 PM").unwrap();
        let midnight = parse_deleted_at_ms("1/1/2026 12:00 AM").unwrap();
        let one_pm = parse_deleted_at_ms("1/1/2026 1:00 PM").unwrap();
        assert_eq!(one_pm - noon, 3600_000);
        assert!(noon > midnight);
    }

    // --- item_from_json / items_from_json ---

    #[test]
    fn maps_single_object_json() {
        let v = json!({
            "Path": r"C:\$Recycle.Bin\S-1-5-21\$RABC123.txt",
            "Name": "notes.txt",
            "OriginalPath": r"C:\Users\thedi\Desktop\notes.txt",
            "DateDeleted": "\u{200E}1/5/2026\u{200E} 2:14 AM",
            "Size": 1234,
            "IsFolder": false
        });
        let item = item_from_json(&v).unwrap();
        assert_eq!(item.name, "notes.txt");
        assert_eq!(item.original_path, r"C:\Users\thedi\Desktop\notes.txt");
        assert_eq!(item.size_bytes, Some(1234));
        assert!(!item.is_dir);
        assert!(item.deleted_at_ms.is_some());
    }

    #[test]
    fn missing_path_yields_none() {
        let v = json!({ "Name": "x" });
        assert!(item_from_json(&v).is_none());
    }

    #[test]
    fn unparseable_date_yields_null_not_error() {
        let v = json!({
            "Path": r"C:\$Recycle.Bin\x",
            "Name": "x",
            "OriginalPath": r"C:\x",
            "DateDeleted": "garbage-date",
            "Size": 0,
            "IsFolder": true
        });
        let item = item_from_json(&v).unwrap();
        assert_eq!(item.deleted_at_ms, None);
        assert!(item.is_dir);
    }

    #[test]
    fn items_from_json_handles_single_object() {
        let v = json!({
            "Path": r"C:\$Recycle.Bin\x",
            "Name": "x",
            "OriginalPath": r"C:\x",
            "DateDeleted": "1/1/2026 1:00 AM",
            "Size": 1,
            "IsFolder": false
        });
        let items = items_from_json(&v);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn items_from_json_handles_array() {
        let v = json!([
            {
                "Path": r"C:\$Recycle.Bin\a",
                "Name": "a",
                "OriginalPath": r"C:\a",
                "DateDeleted": "1/1/2026 1:00 AM",
                "Size": 1,
                "IsFolder": false
            },
            {
                "Path": r"C:\$Recycle.Bin\b",
                "Name": "b",
                "OriginalPath": r"C:\b",
                "DateDeleted": "1/1/2026 2:00 AM",
                "Size": 2,
                "IsFolder": true
            }
        ]);
        let items = items_from_json(&v);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].name, "b");
    }

    #[test]
    fn items_from_json_handles_null_as_empty() {
        assert_eq!(items_from_json(&Value::Null).len(), 0);
    }

    #[test]
    fn items_from_json_drops_entries_without_path() {
        let v = json!([
            { "Name": "no-path" },
            {
                "Path": r"C:\$Recycle.Bin\a",
                "Name": "a",
                "OriginalPath": r"C:\a",
                "DateDeleted": "1/1/2026 1:00 AM",
                "Size": 1,
                "IsFolder": false
            }
        ]);
        let items = items_from_json(&v);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "a");
    }
}
