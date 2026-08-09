#!/usr/bin/env bash
# Record a progress row for the ENTIRE differential suite. Runs
# `run-differential-tests.sh all` (both conformance corpora + the legacy
# feature-family files) once, buckets per-test outcomes, and appends a row to
# tests/integration/differential_progress.md with the commit SHA and delta vs
# the previous measurement.
#
# Usage:
#     tests/scripts/differential-progress.sh
#
# Side effects:
#     - Runs `tests/scripts/run-differential-tests.sh all -v --tb=no` (~10 min).
#     - Appends one row to tests/integration/differential_progress.md
#       (tracked in git).
#     - If DIFFERENTIAL_PROGRESS_LOG is set, copies the raw pytest -v output
#       there (for per-case failure diffs against a baseline).
#
# The goal is for the overall PASSED count to climb monotonically toward the
# suite total. Every red case (TPC included) is a defect to drive to green;
# the hard gate requirement is "no previously-green case regresses."

set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG_FILE="$WORKSPACE_DIR/tests/integration/differential_progress.md"

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

echo "running the full differential suite (run-differential-tests.sh all) ..."
# Tolerate non-zero exit — the suite is expected to be partially red while
# τ grows coverage.
"$SCRIPT_DIR/run-differential-tests.sh" all -v --tb=no > "$TMP" 2>&1 || true

# Optionally preserve the raw per-test output for per-case regression diffs.
if [ -n "${DIFFERENTIAL_PROGRESS_LOG:-}" ]; then
    cp "$TMP" "$DIFFERENTIAL_PROGRESS_LOG"
fi

# Parse pytest -v per-test lines, e.g.
#   differential/test_sql_corpus_differential.py::test_case[agg-001] PASSED [  3%]
# Status is scanned by word (not field position) so node ids containing
# spaces don't break the parse. Buckets: the DataFrame corpus, the SQL
# corpus, and everything else under differential/ ("other").
eval "$(awk '
    /^differential\// {
        status = ""
        for (i = 2; i <= NF; i++)
            if ($i == "PASSED" || $i == "FAILED" || $i == "ERROR" || \
                $i == "SKIPPED" || $i == "XFAIL" || $i == "XPASS") {
                status = $i
                break
            }
        if (status == "") next
        bucket = "other"
        if ($0 ~ /test_dataframe_corpus_differential\.py/) bucket = "df"
        else if ($0 ~ /test_sql_corpus_differential\.py/) bucket = "sql"
        total[bucket]++
        if (status == "PASSED" || status == "XPASS") pass[bucket]++
        else if (status == "SKIPPED" || status == "XFAIL") skip[bucket]++
    }
    END {
        printf "DF_PASS=%d DF_TOTAL=%d ", pass["df"], total["df"]
        printf "SQL_PASS=%d SQL_TOTAL=%d ", pass["sql"], total["sql"]
        printf "OTHER_PASS=%d OTHER_TOTAL=%d ", pass["other"], total["other"]
        printf "SKIPPED=%d\n", skip["df"] + skip["sql"] + skip["other"]
    }' "$TMP")"

PASSED="$((DF_PASS + SQL_PASS + OTHER_PASS))"
TOTAL="$((DF_TOTAL + SQL_TOTAL + OTHER_TOTAL))"
FAILED="$((TOTAL - PASSED - SKIPPED))"

if [ "$TOTAL" -eq 0 ]; then
    echo "ERROR: could not parse any per-test result from runner output." >&2
    cp "$TMP" "$TMP.keep"
    echo "Full output saved to: $TMP.keep (left intact for inspection)" >&2
    exit 1
fi

SHA="$(git -C "$WORKSPACE_DIR" rev-parse --short HEAD)"
TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Initialize the log file with a header on first run.
if [ ! -f "$LOG_FILE" ]; then
    cat > "$LOG_FILE" <<EOF
# Differential suite progress

One row per \`tests/scripts/differential-progress.sh\` invocation. Each row
records per-test outcomes of the FULL differential suite
(\`run-differential-tests.sh all\`) at the given commit, bucketed into the
DataFrame corpus, the SQL corpus, and the remaining legacy feature-family
files ("Other"). Bucket cells are passed/total. The goal is for the overall
Passed count to climb monotonically toward the suite total.

Supersedes the per-corpus ledgers \`v2_progress.md\` and \`v2_sql_progress.md\`
(frozen 2026-07-09).

| Timestamp UTC        | Commit  | DF corpus | SQL corpus |   Other | Passed | Failed | Skipped | Total | Δ passed |
| -------------------- | ------- | --------: | ---------: | ------: | -----: | -----: | ------: | ----: | -------: |
EOF
fi

# Compute delta vs the previous data row (rows whose timestamp starts with a
# four-digit year, so we skip the header rows). Passed is column 7 when
# splitting on '|' (leading pipe yields an empty first field). NOTE: mawk
# (this container's awk) has no {n} interval support — spell the digits out.
LAST_PASSED="$(awk -F '|' '/^\| 2[0-9][0-9][0-9]-/ {gsub(/ /, "", $7); p=$7} END {print p}' "$LOG_FILE")"
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

printf "| %-20s | %-7s | %9s | %10s | %7s | %6d | %6d | %7d | %5d | %8s |\n" \
    "$TS" "$SHA" \
    "$DF_PASS/$DF_TOTAL" "$SQL_PASS/$SQL_TOTAL" "$OTHER_PASS/$OTHER_TOTAL" \
    "$PASSED" "$FAILED" "$SKIPPED" "$TOTAL" "$DELTA_STR" >> "$LOG_FILE"

echo ""
echo "recorded: $PASSED passed / $FAILED failed / $SKIPPED skipped / $TOTAL total  (Δ $DELTA_STR)"
echo "  DataFrame corpus: $DF_PASS/$DF_TOTAL   SQL corpus: $SQL_PASS/$SQL_TOTAL   other: $OTHER_PASS/$OTHER_TOTAL"
echo "log: $LOG_FILE"
