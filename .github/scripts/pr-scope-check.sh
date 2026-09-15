#!/usr/bin/env bash
# Automated first-pass gate for PRs to faderpunk-community-apps.
# KEEP IN SYNC WITH CONTRIBUTING.md "The rules" section.
#
# Usage: pr-scope-check.sh <fixture.json>
#
# fixture.json shape (matches what the CI workflow assembles from `gh api`):
#   {
#     "files": [ {filename, status, additions, deletions, patch}, ... ],
#     "base_catalog": [ ...apps-catalog.json content on the base branch... ],
#     "head_catalog": [ ...apps-catalog.json content on the PR head... ],
#     "base_manual_tab": [ ...manual-tab.json content on the base branch... ],
#     "head_manual_tab": [ ...manual-tab.json content on the PR head... ],
#     "head_sources": { "apps/<name>.rs": "...full file at the PR head...", ... }
#   }
#
# `head_sources` is optional. Without it, the whole-file heuristics fall back
# to the diff's added lines, which is the full file for a new app.
#
# Two PR scopes are recognised, decided from the file list:
#   submission — adds exactly one new apps/<name>.rs, plus one new entry each
#                in apps-catalog.json and manual-tab.json
#   app fix    — modifies one or more existing apps/<name>.rs and nothing else
#
# Never checks out or executes submitted code — only inspects the diff
# text and the two JSON data files, which is what makes this safe to run
# against forked-repo PRs with the default read-only GITHUB_TOKEN.
#
# Exit 0 = no hard-fails (soft-flags may still be present, printed either
# way). Exit 1 = at least one hard-fail. Verdict also written to
# $GITHUB_STEP_SUMMARY when set (no-op locally).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE="${1:?usage: pr-scope-check.sh <fixture.json>}"
SUMMARY="${GITHUB_STEP_SUMMARY:-/dev/null}"

hard_fails=()
soft_flags=()

hard_fail() { hard_fails+=("$1"); }
soft_flag() { soft_flags+=("$1"); }

# ---------------------------------------------------------------------------
# 1. Path-scope check
# ---------------------------------------------------------------------------

app_re='^apps/[a-z][a-z0-9_]*\.rs$'
app_files=$(jq -r --arg re "$app_re" '.files[] | select(.filename | test($re)) | .filename' "$FIXTURE")
added_apps=$(jq -r --arg re "$app_re" '.files[] | select((.filename | test($re)) and .status == "added") | .filename' "$FIXTURE")
modified_apps=$(jq -r --arg re "$app_re" '.files[] | select((.filename | test($re)) and .status == "modified") | .filename' "$FIXTURE")
other_app_changes=$(jq -r --arg re "$app_re" '
  .files[]
  | select((.filename | test($re)) and .status != "added" and .status != "modified")
  | "\(.filename) (\(.status))"
' "$FIXTURE")
catalog_files=$(jq -r '.files[] | select(.filename == "apps-catalog.json") | .filename' "$FIXTURE")
manual_files=$(jq -r '.files[] | select(.filename == "manual-tab.json") | .filename' "$FIXTURE")
other_files=$(jq -r --arg re "$app_re" '
  .files[]
  | select(
      (.filename | test($re) | not)
      and (.filename != "apps-catalog.json")
      and (.filename != "manual-tab.json")
    )
  | .filename
' "$FIXTURE")

app_count=$(echo -n "$app_files" | grep -c . || true)
added_count=$(echo -n "$added_apps" | grep -c . || true)
modified_count=$(echo -n "$modified_apps" | grep -c . || true)
catalog_count=$(echo -n "$catalog_files" | grep -c . || true)
manual_count=$(echo -n "$manual_files" | grep -c . || true)

if [ -n "$other_files" ]; then
  hard_fail "touches file(s) outside apps/, apps-catalog.json, and manual-tab.json: $(echo "$other_files" | tr '\n' ' ')"
fi
if [ -n "$other_app_changes" ]; then
  hard_fail "removes or renames app file(s): $(echo "$other_app_changes" | tr '\n' ' ')— app IDs are permanent and saved layouts reference them"
fi

module=""
checked_apps=""
if [ "$added_count" -eq 0 ] && [ "$modified_count" -gt 0 ]; then
  scope="app fix"
  checked_apps="$modified_apps"
  if [ "$catalog_count" -ne 0 ] || [ "$manual_count" -ne 0 ]; then
    hard_fail "an app fix may only modify existing apps/<name>.rs files — apps-catalog.json and manual-tab.json changes need their own PR"
  fi
else
  scope="submission"
  if [ "$app_count" -ne 1 ]; then
    hard_fail "must add exactly one apps/<name>.rs file (found $app_count)"
  fi
  if [ "$catalog_count" -ne 1 ]; then
    hard_fail "must modify apps-catalog.json exactly once (found $catalog_count)"
  fi
  if [ "$manual_count" -ne 1 ]; then
    hard_fail "must modify manual-tab.json exactly once (found $manual_count)"
  fi
  # A removed or renamed app file has already hard-failed above.
  if [ "$app_count" -eq 1 ] && [ "$added_count" -eq 1 ]; then
    module=$(basename "$app_files" .rs)
    checked_apps="$app_files"
  fi
fi

# ---------------------------------------------------------------------------
# 2. API-boundary, panic/unsafe checks, run on every app file in scope
# ---------------------------------------------------------------------------

check_hard() {
  local text="$1" pattern="$2" reason="$3"
  if grep -qE "$pattern" <<<"$text"; then
    hard_fail "$reason"
  fi
}

check_app_source() {
  local file="$1" patch added source boundary_violations added_file unjustified
  patch=$(jq -r --arg f "$file" '.files[] | select(.filename == $f) | .patch // ""' "$FIXTURE")

  if [ -z "$patch" ]; then
    soft_flag "$file: no patch available (diff too large?) — needs manual review"
    return
  fi

  added=$(grep -E '^\+' <<<"$patch" | grep -vE '^\+\+\+' || true)
  # Whole-file heuristics need the whole file: an app fix's diff holds only
  # the changed lines, so e.g. a new .add_param( would look handler-less.
  source=$(jq -r --arg f "$file" '.head_sources[$f] // empty' "$FIXTURE")
  [ -n "$source" ] || source="$added"

  check_hard "$added" '\bunsafe\b' "$file: uses \`unsafe\` — not permitted in community apps"
  check_hard "$added" '\bpanic!\s*\(' "$file: uses \`panic!()\` — the firmware halts the whole device on panic, not just this app"
  check_hard "$added" '\bunreachable!\s*\(' "$file: uses \`unreachable!()\` — same reason as panic!()"
  check_hard "$added" '\btodo!\s*\(' "$file: uses \`todo!()\` — same reason as panic!()"
  check_hard "$added" '\bMAX_CHANNEL\b|\bMaxCmd\b|\bMaxSender\b' "$file: reaches MAX11300 symbols directly — must go through crate::app::{...} (make_in_jack/make_out_jack/etc.) instead"

  # General API-boundary check: every crate::-rooted path — in `use`
  # statements (including nested brace-lists like
  # `use crate::{ app::{...}, storage::{...} }`, where the offending
  # segment isn't textually adjacent to `crate::`) and in fully-qualified
  # inline references (`crate::tasks::foo::bar()`) — must start with
  # `crate::app`. Supersedes the old crate::storage::/crate::tasks::max
  # line-regexes, which a brace-nested import could slip past; see
  # crate-boundary-check.py's docstring for why a real (if small) parser
  # is needed here instead of another regex.
  added_file=$(mktemp)
  printf '%s\n' "$added" > "$added_file"
  boundary_violations=$(python3 "$SCRIPT_DIR/crate-boundary-check.py" "$added_file")
  rm -f "$added_file"
  if [ -n "$boundary_violations" ]; then
    while IFS= read -r v; do
      hard_fail "$file: imports \`$v\` directly — must go through crate::app::{...} instead"
    done <<<"$boundary_violations"
  fi

  # Busy-loop heuristic: known limitation, documented rather than hidden —
  # flags any `loop {` when the file contains no `.await` anywhere, which
  # can't distinguish "this specific loop never yields" from "some other
  # loop in the same file does" without a real parse. Good enough to catch
  # the common case (a file with one loop and no await at all).
  if grep -qE '\bloop\s*\{' <<<"$source" && ! grep -q '\.await' <<<"$source"; then
    hard_fail "$file: contains a loop {} with no .await anywhere in the file — Core 1 is cooperatively scheduled, an un-yielding loop starves every other app"
  fi

  # Declared params are only reachable through ParamStore::param_handler():
  # without it the app never answers the Configurator's param request, so
  # its params can't be shown or changed and stay at the compiled-in
  # defaults. Same whole-file text heuristic as the busy-loop check.
  if grep -qE '\.add_param\s*\(' <<<"$source" && ! grep -qE '\bparam_handler\s*\(' <<<"$source"; then
    hard_fail "$file: declares parameters (.add_param) but never runs ParamStore::param_handler() — the Configurator can't read or change them"
  fi

  # Soft-flag only if there's no same-line comment justifying it.
  unjustified=$(grep -E '\.unwrap\(\)|\.expect\(' <<<"$added" | grep -v '//' || true)
  if [ -n "$unjustified" ]; then
    soft_flag "$file: uses .unwrap()/.expect() without an adjacent justification comment — needs human judgment, not auto-rejected"
  fi
}

while IFS= read -r file; do
  [ -n "$file" ] && check_app_source "$file"
done <<<"$checked_apps"

# ---------------------------------------------------------------------------
# 3. Catalog validation — appends-only, exactly one new entry
# ---------------------------------------------------------------------------

catalog_id=""
if [ -n "$module" ]; then
  base_catalog=$(jq -c '.base_catalog' "$FIXTURE")
  head_catalog=$(jq -c '.head_catalog' "$FIXTURE")

  missing_or_changed=$(jq -n --argjson base "$base_catalog" --argjson head "$head_catalog" \
    '[$base[] | select(. as $b | ($head | index($b)) == null)] | length')
  if [ "$missing_or_changed" -ne 0 ]; then
    hard_fail "apps-catalog.json: existing entries were modified or removed — only appending a new entry is allowed"
  fi

  new_entries=$(jq -c -n --argjson base "$base_catalog" --argjson head "$head_catalog" \
    '[$head[] | select(. as $h | ($base | index($h)) == null)]')
  new_count=$(echo "$new_entries" | jq 'length')

  if [ "$new_count" -ne 1 ]; then
    hard_fail "apps-catalog.json: must add exactly one new entry (found $new_count)"
  else
    entry=$(echo "$new_entries" | jq -c '.[0]')

    entry_module=$(echo "$entry" | jq -r '.module // empty')
    entry_author=$(echo "$entry" | jq -r '.author // empty')
    entry_id=$(echo "$entry" | jq -r '.appId // empty')

    [ "$entry_module" = "$module" ] || hard_fail "apps-catalog.json entry's module ('$entry_module') doesn't match the submitted app ('$module')"
    [ -n "$entry_author" ] || hard_fail "apps-catalog.json entry is missing an author"

    if ! [[ "$entry_id" =~ ^[0-9]+$ ]]; then
      hard_fail "apps-catalog.json entry's appId is not a plain integer"
    elif [ "$entry_id" -lt 100 ] || [ "$entry_id" -gt 255 ]; then
      hard_fail "apps-catalog.json entry's appId ($entry_id) is outside the reserved community range 100-255"
    else
      id_taken=$(jq --argjson id "$entry_id" '[.[] | select(.appId == $id)] | length' <<<"$base_catalog")
      if [ "$id_taken" -eq 0 ]; then
        catalog_id="$entry_id"
      else
        hard_fail "apps-catalog.json entry's appId ($entry_id) is already taken"
      fi
    fi
  fi
fi

# ---------------------------------------------------------------------------
# 4. manual-tab.json validation — appends-only, exactly one new entry,
#    appId must match the catalog entry, required fields present
# ---------------------------------------------------------------------------

if [ -n "$module" ]; then
  base_manual=$(jq -c '.base_manual_tab' "$FIXTURE")
  head_manual=$(jq -c '.head_manual_tab' "$FIXTURE")

  missing_or_changed=$(jq -n --argjson base "$base_manual" --argjson head "$head_manual" \
    '[$base[] | select(. as $b | ($head | index($b)) == null)] | length')
  if [ "$missing_or_changed" -ne 0 ]; then
    hard_fail "manual-tab.json: existing entries were modified or removed — only appending a new entry is allowed"
  fi

  new_manual_entries=$(jq -c -n --argjson base "$base_manual" --argjson head "$head_manual" \
    '[$head[] | select(. as $h | ($base | index($h)) == null)]')
  new_manual_count=$(echo "$new_manual_entries" | jq 'length')

  if [ "$new_manual_count" -ne 1 ]; then
    hard_fail "manual-tab.json: must add exactly one new entry (found $new_manual_count)"
  else
    mentry=$(echo "$new_manual_entries" | jq -c '.[0]')
    mentry_id=$(echo "$mentry" | jq -r '.appId // empty')

    if [ -n "$catalog_id" ]; then
      [ "$mentry_id" = "$catalog_id" ] || hard_fail "manual-tab.json entry's appId ($mentry_id) doesn't match apps-catalog.json's ($catalog_id)"
    fi

    for field in title description icon color text; do
      val=$(echo "$mentry" | jq -r --arg f "$field" '.[$f] // empty')
      [ -n "$val" ] || hard_fail "manual-tab.json entry is missing required field '$field'"
    done

    channel_count=$(echo "$mentry" | jq '.channels // [] | length')
    if [ "$channel_count" -lt 1 ]; then
      hard_fail "manual-tab.json entry must have at least one channel entry"
    else
      missing_channel_fields=$(echo "$mentry" | jq -r '
        [.channels[]
          | select(
              (has("jackTitle") and has("jackDescription") and has("faderTitle")
               and has("faderDescription") and has("ledTop") and has("ledBottom"))
              | not
            )
        ] | length
      ')
      [ "$missing_channel_fields" -eq 0 ] || hard_fail "manual-tab.json entry has a channel missing required fields (jackTitle/jackDescription/faderTitle/faderDescription/ledTop/ledBottom)"
    fi
  fi
fi

# ---------------------------------------------------------------------------
# Verdict
# ---------------------------------------------------------------------------

{
  echo "## Community app submission scope check"
  echo
  echo "Scope: **$scope**"
  echo
  if [ ${#hard_fails[@]} -eq 0 ]; then
    echo "**No hard-fails.**"
  else
    echo "**Hard-fails (auto-reject):**"
    for f in "${hard_fails[@]}"; do echo "- $f"; done
  fi
  echo
  if [ ${#soft_flags[@]} -eq 0 ]; then
    echo "No soft-flags."
  else
    echo "**Soft-flags (needs human review):**"
    for f in "${soft_flags[@]}"; do echo "- $f"; done
  fi
} | tee -a "$SUMMARY"

[ ${#hard_fails[@]} -eq 0 ]
