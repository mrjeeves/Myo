//! The Dream-mode supervisor — the downtime-only loop that drives memory
//! consolidation.
//!
//! `myo-core` owns the *what* (which memories to compact/forget, and how); this
//! is the *when*. It polls gently, and only ever calls into the dreamer when the
//! system is in genuine downtime — no turn running and at least `idle_secs` since
//! the last activity (see [`MyoState::is_idle`]). Each `step` is one small, atomic
//! unit, so the loop is naturally resumable: the moment a turn arrives, the next
//! idle check fails and the dreamer stands down until the next quiet stretch.

use std::sync::Arc;
use std::time::Duration;

use tauri::AppHandle;

use myo_core::{MyoEvent, StepOutcome};

use crate::events::emit;
use crate::state::MyoState;

/// How long to rest after finding nothing to do (or hitting a transient error,
/// e.g. the engine still warming) before looking again.
const REST_SECS: u64 = 30;

/// Spawn the background dreamer. Runs for the life of the app on Tauri's runtime.
pub fn spawn(app: AppHandle, state: Arc<MyoState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            let cfg = state.dream_config();
            if !cfg.enabled {
                tokio::time::sleep(Duration::from_secs(REST_SECS)).await;
                continue;
            }

            // Pace the loop: this gap is both the poll interval (how soon we
            // notice downtime) and the gentle pause between successive dream
            // steps, so consolidation stays slow and unobtrusive.
            tokio::time::sleep(Duration::from_secs(cfg.step_interval_secs.max(2))).await;

            if !state.is_idle(cfg.idle_secs) {
                continue;
            }

            let mut sink = |ev: MyoEvent| emit(&app, ev);
            match myo_core::memory::dream::step(&state.memory, &state.llm, &cfg, &mut sink).await {
                // Did a unit of work; loop straight back (the sleep above paces it)
                // so a long quiet stretch keeps compacting, one small step at a time.
                Ok(StepOutcome::Worked) => {}
                // Nothing to compact right now: persist the salience stats that
                // recall has been accruing in RAM, then rest.
                Ok(StepOutcome::Idle) => {
                    let _ = state.memory.long_term().persist_stats();
                    tokio::time::sleep(Duration::from_secs(REST_SECS)).await;
                }
                // Engine not ready / transient failure — back off and retry.
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(REST_SECS)).await;
                }
            }
        }
    });
}
