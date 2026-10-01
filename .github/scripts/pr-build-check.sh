#!/usr/bin/env bash
# Confirms a submitted community app builds as an installable FPApp — the
# way it actually ships. Clones faderpunk and runs this repo's own
# `make fpapps` (faderpunk's `fpapp build-community`, the same build the
# downloads site runs after merge) on a copy of this repo holding just the
# submitted app, with its catalog and manual entries from the PR. Anything
# that build would reject after merge fails here instead, before merge.
#
# Unlike the rest of pr-scope.yml, this runs submitted code: the builder
# compiles a small host program from the app and runs it to read the app's
# CONFIG. See pr-scope.yml's header for why that's acceptable there.
#
# Needs the toolchain manual-pages.yml installs: stable and nightly Rust
# with the thumbv8m.main-none-eabihf target, ARM binutils
# (arm-none-eabi-readelf), and jq.
#
# Usage: pr-build-check.sh <app.rs> <module> <apps-catalog.json> <manual-tab.json> [faderpunk-ref]

set -euo pipefail

usage="usage: pr-build-check.sh <app.rs> <module> <apps-catalog.json> <manual-tab.json> [faderpunk-ref]"
APP_FILE="${1:?$usage}"
MODULE="${2:?$usage}"
CATALOG="${3:?$usage}"
MANUAL="${4:?$usage}"
FADERPUNK_REF="${5:-main}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

# The builder builds every entry in the catalog it's given, so hand it a
# catalog holding only this app — with its real appId, version and author.
# Checked before the clone so a missing entry fails in seconds, not minutes.
APP_REPO="$WORKDIR/app"
mkdir -p "$APP_REPO/apps"
jq --arg m "$MODULE" '[.[] | select(.module == $m)]' "$CATALOG" >"$APP_REPO/apps-catalog.json"
entries="$(jq length "$APP_REPO/apps-catalog.json")"
if [ "$entries" -ne 1 ]; then
  echo "error: apps-catalog.json must have exactly one entry with \"module\": \"$MODULE\" (found $entries)" >&2
  exit 1
fi
cp "$APP_FILE" "$APP_REPO/apps/$MODULE.rs"
cp "$MANUAL" "$APP_REPO/manual-tab.json"

git clone --branch "$FADERPUNK_REF" --depth 1 https://github.com/ATOVproject/faderpunk.git "$WORKDIR/faderpunk"

# This repo's Makefile, run from the one-app copy: same invocation as a
# local `make fpapps` and as manual-pages.yml, so the three can't drift.
make -C "$APP_REPO" -f "$REPO_ROOT/Makefile" fpapps FADERPUNK_DIR="$WORKDIR/faderpunk"
