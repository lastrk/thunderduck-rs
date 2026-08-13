#!/usr/bin/env bash
# Download and cache DuckDB's official static release archive for one target.
set -euo pipefail

DUCKDB_VERSION="1.5.5"

common_git="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
[[ -n "$common_git" ]] || { echo "duckdb-cache: not in a git repository" >&2; exit 1; }
MAIN_ROOT="$(dirname "$common_git")"
CACHE_ROOT="${BUILD_CACHE_ROOT:-$MAIN_ROOT/.build-cache}"
target="${2:-${TARGET:-$(rustc -vV | awk '/^host: /{print $2}')}}"

case "$target" in
  x86_64-unknown-linux-gnu)
    asset="static-libs-linux-amd64.zip"
    expected_sha="deb47c5300f3c99725e84cdb14d214c3b12bbd748b613b1698b938c894cb68eb"
    ;;
  aarch64-unknown-linux-gnu)
    asset="static-libs-linux-arm64.zip"
    expected_sha="ea6a34cb49ec2db5ed23d9e8311237c53c32abf9cdbf5dd608c4176c3dd8bfeb"
    ;;
  x86_64-apple-darwin)
    asset="static-libs-osx-amd64.zip"
    expected_sha="a27d36fa1247a3ffa1692e7aa0bf4ea4d1e0ee51da7c4df7a5db5217357b1b4d"
    ;;
  aarch64-apple-darwin)
    asset="static-libs-osx-arm64.zip"
    expected_sha="d79ec66b8a4054b866faada82e9e31f859a713c555b3f1c4b71c4a43d3273e9c"
    ;;
  *)
    echo "duckdb-cache: unsupported target: $target" >&2
    exit 1
    ;;
esac

DIR="$CACHE_ROOT/duckdb/v$DUCKDB_VERSION/$target"
LIB="$DIR/lib/libduckdb_static.a"
HEADER="$DIR/include/duckdb.h"
MARKER="$DIR/.archive-sha256"
DOWNLOAD_DIR="$CACHE_ROOT/downloads"
ARCHIVE="$DOWNLOAD_DIR/duckdb-v$DUCKDB_VERSION-$asset"
URL="https://github.com/duckdb/duckdb/releases/download/v$DUCKDB_VERSION/$asset"

log() { printf '[duckdb-cache] %s\n' "$*" >&2; }

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

download() {
  mkdir -p "$DOWNLOAD_DIR"
  if [[ -f "$ARCHIVE" ]] && [[ "$(sha256 "$ARCHIVE")" == "$expected_sha" ]]; then
    return
  fi

  local tmp="$ARCHIVE.tmp.$$"
  log "downloading DuckDB v$DUCKDB_VERSION $asset"
  curl -fsSL --retry 3 --max-time 300 "$URL" -o "$tmp"
  if [[ "$(sha256 "$tmp")" != "$expected_sha" ]]; then
    rm -f "$tmp"
    log "checksum verification failed for $asset"
    return 1
  fi
  mv -f "$tmp" "$ARCHIVE"
}

prepare() {
  if [[ -f "$LIB" && -f "$HEADER" && -f "$MARKER" ]] \
      && [[ "$(<"$MARKER")" == "$expected_sha" ]]; then
    log "already cached: DuckDB v$DUCKDB_VERSION for $target"
    return
  fi

  command -v unzip >/dev/null || { log "unzip is required"; return 1; }
  download

  mkdir -p "$DIR"
  local work
  work="$(mktemp -d "$DIR/.prepare.XXXXXX")"
  trap 'rm -rf "$work"' RETURN
  unzip -q "$ARCHIVE" -d "$work/extracted"

  if [[ "$target" == *-apple-darwin ]]; then
    command -v libtool >/dev/null || { log "Apple libtool is required"; return 1; }
    libtool -static -o "$work/libduckdb_static.a" "$work"/extracted/*.a
  else
    command -v ar >/dev/null || { log "ar is required"; return 1; }
    (
      cd "$work/extracted"
      {
        echo "CREATE ../libduckdb_static.a"
        for archive in ./*.a; do echo "ADDLIB $archive"; done
        echo "SAVE"
        echo "END"
      } | ar -M
    )
  fi

  mkdir -p "$DIR/lib" "$DIR/include"
  mv -f "$work/libduckdb_static.a" "$LIB"
  cp "$work/extracted/duckdb.h" "$HEADER"
  printf '%s\n' "$expected_sha" > "$MARKER.tmp"
  mv -f "$MARKER.tmp" "$MARKER"
  log "cached official DuckDB v$DUCKDB_VERSION static library for $target"
}

case "${1:-ensure}" in
  ensure) prepare ;;
  dir) printf '%s\n' "$DIR" ;;
  *) echo "usage: duckdb-build-cache.sh {ensure|dir} [target]" >&2; exit 2 ;;
esac
