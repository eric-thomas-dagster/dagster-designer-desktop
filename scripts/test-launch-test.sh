#!/usr/bin/env bash
# Tests scripts/launch-test.sh against fake bundles: it must pass a healthy one and fail, with a useful message, on each way a
# real bundle can be broken.
set -uo pipefail
cd "$(dirname "$0")"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
fail=0

# A fake bundle. $1 = name, $2 = what the fake uv does: serve | crash | silent
make_bundle() {
  local dir="$work/$1"
  mkdir -p "$dir/bin" "$dir/backend/app"
  echo "x" > "$dir/backend/app/main.py"; echo "x" > "$dir/backend/uv.lock"
  cat > "$dir/bin/uv" <<STUB
#!/usr/bin/env bash
[ "\$1" = "--version" ] && { echo "uv 0.0-fake"; exit 0; }
[ "\$1" = "run" ] && [ "\$2" = "python" ] && { shift 2; exec python3 "\$@"; }
case "$2" in
  crash)  echo "ModuleNotFoundError: No module named 'fastapi'" >&2; exit 1 ;;
  silent) sleep 600 ;;
  serve)
    while [ "\$1" != "--port" ]; do shift; done; port="\$2"
    exec python3 - "\$port" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.end_headers()
        self.wfile.write(json.dumps({"status": "ok"} if self.path == "/health" else []).encode())
HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
    ;;
esac
STUB
  chmod +x "$dir/bin/uv"
  echo "$dir"
}

check() {  # name, expected exit (0|1), expected text in the output, then the command
  local name=$1 want=$2 text=$3; shift 3
  local out; out=$("$@" 2>&1); local rc=$?
  [ "$rc" -ne 0 ] && rc=1
  if [ "$rc" != "$want" ] || ! grep -q -- "$text" <<<"$out"; then
    echo "FAIL  $name (exit $rc, wanted $want; looking for: $text)"; sed 's/^/        /' <<<"$out" | tail -8; fail=1
  else
    echo "ok    $name"
  fi
}

export PORT=18799
check "a healthy bundle passes"                         0 "LAUNCH TEST PASSED"          env TIMEOUT=30 ./launch-test.sh "$(make_bundle good serve)"
rel_bundle=$(make_bundle rel serve)
check "a RELATIVE resource path works (CI passes one)"   0 "LAUNCH TEST PASSED"          env TIMEOUT=30 bash -c "cd '$work' && '$PWD/launch-test.sh' rel"
check "a missing resource dir is reported"               1 "no such resource dir"        env TIMEOUT=5 ./launch-test.sh "$work/nope"
check "a backend that crashes at start is reported"     1 "exited before it came up"   env TIMEOUT=30 ./launch-test.sh "$(make_bundle crash crash)"
check "the crash reason is shown from the log"          1 "No module named 'fastapi'"   env TIMEOUT=30 ./launch-test.sh "$(make_bundle crash2 crash)"
check "a backend that never answers times out"          1 "no answer from /health"      env TIMEOUT=4 ./launch-test.sh "$(make_bundle quiet silent)"

d=$(make_bundle nouv serve); rm -f "$d/bin/uv"
check "a missing uv is caught"                          1 "uv is missing"               ./launch-test.sh "$d"
d=$(make_bundle winuv serve); mv "$d/bin/uv" "$d/bin/uv.exe"
check "a Windows-style uv.exe is found"                 0 "LAUNCH TEST PASSED"          env TIMEOUT=30 ./launch-test.sh "$d"
d=$(make_bundle nomain serve); rm -f "$d/backend/app/main.py"
check "a missing app/main.py is caught"                 1 "missing app/main.py"         ./launch-test.sh "$d"
d=$(make_bundle nolock serve); rm -f "$d/backend/uv.lock"
check "a missing uv.lock is caught"                     1 "missing uv.lock"             ./launch-test.sh "$d"
d=$(make_bundle noexec serve); chmod -x "$d/bin/uv"
check "a uv that isn't executable is caught"            1 "uv is not executable" ./launch-test.sh "$d"
check "no argument prints usage"                        1 "usage"                       ./launch-test.sh
exit $fail
