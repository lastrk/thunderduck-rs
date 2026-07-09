#!/usr/bin/env bash
# Witness-driven progress gate for tasks/select-block-review-findings.md
# (see tasks/goal-implement-review-findings.md for the protocol).
#
# Runs BOTH corpora (DataFrame `core` + SQL `sql_v2`) through the standard
# runner, then compares per-case outcomes against:
#   - tests/integration/select_block_corpus_baseline.txt  (hard gate:
#     every baseline-PASS case must still PASS; any regression => exit 1)
#   - tests/integration/select_block_witness_manifest.json (progress:
#     witness cases that flipped red->green, reported per finding)
#
# Usage:
#   ./tests/scripts/witness-progress.sh                   # report + gate
#   ./tests/scripts/witness-progress.sh --capture-baseline
#       (re)generate the baseline from a fresh run. Only for cycle 0 or an
#       intentional, explained re-baseline commit.
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
INTEG_DIR="$WORKSPACE_DIR/tests/integration"
BASELINE="$INTEG_DIR/select_block_corpus_baseline.txt"
MANIFEST="$INTEG_DIR/select_block_witness_manifest.json"
HELPER="$INTEG_DIR/utils/witness_progress.py"
RUNNER="$SCRIPT_DIR/run-differential-tests.sh"

# Spark lives in the MAIN checkout; worktrees have no in-tree .spark/.
export SPARK_HOME="${SPARK_HOME:-/workspace/.spark/spark-4.1.1}"
export THUNDERDUCK_VENV_DIR="${THUNDERDUCK_VENV_DIR:-/workspace/.venv}"

LOG_DIR="$(mktemp -d /tmp/witness-progress-XXXXXX)"
CORE_LOG="$LOG_DIR/core.log"
SQL_LOG="$LOG_DIR/sql_v2.log"

echo "witness-progress: running DataFrame corpus (core) ..."
"$RUNNER" core >"$CORE_LOG" 2>&1 || true
echo "witness-progress: running SQL corpus (sql_v2) ..."
"$RUNNER" sql_v2 >"$SQL_LOG" 2>&1 || true

# The runner tolerates red cases (they are corpus fitness signal), but a run
# that produced NO per-case lines is an infrastructure failure, not a result.
for log in "$CORE_LOG" "$SQL_LOG"; do
    if ! grep -q "::test_case\[" "$log"; then
        echo "witness-progress: ERROR — no per-case results in $log (infra failure?). Last lines:"
        tail -15 "$log"
        exit 2
    fi
done

echo "logs: $CORE_LOG $SQL_LOG"
if [ "${1:-}" = "--capture-baseline" ]; then
    exec python3 "$HELPER" capture "$BASELINE" "$CORE_LOG" "$SQL_LOG"
fi
exec python3 "$HELPER" report "$BASELINE" "$MANIFEST" "$CORE_LOG" "$SQL_LOG"
