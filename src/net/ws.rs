use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::oneshot;

use crate::audio::render::MicMsg;
use crate::keyboard;
use crate::net::server::AppState;

pub async fn ws_audio(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_audio(socket, state))
}

async fn handle_audio(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.audio_tx.subscribe();

    let header = r#"{"type":"format","sampleRate":48000,"channels":2,"sampleFormat":"f32"}"#;
    if sender
        .send(Message::Text(header.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    let drain = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    });

    loop {
        match rx.recv().await {
            Ok(frame) => {
                if sender.send(Message::Binary(frame)).await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                eprintln!("[ws] subscriber lagged, skipped {skipped} frames");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    drain.abort();
}

/// Mic uplink: iPhone -> server. Receives f32 stereo interleaved PCM @ 48kHz
/// as binary WS messages, forwards to the WASAPI render thread via mpsc.
pub async fn ws_mic(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_mic(socket, state))
}

async fn handle_mic(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    eprintln!("[ws-mic] client connected");

    // Tell the client our expected format.
    let header = r#"{"type":"format","sampleRate":48000,"channels":2,"sampleFormat":"f32"}"#;
    let _ = sender
        .send(Message::Text(header.to_string().into()))
        .await;

    while let Some(msg_res) = receiver.next().await {
        let msg = match msg_res {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[ws-mic] recv error: {e}");
                break;
            }
        };
        match msg {
            Message::Binary(bytes) => {
                let bytes = Bytes::copy_from_slice(&bytes);
                if state.mic_tx.send(MicMsg::Pcm(bytes)).await.is_err() {
                    eprintln!("[ws-mic] render thread channel closed");
                    break;
                }
            }
            Message::Text(s) => match s.as_str() {
                "ptt:start" => {
                    eprintln!("[ws-mic] ptt:start -- tapping Alt");
                    keyboard::tap_alt();
                }
                "ptt:stop" => {
                    eprintln!("[ws-mic] ptt:stop -- waiting for drain ack before tapping Alt");
                    let (ack_tx, ack_rx) = oneshot::channel();
                    if state.mic_tx.send(MicMsg::Drain(ack_tx)).await.is_err() {
                        eprintln!(
                            "[ws-mic] render thread channel closed during drain; tapping Alt anyway"
                        );
                        keyboard::tap_alt();
                        continue;
                    }
                    // Don't block the receiver loop while WASAPI drains -- spawn.
                    tokio::spawn(async move {
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(3),
                            ack_rx,
                        )
                        .await
                        {
                            Ok(Ok(())) => {
                                eprintln!("[ws-mic] drain ack received, tapping Alt");
                            }
                            Ok(Err(_)) => {
                                eprintln!(
                                    "[ws-mic] drain ack channel dropped, tapping Alt anyway"
                                );
                            }
                            Err(_) => {
                                eprintln!(
                                    "[ws-mic] drain ack timeout (3s), tapping Alt anyway"
                                );
                            }
                        }
                        keyboard::tap_alt();
                    });
                }
                "key:up" => keyboard::tap_arrow_up(),
                "key:down" => keyboard::tap_arrow_down(),
                "key:left" => keyboard::tap_arrow_left(),
                "key:right" => keyboard::tap_arrow_right(),
                "key:enter" => keyboard::tap_enter(),
                "key:escape" => keyboard::tap_escape(),
                "key:backspace" => keyboard::tap_backspace(),
                "key:desktop-left" => keyboard::tap_desktop_left(),
                "key:desktop-right" => keyboard::tap_desktop_right(),
                "key:alt-tab" => keyboard::alt_tab_cycle(),
                "key:ctrl-tab" => keyboard::tap_ctrl_tab(),
                "key:shift-tab" => keyboard::tap_shift_tab(),
                "key:ctrl-u" => keyboard::tap_ctrl_u(),
                "key:ctrl-w" => keyboard::tap_ctrl_w(),
                "key:ctrl-k" => keyboard::tap_ctrl_k(),
                "key:ctrl-a" => keyboard::tap_ctrl_a(),
                "key:ctrl-e" => keyboard::tap_ctrl_e(),
                "key:btw" => keyboard::type_btw(),
                "key:push" => keyboard::type_push(),
                "key:space" => keyboard::tap_space(),
                "key:min-window" => keyboard::tap_min_window(),
                other => {
                    eprintln!("[ws-mic] ignoring unknown text frame: {other:?}");
                }
            },
            Message::Close(_) => break,
            _ => { /* ignore */ }
        }
    }
    eprintln!("[ws-mic] client disconnected");
}
