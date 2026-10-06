//! Theme: serve `/theme.css` dynamically from the active profile.
//!
//! Profiles: `green`, `amber`, `ice`, `auto`.
//! `auto` reads the matugen-generated palette file (path from config).

use std::sync::Arc;

use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};

use crate::state::AppState;

// ── Static palette definitions ────────────────────────────────────────────────

const GREEN_VARS: &str = r"
  --p-primary: #33ffa0;
  --p-secondary: #ffb000;
  --p-on: #e4f7ed;
  --p-on-primary: #001a0d;
  --p-on-var: #7fa98e;
  --p-bg: #000;
  --p-surface: #0a0f0c;
  --p-high: #111a14;
  --p-low: #060d09;
  --p-outline: rgba(51,255,160,.18);
";

const AMBER_VARS: &str = r"
  --p-primary: #ffb000;
  --p-secondary: #33ffa0;
  --p-on: #f7eddb;
  --p-on-primary: #1a0d00;
  --p-on-var: #a98e50;
  --p-bg: #000;
  --p-surface: #0f0d0a;
  --p-high: #1a1510;
  --p-low: #0d0a06;
  --p-outline: rgba(255,176,0,.18);
";

const ICE_VARS: &str = r"
  --p-primary: #7ac7ff;
  --p-secondary: #ff8fa0;
  --p-on: #d6ecff;
  --p-on-primary: #001a30;
  --p-on-var: #6090b0;
  --p-bg: #000;
  --p-surface: #090e14;
  --p-high: #101820;
  --p-low: #060a10;
  --p-outline: rgba(122,199,255,.18);
";

// ── Skin (structural CSS, shared across all profiles) ─────────────────────────

const SKIN: &str = include_str!("skin.css");

// ── Handler ───────────────────────────────────────────────────────────────────

pub async fn serve_css(State(st): State<Arc<AppState>>) -> Response {
    let cfg = st.config.load();
    let profile = cfg.theme.profile.as_str();
    let css = build_css(profile, cfg.theme.matugen_palette.as_deref());
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        css,
    )
        .into_response()
}

pub fn build_css(profile: &str, matugen_path: Option<&str>) -> String {
    let vars = match profile {
        "amber" => AMBER_VARS.to_string(),
        "ice" => ICE_VARS.to_string(),
        "auto" => read_auto_vars(matugen_path),
        _ => GREEN_VARS.to_string(), // green is the default
    };
    format!(":root{{{vars}}}\n{SKIN}")
}

fn read_auto_vars(path: Option<&str>) -> String {
    let Some(p) = path else { return GREEN_VARS.to_string() };
    match std::fs::read_to_string(p) {
        Ok(content) => content,
        Err(_) => {
            tracing::warn!("Cannot read matugen palette at {p}, falling back to green");
            GREEN_VARS.to_string()
        }
    }
}
