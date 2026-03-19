# Phase 4 Dev Journal — Differential Tests + NA Operations + Unpivot + WriteOperation

**Date**: 2026-03-18
**Branch**: main
**Status**: In Progress

---

## What was built

Phase 4 implements the features deferred from Phase 3, adds schema inference, and wires up
the differential test infrastructure.

### New features

#### `SchemaInferrer` — `crates/core/src/runtime/schema_inferrer.rs`

Issues a `SELECT * FROM ({sql}) LIMIT 0` probe to DuckDB to infer column names and types
without reading any data. Returns a `StructType`. Used by NA operations when `cols` is empty
(i.e., "all columns"). Also converts Arrow datatypes back to core `DataType`.

#### New `LogicalPlan` variants — `crates/core/src/logical/mod.rs`

| Variant | SQL generated | Notes |
|---------|---------------|-------|
| `NADrop` | `SELECT * FROM input WHERE col1 IS NOT NULL AND col2 IS NOT NULL` | Any/All/threshold modes |
| `NAFill` | `SELECT COALESCE(col1, val1) AS col1, ... FROM input` | Per-column fill values |
| `NAReplace` | `SELECT CASE WHEN col = old THEN new ELSE col END AS col, ... FROM input` | Per-column CASE WHEN |
| `Unpivot` | `UNPIVOT (input) ON col1, col2 INTO NAME var VALUE val` | DuckDB native UNPIVOT |

#### `WriteOperation` — `crates/connect-server/src/service.rs`

`df.write.parquet(path)` / `.csv(path)` / `.json(path)` now handled in `handle_command`:
```sql
COPY (SELECT ...) TO 'path' (FORMAT PARQUET)
COPY (SELECT ...) TO 'path' (FORMAT CSV, HEADER)
```

#### Session-aware `RelationConverter`

`RelationConverter::with_session(expr_conv, session)` threads an `Arc<DuckDbSession>` through
the converter for schema inference. Uses `tokio::task::block_in_place` to run async schema
inference synchronously within the sync converter.

`PlanConverter::convert_relation_with_session(relation, session)` is the new entry point used
by `execute_plan` in `service.rs`.

#### `.cargo/config.toml`

```toml
[build]
jobs = 2
```

Limits parallel C++ compilation to prevent OOM when building DuckDB from source. DuckDB's
amalgamation is ~2GB; compiling 4+ units in parallel exhausts available RAM on this machine.

---

## Test status

- Core unit tests: 71/71 passing
- Release binary builds successfully
- Differential tests: Spark installation pending (downloading 4.1.1)

---

## Phase 4 remaining work

- Run baseline differential test suite (41 test files) once Spark is installed
- Fix failures found in differential tests
- Complex literals (Array/Map/Struct) in expression converter
- Describe/Summary/Cov/Corr statistical operations (Phase 5 candidates)
