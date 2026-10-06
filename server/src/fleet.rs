//! Fleet roster and per-host online status.
//! Configured in the main hyprhand.json under the `fleet` key.

use serde_json::Value;

use crate::config::Config;

pub async fn status(cfg: &Config) -> Vec<Value> {
    if cfg.fleet.hosts.is_empty() {
        return vec![];
    }

    // Get Tailscale peer online map.
    let online_map = tailscale_online_map().await;

    cfg.fleet.hosts
        .iter()
        .map(|h| {
            let online = online_map.get(&h.name).copied();
            serde_json::json!({
                "name": h.name,
                "label": h.label.as_deref().unwrap_or(&h.name),
                "url": h.url,
                "online": online,
            })
        })
        .collect()
}

async fn tailscale_online_map() -> std::collections::HashMap<String, bool> {
    let out = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        tokio::process::Command::new("tailscale")
            .args(["status", "--json"])
            .output(),
    )
    .await;

    let bytes = match out {
        Ok(Ok(o)) if o.status.success() => o.stdout,
        _ => return std::collections::HashMap::new(),
    };

    let data: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return std::collections::HashMap::new(),
    };

    let mut map = std::collections::HashMap::new();

    // Self is always online.
    if let Some(name) = data["Self"]["HostName"].as_str() {
        map.insert(name.to_string(), true);
    }

    if let Some(peers) = data["Peer"].as_object() {
        for peer in peers.values() {
            if let (Some(name), Some(online)) = (peer["HostName"].as_str(), peer["Online"].as_bool()) {
                map.insert(name.to_string(), online);
            }
        }
    }

    map
}
