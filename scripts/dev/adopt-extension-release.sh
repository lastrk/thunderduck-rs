#!/usr/bin/env bash
#
# adopt-extension-release.sh — vendor a new thdck_spark_funcs release.
#
# thunderduck checks in ALL 4 platform binaries of the `thdck_spark_funcs`
# DuckDB extension, PLAIN (uncompressed), exactly ONE version at a time,
# under extensions/vendored/. This script is the ONLY supported way to move
# to a new version. It supports two adoption modes:
#
#   1. --from-local <dir>  (current, in-tree source — extension/, see
#      extension/README.md's Provenance section): consumes 4 binaries named
#      thdck_spark_funcs-<duckdb-version>-<platform>.duckdb_extension already
#      built (e.g. by scripts/dev/build-extension.sh or
#      .github/workflows/extension-release.yml) from <dir>. The DuckDB
#      version is read from extension/BUILD_PINS.toml (the in-tree pin), not
#      passed on the command line. The regenerated MANIFEST.toml's [source]
#      block records in-tree provenance (origin/build_sha/built_by) instead
#      of a release tag.
#
#   2. <release-tag> <duckdb-version>  (legacy, release-download mode):
#      downloads the 4 binaries from the (now archived, per
#      docs/context/extension-archival-checklist.md) `gh release` on
#      nubank/thunderduck-duckdb-extension. Retained only until every
#      vendored version has moved to an in-tree build; the [source] block
#      it writes keeps the repo/release_tag form.
#
# Usage:
#   scripts/dev/adopt-extension-release.sh --from-local <dir>
#   scripts/dev/adopt-extension-release.sh <release-tag> <duckdb-version>
#     e.g. scripts/dev/adopt-extension-release.sh ext6 v1.5.4
#
#   scripts/dev/adopt-extension-release.sh --verify
#     CI-style check: re-hash the files already vendored under
#     extensions/vendored/ against MANIFEST.toml (sha256 + size). Does not
#     download anything.
#
# Adoption cadence: only when the `duckdb` crate version in Cargo.toml bumps
# (the extension's embedded DuckDB version must exactly match — see
# docs/context/gotchas.md #6).
#
# History-size escape valve: if the per-adoption tree cost (~31 MB/version,
# since old plain binaries stay in git history) ever becomes a problem,
# migrate extensions/vendored/ to Git LFS
# (`git lfs track 'extensions/vendored/*.duckdb_extension'`) rather than
# reverting to build-time download.
set -euo pipefail

REPO_SLUG="nubank/thunderduck-duckdb-extension"
PLATFORMS=(linux_amd64 linux_arm64 osx_amd64 osx_arm64)

log() { printf '[adopt-extension] %s\n' "$*" >&2; }
err() { printf '[adopt-extension] ERROR: %s\n' "$*" >&2; }

usage() {
  cat >&2 <<'EOF'
Usage:
  adopt-extension-release.sh --from-local <dir>
  adopt-extension-release.sh <release-tag> <duckdb-version>
  adopt-extension-release.sh --verify
EOF
}

# --- portable hashing / stat (macOS coreutils differ from GNU) --------------
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    err "neither sha256sum nor shasum found"
    exit 1
  fi
}

stat_size() {
  if stat -c%s "$1" >/dev/null 2>&1; then
    stat -c%s "$1"
  else
    stat -f%z "$1"
  fi
}

ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
VENDOR_DIR="$ROOT/extensions/vendored"
MANIFEST="$VENDOR_DIR/MANIFEST.toml"

# --- verify mode -------------------------------------------------------------
verify() {
  [[ -f "$MANIFEST" ]] || { err "MANIFEST.toml not found at $MANIFEST"; exit 1; }

  local file="" sha="" fail=0 checked=0
  while IFS= read -r line; do
    case "$line" in
      'file = '*)
        file="${line#file = }"
        file="${file//\"/}"
        ;;
      'sha256 = '*)
        sha="${line#sha256 = }"
        sha="${sha//\"/}"
        ;;
      'size = '*)
        local expected_size actual_size actual_sha path
        expected_size="${line#size = }"
        path="$VENDOR_DIR/$file"
        checked=$((checked + 1))
        if [[ ! -f "$path" ]]; then
          err "MISSING: $file"
          fail=1
          continue
        fi
        actual_sha="$(sha256_of "$path")"
        actual_size="$(stat_size "$path")"
        if [[ "$actual_sha" != "$sha" ]]; then
          err "SHA256 MISMATCH: $file (manifest $sha, actual $actual_sha)"
          fail=1
        fi
        if [[ "$actual_size" != "$expected_size" ]]; then
          err "SIZE MISMATCH: $file (manifest $expected_size, actual $actual_size)"
          fail=1
        fi
        ;;
    esac
  done <"$MANIFEST"

  if [[ "$checked" -eq 0 ]]; then
    err "no [[artifact]] entries parsed out of $MANIFEST"
    exit 1
  fi
  if [[ "$fail" -ne 0 ]]; then
    err "verify FAILED"
    exit 1
  fi
  log "verify OK ($checked artifacts, sha256 + size match MANIFEST.toml)"
}

# --- shared helpers ----------------------------------------------------------

# Remove whatever vendored set is currently checked in (tracked or not —
# first adoption has none).
remove_prior_vendored_set() {
  mkdir -p "$VENDOR_DIR"
  shopt -s nullglob
  local prior=("$VENDOR_DIR"/thdck_spark_funcs-*.duckdb_extension)
  shopt -u nullglob
  if [[ ${#prior[@]} -gt 0 ]]; then
    log "removing prior vendored set (${#prior[@]} files)"
    for f in "${prior[@]}"; do
      if git -C "$ROOT" ls-files --error-unmatch "$f" >/dev/null 2>&1; then
        git -C "$ROOT" rm -f -q -- "$f"
      else
        rm -f "$f"
      fi
    done
  fi
}

# Append one `[[artifact]]` table per platform/filename pair to MANIFEST,
# computing sha256+size from the now-vendored file.
append_artifact_tables() {
  local i platform fname path sha size
  for i in "${!PLATFORMS[@]}"; do
    platform="${PLATFORMS[$i]}"
    fname="${FILENAMES[$i]}"
    path="$VENDOR_DIR/$fname"
    sha="$(sha256_of "$path")"
    size="$(stat_size "$path")"
    cat >>"$MANIFEST" <<EOF

[[artifact]]
platform = "$platform"
file = "$fname"
sha256 = "$sha"
size = $size
EOF
  done
}

manifest_header() {
  cat <<'EOF'
# extensions/vendored/MANIFEST.toml
#
# Vendored thdck_spark_funcs DuckDB extension binaries — checked into git
# PLAIN (uncompressed), all 4 platforms, exactly ONE version at a time.
#
# Regenerate ONLY via:
#   scripts/dev/adopt-extension-release.sh --from-local <dir>
#   scripts/dev/adopt-extension-release.sh <release-tag> <duckdb-version>
# Changes to this file happen only on duckdb crate bumps — the extension's
# embedded DuckDB version must exactly match the `duckdb` crate version in
# Cargo.toml (see docs/context/gotchas.md #6).
#
# Escape valve: if per-adoption tree cost (~31 MB/version, since old plain
# binaries stay in git history) becomes a problem, migrate this directory to
# Git LFS (`git lfs track 'extensions/vendored/*.duckdb_extension'`) rather
# than reverting to build-time download.
EOF
}

resolve_adopted_by() {
  local adopted_by
  adopted_by="$(git -C "$ROOT" config user.name 2>/dev/null || true)"
  if [[ -n "$adopted_by" ]]; then
    local email
    email="$(git -C "$ROOT" config user.email 2>/dev/null || true)"
    [[ -n "$email" ]] && adopted_by="$adopted_by <$email>"
  else
    adopted_by="$(whoami)@$(hostname 2>/dev/null || echo unknown)"
  fi
  printf '%s' "$adopted_by"
}

# Preflight (a bootstrap tree whose .gitignore still blanket-ignores
# extensions/ would make `git add` fail with a raw git error — surface an
# actionable message instead), stage, then re-verify what was just written.
finalize_and_verify() {
  if git -C "$ROOT" check-ignore -q "$VENDOR_DIR/MANIFEST.toml"; then
    err "extensions/vendored is gitignored — fix .gitignore first \
(needs the '/extensions/*' + '!/extensions/vendored/' pattern pair)"
    exit 1
  fi
  git -C "$ROOT" add "$VENDOR_DIR"
  log "staged extensions/vendored (MANIFEST.toml + 4 binaries)"
  verify
}

# --- adopt mode (release-download) -------------------------------------------
adopt() {
  local release_tag="$1" duckdb_version="$2"

  command -v gh >/dev/null 2>&1 || { err "gh CLI not found"; exit 1; }

  remove_prior_vendored_set

  log "downloading $release_tag ($duckdb_version) for ${#PLATFORMS[@]} platforms from $REPO_SLUG"
  FILENAMES=()
  for platform in "${PLATFORMS[@]}"; do
    local fname="thdck_spark_funcs-${duckdb_version}-${platform}.duckdb_extension"
    FILENAMES+=("$fname")
    log "  fetching $fname"
    gh release download "$release_tag" \
      --repo "$REPO_SLUG" \
      --pattern "$fname" \
      --dir "$VENDOR_DIR" \
      --clobber
  done

  local adopted_at adopted_by
  adopted_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  adopted_by="$(resolve_adopted_by)"

  {
    manifest_header
    cat <<EOF

[source]
repo = "$REPO_SLUG"
release_tag = "$release_tag"
duckdb_version = "$duckdb_version"
adopted_at = "$adopted_at"
adopted_by = "$adopted_by"
EOF
    append_artifact_tables
  } >"$MANIFEST"

  log "wrote $MANIFEST"
  finalize_and_verify

  cat >&2 <<EOF

[adopt-extension] Adoption of $release_tag / $duckdb_version complete. Maintainer checklist:

  1. Confirm the \`duckdb\` crate version in Cargo.toml matches $duckdb_version
     (bump it first if this adoption follows a duckdb crate bump).
  2. Build: \`cargo build --release\` — build.rs picks up the new vendored
     binaries automatically, no code changes needed.
  3. Run the extension_loader tests, in particular the \`spark_avg_decimal_probe\`
     DECIMAL(13,6) probe (\`cargo test -p thunderduck-core --lib extension_loader\`)
     — it proves the newly-embedded bytes are the same shape the emission side
     assumes.
  4. Run the DataFrame + SQL differential corpora and confirm no previously-green
     case regresses (differential-progress is the fitness gate — see
     docs/context/testing.md).
  5. Update the pin-matrix docs (docs/context/dependencies.md,
     docs/context/gotchas.md #6, docs/context/delta-cross-repo-dev-loop.md) if
     duckdb_version changed, then commit the staged extensions/vendored/ change
     together with the doc updates.
EOF
}

# --- adopt mode (--from-local) ------------------------------------------------
adopt_from_local() {
  local src_dir="$1"
  [[ -d "$src_dir" ]] || { err "not a directory: $src_dir"; exit 1; }

  local build_pins="$ROOT/extension/BUILD_PINS.toml"
  [[ -f "$build_pins" ]] || { err "missing $build_pins — is the in-tree extension present?"; exit 1; }
  local duckdb_version
  duckdb_version="$(awk -F'"' '/^\[duckdb\]/{f=1; next} /^\[/{f=0} f && /^version = /{print $2; exit}' "$build_pins")"
  [[ -n "$duckdb_version" ]] || { err "could not parse [duckdb].version from $build_pins"; exit 1; }

  remove_prior_vendored_set

  log "adopting $duckdb_version binaries for ${#PLATFORMS[@]} platforms from $src_dir"
  FILENAMES=()
  for platform in "${PLATFORMS[@]}"; do
    local fname="thdck_spark_funcs-${duckdb_version}-${platform}.duckdb_extension"
    local srcfile="$src_dir/$fname"
    [[ -f "$srcfile" ]] || { err "missing $fname under $src_dir"; exit 1; }
    FILENAMES+=("$fname")
    log "  adopting $fname"
    cp "$srcfile" "$VENDOR_DIR/$fname"
  done

  local build_sha built_by adopted_at
  build_sha="$(git -C "$ROOT" rev-parse HEAD)"
  built_by="${BUILT_BY:-$(resolve_adopted_by)}"
  adopted_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  {
    manifest_header
    cat <<EOF

[source]
origin = "in-tree: extension/ (imported from nubank/thunderduck-duckdb-extension, archived)"
build_sha = "$build_sha"
duckdb_version = "$duckdb_version"
built_by = "$built_by"
adopted_at = "$adopted_at"
EOF
    append_artifact_tables
  } >"$MANIFEST"

  log "wrote $MANIFEST"
  finalize_and_verify

  cat >&2 <<EOF

[adopt-extension] In-tree adoption of $duckdb_version (built at $build_sha) complete. Maintainer checklist:

  1. Confirm the \`duckdb\` crate version in Cargo.toml matches $duckdb_version.
  2. Build: \`cargo build --release\` — build.rs picks up the new vendored
     binaries automatically, no code changes needed.
  3. Run the extension_loader tests, in particular the \`spark_avg_decimal_probe\`
     DECIMAL(13,6) probe (\`cargo test -p thunderduck-core --lib extension_loader\`).
  4. Run the DataFrame + SQL differential corpora and confirm no previously-green
     case regresses (docs/context/testing.md).
EOF
}

# --- entry point --------------------------------------------------------------
if [[ "${1:-}" == "--verify" ]]; then
  verify
elif [[ "${1:-}" == "--from-local" ]]; then
  [[ $# -eq 2 ]] || { usage; exit 1; }
  adopt_from_local "$2"
elif [[ $# -eq 2 ]]; then
  adopt "$1" "$2"
else
  usage
  exit 1
fi
