# Dev Journal — 2026-03-18 — Phase 1 Complete

## Summary

Phase 1 (`thunderduck-core` — pure translation engine, no DuckDB/gRPC/Arrow) is complete,
reviewed, bug-fixed, and committed. The crate compiles clean and all 47 unit tests pass.

---

## What Was Built

The `crates/core` crate implements the entire Spark → DuckDB SQL translation pipeline
as a dependency-free Rust library. Its only external crate is `thiserror`.

| Module | File(s) | Lines | Purpose |
|--------|---------|------:|---------|
| `error` | `error.rs` | 20 | `ThunderduckError` enum + `Result<T>` alias |
| `types` | `types/` | 871 | `DataType`, `StructType`, `TypeMapper`, `TypeInferenceEngine` |
| `expression` | `expression/mod.rs` | 543 | `Expression` enum (21 variants) + `data_type()` + `nullable()` |
| `logical` | `logical/mod.rs` | 517 | `LogicalPlan` enum (23 variants) + `infer_schema()` |
| `functions` | `functions/mod.rs` | 1129 | `FunctionRegistry` — 100+ direct + 60+ custom Spark→DuckDB mappings |
| `generator` | `generator/mod.rs` | 1036 | `SqlGenerator` — exhaustive match on all plan and expression variants |
| **Total** | | **4124** | |

---

## Comparison Against Phase 1 Plan

### What the plan specified vs what was delivered

**`types` module** — delivered in full.
- All 19 `DataType` variants including compound (`Array`, `Map`, `Struct`) and interval types. ✓
- `StructType::field_by_name()` (case-insensitive), `field_index()`, `merge()`. ✓
- `TypeMapper::to_duckdb()` and `from_duckdb()` with alias handling (INT, TEXT, HUGEINT, etc.). ✓
- `TypeInferenceEngine`: column lookup, numeric promotion (full Spark ladder), decimal arithmetic
  rules (`add`, `mul`, `div`), aggregate return types, window return types, 80+ function
  return type patterns. ✓

**`expression` module** — delivered in full, one addition.
- All 21 variants from the plan spec. Added `InListExpression` (IN list, not subquery) as a
  22nd variant — required for the `IN (1, 2, 3)` SQL pattern distinct from `IN (SELECT ...)`. ✓
- `data_type()` and `nullable()` implemented exhaustively across all 22 variants. ✓
- Constructor helpers on `Literal` (`null`, `boolean`, `int`, `long`, `double`, `string`, `decimal`). ✓
- `BinaryOp` extended with `BitwiseAnd`, `BitwiseOr`, `BitwiseXor` beyond the plan spec. ✓
- `UnaryOp` extended with `IsNaN`, `IsNotNaN` beyond the plan spec. ✓

**`logical` module** — 23 variants (plan specified 29; 6 deferred).
- All variants in the plan that are reachable via the DataFrame path are implemented. ✓
- Deferred to Phase 3 when protobuf converter is built: `DropColumns`, `Unpivot`, `Pivot`,
  `FillNa`, `DropNa`, `Replace` — these require runtime session state and are not representable
  as pure SQL generation.
- `infer_schema()` fully implemented including project, aggregate, with-columns, to-dataframe,
  range, join merging, and all passthrough cases. ✓

**`functions` module** — delivered with full compat-mode routing.
- ~100 direct mappings; ~60 custom translators covering all categories in the plan. ✓
- Strict-mode routing implemented for `round` and `avg` (→ `thdck_round`, `thdck_avg`). ✓
- `convert_spark_date_format()`: SimpleDateFormat → strftime pattern conversion. ✓
- `session_macros()` API scaffolded for Phase 2 session startup. ✓

**`generator` module** — delivered in full.
- All 23 `LogicalPlan` variants handled with no `todo!()` or `unimplemented!()`. ✓
- All 22 `Expression` variants handled. ✓
- Identifier quoting (`quote_ident`) — always double-quoted for safety. ✓
- Precedence-aware parenthesisation for binary expressions. ✓
- Window frame generation with correct `ROWS`/`RANGE BETWEEN` syntax. ✓
- `SELECT * REPLACE (...)` for `WithColumns` — DuckDB-idiomatic. ✓
- SEMI/ANTI join with DuckDB syntax (no `LEFT` prefix). ✓

### Gaps vs plan (intentional deferrals)

| Plan item | Status | Reason |
|-----------|--------|--------|
| `LogicalPlan` 29 variants | 23 implemented | 6 require session state (Phase 3+) |
| `FunctionRegistry` 500+ mappings | ~160 implemented | Remaining added as test failures surface in Phase 4 |
| Auto-alias counter (`__td_<n>`) | Not added | All FROM targets are either bare tables or wrapped subqueries; alias counter adds complexity before it's needed |
| `clippy` clean | Not verified | To be gated in CI (Phase 3) |
| `SqlGenerator::generate()` is `&mut self` | Implemented as `&self` | State is stateless at generation time; counter deferred |
| Arrow in `LocalDataRelation` | Schema-only stub | Arrow dependency not added until Phase 2 |

---

## Code Review and Bug Fixes

After Phase 1 implementation, a senior Rust code review identified **7 issues** across two files.
All were fixed with regression tests added for the 3 behavioral bugs.

### Behavioral bugs (red → green)

| Bug | Root cause | Fix |
|-----|-----------|-----|
| `gen_tail` used `rowid` | `rowid` is a physical storage ID, meaningless on subquery results | Replaced with `ROW_NUMBER() OVER ()` wrapped in two subqueries to preserve original order |
| `gen_to_dataframe` emitted `col0`, `col1` | Fallback for unresolvable schema referenced non-existent column names | Removed fallback; passes `SELECT *` when schema is unresolvable — Phase 3 converter will always supply schema |
| `forall` translator interpolated `""` | Copy-paste error left empty string where lambda body should go; produced `NOT ((x))` | Rewrote to `list_bool_and(list_transform(arr, pred))` |

### Dead-code / registry cleanups

| Issue | Fix |
|-------|-----|
| `gen_binary` had identical `if/else` branches | Collapsed to single `format!` — `needs_spaces()` had no effect on output |
| `convert_spark_date_format` `'d'` had dead `else` | Both branches produced `%d`; removed dead else arm |
| `greatest`/`least` in both `math_direct` and `cond_direct` | Removed from `cond_direct`; second insert silently overwrote first |
| `array_compact`/`array_union` in `direct` with wrong mappings | Removed from `direct`; correct implementations live in `custom` and take priority |

**Test count**: 42 (end of Phase 1) → 47 (after bug-fix regression tests).

---

## Key Architecture Decisions Validated

1. **Enum over trait objects** — The `LogicalPlan` and `Expression` enums enforce exhaustive
   match at compile time. Adding the `InListExpression` variant in mid-implementation
   immediately produced a compiler error in `SqlGenerator`, preventing a silent gap.

2. **`to_sql()` invariant holds** — No `Display` or `Debug` impls were used to generate SQL.
   The `inject_distinct` helper was flagged in review as a minor violation and noted for
   refactoring before Phase 3.

3. **`LazyLock` for `FunctionRegistry`** — Zero overhead at call sites; the registry
   is built once on first use. Worked correctly with no locking issues.

4. **`thiserror` for typed errors** — The `Result<T>` alias propagates cleanly through
   all generator methods with `?`. No `unwrap()` in production paths.

---

## Commits

| Hash | Message |
|------|---------|
| `1979b48` | Initial project scaffold: architecture, docs, differential test framework |
| `cdd0095` | feat: Phase 1 complete — thunderduck-core crate |
| `8f72b4f` | fix: address all Phase 1 code review bugs (7 issues, 5 regression tests) |

---

## Phase 2 Preview

Phase 2 adds the DuckDB runtime and Arrow streaming layer. The `crates/core` crate remains
dependency-free; Phase 2 adds a `runtime` module (or `crates/runtime`) that imports `duckdb`
and `arrow` and wires `SqlGenerator` output to `DuckDbSession` execution and Arrow batch
streaming. See `docs/phase2-detailed.md` once planned.
