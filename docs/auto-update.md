# Myo — Auto-update

Myo is **set-it-and-forget-it**, like MyOwnLLM. Once installed as a raw binary,
it keeps itself current with **zero interaction**: a background watcher checks
GitHub releases, downloads + verifies the new binary, stages it, and the **next
launch applies it**. This is the update half of "THE hands-free assistant
experience" — you never stop to babysit an installer.

This is a faithful port of MyOwnLLM's `self_update.rs` (a *custom* GitHub
releases updater — **not** the Tauri updater plugin), re-themed for Myo and
extracted into a reusable, Tauri-agnostic crate.

> **Status.** Fully implemented. The engine, CLI, background watcher, and
> release/CI pipeline live in `crates/myo-self-update`; the `myo` binary and the
> desktop **Settings → Updates** panel live in `src-tauri/` +
> `src/surfaces/UpdatesSection.svelte`. Unit-tested, and the whole path
> (engine → Tauri command → Svelte) compiles and type-checks in CI.

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

## Desktop GUI

The desktop window (Tauri 2 + Svelte 5, in `src-tauri/` + `src/`) hosts a
**Settings → Updates** panel — the manual face of the otherwise-automatic
updater. It's wired end to end:

- **Rust commands** — `src-tauri/src/update_commands.rs` exposes six thin
  delegates to the engine: `update_status`, `update_check_now`,
  `update_apply_now` (applies strictly, then `app.restart()`),
  `update_set_enabled`, `update_leftovers_list`, `update_leftovers_clear`.
  They're registered in `src-tauri/src/main.rs` via `generate_handler!`.
- **Startup + watcher** — `main()` calls `apply_pending_if_any()` before
  anything else, and the Tauri `setup` hook starts the background watcher on
  Tauri's own runtime:

  ```rust
  myo_self_update::apply_pending_if_any();               // before the window opens
  // …in .setup():
  tauri::async_runtime::spawn(myo_self_update::watcher::watch_forever());
  ```

  (The lib also offers `watcher::spawn_background()` for plain Tokio contexts;
  the GUI uses `async_runtime::spawn` because Tauri owns the runtime.)
- **Svelte panel** — `src/surfaces/UpdatesSection.svelte` shows the current
  version/channel/install-kind, a "Check for updates" button, an automatic-updates
  toggle, the staged-update banner with "Restart & apply now", and the last-check /
  policy / interval / release-feed details. It binds to exactly the six commands
  above via `@tauri-apps/api`'s `invoke`.

### Single binary: CLI + window

Like MyOwnLLM, one `myo` binary is both. With arguments it's a CLI
(`myo update …`, `myo watch`, `myo --version`); with none it opens the window.
Either path applies a staged update first.

### Building / running

```sh
pnpm install
pnpm build                 # produces dist/, embedded into the binary at compile time
cargo run -p myo           # opens the desktop window (needs a display)
cargo run -p myo -- update # or drive the updater headless
```

Linux needs the webkit/gtk/soup dev packages (`libwebkit2gtk-4.1-dev`,
`libgtk-3-dev`, `libsoup-3.0-dev`, `libjavascriptcoregtk-4.1-dev`); CI installs
them and builds the whole path on every push.
