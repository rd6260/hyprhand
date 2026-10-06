//! Shared application state — one `AppState` instance lives behind `Arc`
//! and is cloned into every handler and background task.

use std::sync::Arc;

use arc_swap::ArcSwap;
use protocol::DeckState;
use tokio::sync::watch;

use crate::auth::{AuthState, Secrets};
use crate::config::Config;

// ── AppState ──────────────────────────────────────────────────────────────────

pub struct AppState {
    /// Hot-swappable config (updated by the config API and hot-reload watcher).
    pub config: Arc<ArcSwap<Config>>,
    /// Secrets (JWT key, PIN hash) — loaded once at startup; written by set-pin.
    pub secrets: Secrets,
    /// Auth lockout state.
    pub auth_state: AuthState,
    /// Live deck state broadcast channel.
    pub state_tx: watch::Sender<Arc<DeckState>>,
    pub state_rx: watch::Receiver<Arc<DeckState>>,
}

impl AppState {
    pub fn new(cfg: Config, secrets: Secrets) -> Self {
        let (state_tx, state_rx) = watch::channel(Arc::new(DeckState::default()));
        Self {
            config: Arc::new(ArcSwap::from_pointee(cfg)),
            secrets,
            auth_state: AuthState::default(),
            state_tx,
            state_rx,
        }
    }

    /// Publish a new state snapshot to all /ws subscribers.
    pub fn publish(&self, state: DeckState) {
        let _ = self.state_tx.send(Arc::new(state));
    }

    /// Get the current state snapshot.
    pub fn current_state(&self) -> Arc<DeckState> {
        self.state_rx.borrow().clone()
    }
}

// ── Background loops ──────────────────────────────────────────────────────────

/// Fast loop: audio + CPU/GPU telemetry (every 2 s by default).
pub async fn fast_loop(state: Arc<AppState>) {
    loop {
        let interval_ms = state.config.load().state.fast_interval_ms;
        tokio::time::sleep(tokio::time::Duration::from_millis(interval_ms)).await;

        let mut s = state.current_state().as_ref().clone();

        // Audio snapshot
        s.audio = crate::audio::snapshot().await;

        // Telemetry
        s.telemetry = crate::telemetry::snapshot();

        state.publish(s);
    }
}

/// Slow loop: sysinfo (Tailscale status, top processes) (every 5 s by default).
pub async fn slow_loop(state: Arc<AppState>) {
    loop {
        let interval_ms = state.config.load().state.slow_interval_ms;
        tokio::time::sleep(tokio::time::Duration::from_millis(interval_ms)).await;

        let mut s = state.current_state().as_ref().clone();
        s.sysinfo = crate::sysinfo::snapshot().await;

        state.publish(s);
    }
}
