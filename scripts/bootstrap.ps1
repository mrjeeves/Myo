# Myo dev bootstrap (Windows): install Rust, Node, pnpm, and WebView2, then
# `pnpm install`. Idempotent — safe to re-run; skips what's already present.
#
# Myo's engines (Odysseus, MyOwnLLM) run as separate sidecars, so this does NOT
# install the ASR/onnxruntime stack. See docs/shell.md for engine setup.

$ErrorActionPreference = "Stop"

function Have($cmd) { return [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }
function Log($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "!!! $msg" -ForegroundColor Yellow }

# ── Rust ─────────────────────────────────────────────────────────────────────
if (Have "cargo") {
  Log "Rust already installed ($(rustc --version))."
} elseif (Have "winget") {
  Log "Installing Rust via winget…"
  winget install --id Rustlang.Rustup -e --silent --accept-source-agreements --accept-package-agreements
} else {
  Warn "Rust not found. Install from https://rustup.rs and re-run."
}

# ── Node ─────────────────────────────────────────────────────────────────────
if (Have "node") {
  $major = [int](node -p "process.versions.node.split('.')[0]")
  if ($major -lt 20) { Warn "Node $(node -v) detected; Myo needs Node 20+." }
  else { Log "Node $(node -v) OK." }
} elseif (Have "winget") {
  Log "Installing Node LTS via winget…"
  winget install --id OpenJS.NodeJS.LTS -e --silent --accept-source-agreements --accept-package-agreements
} else {
  Warn "Node.js not found. Install Node 20+ from https://nodejs.org and re-run."
}

# ── WebView2 (Tauri's webview on Windows) ────────────────────────────────────
if (Have "winget") {
  Log "Ensuring WebView2 runtime…"
  winget install --id Microsoft.EdgeWebView2Runtime -e --silent --accept-source-agreements --accept-package-agreements 2>$null
  if ($LASTEXITCODE -ne 0) { $global:LASTEXITCODE = 0 }  # already-installed is fine
}

# ── pnpm ─────────────────────────────────────────────────────────────────────
if (Have "pnpm") {
  Log "pnpm already installed ($(pnpm --version))."
} elseif (Have "corepack") {
  Log "Enabling pnpm via corepack…"
  corepack enable
  corepack prepare pnpm@latest --activate
} else {
  Warn "pnpm not found and corepack unavailable — install pnpm: https://pnpm.io/installation"
}

# ── Frontend deps ────────────────────────────────────────────────────────────
if (Have "pnpm") {
  Log "Installing frontend dependencies…"
  pnpm install
}

Log "Done. Next: 'just dev' to launch the desktop shell (see docs/shell.md for engine setup)."
