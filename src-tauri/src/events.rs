//! The one seam between `myo-core`'s Tauri-free [`MyoEvent`] stream and the
//! WebView: route each event to its `myo://` channel and emit it.

use myo_core::{Emit, MyoEvent};
use tauri::{AppHandle, Emitter};

/// Emit one normalized event to the frontend. Best-effort (an IPC failure mid-
/// session is unrecoverable and not worth surfacing — same posture as MyOwnLLM).
pub fn emit(app: &AppHandle, ev: MyoEvent) {
    let Emit { channel, payload } = ev.emit();
    let _ = app.emit(channel, payload);
}
