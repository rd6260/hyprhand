//! Audio: pactl/wpctl/playerctl wrappers for volume, mute, sink selection, and MPRIS.

use std::collections::HashSet;

use protocol::{AudioDevice, AudioState, NowPlaying};

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn cmd_out(args: &[&str]) -> String {
    if args.is_empty() { return String::new(); }
    let out = tokio::process::Command::new(args[0])
        .args(&args[1..])
        .output()
        .await
        .ok();
    match out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

async fn cmd_fire(args: &[&str]) -> bool {
    if args.is_empty() { return false; }
    tokio::process::Command::new(args[0])
        .args(&args[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── PipeWire device enumeration ───────────────────────────────────────────────

/// Represents a PipeWire audio node from `pw-dump`.
struct PwNode {
    id: u32,
    name: String,
    desc: String,
    is_source: bool,
}

const DECK_CAPTURE: &[&str] = &["parec", "pacat", "pw-record", "pw-cat"];

async fn pw_audio_nodes() -> (Vec<PwNode>, Vec<PwNode>) {
    let raw = cmd_out(&["pw-dump"]).await;
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (vec![], vec![]);
    };
    let Some(arr) = val.as_array() else { return (vec![], vec![]); };

    let mut sinks = vec![];
    let mut sources = vec![];

    for obj in arr {
        let ty = obj["type"].as_str().unwrap_or("");
        if ty != "PipeWire:Interface:Node" { continue; }
        let props = &obj["info"]["props"];
        let media_class = props["media.class"].as_str().unwrap_or("");
        if media_class != "Audio/Sink" && media_class != "Audio/Source" { continue; }
        let id = obj["id"].as_u64().unwrap_or(0) as u32;
        let name = props["node.name"].as_str().unwrap_or("").to_string();
        let desc = props["node.description"]
            .as_str()
            .or_else(|| props["node.nick"].as_str())
            .unwrap_or(&name)
            .to_string();
        let node = PwNode { id, name, desc, is_source: media_class == "Audio/Source" };
        if node.is_source {
            sources.push(node);
        } else {
            sinks.push(node);
        }
    }
    (sinks, sources)
}

async fn recording_apps() -> Vec<String> {
    // Use pactl list source-outputs to find apps reading the mic
    let raw = cmd_out(&["pactl", "list", "source-outputs"]).await;
    let mut apps = vec![];
    let mut app_name = String::new();
    let mut binary = String::new();
    for line in raw.lines() {
        let s = line.trim();
        if s.starts_with("Source Output #") {
            if !app_name.is_empty() || !binary.is_empty() {
                let ident = if !binary.is_empty() { binary.clone() } else { app_name.clone() };
                if !ident.is_empty() && !DECK_CAPTURE.contains(&ident.as_str()) {
                    apps.push(ident);
                }
            }
            app_name.clear(); binary.clear();
        } else if s.starts_with("application.process.binary") {
            binary = s.split('=').nth(1).unwrap_or("").trim().trim_matches('"').to_string();
        } else if s.starts_with("application.name") {
            app_name = s.split('=').nth(1).unwrap_or("").trim().trim_matches('"').to_string();
        }
    }
    let ident = if !binary.is_empty() { binary } else { app_name };
    if !ident.is_empty() && !DECK_CAPTURE.contains(&ident.as_str()) {
        apps.push(ident);
    }
    let seen: HashSet<_> = apps.into_iter().collect();
    seen.into_iter().collect()
}

// ── MPRIS / playerctl ─────────────────────────────────────────────────────────

const BROWSER_PREFIXES: &[&str] = &[
    "chromium", "chrome", "brave", "firefox", "vivaldi", "opera", "edge", "microsoft-edge",
];

async fn players() -> Vec<(String, String)> {
    let names_raw = cmd_out(&["playerctl", "-l"]).await;
    let names: Vec<String> = names_raw.split_whitespace().map(|s| s.to_string()).collect();
    if names.is_empty() { return vec![]; }
    let statuses_raw = cmd_out(&["playerctl", "-a", "status"]).await;
    let statuses: Vec<String> = statuses_raw.lines().map(|s| s.trim().to_string()).collect();
    names
        .into_iter()
        .enumerate()
        .map(|(i, n)| (n, statuses.get(i).cloned().unwrap_or_default()))
        .collect()
}

fn pick_player(players: &[(String, String)]) -> Option<&str> {
    let has_plasma = players.iter().any(|(n, _)| n.starts_with("plasma-browser-integration"));
    let keep = |n: &str| -> bool {
        if n.starts_with("playerctld") { return false; }
        if has_plasma && BROWSER_PREFIXES.iter().any(|p| n.starts_with(p)) { return false; }
        true
    };
    let cand: Vec<_> = players.iter().filter(|(n, _)| keep(n)).collect();
    let cand = if cand.is_empty() { players.iter().collect::<Vec<_>>() } else { cand };
    for want in ["Playing", "Paused"] {
        if let Some((n, _)) = cand.iter().find(|(_, s)| s == want) {
            return Some(n.as_str());
        }
    }
    cand.first().map(|(n, _)| n.as_str())
}

// ── Public snapshot ───────────────────────────────────────────────────────────

pub async fn snapshot() -> AudioState {
    let vol_raw = cmd_out(&["pactl", "get-sink-volume", "@DEFAULT_SINK@"]).await;
    let volume = vol_raw
        .split('%')
        .next()
        .and_then(|s| s.split_whitespace().last())
        .and_then(|s| s.parse::<i32>().ok());

    let sink_muted = cmd_out(&["pactl", "get-sink-mute", "@DEFAULT_SINK@"]).await.contains("yes");
    let mic_muted = cmd_out(&["pactl", "get-source-mute", "@DEFAULT_SOURCE@"]).await.contains("yes");

    let default_sink = cmd_out(&["pactl", "get-default-sink"]).await.trim().to_string();
    let default_source = cmd_out(&["pactl", "get-default-source"]).await.trim().to_string();

    let (pw_sinks, pw_sources) = pw_audio_nodes().await;
    let sinks = pw_sinks
        .iter()
        .map(|s| AudioDevice { id: s.id, name: s.desc.clone(), active: s.name == default_sink })
        .collect();
    let sources = pw_sources
        .iter()
        .map(|s| AudioDevice { id: s.id, name: s.desc.clone(), active: s.name == default_source })
        .collect();

    let all_players = players().await;
    let player = pick_player(&all_players).map(|s| s.to_string());
    let psel = player.as_deref();

    let mut np_args = vec!["playerctl"];
    if let Some(p) = psel { np_args.extend_from_slice(&["-p", p]); }
    np_args.extend_from_slice(&["metadata", "--format", "{{status}}|{{artist}} - {{title}}"]);
    let np_raw = cmd_out(&np_args).await;
    let (status, title) = np_raw.trim().split_once('|')
        .map(|(s, t)| (s.to_string(), t.trim_matches(|c| c == ' ' || c == '-').to_string()))
        .unwrap_or_default();

    // Art URL → short MD5 hex key for cache busting (client fetches /media/art)
    let art_url = {
        let mut args = vec!["playerctl"];
        if let Some(p) = psel { args.extend_from_slice(&["-p", p]); }
        args.extend_from_slice(&["metadata", "mpris:artUrl"]);
        cmd_out(&args).await.trim().to_string()
    };
    let art_key = if art_url.is_empty() {
        String::new()
    } else {
        format!("{:x}", md5_short(&art_url))
    };

    // Position / duration
    let pos_raw = {
        let mut args = vec!["playerctl"];
        if let Some(p) = psel { args.extend_from_slice(&["-p", p]); }
        args.push("position");
        cmd_out(&args).await
    };
    let position_us = pos_raw.trim().parse::<f64>().ok().map(|s| (s * 1_000_000.0) as i64);
    let dur_raw = {
        let mut args = vec!["playerctl"];
        if let Some(p) = psel { args.extend_from_slice(&["-p", p]); }
        args.extend_from_slice(&["metadata", "mpris:length"]);
        cmd_out(&args).await
    };
    let duration_us = dur_raw.trim().parse::<i64>().ok().filter(|&v| v >= 0);

    let recording = recording_apps().await;

    AudioState {
        volume,
        sink_muted,
        mic_muted,
        sinks,
        sources,
        recording,
        now_playing: NowPlaying {
            status,
            title,
            player: player.unwrap_or_default(),
            art_key,
            position_us,
            duration_us,
        },
    }
}

// ── Controls ──────────────────────────────────────────────────────────────────

pub async fn set_volume(pct: i32) -> bool {
    let pct = pct.clamp(0, 150);
    cmd_fire(&["pactl", "set-sink-volume", "@DEFAULT_SINK@", &format!("{pct}%")]).await
}

pub async fn toggle_mute(target: &str) -> bool {
    if target == "mic" {
        cmd_fire(&["pactl", "set-source-mute", "@DEFAULT_SOURCE@", "toggle"]).await
    } else {
        cmd_fire(&["pactl", "set-sink-mute", "@DEFAULT_SINK@", "toggle"]).await
    }
}

pub async fn set_sink(node_id: u32) -> bool {
    cmd_fire(&["wpctl", "set-default", &node_id.to_string()]).await
}

pub async fn set_source(node_id: u32) -> bool {
    cmd_fire(&["wpctl", "set-default", &node_id.to_string()]).await
}

pub async fn media_action(action: &str) -> bool {
    if !["play-pause", "next", "previous"].contains(&action) { return false; }
    let all_players = players().await;
    let mut args = vec!["playerctl"];
    let player = pick_player(&all_players).map(|s| s.to_string());
    if let Some(ref p) = player { args.extend_from_slice(&["-p", p]); }
    args.push(action);
    cmd_fire(&args).await
}

pub async fn seek_position(position_us: i64) -> bool {
    let all_players = players().await;
    let player = pick_player(&all_players).map(|s| s.to_string());
    let secs = format!("{:.6}", (position_us as f64) / 1_000_000.0);
    let mut args = vec!["playerctl"];
    if let Some(ref p) = player { args.extend_from_slice(&["-p", p]); }
    args.extend_from_slice(&["position", &secs]);
    cmd_fire(&args).await
}

// ── Cover art ─────────────────────────────────────────────────────────────────

fn image_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) { return Some("image/jpeg"); }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") { return Some("image/png"); }
    if data.starts_with(b"GIF8") { return Some("image/gif"); }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") { return Some("image/webp"); }
    None
}

pub async fn art() -> Option<(String, Vec<u8>)> {
    let all_players = players().await;
    let player = pick_player(&all_players).map(|s| s.to_string());
    let mut args = vec!["playerctl"];
    if let Some(ref p) = player { args.extend_from_slice(&["-p", p]); }
    args.extend_from_slice(&["metadata", "mpris:artUrl"]);
    let url = cmd_out(&args).await.trim().to_string();
    if url.is_empty() { return None; }

    let data = if let Some(path) = url.strip_prefix("file://") {
        let path = urlencoding::decode(path).ok()?.into_owned();
        tokio::fs::read(&path).await.ok()?
    } else if url.starts_with("http://") || url.starts_with("https://") {
        // Use curl as a simple HTTP client (avoids reqwest dependency)
        let out = tokio::process::Command::new("curl")
            .args(["-s", "--max-filesize", "5000000", "-L", &url])
            .output()
            .await
            .ok()?;
        if out.status.success() { out.stdout } else { return None }
    } else {
        return None;
    };

    let ct = image_type(&data)?;
    Some((ct.to_string(), data))
}

// ── Utility ───────────────────────────────────────────────────────────────────

fn md5_short(s: &str) -> u64 {
    // Simple FNV-1a for a short hash key (not cryptographic)
    let mut hash = 14695981039346656037u64;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

// urlencoding: simple percent-decode
mod urlencoding {
    pub fn decode(s: &str) -> Result<std::borrow::Cow<'_, str>, ()> {
        if !s.contains('%') { return Ok(s.into()); }
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hi = from_hex(bytes[i + 1])?;
                let lo = from_hex(bytes[i + 2])?;
                out.push((hi << 4 | lo) as char);
                i += 3;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        Ok(out.into())
    }
    fn from_hex(b: u8) -> Result<u8, ()> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(()),
        }
    }
}
