#!/usr/bin/env bash
#
# delta-build.sh — the hot loop of the cross-repo Delta dev loop.
#
# Builds the duckdb-delta extension against our LOCAL delta-kernel-rs checkout
# (.delta-kernel-rs) instead of the pinned GIT_TAG, linked against DuckDB v1.5.5
# (thunderduck's ABI anchor). On success it prints the built loadable extension
# path and the `export THUNDERDUCK_DELTA_EXT_PATH=...` line to feed the server's
# dev-load hook.
#
# Typical loop:  edit .delta-kernel-rs -> ./scripts/dev/delta-build.sh -> restart server -> test
#
# Usage: delta-build.sh [debug|release]   (default: release)
# See docs/context/delta-cross-repo-dev-loop.md.
set -euo pipefail

BUILD_TYPE="${1:-release}"
case "$BUILD_TYPE" in
  release|debug) ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
KERNEL_DIR="$ROOT/.delta-kernel-rs"
DELTA_DIR="$ROOT/.duckdb-delta"

log() { printf '\033[1;34m[delta-build]\033[0m %s\n' "$*"; }

if [[ ! -d "$DELTA_DIR/.git" || ! -d "$KERNEL_DIR/.git" ]]; then
  echo "checkouts missing — run scripts/dev/delta-dev-setup.sh first" >&2
  exit 1
fi

log "building duckdb-delta ($BUILD_TYPE) against local kernel: $KERNEL_DIR"
log "duckdb submodule @ $(git -C "$DELTA_DIR/duckdb" describe --tags 2>/dev/null || echo '?') — must be v1.5.5"

# Userspace gcc toolchain. The devcontainer's gcc 11 can't compile the extension
# (self-referential unordered_map — newer libstdc++ required), and apt is
# unavailable, so we build with a conda-forge gcc bootstrapped by
# delta-toolchain-setup.sh. Auto-bootstrap on first run.
TCENV="$ROOT/.delta-toolchain/env"
if [[ ! -d "$TCENV" ]]; then
  log "userspace toolchain missing — bootstrapping (one-time)"
  "$(dirname "${BASH_SOURCE[0]}")/delta-toolchain-setup.sh"
fi
CXX_BIN="$(ls "$TCENV"/bin/*-linux-gnu-g++ 2>/dev/null | head -1)"
CC_BIN="$(ls "$TCENV"/bin/*-linux-gnu-gcc 2>/dev/null | head -1)"
if [[ -z "$CXX_BIN" || -z "$CC_BIN" ]]; then
  echo "toolchain not usable under $TCENV — run scripts/dev/delta-toolchain-setup.sh" >&2
  exit 1
fi
export CC="$CC_BIN" CXX="$CXX_BIN"
# Statically link the newer libstdc++/libgcc INTO the extension so it loads into
# thunderduck's gcc-11 process (which has the older system libstdc++). CMake
# initializes its linker-flag caches from $LDFLAGS on first configure.
export LDFLAGS="${LDFLAGS:-} -static-libstdc++ -static-libgcc"
log "toolchain: $CXX_BIN ($("$CXX_BIN" -dumpversion 2>/dev/null)), static libstdc++/libgcc"

# CMake pins the compiler in its cache at first configure; if a prior build used
# a different compiler (e.g. the system gcc 11), wipe the build dir so it
# reconfigures cleanly against the toolchain compiler.
CACHE="$DELTA_DIR/build/$BUILD_TYPE/CMakeCache.txt"
if [[ -f "$CACHE" ]] && ! grep -qF "$CXX_BIN" "$CACHE"; then
  log "compiler changed since last configure — clearing build/$BUILD_TYPE"
  rm -rf "$DELTA_DIR/build/$BUILD_TYPE"
fi

# Resource caps. The devcontainer is hard-capped at 8 GiB (cgroup v2
# memory.max), and linking DuckDB's static lib + binaries is the RAM hog, so an
# unthrottled build gets OOM-killed. We pin:
#   - linker parallelism to 2 via DuckDB's own OOM-avoidance knob
#     (DUCKDB_RELEASE/DEBUG_LINK_JOBS -> a Ninja JOB_POOL, see duckdb CMakeLists),
#   - compile + cargo parallelism to a modest level so concurrent g++/rustc plus
#     the 2 in-flight links stay under 8 GiB.
# Override on roomier machines via DELTA_BUILD_LINK_JOBS / DELTA_BUILD_COMPILE_JOBS.
LINK_JOBS="${DELTA_BUILD_LINK_JOBS:-2}"
# 3 (not more) because the conda gcc-13 uses more RAM/process than the system
# gcc 11; with 2 concurrent links this keeps peak RSS comfortably under 8 GiB.
COMPILE_JOBS="${DELTA_BUILD_COMPILE_JOBS:-3}"
export CMAKE_BUILD_PARALLEL_LEVEL="$COMPILE_JOBS"   # C++ compile workers (Ninja)
export CARGO_BUILD_JOBS="$COMPILE_JOBS"             # kernel FFI rustc workers

log "resource caps: compile=$COMPILE_JOBS  linker=$LINK_JOBS  (fits the 8 GiB container cap)"

# GEN=ninja: faster than the default Make generator; also required for the
#   linker JOB_POOL throttle to take effect.
# EXT_FLAGS: the documented extension-ci-tools hook for extra -D cmake vars;
#   chosen over TOOLCHAIN_FLAGS so we don't clobber the makefile's vcpkg appends.
make -C "$DELTA_DIR" "$BUILD_TYPE" \
  GEN=ninja \
  EXT_FLAGS="-DDELTA_KERNEL_LOCAL_DIR=${KERNEL_DIR} -DDUCKDB_RELEASE_LINK_JOBS=${LINK_JOBS} -DDUCKDB_DEBUG_LINK_JOBS=${LINK_JOBS}"

# Locate the loadable extension (name is `delta` per CMake TARGET_NAME).
EXT="$(find "$DELTA_DIR/build/$BUILD_TYPE" -name 'delta.duckdb_extension' -type f 2>/dev/null | head -1)"
if [[ -z "$EXT" ]]; then
  echo "build finished but delta.duckdb_extension not found under build/$BUILD_TYPE" >&2
  exit 1
fi

log "built: $EXT"
echo
echo "  export THUNDERDUCK_DELTA_EXT_PATH='$EXT'"
echo
log "then restart the server; sessions will LOAD this extension (delta_scan available)."
