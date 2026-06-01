# Changelog

All notable changes to Myo are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Myo aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
