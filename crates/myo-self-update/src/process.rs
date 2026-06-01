//! Subprocess spawn helper.
//!
//! On Windows, a GUI-subsystem parent has no console, so each spawned child
//! opens its own — a black window flashes for every `tar` invocation the
//! updater makes. `CREATE_NO_WINDOW` (0x0800_0000) tells Windows not to
//! allocate a console for the child while still letting it inherit our stdio
//! handles, so captured output (`.output()`) keeps working. No-op on Unix.

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Drop-in replacement for `std::process::Command::new` that does not flash a
/// console window on Windows.
pub fn quiet_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    apply_quiet_flags(&mut cmd);
    cmd
}

#[cfg(target_os = "windows")]
fn apply_quiet_flags(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn apply_quiet_flags(_cmd: &mut std::process::Command) {}
