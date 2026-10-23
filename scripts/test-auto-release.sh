#!/usr/bin/env bash
# Checks scripts/auto-release.sh's decisions against a fake `gh`, one scenario at a time.
set -uo pipefail
cd "$(dirname "$0")"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
cat > "$work/bin/gh" <<'STUB'
#!/usr/bin/env bash
args="$*"
case "$args" in
  "api "*"--jq .sha")                       echo "$HEAD_SHA" ;;
  "api "*"--jq .commit.committer.date")     echo "$HEAD_DATE" ;;
  "release view "*)                         printf '%s' "$RELEASE_BODY" ;;
  "run list "*"--limit 1"*)                 echo "$LAST_RUN" ;;
  "run list "*)                             echo "$ACTIVE_RUNS" ;;
  "workflow run "*)                         echo "dispatched" >> "$CALLS" ;;
  *) echo "unexpected gh call: $args" >&2; exit 99 ;;
esac
STUB
chmod +x "$work/bin/gh"

fail=0
run_case() {  # name, expect (dispatch|none), then env assignments
  local name=$1 expect=$2; shift 2
  : > "$work/calls"
  env PATH="$work/bin:$PATH" CALLS="$work/calls" THIS_REPO=o/desktop MAIN_REPO=o/product \
      HEAD_SHA=abcdef1234567890 HEAD_DATE=2026-10-23T12:00:00Z \
      RELEASE_BODY=$'## What'"'"'s new\n- x\n\nBuilt from dagster_designer@1111111' \
      ACTIVE_RUNS=0 LAST_RUN="success 2026-10-23T10:00:00Z" "$@" ./auto-release.sh > "$work/out" 2>&1
  local rc=$? got=none; [ -s "$work/calls" ] && got=dispatch
  if [ $rc -ne 0 ] || [ "$got" != "$expect" ]; then
    echo "FAIL  $name: expected $expect, got $got (exit $rc)"; sed 's/^/        /' "$work/out"; fail=1
  else
    echo "ok    $name"
  fi
}

run_case "latest release already built from the newest commit"  none     RELEASE_BODY="Built from dagster_designer@abcdef1"
run_case "a new product commit nobody released"                 dispatch
run_case "no commit recorded in the release (first run)"        dispatch RELEASE_BODY="just notes"
run_case "a release is already running"                         none     ACTIVE_RUNS=1
run_case "a release is queued behind another"                   none     ACTIVE_RUNS=2
run_case "last build failed after the newest commit: wait"      none     LAST_RUN="failure 2026-10-23T13:00:00Z"
run_case "last build was cancelled after the newest commit"     none     LAST_RUN="cancelled 2026-10-23T13:00:00Z"
run_case "last build failed BEFORE the newest commit: retry"    dispatch LAST_RUN="failure 2026-10-23T11:00:00Z"
run_case "no release runs yet"                                  dispatch LAST_RUN="none 1970-01-01T00:00:00Z"
run_case "full sha recorded in the release"                     none     RELEASE_BODY="Built from dagster_designer@abcdef1234567890"
exit $fail
