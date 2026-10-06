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
