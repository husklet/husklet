#!/usr/bin/env bash
#
# Run a cargo test invocation and, if it fails, NAME THE FAILING TESTS.
#
# Job logs need repository admin rights to download. Without them the only
# diagnosis a failing test step offers is the annotation `Process completed with
# exit code 101`, which costs a local reproduction cycle to turn into a test
# name -- and a reproduction that may not be faithful. That happened on
# 8e1e167c8: two steps were added, they failed on a runner, and identifying WHICH
# test cost more than the fix would have.
#
# Annotations ARE readable without admin, so the information is put where it can
# be read. Usage:
#
#   .github/scripts/cargo-test-named.sh "<label>" <command...>
#
set -o pipefail

label="$1"
shift

log="${RUNNER_TEMP:-/tmp}/cargo-test-$(printf '%s' "$label" | tr -c 'A-Za-z0-9' '-').log"

status=0
"$@" 2>&1 | tee "$log" || status=$?

if [ "$status" -ne 0 ]; then
  failed="$(grep -E '^test .+ \.\.\. FAILED$' "$log" | sed -e 's/^test //' -e 's/ \.\.\. FAILED$//' || true)"
  if [ -n "$failed" ]; then
    printf 'FAILED TESTS (%s):\n%s\n' "$label" "$failed"
    diagnostic="$failed"
    diagnostic="${diagnostic//'%'/'%25'}"
    diagnostic="${diagnostic//$'\r'/'%0D'}"
    diagnostic="${diagnostic//$'\n'/'%0A'}"
    echo "::error title=${label}: failing tests::${diagnostic}"
  else
    # Deliberately distinguished. No named test means this was not an assertion --
    # a build error, a panic outside a test body, or a killed process -- and that
    # is a different investigation from "which test broke".
    echo "::error title=${label}: failed with no named test::exited ${status} but no line matched '^test ... FAILED', so this is not a failing assertion. Look for a build error, a panic outside a test body, or a killed process."
  fi
fi

exit "$status"
