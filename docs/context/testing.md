# Testing Guide

> **Scope: τ (the only production path per ADR-022).** The differential oracle validates τ against Apache Spark 4.1.1 as the Spark-parity contract; the two corpora are the fitness functions — the DataFrame corpus (`run-differential-tests.sh core`, 384 cases) and the SQL corpus (`run-differential-tests.sh sql_v2`, 396 cases); `tests/scripts/differential-progress.sh` runs the entire suite and records the progress row. TPC-H/TPC-DS live INSIDE the corpora as `tpch-*`/`tpcds-*` cases (migrated 2026-07-09; the standalone TPC test files are gone).

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

### Via run script (preferred — the single entry point)

`run-differential-tests.sh` runs everything (`all`) or a subgroup, and forwards
extra pytest args verbatim (quoting preserved).

```bash
# DataFrame corpus — the primary τ fitness gate
./tests/scripts/run-differential-tests.sh core

# SQL corpus — the τ SQL front-end gate
./tests/scripts/run-differential-tests.sh sql_v2

# ENTIRE suite + progress row — runs `all` once, buckets outcomes
# (DataFrame corpus / SQL corpus / other), appends a row to
# tests/integration/differential_progress.md (the single progress ledger;
# it replaced v2-progress.sh / v2-sql-progress.sh on 2026-07-09)
./tests/scripts/differential-progress.sh

# Full differential test suite (both corpora + all remaining legacy files)
./tests/scripts/run-differential-tests.sh all

# TPC clusters only — selected by case-id prefix across BOTH corpora
./tests/scripts/run-differential-tests.sh tpch    # 44 cases: 22 SQL + 22 DataFrame
./tests/scripts/run-differential-tests.sh tpcds   # 133 cases: 100 SQL + 33 DataFrame

# Arbitrary case selection via forwarded pytest args
./tests/scripts/run-differential-tests.sh sql_v2 -k "tpch-q01 or sel-001" --tb=long

# TPC-migration invariants (structure + zero regressions vs recorded baseline)
./tests/scripts/check-tpc-migration.sh                 # full (~2 min)
./tests/scripts/check-tpc-migration.sh --collect-only  # structural only
```

### Direct pytest (activate venv first)

```bash
# DataFrame corpus (τ conformance)
cd tests/integration && python3 -m pytest \
  differential/test_dataframe_corpus_differential.py -v --tb=short

# All differential tests
cd tests/integration && python3 -m pytest differential/ -v --tb=short

# Single corpus case (e.g., TPC-H Q7 on the SQL front-end)
cd tests/integration && python3 -m pytest \
  "differential/test_sql_corpus_differential.py::test_case[tpch-q07]" -v --tb=long
```

### Test tiers

- **DataFrame corpus** (`test_dataframe_corpus_differential.py`) — 384 cases (incl. 22 `tpch-*` + 33 `tpcds-*` DataFrame cluster cases); the τ fitness function.
- **SQL corpus** (`test_sql_corpus_differential.py`) — 396 cases (incl. 22 `tpch-*` + 100 `tpcds-*` SQL cluster cases); the τ SQL front-end fitness function. TPC cases are held to the same standard as every other case — a red TPC case is a defect to fix.
- **Full suite**: `pytest differential/` — both corpora plus the remaining feature-family legacy files (joins, aggregations, window functions, datetime, ...).
- **TPC clusters**: `run-differential-tests.sh tpch` / `tpcds`.
- **Single case**: `-k <case-id>` or the explicit `test_case[<id>]` node id.

### TPC cluster mechanics

- SQL cluster cases load query text verbatim at import time from `tests/integration/sql/{tpch,tpcds}_queries/*.sql` (those files remain the single source of truth).
- DataFrame cluster implementations live in `differential/tpch_dataframe_queries.py` and `differential/tpcds_dataframe_queries.py`; cases adapt them via the session that built the corpus inputs.
- Parquet-backed temp views are registered by the corpus fixtures (`conftest._register_tpc_views`). `customer` exists in BOTH benchmarks with different schemas — the `tpc_view_switcher` fixture re-points it per case category.
- The pre-migration per-query baseline (regression oracle) is `.agent-output/tpc-baseline.md`; `check-tpc-migration.sh` compares against it.

## Key Data & SQL Paths

| Resource | Path |
|----------|------|
| TPC-H parquet data | `tests/integration/tpch_sf001/*.parquet` |
| TPC-DS parquet data | `tests/integration/tpcds_sf001/*.parquet` (auto-generated if absent) |
| TPC-H SQL queries | `tests/integration/sql/tpch_queries/q{1-22}.sql` |
| TPC-DS SQL queries | `tests/integration/sql/tpcds_queries/q{1-99}.sql` |
| DataFrame corpus | `tests/integration/differential/dataframe_corpus.py` |
| SQL corpus | `tests/integration/differential/sql_corpus.py` |
| TPC DataFrame query impls | `tests/integration/differential/{tpch,tpcds}_dataframe_queries.py` |
| Test conftest | `tests/integration/conftest.py` |
| DataFrame diff util | `tests/integration/utils/dataframe_diff.py` |
