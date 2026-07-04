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

# Source the env file written by setup-differential-testing.sh if present
# (sets SPARK_HOME, THUNDERDUCK_VENV_DIR, etc. to the vendored install).
ENV_FILE="$WORKSPACE_DIR/tests/integration/.env"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    . "$ENV_FILE"
fi

# Fall back to the vendored in-tree install, then $HOME/spark/current for
# legacy setups, before failing in the prerequisite check below.
SPARK_HOME="${SPARK_HOME:-$WORKSPACE_DIR/.spark/spark-4.1.1}"
[ -d "$SPARK_HOME" ] || SPARK_HOME="$HOME/spark/current"

# Handle --ci flag
if [[ "$1" == "--ci" ]]; then
    shift
    export THUNDERDUCK_TEST_SUITE_CONTINUE_ON_ERROR="${THUNDERDUCK_TEST_SUITE_CONTINUE_ON_ERROR:-true}"
    export COLLECT_TIMEOUT="${COLLECT_TIMEOUT:-30}"
fi

# Resolve Python interpreter
VENV_DIR="${THUNDERDUCK_VENV_DIR:-$WORKSPACE_DIR/.venv}"
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

# Colors
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
        tpch)        echo "differential/test_differential_v2.py differential/test_tpch_differential.py" ;;
        tpcds)       echo "differential/test_tpcds_differential.py differential/test_tpcds_dataframe_differential.py" ;;
        functions)   echo "differential/test_dataframe_functions.py" ;;
        aggregations) echo "differential/test_multidim_aggregations.py" ;;
        window)      echo "differential/test_window_functions.py" ;;
        operations)  echo "differential/test_dataframe_ops_differential.py" ;;
        lambda)      echo "differential/test_lambda_differential.py" ;;
        joins)       echo "differential/test_joins_differential.py differential/test_using_joins_differential.py" ;;
        statistics)  echo "differential/test_statistics_differential.py" ;;
        types)       echo "differential/test_complex_types_differential.py differential/test_type_literals_differential.py" ;;
        schema)      echo "differential/test_to_schema_differential.py" ;;
        dataframe)   echo "differential/test_tpcds_dataframe_differential.py" ;;
        datetime)    echo "differential/test_datetime_functions_differential.py" ;;
        conditional) echo "differential/test_conditional_differential.py" ;;
        all)         echo "differential/" ;;
        *)           echo "" ;;
    esac
}

get_test_description() {
    case "$1" in
        core)        echo "Conformance corpus (DataFrame API only, biased to divergence) — legacy transpiler" ;;
        core_v2)     echo "Same DataFrame corpus through τ — v2 conformance gate" ;;
        sql_v2)      echo "Spark SQL conformance corpus (spark.sql) — τ SQL front-end gate" ;;
        tpch)        echo "TPC-H SQL and DataFrame tests" ;;
        tpcds)       echo "TPC-DS SQL and DataFrame tests" ;;
        functions)   echo "DataFrame function parity tests" ;;
        aggregations) echo "Multi-dimensional aggregation tests" ;;
        window)      echo "Window function tests" ;;
        operations)  echo "DataFrame operations tests" ;;
        lambda)      echo "Lambda/HOF function tests" ;;
        joins)       echo "Join tests" ;;
        statistics)  echo "Statistics operations (cov, corr, describe)" ;;
        types)       echo "Complex types and type literals" ;;
        schema)      echo "ToSchema df.to(schema) tests" ;;
        dataframe)   echo "TPC-DS DataFrame API tests" ;;
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

# Check prerequisites
echo -e "${BLUE}[1/2] Checking prerequisites...${NC}"

if [ ! -d "$SPARK_HOME" ] || [ ! -f "$SPARK_HOME/bin/spark-submit" ]; then
    echo -e "${RED}ERROR: Apache Spark not found at $SPARK_HOME${NC}"
    echo "Run the setup script first: $SCRIPT_DIR/setup-differential-testing.sh"
    exit 1
fi
echo -e "${GREEN}  Spark found at: $SPARK_HOME${NC}"

if [ ! -d "$WORKSPACE_DIR/tests/integration/tpch_sf001" ]; then
    echo -e "${RED}ERROR: TPC-H data not found at $WORKSPACE_DIR/tests/integration/tpch_sf001${NC}"
    echo "Please ensure TPC-H data files exist in tests/integration/tpch_sf001/"
    exit 1
fi
echo -e "${GREEN}  TPC-H data found${NC}"

# Find Thunderduck Rust binary
BINARY_PATH="${THUNDERDUCK_BINARY:-$WORKSPACE_DIR/target/release/thunderduck-connect-server}"
if [ ! -f "$BINARY_PATH" ]; then
    echo -e "${YELLOW}  Thunderduck binary not found at $BINARY_PATH. Building...${NC}"
    cd "$WORKSPACE_DIR"
    # DuckDB is non-bundled by default. If no external libduckdb is configured
    # (DUCKDB_LIB_DIR — set by local dev via scripts/dev/), compile it from
    # source so fresh clones / CI link successfully.
    BUILD_FEATURES=""
    if [ -z "${DUCKDB_LIB_DIR:-}" ]; then
        echo -e "${YELLOW}  DUCKDB_LIB_DIR unset — compiling DuckDB from source (--features bundled)${NC}"
        BUILD_FEATURES="--features bundled"
    fi
    cargo build --release $BUILD_FEATURES 2>&1 | tail -20
    if [ ! -f "$BINARY_PATH" ]; then
        echo -e "${RED}ERROR: Failed to build Thunderduck server${NC}"
        exit 1
    fi
fi
echo -e "${GREEN}  Thunderduck binary found: $BINARY_PATH${NC}"

# Parse arguments
show_help() {
    echo "Usage: $0 [--ci] [test-group] [pytest-args...]"
    echo ""
    echo "Test groups: core core_v2 sql_v2 tpch tpcds functions aggregations window datetime"
    echo "             conditional operations lambda joins statistics types schema dataframe all"
    echo ""
    echo "  core     — DataFrame-only conformance corpus, legacy transpiler (the fast gate)"
    echo "  core_v2  — same corpus through τ (v2 dev gate)"
    echo "  sql_v2   — Spark SQL conformance corpus (spark.sql), τ SQL front-end gate"
    echo "  all      — everything including core (the comprehensive gate)"
    exit 0
}

if [[ "$1" == "-h" || "$1" == "--help" ]]; then show_help; fi

TEST_GROUP="${1:-all}"
PYTEST_ARGS="${@:2}"

TEST_FILES="$(get_test_files "$TEST_GROUP")"
if [ -z "$TEST_FILES" ]; then
    if [[ "$TEST_GROUP" == -* || "$TEST_GROUP" == *.py ]]; then
        PYTEST_ARGS="$@"
        TEST_GROUP="all"
        TEST_FILES="$(get_test_files "$TEST_GROUP")"
    else
        echo -e "${RED}ERROR: Unknown test group '$TEST_GROUP'${NC}"
        exit 1
    fi
fi

# Run tests
echo ""
echo -e "${BLUE}[2/2] Running tests...${NC}"
echo ""
echo -e "  ${CYAN}Test group:${NC} $TEST_GROUP ($(get_test_description "$TEST_GROUP"))"
echo -e "  ${CYAN}Test files:${NC} $TEST_FILES"
echo ""
echo -e "  ${CYAN}Configuration:${NC}"
echo -e "    Python:            $PYTHON"
echo -e "    Binary:            $BINARY_PATH"
echo -e "    Spark port:        ${SPARK_PORT:-auto}"
echo -e "    Thunderduck port:  ${THUNDERDUCK_PORT:-auto}"
echo -e "    Collect timeout:   ${COLLECT_TIMEOUT:-10}s"
echo -e "    Continue on error: ${THUNDERDUCK_TEST_SUITE_CONTINUE_ON_ERROR:-false}"
echo ""

export SPARK_HOME
export THUNDERDUCK_BINARY="$BINARY_PATH"

# τ is the only transpiler path (ADR-022); there is no dispatch flag to set.

cd "$WORKSPACE_DIR/tests/integration"

TB_STYLE=""
if [ "${VERBOSE_FAILURES:-false}" = "true" ]; then
    TB_STYLE="--tb=long"
fi

set +e
# shellcheck disable=SC2086
$PYTHON -m pytest \
    $TEST_FILES \
    $TB_STYLE \
    $PYTEST_ARGS &
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
