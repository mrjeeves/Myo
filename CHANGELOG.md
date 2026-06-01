# Changelog

All notable changes to Myo are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Myo aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Orchestration core** (`crates/myo-core`): the Tauri-free, fully unit-tested
  heart of the shell — the Odysseus **brain client** (multipart `chat_stream`
  in, Server-Sent-Events out, normalized into a small `myo://` intent stream),
  the **capability** mapping (the four Web/Files/Code/Reach-out toggles composed
  onto Odysseus's `allow_*` flags + persistent `disabled_tools`), engine
  **supervision specs** + the loopback internal-token minter, and the
  **converse** round-trip (brain answer → server TTS, with a WebSpeech fallback).
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
- **Conversation spine**: brain→voice round-trip with barge-in/cancel; the text
  path (`myo_converse_say`) drives a full turn with no microphone.
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
- **Docs**: `docs/auto-update.md`, plus the imported project plan under `docs/`.
- Community health files, CI, and issue/PR templates.

[Unreleased]: https://github.com/mrjeeves/Myo/commits/main
