#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod dictate;
mod files;
mod keyboard;
mod logging;
mod net;
mod state;
mod tls;
mod tray;
mod voice;

use anyhow::Result;
use bytes::Bytes;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

use crate::audio::render::MicMsg;

const HTTPS_PORT: u16 = 8443;

/// Renew (or provision) a Tailscale cert once it's expiring within this many days.
const RENEWAL_THRESHOLD_DAYS: i64 = 14;

/// How the TLS cert in use at startup was chosen -- logged so bridge.log can
/// always answer "why am I on self-signed?" after the fact.
enum CertSource {
    /// Tailscale is up and the matching on-disk LE cert is valid.
    TailscaleLive { name: String },
    /// Tailscale isn't reachable/known yet, but a still-valid LE cert for a
    /// previously-seen Tailscale name was found on disk.
    OnDiskFallback { name: String },
    /// No usable Tailscale cert; using the generated self-signed cert.
    SelfSigned,
}

use crate::logging::log_both;

/// Resolve which cert/key pair to boot with, and why. This is the fix for
/// the July 1 outage: previously, if `tailscale_dns_name()` failed (e.g. the
/// exe autostarted before Tailscale was up), we fell straight to a fresh
/// self-signed cert even when a valid Let's Encrypt cert already sat on disk.
fn choose_initial_cert(cert_dir: &std::path::Path) -> Result<(PathBuf, PathBuf, CertSource)> {
    let ts_name = tls::tailscale_dns_name();

    if let Some(name) = ts_name.as_deref() {
        let crt = cert_dir.join(format!("{name}.crt"));
        let key = cert_dir.join(format!("{name}.key"));
        if crt.exists() && key.exists() {
            return Ok((crt, key, CertSource::TailscaleLive { name: name.to_string() }));
        }
        log_both(&format!(
            "[tls] Tailscale reports {name} but no cert at {} yet",
            crt.display()
        ));
        // Tailscale knows who we are but hasn't issued/cached a cert for that
        // name -- try to provision one right now instead of falling back.
        if tls::provision_or_renew_cert(name, &crt, &key) {
            return Ok((crt, key, CertSource::TailscaleLive { name: name.to_string() }));
        }
    } else {
        log_both("[tls] Tailscale status unavailable at startup (not up yet, or not installed)");
    }

    // Tailscale detection failed (or provisioning failed) -- scan disk for a
    // still-valid cert from a previous run before giving up to self-signed.
    if let Some((name, crt, key)) = tls::find_named_cert_pair(cert_dir) {
        log_both(&format!(
            "[tls] using on-disk Tailscale cert for {name} found by directory scan (Tailscale detection failed this run)"
        ));
        return Ok((crt, key, CertSource::OnDiskFallback { name }));
    }

    log_both("[tls] no valid Tailscale cert available (live or on-disk); falling back to self-signed");
    let p = tls::ensure_cert()?;
    Ok((p.cert, p.key, CertSource::SelfSigned))
}

/// Background task: while we're not yet on a live Tailscale cert, keep
/// re-checking Tailscale status and hot-swap the TLS config in place the
/// moment a real cert becomes available -- no restart needed. Also keeps an
/// already-live Tailscale cert renewed as it approaches expiry.
fn spawn_cert_watcher(
    tls_config: axum_server::tls_rustls::RustlsConfig,
    cert_dir: PathBuf,
    initial_name: Option<String>,
) {
    tokio::spawn(async move {
        let mut live_name = initial_name;
        // Fast retries for the first 10 minutes (covers the "autostarted
        // before Tailscale" case), then back off to a slow poll forever
        // (covers Tailscale being reinstalled/reconfigured, or LE renewal).
        let fast_interval = Duration::from_secs(30);
        let slow_interval = Duration::from_secs(600);
        let fast_phase_end = tokio::time::Instant::now() + Duration::from_secs(600);

        loop {
            let interval = if tokio::time::Instant::now() < fast_phase_end {
                fast_interval
            } else {
                slow_interval
            };
            tokio::time::sleep(interval).await;

            let ts_name = tls::tailscale_dns_name();
            let Some(name) = ts_name else {
                if live_name.is_none() {
                    log_both("[tls] background check: Tailscale still not reachable");
                }
                continue;
            };

            let crt = cert_dir.join(format!("{name}.crt"));
            let key = cert_dir.join(format!("{name}.key"));

            let needs_provision = !crt.exists() || !key.exists() || tls::needs_renewal(&crt, RENEWAL_THRESHOLD_DAYS);
            if needs_provision {
                log_both(&format!(
                    "[tls] background check: cert for {name} missing or expiring soon, requesting from tailscale"
                ));
                if !tls::provision_or_renew_cert(&name, &crt, &key) {
                    continue; // will retry next tick
                }
            }

            if live_name.as_deref() == Some(name.as_str()) && !needs_provision {
                continue; // already on this cert and it's healthy, nothing to do
            }

            match tls_config.reload_from_pem_file(&crt, &key).await {
                Ok(()) => {
                    log_both(&format!(
                        "[tls] hot-reloaded TLS config with Tailscale cert for {name} (no restart)"
                    ));
                    live_name = Some(name);
                }
                Err(e) => {
                    log_both(&format!("[tls] failed to hot-reload cert for {name}: {e}"));
                }
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    let (audio_tx, _) = broadcast::channel::<Bytes>(64);
    let (mic_tx, mic_rx) = mpsc::channel::<MicMsg>(128);

    let initial_device = std::env::var("BRIDGE_DEVICE").ok();
    let capture_ctl = Arc::new(state::CaptureCtl::new(initial_device));

    // Files feature: load (or first-run generate) config, and create the live
    // session store. Both are shared with the tray and the HTTP server.
    let files_config = Arc::new(RwLock::new(files::config::load_or_init()));
    let sessions = Arc::new(files::sessions::SessionStore::load_or_default());
    {
        let cfg = files_config.read().unwrap();
        log_both(&format!(
            "[files] ready: PIN set, scope={:?}, {} effective root(s)",
            cfg.scope,
            cfg.effective_named_roots().len()
        ));
    }

    // Audio-out capture: WASAPI loopback -> broadcast channel -> WS subscribers.
    let tx_for_capture = audio_tx.clone();
    let ctl_for_capture = capture_ctl.clone();
    std::thread::spawn(move || {
        if let Err(e) = audio::capture::run_capture(tx_for_capture, ctl_for_capture) {
            log_both(&format!("[capture] fatal: {e}"));
        }
    });

    // Mic-in render: mpsc channel -> WASAPI render -> VB-CABLE Input device.
    std::thread::spawn(move || {
        if let Err(e) = audio::render::run_render(mic_rx) {
            log_both(&format!("[render] fatal: {e}"));
        }
    });

    let cert_dir = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
        .join("iphone-bridge");

    let (cert_path, key_path, source) = choose_initial_cert(&cert_dir)?;

    let (browser_host, live_name) = match &source {
        CertSource::TailscaleLive { name } => {
            log_both(&format!("[tls] using live Tailscale Let's Encrypt cert for {name}"));
            (name.clone(), Some(name.clone()))
        }
        CertSource::OnDiskFallback { name } => (name.clone(), Some(name.clone())),
        CertSource::SelfSigned => {
            let fallback = net::server::lan_ip().unwrap_or_else(|| "127.0.0.1".to_string());
            (fallback, None)
        }
    };
    let browser_url = format!("https://{browser_host}:{HTTPS_PORT}");

    println!("iphone-bridge v1 (mic enabled)");
    println!();
    println!("Open on iPhone (Safari):");
    println!("    {browser_url}");
    println!();

    // Tray icon (Win32 message loop on its own thread).
    let ctl_for_tray = capture_ctl.clone();
    let url_for_tray = browser_url.clone();
    let config_for_tray = files_config.clone();
    std::thread::spawn(move || {
        if let Err(e) = tray::run_tray(ctl_for_tray, url_for_tray, config_for_tray) {
            log_both(&format!("[tray] fatal: {e}"));
        }
    });

    let tls_config = net::server::load_tls_config(&cert_path, &key_path).await?;

    // If we didn't boot straight onto a live Tailscale cert, keep trying in
    // the background and hot-swap in place once one becomes available.
    if !matches!(source, CertSource::TailscaleLive { .. }) {
        spawn_cert_watcher(tls_config.clone(), cert_dir, live_name);
    }

    let result = net::server::serve_tls(
        audio_tx,
        mic_tx,
        files_config,
        sessions,
        HTTPS_PORT,
        tls_config,
    )
    .await;
    if let Err(e) = &result {
        log_both(&format!("[server] fatal: {e}"));
    }
    result
}
