#!/usr/bin/env bash
# Bump the release version in every file that pins it.
#
# Myo is a Cargo *workspace*: the single source of truth is
# [workspace.package].version in the root Cargo.toml, which every crate
# (the `myo` binary, myo-core, myo-self-update, …) inherits via
# `version.workspace = true`. tauri.conf.json deliberately omits "version"
# (Tauri 2 falls back to Cargo.toml). package.json and Cargo.lock are kept
# in sync here so that the release.yml verify step and the `--locked`
# release build both agree with the tag.
#
# Usage: scripts/bump-version.sh 0.1.0
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <version>" >&2
  exit 64
fi

v="${1#v}"
if ! [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: '$v' is not a semver version (expected X.Y.Z)" >&2
  exit 64
fi

cd "$(git rev-parse --show-toplevel)"

# Root Cargo.toml — bump [workspace.package] version (the single source of
# truth; all crates inherit it via `version.workspace = true`). Only the
# first `version = "..."` inside the [workspace.package] table is touched —
# the many `version = "..."` lines under [workspace.dependencies] are left
# alone.
awk -v v="$v" '
  /^\[workspace\.package\]/ { in_wp=1; print; next }
  /^\[/                     { in_wp=0 }
  in_wp && /^version = "[^"]*"/ && !done { print "version = \"" v "\""; done=1; next }
  { print }
' Cargo.toml > Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml

# package.json — node is the most portable JSON editor we can rely on here.
node -e '
  const fs = require("fs");
  const f = "package.json";
  const j = JSON.parse(fs.readFileSync(f, "utf8"));
  j.version = process.argv[1];
  fs.writeFileSync(f, JSON.stringify(j, null, 2) + "\n");
' "$v"

# Cargo.lock — sync the workspace members' recorded versions without touching
# external dependency pins. `cargo update --workspace` only re-resolves the
# workspace packages (the rest stay pinned). The release build runs with
# `--locked`, so a lockfile out of sync with the bumped manifest would fail.
cargo update --workspace --quiet

echo "Bumped Cargo.toml ([workspace.package]), package.json, and Cargo.lock to $v."
