//! Hyprland IPC: watch events and dispatch commands via hyprctl.

use std::sync::Arc;

use protocol::{ActiveWindow, DeckState, Monitor, Window};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixStream,
    time::{sleep, Duration},
};

use crate::state::AppState;

// ── Hyprland socket paths ─────────────────────────────────────────────────────

fn his() -> String {
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE").unwrap_or_default()
}

fn runtime_dir() -> String {
    std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("RUNTIME_DIR"))
        .unwrap_or_else(|_| "/run/user/1000".into())
}

fn socket_path() -> String {
    format!("{}/hypr/{}/.socket.sock", runtime_dir(), his())
}

fn event_socket_path() -> String {
    format!("{}/hypr/{}/.socket2.sock", runtime_dir(), his())
}

// ── hyprctl helper ────────────────────────────────────────────────────────────

async fn hyprctl_json(cmd: &str) -> Option<serde_json::Value> {
    let out = tokio::process::Command::new("hyprctl")
        .arg("-j")
        .arg(cmd)
        .output()
        .await
        .ok()?;
    if out.status.success() {
        serde_json::from_slice(&out.stdout).ok()
    } else {
        None
    }
}

async fn hyprctl_dispatch(dispatch: &str, args: &[&str]) -> bool {
    let mut cmd = tokio::process::Command::new("hyprctl");
    cmd.arg("dispatch").arg(dispatch);
    for a in args { cmd.arg(a); }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn hyprctl_eval(expr: &str) -> bool {
    tokio::process::Command::new("hyprctl")
        .arg("keyword")
        .arg("exec")
        .arg(expr)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Snapshot ──────────────────────────────────────────────────────────────────

pub async fn snapshot() -> protocol::HyprState {
    let monitors_val = hyprctl_json("monitors").await.unwrap_or_default();
    let clients_val = hyprctl_json("clients").await.unwrap_or_default();
    let active_val = hyprctl_json("activewindow").await.unwrap_or_default();

    let monitors_arr = monitors_val.as_array().cloned().unwrap_or_default();
    let clients_arr = clients_val.as_array().cloned().unwrap_or_default();

    // Build id→name map for clients
    let id_to_name: std::collections::HashMap<i64, String> = monitors_arr
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_i64()?;
            let name = m["name"].as_str()?.to_string();
            Some((id, name))
        })
        .collect();

    let focused_addr = active_val["address"].as_str().unwrap_or("").to_string();

    let mut monitors: Vec<Monitor> = monitors_arr
        .iter()
        .map(|m| Monitor {
            name: m["name"].as_str().unwrap_or("").to_string(),
            model: m["model"].as_str().unwrap_or("").to_string(),
            x: m["x"].as_i64().unwrap_or(0) as i32,
            y: m["y"].as_i64().unwrap_or(0) as i32,
            w: m["width"].as_i64().unwrap_or(0) as i32,
            h: m["height"].as_i64().unwrap_or(0) as i32,
            refresh: m["refreshRate"].as_f64().unwrap_or(0.0).round() as i32,
            active_ws: m["activeWorkspace"]["id"].as_i64().map(|v| v as i32),
            focused: m["focused"].as_bool().unwrap_or(false),
            dpms: m["dpmsStatus"].as_bool().unwrap_or(true),
            scale: m["scale"].as_f64().unwrap_or(1.0),
        })
        .collect();
    monitors.sort_by_key(|m| m.x);

    let mut windows: Vec<Window> = clients_arr
        .iter()
        .filter(|c| c["mapped"].as_bool().unwrap_or(true))
        .map(|c| {
            let addr = c["address"].as_str().unwrap_or("").to_string();
            Window {
                address: addr.clone(),
                title: c["title"].as_str().unwrap_or("").chars().take(60).collect(),
                cls: c["class"].as_str().unwrap_or("").to_string(),
                ws: c["workspace"]["id"].as_i64().map(|v| v as i32),
                monitor: c["monitor"].as_i64()
                    .and_then(|id| id_to_name.get(&id))
                    .cloned()
                    .unwrap_or_default(),
                focused: addr == focused_addr,
            }
        })
        .collect();
    windows.sort_by_key(|w| w.ws.unwrap_or(0));

    let active_window = ActiveWindow {
        title: active_val["title"].as_str().unwrap_or("").to_string(),
        class: active_val["class"].as_str().unwrap_or("").to_string(),
    };

    protocol::HyprState { monitors, windows, active_window }
}

// ── Dispatchers ───────────────────────────────────────────────────────────────

pub async fn focus_workspace(id: i32) -> bool {
    hyprctl_dispatch("workspace", &[&id.to_string()]).await
}

pub async fn set_dpms(monitor: &str, on: bool) -> bool {
    let state = if on { "on" } else { "off" };
    hyprctl_dispatch("dpms", &[state, monitor]).await
}

pub async fn focus_window(address: &str) -> bool {
    if address.is_empty() { return false; }
    hyprctl_dispatch("focuswindow", &[&format!("address:{address}")]).await
}

pub async fn move_window_to_workspace_by_address(address: &str, ws: i32) -> bool {
    hyprctl_dispatch(
        "movetoworkspacesilent",
        &[&format!("{ws},address:{address}")],
    )
    .await
}

pub async fn close_window(address: &str) -> bool {
    hyprctl_dispatch("closewindow", &[&format!("address:{address}")]).await
}

pub async fn move_cursor(x: i32, y: i32) -> bool {
    hyprctl_dispatch("movecursor", &[&x.to_string(), &y.to_string()]).await
}

pub async fn send_shortcut(cls: &str, key: &str, mods: &str) -> bool {
    if cls.is_empty() || key.is_empty() { return false; }
    let arg = format!("{mods},{key},class:{cls}");
    hyprctl_dispatch("sendshortcut", &[&arg]).await
}

/// Execute a command string on a specific workspace.
pub async fn exec_on_workspace(cmd: &str, workspace: i32) -> bool {
    hyprctl_dispatch("exec", &[&format!("[workspace {workspace} silent] {cmd}")]).await
}

/// Return clients list as raw JSON.
pub async fn clients_json() -> Vec<serde_json::Value> {
    hyprctl_json("clients")
        .await
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

// ── Event watcher ─────────────────────────────────────────────────────────────

const INTERESTING_EVENTS: &[&str] = &[
    "workspace",
    "focusedmon",
    "activewindow",
    "openwindow",
    "closewindow",
    "movewindow",
    "monitoradded",
    "monitorremoved",
];

/// Watch Hyprland's event socket. Reconnects with backoff on disconnect.
/// Calls `state.publish()` with an updated hypr snapshot on each interesting event.
pub async fn watch_events(state: Arc<AppState>) {
    loop {
        let path = event_socket_path();
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                tracing::info!("connected to Hyprland event socket at {path}");
                let reader = BufReader::new(stream);
                let mut lines = reader.lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let event = line.split(">>").next().unwrap_or("").trim().to_string();
                            if INTERESTING_EVENTS.contains(&event.as_str()) {
                                let mut s = state.current_state().as_ref().clone();
                                s.hypr = snapshot().await;
                                state.publish(s);
                            }
                        }
                        Ok(None) => {
                            tracing::warn!("Hyprland event socket closed, reconnecting...");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!("Hyprland event socket error: {e}, reconnecting...");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!("Cannot connect to Hyprland event socket ({e}), retrying in 2s...");
            }
        }
        sleep(Duration::from_secs(2)).await;
    }
}
