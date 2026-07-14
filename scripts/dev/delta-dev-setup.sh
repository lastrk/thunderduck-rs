#!/usr/bin/env bash
#
# delta-dev-setup.sh — materialize the cross-repo Delta dev loop.
#
# Creates two gitignored local checkouts of *our forks* at the thunderduck repo
# root and wires them so a custom delta-kernel-rs can replace the stable
# dependency of duckdb-delta:
#
#   .delta-kernel-rs/   fork lastrk/delta-kernel-rs, branch off tag v0.21.0
#                       (the FFI version duckdb-delta currently compiles against)
#   .duckdb-delta/      fork lastrk/duckdb-delta, branch off v1.5-variegata
#                       (DuckDB 1.5.x line); its `duckdb` submodule pinned to the
#                       v1.5.4 tag == thunderduck's linked libduckdb (ABI anchor).
#
# The duckdb-delta CMakeLists is patched to accept -DDELTA_KERNEL_LOCAL_DIR so
# the build uses our local kernel checkout instead of cloning the pinned tag.
#
# Idempotent: safe to re-run. See docs/context/delta-cross-repo-dev-loop.md.
set -euo pipefail

# ---- config ---------------------------------------------------------------
KERNEL_FORK="lastrk/delta-kernel-rs"
KERNEL_UPSTREAM="delta-io/delta-kernel-rs"
KERNEL_BASE_TAG="v0.21.0"

DELTA_FORK="lastrk/duckdb-delta"
DELTA_UPSTREAM="duckdb/duckdb-delta"
DELTA_BASE_BRANCH="v1.5-variegata"

DUCKDB_ABI_TAG="v1.5.4"        # must match thunderduck's duckdb crate (1.10504.0)
WORK_BRANCH="thunderduck-delta-dev"

# ---- locate repo root -----------------------------------------------------
ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
KERNEL_DIR="$ROOT/.delta-kernel-rs"
DELTA_DIR="$ROOT/.duckdb-delta"

log() { printf '\033[1;34m[delta-setup]\033[0m %s\n' "$*"; }

# ---- 1. delta-kernel-rs ---------------------------------------------------
if [[ ! -d "$KERNEL_DIR/.git" ]]; then
  log "cloning $KERNEL_FORK -> .delta-kernel-rs"
  gh repo clone "$KERNEL_FORK" "$KERNEL_DIR" -- -q
else
  log ".delta-kernel-rs already present"
fi
git -C "$KERNEL_DIR" remote get-url upstream >/dev/null 2>&1 \
  || git -C "$KERNEL_DIR" remote add upstream "https://github.com/$KERNEL_UPSTREAM.git"
log "fetching kernel tags"
git -C "$KERNEL_DIR" fetch --quiet --tags upstream
if git -C "$KERNEL_DIR" rev-parse --verify --quiet "$WORK_BRANCH" >/dev/null; then
  log "kernel branch $WORK_BRANCH exists"
else
  log "kernel: creating $WORK_BRANCH off $KERNEL_BASE_TAG"
  git -C "$KERNEL_DIR" checkout -q -b "$WORK_BRANCH" "$KERNEL_BASE_TAG"
fi

# ---- 2. duckdb-delta ------------------------------------------------------
if [[ ! -d "$DELTA_DIR/.git" ]]; then
  log "cloning $DELTA_FORK -> .duckdb-delta"
  gh repo clone "$DELTA_FORK" "$DELTA_DIR" -- -q
else
  log ".duckdb-delta already present"
fi
git -C "$DELTA_DIR" remote get-url upstream >/dev/null 2>&1 \
  || git -C "$DELTA_DIR" remote add upstream "https://github.com/$DELTA_UPSTREAM.git"
log "fetching duckdb-delta $DELTA_BASE_BRANCH"
git -C "$DELTA_DIR" fetch --quiet upstream "$DELTA_BASE_BRANCH" --tags
if git -C "$DELTA_DIR" rev-parse --verify --quiet "$WORK_BRANCH" >/dev/null; then
  log "duckdb-delta branch $WORK_BRANCH exists"
else
  log "duckdb-delta: creating $WORK_BRANCH off upstream/$DELTA_BASE_BRANCH"
  git -C "$DELTA_DIR" checkout -q -b "$WORK_BRANCH" "upstream/$DELTA_BASE_BRANCH"
fi

# ---- 3. submodules + pin duckdb to the ABI tag ----------------------------
log "init submodule: extension-ci-tools"
git -C "$DELTA_DIR" submodule update --quiet --init extension-ci-tools
log "init submodule: duckdb"
git -C "$DELTA_DIR" submodule update --quiet --init duckdb
log "pinning duckdb submodule to $DUCKDB_ABI_TAG (thunderduck ABI anchor)"
git -C "$DELTA_DIR/duckdb" fetch --quiet --tags origin
git -C "$DELTA_DIR/duckdb" checkout -q "$DUCKDB_ABI_TAG"
log "init duckdb's own submodules (recursive)"
git -C "$DELTA_DIR/duckdb" submodule update --quiet --init --recursive

# ---- 4. patch CMakeLists for the local-kernel override --------------------
CMAKE="$DELTA_DIR/CMakeLists.txt"
if grep -q "DELTA_KERNEL_LOCAL_DIR" "$CMAKE"; then
  log "CMakeLists already carries the local-kernel override"
else
  log "patching CMakeLists.txt for -DDELTA_KERNEL_LOCAL_DIR override"
  python3 - "$CMAKE" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()

OLD_PATH = "${CMAKE_BINARY_DIR}/rust/src/delta_kernel"
NEW_PATH = "${DELTA_KERNEL_SRC_DIR}"

# 1. Repoint every hardcoded kernel source/target/header path at a variable.
#    Done BEFORE inserting the default definition so that literal is preserved.
if OLD_PATH not in s:
    sys.exit("expected kernel source path not found; upstream CMakeLists changed")
s = s.replace(OLD_PATH, NEW_PATH)

# 2. Define DELTA_KERNEL_SRC_DIR + git-vs-local args after KERNEL_NAME is set.
anchor = "set(KERNEL_NAME delta_kernel)\n"
if anchor not in s:
    sys.exit("KERNEL_NAME anchor not found; upstream CMakeLists changed")
block = anchor + '''
# --- thunderduck cross-repo dev loop: optional local delta-kernel-rs override ---
# Pass -DDELTA_KERNEL_LOCAL_DIR=<abs path> to build a local kernel checkout
# instead of cloning the pinned GIT_TAG. A no-op DOWNLOAD_COMMAND (not an empty
# string) is used so unquoted ${...} expansion keeps all args intact.
# See thunderduck docs/context/delta-cross-repo-dev-loop.md.
if(DEFINED DELTA_KERNEL_LOCAL_DIR AND NOT "${DELTA_KERNEL_LOCAL_DIR}" STREQUAL "")
  set(DELTA_KERNEL_SRC_DIR "${DELTA_KERNEL_LOCAL_DIR}")
  set(DELTA_KERNEL_GIT_ARGS DOWNLOAD_COMMAND ${CMAKE_COMMAND} -E true BUILD_ALWAYS TRUE)
  message(STATUS "delta-kernel-rs: using LOCAL checkout ${DELTA_KERNEL_SRC_DIR}")
else()
  set(DELTA_KERNEL_SRC_DIR "${CMAKE_BINARY_DIR}/rust/src/delta_kernel")
  set(DELTA_KERNEL_GIT_ARGS GIT_REPOSITORY "https://github.com/delta-io/delta-kernel-rs" GIT_TAG "v0.21.0")
  message(STATUS "delta-kernel-rs: cloning pinned GIT_TAG v0.21.0")
endif()
# --- end thunderduck override ---
'''
s = s.replace(anchor, block, 1)

# 3. Swap the hardcoded git clone args in ExternalProject_Add for our variables.
old_ep = (
    "  ${KERNEL_NAME}\n"
    '  GIT_REPOSITORY "https://github.com/delta-io/delta-kernel-rs"\n'
    "  GIT_TAG v0.21.0\n"
)
new_ep = (
    "  ${KERNEL_NAME}\n"
    '  SOURCE_DIR "${DELTA_KERNEL_SRC_DIR}"\n'
    "  ${DELTA_KERNEL_GIT_ARGS}\n"
)
if old_ep not in s:
    sys.exit("ExternalProject_Add git args block not found; upstream CMakeLists changed")
s = s.replace(old_ep, new_ep, 1)

open(p, "w").write(s)
print("patched", p)
PY
fi

# ---- 5. userspace gcc toolchain (gcc 11 here can't compile the extension) ----
log "bootstrapping userspace build toolchain"
"$(dirname "${BASH_SOURCE[0]}")/delta-toolchain-setup.sh"

log "done."
log "kernel:  $KERNEL_DIR ($(git -C "$KERNEL_DIR" rev-parse --abbrev-ref HEAD))"
log "delta:   $DELTA_DIR ($(git -C "$DELTA_DIR" rev-parse --abbrev-ref HEAD)); duckdb submodule @ $(git -C "$DELTA_DIR/duckdb" describe --tags 2>/dev/null || echo '?')"
log "next:    scripts/dev/delta-build.sh"
