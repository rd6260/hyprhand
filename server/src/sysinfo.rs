//! System information: Tailscale status, top processes, lock screen, kill.

use protocol::{ProcessInfo, SysInfoState, TailscaleInfo};
use sysinfo::System;

pub async fn snapshot() -> SysInfoState {
    let tailscale = tailscale_info().await;
    let procs = top_procs(8);
    let boot = System::boot_time();
    SysInfoState { tailscale, procs, boot }
}

// ── Tailscale ─────────────────────────────────────────────────────────────────

pub async fn tailscale_info() -> TailscaleInfo {
    let out = tokio::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .await
        .ok();

    let raw = match out {
        Some(o) if o.status.success() => o.stdout,
        _ => return TailscaleInfo::default(),
    };

    let data: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => return TailscaleInfo::default(),
    };

    let up = data["BackendState"].as_str() == Some("Running");
    let self_name = data["Self"]["HostName"].as_str().map(|s| s.to_string());

    let peers = data["Peer"]
        .as_object()
        .map(|m| m.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let total = peers.len() as u32;
    let online = peers
        .iter()
        .filter(|p| p["Online"].as_bool().unwrap_or(false))
        .count() as u32;

    TailscaleInfo { up, self_name, online, total }
}

// ── Process list ──────────────────────────────────────────────────────────────

pub fn top_procs(n: usize) -> Vec<ProcessInfo> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All);

    // Get own UID for "mine" flag
    let my_uid = unsafe { libc::getuid() };

    let total_mem = sys.total_memory().max(1);
    let mut procs: Vec<ProcessInfo> = sys
        .processes()
        .iter()
        .map(|(pid, proc_)| {
            let uid = proc_.user_id().map(|u| **u as u32).unwrap_or(u32::MAX);
            ProcessInfo {
                pid: pid.as_u32(),
                name: proc_.name().to_string_lossy().chars().take(22).collect(),
                mem: (proc_.memory() as f64 / total_mem as f64 * 100.0) as f32,
                mine: uid == my_uid,
            }
        })
        .collect();

    procs.sort_by(|a, b| b.mem.partial_cmp(&a.mem).unwrap_or(std::cmp::Ordering::Equal));
    procs.truncate(n);
    procs
}

// ── Actions ───────────────────────────────────────────────────────────────────

/// Send SIGTERM to `pid`. Only succeeds for processes owned by the current user.
pub fn kill(pid: u32) -> serde_json::Value {
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if ret == 0 {
        serde_json::json!({ "ok": true })
    } else {
        let err = std::io::Error::last_os_error();
        serde_json::json!({ "ok": false, "error": err.to_string() })
    }
}

/// Spawn hyprlock in the background.
pub async fn lock() -> bool {
    tokio::process::Command::new("hyprlock")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}
