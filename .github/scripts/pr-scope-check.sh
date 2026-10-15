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
# Three more fixture fields drive the ownership and overlap rules:
#   "pr_author":   the PR opener's GitHub login (empty/missing = unknown, which
#                  hard-fails the scopes that need to know who is asking)
#   "maintainers": logins from .github/MAINTAINERS *at the PR's base commit* —
#                  never the PR head, so a PR cannot promote itself. Missing or
#                  empty means nobody is a maintainer (fail-closed).
#   "overlaps":    optional { "<file>": [other open PR numbers] }, soft-flag only
#
# Five PR scopes are recognised, decided from the file list (anything that
# fits none of them is a hard-fail, so red CI always means a real problem):
#   submission         — adds exactly one new apps/<name>.rs, plus one new entry
#                        each in apps-catalog.json and manual-tab.json
#   app fix            — modifies one or more existing apps/<name>.rs and nothing else
#   manual edit        — changes existing manual-tab.json entries and nothing else;
#                        only the app's catalog author or a maintainer
#   version bump       — changes only `version` on one or more existing
#                        apps-catalog.json entries; same ownership rule
#   maintainer change  — touches only the maintainer-owned paths in maint_re
#                        below (KEEP IN SYNC with .github/CODEOWNERS)
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

pr_author=$(jq -r '.pr_author // empty' "$FIXTURE")

# True (exit 0) if login $1 is in the base-commit maintainer list. Logins are
# case-insensitive on GitHub; an empty login never matches.
is_maintainer() {
  [ -n "$1" ] || return 1
  jq -e --arg a "$1" '(.maintainers // []) | map(ascii_downcase) | index($a | ascii_downcase) != null' "$FIXTURE" >/dev/null
}

# Hard rule for metadata edits (manual entries, versions): only the app's
# catalog author or a maintainer may change it. $1 = appId, $2 = what is
# being changed, for the message. A missing author is reported once by the
# caller (see `need_author`), not per entry.
require_owner() {
  local id="$1" what="$2" owner
  [ -n "$pr_author" ] || return 0
  is_maintainer "$pr_author" && return 0
  owner=$(jq -r --argjson id "$id" '[.base_catalog[] | select(.appId == $id)] | .[0].author // empty' "$FIXTURE")
  if [ -z "$owner" ]; then
    hard_fail "$what: appId $id has no author in apps-catalog.json, so only a maintainer may change it"
  elif [ "${owner,,}" != "${pr_author,,}" ]; then
    hard_fail "$what: only the app's author ($owner) or a maintainer may change it (PR author: $pr_author)"
  fi
}

need_author() {
  [ -n "$pr_author" ] || hard_fail "PR author unknown — cannot verify who may change $1"
}

# True (exit 0) if version $1 is strictly greater than $2, comparing
# major.minor.patch numerically per component. Both must already be
# validated as N.N.N by the caller — no format checking here.
semver_gt() {
  local IFS=.
  local -a a=($1) b=($2)
  for i in 0 1 2; do
    if [ "${a[i]}" -gt "${b[i]}" ]; then return 0; fi
    if [ "${a[i]}" -lt "${b[i]}" ]; then return 1; fi
  done
  return 1
}

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
# Everything that is neither an app file nor one of the two data files. Split
# into the maintainer-owned allowlist and the rest. The allowlist is an
# explicit list, never "everything else": a stray apps/Foo.rs, apps/sub/x.rs
# or new root file must not be waved through as a maintainer change.
# KEEP IN SYNC with .github/CODEOWNERS (same paths, same owner).
maint_re='^(\.github/|scripts/|tools/|Makefile$|[a-z-]+\.schema\.json$|CONTRIBUTING\.md$|README\.md$|\.gitignore$)'
maint_files=$(jq -r --arg re "$maint_re" '
  .files[]
  | select(.filename | test($re))
  | .filename
' "$FIXTURE")
bad_files=$(jq -r --arg re "$app_re" --arg mre "$maint_re" '
  .files[]
  | select(
      (.filename | test($re) | not)
      and (.filename != "apps-catalog.json")
      and (.filename != "manual-tab.json")
      and (.filename | test($mre) | not)
    )
  | .filename
' "$FIXTURE")

app_count=$(echo -n "$app_files" | grep -c . || true)
added_count=$(echo -n "$added_apps" | grep -c . || true)
modified_count=$(echo -n "$modified_apps" | grep -c . || true)
catalog_count=$(echo -n "$catalog_files" | grep -c . || true)
manual_count=$(echo -n "$manual_files" | grep -c . || true)
maint_count=$(echo -n "$maint_files" | grep -c . || true)

if [ -n "$bad_files" ]; then
  hard_fail "touches path(s) not allowed in any PR kind: $(echo "$bad_files" | paste -sd' ' -) — allowed are apps/<name>.rs, apps-catalog.json, manual-tab.json, and maintainer-owned files (.github/, scripts/, tools/, Makefile, *.schema.json, CONTRIBUTING.md, README.md, .gitignore)"
fi
if [ -n "$other_app_changes" ]; then
  hard_fail "removes or renames app file(s): $(echo "$other_app_changes" | tr '\n' ' ')— app IDs are permanent and saved layouts reference them"
fi

module=""
checked_apps=""
if [ "$maint_count" -gt 0 ]; then
  if [ "$app_count" -ne 0 ] || [ "$catalog_count" -ne 0 ] || [ "$manual_count" -ne 0 ]; then
    scope="mixed"
    hard_fail "mixes maintainer-owned files ($(echo "$maint_files" | paste -sd' ' -)) with app, catalog or manual changes — split into separate PRs"
  else
    scope="maintainer change"
    if ! is_maintainer "$pr_author"; then
      soft_flag "maintainer-owned file(s) changed by ${pr_author:-an unknown author}, who is not a maintainer: $(echo "$maint_files" | paste -sd' ' -) — needs maintainer review (CODEOWNERS)"
    fi
  fi
elif [ "$added_count" -eq 0 ] && [ "$modified_count" -gt 0 ]; then
  scope="app fix"
  checked_apps="$modified_apps"
  if [ "$catalog_count" -ne 0 ] || [ "$manual_count" -ne 0 ]; then
    hard_fail "an app fix may only modify existing apps/<name>.rs files — apps-catalog.json and manual-tab.json changes need their own PR"
  fi
  # A fix to someone else's app is welcome, but its author should hear about it.
  if ! is_maintainer "$pr_author"; then
    for f in $modified_apps; do
      fix_module=$(basename "$f" .rs)
      fix_owner=$(jq -r --arg m "$fix_module" '[.base_catalog[] | select(.module == $m)] | .[0].author // empty' "$FIXTURE")
      if [ -n "$fix_owner" ] && [ "${fix_owner,,}" != "${pr_author,,}" ]; then
        soft_flag "$f: fix by ${pr_author:-an unknown author}, but the app's author is $fix_owner — ping them for review"
      fi
    done
  fi
elif [ "$app_count" -eq 0 ] && [ "$catalog_count" -eq 1 ] && [ "$manual_count" -eq 0 ]; then
  # No apps/*.rs touched at all, only apps-catalog.json — the shape a
  # version bump takes (see the "version bump" catalog validation below).
  # Anything else this shape could be (a catalog-only edit that isn't a
  # clean version bump) gets hard-failed there instead of here, so the one
  # error message explains exactly what was wrong with the diff rather
  # than just "wrong scope".
  scope="version bump"
elif [ "$app_count" -eq 0 ] && [ "$catalog_count" -eq 0 ] && [ "$manual_count" -eq 1 ]; then
  # Only manual-tab.json: editing existing entries (see section 4b).
  scope="manual edit"
elif [ -n "$bad_files" ] && [ "$app_count" -eq 0 ] && [ "$catalog_count" -eq 0 ] && [ "$manual_count" -eq 0 ]; then
  scope="unrecognised"  # nothing but disallowed paths; already hard-failed above
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
    entry_version=$(echo "$entry" | jq -r '.version // empty')

    [ "$entry_module" = "$module" ] || hard_fail "apps-catalog.json entry's module ('$entry_module') doesn't match the submitted app ('$module')"
    [ -n "$entry_author" ] || hard_fail "apps-catalog.json entry is missing an author"
    # Who may later edit this app's manual or version is decided by comparing
    # this field with the PR author's login, so it has to BE the submitter's
    # login (maintainers may submit on someone's behalf). An unknown PR author
    # skips the check: CI always supplies it, and the legacy fixtures don't.
    if [ -n "$entry_author" ] && [ -n "$pr_author" ] && ! is_maintainer "$pr_author" \
        && [ "${entry_author,,}" != "${pr_author,,}" ]; then
      hard_fail "apps-catalog.json entry's author ('$entry_author') must be the submitter's GitHub login ('$pr_author') — who may later edit this app's manual and version depends on it"
    fi
    [[ "$entry_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || hard_fail "apps-catalog.json entry's version ('$entry_version') is not plain major.minor.patch"

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
# 3b. Catalog validation — version bump scope: one or more existing entries
#     changed, each only in its `version` field, each strictly increasing,
#     each by the app's author or a maintainer.
# ---------------------------------------------------------------------------

if [ "$scope" = "version bump" ]; then
  need_author "version(s)"
  base_catalog=$(jq -c '.base_catalog' "$FIXTURE")
  head_catalog=$(jq -c '.head_catalog' "$FIXTURE")

  base_count=$(echo "$base_catalog" | jq 'length')
  head_count=$(echo "$head_catalog" | jq 'length')
  base_ids=$(echo "$base_catalog" | jq -c '[.[].appId]')
  head_ids=$(echo "$head_catalog" | jq -c '[.[].appId]')

  if [ "$base_count" -ne "$head_count" ]; then
    hard_fail "apps-catalog.json: a version bump may only change existing entries, not add or remove entries (had $base_count, now $head_count)"
  elif [ "$base_ids" != "$head_ids" ]; then
    hard_fail "apps-catalog.json: a version bump must not change, reorder or replace any appId"
  else
    changed=$(jq -c -n --argjson b "$base_catalog" --argjson h "$head_catalog" \
      '[range(0; $b | length) | select($b[.] != $h[.])]')
    changed_count=$(echo "$changed" | jq 'length')
    if [ "$changed_count" -eq 0 ]; then
      hard_fail "apps-catalog.json: must change at least one existing entry (found 0 changed)"
    fi

    for i in $(echo "$changed" | jq -r '.[]'); do
      old_entry=$(echo "$base_catalog" | jq -c ".[$i]")
      new_entry=$(echo "$head_catalog" | jq -c ".[$i]")
      entry_id=$(echo "$new_entry" | jq -r '.appId')
      old_version=$(echo "$old_entry" | jq -r '.version // empty')
      new_version=$(echo "$new_entry" | jq -r '.version // empty')

      same_except_version=$(jq -n --argjson a "$old_entry" --argjson b "$new_entry" \
        '(($a | del(.version)) == ($b | del(.version)))')

      if [ "$same_except_version" != "true" ]; then
        hard_fail "apps-catalog.json (appId $entry_id): a version bump may only change the 'version' field — module/author/appId must stay identical"
      elif ! [[ "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        hard_fail "apps-catalog.json (appId $entry_id): new version ('$new_version') is not plain major.minor.patch"
      elif ! [[ "$old_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        hard_fail "apps-catalog.json (appId $entry_id): previous version ('$old_version') is not plain major.minor.patch"
      elif ! semver_gt "$new_version" "$old_version"; then
        hard_fail "apps-catalog.json (appId $entry_id): new version ('$new_version') must be strictly greater than the previous version ('$old_version')"
      fi
      require_owner "$entry_id" "apps-catalog.json (appId $entry_id) version"
    done
  fi
fi

# ---------------------------------------------------------------------------
# 4. manual-tab.json entry validation, shared by submissions (the new entry)
#    and manual edits (every changed entry)
# ---------------------------------------------------------------------------

# $1 = entry JSON, $2 = label used in messages. The wording after the label
# is asserted by the test harness, keep it stable.
check_manual_entry() {
  local mentry="$1" label="$2" field val channel_count missing_channel_fields
  for field in title description icon color text; do
    val=$(echo "$mentry" | jq -r --arg f "$field" '.[$f] // empty')
    [ -n "$val" ] || hard_fail "$label is missing required field '$field'"
  done

  channel_count=$(echo "$mentry" | jq '.channels // [] | length')
  if [ "$channel_count" -lt 1 ]; then
    hard_fail "$label must have at least one channel entry"
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
    [ "$missing_channel_fields" -eq 0 ] || hard_fail "$label has a channel missing required fields (jackTitle/jackDescription/faderTitle/faderDescription/ledTop/ledBottom)"
  fi
}

# Submission: appends-only, exactly one new entry, appId must match the catalog entry.
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

    check_manual_entry "$mentry" "manual-tab.json entry"
  fi
fi

# ---------------------------------------------------------------------------
# 4b. Manual edit scope: existing entries change, nothing is added, removed
#     or reordered, appId and title stay put, each changed entry is still
#     valid, and each belongs to the PR author (or the PR author is a maintainer).
# ---------------------------------------------------------------------------

if [ "$scope" = "manual edit" ]; then
  need_author "manual entries"
  base_manual=$(jq -c '.base_manual_tab' "$FIXTURE")
  head_manual=$(jq -c '.head_manual_tab' "$FIXTURE")
  base_ids=$(echo "$base_manual" | jq -c '[.[].appId]')
  head_ids=$(echo "$head_manual" | jq -c '[.[].appId]')

  if [ "$base_ids" != "$head_ids" ]; then
    hard_fail "manual-tab.json: entries may not be added, removed or reordered in a manual edit — only the content of existing entries may change"
  else
    changed=$(jq -c -n --argjson b "$base_manual" --argjson h "$head_manual" \
      '[range(0; $b | length) | select($b[.] != $h[.])]')
    if [ "$(echo "$changed" | jq 'length')" -eq 0 ]; then
      hard_fail "manual-tab.json: no entry changed"
    fi

    for i in $(echo "$changed" | jq -r '.[]'); do
      old_entry=$(echo "$base_manual" | jq -c ".[$i]")
      new_entry=$(echo "$head_manual" | jq -c ".[$i]")
      entry_id=$(echo "$new_entry" | jq -r '.appId')
      label="manual-tab.json entry for appId $entry_id"

      if [ "$(echo "$old_entry" | jq -r '.title')" != "$(echo "$new_entry" | jq -r '.title')" ]; then
        hard_fail "$label: the title must not change in a manual edit"
      fi
      check_manual_entry "$new_entry" "$label"
      require_owner "$entry_id" "$label"
    done
  fi
fi

# ---------------------------------------------------------------------------
# 5. Overlap with other open PRs — advisory only
# ---------------------------------------------------------------------------

overlap_lines=$(jq -r '
  (.overlaps // {}) as $o
  | .files[].filename
  | select($o[.] != null)
  | "\(.)\t\($o[.] | map("#\(.)") | join(", "))"
' "$FIXTURE")
if [ -n "$overlap_lines" ]; then
  while IFS=$'\t' read -r f prs; do
    soft_flag "$f: also changed by open PR(s) $prs — agree a merge order in the PR descriptions; the later one rebases"
  done <<<"$overlap_lines"
fi

# ---------------------------------------------------------------------------
# Verdict
# ---------------------------------------------------------------------------

{
  echo "## Community app submission scope check"
  echo
  echo "Scope: **$scope**"
  echo
  if is_maintainer "$pr_author"; then role="maintainer"; else role="not a maintainer"; fi
  echo "PR author: \`${pr_author:-unknown}\` ($role). Maintainers at the base commit: $(jq -r '(.maintainers // []) | if length == 0 then "none" else join(", ") end' "$FIXTURE")"
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
