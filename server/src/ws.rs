//! WebSocket handlers: /ws (state broadcast) and /ws/input (HID events).

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use axum_extra::extract::CookieJar;
use futures_util::{SinkExt, StreamExt};
use protocol::InputEvent;

use crate::{auth, hid, hypr, state::AppState};

// ── /ws — state broadcast ─────────────────────────────────────────────────────

pub async fn ws_state(
    State(st): State<Arc<AppState>>,
    jar: CookieJar,
    ws: WebSocketUpgrade,
) -> Response {
    if !auth::ws_authed(&jar, &st.secrets.jwt_key) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_ws_state(socket, st))
}

async fn handle_ws_state(socket: WebSocket, st: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();
    let mut rx = st.state_rx.clone();

    // Immediately send the current state on connect.
    {
        let current = st.current_state();
        if let Ok(json) = serde_json::to_string(current.as_ref()) {
            let _ = sink.send(Message::Text(json)).await;
        }
    }

    // Track last sent JSON to avoid redundant sends.
    let mut last_json = String::new();

    loop {
        tokio::select! {
            // Forward state changes to the client.
            changed = rx.changed() => {
                if changed.is_err() { break; }
                let current = rx.borrow_and_update().clone();
                if let Ok(json) = serde_json::to_string(current.as_ref()) {
                    if json == last_json { continue; }
                    last_json = json.clone();
                    if sink.send(Message::Text(json)).await.is_err() { break; }
                }
            }
            // Drain client messages (keep-alive pings, we don't act on them).
            msg = stream.next() => {
                match msg {
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
        }
    }
}

// ── /ws/input — HID events ────────────────────────────────────────────────────

pub async fn ws_input(
    State(st): State<Arc<AppState>>,
    jar: CookieJar,
    ws: WebSocketUpgrade,
) -> Response {
    if !auth::ws_authed(&jar, &st.secrets.jwt_key) {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| handle_ws_input(socket, st))
}

async fn handle_ws_input(socket: WebSocket, st: Arc<AppState>) {
    let (_, mut stream) = socket.split();

    while let Some(msg) = stream.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        let event: InputEvent = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("ws/input: bad event: {e}: {text}");
                continue;
            }
        };

        // Dispatch — bad events must not drop the connection.
        if let Err(e) = dispatch_input(event, &st).await {
            tracing::debug!("ws/input dispatch error: {e}");
        }
    }
}

async fn dispatch_input(event: InputEvent, st: &AppState) -> anyhow::Result<()> {
    match event {
        InputEvent::Move { dx, dy } => hid::mouse_move(dx, dy)?,
        InputEvent::Scroll { dy } => hid::mouse_scroll(dy)?,
        InputEvent::Click { b } => hid::mouse_click(b)?,
        InputEvent::Text { text } => hid::type_text(&text)?,
        InputEvent::Key { key, down } => hid::key_event(&key, down)?,
        InputEvent::Combo { keys } => hid::key_combo(&keys)?,

        // Touch-on-video: map fraction to absolute position then click.
        InputEvent::Vmove { monitor, fx, fy } => {
            if let Some(pos) = monitor_frac_to_abs(&st, &monitor, fx, fy).await {
                hypr::move_cursor(pos.0, pos.1).await;
            }
        }
        InputEvent::Vclick { monitor, fx, fy } => {
            if let Some(pos) = monitor_frac_to_abs(&st, &monitor, fx, fy).await {
                hypr::move_cursor(pos.0, pos.1).await;
                hid::mouse_click(protocol::MouseButton::Left)?;
            }
        }
        InputEvent::Vrclick { monitor, fx, fy } => {
            if let Some(pos) = monitor_frac_to_abs(&st, &monitor, fx, fy).await {
                hypr::move_cursor(pos.0, pos.1).await;
                hid::mouse_click(protocol::MouseButton::Right)?;
            }
        }
        InputEvent::Vdown { monitor, fx, fy } => {
            if let Some(pos) = monitor_frac_to_abs(&st, &monitor, fx, fy).await {
                hypr::move_cursor(pos.0, pos.1).await;
                hid::mouse_button_raw(protocol::MouseButton::Left, true)?;
            }
        }
        InputEvent::Vup => {
            hid::mouse_button_raw(protocol::MouseButton::Left, false)?;
        }
    }
    Ok(())
}

/// Convert (monitor_name, fx, fy) fractions to absolute desktop coordinates.
async fn monitor_frac_to_abs(
    st: &AppState,
    monitor_name: &str,
    fx: f32,
    fy: f32,
) -> Option<(i32, i32)> {
    let state = st.current_state();
    let mon = state.hypr.monitors.iter().find(|m| m.name == monitor_name)?;
    let fx = fx.clamp(0.0, 1.0);
    let fy = fy.clamp(0.0, 1.0);
    let sc = mon.scale as f32;
    let gx = mon.x + (fx * mon.w as f32 / sc) as i32;
    let gy = mon.y + (fy * mon.h as f32 / sc) as i32;
    Some((gx, gy))
}

// Need IntoResponse on StatusCode
use axum::response::IntoResponse;
