#!/usr/bin/env bash
# Mechanical pre-PR heuristics for community apps (Arthur-style review).
# Best-effort: may false-positive; see CONTRIBUTING.md and kosmar wip-lifecycle §J.
#
# Usage:
#   ./scripts/pre-pr-app-check.sh              # all apps/*.rs (legacy debt may fail)
#   ./scripts/pre-pr-app-check.sh dit_dah      # one module (no .rs)
#   ./scripts/pre-pr-app-check.sh --strict     # warnings also fail
#
# Exit 0 = no hard failures (warnings may still print). Exit 1 = hard failure.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

STRICT=0
MODULES=()
for arg in "$@"; do
  case "$arg" in
    --strict) STRICT=1 ;;
    -h | --help)
      sed -n '1,12p' "$0"
      exit 0
      ;;
    *)
      MODULES+=("$arg")
      ;;
  esac
done

if [[ ${#MODULES[@]} -eq 0 ]]; then
  shopt -s nullglob
  for f in apps/*.rs; do
    MODULES+=("$(basename "$f" .rs)")
  done
fi

HARD=0
WARN=0

fail() {
  echo "error: $1" >&2
  HARD=1
}

warn() {
  echo "warn: $1" >&2
  WARN=1
}

strip_rust_comments() {
  python3 - "$1" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
text = re.sub(r"//.*?$", "", text, flags=re.M)
print(text)
PY
}

check_file() {
  local module="$1"
  local path="apps/${module}.rs"
  if [[ ! -f "$path" ]]; then
    fail "$path: missing"
    return
  fi

  echo "== $module =="

  local plain
  plain="$(strip_rust_comments "$path")"

  if grep -qE 'CLOCK_PUBSUB|crate::tasks::clock' <<<"$plain"; then
    fail "$path: CLOCK_PUBSUB or crate::tasks::clock — use app.use_clock() / ClockEvent / current_tick facade"
  fi
  if grep -qE 'crate::tasks::' <<<"$plain"; then
    fail "$path: crate::tasks:: — community apps must use crate::app facade only"
  fi

  if grep -qE '\b(panic!|unreachable!|todo!)\s*\(' <<<"$plain"; then
    fail "$path: panic!/unreachable!/todo! — device-wide halt"
  fi
  while IFS= read -r line; do
    if grep -qE '\.(unwrap|expect)\s*\(' <<<"$line" && ! grep -qE '//.*infallible|//.*cannot fail|// SAFETY' <<<"$line"; then
      warn "$path: unwrap/expect without adjacent justification comment: ${line:0:120}"
    fi
  done < <(grep -nE '\.(unwrap|expect)\s*\(' "$path" 2>/dev/null | head -20 || true)

  if grep -q 'SceneEvent::LoadScene' "$path"; then
    if ! grep -q 'load_from_scene' "$path"; then
      fail "$path: SceneEvent::LoadScene without load_from_scene"
    fi
  fi
  if grep -q 'SceneEvent::SaveScene' "$path"; then
    if ! grep -q 'save_to_scene' "$path"; then
      fail "$path: SceneEvent::SaveScene without save_to_scene"
    fi
  fi
  if ! python3 - "$path" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
text = re.sub(r'"(?:\\.|[^"\\])*"', '""', text)
text = re.sub(r"//.*?$", "", text, flags=re.M)
for m in re.finditer(
    r"SceneEvent::SaveScene\s*\([^)]*\)\s*=>\s*(\{[^}]*\}|\(\s*\))",
    text,
    flags=re.S,
):
    body = m.group(1)
    if "save_to_scene" not in body:
        sys.exit(1)
sys.exit(0)
PY
  then
    fail "$path: SceneEvent::SaveScene arm is empty or missing save_to_scene"
  fi

  local param_const add_count
  param_const="$(grep -E '^pub const PARAMS:' "$path" | head -1 | sed -E 's/.*= *([0-9]+).*/\1/' || true)"
  add_count="$(grep -c '\.add_param(' "$path" || true)"
  if [[ -n "$param_const" && "$add_count" -gt 0 && "$param_const" != "$add_count" ]]; then
    fail "$path: PARAMS=$param_const but $add_count .add_param() calls in CONFIG"
  fi
  if [[ "${add_count:-0}" -gt 0 ]]; then
    if ! grep -q 'ParamStore' "$path"; then
      fail "$path: CONFIG has params but no ParamStore"
    fi
    if ! grep -q 'param_handler' "$path"; then
      fail "$path: CONFIG has params but param_handler() not spawned alongside run()"
    fi
    if ! grep -q 'fn from_values' "$path" || ! grep -q 'fn to_values' "$path"; then
      fail "$path: AppParams needs from_values and to_values for param round-trip"
    fi
  fi

  if grep -q 'param_handler' <<<"$plain" && grep -qE 'Params::default\(\)' <<<"$plain"; then
    if ! grep -qE 'ParamStore::update|\.update\(' <<<"$plain"; then
      warn "$path: param_handler + Params::default — on-device param edits that restart run() must not clobber FRAM (ParamStore::update or avoid re-seed)"
    fi
  fi

  local app_id desc_rs desc_manual
  app_id="$(python3 - "$module" <<'PY'
import json, sys
mod = sys.argv[1]
for e in json.load(open("apps-catalog.json")):
    if e.get("module") == mod:
        print(e.get("appId", ""))
        break
PY
)"
  if [[ -n "$app_id" ]]; then
    desc_rs="$(grep -E 'Config::new\(' "$path" | head -1 | sed -E 's/.*Config::new\(\s*"([^"]*)".*/\1/' || true)"
    desc_manual="$(python3 - "$app_id" <<'PY'
import json, sys
aid = int(sys.argv[1])
for e in json.load(open("manual-tab.json")):
    if e.get("appId") == aid:
        print(e.get("description", ""))
        break
PY
)"
    if [[ -n "$desc_manual" && -n "$desc_rs" ]]; then
      if [[ "$desc_manual" != *"$desc_rs"* && "$desc_rs" != *"$desc_manual"* ]]; then
        warn "$path: manual-tab description may not match CONFIG tagline (human review)"
      fi
    fi
    if ! python3 - "$app_id" <<'PY'
import json, sys
aid = int(sys.argv[1])
for e in json.load(open("manual-tab.json")):
    if e.get("appId") == aid:
        ch = e.get("channels")
        if not ch:
            sys.exit(1)
        sys.exit(0)
sys.exit(2)
PY
    then
      rc=$?
      if [[ "$rc" == 1 ]]; then
        warn "manual-tab.json appId=$app_id: missing channels[] (required for jack/fader docs)"
      fi
    fi
  fi
}

for mod in "${MODULES[@]}"; do
  check_file "$mod"
done

echo "---"
if [[ "$HARD" -ne 0 ]]; then
  echo "pre-pr-app-check: FAILED ($HARD hard issue(s), $WARN warning(s))" >&2
  exit 1
fi
if [[ "$STRICT" -eq 1 && "$WARN" -ne 0 ]]; then
  echo "pre-pr-app-check: FAILED under --strict ($WARN warning(s))" >&2
  exit 1
fi
echo "pre-pr-app-check: OK (${#MODULES[@]} module(s), $WARN warning(s))"
exit 0
