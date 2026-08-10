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
#     "head_manual_tab": [ ...manual-tab.json content on the PR head... ]
#   }
#
# Never checks out or executes submitted code — only inspects the diff
# text and the two JSON data files, which is what makes this safe to run
# against forked-repo PRs with the default read-only GITHUB_TOKEN.
#
# Exit 0 = no hard-fails (soft-flags may still be present, printed either
# way). Exit 1 = at least one hard-fail. Verdict also written to
# $GITHUB_STEP_SUMMARY when set (no-op locally).

set -euo pipefail

FIXTURE="${1:?usage: pr-scope-check.sh <fixture.json>}"
SUMMARY="${GITHUB_STEP_SUMMARY:-/dev/null}"

hard_fails=()
soft_flags=()

hard_fail() { hard_fails+=("$1"); }
soft_flag() { soft_flags+=("$1"); }

# ---------------------------------------------------------------------------
# 1. Path-scope check
# ---------------------------------------------------------------------------

app_files=$(jq -r '.files[] | select(.filename | test("^apps/[a-z][a-z0-9_]*\\.rs$")) | .filename' "$FIXTURE")
catalog_files=$(jq -r '.files[] | select(.filename == "apps-catalog.json") | .filename' "$FIXTURE")
manual_files=$(jq -r '.files[] | select(.filename == "manual-tab.json") | .filename' "$FIXTURE")
other_files=$(jq -r '
  .files[]
  | select(
      (.filename | test("^apps/[a-z][a-z0-9_]*\\.rs$") | not)
      and (.filename != "apps-catalog.json")
      and (.filename != "manual-tab.json")
    )
  | .filename
' "$FIXTURE")

app_count=$(echo -n "$app_files" | grep -c . || true)
catalog_count=$(echo -n "$catalog_files" | grep -c . || true)
manual_count=$(echo -n "$manual_files" | grep -c . || true)

if [ -n "$other_files" ]; then
  hard_fail "touches file(s) outside apps/, apps-catalog.json, and manual-tab.json: $(echo "$other_files" | tr '\n' ' ')"
fi
if [ "$app_count" -ne 1 ]; then
  hard_fail "must add exactly one apps/<name>.rs file (found $app_count)"
fi
if [ "$catalog_count" -ne 1 ]; then
  hard_fail "must modify apps-catalog.json exactly once (found $catalog_count)"
fi
if [ "$manual_count" -ne 1 ]; then
  hard_fail "must modify manual-tab.json exactly once (found $manual_count)"
fi

module=""
if [ "$app_count" -eq 1 ]; then
  app_status=$(jq -r --arg f "$app_files" '.files[] | select(.filename == $f) | .status' "$FIXTURE")
  [ "$app_status" = "added" ] || hard_fail "$app_files must be newly added, not modified"
  module=$(basename "$app_files" .rs)
fi

# ---------------------------------------------------------------------------
# 2. API-boundary, panic/unsafe checks (only meaningful once we have exactly
#    one app file to look at)
# ---------------------------------------------------------------------------

if [ -n "$module" ]; then
  patch=$(jq -r --arg f "$app_files" '.files[] | select(.filename == $f) | .patch // ""' "$FIXTURE")

  if [ -z "$patch" ]; then
    soft_flag "no patch available for apps/$module.rs (diff too large?) — needs manual review"
  else
    added=$(echo "$patch" | grep -E '^\+' | grep -vE '^\+\+\+' || true)

    check_hard() {
      local pattern="$1" reason="$2"
      if echo "$added" | grep -qE "$pattern"; then
        hard_fail "$reason"
      fi
    }

    check_hard '\bunsafe\b' "uses \`unsafe\` — not permitted in community apps"
    check_hard '\bpanic!\s*\(' "uses \`panic!()\` — the firmware halts the whole device on panic, not just this app"
    check_hard '\bunreachable!\s*\(' "uses \`unreachable!()\` — same reason as panic!()"
    check_hard '\btodo!\s*\(' "uses \`todo!()\` — same reason as panic!()"
    check_hard 'crate::storage::' "imports crate::storage:: directly — must go through crate::app::{...} instead"
    check_hard '\bMAX_CHANNEL\b|\bMaxCmd\b|\bMaxSender\b|crate::tasks::max' "reaches MAX11300 directly — must go through crate::app::{...} (make_in_jack/make_out_jack/etc.) instead"

    # Busy-loop heuristic: known limitation, documented rather than hidden —
    # flags any `loop {` when the added lines contain no `.await` anywhere,
    # which can't distinguish "this specific loop never yields" from "some
    # other loop in the same file does" without a real parse. Good enough
    # to catch the common case (a file with one loop and no await at all).
    if echo "$added" | grep -qE '\bloop\s*\{' && ! echo "$added" | grep -q '\.await'; then
      hard_fail "contains a loop {} with no .await anywhere in the diff — Core 1 is cooperatively scheduled, an un-yielding loop starves every other app"
    fi

    if echo "$added" | grep -qE '\.unwrap\(\)|\.expect\('; then
      # Soft-flag only if there's no same-line comment justifying it.
      unjustified=$(echo "$added" | grep -E '\.unwrap\(\)|\.expect\(' | grep -v '//' || true)
      if [ -n "$unjustified" ]; then
        soft_flag "uses .unwrap()/.expect() without an adjacent justification comment — needs human judgment, not auto-rejected"
      fi
    fi
  fi
fi

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
