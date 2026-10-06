//! Hyprland IPC over its Unix sockets directly (no hyprctl, no hyprland-rs).
//!
//! Wire format (same as what hyprctl speaks):
//!   command socket  `.socket.sock`   write `[flags]/command args`, read reply until EOF
//!                                    e.g. `j/monitors`, `/dispatch <arg>`
//!   event socket    `.socket2.sock`  streams `EVENT>>DATA\n`
//!
//! Since Hyprland 0.55 a `hyprland.lua` config makes `/dispatch` evaluate its
//! argument as Lua (`return hl.dispatch(<arg>)`), so the legacy
//! `workspace 3` form is rejected. We probe once with `hl.dsp.no_op()` and then
//! send either `hl.dsp.*(...)` (Lua) or the legacy string (hyprlang configs).

use std::sync::Arc;

use protocol::{ActiveWindow, Monitor, Window};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::OnceCell,
    time::{sleep, timeout, Duration},
};

use crate::state::AppState;

// ── Socket paths ──────────────────────────────────────────────────────────────

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

// ── Raw command socket ────────────────────────────────────────────────────────

/// One request = one connection. Hyprland replies and closes, so read to EOF.
async fn request(payload: &str) -> Option<String> {
    let path = socket_path();
    let fut = async {
        let mut s = UnixStream::connect(&path).await?;
        s.write_all(payload.as_bytes()).await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(buf)
    };
    match timeout(Duration::from_secs(3), fut).await {
        Ok(Ok(buf)) => Some(String::from_utf8_lossy(&buf).into_owned()),
        Ok(Err(e)) => {
            tracing::debug!("hyprland socket error for `{payload}`: {e}");
            None
        }
        Err(_) => {
            tracing::warn!("hyprland socket timed out for `{payload}`");
            None
        }
    }
}

/// `j/<cmd>` → parsed JSON (monitors, clients, activewindow, ...).
async fn query_json(cmd: &str) -> Option<serde_json::Value> {
    let reply = request(&format!("j/{cmd}")).await?;
    serde_json::from_str(&reply).ok()
}

// ── Lua vs legacy dispatch ────────────────────────────────────────────────────

static LUA_MODE: OnceCell<bool> = OnceCell::const_new();

/// True when the running compositor evaluates dispatch args as Lua.
/// `hl.dsp.no_op()` is side-effect free and only parses under a Lua config.
/// Not cached if Hyprland was unreachable, so it re-probes next time.
async fn lua_mode() -> bool {
    LUA_MODE
        .get_or_try_init(|| async {
            let r = request("/dispatch hl.dsp.no_op()").await.ok_or(())?;
            let lua = r.trim() == "ok";
            tracing::info!(
                "Hyprland dispatch mode: {}",
                if lua { "lua (0.55+)" } else { "legacy" }
            );
            Ok::<_, ()>(lua)
        })
        .await
        .copied()
        .unwrap_or(false)
}

async fn dispatch(lua: String, legacy: String) -> bool {
    let cmd = if lua_mode().await { lua } else { legacy };
    match request(&format!("/dispatch {cmd}")).await {
        Some(r) if r.trim() == "ok" => true,
        Some(r) => {
            tracing::warn!("dispatch `{cmd}` rejected: {}", r.trim());
            false
        }
        None => false,
    }
}

/// Quote a Rust string as a Lua string literal. Inputs come from HTTP
/// requests, so this must escape properly to avoid Lua injection.
fn lua_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c if c.is_control() => o.push_str(&format!("\\{:03}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Window addresses look like `0x55d0c8a1b2c0`. Reject anything else.
fn valid_addr(a: &str) -> bool {
    a.strip_prefix("0x")
        .map(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or(false)
}

// ── Snapshot ──────────────────────────────────────────────────────────────────

pub async fn snapshot() -> protocol::HyprState {
    let monitors_val = query_json("monitors").await.unwrap_or_default();
    let clients_val = query_json("clients").await.unwrap_or_default();
    let active_val = query_json("activewindow").await.unwrap_or_default();

    let monitors_arr = monitors_val.as_array().cloned().unwrap_or_default();
    let clients_arr = clients_val.as_array().cloned().unwrap_or_default();

    // monitor id → name, for clients (which only carry the monitor id)
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
                monitor: c["monitor"]
                    .as_i64()
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
    dispatch(
        format!(r#"hl.dsp.focus({{ workspace = "{id}" }})"#),
        format!("workspace {id}"),
    )
    .await
}

pub async fn set_dpms(monitor: &str, on: bool) -> bool {
    let action = if on { "enable" } else { "disable" };
    dispatch(
        format!(
            "hl.dsp.dpms({{ action = {}, monitor = {} }})",
            lua_str(action),
            lua_str(monitor)
        ),
        format!("dpms {} {monitor}", if on { "on" } else { "off" }),
    )
    .await
}

pub async fn focus_window(address: &str) -> bool {
    if !valid_addr(address) {
        return false;
    }
    dispatch(
        format!(
            "hl.dsp.focus({{ window = {} }})",
            lua_str(&format!("address:{address}"))
        ),
        format!("focuswindow address:{address}"),
    )
    .await
}

/// Move a window to a workspace without following it (the old `movetoworkspacesilent`).
pub async fn move_window_to_workspace_by_address(address: &str, ws: i32) -> bool {
    if !valid_addr(address) {
        return false;
    }
    dispatch(
        format!(
            "hl.dsp.window.move({{ workspace = {}, window = {}, follow = false }})",
            lua_str(&ws.to_string()),
            lua_str(&format!("address:{address}"))
        ),
        format!("movetoworkspacesilent {ws},address:{address}"),
    )
    .await
}

pub async fn close_window(address: &str) -> bool {
    if !valid_addr(address) {
        return false;
    }
    dispatch(
        format!(
            "hl.dsp.window.close({{ window = {} }})",
            lua_str(&format!("address:{address}"))
        ),
        format!("closewindow address:{address}"),
    )
    .await
}

pub async fn move_cursor(x: i32, y: i32) -> bool {
    dispatch(
        format!("hl.dsp.cursor.move({{ x = {x}, y = {y} }})"),
        format!("movecursor {x} {y}"),
    )
    .await
}

pub async fn send_shortcut(cls: &str, key: &str, mods: &str) -> bool {
    if cls.is_empty() || key.is_empty() {
        return false;
    }
    dispatch(
        format!(
            "hl.dsp.send_shortcut({{ mods = {}, key = {}, window = {} }})",
            lua_str(mods),
            lua_str(key),
            lua_str(&format!("class:{cls}"))
        ),
        format!("sendshortcut {mods},{key},class:{cls}"),
    )
    .await
}

/// Execute a command string on a specific workspace (silently).
pub async fn exec_on_workspace(cmd: &str, workspace: i32) -> bool {
    dispatch(
        format!(
            "hl.dsp.exec_cmd({}, {{ workspace = {} }})",
            lua_str(cmd),
            lua_str(&format!("{workspace} silent"))
        ),
        format!("exec [workspace {workspace} silent] {cmd}"),
    )
    .await
}

/// Return clients list as raw JSON.
pub async fn clients_json() -> Vec<serde_json::Value> {
    query_json("clients")
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

/// Watch Hyprland's event socket (`EVENT>>DATA` lines). Reconnects on disconnect.
/// Publishes an updated hypr snapshot on each interesting event.
pub async fn watch_events(state: Arc<AppState>) {
    loop {
        let path = event_socket_path();
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                tracing::info!("connected to Hyprland event socket at {path}");
                let mut lines = BufReader::new(stream).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let event = line.split_once(">>").map(|(e, _)| e).unwrap_or(&line);
                            if INTERESTING_EVENTS.contains(&event.trim()) {
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
