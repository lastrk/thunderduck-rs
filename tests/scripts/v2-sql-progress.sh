#!/usr/bin/env bash
# Record the τ SQL front-end's `sql_v2` PASSED count, append a row to
# tests/integration/v2_sql_progress.md with the commit SHA and delta vs the
# previous measurement.
#
# Usage:
#     tests/scripts/v2-sql-progress.sh
#
# Side effects:
#     - Runs `tests/scripts/run-differential-tests.sh sql_v2` (~1 min).
#     - Appends one row to tests/integration/v2_sql_progress.md (tracked in git).
#
# The goal is for the PASSED count to climb monotonically toward the SQL corpus
# total (`differential/sql_corpus.py` CASES) as τ grows temp-view registration,
# the catalog bridge, and SQL grammar coverage. Sibling of `v2-progress.sh`
# (which tracks the DataFrame-API `core_v2` corpus); the two milestones are
# tracked in separate logs.

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG_FILE="$WORKSPACE_DIR/tests/integration/v2_sql_progress.md"

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

echo "running sql_v2 suite ..."
# Tolerate non-zero exit — the suite is expected to fail until τ lands the SQL
# front-end (temp views, catalog bridge, grammar).
"$SCRIPT_DIR/run-differential-tests.sh" sql_v2 > "$TMP" 2>&1 || true

# Parse the pytest tail line, e.g.
#   "================ 250 failed, 12 passed, 2 warnings in 60.24s ================"
PASSED="$(grep -oE '[0-9]+ passed' "$TMP" | tail -1 | grep -oE '[0-9]+' || echo 0)"
FAILED="$(grep -oE '[0-9]+ failed' "$TMP" | tail -1 | grep -oE '[0-9]+' || echo 0)"
TOTAL="$((PASSED + FAILED))"

if [ "$TOTAL" -eq 0 ]; then
    echo "ERROR: could not parse pytest summary from runner output." >&2
    echo "Full output saved to: $TMP (left intact for inspection)" >&2
    trap - EXIT
    exit 1
fi

SHA="$(git -C "$WORKSPACE_DIR" rev-parse --short HEAD)"
TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Initialize the log file with a header on first run.
if [ ! -f "$LOG_FILE" ]; then
    cat > "$LOG_FILE" <<EOF
# τ SQL front-end progress

One row per \`tests/scripts/v2-sql-progress.sh\` invocation. Each row records the
\`sql_v2\` suite (Spark SQL conformance corpus \`differential/sql_corpus.py\`,
run through τ) PASSED count at the given commit. The goal is for PASSED to climb
monotonically toward $TOTAL (the corpus total) as τ grows temp-view
registration, the catalog bridge, and SQL grammar coverage.

| Timestamp UTC        | Commit  | Passed | Failed | Total | Δ vs prev |
| -------------------- | ------- | -----: | -----: | ----: | --------: |
EOF
fi

# Compute delta vs the previous data row (rows whose timestamp starts with a
# four-digit year, so we skip the header rows).
LAST_PASSED="$(awk -F '|' '/^\| 2[0-9]{3}-/ {gsub(/ /, "", $4); p=$4} END {print p}' "$LOG_FILE")"
if [ -z "$LAST_PASSED" ]; then
    DELTA_STR="n/a"
else
    DELTA="$((PASSED - LAST_PASSED))"
    if [ "$DELTA" -ge 0 ]; then
        DELTA_STR="+$DELTA"
    else
        DELTA_STR="$DELTA"
    fi
fi

printf "| %-20s | %-7s | %6d | %6d | %5d | %9s |\n" \
    "$TS" "$SHA" "$PASSED" "$FAILED" "$TOTAL" "$DELTA_STR" >> "$LOG_FILE"

echo ""
echo "recorded: $PASSED passed / $FAILED failed / $TOTAL total  (Δ $DELTA_STR)"
echo "log: $LOG_FILE"
