//! Android share target handler.
//!
//! The service worker intercepts POST /share and forwards it to POST /share/api
//! (authenticated, same-origin). Files go to ~/hyprhand-drop; URLs open in
//! xdg-open; plain text goes to wl-copy.

use axum::extract::Multipart;
use serde_json::Value;

use crate::{clipboard, grab};

pub async fn handle_multipart(mut multipart: Multipart) -> Value {
    let mut url = String::new();
    let mut text = String::new();
    let mut files: Vec<(String, Vec<u8>)> = vec![];

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "url" => url = field.text().await.unwrap_or_default(),
            "text" => text = field.text().await.unwrap_or_default(),
            "files" | "file" => {
                let filename = field.file_name().unwrap_or("upload").to_string();
                let data = field.bytes().await.unwrap_or_default().to_vec();
                files.push((filename, data));
            }
            _ => {}
        }
    }

    if !files.is_empty() {
        let mut names = vec![];
        for (name, data) in &files {
            let r = grab::save_upload(name, data);
            if r["ok"].as_bool() == Some(true) {
                names.push(r["name"].as_str().unwrap_or("").to_string());
            }
        }
        if names.is_empty() {
            return serde_json::json!({ "ok": false, "action": "save", "detail": "save failed" });
        }
        // If it's a single image, also copy to clipboard
        if files.len() == 1 {
            let (_, data) = &files[0];
            if is_image(data) {
                let _ = write_to_clipboard(data, image_mime(data)).await;
            }
        }
        let detail = if names.len() == 1 {
            format!("saved {}", names[0])
        } else {
            format!("saved {} files", names.len())
        };
        return serde_json::json!({ "ok": true, "action": "save", "detail": detail });
    }

    // URL or text classification
    match classify(&url, &text) {
        None => serde_json::json!({ "ok": false, "action": "none", "detail": "empty share" }),
        Some(("open", href)) => {
            let ok = xdg_open(&href).await;
            serde_json::json!({ "ok": ok, "action": "open",
                "detail": if ok { "opened on rig" } else { "open failed" } })
        }
        Some(("copy", payload)) => {
            let r = clipboard::write(&payload).await;
            let ok = r["ok"].as_bool().unwrap_or(false);
            serde_json::json!({ "ok": ok, "action": "copy",
                "detail": if ok { "→ rig clipboard" } else { "wl-copy failed" } })
        }
        Some(_) => unreachable!(),
    }
}

fn classify<'a>(url: &'a str, text: &'a str) -> Option<(&'static str, String)> {
    let url = url.trim();
    let text = text.trim();

    for cand in [url, text] {
        if cand.starts_with("http://") || cand.starts_with("https://") {
            return Some(("open", cand.to_string()));
        }
    }
    let payload = if !text.is_empty() { text } else { url };
    if payload.is_empty() { None } else { Some(("copy", payload.to_string())) }
}

async fn xdg_open(url: &str) -> bool {
    tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        tokio::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .map(|s| s.success())
    .unwrap_or(true) // timeout = xdg-open may have taken over; assume ok
}

fn is_image(data: &[u8]) -> bool {
    image_mime(data).is_some()
}

fn image_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) { return Some("image/jpeg"); }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") { return Some("image/png"); }
    if data.starts_with(b"GIF8") { return Some("image/gif"); }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") { return Some("image/webp"); }
    None
}

async fn write_to_clipboard(data: &[u8], mime: Option<&str>) -> bool {
    let mut args = vec!["wl-copy"];
    let mime_str;
    if let Some(m) = mime {
        mime_str = m.to_string();
        args.extend_from_slice(&["-t", &mime_str]);
    }

    let mut child = match tokio::process::Command::new("wl-copy")
        .args(if let Some(m) = mime { vec!["-t", m] } else { vec![] })
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(data).await;
    }
    let _ = child.wait().await;
    true
}
