# Testing Guide

> **Scope: τ (the only production path per ADR-022).** The differential oracle validates τ against Apache Spark 4.1.1 as the Spark-parity contract; the two corpora are the fitness functions — the DataFrame corpus (`run-differential-tests.sh core`, 405 cases) and the SQL corpus (`run-differential-tests.sh sql_v2`, 408 cases); `tests/scripts/differential-progress.sh` runs the entire suite and records the progress row. TPC-H/TPC-DS live INSIDE the corpora as `tpch-*`/`tpcds-*` cases (migrated 2026-07-09; the standalone TPC test files are gone).

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

> **Spark IS INSTALLED** — vendored in the **main checkout** at `/workspace/.spark/spark-4.1.1`
> (with its venv at `/workspace/.venv`). The runner's default probe (`$HOME/spark/current`)
> misses it, and worktrees have no in-tree `.spark/`. From a worktree, export the paths first:
> ```bash
> export SPARK_HOME=/workspace/.spark/spark-4.1.1 THUNDERDUCK_VENV_DIR=/workspace/.venv
> ```
> Do **not** re-run `setup-differential-testing.sh` — Spark is already present.

### Oracle modes — golden by default, live/record on demand

The two conformance corpora (`core`, `sql_v2`) run against a **golden-file
oracle by default**: each case's reference result was captured once from Apache
Spark 4.1.1 and is stored per-case at
`tests/integration/differential/goldens/{dataframe,sql}/<case-id>.json` (checked
into git). Normal runs execute **only τ** and diff against the golden — **no
Spark JVM is started**, so `core`/`sql_v2` finish in ~4–5 s each. The golden is a
*cache* of the ADR-015 reference oracle (not a replacement); `--oracle live`
remains the authority. Mode is selected by `THUNDERDUCK_ORACLE` (default
`golden`) or the runner flags:

```bash
./tests/scripts/run-differential-tests.sh core            # golden (default): τ-only, no Spark
./tests/scripts/run-differential-tests.sh --oracle live core   # diff τ vs a live Spark reference
./tests/scripts/run-differential-tests.sh --record core -k my-new-case  # capture Spark → golden
```

Add or change a case, then `--record -k <id>` and commit the regenerated
golden. Re-record after any input-fixture or Spark-pin change. Recording a large
heavy cluster live can hit a cumulative Spark-reference slowdown — record in
smaller `-k` chunks (fresh Spark session each) if the tail crawls. See
`tests/integration/utils/golden.py`. (The full `all` suite and the legacy
feature-family modules still run live Spark; only the two corpora are goldened.)

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

- **DataFrame corpus** (`test_dataframe_corpus_differential.py`) — 405 cases (incl. 22 `tpch-*` + 33 `tpcds-*` DataFrame cluster cases); the τ fitness function.
- **SQL corpus** (`test_sql_corpus_differential.py`) — 408 cases (incl. 22 `tpch-*` + 100 `tpcds-*` SQL cluster cases); the τ SQL front-end fitness function. TPC cases are held to the same standard as every other case — a red TPC case is a defect to fix.
- **Full suite**: `pytest differential/` — both corpora plus the remaining feature-family legacy files (joins, aggregations, window functions, datetime, ...).
- **TPC clusters**: `run-differential-tests.sh tpch` / `tpcds`.
- **Single case**: `-k <case-id>` or the explicit `test_case[<id>]` node id.

### TPC cluster mechanics

- SQL cluster cases load query text verbatim at import time from `tests/integration/sql/{tpch,tpcds}_queries/*.sql` (those files remain the single source of truth).
- DataFrame cluster implementations live in `differential/tpch_dataframe_queries.py` and `differential/tpcds_dataframe_queries.py`; cases adapt them via the session that built the corpus inputs.
- Parquet-backed temp views are registered by the corpus fixtures (`conftest._register_tpc_views`). `customer` exists in BOTH benchmarks with different schemas — the `tpc_view_switcher` fixture re-points it per case category.
- The pre-migration per-query baseline (regression oracle) is `.agent-output/tpc-baseline.md`; `check-tpc-migration.sh` compares against it.

## Per-worktree test isolation

Multiple git worktrees run tests on one machine. Each worktree gets its own
remembered server ports, isolated Spark daemon state, and scoped cleanup, so
runs never clash and cleanup never kills another worktree's servers:

- **Ports** are picked once (random free ports) and persisted to
  `<worktree_root>/.thunderduck-test-env.json` by
  `tests/integration/utils/test_env.py`; runs reuse them, so a manual PySpark
  client connects to the right instance (`sc://localhost:<thunderduck_port>`)
  and cleanup can locate dangling servers. Explicit `THUNDERDUCK_PORT` /
  `SPARK_PORT` env vars still override.
- **Spark reference** runs with per-worktree `SPARK_PID_DIR` / `SPARK_LOG_DIR` /
  `SPARK_IDENT_STRING` and a `-Dthunderduck.worktree=<id>` JVM marker, so Spark's
  (port-agnostic) class+instance daemon bookkeeping doesn't collide.
- **Cleanup**: `./tests/scripts/kill-test-servers.sh` kills only *this*
  worktree's servers after proving ownership (Thunderduck binary under this
  worktree's `target/`, or the Spark JVM marker). `--all` sweeps every worktree
  (still ownership-verified); `--stale` reaps only orphaned (ppid==1) servers;
  `--list` / `--list-all` show status. **Never** use
  `pkill -f thunderduck-connect-server` — it crosses worktrees.
- **Unit tests** are already isolated (in-memory DuckDB, per-process extension
  temp dir); `THUNDERDUCK_DUCKDB_EXTENSION_DIR` optionally points DuckDB's
  `INSTALL` cache at a per-worktree dir to avoid a shared-`~/.duckdb` race.

### Devcontainer worktree workflow

Create worktrees *inside the mounted checkout* so one devcontainer can see
them all. They are real Git worktrees, not copied repositories:

```bash
# Run once from the main checkout after opening the devcontainer.
scripts/dev/dev-cache-setup.sh

# Create two independent branches that are visible at /workspace/.worktrees/.
git worktree add -b feature/one .worktrees/feature-one HEAD
git worktree add -b feature/two .worktrees/feature-two HEAD
```

Use a separate terminal for each worktree. Keep its `target/` directory local
to that worktree—**do not set a shared `CARGO_TARGET_DIR`**. Cargo mutates that
directory and concurrent builds would contend, invalidate each other's
incremental state, and encode worktree-specific paths. The dev cache setup
shares the safe, expensive parts instead: `sccache` and the prebuilt
`libduckdb_static.a` under the main checkout's `.build-cache/`; the mandatory
`thdck_spark_funcs` extension is already vendored in every worktree.

```bash
cd .worktrees/feature-one
cargo test
./tests/scripts/run-differential-tests.sh core

cd ../feature-two
cargo test
./tests/scripts/run-differential-tests.sh sql_v2
```

The differential runner resolves Spark, the Python venv, and generated TPC
data through Git's common directory when they are absent from a linked
worktree. No `/workspace` export or copied `.spark`, `.venv`, or parquet data
is needed. Each worktree retains its own release binary, remembered ports,
Spark PID/log state, DuckDB extension cache, and cleanup scope.

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
