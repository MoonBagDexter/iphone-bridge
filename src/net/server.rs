use anyhow::Result;
use axum::{
    routing::{get, post},
    Router,
};
use axum_server::tls_rustls::RustlsConfig;
use bytes::Bytes;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::audio::render::MicMsg;
use crate::dictate::DictationBuffer;
use crate::files::api as files_api;
use crate::files::config::Config;
use crate::files::sessions::SessionStore;
use crate::net::ws::{ws_audio, ws_mic};

#[derive(Clone)]
pub struct AppState {
    pub audio_tx: broadcast::Sender<Bytes>,
    pub mic_tx: mpsc::Sender<MicMsg>,
    /// Files-feature config (PIN, scope, roots); persisted on every change.
    pub config: Arc<RwLock<Config>>,
    /// Live Claude Code sessions spawned via `/api/spawn`.
    pub sessions: Arc<SessionStore>,
    /// Mic audio captured for dictation. Lives in shared state rather than per
    /// connection because iOS drops the mic socket freely mid-recording.
    pub dictation: Arc<Mutex<DictationBuffer>>,
}

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const WORKLET_JS: &str = include_str!("../../web/worklet.js");
const MIC_WORKLET_JS: &str = include_str!("../../web/mic-worklet.js");

// Tell every browser (including the iOS home-screen web-clip cache) to never
// serve a stale copy. Assets are tiny and rebuilds change them on every deploy.
const NO_STORE: &str = "no-store, no-cache, must-revalidate, max-age=0";

type StaticResponse = ([(axum::http::HeaderName, &'static str); 2], &'static str);

fn static_response(content_type: &'static str, body: &'static str) -> StaticResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (axum::http::header::CACHE_CONTROL, NO_STORE),
        ],
        body,
    )
}

async fn index() -> StaticResponse {
    static_response("text/html; charset=utf-8", INDEX_HTML)
}
async fn app_js() -> StaticResponse {
    static_response("application/javascript", APP_JS)
}
async fn worklet_js() -> StaticResponse {
    static_response("application/javascript", WORKLET_JS)
}
async fn mic_worklet_js() -> StaticResponse {
    static_response("application/javascript", MIC_WORKLET_JS)
}

/// 64 MB: comfortably past ten minutes of phone audio in any codec, while
/// still refusing anything that clearly isn't a voice note.
const DICTATE_BODY_LIMIT: usize = 64 * 1024 * 1024;

/// Transcribe a recording uploaded by the iOS Shortcut, type it into the
/// focused window, and hand the text back so the phone can show it.
///
/// The body is the audio file itself -- Shortcuts' "Get Contents of URL"
/// posts files raw, and a multipart wrapper would only add a step to build.
async fn dictate_upload(body: Bytes) -> impl axum::response::IntoResponse {
    use axum::http::StatusCode;
    use axum::Json;

    // Log arrival before doing any work: when a recording goes missing, the
    // first thing worth knowing is whether it reached the PC at all.
    crate::logging::log_both(&format!(
        "[dictate] upload received: {} bytes",
        body.len()
    ));

    if body.is_empty() {
        crate::logging::log_both("[dictate] rejected: empty body");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "empty recording"})),
        );
    }

    let started = std::time::Instant::now();
    let bytes = body.to_vec();
    let outcome = tokio::task::spawn_blocking(move || {
        let text = crate::dictate::transcribe_upload(&bytes)?;
        crate::dictate::deliver(&text)?;
        anyhow::Ok(text)
    })
    .await;
    let elapsed = started.elapsed().as_secs_f32();

    match outcome {
        Ok(Ok(text)) => {
            crate::logging::log_both(&format!(
                "[dictate] transcribed in {elapsed:.1}s, {} chars: {text:?}",
                text.chars().count()
            ));
            (StatusCode::OK, Json(serde_json::json!({ "text": text })))
        }
        Ok(Err(e)) => {
            crate::logging::log_both(&format!("[dictate] failed after {elapsed:.1}s: {e:#}"));
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
        Err(e) => {
            crate::logging::log_both(&format!("[dictate] task panicked after {elapsed:.1}s: {e}"));
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "transcription crashed" })),
            )
        }
    }
}

fn router(
    audio_tx: broadcast::Sender<Bytes>,
    mic_tx: mpsc::Sender<MicMsg>,
    config: Arc<RwLock<Config>>,
    sessions: Arc<SessionStore>,
) -> Router {
    let state = AppState {
        audio_tx,
        mic_tx,
        config,
        sessions,
        dictation: Arc::new(Mutex::new(DictationBuffer::default())),
    };

    // Files API: every `/api/*` route is gated by the PIN middleware. The
    // audio/mic WS and static routes stay unauthenticated (as today).
    // The upload route needs a larger body limit than axum's default (2 MB for
    // multipart); scope the 200 MB `DefaultBodyLimit` to just that route.
    let upload_route = Router::new()
        .route("/api/upload", post(files_api::upload))
        .layer(axum::extract::DefaultBodyLimit::max(
            files_api::UPLOAD_BODY_LIMIT,
        ));

    // Recordings arrive as a raw body from the iOS Shortcut, well over axum's
    // 2 MB default -- ten minutes of AAC is roughly 5 MB.
    let dictate_route = Router::new()
        .route("/api/dictate", post(dictate_upload))
        .layer(axum::extract::DefaultBodyLimit::max(DICTATE_BODY_LIMIT));

    let api = Router::new()
        .merge(dictate_route)
        .route("/api/roots", get(files_api::roots))
        .route("/api/ls", get(files_api::ls))
        .route("/api/gitstatus", get(files_api::gitstatus))
        .route("/api/mkdir", post(files_api::mkdir))
        .route("/api/rename", post(files_api::rename))
        .route("/api/delete", post(files_api::delete))
        .route("/api/spawn", post(files_api::spawn))
        .route("/api/sessions", get(files_api::list_sessions))
        .route("/api/kill", post(files_api::kill))
        .route("/api/trash", get(files_api::list_trash))
        .route("/api/trash/restore", post(files_api::restore_trash))
        .route("/api/search", get(files_api::search))
        .route("/api/download", get(files_api::download))
        .route("/api/gitchanges", get(files_api::gitchanges))
        .route("/api/session-peek", get(files_api::session_peek))
        .route("/api/zip", get(files_api::zip))
        .merge(upload_route)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            files_api::require_pin,
        ));

    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/worklet.js", get(worklet_js))
        .route("/mic-worklet.js", get(mic_worklet_js))
        .route("/audio", get(ws_audio))
        .route("/mic", get(ws_mic))
        .merge(api)
        .with_state(state)
}

/// Build the `RustlsConfig` up front so the caller can hold a clone of it and
/// hot-swap the cert later (e.g. once a real Tailscale cert becomes
/// available) without restarting the listener -- `RustlsConfig` is `Clone`
/// and reloads are applied in-place via `reload_from_pem_file`.
pub async fn load_tls_config(cert_path: &Path, key_path: &Path) -> Result<RustlsConfig> {
    Ok(RustlsConfig::from_pem_file(cert_path, key_path).await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn serve_tls(
    audio_tx: broadcast::Sender<Bytes>,
    mic_tx: mpsc::Sender<MicMsg>,
    files_config: Arc<RwLock<Config>>,
    sessions: Arc<SessionStore>,
    port: u16,
    tls_config: RustlsConfig,
) -> Result<()> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = std::net::TcpListener::bind(addr)?;
    // tokio's from_std (inside from_tcp_rustls) requires non-blocking mode.
    listener.set_nonblocking(true)?;
    make_uninheritable(&listener)?;
    eprintln!("[server] listening on https://0.0.0.0:{port}");
    axum_server::tls_rustls::from_tcp_rustls(listener, tls_config)?
        .serve(
            router(audio_tx, mic_tx, files_config, sessions)
                .into_make_service(),
        )
        .await?;
    Ok(())
}

/// Clear the inherit flag on the listening socket. Child processes spawned via
/// `std::process::Command` (Claude sessions from /api/spawn) inherit handles
/// on Windows; an inherited listener keeps port 8443 bound after the bridge
/// exits, so a restarted bridge can't bind while any spawned session lives.
fn make_uninheritable(listener: &std::net::TcpListener) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    let ok = unsafe {
        SetHandleInformation(listener.as_raw_socket() as _, HANDLE_FLAG_INHERIT, 0)
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub fn lan_ip() -> Option<String> {
    local_ip_address::local_ip().ok().map(|ip| ip.to_string())
}
