# Thunderduck Rust — Implementation Plan

## Guiding Principles

- **DataFrame path first**: The protobuf → LogicalPlan → SQL path is the primary value driver. The SparkSQL (raw SQL string) path is secondary and added in Phase 2.
- **Differential tests are the acceptance criteria**: Each phase ends when a measurable subset of the 746 differential tests passes.
- **Core before server**: The `core` crate (pure translation logic) is fully unit-testable without any gRPC or DuckDB dependencies. Build and test it independently before wiring up the server.
- **No premature abstractions**: Implement what's needed for the current phase. Port the function registry exhaustively, not incrementally.

---

## Phase Overview

| Phase | Deliverable | Acceptance Criteria |
|-------|-------------|---------------------|
| **1** | Core types + SQL generation | All unit tests pass; SQL generator produces correct DuckDB SQL for all 29 plan node types |
| **2** | DuckDB runtime + Arrow streaming | DuckDB queries execute; Arrow batches stream correctly; relaxed-mode integration test server starts |
| **3** | gRPC server + protobuf converter | PySpark client connects; TPC-H DataFrame path tests pass (target: 30+ tests) |
| **4** | Differential test parity (relaxed) | TPC-H 100% pass in relaxed mode; TPC-DS 95%+ pass |
| **5** | SparkSQL parser | Raw `spark.sql()` path works; SQL-based differential tests pass |
| **6** | Strict mode + extension integration | `thdck_spark_funcs` extension loaded; strict mode TPC-H/TPC-DS 100% pass |

---

## Phase 1 — Core Types + SQL Generation

**Goal**: A fully unit-tested `core` crate that can translate any `LogicalPlan` tree into correct DuckDB SQL. No DuckDB, no gRPC, no network — pure Rust.

**See**: [docs/phase1-detailed.md](phase1-detailed.md) for the full breakdown.

**Deliverables**:
- `DataType` enum + `StructType` + `StructField`
- `Expression` enum (21+ variants) with `to_sql()`, `data_type()`, `nullable()`
- `LogicalPlan` enum (29 variants)
- `TypeInferenceEngine`
- `SqlGenerator` with exhaustive `match` on all plan + expression variants
- `FunctionRegistry` (500+ mappings)
- Unit tests for every plan node and expression type

**Does NOT include**: DuckDB execution, Arrow, gRPC, SparkSQL parser, protobuf conversion.

---

## Phase 2 — DuckDB Runtime + Arrow Streaming

**Goal**: Execute SQL against DuckDB and stream Arrow batches back. Prove the zero-copy path works end-to-end with a standalone integration test (no PySpark client yet).

**Deliverables**:
- `DuckDbSession` struct: owns `duckdb::Connection` on a dedicated OS thread; communicates via `mpsc` channels
- `SessionManager`: `DashMap`-based concurrent session map; named in-memory databases per session
- `ArrowStreamer`: drives `Connection::query_arrow()` and yields `RecordBatch` objects
- Extension loading: detect platform, embed `thdck_spark_funcs.duckdb_extension` via `include_bytes!`, extract and `LOAD`
- `CompatMode` detection: auto-detect based on extension availability
- DuckDB configuration: memory limit, thread count, timezone, null ordering, `NULLS FIRST`
- Rust integration test: `SqlGenerator` → DuckDB SQL → `DuckDbSession` → Arrow batch → assert schema + data

**Does NOT include**: gRPC server, PySpark client.

---

## Phase 3 — gRPC Server + Protobuf Converter (DataFrame Path)

**Goal**: A working Spark Connect server that PySpark can connect to. DataFrame API operations only — no raw SQL parsing.

**Deliverables**:
- `connect-server` crate with tonic service
- Copy Spark Connect `.proto` files; `build.rs` compiles them with `tonic_build`
- `RelationConverter`: all 29 Spark Connect relation types → `LogicalPlan` variants
- `ExpressionConverter`: all Spark Connect expression types → `Expression` variants
- `SparkConnectService`: `execute_plan()` and `analyze_plan()` tonic handlers
- Session ID extraction and routing to `SessionManager`
- Arrow IPC serialisation of `RecordBatch` into `ExecutePlanResponse` stream
- `ReleaseSession` / `ReleaseExecute` gRPC method stubs
- Smoke test: `pyspark.sql.SparkSession.builder.remote("sc://localhost:15002").getOrCreate()` succeeds

**Acceptance criteria**: `tests/integration/differential/test_simple_sql.py` and basic DataFrame operations pass.

---

## Phase 4 — Differential Test Parity (Relaxed Mode)

**Goal**: Pass the full TPC-H and TPC-DS differential test suites in relaxed mode.

**Deliverables**:
- Close all gaps discovered during Phase 3 testing: missing relation types, expression types, function mappings
- Complete `FunctionRegistry` coverage for all functions exercised by TPC-H / TPC-DS
- `SchemaInferrer`: fallback schema inference via DuckDB `DESCRIBE` when plan-level inference is unavailable
- Temp view support (`CREATE TEMPORARY VIEW`, `createOrReplaceTempView`)
- `WriteOperation` support: local Parquet write
- All join types correct (INNER, LEFT, RIGHT, FULL, CROSS, SEMI, ANTI)
- Window functions correct
- Aggregation with ROLLUP / CUBE / GROUPING SETS
- CTE (`WITH`) support

**Acceptance criteria**:
- TPC-H: 100% pass (relaxed mode)
- TPC-DS: 95%+ pass (relaxed mode)
- All window, join, aggregation differential tests pass

---

## Phase 5 — SparkSQL Parser

**Goal**: Raw SQL strings passed via `spark.sql("SELECT ...")` are parsed and executed correctly.

**Deliverables**:
- `SparkSqlParser` using `sqlparser-rs` with a custom `SparkDialect`
- `SparkDialect` additions: `TABLESAMPLE`, `LATERAL VIEW EXPLODE/POSEXPLODE`, `DISTRIBUTE BY`, `CLUSTER BY`, `TRANSFORM`, lambda syntax, `PIVOT`, Spark `INTERVAL` literals
- AST builder: sqlparser-rs `Statement` → Thunderduck `LogicalPlan` + `Expression` tree
- Schema-aware column resolution in the parser (uses `DuckDb::DESCRIBE` for referenced tables)
- SLL-first, LL-fallback parsing strategy (mirrors Java reference)

**Acceptance criteria**:
- All SQL-path differential tests pass (TPC-H SQL queries, TPC-DS SQL queries)
- `test_differential_v2.py`, `test_tpcds_differential.py` fully pass in relaxed mode

---

## Phase 6 — Strict Mode + Extension Integration

**Goal**: Exact Spark numeric semantics via the `thdck_spark_funcs` DuckDB extension.

**Deliverables**:
- Confirmed extension build from `thunderduck-duckdb-extension` (v1.5.0 branch) for all 4 platforms
- `FunctionRegistry` strict-mode routing: `round()`, `avg()` on Decimal, `sum()` on int → extension functions
- Strict-mode CAST injection at top-level SELECT projection for remaining type mismatches
- CI pipeline: build extension, run strict-mode differential tests

**Acceptance criteria**:
- TPC-H: 100% pass (strict mode)
- TPC-DS: 99%+ pass (strict mode, matching Java reference baseline)
- `THUNDERDUCK_COMPAT_MODE=strict ./tests/scripts/run-differential-tests.sh tpch` → all green

---

## Dependency Graph

```
Phase 1 (core types + SQL gen)
    └── Phase 2 (DuckDB runtime + Arrow)
            └── Phase 3 (gRPC server + protobuf converter)
                    └── Phase 4 (differential test parity, relaxed)
                            ├── Phase 5 (SparkSQL parser)
                            └── Phase 6 (strict mode)
```

Phases 5 and 6 are independent of each other and can proceed in parallel after Phase 4.
