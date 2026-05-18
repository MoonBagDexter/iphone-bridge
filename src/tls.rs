use anyhow::{anyhow, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        let output = Command::new(cmd).args(["status", "--json"]).output().ok()?;
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
