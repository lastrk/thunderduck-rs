#!/usr/bin/env bash
# Single-threaded build profiler.
#
# Usage:
#   run-profile.sh <label> [--clean] [-- <extra cargo args>]
#
# Forces -j1 so every rustc / cc invocation runs alone -> the per-process peak
# RSS we record IS the per-thread RAM footprint. Captures three artifacts under
# tasks/build-profiling/results/<label>/:
#   rustc.csv      per-crate wall time + peak RSS (from RUSTC_WRAPPER)
#   sampler.csv    whole-build RSS + concurrency timeline (from /proc sampler)
#   cargo.log      cargo stderr/stdout, incl. total time
#   summary.txt    headline numbers
# plus cargo's own cargo-timing.html (the unit DAG).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
LABEL="${1:?usage: run-profile.sh <label> [--clean] [-- <cargo args>]}"
shift

CLEAN=0
EXTRA=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --clean) CLEAN=1; shift ;;
    --) shift; EXTRA=("$@"); break ;;
    *) EXTRA+=("$1"); shift ;;
  esac
done

OUT="$HERE/results/$LABEL"
mkdir -p "$OUT"
: > "$OUT/rustc.csv"
echo "kind,crate,crate_type,wall_s,maxrss_kib,emit" > "$OUT/rustc.csv"

cd "$ROOT"
if [[ $CLEAN -eq 1 ]]; then
  echo "[profile] cargo clean" >&2
  cargo clean
fi

export PROFILE_LOG="$OUT/rustc.csv"
export RUSTC_WRAPPER="$HERE/rustc_wrap.py"
# Keep codegen attributable to crates, not spread across background threads.
export CARGO_BUILD_JOBS=1

echo "[profile] starting sampler" >&2
python3 "$HERE/sampler.py" "$OUT/sampler.csv" 0.2 &
SAMPLER=$!
trap 'kill "$SAMPLER" 2>/dev/null || true' EXIT

START=$(date +%s.%N)
set +e
cargo build -j1 --timings "${EXTRA[@]}" >"$OUT/cargo.log" 2>&1
RC=$?
set -e
END=$(date +%s.%N)

kill "$SAMPLER" 2>/dev/null || true
wait "$SAMPLER" 2>/dev/null || true
trap - EXIT

# cargo writes target/cargo-timings/cargo-timing.html; copy the latest in.
LATEST_TIMING=$(ls -t target/cargo-timings/cargo-timing-*.html 2>/dev/null | head -1 || true)
[[ -n "$LATEST_TIMING" ]] && cp "$LATEST_TIMING" "$OUT/cargo-timing.html"

WALL=$(awk "BEGIN{printf \"%.2f\", $END - $START}")
{
  echo "label:        $LABEL"
  echo "exit code:    $RC"
  echo "wall seconds: $WALL"
  echo "jobs:         1 (forced)"
  echo "extra args:   ${EXTRA[*]:-<none>}"
} > "$OUT/summary.txt"

echo "[profile] done rc=$RC wall=${WALL}s -> $OUT" >&2
exit "$RC"
