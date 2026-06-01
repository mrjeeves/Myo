#!/bin/sh
# Myo installer — downloads the latest release binary for this platform,
# verifies its SHA-256, and drops `myo` on your PATH. After this, Myo keeps
# itself up to date; you never run this again.
#
#   curl -fsSL https://raw.githubusercontent.com/mrjeeves/Myo/main/scripts/install.sh | sh
#
# Env knobs:
#   MYO_INSTALL_DIR   where to install (default: $HOME/.local/bin)
#   MYO_VERSION       a tag like v0.1.0 (default: latest)
set -eu

REPO="mrjeeves/Myo"
INSTALL_DIR="${MYO_INSTALL_DIR:-$HOME/.local/bin}"

say()  { printf '\033[36m▸\033[0m %s\n' "$1"; }
err()  { printf '\033[31m✗ %s\033[0m\n' "$1" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || err "curl is required."
command -v tar  >/dev/null 2>&1 || err "tar is required."

os="$(uname -s)"
arch="$(uname -m)"
case "$os-$arch" in
  Linux-x86_64)            platform="linux-x86_64" ;;
  Linux-aarch64|Linux-arm64) platform="linux-aarch64" ;;
  Darwin-x86_64)           platform="macos-x86_64" ;;
  Darwin-arm64)            platform="macos-aarch64" ;;
  *) err "unsupported platform: $os $arch. On Windows use scripts/install.ps1; otherwise build from source." ;;
esac

asset="myo-${platform}.tar.gz"
if [ "${MYO_VERSION:-latest}" = "latest" ]; then
  base="https://github.com/${REPO}/releases/latest/download"
else
  base="https://github.com/${REPO}/releases/download/${MYO_VERSION}"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Downloading ${asset}…"
curl -fSL "${base}/${asset}"        -o "${tmp}/${asset}" \
  || err "download failed — has the first release been published yet? (https://github.com/${REPO}/releases)"
curl -fSL "${base}/${asset}.sha256" -o "${tmp}/${asset}.sha256" \
  || err "checksum download failed."

say "Verifying SHA-256…"
expected="$(awk '{print $1}' "${tmp}/${asset}.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "${tmp}/${asset}" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "${tmp}/${asset}" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || err "checksum mismatch (expected ${expected}, got ${actual}). Aborting."

tar -xzf "${tmp}/${asset}" -C "${tmp}"
[ -f "${tmp}/myo" ] || err "archive did not contain a 'myo' binary."

mkdir -p "$INSTALL_DIR"
mv "${tmp}/myo" "${INSTALL_DIR}/myo"
chmod +x "${INSTALL_DIR}/myo"
say "Installed myo → ${INSTALL_DIR}/myo"

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) printf '\033[33m!\033[0m %s is not on your PATH. Add this to your shell profile:\n    export PATH="%s:$PATH"\n' "$INSTALL_DIR" "$INSTALL_DIR" ;;
esac

printf '\033[32m✓ Done.\033[0m Run \033[1mmyo\033[0m to open the window, or \033[1mmyo --version\033[0m.\n'
