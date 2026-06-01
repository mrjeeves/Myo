//! `myo-self-update` — Myo's set-it-and-forget-it self-updater.
//!
//! Ported from MyOwnLLM's battle-tested `self_update.rs`: a custom GitHub
//! releases updater (not the Tauri updater plugin) that downloads the
//! per-platform binary, verifies its SHA-256, and atomically swaps it in —
//! "stage now, apply on next launch."
//!
//! The crate is intentionally Tauri-agnostic. The serde-friendly [`status`],
//! [`check_now`], [`set_enabled`] and [`apply_pending_strict`] entry points are
//! exactly what a future Settings → Updates panel's Tauri commands wrap; the
//! [`watcher`] drives the background cadence; and [`cmd_update`] backs the
//! `myo update` CLI.
//!
//! Typical wiring in the `myo` binary:
//! ```no_run
//! # async fn run() {
//! // First thing every launch: apply any update staged by a previous run.
//! myo_self_update::apply_pending_if_any();
//! // Then keep checking in the background.
//! myo_self_update::watcher::spawn_background();
//! # }
//! ```

pub mod config;
pub mod fsutil;
pub mod paths;
pub mod process;
mod time;
mod update;
pub mod watcher;

pub use update::{
    apply_pending_if_any, apply_pending_strict, check_now, clear_update_leftovers, cmd_update,
    detect_install_kind, force_check, list_update_leftovers, set_enabled, status, tick,
    ApplyPolicy, CheckOutcome, InstallKind, PendingUpdate, UpdateLeftover, UpdateStatus,
};

/// The version this binary/library was compiled at. The `myo` binary and this
/// crate share the workspace version, so this is the app's running version and
/// the single point to change if they're ever decoupled.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
