#!/usr/bin/env bash
# Runs pr-scope-check.sh against every fixture in testdata/ and asserts the
# expected verdict. Fully local — no GitHub API calls, no repo checkout.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/pr-scope-check.sh"
DATA="$SCRIPT_DIR/testdata"

pass=0
fail=0

# name | expect | message
#   expect "pass": exit 0 / no hard-fails.
#   expect "fail": exit 1 / hard-fails. The message is REQUIRED — an exit code
#                  alone can't tell "rejected for the intended reason" from
#                  "rejected for an unrelated one" (e.g. a missing author).
#   message: a fixed string the output must contain. For a "pass" it is
#            optional and asserts a soft-flag fired.
# Every fixture in testdata/ must be listed here (checked at the end).
cases=(
  "clean|pass|"
  "scope-violation|fail|not allowed in any PR kind"
  'api-bypass|fail|imports `crate::storage::store_layout` directly'
  'api-bypass-braced|fail|storage::store_layout` directly'
  'panic|fail|uses `panic!()`'
  "id-collision|fail|appId (100) is already taken"
  "unwrap-unjustified|pass|without an adjacent justification"
  "busy-loop|fail|contains a loop {} with no .await"
  "params-no-handler|fail|never runs ParamStore::param_handler()"
  "params-with-handler|pass|"
  "app-fix|pass|"
  "app-fix-new-param|pass|"
  'app-fix-unsafe|fail|uses `unsafe`'
  "app-fix-with-catalog|fail|an app fix may only modify existing"
  "app-removed|fail|removes or renames app file(s)"
  "app-renamed|fail|removes or renames app file(s)"
  "manual-id-mismatch|fail|doesn't match apps-catalog.json's"
  "manual-missing-field|fail|manual-tab.json entry is missing required field 'text'"

  # version bump: one or more existing entries, version only, increasing, by the owner
  "version-bump|pass|"
  "version-bump-multiple|pass|"
  "version-bump-decrease|fail|must be strictly greater"
  "version-bump-multiple-one-decrease|fail|must be strictly greater"
  "version-bump-other-field|fail|may only change the 'version' field"
  "version-bump-plus-new-entry|fail|not add or remove entries"
  "version-bump-no-author|fail|PR author unknown"
  "version-bump-not-owner|fail|only the app's author (bob) or a maintainer"

  # manual edit: existing entries only, by the owner
  "manual-edit|pass|"
  "manual-edit-multi|pass|"
  "manual-edit-maintainer|pass|"
  "manual-edit-not-owner|fail|only the app's author (bob) or a maintainer"
  "manual-edit-no-author|fail|PR author unknown"
  "manual-edit-no-change|fail|no entry changed"
  "manual-edit-add-entry|fail|may not be added, removed or reordered"
  "manual-edit-reorder|fail|may not be added, removed or reordered"
  "manual-edit-title-change|fail|the title must not change"
  "manual-edit-missing-field|fail|is missing required field 'text'"

  # maintainer change: explicit allowlist, never "everything else"
  "maintainer-change|pass|"
  "maintainer-change-outsider|pass|needs maintainer review"
  "maintainer-self-promote|pass|needs maintainer review"
  "maintainer-change-mixed|fail|split into separate PRs"
  "maintainer-change-bad-path-case|fail|not allowed in any PR kind: apps/Foo.rs"
  "maintainer-change-bad-path-subdir|fail|not allowed in any PR kind: apps/sub/x.rs"
  "maintainer-change-bad-path-root|fail|not allowed in any PR kind: notes.txt"
  "maintainer-change-bad-path-mixed|fail|not allowed in any PR kind: notes.txt"

  # submission: the catalog author must be the submitter's login (ownership rules key off it)
  "submission-author-mismatch|fail|must be the submitter's GitHub login ('mallory')"
  "submission-author-match|pass|"
  "submission-by-maintainer|pass|"

  # advisory soft-flags
  "app-fix-not-owner|pass|fix by mallory, but the app's author is alice"
  "overlap|pass|also changed by open PR(s) #88, #91"
)

listed=()
for case in "${cases[@]}"; do
  IFS='|' read -r name expect msg <<<"$case"
  listed+=("$name")

  set +e
  output=$(GITHUB_STEP_SUMMARY=/dev/null "$CHECK" "$DATA/$name.json" 2>&1)
  status=$?
  set -e

  if [ "$expect" = "pass" ]; then
    if [ "$status" -ne 0 ]; then
      echo "FAIL: $name (expected exit 0, got $status)"
      echo "$output" | sed 's/^/    /'
      fail=$((fail + 1))
    elif [ -n "$msg" ] && ! grep -qF -- "$msg" <<<"$output"; then
      echo "FAIL: $name (passed, but the expected soft-flag didn't fire: $msg)"
      echo "$output" | sed 's/^/    /'
      fail=$((fail + 1))
    else
      echo "PASS: $name (exited 0 as expected${msg:+, soft-flag fired})"
      pass=$((pass + 1))
    fi
  else
    if [ -z "$msg" ]; then
      echo "FAIL: $name (negative case has no expected message — add one to the cases table)"
      fail=$((fail + 1))
    elif [ "$status" -eq 0 ]; then
      echo "FAIL: $name (expected a hard-fail, but it passed)"
      echo "$output" | sed 's/^/    /'
      fail=$((fail + 1))
    elif ! grep -qF -- "$msg" <<<"$output"; then
      echo "FAIL: $name (rejected, but not for the intended reason: $msg)"
      echo "$output" | sed 's/^/    /'
      fail=$((fail + 1))
    else
      echo "PASS: $name (rejected for the intended reason)"
      pass=$((pass + 1))
    fi
  fi
done

# A fixture nobody asserts on is a test that silently doesn't exist.
for f in "$DATA"/*.json; do
  base="$(basename "$f" .json)"
  found=0
  for n in "${listed[@]}"; do [ "$n" = "$base" ] && found=1; done
  if [ "$found" -eq 0 ]; then
    echo "FAIL: testdata/$base.json is not listed in the cases table"
    fail=$((fail + 1))
  fi
done

# CODEOWNERS and the script's maintainer-owned allowlist (maint_re) are one
# list: every CODEOWNERS path must be matched by maint_re, and maint_re must
# not have extra alternatives. Otherwise a path could be a "maintainer change"
# without a code-owner review, or need one without being allowed at all.
CODEOWNERS="$SCRIPT_DIR/../CODEOWNERS"
maint_re=$(sed -n "s/^maint_re='\(.*\)'$/\1/p" "$CHECK")
owners_count=0
sync_ok=1
while read -r path _; do
  owners_count=$((owners_count + 1))
  sample="${path#/}"
  case "$sample" in
    */) sample="${sample}x" ;;
    *\**) sample="${sample/\*/a-b}" ;;
  esac
  if ! grep -qE "$maint_re" <<<"$sample"; then
    echo "FAIL: CODEOWNERS path $path is not matched by maint_re in pr-scope-check.sh"
    sync_ok=0
  fi
done < <(grep -vE '^\s*(#|$)' "$CODEOWNERS")
alternatives=$(( $(grep -o '|' <<<"$maint_re" | wc -l) + 1 ))
if [ "$alternatives" -ne "$owners_count" ]; then
  echo "FAIL: maint_re has $alternatives alternatives but CODEOWNERS lists $owners_count paths"
  sync_ok=0
fi
if [ "$sync_ok" -eq 1 ]; then
  echo "PASS: CODEOWNERS and maint_re list the same $owners_count paths"
  pass=$((pass + 1))
else
  fail=$((fail + 1))
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
