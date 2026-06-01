//! Engine supervision — *what* to launch and *how* to tell it's healthy.
//!
//! The actual child-process plumbing (spawn, kill-on-Drop, path resolution)
//! lives in the `myo` binary, where Tauri state and the OS live. This module
//! holds the Tauri-free, testable half: the launch [`EngineSpec`]s (program,
//! args, env, cwd, health URL), the loopback ports, the internal-auth token
//! minter, and the one-call brain↔MyOwnLLM wiring.
//!
//! Ports: Myo owns *both* engines on **private** ports — Odysseus on `:17000`
//! (not the usual `:7000`) and MyOwnLLM on `:11473` (not the shared `:1473`) —
//! so it never attaches to a user's own / orphaned instance (mismatched token →
//! 403, or stale engine → 404).

use std::path::PathBuf;

use anyhow::Result;

use crate::brain::BrainClient;

/// Myo's **private** Odysseus (brain) port — deliberately *not* Odysseus's
/// usual `:7000`. Myo spawns and owns its own brain here (with its persisted
/// internal token), so it never attaches to a *foreign* Odysseus on `:7000` — a
/// user's own dev instance, or one orphaned by a Ctrl-C'd `just dev` — whose
/// token wouldn't match and which 403'd every authenticated call. An orphan of
/// Myo's *own* brain on this port is harmless: same persisted token, so
/// re-attaching authenticates.
pub const ODYSSEUS_PORT: u16 = 17000;
/// Myo's **private** MyOwnLLM (model engine) port — deliberately *not*
/// MyOwnLLM's shared default `:1473`. Myo owns its own engine instance here, so
/// it never contends with (or attaches to) a user's separately-run MyOwnLLM /
/// desktop app on `:1473` — the bug that 404'd voice input when Myo latched
/// onto a stale engine. An orphan of *Myo's own* engine left on this port by a
/// Ctrl-C'd run is harmless: it's the same pinned, route-capable build.
pub const MYOWNLLM_PORT: u16 = 11473;

/// `http://127.0.0.1:17000` — Myo's owned brain URL.
pub fn odysseus_base_url() -> String {
    format!("http://127.0.0.1:{ODYSSEUS_PORT}")
}

/// `http://127.0.0.1:11473` — Myo's owned model-engine URL (also what Odysseus
/// is pointed at as its default OpenAI endpoint).
pub fn myownllm_base_url() -> String {
    format!("http://127.0.0.1:{MYOWNLLM_PORT}")
}

/// A launch recipe for one engine. Consumed by the binary's process supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSpec {
    /// Engine id (`odysseus` / `myownllm`) — also the `myo://engine` name.
    pub name: String,
    /// The program to run (an absolute path once the binary resolves it).
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Extra environment variables to set on the child.
    pub env: Vec<(String, String)>,
    /// Working directory, if the engine needs one (Odysseus runs from its tree).
    pub cwd: Option<PathBuf>,
    /// The URL to poll for liveness.
    pub health_url: String,
}

/// Odysseus: `uvicorn app:app --host 127.0.0.1 --port 7000`, run from its source
/// tree, with the internal token injected *before* boot (the token is read once
/// at import, so it must exist in the environment first — PLAN risk #7). The
/// in-process pollers/tasks are turned off so Myo owns the cadence.
pub fn odysseus_spec(uvicorn: impl Into<String>, cwd: PathBuf, token: &str) -> EngineSpec {
    EngineSpec {
        name: "odysseus".into(),
        program: uvicorn.into(),
        args: vec![
            "app:app".into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            ODYSSEUS_PORT.to_string(),
        ],
        env: vec![
            ("ODYSSEUS_INTERNAL_TOKEN".into(), token.into()),
            ("AUTH_ENABLED".into(), "true".into()),
            ("ODYSSEUS_INPROCESS_POLLERS".into(), "0".into()),
            ("ODYSSEUS_INPROCESS_TASKS".into(), "0".into()),
        ],
        cwd: Some(cwd),
        health_url: format!("{}/api/health", odysseus_base_url()),
    }
}

/// MyOwnLLM: `myownllm serve --port 1473`, liveness via `/healthz`.
pub fn myownllm_spec(program: impl Into<String>) -> EngineSpec {
    EngineSpec {
        name: "myownllm".into(),
        program: program.into(),
        args: vec!["serve".into(), "--port".into(), MYOWNLLM_PORT.to_string()],
        env: vec![],
        cwd: None,
        health_url: format!("{}/healthz", myownllm_base_url()),
    }
}

/// Mint Odysseus's `ODYSSEUS_INTERNAL_TOKEN` — a 64-hex-char token seeded from
/// the OS RNG via `RandomState` (whose keys come from the platform CSPRNG),
/// with zero extra dependencies. Good for a loopback-only, per-session admin
/// token; the brain only ever accepts it from `127.0.0.1`.
pub fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(64);
    for i in 0u64..4 {
        // Each `RandomState::new()` derives from the thread's OS-seeded keys.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Load Myo's persisted internal brain token, minting + saving one on first run.
///
/// Persisting it (rather than minting a fresh token each launch) is what lets
/// Myo safely *reuse* an Odysseus that's already running — including one
/// orphaned by a Ctrl-C'd `just dev`. A per-launch token mismatches that brain's
/// token and 403s every authenticated call; a stable token authenticates. Stored
/// `0600` at `~/.myo/internal-token` — a loopback-only admin credential.
pub fn persistent_token() -> String {
    match crate::paths::myo_dir() {
        Ok(dir) => persistent_token_in(&dir),
        Err(_) => random_token(),
    }
}

fn persistent_token_in(dir: &std::path::Path) -> String {
    let path = dir.join("internal-token");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let t = s.trim();
        if is_valid_token(t) {
            return t.to_string();
        }
    }
    let token = random_token();
    let _ = std::fs::create_dir_all(dir);
    if std::fs::write(&path, &token).is_ok() {
        restrict_permissions(&path);
    }
    token
}

fn is_valid_token(t: &str) -> bool {
    t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

/// Point the brain at MyOwnLLM if it isn't already (idempotent). Returns whether
/// a registration was performed. The first endpoint auto-becomes Odysseus's
/// default, so this is all the model wiring `ensure_ready` needs.
pub async fn ensure_myownllm_registered(brain: &BrainClient) -> Result<bool> {
    let existing = brain.list_model_endpoints().await?;
    let already = existing
        .as_array()
        .map(|eps| {
            eps.iter().any(|e| {
                e.get("base_url")
                    .and_then(|u| u.as_str())
                    .map(|u| url_has_port(u, MYOWNLLM_PORT))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if already {
        return Ok(false);
    }
    brain.register_model_endpoint(&myownllm_base_url()).await?;
    Ok(true)
}

/// Does `url` use exactly `port`? Matches `:<port>` only when it's a whole
/// segment (end, `/`, `?`, `#`) — so `:1473` doesn't false-match `:14730`.
fn url_has_port(url: &str, port: u16) -> bool {
    let needle = format!(":{port}");
    let mut from = 0;
    while let Some(i) = url[from..].find(&needle) {
        let end = from + i + needle.len();
        match url.as_bytes().get(end) {
            None => return true,
            Some(c) if !c.is_ascii_digit() => return true,
            _ => from = end, // a longer port like :14730 — keep looking
        }
    }
    false
}

/// Did the server at `url` answer at all (even with a non-2xx status)? Used to
/// poll a just-spawned engine until it binds its port. A bare connection
/// failure (nothing listening yet) is the only "not reachable".
pub async fn endpoint_reachable(url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    client.get(url).send().await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odysseus_spec_injects_token_and_pins_port() {
        let spec = odysseus_spec("/venv/bin/uvicorn", PathBuf::from("/srv/ody"), "deadbeef");
        assert_eq!(spec.program, "/venv/bin/uvicorn");
        assert!(spec.args.contains(&"app:app".to_string()));
        assert!(spec.args.contains(&"17000".to_string()));
        assert_eq!(spec.cwd, Some(PathBuf::from("/srv/ody")));
        assert!(spec
            .env
            .iter()
            .any(|(k, v)| k == "ODYSSEUS_INTERNAL_TOKEN" && v == "deadbeef"));
        assert!(spec
            .env
            .iter()
            .any(|(k, v)| k == "AUTH_ENABLED" && v == "true"));
        assert_eq!(spec.health_url, "http://127.0.0.1:17000/api/health");
    }

    #[test]
    fn myownllm_spec_serves_on_myos_private_port() {
        let spec = myownllm_spec("/usr/local/bin/myownllm");
        assert_eq!(spec.args, vec!["serve", "--port", "11473"]);
        assert_eq!(spec.health_url, "http://127.0.0.1:11473/healthz");
        assert!(spec.cwd.is_none());
    }

    #[test]
    fn url_port_match_is_exact_not_substring() {
        assert!(url_has_port("http://127.0.0.1:1473", 1473));
        assert!(url_has_port("http://127.0.0.1:1473/v1", 1473));
        // The bug this guards against: :1473 must NOT match :14730 / :14739.
        assert!(!url_has_port("http://127.0.0.1:14730", 1473));
        assert!(!url_has_port("http://127.0.0.1:14739/v1", 1473));
        assert!(!url_has_port("http://127.0.0.1:7000", 1473));
    }

    #[test]
    fn token_is_64_hex_chars_and_unique() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two mints must differ");
    }

    #[test]
    fn persistent_token_is_stable_across_calls() {
        let dir = std::env::temp_dir().join(format!("myo-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // First call mints + saves; second call must read back the same token —
        // that stability is what lets a reused/orphaned brain authenticate.
        let a = persistent_token_in(&dir);
        assert!(is_valid_token(&a));
        let b = persistent_token_in(&dir);
        assert_eq!(a, b, "token must persist across launches");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
