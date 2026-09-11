//! Low-latency pointer control, independent of microphone activation.
use axum::{extract::ws::{Message, WebSocket, WebSocketUpgrade}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}};
use futures_util::StreamExt;
use serde::Deserialize;
use std::{mem::size_of, time::Duration};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSEEVENTF_MOVE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_HWHEEL};

static CONTROLLER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum Command {
    Move { x: i32, y: i32 },
    Wheel { x: i32, y: i32 },
    Click,
    Right,
    Down,
    Up,
    Ping,
}

fn parse(text: &str) -> Option<Command> {
    if text.len() > 160 { return None; }
    let cmd: Command = serde_json::from_str(text).ok()?;
    match cmd {
        Command::Move { x, y } | Command::Wheel { x, y }
            if !(-2048..=2048).contains(&x) || !(-2048..=2048).contains(&y) => None,
        _ => Some(cmd),
    }
}

fn send(flags: u32, x: i32, y: i32, data: i32) {
    let input = INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT {
        dx: x, dy: y, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0,
    } } };
    let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    if sent != 1 { crate::logging::log_both("[trackpad] Windows rejected pointer input"); }
}

#[derive(Default)]
struct Button { held: bool }
impl Button {
    fn release(&mut self) {
        if self.held { send(MOUSEEVENTF_LEFTUP, 0, 0, 0); self.held = false; }
    }
    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Move { x, y } => send(MOUSEEVENTF_MOVE, x, y, 0),
            Command::Wheel { x, y } => {
                if y != 0 { send(MOUSEEVENTF_WHEEL, 0, 0, y); }
                if x != 0 { send(MOUSEEVENTF_HWHEEL, 0, 0, -x); }
            }
            Command::Down if !self.held => { self.held = true; send(MOUSEEVENTF_LEFTDOWN, 0, 0, 0); }
            Command::Up => self.release(),
            Command::Click if !self.held => {
                send(MOUSEEVENTF_LEFTDOWN, 0, 0, 0); send(MOUSEEVENTF_LEFTUP, 0, 0, 0);
            }
            Command::Right if !self.held => {
                send(MOUSEEVENTF_RIGHTDOWN, 0, 0, 0); send(MOUSEEVENTF_RIGHTUP, 0, 0, 0);
            }
            _ => {}
        }
    }
}
impl Drop for Button { fn drop(&mut self) { self.release(); } }

pub async fn upgrade(ws: WebSocketUpgrade, headers: HeaderMap) -> Response {
    // Browser pointer control must originate from the bridge itself.
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else { return StatusCode::FORBIDDEN.into_response(); };
    let expected = format!("https://{host}");
    if origin != Some(expected.as_str()) { return StatusCode::FORBIDDEN.into_response(); }
    ws.max_message_size(160).on_upgrade(handle)
}

async fn handle(mut socket: WebSocket) {
    let Ok(_owner) = CONTROLLER.try_lock() else {
        let _ = socket.send(Message::Text(r#"{"type":"busy"}"#.into())).await;
        return;
    };
    let mut button = Button::default();
    if socket.send(Message::Text(r#"{"type":"ready"}"#.into())).await.is_err() { return; }
    loop {
        match tokio::time::timeout(Duration::from_secs(2), socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Some(cmd) = parse(&text) { button.apply(cmd); } else { break; }
            }
            Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => {},
            Err(_) if !button.held => continue,
            // A suspended phone or broken connection cannot leave Windows dragging.
            _ => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_signed_motion_and_button_release() {
        assert!(matches!(parse(r#"{"op":"move","x":-12,"y":4}"#), Some(Command::Move { x: -12, y: 4 })));
        assert!(matches!(parse(r#"{"op":"up"}"#), Some(Command::Up)));
        assert!(matches!(parse(r#"{"op":"wheel","x":0,"y":-120}"#), Some(Command::Wheel { .. })));
    }
    #[test]
    fn rejects_malformed_or_unbounded_input() {
        for text in [r#"{"op":"move","x":2147483647,"y":0}"#, r#"{"op":"move","x":0.5,"y":0}"#, r#"{"op":"move","x":0}"#, r#"{"op":"launch"}"#, r#"{"op":"click","extra":true}"#] {
            assert!(parse(text).is_none(), "{text}");
        }
    }
}
