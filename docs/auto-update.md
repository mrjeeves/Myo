# Myo — Auto-update

Myo is **set-it-and-forget-it**, like MyOwnLLM. Once installed as a raw binary,
it keeps itself current with **zero interaction**: a background watcher checks
GitHub releases, downloads + verifies the new binary, stages it, and the **next
launch applies it**. This is the update half of "THE hands-free assistant
experience" — you never stop to babysit an installer.

This is a faithful port of MyOwnLLM's `self_update.rs` (a *custom* GitHub
releases updater — **not** the Tauri updater plugin), re-themed for Myo and
extracted into a reusable, Tauri-agnostic crate.

> **Status.** The engine, the `myo` CLI, the background watcher, and the
> release/CI pipeline are implemented and unit-tested (`crates/myo-self-update`,
> `crates/myo`). The **GUI Updates panel** is specified at the bottom of this
> doc and lands with the Tauri shell (PLAN step 1/7) — it needs a window to live
> in, so it isn't built yet, but every contract it depends on already exists.

---

## How it works

```
        ┌─ every launch ─────────────────────────────────────────────┐
        │ myo_self_update::apply_pending_if_any()                     │
        │   • reaps a Windows <exe>.old from a prior swap             │
        │   • if ~/.myo/updates/pending.json points at a NEWER ver,   │
        │     atomically renames the staged binary over the current  │
        │     one and clears the marker                              │
        └────────────────────────────────────────────────────────────┘

        ┌─ background (watcher::spawn_background / `myo watch`) ──────┐
        │ tick() every 30 min, but only hits the network once per    │
        │ auto_update.check_interval_hours (default 6h):             │
        │   GitHub releases → newest tag → semver compare            │
        │   → auto_apply policy gate → download asset                │
        │   → verify SHA-256 → extract → stage + write pending.json  │
        └────────────────────────────────────────────────────────────┘
```

- **Stage now, apply on next launch.** We never restart a running Myo out from
  under itself; the swap happens cleanly at the next start.
- **Atomic replacement.** Unix renames the new binary over the live one (the
  running process keeps its old inode until exit). Windows can't rename an open
  `.exe`, so the running binary is side-renamed to `<exe>.old` (allowed even
  while mapped), the new one moves into place, and `.old` is reaped next launch.
- **Integrity.** Every download is checked against its `*.sha256` sidecar (or a
  `SHA256SUMS` fallback) before it can replace anything. Checksum/signature
  sidecars can never be mistaken for the binary itself.
- **Package-manager installs are left alone.** Homebrew / apt / rpm / MSI /
  Chocolatey / Scoop paths are detected and skipped — the OS package manager
  owns versioning there.

## Configuration — `~/.myo/config.json`

```jsonc
{
  "auto_update": {
    "enabled": true,            // master switch
    "channel": "stable",        // "stable" | "beta"
    "auto_apply": "patch",      // "patch" | "minor" | "all" | "none"
    "check_interval_hours": 6.0,
    "stable_url": "",           // optional: redirect the release feed
    "beta_url": ""              // optional: redirect the beta feed
  }
}
```

- **`auto_apply`** governs only *background* staging: `patch` stages same-major,
  same-minor bumps; `minor` allows same-major; `all` allows anything; `none`
  stages nothing. An explicit `myo update` ignores this — invoking it is consent
  to upgrade now.
- **Kill switches:** `auto_update.enabled = false`, or the env var
  `MYO_AUTOUPDATE=0`.
- **Feed redirection (priority order):** `auto_update.stable_url` / `beta_url`
  in config → build-time `MYO_RELEASE_URL_STABLE` / `MYO_RELEASE_URL_BETA` →
  the GitHub default (`api.github.com/repos/mrjeeves/Myo/releases[/latest]`).

## CLI

```
myo update            # one shot: check + download + verify + apply now
myo update status     # current version, install kind, channel, feed, pending
myo update check      # check + stage per policy (bypasses the cooldown)
myo update apply      # apply a previously-staged update
myo update enable     # set auto_update.enabled = true
myo update disable    # set auto_update.enabled = false
myo watch             # run the background watcher in the foreground
```

## State on disk (`~/.myo/`)

| Path | Purpose |
|---|---|
| `config.json` | `auto_update.*` knobs |
| `cache/last-update-check` | unix-seconds stamp for the interval gate |
| `updates/<version>/` | downloaded archive + extracted binary (staging) |
| `updates/pending.json` | `{version, path, staged_at}` — what to apply next launch |
| `updates/pm-detected.flag` | set once when a package-manager install is seen |
| `watcher.lock` | advisory single-watcher lock (pid; first process wins) |

---

## Release pipeline (what the updater consumes)

`.github/workflows/release.yml` runs on a `vX.Y.Z` tag (or manual dispatch). For
each of the **5 targets** it builds `myo`, packages a portable artifact, and
attaches a basename-scoped checksum sidecar to the GitHub release:

| Target | Asset | Checksum |
|---|---|---|
| `linux-x86_64` | `myo-linux-x86_64.tar.gz` | `…​.tar.gz.sha256` |
| `linux-aarch64` | `myo-linux-aarch64.tar.gz` | `…​.tar.gz.sha256` |
| `macos-x86_64` | `myo-macos-x86_64.tar.gz` | `…​.tar.gz.sha256` |
| `macos-aarch64` | `myo-macos-aarch64.tar.gz` | `…​.tar.gz.sha256` |
| `windows-x86_64` | `myo-windows-x86_64.zip` | `…​.zip.sha256` |

`current_target_triple_hint()` in the engine matches exactly these `name`
substrings, so each install picks the right asset.

**Cutting a release:** bump `[workspace.package] version` in the root
`Cargo.toml`, tag `vX.Y.Z`, push the tag. The workflow asserts the tag matches
the manifest version (a mismatch would make every install misreport its version
and loop "update available" forever).

---

## GUI wiring (lands with the Tauri shell)

The engine is already shaped for a Settings → Updates panel: `status()`,
`check_now()`, `set_enabled()`, `apply_pending_strict()`, and the leftover
helpers all return serde-friendly types. When `src-tauri` exists (PLAN step 1),
add these six commands — each a thin delegate — and register them in the Tauri
builder:

```rust
// src-tauri/src/update_commands.rs
use myo_self_update as su;

#[tauri::command]
fn update_status() -> Result<su::UpdateStatus, String> {
    su::status().map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_check_now() -> Result<su::CheckOutcome, String> {
    su::check_now().await.map_err(|e| e.to_string())
}

#[tauri::command]
fn update_apply_now(app: tauri::AppHandle) -> Result<(), String> {
    // Apply strictly so a swap failure surfaces BEFORE we relaunch — otherwise
    // the user sees the old version after "restart" and assumes it worked.
    su::apply_pending_strict().map_err(|e| e.to_string())?;
    app.restart();
}

#[tauri::command]
fn update_set_enabled(enabled: bool) -> Result<su::UpdateStatus, String> {
    su::set_enabled(enabled).map_err(|e| e.to_string())?;
    su::status().map_err(|e| e.to_string())
}

#[tauri::command]
fn update_leftovers_list() -> Vec<su::UpdateLeftover> { su::list_update_leftovers() }

#[tauri::command]
fn update_leftovers_clear() -> u64 { su::clear_update_leftovers() }
```

…and, at the top of `main`/`setup`, the two load-bearing calls:

```rust
su::apply_pending_if_any();          // before anything else
su::watcher::spawn_background();      // once a Tokio runtime exists
```

A ready Svelte 5 panel (ported from MyOwnLLM's `UpdatesSection.svelte`,
re-themed for Myo — `myo`, `~/.myo`, `MYO_RELEASE_URL_*`) drops into
`src/surfaces/` and binds to exactly those commands:

```svelte
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  interface PendingUpdate { version: string; staged_at: string; }
  interface UpdateStatus {
    current_version: string;
    install_kind: "raw" | "package_manager";
    enabled: boolean;
    channel: string;
    auto_apply: string;
    check_interval_hours: number;
    last_check_unix: number | null;
    pending: PendingUpdate | null;
    release_url: string;
    release_url_overridden: boolean;
  }
  type CheckOutcome =
    | { kind: "disabled" }
    | { kind: "package_manager" }
    | { kind: "up_to_date"; current: string; latest: string }
    | { kind: "staged"; version: string }
    | { kind: "policy_blocked"; current: string; latest: string; policy: string };

  let status = $state<UpdateStatus | null>(null);
  let checking = $state(false);
  let outcome = $state<CheckOutcome | null>(null);
  let error = $state("");

  onMount(refresh);
  async function refresh() {
    try { status = await invoke<UpdateStatus>("update_status"); }
    catch (e) { error = String(e); }
  }
  async function checkNow() {
    checking = true; outcome = null; error = "";
    try {
      outcome = await invoke<CheckOutcome>("update_check_now");
      status = await invoke<UpdateStatus>("update_status");
    } catch (e) { error = String(e); } finally { checking = false; }
  }
  async function applyNow() {
    try { await invoke("update_apply_now"); } catch (e) { error = String(e); }
  }
  async function setEnabled(enabled: boolean) {
    try { status = await invoke<UpdateStatus>("update_set_enabled", { enabled }); outcome = null; }
    catch (e) { error = String(e); }
  }
</script>

{#if status}
  <div class="updates">
    <header>
      <div>
        <strong>myo {status.current_version}</strong>
        <small>{status.install_kind === "package_manager" ? "package manager" : "raw binary"} · {status.channel} channel</small>
      </div>
      <button onclick={checkNow} disabled={checking || !status.enabled}>
        {checking ? "Checking…" : "Check for updates"}
      </button>
    </header>

    <label>
      <input type="checkbox" checked={status.enabled}
             onchange={(e) => setEnabled((e.currentTarget as HTMLInputElement).checked)} />
      Automatic updates ({status.enabled ? `every ${status.check_interval_hours}h` : "paused"})
    </label>

    {#if status.pending}
      <div class="pending">
        Update staged: <strong>{status.pending.version}</strong> — restart to apply.
        <button onclick={applyNow}>Restart &amp; apply now</button>
      </div>
    {/if}

    {#if outcome?.kind === "staged"}<p>{outcome.version} staged. Restart to apply.</p>
    {:else if outcome?.kind === "up_to_date"}<p>Up to date ({outcome.latest}).</p>
    {:else if outcome?.kind === "policy_blocked"}<p>{outcome.latest} available but auto_apply="{outcome.policy}" blocks it.</p>{/if}

    {#if error}<p class="error">{error}</p>{/if}
  </div>
{/if}
```

That panel is the only piece awaiting the shell. Everything behind it — checks,
downloads, verification, staging, the atomic swap, the policy/channel logic,
the background cadence — is implemented and tested today.
