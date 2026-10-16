#!/usr/bin/env bash
# Works out which community apps have unreleased changes and prepares their
# version bumps. Used by .github/workflows/prepare-release.yml; also runnable
# locally from the repo root (read-only except for `apply`).
#
#   prepare-release.sh plan  [REF]       JSON plan on stdout (default REF: HEAD)
#   prepare-release.sh apply PLAN.json   rewrite the versions in apps-catalog.json
#   prepare-release.sh body  PLAN.json   markdown for the PR body / commit message
#   prepare-release.sh fresh PREPARED_ON MERGED_ONTO
#                                        exit 1 if anything that ships changed in between
#
# What counts as "unreleased": an app has a change when, since the commit that
# set its CURRENT catalog version, either its apps/<module>.rs or its
# manual-tab.json entry differs. The manual counts because it ships inside the
# .fpapp package. The comparison is by content, so a change that was reverted
# in between is not a change.
#
# How far to bump, from the conventional-commit subjects of the commits that
# touched apps/<module>.rs (history is squash-merged, so first-parent subjects
# are the PR titles):
#   breaking (`type!:` or "BREAKING CHANGE")  major, but minor while the app is 0.x
#   feat                                       minor
#   anything else, or a manual-only change     patch
# The highest level across the commits wins.

set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

# Version of $2 (module) in apps-catalog.json at commit $1, or nothing.
version_at() {
  git show "$1:apps-catalog.json" 2>/dev/null |
    jq -r --arg m "$2" '[.[] | select(.module == $m)][0].version // empty'
}

# The manual-tab.json entry for appId $2 at commit $1, canonicalised so that
# key order and whitespace don't count as a change.
manual_at() {
  git show "$1:manual-tab.json" 2>/dev/null |
    jq -S -c --argjson id "$2" '[.[] | select(.appId == $id)][0] // empty'
}

# The oldest commit in the unbroken run (counting back from $1) in which module
# $2 already had version $3 — i.e. the commit that set its current version.
release_commit() {
  local ref="$1" module="$2" current="$3" c v found=""
  while read -r c; do
    v="$(version_at "$c" "$module")"
    if [ "$v" = "$current" ]; then found="$c"; else break; fi
  done < <(git log --first-parent --format=%H "$ref" -- apps-catalog.json)
  printf '%s' "$found"
}

# next_version <current> <level>
next_version() {
  local major minor patch
  IFS=. read -r major minor patch <<<"$1"
  case "$2" in
    major) echo "$((major + 1)).0.0" ;;
    minor) echo "$major.$((minor + 1)).0" ;;
    *) echo "$major.$minor.$((patch + 1))" ;;
  esac
}

# Highest level named by the conventional-commit subjects on stdin (TSV: sha, subject).
level_from_subjects() {
  local level=patch subject type bang
  while IFS=$'\t' read -r _ subject; do
    [ -n "$subject" ] || continue
    type=""; bang=""
    if [[ "$subject" =~ ^([A-Za-z]+)(\([^\)]*\))?(!)?: ]]; then
      type="${BASH_REMATCH[1],,}"
      bang="${BASH_REMATCH[3]}"
    fi
    if [ -n "$bang" ] || [[ "$subject" == *"BREAKING CHANGE"* ]]; then
      level=major
      break
    elif [ "$type" = "feat" ]; then
      level=minor
    fi
  done
  printf '%s' "$level"
}

cmd_plan() {
  local ref="${1:-HEAD}" lines module app_id current rel title level to
  local app_commits manual_commits app_changed manual_changed c p changes
  git rev-parse --verify --quiet "$ref^{commit}" >/dev/null || die "unknown ref: $ref"
  lines="$(mktemp)"
  trap 'rm -f "$lines"' RETURN

  while IFS=$'\t' read -r module app_id; do
    current="$(version_at "$ref" "$module")"
    [ -n "$current" ] || die "$module has no version in apps-catalog.json at $ref"
    rel="$(release_commit "$ref" "$module" "$current")"
    if [ -z "$rel" ]; then
      echo "warning: could not find the commit that set $module $current; skipping" >&2
      continue
    fi

    app_changed=0
    git diff --quiet "$rel" "$ref" -- "apps/$module.rs" || app_changed=1
    manual_changed=0
    [ "$(manual_at "$rel" "$app_id")" = "$(manual_at "$ref" "$app_id")" ] || manual_changed=1
    [ "$app_changed" -eq 1 ] || [ "$manual_changed" -eq 1 ] || continue

    app_commits="$(git log --first-parent --format='%h%x09%s' "$rel..$ref" -- "apps/$module.rs")"
    manual_commits=""
    if [ "$manual_changed" -eq 1 ]; then
      while read -r c; do
        p="$(git rev-parse --verify --quiet "$c^" || true)"
        [ -n "$p" ] || continue
        if [ "$(manual_at "$c" "$app_id")" != "$(manual_at "$p" "$app_id")" ]; then
          manual_commits+="$(git show -s --format='%h%x09%s' "$c")"$'\n'
        fi
      done < <(git log --first-parent --format=%H "$rel..$ref" -- manual-tab.json)
    fi

    level="$(level_from_subjects <<<"$app_commits")"
    # Semver: while an app is 0.x a breaking change is only a minor bump.
    if [ "$level" = "major" ] && [ "${current%%.*}" = "0" ]; then level=minor; fi
    to="$(next_version "$current" "$level")"
    title="$(git show "$ref:manual-tab.json" | jq -r --argjson id "$app_id" '[.[] | select(.appId == $id)][0].title // empty')"

    changes="$(
      {
        [ -z "$app_commits" ] || sed 's/^/app\t/' <<<"$app_commits"
        [ -z "$manual_commits" ] || printf '%s' "$manual_commits" | sed 's/^/manual\t/'
      } | jq -R -s 'split("\n") | map(select(length > 0) | split("\t") | {kind: .[0], sha: .[1], subject: .[2]})'
    )"

    jq -n -c --arg module "$module" --argjson appId "$app_id" --arg title "${title:-$module}" \
      --arg from "$current" --arg to "$to" --arg level "$level" --argjson changes "$changes" \
      '{module: $module, appId: $appId, title: $title, from: $from, to: $to, level: $level, changes: $changes}' >>"$lines"
  done < <(git show "$ref:apps-catalog.json" | jq -r '.[] | [.module, .appId] | @tsv')

  jq -s 'sort_by(.appId)' "$lines"
}

cmd_apply() {
  local plan="${1:?usage: prepare-release.sh apply PLAN.json}" tmp
  [ -f apps-catalog.json ] || die "run from the repo root (no apps-catalog.json here)"
  tmp="$(mktemp)"
  jq --slurpfile p "$plan" '
    ($p[0] | map({(.module): .to}) | add // {}) as $v
    | map(if $v[.module] then .version = $v[.module] else . end)
  ' apps-catalog.json >"$tmp"
  mv "$tmp" apps-catalog.json
}

cmd_body() {
  local plan="${1:?usage: prepare-release.sh body PLAN.json}" n
  n="$(jq length "$plan")"
  echo "Merging this PR bumps the version of $n app(s) and publishes a release with a ZIP of every community app."
  echo
  echo "| App | Version | Bump | Changes since the last release |"
  echo "|---|---|---|---|"
  jq -r '.[] | "| \(.title) (`\(.module)`) | \(.from) → \(.to) | \(.level) | \(.changes | map(.subject | gsub("\\|"; "\\|")) | join("<br>")) |"' "$plan"
}

# A release PR is a snapshot: its version table was worked out on PREPARED_ON.
# If anything that ships (apps/, manual-tab.json, apps-catalog.json) merged
# after that and before the PR itself, the release would build code that its
# versions don't describe. The check has to compare those two commits rather
# than re-plan at the merge commit: the release commit is newer than the late
# change, so a re-plan would start counting from after it and see nothing.
cmd_fresh() {
  local prepared="${1:?usage: prepare-release.sh fresh PREPARED_ON MERGED_ONTO}"
  local merged="${2:?usage: prepare-release.sh fresh PREPARED_ON MERGED_ONTO}"
  if git diff --quiet "$prepared" "$merged" -- apps manual-tab.json apps-catalog.json; then
    return 0
  fi
  echo "This release PR is stale: apps, manuals or the catalog changed on main after it was prepared:" >&2
  git diff --stat "$prepared" "$merged" -- apps manual-tab.json apps-catalog.json >&2
  echo "Run Prepare release again, then close and reopen the new PR." >&2
  return 1
}

case "${1:-}" in
  fresh) shift; cmd_fresh "$@" ;;
  plan) shift; cmd_plan "$@" ;;
  apply) shift; cmd_apply "$@" ;;
  body) shift; cmd_body "$@" ;;
  *) die "usage: prepare-release.sh plan [REF] | apply PLAN.json | body PLAN.json | fresh PREPARED_ON MERGED_ONTO" ;;
esac
