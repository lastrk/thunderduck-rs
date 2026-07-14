#!/usr/bin/env bash
# duckdb-build-cache.sh — produce a shared, immutable prebuilt libduckdb once.
#
# DuckDB's bundled C++ amalgamation is the same for a given version, so we build
# it ONCE and stash the static archive + headers under the main repo's
# .build-cache/ (persistent, shared by every worktree). Local builds then link
# this prebuilt via DUCKDB_LIB_DIR / DUCKDB_STATIC (default, non-bundled), so no
# worktree ever recompiles the ~2 GB amalgamation or re-runs the ~1.9 GB `ar`.
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

log() { printf '[duckdb-cache] %s\n' "$*" >&2; }

harvest() {
  local out="$1"   # a libduckdb-sys-<hash>/out directory
  local a hdr
  a="$out/libduckdb.a"
  [[ -f "$a" ]] || { log "no libduckdb.a in $out"; return 1; }
  hdr="$(find "$out" -name duckdb.h -path '*src/include*' 2>/dev/null | head -1)"
  [[ -n "$hdr" ]] || { log "no duckdb.h under $out"; return 1; }
  mkdir -p "$DIR/lib" "$INC"
  # atomic-ish: copy to tmp then rename the lib (the presence test gate).
  cp -p "$hdr"/../*.h "$INC"/ 2>/dev/null || true
  cp -rp "$(dirname "$hdr")" "$INC/include" 2>/dev/null || true   # full src/include tree
  cp -p "$a" "$LIB.tmp"
  mv -f "$LIB.tmp" "$LIB"
  local kib; kib="$(du -k "$LIB" | cut -f1)"
  log "cached libduckdb_static.a ($(du -h "$LIB" | cut -f1)) + headers for v$ver"
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
  local hit
  hit="$(ls -t "$MAIN_ROOT"/target/release/build/libduckdb-sys-*/out/libduckdb.a \
                "$MAIN_ROOT"/.claude/worktrees/*/target/release/build/libduckdb-sys-*/out/libduckdb.a \
                2>/dev/null | head -1 || true)"
  [[ -n "$hit" ]] && dirname "$hit"
}

build_once() {
  log "no prebuilt found — building DuckDB once with --release --features bundled (one-time, slow)"
  local tmp="$CACHE_ROOT/duckdb/.build-$ver"
  rm -rf "$tmp"; mkdir -p "$tmp"
  # --release so the amalgamation is compiled -O3 (small, fast), not the 2 GB
  # debug archive. bundled feature compiles libduckdb-sys from source.
  ( cd "$MAIN_ROOT" && CARGO_TARGET_DIR="$tmp" cargo build --release --features bundled -p thunderduck-core )
  local out; out="$(dirname "$(ls -t "$tmp"/release/build/libduckdb-sys-*/out/libduckdb.a | head -1)")"
  harvest "$out"
  rm -rf "$tmp"
}

case "${1:-ensure}" in
  dir) echo "$DIR" ;;
  harvest) [[ -n "${2:-}" ]] || { echo "usage: harvest <out-dir>" >&2; exit 2; }; harvest "$2" ;;
  ensure)
    if [[ -f "$LIB" ]]; then log "already cached for v$ver: $LIB"; exit 0; fi
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
