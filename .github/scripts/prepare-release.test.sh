#!/usr/bin/env bash
# Tests prepare-release.sh against a synthetic git history built in a temp dir
# — no network, no GitHub. Each app in the history exercises one rule.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREPARE="${PREPARE:-$SCRIPT_DIR/prepare-release.sh}"  # overridable to run against a mutated copy

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
git init -q -b main
git config commit.gpgsign false

pass=0
fail=0
check() { # name expected actual
  if [ "$2" = "$3" ]; then
    echo "PASS: $1"
    pass=$((pass + 1))
  else
    echo "FAIL: $1"
    echo "    expected: $2"
    echo "    actual:   $3"
    fail=$((fail + 1))
  fi
}

commit() { git add -A && git commit -q -m "$1"; }
touch_app() { echo "// $RANDOM" >>"apps/$1.rs"; }
catalog_version() { jq -c --arg m "$1" --arg v "$2" 'map(if .module == $m then .version = $v else . end)' apps-catalog.json >c.tmp && mv c.tmp apps-catalog.json; }
manual_text() { jq -c --argjson id "$1" --arg t "$2" 'map(if .appId == $id then .text = $t else . end)' manual-tab.json >m.tmp && mv m.tmp manual-tab.json; }
summary() { "$PREPARE" plan | jq -c '[.[] | {module, from, to, level, n: (.changes | length)}]'; }

# --- history ---------------------------------------------------------------
mkdir apps
for m in a b c d e f g; do echo "// $m" >"apps/$m.rs"; done
cat >apps-catalog.json <<'EOF'
[
  {"appId": 100, "module": "a", "author": "x", "version": "0.1.0"},
  {"appId": 101, "module": "b", "author": "x", "version": "0.1.0"},
  {"appId": 102, "module": "c", "author": "x", "version": "0.1.0"},
  {"appId": 103, "module": "d", "author": "x", "version": "0.1.0"},
  {"appId": 104, "module": "e", "author": "x", "version": "0.1.0"},
  {"appId": 105, "module": "f", "author": "x", "version": "1.2.3"},
  {"appId": 106, "module": "g", "author": "x", "version": "0.1.0"}
]
EOF
cat >manual-tab.json <<'EOF'
[
  {"appId": 100, "title": "A", "text": "a"},
  {"appId": 101, "title": "B", "text": "b"},
  {"appId": 102, "title": "C", "text": "c"},
  {"appId": 103, "title": "D", "text": "d"},
  {"appId": 104, "title": "E", "text": "e"},
  {"appId": 105, "title": "F", "text": "f"},
  {"appId": 106, "title": "G", "text": "g"}
]
EOF
commit "feat: add apps"

check "a fresh catalog has nothing to release" "[]" "$(summary)"

touch_app a; commit "fix: A thing (#1)"
touch_app b; commit "feat: B new thing (#2)"
manual_text 102 "c rewritten"; commit "docs: C manual (#3)"
manual_text 101 "b rewritten"; commit "docs: B manual (#3b)"
touch_app a; commit "refactor!: A rewrite (#4)"
touch_app d; commit "fix: D thing (#5)"
git revert --no-edit HEAD >/dev/null                      # d: net no change
touch_app e; commit "fix: E thing (#6)"
catalog_version e 0.1.1; commit "chore: bump e (#7)"       # e released here
touch_app e; commit "fix: E again (#8)"
touch_app f; commit "feat(f)!: F breaking (#9)"
touch_app g; commit "feat: G new (#10)"
touch_app g; commit "fix: G small (#11)"

# a: 'refactor!' is breaking; at 0.x that is only a minor bump, 2 commits
# b: feat -> minor; its manual edit rides along as context
# c: manual-only change -> NOT released (the version tracks the app's code)
# d: changed then reverted -> no bump
# e: only the change AFTER its last bump counts
# f: breaking at >=1.0 -> major
# g: feat + fix -> highest wins (minor)
expected='[{"module":"a","from":"0.1.0","to":"0.2.0","level":"minor","n":2},{"module":"b","from":"0.1.0","to":"0.2.0","level":"minor","n":2},{"module":"e","from":"0.1.1","to":"0.1.2","level":"patch","n":1},{"module":"f","from":"1.2.3","to":"2.0.0","level":"major","n":1},{"module":"g","from":"0.1.0","to":"0.2.0","level":"minor","n":2}]'
check "plan: bump levels, reverts and prior bumps" "$expected" "$(summary)"

"$PREPARE" plan >plan.json
check "plan: reasons carry the commit subjects" \
  '["fix: A thing (#1)","refactor!: A rewrite (#4)"]' \
  "$(jq -c '[.[] | select(.module == "a") | .changes[].subject] | flatten | sort' plan.json)"
check "plan: a manual-only edit does not release an app" \
  '[]' "$(jq -c '[.[] | select(.module == "c")]' plan.json)"
check "plan: a manual edit rides along as context for an app released for its code" \
  '["app","manual"]' "$(jq -c '[.[] | select(.module == "b") | .changes[].kind] | unique' plan.json)"
check "plan: title comes from the manual entry" '"B"' "$(jq -c '.[] | select(.module == "b") | .title' plan.json)"

# --- apply: versions change, nothing else -----------------------------------
before="$(jq -c 'map(del(.version))' apps-catalog.json)"
"$PREPARE" apply plan.json
check "apply: only versions change" "$before" "$(jq -c 'map(del(.version))' apps-catalog.json)"
check "apply: versions match the plan" \
  '{"a":"0.2.0","b":"0.2.0","c":"0.1.0","d":"0.1.0","e":"0.1.2","f":"2.0.0","g":"0.2.0"}' \
  "$(jq -c 'map({(.module): .version}) | add' apps-catalog.json)"

# --- body --------------------------------------------------------------------
body="$("$PREPARE" body plan.json)"
check "body: header counts the apps" "yes" "$(grep -q 'bumps the version of 5 app(s)' <<<"$body" && echo yes || echo no)"
check "body: a row per app" "yes" "$(grep -qF '| A (`a`) | 0.1.0 → 0.2.0 | minor |' <<<"$body" && echo yes || echo no)"

# --- once the bumps are committed there is nothing left to release ------------
commit "chore: prepare release"
release_sha="$(git rev-parse HEAD)"
check "after the release commit: nothing to release" "[]" "$(summary)"

# --- a later change starts a new cycle ---------------------------------------
touch_app b; commit "fix: B follow-up (#12)"
check "next cycle only sees newer changes" \
  '[{"module":"b","from":"0.2.0","to":"0.2.1","level":"patch","n":1}]' "$(summary)"

# --- a release PR that went stale -------------------------------------------
fresh() { "$PREPARE" fresh "$1" "$2" >/dev/null 2>&1 && echo fresh || echo stale; }
prepared="$(git rev-parse HEAD)"
check "fresh: nothing merged since it was prepared" "fresh" "$(fresh "$prepared" HEAD)"
echo "notes" >NOTES.md; commit "docs: notes"
check "fresh: an unrelated change merged since" "fresh" "$(fresh "$prepared" HEAD)"
touch_app c; commit "fix: C late (#13)"
check "stale: an app changed after the PR was prepared" "stale" "$(fresh "$prepared" HEAD)"
prepared="$(git rev-parse HEAD)"
manual_text 101 "b rewritten again"; commit "docs: B manual (#14)"
check "fresh: a manual edit merged since does not make it stale" "fresh" "$(fresh "$prepared" HEAD)"
prepared="$(git rev-parse HEAD)"
catalog_version c 0.9.0; commit "chore: bump c (#15)"
check "stale: the catalog changed after the PR was prepared" "stale" "$(fresh "$prepared" HEAD)"
# Why `fresh` compares two commits instead of re-planning: c's bump commit is
# newer than its late fix, so a plan at HEAD starts counting after the fix.
check "re-plan trap: the late fix to c is invisible to plan, hence the fresh check" "[]" \
  "$("$PREPARE" plan | jq -c '[.[] | select(.module == "c")]')"

# --- planning an older ref reproduces the older answer ------------------------
check "plan at an explicit ref" "[]" "$("$PREPARE" plan "$release_sha" | jq -c '[.[] | {module}]')"
check "unknown ref is an error" "yes" "$("$PREPARE" plan no-such-ref >/dev/null 2>&1 && echo no || echo yes)"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
