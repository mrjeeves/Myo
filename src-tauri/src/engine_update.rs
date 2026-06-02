//! Runtime self-healing of the model engine — the launch-time twin of
//! `build.rs`'s bundling.
//!
//! Bundling pins `myownllm` (see `.myownllm-rev`), but the engine Myo actually
//! *resolves* at runtime can still be stale: the common case is a build where
//! the pinned release wasn't downloadable (so the sidecar is a zero-byte stub),
//! leaving [`crate::supervisor::resolve_myownllm`] to fall through to an older
//! `myownllm` on `PATH` — which 404s the transcription/stream routes. Rather
//! than let that reach the user, [`ensure_pinned_engine`] fetches the pinned
//! release into a copy Myo owns (`~/.myo/engine`), once, before we spawn —
//! narrating it on `myo://engine`. If the fetch can't happen (offline, the tag
//! isn't released yet), it falls back to the resolved copy so startup still
//! proceeds and the normal error path applies.
//!
//! No new deps: the pin is stamped in by `build.rs` (`MYOWNLLM_PINNED_REV`), and
//! the download shells out to `curl` + `tar`/PowerShell exactly like `build.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::AppHandle;

use myo_core::engine::{host_platform, parse_version_token, release_asset_url, version_meets};
use myo_core::MyoEvent;

use crate::events::emit;

/// The pinned MyOwnLLM release tag, stamped in by `build.rs`. `None` when
/// `.myownllm-rev` was absent at build time — which disables self-update
/// (Myo behaves exactly as before).
const PINNED_REV: Option<&str> = option_env!("MYOWNLLM_PINNED_REV");

fn engine_event(status: &str, detail: Option<&str>) -> MyoEvent {
    MyoEvent::Engine {
        name: "myownllm".into(),
        status: status.into(),
        detail: detail.map(str::to_string),
    }
}

/// Resolve the engine binary to spawn, self-healing a stale local copy up to the
/// pinned version first. `candidate` is what [`resolve_myownllm`] found
/// (override / bundled sidecar / `PATH`). Never fails: on any trouble it returns
/// `candidate` so startup proceeds and a genuine problem still surfaces normally.
///
/// [`resolve_myownllm`]: crate::supervisor::resolve_myownllm
pub async fn ensure_pinned_engine(app: &AppHandle, candidate: &str) -> String {
    // No pin compiled in → nothing to enforce.
    let Some(pin) = PINNED_REV.map(str::trim).filter(|s| !s.is_empty()) else {
        return candidate.to_string();
    };

    // Already at (or past) the pin? Use it as-is — no probing the network.
    if candidate_meets_pin(candidate, pin) {
        return candidate.to_string();
    }

    // A pinned engine we fetched on an earlier run? (Named by tag, so its
    // presence means it *is* the pin.)
    if let Some(cached) = cached_engine_if_present(pin) {
        return cached;
    }

    // Self-update: fetch the pinned release into ~/.myo/engine, once.
    emit(
        app,
        engine_event(
            "updating",
            Some(&format!(
                "the local MyOwnLLM is older than {pin} — fetching the pinned engine…"
            )),
        ),
    );
    let pin_owned = pin.to_string();
    match tokio::task::spawn_blocking(move || fetch_pinned_engine(&pin_owned)).await {
        Ok(Ok(path)) => {
            emit(
                app,
                engine_event("updated", Some(&format!("fetched MyOwnLLM {pin}"))),
            );
            path
        }
        Ok(Err(e)) => {
            emit(
                app,
                engine_event(
                    "update-failed",
                    Some(&format!(
                        "couldn't fetch MyOwnLLM {pin} ({e}) — using the local copy for now"
                    )),
                ),
            );
            candidate.to_string()
        }
        Err(e) => {
            emit(
                app,
                engine_event(
                    "update-failed",
                    Some(&format!("engine fetch task failed: {e}")),
                ),
            );
            candidate.to_string()
        }
    }
}

/// Does `candidate` already satisfy the pin? An explicit `MYO_MYOWNLLM_BIN`
/// override is trusted (the developer chose it); otherwise we ask the binary its
/// `--version`. An engine we can't run/parse is treated as not meeting the pin
/// (so it gets replaced by the known-good one).
fn candidate_meets_pin(candidate: &str, pin: &str) -> bool {
    if std::env::var_os("MYO_MYOWNLLM_BIN").is_some() {
        return true;
    }
    match binary_version(candidate) {
        Some(have) => version_meets(&have, pin),
        None => false,
    }
}

/// `<bin> --version` → its version token, or `None` if it can't be run/parsed.
fn binary_version(bin: &str) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_version_token(&raw).map(str::to_string)
}

/// `~/.myo/engine`, created on demand.
fn engine_cache_dir() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".myo").join("engine");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn cached_engine_path(pin: &str) -> Option<PathBuf> {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    Some(engine_cache_dir()?.join(format!("myownllm-{pin}{suffix}")))
}

/// A previously fetched pinned engine, if present and a real executable.
fn cached_engine_if_present(pin: &str) -> Option<String> {
    let p = cached_engine_path(pin)?;
    validate_executable(&p)
        .ok()
        .map(|()| p.to_string_lossy().into_owned())
}

/// Download + extract the pinned release into the engine cache, returning the
/// installed path. Shells out to `curl` + `tar`/PowerShell (present on the
/// target OSes) — the runtime mirror of `build.rs::download_release_asset`.
fn fetch_pinned_engine(pin: &str) -> Result<String, String> {
    let platform = host_platform().ok_or_else(|| {
        format!(
            "no prebuilt MyOwnLLM for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let dest = cached_engine_path(pin).ok_or("no home directory for the engine cache")?;
    let cache_dir = dest.parent().ok_or("bad cache path")?.to_path_buf();
    let staging = cache_dir.join(format!(".staging-{pin}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("create staging dir: {e}"))?;

    let url = release_asset_url(pin, platform);
    let is_windows = !suffix.is_empty();
    let archive = staging.join(if is_windows {
        "engine.zip"
    } else {
        "engine.tar.gz"
    });

    let status = Command::new("curl")
        .args(["-fL", "--retry", "3", "-o"])
        .arg(&archive)
        .arg(&url)
        .status()
        .map_err(|e| format!("curl spawn failed: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("download failed ({status}) from {url}"));
    }

    let extract = if is_windows {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                    archive.display(),
                    staging.display()
                ),
            ])
            .output()
    } else {
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .output()
    };
    let out = extract.map_err(|e| format!("extract spawn failed: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "extract failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let extracted = staging.join(format!("myownllm{suffix}"));
    if let Err(e) = validate_executable(&extracted) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    // Install: rename within the cache dir (same filesystem), else copy.
    let _ = std::fs::remove_file(&dest);
    if std::fs::rename(&extracted, &dest).is_err() {
        std::fs::copy(&extracted, &dest).map_err(|e| format!("install copy: {e}"))?;
    }
    make_executable(&dest);
    let _ = std::fs::remove_dir_all(&staging);
    Ok(dest.to_string_lossy().into_owned())
}

/// Looks like a real executable? ELF / PE / Mach-O, ≥ 1 MiB — catches HTML
/// error pages, truncated downloads, and the zero-byte sidecar stub.
fn validate_executable(path: &Path) -> Result<(), String> {
    use std::io::Read;
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
    if meta.len() < 1_048_576 {
        return Err(format!(
            "{} is only {} bytes (< 1 MiB)",
            path.display(),
            meta.len()
        ));
    }
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .map_err(|e| format!("read magic {}: {e}", path.display()))?;
    let looks_pe = head[0..2] == *b"MZ";
    let looks_elf = head == [0x7f, b'E', b'L', b'F'];
    let looks_macho = matches!(
        u32::from_le_bytes(head),
        0xFEED_FACE | 0xFEED_FACF | 0xCAFE_BABE | 0xBEBA_FECA
    ) || matches!(
        u32::from_be_bytes(head),
        0xFEED_FACE | 0xFEED_FACF | 0xCAFE_BABE | 0xBEBA_FECA
    );
    if looks_pe || looks_elf || looks_macho {
        Ok(())
    } else {
        Err(format!(
            "{} doesn't look like an executable (first bytes {head:02x?})",
            path.display()
        ))
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
