//! Configuration: load/create `~/.config/hyprhand/hyprhand.json`.
//!
//! Rules:
//! - Auto-create with defaults if missing (mode 0700 on the directory).
//! - Parse with `#[serde(default)]` so older files keep working when fields are added.
//! - Never silently overwrite an existing file on startup; fail loudly on bad JSON.
//! - Saves atomically: write to `.tmp`, copy old to `.bak`, rename `.tmp` → target.

use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

// ── Paths ─────────────────────────────────────────────────────────────────────

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .expect("cannot determine $XDG_CONFIG_HOME / home directory")
        .join("hyprhand")
}

pub fn config_path() -> PathBuf {
    config_dir().join("hyprhand.json")
}

// ── Schema ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(default)]
pub struct Config {
    pub server: ServerCfg,
    pub auth: AuthCfg,
    pub theme: ThemeCfg,
    pub state: StateCfg,
    pub commands: Vec<Command>,
    pub modes: Vec<Mode>,
    pub context: ContextCfg,
    pub fleet: FleetCfg,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ServerCfg {
    pub bind: String,
    pub port: u16,
    /// If set, serve static files from this directory instead of embedded copy.
    pub web_dir: Option<String>,
}
impl Default for ServerCfg {
    fn default() -> Self {
        Self { bind: "127.0.0.1".into(), port: 8765, web_dir: None }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct AuthCfg {
    pub session_hours: u64,
    pub max_attempts: u32,
    pub lockout_base_seconds: u64,
}
impl Default for AuthCfg {
    fn default() -> Self {
        Self { session_hours: 168, max_attempts: 5, lockout_base_seconds: 30 }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ThemeCfg {
    /// "green" | "amber" | "ice" | "auto"
    pub profile: String,
    pub matugen_palette: Option<String>,
    pub ambient: AmbientCfg,
}
impl Default for ThemeCfg {
    fn default() -> Self {
        Self { profile: "green".into(), matugen_palette: None, ambient: AmbientCfg::default() }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct AmbientCfg {
    pub idle_min: f64,
    pub liturgy: bool,
}
impl Default for AmbientCfg {
    fn default() -> Self {
        Self { idle_min: 2.0, liturgy: true }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct StateCfg {
    pub fast_interval_ms: u64,
    pub slow_interval_ms: u64,
}
impl Default for StateCfg {
    fn default() -> Self {
        Self { fast_interval_ms: 2000, slow_interval_ms: 5000 }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Command {
    pub label: String,
    pub cmd: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Mode {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Step {
    Launch {
        cmd: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        r#match: Option<String>,
        #[serde(default)]
        once: bool,
    },
    Close {
        r#match: String,
        #[serde(default)]
        workspaces: Vec<i32>,
    },
    Move {
        r#match: String,
        workspace: i32,
    },
    Focus {
        workspace: i32,
    },
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(default)]
pub struct ContextCfg {
    /// app class → per-app control config
    pub apps: std::collections::HashMap<String, ContextApp>,
    /// app binaries / classes that indicate a call is in progress
    pub call_apps: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContextApp {
    pub label: String,
    /// "persist" = show whenever window exists; "focus" = only when focused
    pub show: String,
    #[serde(default)]
    pub controls: Vec<ContextControl>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContextControl {
    pub label: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mods: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
#[serde(default)]
pub struct FleetCfg {
    pub hosts: Vec<FleetHost>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FleetHost {
    pub name: String,
    pub label: Option<String>,
    pub url: String,
}

// ── Load / save ───────────────────────────────────────────────────────────────

/// Load the config from disk, creating it with defaults if it doesn't exist.
///
/// On parse error: exits with a clear message pointing to the offending file.
/// Never silently resets user settings.
pub fn load_or_create() -> anyhow::Result<Config> {
    let dir = config_dir();

    // Create the directory with mode 0700 if missing.
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("Failed to create config directory {}", dir.display()))?;
    }

    let path = config_path();
    if !path.exists() {
        let cfg = Config::default();
        let text = serde_json::to_string_pretty(&cfg)?;
        fs::write(&path, &text)
            .with_context(|| format!("Failed to write default config to {}", path.display()))?;
        tracing::info!("created default config at {}", path.display());
        return Ok(cfg);
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Cannot read {}", path.display()))?;
    serde_json::from_str::<Config>(&raw).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON: {e}\nFix the file or delete it to regenerate defaults.",
            path.display()
        )
    })
}

/// Atomically write a config to disk.
///
/// Steps:
/// 1. Validate (serialize round-trip).
/// 2. Write to `hyprhand.json.tmp`.
/// 3. Rename old file to `hyprhand.json.bak`.
/// 4. Rename `.tmp` → `hyprhand.json`.
pub fn save(cfg: &Config) -> anyhow::Result<()> {
    let dir = config_dir();
    let path = config_path();
    let tmp = dir.join("hyprhand.json.tmp");
    let bak = dir.join("hyprhand.json.bak");

    let text = serde_json::to_string_pretty(cfg)?;
    fs::write(&tmp, &text).with_context(|| format!("write tmp {}", tmp.display()))?;

    if path.exists() {
        fs::rename(&path, &bak).with_context(|| format!("rename to bak {}", bak.display()))?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("rename tmp → config {}", path.display()))?;

    tracing::debug!("config saved to {}", path.display());
    Ok(())
}
