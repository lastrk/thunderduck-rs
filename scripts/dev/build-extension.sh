#!/usr/bin/env bash
#
# build-extension.sh — local dev build of the thdck_spark_funcs extension.
#
# Builds extension/ (the in-tree DuckDB extension source, see
# extension/README.md's Provenance section) for the HOST platform only,
# against the single DuckDB version pinned in extension/BUILD_PINS.toml.
# There is no multiversion build here — local dev and CI both build exactly
# ONE DuckDB version at a time (see extension/BUILD_PINS.toml's header
# comment and .github/workflows/extension-release.yml).
#
# Usage:
#   scripts/dev/build-extension.sh [--init] [--smoke]
#
#   --init   initialize the extension/duckdb + extension/extension-ci-tools
#            submodules (scoped to those two paths only) if not already
#            checked out.
#   --smoke  after a successful build, also run:
#              1. `make -C extension test` — the SQLLogicTest suite, and
#              2. the swap-in proof: THUNDERDUCK_EXT_PATH=<built binary>
#                 cargo test -p thunderduck-core --lib extension_loader
#                 -- --nocapture — proves thunderduck-rs's extension_loader
#                 (in particular the spark_avg DECIMAL(13,6) probe) passes
#                 against the LOCALLY BUILT binary, not just the vendored one.
#
# Parallelism: this devcontainer is capped at 8 GiB. Override the build
# parallelism with NPROC=<n> (default: 4) if you hit an OOM kill; the
# underlying cmake --build step honors CMAKE_BUILD_PARALLEL_LEVEL, which this
# script sets from NPROC.
set -euo pipefail

ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
EXT_DIR="$ROOT/extension"
BUILD_PINS="$EXT_DIR/BUILD_PINS.toml"
CARGO_TOML="$ROOT/Cargo.toml"

log() { printf '[build-extension] %s\n' "$*" >&2; }
err() { printf '[build-extension] ERROR: %s\n' "$*" >&2; }

DO_INIT=0
DO_SMOKE=0
for arg in "$@"; do
  case "$arg" in
    --init) DO_INIT=1 ;;
    --smoke) DO_SMOKE=1 ;;
    -h | --help)
      sed -n '2,25p' "${BASH_SOURCE[0]}"
      exit 0
      ;;
    *)
      err "unknown argument: $arg (supported: --init, --smoke)"
      exit 1
      ;;
  esac
done

# --- 1. submodules present? --------------------------------------------------
need_init=0
for path in extension/duckdb extension/extension-ci-tools; do
  if [[ -z "$(ls -A "$ROOT/$path" 2>/dev/null)" ]]; then
    need_init=1
  fi
done
if [[ "$need_init" -eq 1 ]]; then
  if [[ "$DO_INIT" -eq 1 ]]; then
    log "initializing extension/duckdb + extension/extension-ci-tools submodules"
    git -C "$ROOT" submodule update --init extension/duckdb extension/extension-ci-tools
  else
    err "extension/duckdb and/or extension/extension-ci-tools submodules are not initialized."
    err "Re-run with --init, or manually:"
    err "  git submodule update --init extension/duckdb extension/extension-ci-tools"
    exit 1
  fi
fi

# --- 2. parse the pin -------------------------------------------------------
[[ -f "$BUILD_PINS" ]] || {
  err "missing $BUILD_PINS"
  exit 1
}
pinned_version="$(awk -F'"' '/^\[duckdb\]/{f=1; next} /^\[/{f=0} f && /^version = /{print $2; exit}' "$BUILD_PINS")"
[[ -n "$pinned_version" ]] || {
  err "could not parse [duckdb].version from $BUILD_PINS"
  exit 1
}

# --- 3. three-way lock -------------------------------------------------------
# (a) extension/duckdb submodule checked out at the pinned tag
submodule_tag="$(git -C "$EXT_DIR/duckdb" describe --tags --exact-match 2>/dev/null || true)"
if [[ "$submodule_tag" != "$pinned_version" ]]; then
  err "extension/duckdb is checked out at '${submodule_tag:-<detached, no exact tag>}', expected '$pinned_version' (extension/BUILD_PINS.toml)."
  err "Fix: cd extension/duckdb && git checkout $pinned_version && cd $ROOT && git add extension/duckdb"
  exit 1
fi

# (b) the `duckdb` crate version in the workspace Cargo.toml decodes to the
# same DuckDB version. duckdb-rs encodes it in the crate's minor field:
# 1.10504.0 -> minor "10504" -> split into major (leading digits) + 2-digit
# DuckDB minor + 2-digit DuckDB patch -> v1.5.4. See docs/context/gotchas.md #6.
crate_version="$(awk -F'"' '/^duckdb[[:space:]]*=/{print $2; exit}' "$CARGO_TOML")"
[[ -n "$crate_version" ]] || {
  err "could not parse the duckdb crate version from $CARGO_TOML"
  exit 1
}
minor_field="$(printf '%s' "$crate_version" | awk -F. '{print $2}')"
if [[ ! "$minor_field" =~ ^[0-9]{5,}$ ]]; then
  err "duckdb crate version '$crate_version' has an unexpected minor field '$minor_field' (expected >= 5 digits, e.g. 10504)"
  exit 1
fi
crate_patch="${minor_field: -2}"
crate_minor="${minor_field: -4:2}"
crate_major="${minor_field:0:$((${#minor_field} - 4))}"
decoded_version="v${crate_major}.${crate_minor#0}.${crate_patch#0}"
if [[ "$decoded_version" != "$pinned_version" ]]; then
  err "duckdb crate version '$crate_version' decodes to '$decoded_version', but extension/BUILD_PINS.toml pins '$pinned_version'."
  err "These must match exactly (docs/context/gotchas.md #6, docs/context/dependencies.md). Fix whichever is stale."
  exit 1
fi

log "three-way lock OK: submodule=$submodule_tag  BUILD_PINS=$pinned_version  duckdb crate=$crate_version ($decoded_version)"

# --- 4. host platform ---------------------------------------------------------
uname_s="$(uname -s)"
uname_m="$(uname -m)"
case "$uname_s-$uname_m" in
  Linux-x86_64) platform=linux_amd64 ;;
  Linux-aarch64) platform=linux_arm64 ;;
  Darwin-x86_64) platform=osx_amd64 ;;
  Darwin-arm64 | Darwin-aarch64) platform=osx_arm64 ;;
  *)
    err "unsupported host platform: $uname_s-$uname_m (supported: linux_amd64, linux_arm64, osx_amd64, osx_arm64)"
    exit 1
    ;;
esac
log "host platform: $platform"

# --- 5. build ------------------------------------------------------------------
if command -v ninja >/dev/null 2>&1; then
  export GEN=ninja
  log "using ninja generator"
fi
export CMAKE_BUILD_PARALLEL_LEVEL="${NPROC:-4}"
log "building release (parallel=$CMAKE_BUILD_PARALLEL_LEVEL; override with NPROC=<n>)"
make -C "$EXT_DIR" release

BIN="$EXT_DIR/build/release/extension/thdck_spark_funcs/thdck_spark_funcs.duckdb_extension"
[[ -f "$BIN" ]] || {
  err "expected built binary not found at $BIN"
  exit 1
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}
stat_size() { stat -c%s "$1" 2>/dev/null || stat -f%z "$1"; }

sha256="$(sha256_of "$BIN")"
size="$(stat_size "$BIN")"
log "built:   $BIN"
log "sha256:  $sha256"
log "size:    $size bytes"

# --- 6. footer check: expect the pinned DuckDB version + this platform ------
footer="$(tail -c 1024 "$BIN" | strings)"
footer_ok=1
if ! grep -qF -- "$pinned_version" <<<"$footer" && ! grep -qF -- "${pinned_version#v}" <<<"$footer"; then
  err "footer check: DuckDB version '$pinned_version' not found in the binary's footer"
  footer_ok=0
fi
if ! grep -qF -- "$platform" <<<"$footer"; then
  err "footer check: platform '$platform' not found in the binary's footer"
  footer_ok=0
fi
[[ "$footer_ok" -eq 1 ]] || exit 1
log "footer check OK ($pinned_version + $platform present in the binary footer)"

# --- 7. --smoke ----------------------------------------------------------------
if [[ "$DO_SMOKE" -eq 1 ]]; then
  log "running make test (SQLLogicTest suite)"
  make -C "$EXT_DIR" test

  log "swap-in proof: THUNDERDUCK_EXT_PATH=$BIN cargo test -p thunderduck-core --lib extension_loader -- --nocapture"
  THUNDERDUCK_EXT_PATH="$BIN" cargo test -p thunderduck-core --lib extension_loader -- --nocapture
fi

log "done."
