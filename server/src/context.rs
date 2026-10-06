//! Context strip: per-app keymaps and call-app detection.
//! Config is embedded in the main hyprhand.json under the `context` key.

use serde_json::Value;

use crate::config::Config;

pub fn listing(cfg: &Config) -> Value {
    serde_json::json!({
        "apps": cfg.context.apps,
        "call_apps": cfg.context.call_apps,
    })
}
