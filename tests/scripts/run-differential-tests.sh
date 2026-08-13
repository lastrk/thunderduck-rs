#!/usr/bin/env bash
# Run differential tests: Apache Spark 4.1.1 (reference) vs Thunderduck Rust (under test)
#
# Compatible with bash 4+ and zsh.
#
# Usage:
#   ./run-differential-tests.sh [--ci] [test-group] [pytest-args...]
#
# Flags:
#   --ci        - CI mode: sets CONTINUE_ON_ERROR=true and COLLECT_TIMEOUT=30
#
# Test groups:
#   all         - Run all differential tests (default)
#   tpch        - TPC-H SQL and DataFrame tests
#   tpcds       - TPC-DS SQL and DataFrame tests
#   functions   - DataFrame function parity tests
#   aggregations - Multi-dimensional aggregation tests
#   window      - Window function tests
#   datetime    - Date/time function tests
#   conditional - Conditional expressions (when/otherwise)
#   operations  - DataFrame operations tests
#   lambda      - Lambda/HOF function tests
#   joins       - Join tests
#   statistics  - Statistics operations
#   types       - Complex types and type literals
#   schema      - ToSchema tests
#   dataframe   - TPC-DS DataFrame API tests
#
# Environment variables (all optional):
#   SPARK_PORT=15003                    - Spark reference server port
#   THUNDERDUCK_PORT=15002              - Thunderduck server port
#   THUNDERDUCK_BINARY=path/to/binary   - Override Thunderduck binary path
#   CONNECT_TIMEOUT=10                  - Session creation timeout (seconds)
#   COLLECT_TIMEOUT=10                  - Result collection timeout
#   SERVER_STARTUP_TIMEOUT=60           - Server startup timeout
#   THUNDERDUCK_VENV_DIR=.venv          - Override venv location
#   THUNDERDUCK_TEST_SUITE_CONTINUE_ON_ERROR=true
#   VERBOSE_FAILURES=true               - Use long tracebacks (--tb=long)

# Detect shell
if [ -n "$ZSH_VERSION" ]; then
    emulate -L sh
    setopt SH_WORD_SPLIT
    SCRIPT_PATH="${(%):-%x}"
elif [ -n "$BASH_VERSION" ]; then
    if [ "${BASH_VERSINFO[0]}" -lt 4 ]; then
        echo "ERROR: This script requires bash 4.0 or later (found: $BASH_VERSION)"
        exit 1
    fi
    SCRIPT_PATH="${BASH_SOURCE[0]}"
else
    echo "ERROR: This script requires bash 4+ or zsh"
    exit 1
fi

set -e

SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# All worktrees share one git common directory.  Resolve the main checkout
# dynamically so a worktree can reuse generated Spark, venv, and TPC assets
# without hard-coding the devcontainer's /workspace path or copying gigabytes.
COMMON_GIT_DIR="$(git -C "$WORKSPACE_DIR" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
if [ -n "$COMMON_GIT_DIR" ]; then
    MAIN_WORKSPACE_DIR="$(dirname "$COMMON_GIT_DIR")"
else
    MAIN_WORKSPACE_DIR="$WORKSPACE_DIR"
fi

# Source the env file written by setup-differential-testing.sh if present
# (sets SPARK_HOME, THUNDERDUCK_VENV_DIR, etc. to the vendored install).
ENV_FILE="$WORKSPACE_DIR/tests/integration/.env"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    . "$ENV_FILE"
fi

# .env predates worktree support and may contain the main checkout's absolute
# WORKSPACE_DIR. The path of this runner is the only valid source for the
# worktree under test.
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Fall back to the main checkout's shared install, then $HOME/spark/current,
# before failing in the prerequisite check below.
if [ -z "${SPARK_HOME:-}" ] || [ ! -d "$SPARK_HOME" ]; then
    SPARK_HOME="$MAIN_WORKSPACE_DIR/.spark/spark-4.1.1"
fi
[ -d "$SPARK_HOME" ] || SPARK_HOME="$HOME/spark/current"

if [[ "$1" == "--ci" ]]; then
    shift
    export THUNDERDUCK_TEST_SUITE_CONTINUE_ON_ERROR="${THUNDERDUCK_TEST_SUITE_CONTINUE_ON_ERROR:-true}"
    export COLLECT_TIMEOUT="${COLLECT_TIMEOUT:-30}"
fi

# Oracle mode (see tests/integration/utils/golden.py):
#   golden (default) — diff τ against recorded golden files; no Spark started.
#   live             — diff τ against a live Spark reference (full authority).
#   record           — run Spark and (over)write the goldens for the selection.
# --record is shorthand for --oracle record. These may precede the test group.
while true; do
    case "$1" in
        --record) export THUNDERDUCK_ORACLE=record; shift ;;
        --oracle) export THUNDERDUCK_ORACLE="$2"; shift 2 ;;
        --oracle=*) export THUNDERDUCK_ORACLE="${1#--oracle=}"; shift ;;
        *) break ;;
    esac
done
export THUNDERDUCK_ORACLE="${THUNDERDUCK_ORACLE:-golden}"
if [[ "$THUNDERDUCK_ORACLE" != "golden" && "$THUNDERDUCK_ORACLE" != "live" && "$THUNDERDUCK_ORACLE" != "record" ]]; then
    echo "ERROR: --oracle must be golden|live|record (got '$THUNDERDUCK_ORACLE')"
    exit 1
fi

# Resolve Python interpreter
VENV_DIR="${THUNDERDUCK_VENV_DIR:-}"
if [ -z "$VENV_DIR" ] || [ ! -x "$VENV_DIR/bin/python3" ]; then
    VENV_DIR="$MAIN_WORKSPACE_DIR/.venv"
fi
if [ -n "$VIRTUAL_ENV" ]; then
    PYTHON="python3"
elif [ -x "$VENV_DIR/bin/python3" ]; then
    PYTHON="$VENV_DIR/bin/python3"
elif command -v python3 &> /dev/null; then
    PYTHON="python3"
else
    echo "ERROR: No Python interpreter found."
    echo "Run the setup script first: $SCRIPT_DIR/setup-differential-testing.sh"
    exit 1
fi

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

PID_FILE="$WORKSPACE_DIR/tests/integration/logs/.server-pids"
PYTEST_PID=""

get_test_files() {
    case "$1" in
        core | core_v2) echo "differential/test_dataframe_corpus_differential.py" ;;
        sql_v2)      echo "differential/test_sql_corpus_differential.py" ;;
        tpch | tpcds) echo "differential/test_sql_corpus_differential.py differential/test_dataframe_corpus_differential.py" ;;
        functions)   echo "differential/test_dataframe_functions.py" ;;
        aggregations) echo "differential/test_multidim_aggregations.py" ;;
        window)      echo "differential/test_window_functions.py" ;;
        operations)  echo "differential/test_dataframe_ops_differential.py" ;;
        lambda)      echo "differential/test_lambda_differential.py" ;;
        joins)       echo "differential/test_joins_differential.py differential/test_using_joins_differential.py" ;;
        statistics)  echo "differential/test_statistics_differential.py" ;;
        types)       echo "differential/test_complex_types_differential.py differential/test_type_literals_differential.py" ;;
        schema)      echo "differential/test_to_schema_differential.py" ;;
        datetime)    echo "differential/test_datetime_functions_differential.py" ;;
        conditional) echo "differential/test_conditional_differential.py" ;;
        all)         echo "differential/" ;;
        *)           echo "" ;;
    esac
}

# Case-id filter (-k) applied on top of the group's test files. The tpch /
# tpcds groups select their corpus clusters by case-id prefix across BOTH
# corpora (SQL + DataFrame) — the clusters live inside the corpus files, so
# file selection alone would run the whole corpus.
get_test_filter() {
    case "$1" in
        tpch)  echo "tpch-" ;;
        tpcds) echo "tpcds-" ;;
        *)     echo "" ;;
    esac
}

get_test_description() {
    case "$1" in
        core)        echo "Conformance corpus (DataFrame API only, biased to divergence) — τ fitness gate" ;;
        core_v2)     echo "Alias for 'core' (retained for tooling that references core_v2 by name)" ;;
        sql_v2)      echo "Spark SQL conformance corpus (spark.sql) — τ SQL front-end gate" ;;
        tpch)        echo "TPC-H cluster (SQL + DataFrame corpus cases, tpch-* ids)" ;;
        tpcds)       echo "TPC-DS cluster (SQL + DataFrame corpus cases, tpcds-* ids)" ;;
        functions)   echo "DataFrame function parity tests" ;;
        aggregations) echo "Multi-dimensional aggregation tests" ;;
        window)      echo "Window function tests" ;;
        operations)  echo "DataFrame operations tests" ;;
        lambda)      echo "Lambda/HOF function tests" ;;
        joins)       echo "Join tests" ;;
        statistics)  echo "Statistics operations (cov, corr, describe)" ;;
        types)       echo "Complex types and type literals" ;;
        schema)      echo "ToSchema df.to(schema) tests" ;;
        datetime)    echo "Date/time function tests" ;;
        conditional) echo "Conditional expressions (when/otherwise)" ;;
        all)         echo "All differential tests (includes core)" ;;
        *)           echo "" ;;
    esac
}

cleanup() {
    echo ""
    echo -e "${BLUE}Cleaning up...${NC}"
    if [ -n "$PYTEST_PID" ] && kill -0 "$PYTEST_PID" 2>/dev/null; then
        kill "$PYTEST_PID" 2>/dev/null || true
        sleep 1
        kill -9 "$PYTEST_PID" 2>/dev/null || true
    fi
    if [ -f "$PID_FILE" ]; then
        while IFS=: read -r name port pid; do
            if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
                echo "  Stopping $name (PID: $pid, port: $port)..."
                kill -- -"$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
            fi
        done < "$PID_FILE"
        rm -f "$PID_FILE"
    fi
    echo -e "${GREEN}  Cleanup complete${NC}"
}
trap cleanup EXIT INT TERM

echo -e "${BLUE}================================================================${NC}"
echo -e "${BLUE}Differential Tests: Apache Spark 4.1.1 vs Thunderduck (Rust)${NC}"
echo -e "${BLUE}================================================================${NC}"
echo ""

echo -e "${BLUE}[1/2] Checking prerequisites...${NC}"

# Spark is only required for live/record oracle modes; golden mode never starts
# it (the reference comes from recorded golden files).
if [ ! -d "$SPARK_HOME" ] || [ ! -f "$SPARK_HOME/bin/spark-submit" ]; then
    if [[ "$THUNDERDUCK_ORACLE" == "golden" ]]; then
        echo -e "${YELLOW}  Spark not found at $SPARK_HOME — not needed (oracle=golden)${NC}"
    else
        echo -e "${RED}ERROR: Apache Spark not found at $SPARK_HOME (required for oracle=$THUNDERDUCK_ORACLE)${NC}"
        echo "Run the setup script first: $SCRIPT_DIR/setup-differential-testing.sh"
        exit 1
    fi
else
    echo -e "${GREEN}  Spark found at: $SPARK_HOME${NC}"
fi

TPCH_DATA_DIR="${THUNDERDUCK_TPCH_DATA_DIR:-$WORKSPACE_DIR/tests/integration/tpch_sf001}"
if [ ! -d "$TPCH_DATA_DIR" ]; then
    TPCH_DATA_DIR="$MAIN_WORKSPACE_DIR/tests/integration/tpch_sf001"
fi
if [ ! -d "$TPCH_DATA_DIR" ]; then
    echo -e "${RED}ERROR: TPC-H data not found at $TPCH_DATA_DIR${NC}"
    echo "Please ensure TPC-H data files exist in tests/integration/tpch_sf001/"
    exit 1
fi
echo -e "${GREEN}  TPC-H data found${NC}"

# Find Thunderduck Rust binary. ALWAYS build unless the caller supplied an
# explicit THUNDERDUCK_BINARY: cargo is a fast no-op when the tree is
# unchanged, and silently running the suite against a stale binary is a
# false-green machine — the differential gate must test the sources as they
# stand, not whatever binary happened to exist.
BINARY_PATH="${THUNDERDUCK_BINARY:-$WORKSPACE_DIR/target/release/thunderduck-connect-server}"
if [ -z "${THUNDERDUCK_BINARY:-}" ]; then
    echo -e "${YELLOW}  Building Thunderduck server (no-op when up to date)...${NC}"
    cd "$WORKSPACE_DIR"
    # Prepare the official static DuckDB library when this shell does not have
    # the local development environment.
    if [ -z "${DUCKDB_LIB_DIR:-}" ]; then
        "$WORKSPACE_DIR/scripts/dev/duckdb-build-cache.sh" ensure
        duckdb_dir="$("$WORKSPACE_DIR/scripts/dev/duckdb-build-cache.sh" dir)"
        export DUCKDB_LIB_DIR="$duckdb_dir/lib"
        export DUCKDB_INCLUDE_DIR="$duckdb_dir/include"
        export DUCKDB_STATIC=1
    fi
    # Subshell pipefail so the guard tests CARGO's exit status, not tail's
    # (plain `cmd | tail` under set -e without pipefail always sees tail's 0,
    # silently gating against a stale binary when the build breaks).
    if ! (set -o pipefail; cargo build --release 2>&1 | tail -20); then
        echo -e "${RED}ERROR: Failed to build Thunderduck server${NC}"
        exit 1
    fi
fi
if [ ! -f "$BINARY_PATH" ]; then
    echo -e "${RED}ERROR: Thunderduck binary not found at $BINARY_PATH${NC}"
    exit 1
fi
echo -e "${GREEN}  Thunderduck binary found: $BINARY_PATH${NC}"

show_help() {
    echo "Usage: $0 [--ci] [--record | --oracle MODE] [test-group] [pytest-args...]"
    echo ""
    echo "Test groups: core core_v2 sql_v2 tpch tpcds functions aggregations window datetime"
    echo "             conditional operations lambda joins statistics types schema all"
    echo ""
    echo "  core     — DataFrame-only conformance corpus (τ fitness gate)"
    echo "  core_v2  — alias for 'core' (retained for tooling that references it by name)"
    echo "  sql_v2   — Spark SQL conformance corpus (spark.sql), τ SQL front-end gate"
    echo "  tpch     — TPC-H cluster: tpch-* corpus cases (SQL + DataFrame)"
    echo "  tpcds    — TPC-DS cluster: tpcds-* corpus cases (SQL + DataFrame)"
    echo "  all      — everything including core (the comprehensive gate)"
    echo ""
    echo "Oracle mode (the two conformance corpora only — core/core_v2/sql_v2):"
    echo "  --oracle golden   (default) diff τ against recorded golden files; NO Spark."
    echo "  --oracle live               diff τ against a live Spark reference (authority)."
    echo "  --oracle record | --record  run Spark and (over)write goldens for the selection."
    echo ""
    echo "  Add / change a case, then record + commit its golden:"
    echo "    $0 --record core -k my-new-case      # writes goldens/dataframe/my-new-case.json"
    echo "  Refresh everything after an input-fixture or Spark-pin change:"
    echo "    $0 --record core && $0 --record sql_v2"
    echo ""
    echo "Extra pytest args are forwarded verbatim (quoting preserved), e.g.:"
    echo "  $0 sql_v2 -k 'tpch-q01 or sel-001' --tb=long"
    echo "NOTE: the tpch/tpcds groups already use -k for cluster selection;"
    echo "passing another -k there overrides it (pytest keeps the last -k)."
    exit 0
}

if [[ "$1" == "-h" || "$1" == "--help" ]]; then show_help; fi

TEST_GROUP="${1:-all}"
PYTEST_ARGS=("${@:2}")

TEST_FILES="$(get_test_files "$TEST_GROUP")"
if [ -z "$TEST_FILES" ]; then
    if [[ "$TEST_GROUP" == -* || "$TEST_GROUP" == *.py ]]; then
        PYTEST_ARGS=("$@")
        TEST_GROUP="all"
        TEST_FILES="$(get_test_files "$TEST_GROUP")"
    else
        echo -e "${RED}ERROR: Unknown test group '$TEST_GROUP'${NC}"
        exit 1
    fi
fi

# Cluster selection for tpch/tpcds (see get_test_filter).
FILTER_ARGS=()
TEST_FILTER="$(get_test_filter "$TEST_GROUP")"
if [ -n "$TEST_FILTER" ]; then
    FILTER_ARGS=(-k "$TEST_FILTER")
fi

echo ""
echo -e "${BLUE}[2/2] Running tests...${NC}"
echo ""
echo -e "  ${CYAN}Test group:${NC} $TEST_GROUP ($(get_test_description "$TEST_GROUP"))"
echo -e "  ${CYAN}Test files:${NC} $TEST_FILES"
echo ""
echo -e "  ${CYAN}Configuration:${NC}"
echo -e "    Oracle:            $THUNDERDUCK_ORACLE"
echo -e "    Python:            $PYTHON"
echo -e "    Binary:            $BINARY_PATH"
echo -e "    Spark port:        ${SPARK_PORT:-auto}"
echo -e "    Thunderduck port:  ${THUNDERDUCK_PORT:-auto}"
echo -e "    Collect timeout:   ${COLLECT_TIMEOUT:-10}s"
echo -e "    Continue on error: ${THUNDERDUCK_TEST_SUITE_CONTINUE_ON_ERROR:-false}"
echo ""

export SPARK_HOME
export THUNDERDUCK_BINARY="$BINARY_PATH"
export THUNDERDUCK_TPCH_DATA_DIR="$TPCH_DATA_DIR"
export THUNDERDUCK_TPCDS_DATA_DIR="${THUNDERDUCK_TPCDS_DATA_DIR:-$MAIN_WORKSPACE_DIR/tests/integration/tpcds_sf001}"

cd "$WORKSPACE_DIR/tests/integration"

TB_STYLE=""
if [ "${VERBOSE_FAILURES:-false}" = "true" ]; then
    TB_STYLE="--tb=long"
fi

set +e
# shellcheck disable=SC2086
$PYTHON -m pytest \
    $TEST_FILES \
    "${FILTER_ARGS[@]}" \
    $TB_STYLE \
    "${PYTEST_ARGS[@]}" &
PYTEST_PID=$!
wait $PYTEST_PID
TEST_EXIT_CODE=$?
set -e

echo ""
echo -e "${BLUE}================================================================${NC}"
echo -e "${BLUE}Test Group: ${CYAN}$TEST_GROUP${NC}"
echo -e "${BLUE}================================================================${NC}"
if [ $TEST_EXIT_CODE -eq 0 ]; then
    echo -e "${GREEN}ALL TESTS PASSED${NC}"
else
    echo -e "${RED}SOME TESTS FAILED${NC}"
fi
echo -e "${BLUE}================================================================${NC}"

exit $TEST_EXIT_CODE
