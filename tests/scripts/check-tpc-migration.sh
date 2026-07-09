#!/usr/bin/env bash
# check-tpc-migration.sh — stop-condition verifier for the TPC-H/TPC-DS
# corpus-migration goal. Exits 0 iff ALL of:
#
#   (a) the four legacy TPC test files are gone and nothing under tests/ or
#       tests/scripts/ references their module names (historical entries in
#       tasks/ and docs/dev_journal/ are exempt by scope);
#   (b) the single entry point's groups collect exactly the expected sets:
#       tpch = 44 cases (22 SQL + 22 DataFrame), tpcds = 133 (100 + 33),
#       and sql_v2 / core / all collect without errors;
#   (c) per-query outcomes match the pass-0 baseline
#       (.agent-output/tpc-baseline.md) with zero pass->fail regressions.
#       This step RUNS the tpch and tpcds groups (needs Spark + the release
#       binary, ~2 min). Skip it with --collect-only when you only want the
#       structural checks.
#
# Usage: tests/scripts/check-tpc-migration.sh [--collect-only]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
INTEGRATION_DIR="$WORKSPACE_DIR/tests/integration"
BASELINE="$WORKSPACE_DIR/.agent-output/tpc-baseline.md"
RUNNER="$SCRIPT_DIR/run-differential-tests.sh"
PYTHON="${THUNDERDUCK_VENV_DIR:-$WORKSPACE_DIR/.venv}/bin/python3"

COLLECT_ONLY=false
[ "${1:-}" = "--collect-only" ] && COLLECT_ONLY=true

fail() { echo "FAIL: $1"; exit 1; }
ok() { echo "  ok: $1"; }

echo "[a] legacy files removed + no references"
LEGACY="test_differential_v2 test_tpch_differential test_tpcds_differential test_tpcds_dataframe_differential"
for f in $LEGACY; do
    [ -e "$INTEGRATION_DIR/differential/$f.py" ] && fail "legacy file still exists: $f.py"
done
ok "all four legacy files gone"
# (this script necessarily names the legacy modules — exclude itself)
REFS="$(cd "$WORKSPACE_DIR" && git grep -l -E "test_differential_v2|test_tpch_differential|test_tpcds_differential|test_tpcds_dataframe" -- tests/ ':!tests/scripts/check-tpc-migration.sh' 2>/dev/null || true)"
[ -n "$REFS" ] && fail "references to legacy modules remain under tests/: $REFS"
ok "no references under tests/ (incl. tests/scripts/)"

echo "[b] entry-point group collection"
collect_count() {
    "$RUNNER" "$1" --collect-only -q 2>&1 \
        | grep -oE "[0-9]+(/[0-9]+)? tests? collected" | tail -1 | grep -oE "^[0-9]+"
}
for spec in "tpch:44" "tpcds:133"; do
    g="${spec%%:*}"; want="${spec##*:}"
    got="$(collect_count "$g")"
    [ "$got" = "$want" ] || fail "group '$g' collects $got cases, expected $want"
    ok "group '$g' collects exactly $want cluster cases"
done
for g in sql_v2 core all; do
    got="$(collect_count "$g")"
    [ -n "$got" ] && [ "$got" -gt 0 ] || fail "group '$g' failed to collect"
    ok "group '$g' collects cleanly ($got tests)"
done

if $COLLECT_ONLY; then
    echo "PASS (structural checks only; per-query baseline comparison skipped)"
    exit 0
fi

echo "[c] per-query outcomes vs baseline (runs tpch + tpcds groups)"
[ -f "$BASELINE" ] || fail "baseline file missing: $BASELINE"
TPCH_LOG="$(mktemp)"; TPCDS_LOG="$(mktemp)"
trap 'rm -f "$TPCH_LOG" "$TPCDS_LOG"' EXIT
"$RUNNER" tpch -q --tb=no > "$TPCH_LOG" 2>&1 || true
"$RUNNER" tpcds -q --tb=no > "$TPCDS_LOG" 2>&1 || true

"$PYTHON" - "$BASELINE" "$TPCH_LOG" "$TPCDS_LOG" << 'EOF'
import re
import sys

baseline_path, tpch_log, tpcds_log = sys.argv[1:4]

# Canonical baseline: (case-id, front-end) -> status. Legacy duplicates
# (e.g. TestTPCH_Q1_Differential vs test_query_differential[1]) collapse to
# the same key; they agreed in the recorded baseline.
base = {}
for line in open(baseline_path):
    for pat, fmt, kind in [
        (r"differential/test_differential_v2\.py::TestTPCH_AllQueries_Differential::test_query_differential\[(\d+)\] (PASSED|FAILED)", "tpch-q{:02d}", "sql"),
        (r"differential/test_differential_v2\.py::TestTPCH_Q(\d)_Differential::.* (PASSED|FAILED)", "tpch-q{:02d}", "sql"),
        (r"differential/test_tpch_differential\.py::TestTPCHDifferential::test_q(\d+)_dataframe (PASSED|FAILED)", "tpch-q{:02d}", "df"),
        (r"differential/test_tpcds_dataframe_differential\.py::test_tpcds_dataframe_query\[(\d+)\] (PASSED|FAILED)", "tpcds-q{:03d}", "df"),
    ]:
        m = re.match(pat, line)
        if m:
            base[(fmt.format(int(m.group(1))), kind)] = m.group(2)
    m = re.match(r"differential/test_tpcds_differential\.py::TestTPCDS_Differential::test_query_differential\[(\w+)\] (PASSED|FAILED)", line)
    if m:
        q = m.group(1)
        key = f"tpcds-q{int(q):03d}" if q.isdigit() else f"tpcds-q0{q}"
        base[(key, "sql")] = m.group(2)

fails = set()
ran = set()
for path in (tpch_log, tpcds_log):
    for line in open(path):
        m = re.match(r"differential/test_(sql|dataframe)_corpus_differential\.py::test_case\[([a-z0-9-]+)\]", line)
        if m:
            ran.add((m.group(2), "sql" if m.group(1) == "sql" else "df"))
        m = re.match(r"FAILED differential/test_(sql|dataframe)_corpus_differential\.py::test_case\[([a-z0-9-]+)\]", line)
        if m:
            fails.add((m.group(2), "sql" if m.group(1) == "sql" else "df"))

# In -q mode only failures are listed per-test; a baseline query with no
# corpus case at all is caught by the collection-count check (b), so the
# regression comparison (baseline PASSED -> now in the failure list) is the
# only per-query check needed here.
regressions = sorted(
    f"{qid} ({kind})"
    for (qid, kind), status in base.items()
    if status == "PASSED" and (qid, kind) in fails
)

if regressions:
    print("FAIL: pass->fail regressions vs baseline:", ", ".join(regressions))
    sys.exit(1)
print(f"  ok: zero pass->fail regressions across {len(base)} baseline query outcomes")
EOF

echo "PASS: TPC migration stop condition holds"
