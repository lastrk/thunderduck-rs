# Thunderduck Rust — Implementation Plan

## Guiding Principles

- **DataFrame path first**: The protobuf → LogicalPlan → SQL path is the primary value driver. The SparkSQL (raw SQL string) path is secondary and added in Phase 2.
- **Differential tests are the acceptance criteria**: Each phase ends when a measurable subset of the 746 differential tests passes.
- **Core before server**: The `core` crate (pure translation logic) is fully unit-testable without any gRPC or DuckDB dependencies. Build and test it independently before wiring up the server.
- **No premature abstractions**: Implement what's needed for the current phase. Port the function registry exhaustively, not incrementally.

---

## Phase Overview

| Phase | Deliverable | Status | Acceptance Criteria |
|-------|-------------|--------|---------------------|
| **1** | Core types + SQL generation | **COMPLETE** (2026-03-18) | All unit tests pass; SQL generator produces correct DuckDB SQL for all plan node types |
| **2** | DuckDB runtime + Arrow streaming | **COMPLETE** (2026-03-18) | DuckDB queries execute; Arrow batches stream correctly; relaxed-mode integration test server starts |
| **3** | gRPC server + protobuf converter | **COMPLETE** (2026-03-18) | PySpark client connects; TPC-H DataFrame path tests pass |
| **4** | Differential test parity (relaxed) | **COMPLETE** (2026-03-21) | 670/670 differential tests pass in relaxed mode |
| **5** | SparkSQL parser | **Partial** — preprocessing pass in place; full parser not yet built | Raw `spark.sql()` path works for common cases; SQL-based differential tests pass |
| **6** | Strict mode + extension integration | Not started | `thdck_spark_funcs` extension loaded; strict mode TPC-H/TPC-DS 100% pass |

---

## Phase 1 — Core Types + SQL Generation

**Goal**: A fully unit-tested `core` crate that can translate any `LogicalPlan` tree into correct DuckDB SQL. No DuckDB, no gRPC, no network — pure Rust. See [`dev_journal/2026-03-18-phase1-complete.md`](dev_journal/2026-03-18-phase1-complete.md) for the full account.

**Deliverables**:
- `DataType` enum + `StructType` + `StructField`
- `Expression` enum (21+ variants) with `to_sql()`, `data_type()`, `nullable()`
- `LogicalPlan` enum (36 variants as of 2026-03-21)
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
- `RelationConverter`: all Spark Connect relation types → `LogicalPlan` variants
- `ExpressionConverter`: all Spark Connect expression types → `Expression` variants
- `SparkConnectService`: `execute_plan()` and `analyze_plan()` tonic handlers
- Session ID extraction and routing to `SessionManager`
- Arrow IPC serialisation of `RecordBatch` into `ExecutePlanResponse` stream
- `ReleaseSession` / `ReleaseExecute` gRPC method stubs
- Smoke test: `pyspark.sql.SparkSession.builder.remote("sc://localhost:15002").getOrCreate()` succeeds

**Acceptance criteria**: `tests/integration/differential/test_simple_sql.py` and basic DataFrame operations pass.

---

## Phase 4 — Differential Test Parity (Relaxed Mode)

**Goal**: Pass the full differential test suite in relaxed mode.

**Status**: **COMPLETE** as of 2026-03-21. 670/670 differential tests pass.

**Delivered**:
- `NADrop`, `NAFill`, `NAReplace`, `Unpivot`, `Pivot` plan nodes + SQL generation
- `StatCov`, `StatCorr`, `ApproxQuantile`, `StatCrosstab`, `StatFreqItems`, `StatSampleBy` plan nodes
- `Describe`, `Summary` plan nodes
- `UpdateFields` expression (struct `withField` / `dropFields`)
- Complex literals: `Array`, `Map`, `Struct`, `SpecializedArray` fully converted
- `SchemaInferrer` via `LIMIT 0` probe; `ExecDdl` + `SchemaOf` session commands
- `WriteOperation` command: Parquet / CSV / JSON via `COPY ... TO`
- Join plan_id qualification, window frame boundaries, `unionByName`, `parse_type_str`
- 20+ session macros bridging Spark→DuckDB function name gaps
- Schema inference fixes: join outer nullability, USING column ordering, AliasedRelation aliases, Union widening, ROLLUP/CUBE nullability, `spark_column_name`
- Generator fixes: `extract_filters` for filter stacking, SEMI/ANTI qualifier stripping, USING join column reordering, `withColumn` column ordering
- GROUPING/GROUPING_ID type CASTs, DECIMAL SUM/AVG precision, Union widening CASTs, ROLLUP/CUBE NULLS FIRST

---

## Phase 5 — SparkSQL Parser

**Goal**: Raw SQL strings passed via `spark.sql("SELECT ...")` are parsed and executed correctly.

**Current status**: Partial. The `spark.sql()` path works for the large majority of queries via
a Spark→DuckDB SQL preprocessing pass (`preprocess_spark_sql` in `generator/mod.rs`) that handles
dialect differences without a full parse. All 670 differential tests pass using this approach.
A full `sqlparser-rs`-based parser has not been built.

**Preprocessing pass covers**:
- Backtick → double-quote identifier rewriting (2026-03-25)
- `ARRAY(...)` → `LIST_VALUE(...)`, `NAMED_STRUCT(...)` → struct literal, `MAP(...)` rewrite
- 1:1 function name renames (`SIZE` → `LEN`, `TRANSFORM` → `LIST_TRANSFORM`, etc.)
- Higher-order function rewrites (`exists`, `forall`, `aggregate`, `filter`, `zip_with`)
- `json_tuple`, `from_json`, `overlay`, `percentile`, `split` with limit
- Spark angle-bracket type syntax (`ARRAY<TYPE>` → `TYPE[]`)
- Date + interval arithmetic, HOF rewrites

**Remaining for a full parser** (deferred until a differential test gap requires it):
- `LATERAL VIEW EXPLODE/POSEXPLODE`
- `DISTRIBUTE BY`, `CLUSTER BY`, `TRANSFORM` pipeline syntax
- `TABLESAMPLE` clause
- Spark `INTERVAL` literal syntax variants
- Full schema-aware column resolution

**Acceptance criteria** (when full parser is built):
- All SQL-path differential tests pass
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
                    └── Phase 4 (differential test parity, relaxed) ✓
                            ├── Phase 5 (SparkSQL parser — partial)
                            └── Phase 6 (strict mode — not started)
```

Phases 5 and 6 are independent of each other and can proceed in parallel after Phase 4.
