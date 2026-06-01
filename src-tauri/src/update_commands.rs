//! Tauri command wrappers for the Settings → Updates panel.
//!
//! Thin delegates to `myo_self_update` — the engine does all the work; these
//! just adapt its `Result`/serde types to the Tauri command boundary.

use myo_self_update as su;

#[tauri::command]
pub fn update_status() -> Result<su::UpdateStatus, String> {
    su::status().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_check_now() -> Result<su::CheckOutcome, String> {
    su::check_now().await.map_err(|e| e.to_string())
}

/// Apply strictly so a swap failure surfaces BEFORE we relaunch — otherwise the
/// user sees the old version after "restart" and assumes it worked.
#[tauri::command]
#[allow(unreachable_code)] // `app.restart()` diverges on Tauri builds that type it as `!`.
pub fn update_apply_now(app: tauri::AppHandle) -> Result<(), String> {
    su::apply_pending_strict().map_err(|e| e.to_string())?;
    app.restart();
    Ok(())
}

#[tauri::command]
pub fn update_set_enabled(enabled: bool) -> Result<su::UpdateStatus, String> {
    su::set_enabled(enabled).map_err(|e| e.to_string())?;
    su::status().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_leftovers_list() -> Vec<su::UpdateLeftover> {
    su::list_update_leftovers()
}

#[tauri::command]
pub fn update_leftovers_clear() -> u64 {
    su::clear_update_leftovers()
}
