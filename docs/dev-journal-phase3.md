# Phase 3 Dev Journal — gRPC Connect Server + Protobuf Converter

**Date**: 2026-03-18
**Branch**: main
**Commits**: see `feat: Phase 3 complete` + `refactor: simplify connect-server`

---

## What was built

Phase 3 delivers a working Spark Connect gRPC server. PySpark can now connect, submit
DataFrame plans, and receive Arrow batches over the wire — no JVM required.

### New crate: `crates/connect-server`

```
crates/connect-server/
├── build.rs                    tonic_build compiles all 11 proto files
├── proto/spark/connect/        copied verbatim from .reference/
└── src/
    ├── main.rs                 CLI (--bind, --strict, --relaxed) → tokio server
    ├── error.rs                ConnectError → tonic::Status mapping
    ├── service.rs              SparkConnectService impl
    ├── arrow_ipc.rs            RecordBatch[] → ArrowBatch proto messages
    └── converter/
        ├── plan_converter.rs   stateless entry point
        ├── relation_converter.rs  proto Relation → LogicalPlan (29 variants)
        ├── expression_converter.rs proto Expression → Expression (all types)
        └── type_converter.rs   proto DataType ↔ core DataType (bidirectional)
```

### New `LogicalPlan` variants in `crates/core`

- `DropColumns` — generates `SELECT * EXCLUDE ("col1", "col2") FROM input`
- `ShowString` — stub; delegates to input with `LIMIT num_rows`

### gRPC methods implemented

| Method | Status |
|--------|--------|
| `ExecutePlan` | Full — convert → SQL → execute → Arrow stream |
| `AnalyzePlan` | Schema sub-op only |
| `Config` | Get/GetWithDefault/IsModifiable/Set/Unset all handled |
| `ReleaseSession` | Calls `SessionManager::release` |
| `ReleaseExecute` | Acknowledged (no-op) |
| `AddArtifacts` / `ArtifactStatus` | Empty stub |
| `Interrupt` / `ReattachExecute` / `FetchErrorDetails` / `CloneSession` | `unimplemented` |

---

## Bugs fixed during integration

### 1. DuckDB OOM on recompilation

**Root cause**: Adding `connect-server` to the workspace changed the cargo metadata hash for
`libduckdb-sys`, triggering a full C++ recompile of the 2 GB DuckDB source — which OOM-killed
the compiler.

**Fix**: Hardlinked the already-compiled `libduckdb.a` from the old hash directory to the new
one, and manually wrote the `output` / `root-output` files so cargo treats it as already built.
No source changes needed; this is a one-time workspace bootstrap issue.

### 2. `/UTC` timezone rejected by DuckDB

**Root cause**: `/etc/timezone` in this environment contains `/UTC` (with leading slash). DuckDB
only accepts IANA names without the leading slash.

**Fix**: `session.rs` `detect_timezone()` strips leading `/` from the `/etc/timezone` content.

```rust
let tz = contents.trim().trim_start_matches('/').to_string();
```

### 3. `conf.get()` IndexError in PySpark

**Root cause**: PySpark calls `spark.conf.get("spark.sql.pyspark.jvmStacktrace.enabled")` before
every DataFrame operation. Our `Config` handler returned empty `pairs`, causing PySpark to
crash on `pairs[0][1]`.

**Fix**: `Get` operations now return an empty-string value for every requested key. `GetWithDefault`
returns the caller's default. `IsModifiable` returns `"true"` for all keys.

### 4. Binary operators sent as `UnresolvedFunction`

**Root cause**: PySpark encodes `col("id") > 5` as `UnresolvedFunction(">", [id, 5])`, not as a
proto `Binary` expression. Our converter passed it to `FunctionRegistry`, producing `>("id", 5)`.

**Fix**: `convert_unresolved_function` now intercepts all arithmetic and comparison operator names
(`>`, `>=`, `<`, `<=`, `=`, `==`, `!=`, `<>`, `and`, `&&`, `or`, `||`, `+`, `-`, `*`, `/`, `%`)
and emits `BinaryExpression` / `UnaryExpression` instead of `FunctionCall`.

---

## Smoke test result

```python
spark = SparkSession.builder.remote("sc://localhost:15002").getOrCreate()
assert spark.range(10).count() == 10          # PASS
assert spark.range(10).filter(col("id") > 5).count() == 4  # PASS
spark.range(5).toDF("x").select("x").show()  # PASS
```

Server starts in ~50ms, memory baseline ~30MB. Core tests: 71/71 still passing.

---

## Code quality pass (post-phase refactor)

After the feature landed, a senior review pass identified and fixed 8 issues:

| File | Issue | Fix |
|------|-------|-----|
| `service.rs` | Arrow-batch→response loop duplicated in `execute_plan` and `SqlCommand` | Extracted `batches_to_responses()` helper |
| `service.rs` | `SqlGenerator::new(CompatMode::Relaxed)` vs `SqlGenerator::relaxed()` inconsistency | Use `relaxed()` everywhere |
| `service.rs` | `empty_result_response` `response_suffix` param always `"0"` | Dropped parameter |
| `service.rs` | `use` imports inside function bodies | Moved to file top |
| `type_converter.rs` | `Struct` arm duplicated `proto_struct_to_struct_type` body | `Struct` arm now calls the function; `#[allow(dead_code)]` removed |
| `type_converter.rs` | `if let Some(x) = field { f(x)? } else { Default }` repeated 5× | `.map(f).transpose()?.unwrap_or(Default)` |
| `relation_converter.rs` | `find_grouping_index` wraps `Iterator::position` with no benefit | Inlined |
| `relation_converter.rs` | Rollup/Cube arms had identical `let sets = ...` bodies | Factored into `singleton_sets` above the match |
| `expression_converter.rs` | `thunderduck_core::expression::NullOrdering` full-qualified inline | `NullOrdering as CoreNullOrdering` at import |
| `expression_converter.rs` | `if let Some(fs) = frame { Some(f(fs)?) } else { None }` | `.map(f).transpose()?` |

---

## Phase 4 deferred items

Items encountered but intentionally left for Phase 4:

- `NAFill` / `NADrop` / `NAReplace` relations — require schema inference
- `Describe` / `Summary` / `Cov` / `Corr` — statistical operations
- `Unpivot` — complex reshape
- `UpdateFields` expression — struct `withField` / `dropFields`
- Complex literals (`Array`, `Map`, `Struct`) — fall back to `null` currently
- `WriteOperation` command — local Parquet write
- `CTE` / `WITH` support
- `SchemaInferrer` via DuckDB `DESCRIBE` fallback
- Full differential test suite (41 test files in `tests/integration/differential/`)
