#!/usr/bin/env bash
#
# delta-toolchain-setup.sh — bootstrap a userspace gcc for the Delta dev loop.
#
# This devcontainer ships gcc 11, whose libstdc++ rejects a self-referential
# `unordered_map` used in duckdb-delta's source (newer libstdc++ accepts it).
# `apt` is unavailable (no root), so we install a relocatable conda-forge gcc
# into a gitignored dir via micromamba — no root, self-contained.
#
# delta-build.sh uses this toolchain (with -static-libstdc++/-static-libgcc) so
# the resulting extension still loads into thunderduck's gcc-11 process.
#
# Idempotent. See docs/context/delta-cross-repo-dev-loop.md.
set -euo pipefail

GCC_VERSION="${DELTA_GCC_VERSION:-13}"
MAMBA_ARCH="linux-aarch64"   # this devcontainer is aarch64

ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
TC="$ROOT/.delta-toolchain"
MM="$TC/bin/micromamba"
ENV="$TC/env"
export MAMBA_ROOT_PREFIX="$TC/root"

log() { printf '\033[1;34m[delta-toolchain]\033[0m %s\n' "$*"; }

mkdir -p "$TC/bin"

if [[ ! -x "$MM" ]]; then
  log "downloading micromamba ($MAMBA_ARCH)"
  curl -Ls "https://micro.mamba.pm/api/micromamba/${MAMBA_ARCH}/latest" | tar -xj -C "$TC" bin/micromamba
fi

# Conda names compiler binaries with a host triplet.
existing_cxx() { ls "$ENV"/bin/*-linux-gnu-g++ 2>/dev/null | head -1; }

if [[ -z "$(existing_cxx)" ]]; then
  log "creating conda-forge gcc/gxx=$GCC_VERSION env (this downloads ~a few hundred MB)"
  "$MM" create -y -p "$ENV" -c conda-forge \
    "gxx_linux-aarch64=${GCC_VERSION}" \
    "gcc_linux-aarch64=${GCC_VERSION}"
else
  log "toolchain env already present"
fi

CXX_BIN="$(existing_cxx)"
CC_BIN="$(ls "$ENV"/bin/*-linux-gnu-gcc 2>/dev/null | head -1)"
if [[ -z "$CXX_BIN" || -z "$CC_BIN" ]]; then
  echo "toolchain setup failed: compiler binaries not found under $ENV/bin" >&2
  exit 1
fi

log "CC : $CC_BIN"
log "CXX: $CXX_BIN"
log "version: $("$CXX_BIN" -dumpversion 2>/dev/null)"
