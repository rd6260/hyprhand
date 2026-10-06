//! Auth: PIN hashing (argon2), JWT issue/verify, cookie middleware, lockout.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Context;
use argon2::{
    password_hash::{PasswordHash, PasswordVerifier, SaltString},
    Argon2, PasswordHasher,
};
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum_extra::extract::CookieJar;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::config;
use crate::state::AppState;

const COOKIE_NAME: &str = "hyprhand_session";

// ── Secrets file ──────────────────────────────────────────────────────────────

fn secrets_path() -> std::path::PathBuf {
    config::config_dir().join("secrets.json")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Secrets {
    pub jwt_key: String,
    /// argon2 hash of the PIN, or None if not set yet.
    pub pin_hash: Option<String>,
}

/// Load or create `~/.config/hyprhand/secrets.json` (mode 0600).
pub fn load_or_create_secrets() -> anyhow::Result<Secrets> {
    let path = secrets_path();

    // Ensure config dir exists.
    let dir = config::config_dir();
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }

    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        return serde_json::from_str(&raw)
            .with_context(|| format!("{} is not valid JSON", path.display()));
    }

    // Generate a fresh JWT key.
    let mut key_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut key_bytes);
    let jwt_key = hex::encode(&key_bytes);

    let secrets = Secrets { jwt_key, pin_hash: None };
    save_secrets(&secrets)?;
    tracing::info!("created new secrets at {}", path.display());
    Ok(secrets)
}

fn save_secrets(secrets: &Secrets) -> anyhow::Result<()> {
    let path = secrets_path();
    let text = serde_json::to_string_pretty(secrets)?;
    std::fs::write(&path, &text)?;
    // chmod 0600
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

// ── PIN management ────────────────────────────────────────────────────────────

/// Hash a PIN with argon2 and store it in secrets.json.
pub fn set_pin(pin: &str) -> anyhow::Result<()> {
    let mut secrets = load_or_create_secrets()?;
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(pin.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?
        .to_string();
    secrets.pin_hash = Some(hash);
    save_secrets(&secrets)?;
    Ok(())
}

/// Returns true if `pin` matches the stored hash.
pub fn verify_pin(pin: &str) -> bool {
    let path = secrets_path();
    let Ok(raw) = std::fs::read_to_string(&path) else { return false };
    let Ok(secrets) = serde_json::from_str::<Secrets>(&raw) else { return false };
    let Some(hash_str) = secrets.pin_hash else { return false };
    let Ok(hash) = PasswordHash::new(&hash_str) else { return false };
    Argon2::default()
        .verify_password(pin.as_bytes(), &hash)
        .is_ok()
}

// ── JWT ───────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    iat: u64,
    exp: u64,
}

pub fn issue_token(jwt_key: &str) -> anyhow::Result<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let exp = now + 168 * 3600; // 7 days default
    let claims = Claims { sub: "hyprhand".into(), iat: now, exp };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(jwt_key.as_bytes()),
    )
    .context("JWT encode failed")
}

pub fn verify_token(token: &str, jwt_key: &str) -> bool {
    let mut validation = Validation::default();
    validation.set_required_spec_claims(&["sub", "exp"]);
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(jwt_key.as_bytes()),
        &validation,
    )
    .is_ok()
}

pub fn make_session_cookie(token: String) -> axum_extra::extract::cookie::Cookie<'static> {
    use axum_extra::extract::cookie::{Cookie, SameSite};
    Cookie::build((COOKIE_NAME, token))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .path("/")
        .build()
}

// ── Lockout ───────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct AuthState {
    inner: Mutex<AuthInner>,
}

#[derive(Default)]
struct AuthInner {
    failures: u32,
    locked_until: Option<Instant>,
}

impl AuthState {
    /// Returns seconds remaining in the lockout, 0.0 if not locked.
    pub fn lock_remaining(&self) -> f64 {
        let inner = self.inner.lock().unwrap();
        if let Some(until) = inner.locked_until {
            let remaining = until.saturating_duration_since(Instant::now());
            remaining.as_secs_f64()
        } else {
            0.0
        }
    }

    pub fn record_failure(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.failures += 1;
        if inner.failures >= 5 {
            let extra = inner.failures - 5;
            let delay = std::cmp::min(30u64 * (2u64.saturating_pow(extra)), 900);
            inner.locked_until = Some(Instant::now() + Duration::from_secs(delay));
        }
    }

    pub fn record_success(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.failures = 0;
        inner.locked_until = None;
    }
}

// ── Middleware ────────────────────────────────────────────────────────────────

/// Axum middleware: check session cookie on every protected route (HTTP + WS upgrades).
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let jwt_key = &state.secrets.jwt_key;
    let token = jar
        .get(COOKIE_NAME)
        .map(|c| c.value().to_owned());

    if token.as_deref().map(|t| verify_token(t, jwt_key)) == Some(true) {
        return next.run(req).await;
    }

    // Redirect HTML requests to /login, return 401 for API/WS.
    let accept = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if accept.contains("text/html") {
        axum::response::Redirect::to("/login").into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "auth required").into_response()
    }
}

/// Helper for checking auth on a WebSocket upgrade request (before calling on_upgrade).
pub fn ws_authed(jar: &CookieJar, jwt_key: &str) -> bool {
    jar.get(COOKIE_NAME)
        .map(|c| verify_token(c.value(), jwt_key))
        .unwrap_or(false)
}

// Needed for hex encoding (we use rand + manual hex).
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
