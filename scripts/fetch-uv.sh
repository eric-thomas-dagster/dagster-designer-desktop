#!/usr/bin/env bash
# Downloads uv's official release binaries for both Mac architectures and
# lipo-merges them into one universal2 binary at vendor/uv/uv, so the app
# doesn't need `uv` (or, transitively, Python -- uv downloads its own
# managed Python builds on demand) pre-installed on the machine it runs on.
#
# Not committed to git (see .gitignore) -- run this before `tauri build`,
# same idea as `npm install` before a JS build. tauri.conf.json's
# bundle.resources then copies vendor/uv/uv into the app bundle, and
# main.rs's spawn_backend() calls it by its bundled path instead of
# relying on `uv` being on PATH.
set -euo pipefail

VERSION="${UV_VERSION:-latest}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENDOR_DIR="$SCRIPT_DIR/../vendor/uv"

# Runs on every `app:build`; skip the ~50MB re-download once it's already
# there. Pass FORCE=1 to refetch (e.g. to pick up a new uv release).
if [ -x "$VENDOR_DIR/uv" ] && [ "${FORCE:-}" != "1" ]; then
  echo "vendor/uv/uv already present, skipping (FORCE=1 to refetch)"
  exit 0
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

url_for() {
  local target="$1"
  if [ "$VERSION" = "latest" ]; then
    echo "https://github.com/astral-sh/uv/releases/latest/download/uv-${target}.tar.gz"
  else
    echo "https://github.com/astral-sh/uv/releases/download/${VERSION}/uv-${target}.tar.gz"
  fi
}

fetch() {
  local target="$1"
  local out="$WORK_DIR/$target"
  mkdir -p "$out"
  echo "Fetching uv for $target..." >&2
  curl -sL --fail --max-time 120 "$(url_for "$target")" -o "$out/uv.tar.gz"
  tar -xzf "$out/uv.tar.gz" -C "$out"
  echo "$out/uv-${target}/uv"
}

mkdir -p "$VENDOR_DIR"
ARM64_BIN="$(fetch aarch64-apple-darwin)"
X86_64_BIN="$(fetch x86_64-apple-darwin)"

echo "Merging into a universal2 binary..."
lipo -create -output "$VENDOR_DIR/uv" "$ARM64_BIN" "$X86_64_BIN"
chmod +x "$VENDOR_DIR/uv"

echo "Done: $VENDOR_DIR/uv"
lipo -info "$VENDOR_DIR/uv"
"$VENDOR_DIR/uv" --version
