#!/usr/bin/env bash
# Builds the Community App Catalogue static site.
#
#   FADERPUNK_DIR=../faderpunk ./build.sh <output-dir> [downloads-dir]
#
# Renders manual-tab.json through the real Configurator manual components
# from $FADERPUNK_DIR (see src/entry.tsx for why), compiles the same
# Tailwind theme they're written against, and copies the icon assets they
# reference. Output is plain static HTML/CSS/SVG plus the .fpapp
# downloads — no client-side JS.
set -euo pipefail

SITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SITE_DIR/../.." && pwd)"
OUT_DIR="$(cd "$(dirname "${1:?usage: build.sh <output-dir> [downloads-dir]}")" && pwd)/$(basename "$1")"
DOWNLOADS_DIR="${2:-}"
FADERPUNK_DIR="${FADERPUNK_DIR:-$REPO_ROOT/../faderpunk}"

if [ ! -f "$FADERPUNK_DIR/configurator/src/components/manual/ManualApp.tsx" ]; then
  echo "error: FADERPUNK_DIR ($FADERPUNK_DIR) is not a faderpunk checkout" >&2
  echo "       clone https://github.com/ATOVproject/faderpunk next to this repo," >&2
  echo "       or pass FADERPUNK_DIR=/path/to/faderpunk" >&2
  exit 1
fi

# Fixed symlink so import specifiers and styles.css's @source globs (which
# can't read env vars) stay stable wherever faderpunk actually lives.
ln -sfn "$(cd "$FADERPUNK_DIR" && pwd)" "$SITE_DIR/.faderpunk"

cd "$SITE_DIR"
mkdir -p "$OUT_DIR"

echo "==> bundling the renderer"
npx vite build --logLevel warn

echo "==> compiling CSS (same Tailwind theme the components are written against)"
npx @tailwindcss/cli -i src/styles.css -o "$OUT_DIR/styles.css" --minify

# theme.css's @font-face and mask urls are root-absolute (/fonts/, /icons/)
# because the Configurator is served from a domain root. This is a project
# Pages site under /<repo>/, so rebase them — same reason entry.tsx rebases
# the /img/ glyph paths in the markup.
SITE_BASE="${SITE_BASE:-/}"
if [ "$SITE_BASE" != "/" ]; then
  sed -i "s#url(/fonts/#url(${SITE_BASE}fonts/#g; s#url(/icons/#url(${SITE_BASE}icons/#g" "$OUT_DIR/styles.css"
fi

echo "==> copying icon and font assets"
# ManualApp's Icon renders <BASE_URL>icons/<name>.svg, and the manual
# markup references /img/*.svg for the jack, fader and button glyphs.
mkdir -p "$OUT_DIR/icons" "$OUT_DIR/img" "$OUT_DIR/fonts"
cp "$FADERPUNK_DIR"/configurator/public/icons/*.svg "$OUT_DIR/icons/"
cp "$FADERPUNK_DIR"/configurator/public/img/*.svg "$OUT_DIR/img/"
# theme.css's @font-face rules point at /fonts/*, and fonts don't cross
# the iframe boundary from the embedding Configurator — each document
# needs its own copy, or the page silently falls back to system fonts.
#
# Barlow only, deliberately. It's the body font and is open (SIL OFL), so
# shipping it here is fine. VoxRound — the display face used for the
# "Channel N" and button labels — is a commercial licence held for the
# Configurator's own deployment; redistributing it from this repo isn't
# covered by that, so it's left out and src/styles.css falls --font-vox
# back to Barlow instead. Affects a handful of small labels, nothing
# structural.
cp "$FADERPUNK_DIR"/configurator/public/fonts/Barlow-*.ttf "$OUT_DIR/fonts/"

echo "==> rendering"
node dist/entry.js "$REPO_ROOT" "$OUT_DIR" ${DOWNLOADS_DIR:+"$DOWNLOADS_DIR"}
