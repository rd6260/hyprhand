//! Clipboard bridge: read/write the Wayland clipboard via wl-paste/wl-copy.

use serde_json::Value;

const TIMEOUT: u64 = 5;

async fn cmd_out(args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = tokio::process::Command::new(args[0]);
    cmd.args(&args[1..]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(TIMEOUT),
        async { cmd.output().await },
    )
    .await;

    match result {
        Ok(Ok(o)) if o.status.success() => Some(o.stdout),
        _ => None,
    }
}

async fn cmd_write(args: &[&str], input: &[u8]) -> bool {
    let mut child = match tokio::process::Command::new(args[0])
        .args(&args[1..])
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
        let _ = stdin.write_all(input).await;
    }

    tokio::time::timeout(
        tokio::time::Duration::from_secs(TIMEOUT),
        child.wait(),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .map(|s| s.success())
    .unwrap_or(false)
}

pub async fn read() -> Value {
    match cmd_out(&["wl-paste", "--no-newline"]).await {
        Some(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            serde_json::json!({ "ok": true, "text": text })
        }
        None => serde_json::json!({ "ok": false, "text": "", "error": "wl-paste failed or timeout" }),
    }
}

pub async fn write(text: &str) -> Value {
    let ok = cmd_write(&["wl-copy"], text.as_bytes()).await;
    if ok {
        serde_json::json!({ "ok": true })
    } else {
        serde_json::json!({ "ok": false, "error": "wl-copy failed" })
    }
}
