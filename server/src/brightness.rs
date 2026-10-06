//! Brightness control: DDC/CI external monitors (ddcutil) and laptop backlight (brightnessctl).

use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, OnceLock},
};

// ── Detected backend (cached after first detection) ───────────────────────────

struct BrightState {
    detected: bool,
    bus_by_monitor: HashMap<String, u32>, // DDC: monitor name → i2c bus
    bl_monitor: String,                    // backlight: hyprland monitor name
    bl_device: String,                     // backlight: brightnessctl device name
}

static STATE: Mutex<BrightState> = Mutex::new(BrightState {
    detected: false,
    bus_by_monitor: HashMap::new(),
    bl_monitor: String::new(),
    bl_device: String::new(),
});

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn cmd_out(args: &[&str], timeout_secs: u64) -> String {
    if args.is_empty() { return String::new(); }
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new(args[0]).args(&args[1..]).output(),
    )
    .await
    {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

async fn cmd_fire(args: &[&str], timeout_secs: u64) -> bool {
    if args.is_empty() { return false; }
    tokio::time::timeout(
        tokio::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new(args[0])
            .args(&args[1..])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .map(|s| s.success())
    .unwrap_or(false)
}

// ── DDC detection ─────────────────────────────────────────────────────────────

fn parse_ddc(text: &str) -> HashMap<String, u32> {
    let mut result = HashMap::new();
    let mut valid = false;
    let mut bus: Option<u32> = None;

    for line in text.lines() {
        let is_header = !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty();
        if is_header {
            valid = line.trim().starts_with("Display");
            bus = None;
            continue;
        }
        let s = line.trim();
        if s.starts_with("I2C bus:") {
            bus = s.split("i2c-").nth(1)
                .and_then(|n| n.split_whitespace().next())
                .and_then(|n| n.parse().ok());
        } else if s.starts_with("DRM connector:") {
            if let (Some(b), true) = (bus, valid) {
                let conn = s.split(':').nth(1).unwrap_or("").trim();
                let name = regex_strip_card(conn);
                if !name.is_empty() {
                    result.insert(name, b);
                }
            }
        }
    }
    result
}

fn regex_strip_card(conn: &str) -> String {
    // Strip leading "cardN-" prefix
    if let Some(pos) = conn.find('-') {
        let prefix = &conn[..pos];
        if prefix.starts_with("card") && prefix[4..].chars().all(|c| c.is_ascii_digit()) {
            return conn[pos + 1..].to_string();
        }
    }
    conn.to_string()
}

// ── Backlight detection ───────────────────────────────────────────────────────

fn detect_backlight() -> Option<(String, String)> {
    // Check brightnessctl is available
    if std::process::Command::new("brightnessctl").arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status().is_err() { return None; }

    let bl_dir = Path::new("/sys/class/backlight");
    let entries: Vec<_> = std::fs::read_dir(bl_dir).ok()?.flatten()
        .filter(|e| e.path().join("max_brightness").exists())
        .collect();
    if entries.is_empty() { return None; }

    // Prefer intel_backlight
    let dev = entries.iter()
        .find(|e| e.file_name().to_string_lossy() == "intel_backlight")
        .or_else(|| entries.first())?;

    let monitor_name = edp_monitor_name();
    Some((monitor_name, dev.file_name().to_string_lossy().into_owned()))
}

fn edp_monitor_name() -> String {
    let drm = Path::new("/sys/class/drm");
    let connected: Vec<String> = std::fs::read_dir(drm)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().contains('-'))
                .filter(|e| {
                    std::fs::read_to_string(e.path().join("status"))
                        .map(|s| s.trim() == "connected")
                        .unwrap_or(false)
                })
                .map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    regex_strip_card(&name)
                })
                .collect()
        })
        .unwrap_or_default();

    connected.iter().find(|n| n.starts_with("eDP"))
        .or_else(|| connected.first())
        .cloned()
        .unwrap_or_else(|| "eDP-1".to_string())
}

// ── Detection (runs once) ─────────────────────────────────────────────────────

async fn ensure_detected() {
    let already = STATE.lock().unwrap().detected;
    if already { return; }

    let ddc_text = cmd_out(&["ddcutil", "detect", "--brief"], 25).await;
    let bus_map = parse_ddc(&ddc_text);
    let bl = detect_backlight();

    let mut s = STATE.lock().unwrap();
    s.detected = true;
    s.bus_by_monitor = bus_map;
    if let Some((mon, dev)) = bl {
        s.bl_monitor = mon;
        s.bl_device = dev;
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns {monitor_name: brightness%} for all controllable monitors.
pub async fn levels() -> HashMap<String, i32> {
    ensure_detected().await;
    let mut out = HashMap::new();

    // DDC monitors
    let buses: Vec<(String, u32)> = {
        let s = STATE.lock().unwrap();
        s.bus_by_monitor.iter().map(|(k, v)| (k.clone(), *v)).collect()
    };
    for (name, bus) in buses {
        let raw = cmd_out(&["ddcutil", "--bus", &bus.to_string(), "getvcp", "10", "--brief"], 10).await;
        // Parse "VCP 10 SL XX YY" or "VCP 10 C XX YY"
        if let Some(val) = parse_vcp10(&raw) {
            out.insert(name, val);
        }
    }

    // Backlight
    let (bl_mon, _bl_dev) = {
        let s = STATE.lock().unwrap();
        (s.bl_monitor.clone(), s.bl_device.clone())
    };
    if !bl_mon.is_empty() {
        if let Some(pct) = read_backlight_pct() {
            out.insert(bl_mon, pct);
        }
    }

    out
}

pub async fn set_level(monitor: &str, pct: i32) -> bool {
    ensure_detected().await;
    let pct = pct.clamp(0, 100);

    let (bus, bl_mon, bl_dev) = {
        let s = STATE.lock().unwrap();
        let bus = s.bus_by_monitor.get(monitor).copied();
        (bus, s.bl_monitor.clone(), s.bl_device.clone())
    };

    if let Some(b) = bus {
        cmd_fire(&["ddcutil", "--bus", &b.to_string(), "setvcp", "10", &pct.to_string()], 10).await
    } else if monitor == bl_mon && !bl_dev.is_empty() {
        let safe_pct = pct.max(5); // never fully dark on laptop panel
        cmd_fire(&["brightnessctl", "-d", &bl_dev, "set", &format!("{safe_pct}%")], 5).await
    } else {
        false
    }
}

fn parse_vcp10(text: &str) -> Option<i32> {
    // "VCP 10 SL 64 64" or "VCP 10 C 64 100"
    let line = text.lines().find(|l| l.contains("VCP 10"))?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    // current value is 4th token (0-indexed 3)
    parts.get(3)?.parse().ok()
}

fn read_backlight_pct() -> Option<i32> {
    let base = Path::new("/sys/class/backlight");
    for entry in std::fs::read_dir(base).ok()?.flatten() {
        let cur = std::fs::read_to_string(entry.path().join("brightness"))
            .ok()?.trim().parse::<u64>().ok()?;
        let max = std::fs::read_to_string(entry.path().join("max_brightness"))
            .ok()?.trim().parse::<u64>().ok()?;
        if max > 0 {
            return Some(((cur * 100) / max) as i32);
        }
    }
    None
}
