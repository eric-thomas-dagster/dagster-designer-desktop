#!/usr/bin/env bash
# Starts the backend from a BUILT app's resources exactly the way the app does -- the bundled uv, the bundled backend folder,
# the same environment variables -- and checks that it comes up and answers. It catches what unit tests can't: a file missing
# from the bundle, a uv that won't run, a dependency that won't install on this OS, a backend that fails to import.
#
# usage: launch-test.sh <resource dir>      e.g. ".../Dagster Designer.app/Contents/Resources"
# env:   PORT (default 18765), TIMEOUT seconds to wait for the first start (default 900: uv installs Python and dependencies)
set -uo pipefail

RES="${1:?usage: launch-test.sh <resource dir>}"
PORT="${PORT:-18765}"
TIMEOUT="${TIMEOUT:-900}"
UV="$RES/bin/uv"
[ -e "$UV" ] || { [ -e "$UV.exe" ] && UV="$UV.exe"; }   # Windows names it uv.exe
BACKEND="$RES/backend"

fail() { echo "LAUNCH TEST FAILED: $*" >&2; exit 1; }

[ -e "$UV" ] || fail "the bundled uv is missing: $RES/bin/uv"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) ;;  # Windows has no executable bit; running it below is the real check
  *) [ -x "$UV" ] || fail "the bundled uv is not executable: $UV" ;;
esac
[ -f "$BACKEND/app/main.py" ] || fail "the bundled backend is missing app/main.py: $BACKEND"
[ -f "$BACKEND/uv.lock" ] || fail "the bundled backend is missing uv.lock: $BACKEND"
"$UV" --version > /dev/null || fail "the bundled uv doesn't run"

work=$(mktemp -d)
log="$work/backend.log"
cd "$BACKEND" || fail "can't enter $BACKEND"
DATA_DIR="$work/data" PROJECTS_DIR="$work/projects" PYTHONUTF8=1 \
  "$UV" run uvicorn app.main:app --host 127.0.0.1 --port "$PORT" > "$log" 2>&1 &
pid=$!
cleanup() { kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; }
trap cleanup EXIT

show_log() { echo "---- last lines of the backend log ----" >&2; tail -40 "$log" >&2; }

start=$(date +%s)
until curl -fsS "http://127.0.0.1:$PORT/health" > /dev/null 2>&1; do
  kill -0 "$pid" 2>/dev/null || { show_log; fail "the backend exited before it came up"; }
  [ $(( $(date +%s) - start )) -lt "$TIMEOUT" ] || { show_log; fail "no answer from /health after ${TIMEOUT}s"; }
  sleep 3
done
echo "backend up after $(( $(date +%s) - start ))s"

# Past /health: routes that touch the app's own code and settings, not just the web server.
for path in /api/v1/ai/providers /api/v1/projects/summary; do
  body=$(curl -fsS "http://127.0.0.1:$PORT$path") || { show_log; fail "GET $path failed"; }
  echo "$body" | "$UV" run python -c 'import json,sys; json.load(sys.stdin)' 2>/dev/null || fail "GET $path did not return JSON: ${body:0:200}"
  echo "ok  GET $path"
done
echo "LAUNCH TEST PASSED"
