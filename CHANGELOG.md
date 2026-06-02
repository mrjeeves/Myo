# Changelog

All notable changes to Myo are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Myo aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Dependencies**: bumped `sha2` 0.10→0.11 and `dirs` 5→6 (Rust), `typescript`
  5→6 and `svelte-check` →4.5 (frontend), and the CI actions (`checkout`,
  `setup-node`, `pnpm/action-setup`→v6, `action-gh-release`→v3). The `vite` 6→8 /
  `vite-plugin-svelte` 4→7 jump was held back — vite 8's rolldown bundler drops
  the `esbuild` minify path our config uses and needs a separate migration.

### Removed

- **Dependabot version-update config** (`.github/dependabot.yml`): retired the
  weekly auto-PR noise. (Repo-level security alerts are independent of this file
  and unaffected.)

### Added

- **Agent core** (`crates/myo-core`): the Tauri-free, fully unit-tested heart of
  the agent — the **native brain** (`llm.rs`: streams MyOwnLLM
  `/v1/chat/completions`, normalized into a small `myo://` intent stream, with a
  persona + running history), the **capability** mapping (the four
  Web/Files/Code/Reach-out toggles), engine **supervision specs**, and the
  **converse** round-trip (ASR → brain → **native TTS** via MyOwnLLM
  `/v1/audio/speech`, with a WebSpeech fallback). *(A legacy Odysseus brain
  client lingers until the memory/tool slices are ported natively.)*
- **Myo Core API** (`src-tauri`): the Core API `#[tauri::command]`s
  (`myo_converse_say`, `myo_capabilities_get/set`, `myo_memory_list/forget`,
  `myo_engines_status/ensure_ready`, `myo_converse_cancel/incognito`, …), the
  OS-facing **engine supervisor** (probe → spawn → health-poll → kill-on-Drop,
  ported in spirit from MyOwnLLM's daemon), and the `myo://` event bridge.
- **Dissolved-UI surface renderer** (`src`): a reactive store that folds the
  `myo://` stream into state, plus the surfaces — Presence (voice-state orb),
  Conversation, Activity feed (with inline tool images), editable
  DocumentArtifact, Control (capability + incognito toggles), and Memory
  (review/search/forget). Honors the agent's `ui_control` panel/toggle
  directives. The Updates panel folds into a Settings slide-over.
- **Conversation spine**: native brain→voice round-trip with barge-in/cancel;
  the text path (`myo_converse_say`) drives a full turn with no microphone.
- **Docs**: [`docs/shell.md`](docs/shell.md) — what's wired today, the Core API,
  and how to run the shell against the real engines.
- **Self-update engine** (`crates/myo-self-update`): GitHub-releases feed with
  stable/beta channels, `auto_apply` policy gating (patch/minor/all/none),
  SHA-256-verified downloads, archive extraction, atomic binary replacement
  (Unix rename + Windows side-swap), stage-now/apply-on-next-launch,
  package-manager-install detection, leftover cleanup, and a background watcher.
  Fully unit-tested.
- **`myo` binary** (`src-tauri`): a single binary that is both a headless CLI
  (`myo update [status|check|apply|enable|disable]`, `myo watch`, `myo --version`)
  and the desktop shell. Applies any staged update on every launch.
- **Desktop shell** (Tauri 2 + Svelte 5) with a **Settings → Updates** panel
  (`src/surfaces/UpdatesSection.svelte`) wired to six Tauri commands.
- **Release pipeline** building all five desktop targets (`linux-x86_64`,
  `linux-aarch64`, `macos-x86_64`, `macos-aarch64`, `windows-x86_64`) with
  per-asset SHA-256 sidecars.
- **One-line installers** (`scripts/install.sh`, `scripts/install.ps1`).
- **Branding**: the Myo voice-waveform icon and full app icon set.
- **Docs**: `docs/auto-update.md`, the native-agent direction (`docs/native-agent.md`), the decisions, and the engine-integration references under `docs/`.
- Community health files, CI, and issue/PR templates.

[Unreleased]: https://github.com/mrjeeves/Myo/commits/main
