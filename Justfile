# Myo — one-command operations.
# Install `just` (https://just.systems), then run `just setup` to get going.
#
# Myo is a Cargo *workspace* (root Cargo.toml) + a Svelte 5 frontend, so the
# cargo recipes operate on `--workspace` from the repo root (unlike MyOwnLLM,
# whose crate lives under src-tauri/).

set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

default: help

help:
    @just --list

# Install all dev prerequisites (Rust, Node, pnpm, and the Tauri/GTK build deps).
[unix]
[doc("Install dev prerequisites (Rust, Node, pnpm, Tauri/GTK deps), then pnpm install.")]
setup:
    @./scripts/bootstrap.sh

[windows]
[doc("Install dev prerequisites (Rust, Node, pnpm, Tauri/WebView2), then pnpm install.")]
setup:
    @& .\scripts\bootstrap.ps1

# Run the desktop shell in dev mode with hot reload.
#
# The shell drives two loopback engines — Odysseus (:7000) and MyOwnLLM (:1473).
# Point it at your checkouts first (see docs/shell.md):
#   export MYO_ODYSSEUS_DIR=…  MYO_UVICORN=…  MYO_MYOWNLLM_BIN=…
dev:
    @pnpm install --frozen-lockfile
    @pnpm tauri dev

# Build a production Tauri bundle.
build:
    @pnpm install --frozen-lockfile
    @pnpm tauri build

# Run the `myo` binary (CLI: update / watch / --version). Builds if needed.
[unix]
[doc("Run the myo binary (build first if needed). e.g. `just run update status`.")]
run *ARGS:
    @if [ -x target/release/myo ]; then \
        target/release/myo {{ARGS}}; \
    else \
        cargo run -p myo -- {{ARGS}}; \
    fi

[windows]
[doc("Run the myo binary (build first if needed). e.g. `just run update status`.")]
run *ARGS:
    @if (Test-Path target/release/myo.exe) { & target/release/myo.exe {{ARGS}} } else { cargo run -p myo -- {{ARGS}} }

# Format Rust + frontend.
[unix]
[doc("Format Rust (cargo fmt) + frontend (prettier, best-effort).")]
fmt:
    @cargo fmt --all
    @pnpm exec prettier --write "src/**/*.{ts,svelte,json}" 2>/dev/null || true

[windows]
[doc("Format Rust (cargo fmt) + frontend (prettier, best-effort).")]
fmt:
    @cargo fmt --all
    @pnpm exec prettier --write "src/**/*.{ts,svelte,json}"; if ($LASTEXITCODE -ne 0) { $global:LASTEXITCODE = 0 }

# Lint Rust (clippy, warnings-as-errors) + svelte-check.
lint:
    @cargo clippy --workspace --all-targets -- -D warnings
    @pnpm check

# Run the Rust test suite.
test:
    @cargo test --workspace

# Everything CI runs, locally, before you push.
check:
    @cargo fmt --all -- --check
    @pnpm install --frozen-lockfile
    @pnpm build
    @pnpm check
    @cargo clippy --workspace --all-targets -- -D warnings
    @cargo test --workspace
