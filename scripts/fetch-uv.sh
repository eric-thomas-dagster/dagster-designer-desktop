#!/usr/bin/env bash
# Downloads uv's official release binaries (uv and uvx -- separate binaries,
# not a symlink pair) for both Mac architectures and lipo-merges each into a
# universal2 binary at vendor/uv/{uv,uvx}, so the app doesn't need uv (or,
# transitively, Python -- uv downloads its own managed Python builds on
# demand) pre-installed on the machine it runs on.
#
# Not committed to git (see .gitignore) -- run this before `tauri build`,
# same idea as `npm install` before a JS build. tauri.conf.json's
# bundle.resources then copies vendor/uv/{uv,uvx} into the app bundle, and
# the backend's find_uv_binary() (app/core/uv_binary.py) resolves to the
# bundled copy instead of relying on uv/uvx being on PATH.
set -euo pipefail

VERSION="${UV_VERSION:-latest}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENDOR_DIR="$SCRIPT_DIR/../vendor/uv"

# Runs on every `app:build`; skip the ~50MB re-download once both binaries
# are already there. Pass FORCE=1 to refetch (e.g. to pick up a new release).
if [ -x "$VENDOR_DIR/uv" ] && [ -x "$VENDOR_DIR/uvx" ] && [ "${FORCE:-}" != "1" ]; then
  echo "vendor/uv/{uv,uvx} already present, skipping (FORCE=1 to refetch)"
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

# Downloads and extracts one target triple's release tarball, echoing its
# directory (contains both the uv and uvx binaries) on stdout.
fetch() {
  local target="$1"
  local out="$WORK_DIR/$target"
  mkdir -p "$out"
  echo "Fetching uv for $target..." >&2
  curl -sL --fail --max-time 120 "$(url_for "$target")" -o "$out/uv.tar.gz"
  tar -xzf "$out/uv.tar.gz" -C "$out"
  echo "$out/uv-${target}"
}

mkdir -p "$VENDOR_DIR"
ARM64_DIR="$(fetch aarch64-apple-darwin)"
X86_64_DIR="$(fetch x86_64-apple-darwin)"

echo "Merging into universal2 binaries..."
for bin in uv uvx; do
  lipo -create -output "$VENDOR_DIR/$bin" "$ARM64_DIR/$bin" "$X86_64_DIR/$bin"
  chmod +x "$VENDOR_DIR/$bin"
  echo "Done: $VENDOR_DIR/$bin"
  lipo -info "$VENDOR_DIR/$bin"
done

"$VENDOR_DIR/uv" --version
"$VENDOR_DIR/uvx" --version
