# Testing Guide

> **Scope: τ (the only production path per ADR-022).** The differential oracle validates τ against Apache Spark 4.1.1 as the Spark-parity contract; the DataFrame corpus (`tests/scripts/v2-progress.sh`, 324 cases) is the fitness function.

## Unit Tests (`cargo test`)

```bash
# All unit tests
cargo test

# Single module
cargo test -p thunderduck-core -- types::

# Single test
cargo test -p thunderduck-core -- transpiler_v2::emission::tests::render_project

# With stdout output
cargo test -- --nocapture
```

## Integration / Differential Tests (pytest)

The differential suite validates Thunderduck against Apache Spark 4.1.1 by running the same query through both engines and diffing the result.

### Via run script (preferred)

```bash
# DataFrame corpus — the primary τ fitness gate
./tests/scripts/v2-progress.sh

# Full differential test suite (TPC-H + TPC-DS + everything)
./tests/scripts/run-differential-tests.sh all

# Quick check: TPC-H only
./tests/scripts/run-differential-tests.sh tpch
```

### Direct pytest (activate venv first)

```bash
# DataFrame corpus (τ conformance)
cd tests/integration && python3 -m pytest \
  differential/test_dataframe_corpus_differential.py -v --tb=short

# All differential tests
cd tests/integration && python3 -m pytest differential/ -v --tb=short

# Single parameterized SQL query (e.g., TPC-H Q7)
cd tests/integration && python3 -m pytest \
  "differential/test_differential_v2.py::TestTPCH_AllQueries_Differential[7]" -v --tb=long
```

### Test tiers

- **DataFrame corpus** (`test_dataframe_corpus_differential.py`) — 324 cases; the τ fitness function.
- **Full suite**: `pytest differential/` — runs all differential files (TPC-H, TPC-DS, joins, aggregations, window functions, array functions, datetime, type casting, JSON, math/bitwise, string/collection).
- **Quick check**: TPC-H via `run-differential-tests.sh tpch`.
- **Single file/test**: target specific files or parameterized tests.

## Key Data & SQL Paths

| Resource | Path |
|----------|------|
| TPC-H parquet data | `tests/integration/tpch_sf001/*.parquet` |
| TPC-H SQL queries | `tests/integration/sql/tpch_queries/q{1-22}.sql` |
| TPC-DS SQL queries | `tests/integration/sql/tpcds_queries/q{1-99}.sql` |
| DataFrame corpus | `tests/integration/differential/dataframe_corpus.py` |
| Test conftest | `tests/integration/conftest.py` |
| DataFrame diff util | `tests/integration/utils/dataframe_diff.py` |
