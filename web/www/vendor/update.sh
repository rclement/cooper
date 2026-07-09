#!/usr/bin/env bash
# Re-vendors the third-party packages under web/www/vendor/ — deterministic
# replacement for hand-rolling `npm pack` + copy + (for duckdb-wasm) an
# esbuild bundle step every time a version needs bumping. See ./README.md
# for what's vendored, why, and any package-specific notes.
#
# Usage:
#   ./update.sh                      # re-vendor everything at the pinned versions below
#   ./update.sh marked                # re-vendor just one package at its pinned version
#   ./update.sh marked 18.1.0         # re-vendor just one package at an explicit version
#
# Bumping a version: edit the *_VERSION variable below, run
# `./update.sh <package>`, then update the version in ./README.md's table
# (and, for duckdb-wasm, re-check the mvp/eh/coi tradeoff notes there still
# hold). Requires npm and curl on PATH.
set -euo pipefail

MARKED_VERSION="18.0.5"
DOMPURIFY_VERSION="3.4.11"
WLLAMA_VERSION="3.5.1"
PYODIDE_VERSION="0.29.4"
DUCKDB_WASM_VERSION="1.29.0"
ESBUILD_VERSION="0.24.0"

VENDOR_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

log() { echo "==> $*"; }

# Downloads and extracts <pkg>@<version> into a fresh subdirectory of
# $SCRATCH, echoing the path to the extracted package/ directory.
fetch_npm_package() {
  local pkg="$1" version="$2"
  local dir
  dir="$(mktemp -d "$SCRATCH/pkg.XXXXXX")"
  (cd "$dir" && npm pack "${pkg}@${version}" --silent >/dev/null && tar xzf ./*.tgz && rm -f ./*.tgz)
  echo "$dir/package"
}

vendor_marked() {
  local version="${1:-$MARKED_VERSION}"
  log "marked@$version"
  local pkg
  pkg="$(fetch_npm_package marked "$version")"
  cp "$pkg/lib/marked.esm.js" "$VENDOR_DIR/marked/marked.esm.js"
  cp "$pkg/LICENSE" "$VENDOR_DIR/marked/LICENSE"
}

vendor_dompurify() {
  local version="${1:-$DOMPURIFY_VERSION}"
  log "dompurify@$version"
  local pkg
  pkg="$(fetch_npm_package dompurify "$version")"
  cp "$pkg/dist/purify.es.mjs" "$VENDOR_DIR/dompurify/purify.es.mjs"
  cp "$pkg/LICENSE" "$VENDOR_DIR/dompurify/LICENSE"
}

vendor_wllama() {
  local version="${1:-$WLLAMA_VERSION}"
  log "@wllama/wllama@$version"
  local pkg
  pkg="$(fetch_npm_package "@wllama/wllama" "$version")"
  cp "$pkg/esm/index.js" "$VENDOR_DIR/wllama/index.js"
  mkdir -p "$VENDOR_DIR/wllama/wasm"
  cp "$pkg/esm/wasm/wllama.wasm" "$VENDOR_DIR/wllama/wasm/wllama.wasm"
  cp "$pkg/LICENCE" "$VENDOR_DIR/wllama/LICENCE"
}

vendor_pyodide() {
  local version="${1:-$PYODIDE_VERSION}"
  log "pyodide@$version"
  local pkg out
  pkg="$(fetch_npm_package pyodide "$version")"
  out="$VENDOR_DIR/pyodide"
  mkdir -p "$out"
  # Core interpreter only — package wheels (numpy, pandas, etc.) aren't part
  # of the npm distribution at all; see ./README.md#pyodide.
  for f in pyodide.mjs pyodide.asm.js pyodide.asm.wasm python_stdlib.zip pyodide-lock.json; do
    cp "$pkg/$f" "$out/$f"
  done
  # Not bundled in the npm tarball; the repo's LICENSE is the real one
  # (pyodide's overall license is MPL-2.0, despite package.json saying MIT).
  curl -sL --fail "https://raw.githubusercontent.com/pyodide/pyodide/${version}/LICENSE" -o "$out/LICENSE"
}

vendor_duckdb_wasm() {
  local version="${1:-$DUCKDB_WASM_VERSION}"
  log "@duckdb/duckdb-wasm@$version"
  local pkg out
  pkg="$(fetch_npm_package "@duckdb/duckdb-wasm" "$version")"
  out="$VENDOR_DIR/duckdb-wasm"
  mkdir -p "$out"

  # The npm build's browser ESM entrypoint imports "apache-arrow" as a bare
  # specifier rather than inlining it, so a plain copy wouldn't be
  # dependency-free like the rest of vendor/. Bundle it in with esbuild,
  # pinned to the exact semver range duckdb-wasm itself depends on.
  local arrow_range bundle_dir
  arrow_range="$(node -p "require('$pkg/package.json').dependencies['apache-arrow']")"
  bundle_dir="$(mktemp -d "$SCRATCH/duckdb-bundle.XXXXXX")"
  (
    cd "$bundle_dir"
    npm init -y --silent >/dev/null
    npm install --silent "apache-arrow@${arrow_range}" "esbuild@${ESBUILD_VERSION}" >/dev/null
    cp "$pkg/dist/duckdb-browser.mjs" .
    ./node_modules/.bin/esbuild duckdb-browser.mjs --bundle --format=esm --outfile=duckdb-browser.esm.js
  )
  cp "$bundle_dir/duckdb-browser.esm.js" "$out/duckdb-browser.esm.js"

  # Only "eh" and "coi" wasm variants — not "mvp"; see ./README.md#duckdb-wasm
  # for why.
  cp "$pkg/dist/duckdb-eh.wasm" "$out/duckdb-eh.wasm"
  cp "$pkg/dist/duckdb-browser-eh.worker.js" "$out/duckdb-browser-eh.worker.js"
  cp "$pkg/dist/duckdb-coi.wasm" "$out/duckdb-coi.wasm"
  cp "$pkg/dist/duckdb-browser-coi.worker.js" "$out/duckdb-browser-coi.worker.js"
  cp "$pkg/dist/duckdb-browser-coi.pthread.worker.js" "$out/duckdb-browser-coi.pthread.worker.js"

  # Not bundled in the npm tarball either; pin to the matching release tag.
  curl -sL --fail "https://raw.githubusercontent.com/duckdb/duckdb-wasm/v${version}/LICENSE" -o "$out/LICENSE"
}

usage() {
  echo "usage: $0 [marked|dompurify|wllama|pyodide|duckdb-wasm] [version]" >&2
}

main() {
  local target="${1:-all}" version="${2:-}"
  case "$target" in
    marked) vendor_marked "$version" ;;
    dompurify) vendor_dompurify "$version" ;;
    wllama) vendor_wllama "$version" ;;
    pyodide) vendor_pyodide "$version" ;;
    duckdb-wasm) vendor_duckdb_wasm "$version" ;;
    all)
      vendor_marked
      vendor_dompurify
      vendor_wllama
      vendor_pyodide
      vendor_duckdb_wasm
      ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 1 ;;
  esac
  log "done"
}

main "$@"
