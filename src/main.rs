#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod keyboard;
mod net;
mod state;
mod tls;
mod tray;

use anyhow::Result;
use bytes::Bytes;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use crate::audio::render::MicMsg;

const HTTPS_PORT: u16 = 8443;

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls crypto provider"))?;

    let (audio_tx, _) = broadcast::channel::<Bytes>(64);
    let (mic_tx, mic_rx) = mpsc::channel::<MicMsg>(128);

    let initial_device = std::env::var("BRIDGE_DEVICE").ok();
    let capture_ctl = Arc::new(state::CaptureCtl::new(initial_device));

    // Audio-out capture: WASAPI loopback -> broadcast channel -> WS subscribers.
    let tx_for_capture = audio_tx.clone();
    let ctl_for_capture = capture_ctl.clone();
    std::thread::spawn(move || {
        if let Err(e) = audio::capture::run_capture(tx_for_capture, ctl_for_capture) {
            eprintln!("[capture] fatal: {e}");
        }
    });

    // Mic-in render: mpsc channel -> WASAPI render -> VB-CABLE Input device.
    std::thread::spawn(move || {
        if let Err(e) = audio::render::run_render(mic_rx) {
            eprintln!("[render] fatal: {e}");
        }
    });

    let cert_dir = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
        .join("iphone-bridge");
    let ts_name = tls::tailscale_dns_name();

    let (cert_path, key_path, browser_url) = if let Some(name) = ts_name.as_deref() {
        let crt = cert_dir.join(format!("{name}.crt"));
        let key = cert_dir.join(format!("{name}.key"));
        if crt.exists() && key.exists() {
            eprintln!("[tls] using Tailscale-issued real Let's Encrypt cert for {name}");
            (crt, key, format!("https://{name}:{HTTPS_PORT}"))
        } else {
            eprintln!(
                "[tls] Tailscale cert not found at {}; falling back to self-signed",
                crt.display()
            );
            let p = tls::ensure_cert()?;
            let host = name.to_string();
            (p.cert, p.key, format!("https://{host}:{HTTPS_PORT}"))
        }
    } else {
        let p = tls::ensure_cert()?;
        let fallback = net::server::lan_ip().unwrap_or_else(|| "127.0.0.1".to_string());
        (p.cert, p.key, format!("https://{fallback}:{HTTPS_PORT}"))
    };

    println!("iphone-bridge v1 (mic enabled)");
    println!();
    println!("Open on iPhone (Safari):");
    println!("    {browser_url}");
    println!();

    // Tray icon (Win32 message loop on its own thread).
    let ctl_for_tray = capture_ctl.clone();
    let url_for_tray = browser_url.clone();
    std::thread::spawn(move || {
        if let Err(e) = tray::run_tray(ctl_for_tray, url_for_tray) {
            eprintln!("[tray] fatal: {e}");
        }
    });

    net::server::serve_tls(audio_tx, mic_tx, HTTPS_PORT, &cert_path, &key_path).await
}
