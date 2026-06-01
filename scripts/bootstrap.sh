#!/usr/bin/env bash
# Myo dev bootstrap: install Rust, Node, pnpm, and the Tauri build deps, then
# `pnpm install`. Idempotent — safe to re-run; skips anything already present.
#
# Myo's engines (Odysseus, MyOwnLLM) run as separate sidecars, so this does NOT
# install the ASR/onnxruntime stack — that lives with MyOwnLLM. See docs/shell.md
# for pointing Myo at your engine checkouts.

set -euo pipefail

CI_MODE=false
for arg in "$@"; do
  [[ "$arg" == "--ci" ]] && CI_MODE=true
done

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }
have() { command -v "$1" >/dev/null 2>&1; }

OS="$(uname -s)"

# ── Platform build deps ──────────────────────────────────────────────────────

install_linux_deps() {
  if [[ "$CI_MODE" == "true" ]]; then
    log "CI mode: skipping apt step (deps provided by the workflow)"
    return
  fi
  [[ -f /etc/os-release ]] && . /etc/os-release
  case "${ID:-}" in
    ubuntu | debian | pop | linuxmint | raspbian)
      log "Installing Tauri build deps (apt)…"
      sudo apt-get update -qq
      sudo apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
        libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev \
        libssl-dev xdg-utils curl wget file build-essential pkg-config
      ;;
    fedora | rhel | centos)
      log "Installing Tauri build deps (dnf)…"
      sudo dnf install -y \
        webkit2gtk4.1-devel gtk3-devel libsoup3-devel libappindicator-gtk3-devel \
        librsvg2-devel openssl-devel curl wget file gcc gcc-c++ make pkgconf-pkg-config
      ;;
    arch | manjaro)
      log "Installing Tauri build deps (pacman)…"
      sudo pacman -S --needed --noconfirm \
        webkit2gtk-4.1 gtk3 libsoup3 libayatana-appindicator librsvg openssl \
        curl wget file base-devel
      ;;
    *)
      warn "Unrecognised Linux distro (${ID:-?}). Install Tauri deps manually:"
      warn "  https://tauri.app/start/prerequisites/#linux"
      ;;
  esac
}

install_macos_deps() {
  if ! xcode-select -p >/dev/null 2>&1; then
    log "Installing Xcode Command Line Tools (you may be prompted)…"
    xcode-select --install || true
  fi
}

# ── Toolchains ───────────────────────────────────────────────────────────────

install_rust() {
  if have cargo; then
    log "Rust already installed ($(rustc --version))."
    return
  fi
  log "Installing Rust via rustup…"
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1090
  . "$HOME/.cargo/env"
}

check_node() {
  if ! have node; then
    warn "Node.js not found. Install Node 20+ from https://nodejs.org and re-run."
    return 1
  fi
  local major
  major="$(node -p 'process.versions.node.split(".")[0]')"
  if ((major < 20)); then
    warn "Node $(node -v) detected; Myo needs Node 20+."
  else
    log "Node $(node -v) OK."
  fi
}

enable_pnpm() {
  if have pnpm; then
    log "pnpm already installed ($(pnpm --version))."
    return
  fi
  if have corepack; then
    log "Enabling pnpm via corepack…"
    corepack enable
    corepack prepare pnpm@latest --activate
  else
    warn "pnpm not found and corepack unavailable — install pnpm: https://pnpm.io/installation"
  fi
}

# ── Run ──────────────────────────────────────────────────────────────────────

case "$OS" in
  Linux) install_linux_deps ;;
  Darwin) install_macos_deps ;;
  *) warn "Unsupported OS '$OS' — install Tauri deps manually." ;;
esac

install_rust
check_node || true
enable_pnpm

if have pnpm; then
  log "Installing frontend dependencies…"
  pnpm install
fi

log "Done. Next: 'just dev' to launch the desktop shell (see docs/shell.md for engine setup)."
