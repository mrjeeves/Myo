# Myo installer (Windows) — downloads the latest release binary, verifies its
# SHA-256, and puts myo.exe on your PATH. After this, Myo keeps itself current.
#
#   irm https://raw.githubusercontent.com/mrjeeves/Myo/main/scripts/install.ps1 | iex
#
# Env knobs:
#   $env:MYO_INSTALL_DIR   install location (default: %LOCALAPPDATA%\Myo)
#   $env:MYO_VERSION       a tag like v0.1.0 (default: latest)
$ErrorActionPreference = "Stop"

$Repo = "mrjeeves/Myo"
$InstallDir = if ($env:MYO_INSTALL_DIR) { $env:MYO_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Myo" }

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne "AMD64") {
  throw "Unsupported architecture: $arch. Myo on Windows currently ships x86_64 only."
}
$asset = "myo-windows-x86_64.zip"
$base = if (-not $env:MYO_VERSION -or $env:MYO_VERSION -eq "latest") {
  "https://github.com/$Repo/releases/latest/download"
} else {
  "https://github.com/$Repo/releases/download/$($env:MYO_VERSION)"
}

$tmp = Join-Path $env:TEMP ("myo-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  Write-Host "▸ Downloading $asset…" -ForegroundColor Cyan
  try {
    Invoke-WebRequest -Uri "$base/$asset"        -OutFile "$tmp\$asset"
    Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile "$tmp\$asset.sha256"
  } catch {
    throw "Download failed — has the first release been published yet? https://github.com/$Repo/releases"
  }

  Write-Host "▸ Verifying SHA-256…" -ForegroundColor Cyan
  $expected = ((Get-Content "$tmp\$asset.sha256" -Raw).Trim() -split '\s+')[0]
  $actual = (Get-FileHash "$tmp\$asset" -Algorithm SHA256).Hash.ToLower()
  if ($expected.ToLower() -ne $actual) {
    throw "Checksum mismatch (expected $expected, got $actual). Aborting."
  }

  Expand-Archive -Path "$tmp\$asset" -DestinationPath $tmp -Force
  if (-not (Test-Path "$tmp\myo.exe")) { throw "Archive did not contain myo.exe." }

  New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
  Move-Item "$tmp\myo.exe" (Join-Path $InstallDir "myo.exe") -Force
  Write-Host "▸ Installed myo.exe → $InstallDir" -ForegroundColor Cyan

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if (($userPath -split ';') -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
    Write-Host "! Added $InstallDir to your user PATH — open a new terminal to pick it up." -ForegroundColor Yellow
  }

  Write-Host "✓ Done. Run 'myo' to open the window, or 'myo --version'." -ForegroundColor Green
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
