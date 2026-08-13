#!/usr/bin/env bash
# duckdb-build-cache.sh — inspect legacy local static archives.
#
# DuckDB 1.5.5 clean builds download the exact official library. This helper
# retains its inspection interface for older worktrees, but does not make a
# static archive for the current version because it is incompatible with the
# extension-loading configuration.
#
# Layout:  .build-cache/duckdb/<libduckdb-sys-version>/
#            lib/libduckdb_static.a     (link name expected by DUCKDB_STATIC=1)
#            include/                    (duckdb.h etc., for DUCKDB_INCLUDE_DIR)
#
# Usage:
#   duckdb-build-cache.sh ensure        # cache it if missing (harvest or build)
#   duckdb-build-cache.sh dir           # print the cache dir for the current version
#   duckdb-build-cache.sh harvest FROM  # harvest from a given libduckdb-sys out dir
set -euo pipefail

common_git="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
[[ -z "$common_git" ]] && { echo "duckdb-build-cache: not in a git repo" >&2; exit 1; }
MAIN_ROOT="$(dirname "$common_git")"
CACHE_ROOT="${BUILD_CACHE_ROOT:-$MAIN_ROOT/.build-cache}"

lockfile="$MAIN_ROOT/Cargo.lock"; [[ -f "$PWD/Cargo.lock" ]] && lockfile="$PWD/Cargo.lock"
ver="$(awk '/^name = "libduckdb-sys"/{f=1} f&&/^version = /{gsub(/[",]/,"",$3);print $3;exit}' "$lockfile" 2>/dev/null || true)"
[[ -z "$ver" ]] && { echo "duckdb-build-cache: cannot read libduckdb-sys version (build/resolve first)" >&2; exit 1; }
DIR="$CACHE_ROOT/duckdb/$ver"
LIB="$DIR/lib/libduckdb_static.a"
INC="$DIR/include"

crate_minor="$(printf '%s' "$ver" | awk -F. '{print $2}')"
[[ "$crate_minor" =~ ^[0-9]{5,}$ ]] || {
  echo "duckdb-build-cache: unexpected libduckdb-sys version '$ver'" >&2
  exit 1
}
duckdb_patch="${crate_minor: -2}"
duckdb_minor="${crate_minor: -4:2}"
duckdb_major="${crate_minor:0:$((${#crate_minor} - 4))}"
expected_duckdb_version="v${duckdb_major}.${duckdb_minor#0}.${duckdb_patch#0}"

log() { printf '[duckdb-cache] %s\n' "$*" >&2; }

archive_matches_expected_version() {
  local version_object
  version_object="$(ar t "$1" | grep -E '(pragma_version\.o$|func_table_version\.cpp\.o$)' | head -1 || true)"
  [[ -n "$version_object" ]] \
    && ar p "$1" "$version_object" | strings | grep -Fx "$expected_duckdb_version" >/dev/null
}

harvest() {
  local out="$1"   # a libduckdb-sys-<hash>/out directory
  local a hdr
  a="$out/libduckdb.a"
  [[ -f "$a" ]] || { log "no libduckdb.a in $out"; return 1; }
  archive_matches_expected_version "$a" || {
    log "libduckdb.a is not $expected_duckdb_version: $a"
    return 1
  }
  hdr="$(find "$out" -name duckdb.h -path '*src/include*' 2>/dev/null | head -1)"
  [[ -n "$hdr" ]] || { log "no duckdb.h under $out"; return 1; }
  mkdir -p "$DIR/lib" "$INC"
  # atomic-ish: copy to tmp then rename the lib (the presence test gate).
  cp -p "$hdr"/../*.h "$INC"/ 2>/dev/null || true
  cp -rp "$(dirname "$hdr")" "$INC/include" 2>/dev/null || true   # full src/include tree
  cp -p "$a" "$LIB.tmp"
  mv -f "$LIB.tmp" "$LIB"
  local kib; kib="$(du -k "$LIB" | cut -f1)"
  log "cached libduckdb_static.a ($(du -h "$LIB" | cut -f1)) + headers for $expected_duckdb_version (crate v$ver)"
  # A release (-O3) archive is ~100 MB; a debug (-O0+debuginfo) one is ~2 GB.
  if [[ "$kib" -gt 524288 ]]; then
    log "WARN: archive >512 MB — this looks like a DEBUG build. Re-run after a release build for a small/fast lib."
  fi
}

find_existing_out() {
  # Only harvest from a RELEASE build: the cc crate compiles the amalgamation
  # with the consuming profile's opt-level, so a debug build yields a ~2 GB
  # -O0 archive while release is a ~100 MB -O3 one. The lib is an external C-ABI
  # archive, so the optimized build links fine into debug Rust builds too.
  local archive
  while IFS= read -r archive; do
    if archive_matches_expected_version "$archive"; then
      dirname "$archive"
      return
    fi
  done < <(ls -t "$MAIN_ROOT"/target/release/build/libduckdb-sys-*/out/libduckdb.a \
                "$MAIN_ROOT"/.claude/worktrees/*/target/release/build/libduckdb-sys-*/out/libduckdb.a \
                2>/dev/null || true)
}

build_once() {
  log "no compatible static archive is available for $expected_duckdb_version"
  log "clean builds download the official libduckdb release instead"
}

case "${1:-ensure}" in
  dir) echo "$DIR" ;;
  harvest) [[ -n "${2:-}" ]] || { echo "usage: harvest <out-dir>" >&2; exit 2; }; harvest "$2" ;;
  ensure)
    if [[ -f "$LIB" ]] && archive_matches_expected_version "$LIB"; then
      log "already cached for $expected_duckdb_version (crate v$ver): $LIB"
      exit 0
    fi
    if [[ -f "$LIB" ]]; then
      log "cached archive has the wrong DuckDB version; rebuilding $expected_duckdb_version"
    fi
    existing="$(find_existing_out || true)"
    if [[ -n "$existing" ]]; then
      log "harvesting existing build: $existing"
      harvest "$existing"
    else
      build_once
    fi
    ;;
  *) echo "usage: duckdb-build-cache.sh {ensure|dir|harvest <out-dir>}" >&2; exit 2 ;;
esac
