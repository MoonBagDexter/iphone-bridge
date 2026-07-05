use anyhow::{anyhow, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use time::OffsetDateTime;
use x509_parser::prelude::{FromDer, X509Certificate};

/// Where we cache the generated cert+key between runs so iPhones don't have to
/// re-trust on every restart of the bridge.
fn cert_dir() -> Result<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .map_err(|_| anyhow!("LOCALAPPDATA env var not set"))?;
    let dir = PathBuf::from(base).join("iphone-bridge");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub struct CertPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Return paths to a usable cert/key pair, generating + caching one if not
/// already present. The cert is self-signed and includes localhost, 127.0.0.1,
/// and the LAN IP (if discoverable) as Subject Alternative Names.
pub fn ensure_cert() -> Result<CertPaths> {
    let dir = cert_dir()?;
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        return Ok(CertPaths {
            cert: cert_path,
            key: key_path,
        });
    }

    generate(&cert_path, &key_path)?;
    Ok(CertPaths {
        cert: cert_path,
        key: key_path,
    })
}

/// Collect every IPv4 + IPv6 address bound to a local interface (LAN, Tailscale,
/// localhost, etc.) plus any Tailscale magic-DNS name. The resulting cert is
/// valid no matter how the iPhone reaches us.
fn all_local_sans() -> Vec<SanType> {
    let mut ips: HashSet<IpAddr> = HashSet::new();
    ips.insert(IpAddr::from([127, 0, 0, 1]));

    if let Ok(addrs) = local_ip_address::list_afinet_netifas() {
        for (_iface, ip) in addrs {
            ips.insert(ip);
        }
    }

    let mut sans: Vec<SanType> = Vec::new();
    sans.push(SanType::DnsName("localhost".try_into().unwrap()));
    for ip in ips {
        sans.push(SanType::IpAddress(ip));
    }

    // Tailscale MagicDNS name -- best-effort, ignored if Tailscale isn't installed.
    if let Some(name) = tailscale_dns_name() {
        eprintln!("[tls] including Tailscale DNS name: {name}");
        if let Ok(d) = name.as_str().try_into() {
            sans.push(SanType::DnsName(d));
        }
    }

    sans
}

pub fn tailscale_dns_name() -> Option<String> {
    let candidates = [
        "C:\\Program Files\\Tailscale\\tailscale.exe",
        "tailscale",
    ];
    for cmd in candidates {
        let Ok(output) = Command::new(cmd).args(["status", "--json"]).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Tolerant of pretty-printed JSON ("DNSName": "...") vs compact.
        let key = "\"DNSName\"";
        if let Some(start) = stdout.find(key) {
            let after_key = &stdout[start + key.len()..];
            if let Some(colon) = after_key.find(':') {
                let after_colon = &after_key[colon + 1..];
                if let Some(open_quote) = after_colon.find('"') {
                    let value_start = open_quote + 1;
                    if let Some(close_quote) = after_colon[value_start..].find('"') {
                        let mut name = after_colon[value_start..value_start + close_quote].to_string();
                        if name.ends_with('.') {
                            name.pop();
                        }
                        if !name.is_empty() {
                            return Some(name);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Parse a PEM-encoded cert file and return its `notAfter` expiry timestamp.
fn cert_expiry(cert_path: &Path) -> Result<OffsetDateTime> {
    let pem_bytes = fs::read(cert_path)?;
    let pem = pem::parse(&pem_bytes).map_err(|e| anyhow!("bad PEM at {}: {e}", cert_path.display()))?;
    let (_, cert) = X509Certificate::from_der(pem.contents())
        .map_err(|e| anyhow!("bad X.509 DER at {}: {e}", cert_path.display()))?;
    Ok(cert.validity().not_after.to_datetime())
}

/// Days remaining until `cert_path` expires (negative if already expired).
fn days_until_expiry(cert_path: &Path) -> Result<i64> {
    let not_after = cert_expiry(cert_path)?;
    let remaining = not_after - OffsetDateTime::now_utc();
    Ok(remaining.whole_days())
}

/// True if `cert_path` is expired or will expire within `threshold_days`.
pub fn needs_renewal(cert_path: &Path, threshold_days: i64) -> bool {
    match days_until_expiry(cert_path) {
        Ok(days) => days < threshold_days,
        Err(_) => true, // unparsable/missing -> treat as needing (re)provisioning
    }
}

/// Scan `dir` for a `<name>.crt` / `<name>.key` pair that is not expired,
/// ignoring the generic self-signed fallback names (`cert.pem`/`key.pem`) and
/// any `.crt` without a matching `.key` (or vice versa). Returns the DNS name
/// derived from the filename stem plus the two paths.
///
/// If multiple valid pairs exist, the one with the longest remaining
/// validity wins.
pub fn find_named_cert_pair(dir: &Path) -> Option<(String, PathBuf, PathBuf)> {
    let entries = fs::read_dir(dir).ok()?;

    let mut best: Option<(String, PathBuf, PathBuf, i64)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("crt") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        // Skip the self-signed fallback pair; that's handled separately by ensure_cert().
        if stem == "cert" {
            continue;
        }
        let key_path = dir.join(format!("{stem}.key"));
        if !key_path.exists() {
            continue;
        }
        let days = match days_until_expiry(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[tls] skipping unparsable cert {}: {e}", path.display());
                continue;
            }
        };
        if days < 0 {
            continue; // expired
        }
        if best.as_ref().map(|(_, _, _, d)| days > *d).unwrap_or(true) {
            best = Some((stem.to_string(), path, key_path, days));
        }
    }

    best.map(|(name, crt, key, _)| (name, crt, key))
}

/// Ask `tailscale cert` to (re)issue a Let's Encrypt cert for `name` into
/// `cert_path`/`key_path`. Same subprocess-candidate style as
/// `tailscale_dns_name`. Best-effort: logs and returns `false` on any
/// failure rather than erroring the whole startup, since we can always fall
/// back to the self-signed cert.
pub fn provision_or_renew_cert(name: &str, cert_path: &Path, key_path: &Path) -> bool {
    let candidates = [
        "C:\\Program Files\\Tailscale\\tailscale.exe",
        "tailscale",
    ];
    for cmd in candidates {
        let result = Command::new(cmd)
            .args([
                "cert",
                "--cert-file",
                &cert_path.to_string_lossy(),
                "--key-file",
                &key_path.to_string_lossy(),
                name,
            ])
            .output();
        match result {
            Ok(output) if output.status.success() => {
                eprintln!(
                    "[tls] tailscale cert issued/renewed for {name} at {}",
                    cert_path.display()
                );
                return true;
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("[tls] `{cmd} cert {name}` failed: {stderr}");
                // A real failure (tailscaled not running, DNS not verified, etc.)
                // isn't fixed by trying the other candidate path.
                return false;
            }
            Err(_) => continue, // this candidate isn't on disk/PATH, try the next
        }
    }
    eprintln!("[tls] could not run `tailscale cert` for {name} (tailscale not found)");
    false
}

fn generate(cert_out: &Path, key_out: &Path) -> Result<()> {
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "iphone-bridge");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "iphone-bridge local");
    params.subject_alt_names = all_local_sans();

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    fs::write(cert_out, cert_pem.as_bytes())?;
    fs::write(key_out, key_pem.as_bytes())?;

    eprintln!(
        "[tls] generated self-signed cert with {} SAN entries at {}",
        params.subject_alt_names.len(),
        cert_out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::date_time_ymd;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Write a self-signed cert/key pair named `<stem>.crt`/`<stem>.key` in
    /// `dir` with the given validity window. Returns (crt_path, key_path).
    fn write_test_cert(
        dir: &Path,
        stem: &str,
        not_before: OffsetDateTime,
        not_after: OffsetDateTime,
    ) -> (PathBuf, PathBuf) {
        let mut params = CertificateParams::default();
        params.not_before = not_before;
        params.not_after = not_after;
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "test");
        let key_pair = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        let crt_path = dir.join(format!("{stem}.crt"));
        let key_path = dir.join(format!("{stem}.key"));
        fs::write(&crt_path, cert.pem().as_bytes()).unwrap();
        fs::write(&key_path, key_pair.serialize_pem().as_bytes()).unwrap();
        (crt_path, key_path)
    }

    /// Unique scratch dir per test so parallel `cargo test` runs don't collide.
    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "iphone-bridge-tls-test-{tag}-{}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn days_until_expiry_is_positive_for_far_future_cert() {
        let dir = temp_dir("future");
        let (crt, _key) = write_test_cert(
            &dir,
            "future",
            date_time_ymd(2020, 1, 1),
            date_time_ymd(2099, 1, 1),
        );
        let days = days_until_expiry(&crt).expect("should parse");
        assert!(days > 365 * 10, "expected far-future cert, got {days} days");
    }

    #[test]
    fn days_until_expiry_is_negative_for_expired_cert() {
        let dir = temp_dir("expired");
        let (crt, _key) = write_test_cert(
            &dir,
            "expired",
            date_time_ymd(2000, 1, 1),
            date_time_ymd(2001, 1, 1),
        );
        let days = days_until_expiry(&crt).expect("should parse");
        assert!(days < 0, "expected expired cert, got {days} days");
    }

    #[test]
    fn days_until_expiry_sign_distinguishes_valid_from_expired() {
        let dir = temp_dir("validity");
        let (good, _) = write_test_cert(
            &dir,
            "good",
            date_time_ymd(2020, 1, 1),
            date_time_ymd(2099, 1, 1),
        );
        let (bad, _) = write_test_cert(
            &dir,
            "bad",
            date_time_ymd(2000, 1, 1),
            date_time_ymd(2001, 1, 1),
        );
        assert!(days_until_expiry(&good).unwrap() >= 0);
        assert!(days_until_expiry(&bad).unwrap() < 0);
    }

    #[test]
    fn needs_renewal_true_when_expired() {
        let dir = temp_dir("renew-expired");
        let (crt, _) = write_test_cert(
            &dir,
            "old",
            date_time_ymd(2000, 1, 1),
            date_time_ymd(2001, 1, 1),
        );
        assert!(needs_renewal(&crt, 14));
    }

    #[test]
    fn needs_renewal_true_when_expiring_soon() {
        let dir = temp_dir("renew-soon");
        let now = OffsetDateTime::now_utc();
        let (crt, _) = write_test_cert(&dir, "soon", now - time::Duration::days(1), now + time::Duration::days(5));
        assert!(needs_renewal(&crt, 14), "cert expiring in 5 days should need renewal at 14-day threshold");
    }

    #[test]
    fn needs_renewal_false_when_far_from_expiry() {
        let dir = temp_dir("renew-not-needed");
        let (crt, _) = write_test_cert(
            &dir,
            "fresh",
            date_time_ymd(2020, 1, 1),
            date_time_ymd(2099, 1, 1),
        );
        assert!(!needs_renewal(&crt, 14));
    }

    #[test]
    fn needs_renewal_true_when_cert_missing() {
        let dir = temp_dir("renew-missing");
        let missing = dir.join("nonexistent.crt");
        assert!(needs_renewal(&missing, 14));
    }

    #[test]
    fn find_named_cert_pair_picks_valid_named_pair() {
        let dir = temp_dir("scan-basic");
        write_test_cert(
            &dir,
            "msi.tail588b52.ts.net",
            date_time_ymd(2020, 1, 1),
            date_time_ymd(2099, 1, 1),
        );
        let (name, crt, key) = find_named_cert_pair(&dir).expect("should find the pair");
        assert_eq!(name, "msi.tail588b52.ts.net");
        assert!(crt.exists());
        assert!(key.exists());
    }

    #[test]
    fn find_named_cert_pair_ignores_expired() {
        let dir = temp_dir("scan-expired");
        write_test_cert(
            &dir,
            "expired.example.ts.net",
            date_time_ymd(2000, 1, 1),
            date_time_ymd(2001, 1, 1),
        );
        assert!(find_named_cert_pair(&dir).is_none());
    }

    #[test]
    fn find_named_cert_pair_ignores_self_signed_fallback_name() {
        let dir = temp_dir("scan-fallback");
        // The generic self-signed fallback uses cert.pem/key.pem, not
        // cert.crt/cert.key -- but guard the "cert" stem explicitly too,
        // and confirm a bare cert.pem/key.pem orphan pair is not picked up
        // since find_named_cert_pair only looks at *.crt files.
        fs::write(dir.join("cert.pem"), b"not a cert").unwrap();
        fs::write(dir.join("key.pem"), b"not a key").unwrap();
        assert!(find_named_cert_pair(&dir).is_none());
    }

    #[test]
    fn find_named_cert_pair_ignores_orphan_crt_without_key() {
        let dir = temp_dir("scan-orphan");
        let (crt, key) = write_test_cert(
            &dir,
            "orphan.example.ts.net",
            date_time_ymd(2020, 1, 1),
            date_time_ymd(2099, 1, 1),
        );
        fs::remove_file(&key).unwrap();
        assert!(find_named_cert_pair(&dir).is_none());
        assert!(crt.exists());
    }

    #[test]
    fn find_named_cert_pair_picks_longest_lived_when_multiple_valid() {
        let dir = temp_dir("scan-multi");
        write_test_cert(
            &dir,
            "soon-to-expire.ts.net",
            date_time_ymd(2020, 1, 1),
            OffsetDateTime::now_utc() + time::Duration::days(3),
        );
        write_test_cert(
            &dir,
            "long-lived.ts.net",
            date_time_ymd(2020, 1, 1),
            date_time_ymd(2099, 1, 1),
        );
        let (name, _, _) = find_named_cert_pair(&dir).expect("should find a pair");
        assert_eq!(name, "long-lived.ts.net");
    }
}
