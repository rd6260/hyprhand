//! Shared protocol types for hyprhand.
//! Used by the server and serialised over the WebSocket wire.

use serde::{Deserialize, Serialize};

// ── Input events (phone → server /ws/input) ──────────────────────────────────

/// Every message the phone sends over the input WebSocket.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum InputEvent {
    /// Relative mouse movement.
    Move { dx: i32, dy: i32 },
    /// Scroll wheel (positive = up).
    Scroll { dy: i32 },
    /// Mouse button click (press + release).
    Click { b: MouseButton },
    /// Type a Unicode string (converted to keycodes).
    Text { text: String },
    /// Single key press or release.
    Key { key: String, down: bool },
    /// Key chord (press all, release all in reverse).
    Combo { keys: Vec<String> },
    // ── Touch-on-video helpers ──
    /// Cursor move to fraction (0..1) of the monitor area.
    Vmove { monitor: String, fx: f32, fy: f32 },
    /// Tap → left click.
    Vclick { monitor: String, fx: f32, fy: f32 },
    /// Long-press → right click.
    Vrclick { monitor: String, fx: f32, fy: f32 },
    /// Drag start → button down.
    Vdown { monitor: String, fx: f32, fy: f32 },
    /// Drag end → button up (no position needed).
    Vup,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

// ── Live deck state (server → phone /ws broadcast) ───────────────────────────

/// Full state object broadcast over /ws.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct DeckState {
    pub hypr: HyprState,
    pub audio: AudioState,
    pub telemetry: TelemetryState,
    pub sysinfo: SysInfoState,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct HyprState {
    pub monitors: Vec<Monitor>,
    pub windows: Vec<Window>,
    pub active_window: ActiveWindow,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Monitor {
    pub name: String,
    pub model: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub refresh: i32,
    pub active_ws: Option<i32>,
    pub focused: bool,
    pub dpms: bool,
    pub scale: f64,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Window {
    pub address: String,
    pub title: String,
    pub cls: String,
    pub ws: Option<i32>,
    pub monitor: String,
    pub focused: bool,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct ActiveWindow {
    pub title: String,
    pub class: String,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct AudioState {
    pub volume: Option<i32>,
    pub sink_muted: bool,
    pub mic_muted: bool,
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub recording: Vec<String>,
    pub now_playing: NowPlaying,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct AudioDevice {
    pub id: u32,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct NowPlaying {
    pub status: String,
    pub title: String,
    pub player: String,
    pub art_key: String,
    pub position_us: Option<i64>,
    pub duration_us: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct TelemetryState {
    pub cpu: Option<CpuInfo>,
    pub gpu: Option<GpuInfo>,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct CpuInfo {
    pub util: f32,
    pub temp: Option<f32>,
    pub mem_used: f32,
    pub mem_total: f32,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct GpuInfo {
    pub util: f32,
    pub temp: f32,
    pub vram_used: f32,
    pub vram_total: f32,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct SysInfoState {
    pub tailscale: TailscaleInfo,
    pub procs: Vec<ProcessInfo>,
    pub boot: u64,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct TailscaleInfo {
    pub up: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_name: Option<String>,
    pub online: u32,
    pub total: u32,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub mem: f32,
    pub mine: bool,
}
