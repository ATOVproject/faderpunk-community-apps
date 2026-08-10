#!/usr/bin/env bash
# Confirms a submitted community app compiles standalone against the real
# App<N> API. Clones faderpunk, injects the app alongside every factory
# app (mirroring what faderpunk-store's CLI does — a self-contained shell
# equivalent here since faderpunk-store isn't published anywhere this repo
# could depend on yet), and runs the same root-level, stable-Rust build CI
# and releases actually use (not build-uf2.sh's nightly/build-std path).
#
# Usage: pr-build-check.sh <path-to-submitted-app.rs> <module-name> [faderpunk-ref]

set -euo pipefail

APP_FILE="${1:?usage: pr-build-check.sh <app.rs> <module> [faderpunk-ref]}"
MODULE="${2:?usage: pr-build-check.sh <app.rs> <module> [faderpunk-ref]}"
FADERPUNK_REF="${3:-main}"

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

git clone --branch "$FADERPUNK_REF" --depth 1 https://github.com/ATOVproject/faderpunk.git "$WORKDIR/faderpunk"

APPS_DIR="$WORKDIR/faderpunk/faderpunk/src/apps"
cp "$APP_FILE" "$APPS_DIR/community_$MODULE.rs"

# Rebuild apps/mod.rs: every factory entry already there (parsed straight
# out of the pristine file — same regex-over-a-flat-list approach as
# faderpunk-store-core's registry.rs), plus one new entry for the submitted
# app. The ID here is a placeholder — appId correctness is validated
# separately by pr-scope-check.sh's catalog check, not this script.
#
# Capture the factory list into a variable *before* opening the output
# redirect below — `{ ...; grep ... "$f"; ... } > "$f"` is a classic bash
# hazard: the shell truncates the target file for the whole compound
# command before any command inside it runs, so a grep reading that same
# file mid-block sees a truncated/racy version of it, not the original
# content. Confirmed this the hard way: it silently produced an
# apps/mod.rs with only the new entry and none of the factory apps.
factory_entries="$(grep -oE '[0-9]+ => [a-z_][a-z0-9_]*' "$APPS_DIR/mod.rs")"

{
  echo "register_apps!("
  echo "$factory_entries" | sed 's/^/    /; s/$/,/'
  echo "    250 => community_$MODULE,"
  echo ");"
} >"$APPS_DIR/mod.rs"

cd "$WORKDIR/faderpunk"
cargo build --bin faderpunk --release --target thumbv8m.main-none-eabihf
