//! Engine process supervision — the OS-facing half of `myo_core::supervisor`.
//!
//! Resolves where the engines live, spawns them (kill-on-Drop via
//! [`EngineChild`]), polls them healthy, and points the brain at the model
//! engine — narrating the whole thing on `myo://engine`. Ported in spirit from
//! MyOwnLLM's `mesh/daemon.rs` probe-then-spawn-then-poll flow.
//!
//! Engine locations are resolved from env first (so dev and CI can point at an
//! existing checkout), then bundled-resource conventions, then `PATH`:
//!   * `MYO_ODYSSEUS_DIR` — the Odysseus source tree (contains `app.py`)
//!   * `MYO_UVICORN`      — the uvicorn binary (default: the managed venv)
//!   * `MYO_MYOWNLLM_BIN` — the `myownllm` binary

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tauri::AppHandle;

use myo_core::supervisor as specs;
use myo_core::MyoEvent;

use crate::events::emit;
use crate::state::{EngineChild, MyoState};

/// How long to wait for an engine we just spawned to report healthy.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(45);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Bring both engines up (if not already), then wire the brain to the model
/// engine and apply the persisted capability allowlist. Idempotent: safe to
/// call again — already-healthy engines are left alone.
pub async fn ensure_ready(app: AppHandle, state: Arc<MyoState>) {
    start_odysseus(&app, &state).await;
    start_myownllm(&app, &state).await;

    // Pre-pay the ASR engine's cold start (onnxruntime + model download/load)
    // in the background so Myo's first spoken words don't wait on it. Myo is
    // always listening, so she'll want her ears warm. Best-effort: it logs and
    // gives up if the model engine isn't serving yet.
    {
        let st = state.clone();
        let health_url = format!("{}/healthz", specs::myownllm_base_url());
        tauri::async_runtime::spawn(async move {
            if specs::endpoint_reachable(&health_url).await {
                let _ = st.asr.warm_up().await;
            }
        });
    }

    // Auto-config (PLAN step 6): point the brain at MyOwnLLM and push the
    // current capability allowlist. Both need a healthy brain.
    if state.brain.health().await.unwrap_or(false) {
        match specs::ensure_myownllm_registered(&state.brain).await {
            Ok(registered) => emit(
                &app,
                engine(
                    "model",
                    "ready",
                    Some(if registered {
                        "registered MyOwnLLM as the default endpoint"
                    } else {
                        "MyOwnLLM already registered"
                    }),
                ),
            ),
            Err(e) => emit(&app, engine("model", "error", Some(&e.to_string()))),
        }

        let disabled = {
            let s = state.settings.lock().unwrap();
            s.capabilities.disabled_tools()
        };
        let _ = state.brain.set_disabled_tools(&disabled).await;
    }
}

async fn start_odysseus(app: &AppHandle, state: &Arc<MyoState>) {
    emit(app, engine("odysseus", "checking", None));
    if state.brain.health().await.unwrap_or(false) {
        emit(app, engine("odysseus", "healthy", None));
        return;
    }
    let Some(dir) = resolve_odysseus_dir() else {
        emit(
            app,
            engine(
                "odysseus",
                "unavailable",
                Some("Odysseus source not found — set MYO_ODYSSEUS_DIR to its checkout"),
            ),
        );
        return;
    };

    emit(app, engine("odysseus", "starting", None));
    let spec = specs::odysseus_spec(resolve_uvicorn(), dir, &state.token);
    if let Err(e) = spawn_into(state, &spec) {
        emit(app, engine("odysseus", "error", Some(&e.to_string())));
        return;
    }

    if wait_for(STARTUP_TIMEOUT, || async {
        state.brain.health().await.unwrap_or(false)
    })
    .await
    {
        emit(app, engine("odysseus", "healthy", None));
    } else {
        emit(
            app,
            engine(
                "odysseus",
                "timeout",
                Some("did not become healthy in time"),
            ),
        );
    }
}

async fn start_myownllm(app: &AppHandle, state: &Arc<MyoState>) {
    let health_url = format!("{}/healthz", specs::myownllm_base_url());
    emit(app, engine("myownllm", "checking", None));
    if specs::endpoint_reachable(&health_url).await {
        emit(app, engine("myownllm", "healthy", None));
        return;
    }

    emit(app, engine("myownllm", "starting", None));
    let spec = specs::myownllm_spec(resolve_myownllm());
    if let Err(e) = spawn_into(state, &spec) {
        emit(app, engine("myownllm", "error", Some(&e.to_string())));
        return;
    }

    if wait_for(STARTUP_TIMEOUT, || async {
        specs::endpoint_reachable(&health_url).await
    })
    .await
    {
        emit(app, engine("myownllm", "healthy", None));
    } else {
        emit(
            app,
            engine("myownllm", "timeout", Some("did not start serving in time")),
        );
    }
}

/// Spawn an engine and stash its kill-on-Drop handle in the shared state.
fn spawn_into(state: &Arc<MyoState>, spec: &specs::EngineSpec) -> Result<()> {
    let child = spawn(spec)?;
    state.children.lock().unwrap().push(child);
    Ok(())
}

fn spawn(spec: &specs::EngineSpec) -> Result<EngineChild> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    // Inherit stdio so engine logs land in Myo's console; detach stdin.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {} ({})", spec.name, spec.program))?;
    Ok(EngineChild::new(&spec.name, child))
}

/// Poll `check` every [`POLL_INTERVAL`] until it returns true or the deadline
/// passes. The closure returns a future so it can do async health checks.
async fn wait_for<F, Fut>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        if check().await {
            return true;
        }
    }
    false
}

fn engine(name: &str, status: &str, detail: Option<&str>) -> MyoEvent {
    MyoEvent::Engine {
        name: name.to_string(),
        status: status.to_string(),
        detail: detail.map(str::to_string),
    }
}

fn resolve_uvicorn() -> String {
    if let Ok(p) = std::env::var("MYO_UVICORN") {
        return p;
    }
    if let Some(home) = dirs::home_dir() {
        let venv = home
            .join(".myo")
            .join("odysseus-venv")
            .join("bin")
            .join("uvicorn");
        if venv.exists() {
            return venv.to_string_lossy().into_owned();
        }
    }
    "uvicorn".to_string()
}

fn resolve_odysseus_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MYO_ODYSSEUS_DIR") {
        return Some(PathBuf::from(p));
    }
    // Bundled under the app's resources, or a sibling dev checkout.
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("resources").join("odysseus"));
        }
    }
    candidates.push(PathBuf::from("../odysseus"));
    candidates.into_iter().find(|c| c.join("app.py").exists())
}

fn resolve_myownllm() -> String {
    if let Ok(p) = std::env::var("MYO_MYOWNLLM_BIN") {
        return p;
    }
    let exe_name = if cfg!(windows) {
        "myownllm.exe"
    } else {
        "myownllm"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join(exe_name);
            if bundled.exists() {
                return bundled.to_string_lossy().into_owned();
            }
        }
    }
    "myownllm".to_string()
}
