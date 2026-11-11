//! Screenshot (rig → phone) via grim, and file drop (phone → rig).
//!
//! Uploads land in `~/hyprhand-drop`. Filenames are sanitised to basename only.

use std::path::Path;

use serde_json::Value;

pub async fn screenshot(monitor: &str) -> Option<Vec<u8>> {
    if monitor.is_empty() { return None; }

    let out = tokio::process::Command::new("grim")
        .args(["-o", monitor, "-"])
        .output()
        .await
        .ok()?;

    if out.status.success() && out.stdout.starts_with(b"\x89PNG") {
        Some(out.stdout)
    } else {
        None
    }
}

// ── Cameras (V4L2) ────────────────────────────────────────────────────────────

/// Video-capture nodes from sysfs as `(node, name)`, e.g. `("video0", "USB2.0 HD UVC WebCam")`.
/// UVC cams expose a second metadata node per camera; only `index == 0` is the capture node.
pub fn cameras() -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir("/sys/class/video4linux") else { return vec![] };
    let mut cams: Vec<(String, String)> = rd
        .flatten()
        .filter_map(|e| {
            let node = e.file_name().to_str()?.to_string();
            if !node.starts_with("video") { return None; }
            let p = e.path();
            let index = std::fs::read_to_string(p.join("index")).unwrap_or_default();
            if index.trim() != "0" { return None; }
            let name = std::fs::read_to_string(p.join("name")).unwrap_or_default();
            // UVC names often repeat themselves ("Cam: Cam") — keep the first half
            let name = name.trim().split(": ").next().unwrap_or("").trim().to_string();
            Some((node.clone(), if name.is_empty() { node } else { name }))
        })
        .collect();
    cams.sort_by(|a, b| a.0.cmp(&b.0));
    cams
}

/// Grab one PNG frame from a camera node (e.g. "video0") via ffmpeg.
pub async fn camera_snapshot(node: &str) -> Option<Vec<u8>> {
    // only accept nodes we actually listed — never pass arbitrary paths to ffmpeg
    if !cameras().iter().any(|(n, _)| n == node) { return None; }
    let dev = format!("/dev/{node}");
    let fut = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-f", "v4l2", "-i", &dev,
            // skip the first frames so auto-exposure / white balance can settle
            "-vf", "select=gte(n\\,10)", "-frames:v", "1",
            "-f", "image2pipe", "-c:v", "png", "-",
        ])
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), fut).await.ok()?.ok()?;
    if out.status.success() && out.stdout.starts_with(b"\x89PNG") {
        Some(out.stdout)
    } else {
        tracing::warn!("camera snapshot {dev} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        None
    }
}

pub fn save_upload(filename: &str, data: &[u8]) -> Value {
    let name = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    if name.is_empty() {
        return serde_json::json!({ "ok": false, "error": "bad filename" });
    }

    let drop_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("hyprhand-drop");

    if let Err(e) = std::fs::create_dir_all(&drop_dir) {
        return serde_json::json!({ "ok": false, "error": e.to_string() });
    }

    let dest = unique_path(&drop_dir.join(&name));
    match std::fs::write(&dest, data) {
        Ok(()) => serde_json::json!({
            "ok": true,
            "name": dest.file_name().unwrap().to_string_lossy(),
            "size": data.len()
        }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    }
}

fn unique_path(path: &Path) -> std::path::PathBuf {
    if !path.exists() { return path.to_owned(); }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let dir = path.parent().unwrap();
    let mut i = 1u32;
    loop {
        let name = if ext.is_empty() {
            format!("{stem}-{i}")
        } else {
            format!("{stem}-{i}.{ext}")
        };
        let candidate = dir.join(name);
        if !candidate.exists() { return candidate; }
        i += 1;
    }
}
