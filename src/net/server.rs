use anyhow::Result;
use axum::{response::Html, routing::get, Router};
use axum_server::tls_rustls::RustlsConfig;
use bytes::Bytes;
use std::net::SocketAddr;
use std::path::Path;
use tokio::sync::{broadcast, mpsc};

use crate::audio::render::MicMsg;
use crate::net::ws::{ws_audio, ws_mic};

#[derive(Clone)]
pub struct AppState {
    pub audio_tx: broadcast::Sender<Bytes>,
    pub mic_tx: mpsc::Sender<MicMsg>,
}

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const WORKLET_JS: &str = include_str!("../../web/worklet.js");
const MIC_WORKLET_JS: &str = include_str!("../../web/mic-worklet.js");

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

fn js_response(
    body: &'static str,
) -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        body,
    )
}

async fn app_js() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    js_response(APP_JS)
}
async fn worklet_js() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    js_response(WORKLET_JS)
}
async fn mic_worklet_js() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    js_response(MIC_WORKLET_JS)
}

fn router(audio_tx: broadcast::Sender<Bytes>, mic_tx: mpsc::Sender<MicMsg>) -> Router {
    let state = AppState { audio_tx, mic_tx };
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/worklet.js", get(worklet_js))
        .route("/mic-worklet.js", get(mic_worklet_js))
        .route("/audio", get(ws_audio))
        .route("/mic", get(ws_mic))
        .with_state(state)
}

pub async fn serve_tls(
    audio_tx: broadcast::Sender<Bytes>,
    mic_tx: mpsc::Sender<MicMsg>,
    port: u16,
    cert_path: &Path,
    key_path: &Path,
) -> Result<()> {
    let config = RustlsConfig::from_pem_file(cert_path, key_path).await?;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    eprintln!("[server] listening on https://0.0.0.0:{port}");
    axum_server::bind_rustls(addr, config)
        .serve(router(audio_tx, mic_tx).into_make_service())
        .await?;
    Ok(())
}

pub fn lan_ip() -> Option<String> {
    local_ip_address::local_ip().ok().map(|ip| ip.to_string())
}
