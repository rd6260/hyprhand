//! hyprhand server — main entry point, router assembly, startup sequence.

mod assets;
mod audio;
mod auth;
mod brightness;
mod clipboard;
mod config;
mod context;
mod fleet;
mod grab;
mod hid;
mod hypr;
mod modes;
mod share;
mod state;
mod sysinfo;
mod telemetry;
mod theme;
mod ws;

use std::sync::Arc;

use anyhow::Context as _;
use axum::{
    extract::{Multipart, Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Json, Redirect, Response},
    routing::{get, post},
    Router,
};
use axum_extra::extract::CookieJar;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tokio::net::TcpListener;
use tracing::info;

use state::AppState;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "hyprhand", about = "hyprhand — Hyprland remote control server")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Hash and store the login PIN.
    SetPin {
        #[arg(help = "The PIN to set (e.g. hyprhand set-pin 1234)")]
        pin: String,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hyprhand=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    if let Some(Command::SetPin { pin }) = cli.cmd {
        // Ensure config dir exists before writing secrets.
        let _ = config::load_or_create()?;
        auth::set_pin(&pin)?;
        info!("PIN updated successfully.");
        return Ok(());
    }

    let cfg = config::load_or_create()?;
    let secrets = auth::load_or_create_secrets()?;

    let app_state = Arc::new(AppState::new(cfg.clone(), secrets));

    // Background tasks
    tokio::spawn(hypr::watch_events(app_state.clone()));
    tokio::spawn(state::fast_loop(app_state.clone()));
    tokio::spawn(state::slow_loop(app_state.clone()));

    let app = build_router(app_state.clone());

    let bind = format!("{}:{}", cfg.server.bind, cfg.server.port);
    info!("hyprhand listening on http://{}", bind);
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("Failed to bind to {bind}"))?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Router ────────────────────────────────────────────────────────────────────

async fn redirect_on_unauth(req: Request, next: Next) -> Response {
    let wants_html = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("text/html"))
        .unwrap_or(false);
    let res = next.run(req).await;
    if wants_html && res.status() == StatusCode::UNAUTHORIZED {
        return Redirect::to("/login").into_response();
    }
    res
}

fn build_router(state: Arc<AppState>) -> Router {
    // Public routes — no auth
    let public = Router::new()
        .route("/login", get(login_page).post(login_handler))
        .route("/sw.js", get(assets::serve_sw))
        .route("/share", get(share_get).post(share_fallback))
        .route("/static/*path", get(assets::serve_static));

    // Protected routes — auth middleware applied
    let protected = Router::new()
        .route("/", get(assets::serve_index))
        // WebSocket
        .route("/ws", get(ws::ws_state))
        .route("/ws/input", get(ws::ws_input))
        // Hyprland
        .route("/hypr/workspace", post(hypr_workspace))
        .route("/hypr/dpms", post(hypr_dpms))
        .route("/hypr/focus", post(hypr_focus))
        .route("/hypr/move", post(hypr_move_window))
        // Audio
        .route("/audio/volume", post(audio_volume))
        .route("/audio/mute", post(audio_mute))
        .route("/audio/sink", post(audio_sink))
        .route("/audio/source", post(audio_source_route))
        .route("/media/{action}", post(media_action))
        .route("/media/seek", post(media_seek))
        .route("/media/art", get(media_art))
        // Brightness
        .route("/brightness", get(brightness_get).post(brightness_set))
        // Modes
        .route("/modes", get(modes_list))
        .route("/modes/run", post(modes_run))
        // Commands
        .route("/commands", get(commands_list))
        .route("/commands/run", post(commands_run))
        .route("/commands/save", post(commands_save))
        .route("/commands/delete", post(commands_delete))
        .route("/commands/move", post(commands_move))
        // Context strip
        .route("/context", get(context_get))
        .route("/context/key", post(context_key))
        // Config API
        .route("/api/config", get(config_read).put(config_write))
        // Theme
        .route("/theme.css", get(theme::serve_css))
        .route("/theme", get(theme_get).post(theme_post))
        // System
        .route("/sys/lock", post(sys_lock))
        .route("/sys/kill", post(sys_kill))
        // Files
        .route("/screenshot", get(screenshot_handler))
        .route("/cameras", get(cameras_handler))
        .route("/upload", post(upload_handler))
        // Clipboard
        .route("/clipboard", get(clipboard_read).post(clipboard_write))
        // Share (authenticated path from service worker)
        .route("/share/api", post(share_api))
        // HID availability probe
        .route("/input/available", get(input_available))
        // Fleet
        .route("/fleet", get(fleet_status))
        // WebRTC stubs (placeholder)
        .route("/audio/webrtc/offer", post(webrtc_offer_stub))
        .route("/audio/webrtc/stop", post(webrtc_stop_stub))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .layer(axum::middleware::from_fn(redirect_on_unauth));


    Router::new()
        .merge(public)
        .merge(protected)
        .with_state(state)
}

// ── Login ─────────────────────────────────────────────────────────────────────

async fn login_page() -> Response {
    Html(assets::login_html()).into_response()
}

#[derive(Deserialize)]
struct LoginBody {
    pin: String,
}

async fn login_handler(
    State(st): State<Arc<AppState>>,
    jar: CookieJar,
    axum::Json(body): axum::Json<LoginBody>,
) -> Response {
    let remaining = st.auth_state.lock_remaining();
    if remaining > 0.0 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "ok": false, "error": format!("locked for {:.0}s", remaining) })),
        )
            .into_response();
    }

    if auth::verify_pin(&body.pin) {
        st.auth_state.record_success();
        let token = match auth::issue_token(&st.secrets.jwt_key) {
            Ok(t) => t,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
                )
                    .into_response()
            }
        };
        let cookie = auth::make_session_cookie(token);
        return (jar.add(cookie), Json(serde_json::json!({ "ok": true }))).into_response();
    }

    st.auth_state.record_failure();
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "ok": false, "error": "invalid PIN" })),
    )
        .into_response()
}

// ── Hyprland handlers ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WorkspaceBody { id: i32 }
async fn hypr_workspace(State(_): State<Arc<AppState>>, Json(b): Json<WorkspaceBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": hypr::focus_workspace(b.id).await }))
}

#[derive(Deserialize)]
struct DpmsBody { monitor: String, on: bool }
async fn hypr_dpms(State(_): State<Arc<AppState>>, Json(b): Json<DpmsBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": hypr::set_dpms(&b.monitor, b.on).await }))
}

#[derive(Deserialize)]
struct FocusBody { address: String }
async fn hypr_focus(State(_): State<Arc<AppState>>, Json(b): Json<FocusBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": hypr::focus_window(&b.address).await }))
}

#[derive(Deserialize)]
struct MoveBody { address: String, workspace: i32 }
async fn hypr_move_window(State(_): State<Arc<AppState>>, Json(b): Json<MoveBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": hypr::move_window_to_workspace_by_address(&b.address, b.workspace).await }))
}

// ── Audio ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VolumeBody { pct: i32 }
async fn audio_volume(State(_): State<Arc<AppState>>, Json(b): Json<VolumeBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": audio::set_volume(b.pct).await }))
}

#[derive(Deserialize)]
struct MuteBody { target: Option<String> }
async fn audio_mute(State(_): State<Arc<AppState>>, Json(b): Json<MuteBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": audio::toggle_mute(b.target.as_deref().unwrap_or("sink")).await }))
}

#[derive(Deserialize)]
struct SinkBody { id: u32 }
async fn audio_sink(State(_): State<Arc<AppState>>, Json(b): Json<SinkBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": audio::set_sink(b.id).await }))
}
async fn audio_source_route(State(_): State<Arc<AppState>>, Json(b): Json<SinkBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": audio::set_source(b.id).await }))
}

async fn media_action(State(_): State<Arc<AppState>>, Path(action): Path<String>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": audio::media_action(&action).await }))
}

#[derive(Deserialize)]
struct SeekBody { position_us: i64 }
async fn media_seek(State(_): State<Arc<AppState>>, Json(b): Json<SeekBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": audio::seek_position(b.position_us).await }))
}

async fn media_art(State(_): State<Arc<AppState>>) -> Response {
    match audio::art().await {
        Some((ct, data)) => (
            [
                (axum::http::header::CONTENT_TYPE, ct),
                (axum::http::header::CACHE_CONTROL, "max-age=3600".to_string()),
            ],
            data,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Brightness ────────────────────────────────────────────────────────────────

async fn brightness_get(State(_): State<Arc<AppState>>) -> impl IntoResponse {
    Json(brightness::levels().await)
}

#[derive(Deserialize)]
struct BrightnessBody { monitor: String, pct: i32 }
async fn brightness_set(State(_): State<Arc<AppState>>, Json(b): Json<BrightnessBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": brightness::set_level(&b.monitor, b.pct).await }))
}

// ── Modes ─────────────────────────────────────────────────────────────────────

async fn modes_list(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = st.config.load();
    let list: Vec<_> = cfg
        .modes
        .iter()
        .map(|m| serde_json::json!({ "id": m.name, "label": m.name }))
        .collect();
    Json(list)
}

#[derive(Deserialize)]
struct RunBody { id: String }
async fn modes_run(State(st): State<Arc<AppState>>, Json(b): Json<RunBody>) -> impl IntoResponse {
    let cfg = st.config.load();
    match cfg.modes.iter().find(|m| m.name == b.id).cloned() {
        None => Json(serde_json::json!({ "ok": false, "error": "unknown mode" })),
        Some(m) => Json(modes::run(&m).await),
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

async fn commands_list(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = st.config.load();
    let list: Vec<_> = cfg
        .commands
        .iter()
        .enumerate()
        .map(|(i, c)| serde_json::json!({ "id": i, "label": c.label, "cmd": c.cmd }))
        .collect();
    Json(list)
}

#[derive(Deserialize)]
struct CmdRunBody { id: usize }
/// Run a command via `sh -c`, detached into its own process group so GUI apps it
/// launches survive a server restart. Waits briefly so quick commands can report
/// their exit status; long-running ones (app launches) report "started".
async fn commands_run(State(st): State<Arc<AppState>>, Json(b): Json<CmdRunBody>) -> impl IntoResponse {
    let Some(cmd) = st.config.load().commands.get(b.id).map(|c| c.cmd.clone()) else {
        return Json(serde_json::json!({ "ok": false, "error": "unknown command" }));
    };
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    };
    match tokio::time::timeout(std::time::Duration::from_millis(1500), child.wait()).await {
        Ok(Ok(status)) if status.success() => Json(serde_json::json!({ "ok": true, "status": "done" })),
        Ok(Ok(status)) => Json(serde_json::json!({
            "ok": false,
            "error": format!("exited with {}", status.code().map_or("signal".into(), |c| c.to_string())),
        })),
        Ok(Err(e)) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        Err(_) => {
            // Still running: reap it in the background so it doesn't become a zombie.
            tokio::spawn(async move { let _ = child.wait().await; });
            Json(serde_json::json!({ "ok": true, "status": "started" }))
        }
    }
}

#[derive(Deserialize)]
struct CmdSaveBody { id: Option<usize>, label: String, cmd: String }
/// Create (no `id`) or update (`id`) a command.
async fn commands_save(State(st): State<Arc<AppState>>, Json(b): Json<CmdSaveBody>) -> impl IntoResponse {
    let (label, cmd) = (b.label.trim().to_string(), b.cmd.trim().to_string());
    if label.is_empty() || cmd.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "name and command are required" }));
    }
    let mut cfg = st.config.load().as_ref().clone();
    let entry = config::Command { label, cmd };
    match b.id {
        None => cfg.commands.push(entry),
        Some(i) if i < cfg.commands.len() => cfg.commands[i] = entry,
        Some(_) => return Json(serde_json::json!({ "ok": false, "error": "unknown command" })),
    }
    save_config(&st, cfg)
}

#[derive(Deserialize)]
struct CmdDeleteBody { id: usize }
async fn commands_delete(State(st): State<Arc<AppState>>, Json(b): Json<CmdDeleteBody>) -> impl IntoResponse {
    let mut cfg = st.config.load().as_ref().clone();
    if b.id >= cfg.commands.len() {
        return Json(serde_json::json!({ "ok": false, "error": "unknown command" }));
    }
    cfg.commands.remove(b.id);
    save_config(&st, cfg)
}

#[derive(Deserialize)]
struct CmdMoveBody { id: usize, dir: i32 }
/// Swap a command with its neighbour (`dir` = -1 up, +1 down).
async fn commands_move(State(st): State<Arc<AppState>>, Json(b): Json<CmdMoveBody>) -> impl IntoResponse {
    let mut cfg = st.config.load().as_ref().clone();
    let j = b.id as i64 + b.dir.signum() as i64;
    if b.id >= cfg.commands.len() || j < 0 || j as usize >= cfg.commands.len() {
        return Json(serde_json::json!({ "ok": false, "error": "cannot move" }));
    }
    cfg.commands.swap(b.id, j as usize);
    save_config(&st, cfg)
}

fn save_config(st: &AppState, cfg: config::Config) -> Json<serde_json::Value> {
    match config::save(&cfg) {
        Ok(()) => {
            st.config.store(Arc::new(cfg));
            Json(serde_json::json!({ "ok": true }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

// ── Context ───────────────────────────────────────────────────────────────────

async fn context_get(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    Json(context::listing(&st.config.load()))
}

#[derive(Deserialize)]
struct ContextKeyBody { cls: String, key: String, mods: Option<String> }
async fn context_key(State(_): State<Arc<AppState>>, Json(b): Json<ContextKeyBody>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": hypr::send_shortcut(&b.cls, &b.key, b.mods.as_deref().unwrap_or("")).await }))
}

// ── Config API ────────────────────────────────────────────────────────────────

async fn config_read(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    Json(st.config.load().as_ref().clone())
}

async fn config_write(
    State(st): State<Arc<AppState>>,
    Json(new_cfg): Json<config::Config>,
) -> impl IntoResponse {
    match config::save(&new_cfg) {
        Ok(()) => {
            st.config.store(Arc::new(new_cfg));
            Json(serde_json::json!({ "ok": true }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

// ── Theme ─────────────────────────────────────────────────────────────────────

async fn theme_get(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = st.config.load();
    Json(serde_json::json!({
        "profile": cfg.theme.profile,
        "ambient": cfg.theme.ambient,
    }))
}

#[derive(Deserialize)]
struct ThemeBody {
    profile: Option<String>,
    ambient: Option<serde_json::Value>,
}
async fn theme_post(State(st): State<Arc<AppState>>, Json(b): Json<ThemeBody>) -> impl IntoResponse {
    let mut cfg = st.config.load().as_ref().clone();
    if let Some(p) = b.profile {
        cfg.theme.profile = p;
    }
    if let Some(amb) = b.ambient {
        if let Ok(a) = serde_json::from_value(amb) {
            cfg.theme.ambient = a;
        }
    }
    match config::save(&cfg) {
        Ok(()) => {
            st.config.store(Arc::new(cfg));
            Json(serde_json::json!({ "ok": true }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

// ── System ────────────────────────────────────────────────────────────────────

async fn sys_lock(State(_): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": sysinfo::lock().await }))
}

#[derive(Deserialize)]
struct KillBody { pid: u32 }
async fn sys_kill(State(_): State<Arc<AppState>>, Json(b): Json<KillBody>) -> impl IntoResponse {
    Json(sysinfo::kill(b.pid))
}

// ── Files ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ScreenshotQuery { monitor: Option<String>, camera: Option<String> }
async fn screenshot_handler(
    State(_): State<Arc<AppState>>,
    Query(q): Query<ScreenshotQuery>,
) -> Response {
    let shot = match q.camera.as_deref() {
        Some(cam) => grab::camera_snapshot(cam).await,
        None => grab::screenshot(q.monitor.as_deref().unwrap_or("")).await,
    };
    match shot {
        Some(data) => ([("Content-Type", "image/png")], data).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn cameras_handler(State(_): State<Arc<AppState>>) -> impl IntoResponse {
    let list: Vec<_> = grab::cameras()
        .into_iter()
        .map(|(node, name)| serde_json::json!({ "node": node, "name": name }))
        .collect();
    Json(list)
}

async fn upload_handler(State(_): State<Arc<AppState>>, mut multipart: Multipart) -> impl IntoResponse {
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.file_name().unwrap_or("upload").to_string();
        let data = field.bytes().await.unwrap_or_default().to_vec();
        return Json(grab::save_upload(&name, &data));
    }
    Json(serde_json::json!({ "ok": false, "error": "no file" }))
}

// ── Clipboard ─────────────────────────────────────────────────────────────────

async fn clipboard_read(State(_): State<Arc<AppState>>) -> impl IntoResponse {
    Json(clipboard::read().await)
}

#[derive(Deserialize)]
struct ClipBody { text: String }
async fn clipboard_write(State(_): State<Arc<AppState>>, Json(b): Json<ClipBody>) -> impl IntoResponse {
    Json(clipboard::write(&b.text).await)
}

// ── Share ─────────────────────────────────────────────────────────────────────

async fn share_get() -> impl IntoResponse {
    axum::response::Redirect::to("/")
}

async fn share_fallback() -> impl IntoResponse {
    Html(r#"<body style="margin:0;height:100vh;display:flex;align-items:center;justify-content:center;background:#000;color:#ffb000;font-family:monospace;text-align:center;padding:0 24px">open the hyprhand app once (to arm its service worker), then share again</body>"#)
}

async fn share_api(State(_): State<Arc<AppState>>, multipart: Multipart) -> impl IntoResponse {
    Json(share::handle_multipart(multipart).await)
}

// ── Input availability ────────────────────────────────────────────────────────

async fn input_available(State(_): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({ "ok": hid::available() }))
}

// ── Fleet ─────────────────────────────────────────────────────────────────────

async fn fleet_status(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    Json(fleet::status(&st.config.load()).await)
}

// ── WebRTC stubs ──────────────────────────────────────────────────────────────

async fn webrtc_offer_stub() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": false, "error": "WebRTC not yet implemented" }))
}
async fn webrtc_stop_stub() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
}
