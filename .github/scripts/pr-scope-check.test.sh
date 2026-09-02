#!/usr/bin/env bash
# Runs pr-scope-check.sh against every fixture in testdata/ and asserts the
# expected verdict. Fully local — no GitHub API calls, no repo checkout.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/pr-scope-check.sh"
DATA="$SCRIPT_DIR/testdata"

pass=0
fail=0

# name, expect ("pass" = exit 0 / no hard-fails, "fail" = exit 1 / hard-fails)
cases=(
  "clean:pass"
  "scope-violation:fail"
  "api-bypass:fail"
  "api-bypass-braced:fail"
  "panic:fail"
  "id-collision:fail"
  "unwrap-unjustified:pass"
  "busy-loop:fail"
  "manual-id-mismatch:fail"
  "manual-missing-field:fail"
)

for case in "${cases[@]}"; do
  name="${case%%:*}"
  expect="${case##*:}"

  set +e
  output=$(GITHUB_STEP_SUMMARY=/dev/null "$CHECK" "$DATA/$name.json" 2>&1)
  status=$?
  set -e

  if [ "$expect" = "pass" ]; then
    if [ "$status" -eq 0 ]; then
      echo "PASS: $name (exited 0 as expected)"
      pass=$((pass + 1))
    else
      echo "FAIL: $name (expected exit 0, got $status)"
      echo "$output" | sed 's/^/    /'
      fail=$((fail + 1))
    fi
  else
    if [ "$status" -ne 0 ]; then
      echo "PASS: $name (rejected as expected)"
      pass=$((pass + 1))
    else
      echo "FAIL: $name (expected a hard-fail, but it passed)"
      echo "$output" | sed 's/^/    /'
      fail=$((fail + 1))
    fi
  fi
done

# unwrap-unjustified must specifically soft-flag, not silently pass clean —
# verify the soft-flag actually fired, not just that exit code was 0.
output=$(GITHUB_STEP_SUMMARY=/dev/null "$CHECK" "$DATA/unwrap-unjustified.json" 2>&1 || true)
if echo "$output" | grep -q "unwrap()/.expect() without an adjacent justification"; then
  echo "PASS: unwrap-unjustified soft-flag actually fired"
  pass=$((pass + 1))
else
  echo "FAIL: unwrap-unjustified should have soft-flagged but didn't"
  fail=$((fail + 1))
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
