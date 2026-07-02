# Testing Guide

> **Scope: universal — applies to both legacy and v2 code.** Unit tests, integration/differential tests, and the differential harness itself are shared infrastructure. Per ADR-015, the differential oracle is the parity contract between v2 and legacy — both implementations are validated against the same corpus and the same result-canonicalization rules. Under ADR-021, code-sharing between the two implementations is limited to value-level types; test infrastructure is not "shared code" in the ADR-021 sense, it's the test harness that validates parity.
>
> The `core_v2` differential suite (currently reads 12/324 baseline post-v2-delete) is the v2 progress signal per `tests/scripts/v2-progress.sh`; it re-populates as Slice A → Slice J land.

## Unit Tests (`cargo test`)

```bash
# All unit tests
cargo test

# Single module
cargo test -p thunderduck-core -- types::

# Single test
cargo test -p thunderduck-core -- generator::tests::test_project_to_sql

# With stdout output
cargo test -- --nocapture
```

## Integration / Differential Tests (pytest)

The differential suite validates Thunderduck against Apache Spark 4.1.1 by running the same query through both engines and diffing the result.

### Via run script (preferred)

```bash
# Full differential test suite (all 41 test files)
./tests/scripts/run-differential-tests.sh all

# Quick check: TPC-H only
./tests/scripts/run-differential-tests.sh tpch
```

### Direct pytest (activate venv first)

```bash
# All differential tests
cd tests/integration && python3 -m pytest differential/ -v --tb=short

# Single parameterized SQL query (e.g., TPC-H Q7)
cd tests/integration && python3 -m pytest \
  "differential/test_differential_v2.py::TestTPCH_AllQueries_Differential[7]" -v --tb=long
```

### Test Tiers

- **Full suite**: `pytest differential/` — runs all 41 test files (TPC-H, TPC-DS, joins, aggregations, window functions, array functions, datetime, type casting, JSON, math/bitwise, string/collection, etc.)
- **Quick check**: `test_differential_v2.py test_tpch_differential.py` — TPC-H only
- **TPC-DS**: `test_tpcds_differential.py test_tpcds_dataframe_differential.py`
- **Single file/test**: target specific files or parameterized tests

## Key Data & SQL Paths

| Resource | Path |
|----------|------|
| TPC-H parquet data | `tests/integration/tpch_sf001/*.parquet` |
| TPC-H SQL queries | `tests/integration/sql/tpch_queries/q{1-22}.sql` |
| TPC-DS SQL queries | `tests/integration/sql/tpcds_queries/q{1-99}.sql` |
| Test conftest | `tests/integration/conftest.py` |
| DataFrame diff util | `tests/integration/utils/dataframe_diff.py` |
