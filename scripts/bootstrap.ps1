# Myo dev bootstrap (Windows): install Rust, Node, pnpm, and WebView2, then
# `pnpm install`. Idempotent - safe to re-run; skips what's already present.
#
# Myo's engines (Odysseus, MyOwnLLM) run as separate sidecars, so this does NOT
# install the ASR/onnxruntime stack. See docs/shell.md for engine setup.
#
# NOTE: keep this file pure ASCII. Windows PowerShell 5.1 reads a BOM-less
# script as the system ANSI code page, where stray non-ASCII bytes can be
# mistaken for smart-quote string delimiters and break parsing.

$ErrorActionPreference = "Stop"

function Have($cmd) { return [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }
function Log($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Warn($msg) { Write-Host "!!! $msg" -ForegroundColor Yellow }

# --- Rust --------------------------------------------------------------------
if (Have "cargo") {
  Log "Rust already installed ($(rustc --version))."
} elseif (Have "winget") {
  Log "Installing Rust via winget..."
  winget install --id Rustlang.Rustup -e --silent --accept-source-agreements --accept-package-agreements
} else {
  Warn "Rust not found. Install from https://rustup.rs and re-run."
}

# --- MSVC C++ build tools ----------------------------------------------------
# Rust targets x86_64-pc-windows-msvc by default, which links via Microsoft's
# link.exe. Without the C++ build tools, `cargo build` fails with
# "linker `link.exe` not found". Probe the canonical install paths (link.exe is
# only on PATH inside a Developer prompt), then install only if missing.
function Have-MsvcLinker {
  if (Have "link.exe") { return $true }
  foreach ($base in @(
      "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
      "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC",
      "${env:ProgramFiles}\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC",
      "${env:ProgramFiles}\Microsoft Visual Studio\2022\Professional\VC\Tools\MSVC",
      "${env:ProgramFiles}\Microsoft Visual Studio\2022\Enterprise\VC\Tools\MSVC"
    )) {
    if (Test-Path $base) {
      $found = Get-ChildItem -Path $base -Recurse -Filter "link.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
      if ($found) { return $true }
    }
  }
  return $false
}

if (-not (Have-MsvcLinker)) {
  if (Have "winget") {
    Log "Installing Visual Studio Build Tools (C++ workload, ~5 GB - first run only)..."
    winget install --id Microsoft.VisualStudio.2022.BuildTools --silent `
      --accept-source-agreements --accept-package-agreements `
      --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"
    if ($LASTEXITCODE -ne 0) {
      $global:LASTEXITCODE = 0  # don't abort the rest of setup
      Warn "Build Tools install did not finish. If 'cargo build' later fails with 'link.exe not found', install manually:"
      Warn "  https://visualstudio.microsoft.com/downloads/  ->  Build Tools for Visual Studio 2022  ->  'Desktop development with C++', then re-run this script."
    }
  } else {
    Warn "MSVC C++ build tools not found and winget is unavailable."
    Warn "Install 'Desktop development with C++' from https://visualstudio.microsoft.com/downloads/ and re-run."
  }
}

# --- Node --------------------------------------------------------------------
if (Have "node") {
  $major = [int](node -p "process.versions.node.split('.')[0]")
  if ($major -lt 20) { Warn "Node $(node -v) detected; Myo needs Node 20+." }
  else { Log "Node $(node -v) OK." }
} elseif (Have "winget") {
  Log "Installing Node LTS via winget..."
  winget install --id OpenJS.NodeJS.LTS -e --silent --accept-source-agreements --accept-package-agreements
} else {
  Warn "Node.js not found. Install Node 20+ from https://nodejs.org and re-run."
}

# --- WebView2 (Tauri's webview on Windows) -----------------------------------
if (Have "winget") {
  Log "Ensuring WebView2 runtime..."
  winget install --id Microsoft.EdgeWebView2Runtime -e --silent --accept-source-agreements --accept-package-agreements 2>$null
  if ($LASTEXITCODE -ne 0) { $global:LASTEXITCODE = 0 }  # already-installed is fine
}

# --- pnpm --------------------------------------------------------------------
if (Have "pnpm") {
  Log "pnpm already installed ($(pnpm --version))."
} elseif (Have "corepack") {
  Log "Enabling pnpm via corepack..."
  corepack enable
  corepack prepare pnpm@latest --activate
} else {
  Warn "pnpm not found and corepack unavailable - install pnpm: https://pnpm.io/installation"
}

# --- Frontend deps -----------------------------------------------------------
if (Have "pnpm") {
  Log "Installing frontend dependencies..."
  pnpm install
}

Log "Done. Next: 'just dev' to launch the desktop shell (see docs/shell.md for engine setup)."
