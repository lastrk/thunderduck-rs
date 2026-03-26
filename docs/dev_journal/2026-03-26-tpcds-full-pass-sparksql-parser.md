# Dev Journal — 2026-03-26 — TPC-DS Full Pass (126/126) + SparkSQL Parser

**Date**: 2026-03-26
**Branch**: main
**Commit**: `cb9e81f`
**Status**: 126 TPC-DS + 670 TPC-H/other differential tests passing, 0 failing

---

## Summary

Two large deliverables landed in this session:

1. **SparkSQL parser** (`crates/core/src/parser/`) — a proper `sqlparser-rs`-based parse-and-convert
   pipeline replacing the text-preprocessing path for `spark.sql()`, as prescribed by ADR-21.
2. **TPC-DS differential test suite** — 126 tests, all passing after fixing four correctness bugs
   introduced by the new parser path.

---

## 1. SparkSQL Parser — `crates/core/src/parser/`

### Architecture

Implements the clean three-stage pipeline from ADR-21:

```
spark.sql() string
    ↓  SparkSqlParser::parse()         ← sqlparser-rs + SparkDialect
sqlparser AST (Statement)
    ↓  SqlConverter::convert()         ← mirrors RelationConverter
Thunderduck LogicalPlan + Expression
    ↓  SqlGenerator::generate()        ← unchanged
DuckDB SQL string
```

### Files added

| File | Contents |
|------|----------|
| `crates/core/src/parser/mod.rs` | `SparkSqlParser` — entry point, calls sqlparser-rs and dispatches to `SqlConverter` |
| `crates/core/src/parser/dialect.rs` | `SparkDialect` — custom sqlparser-rs `Dialect` impl; enables backtick quoting, lambda syntax, `STRUCT`/`ARRAY`/`MAP` literals |
| `crates/core/src/parser/sql_converter.rs` | `SqlConverter` — 1006-line visitor converting `sqlparser::ast::Statement` to `LogicalPlan`; handles SELECT (projections, WHERE, GROUP BY, HAVING, ORDER BY, LIMIT/OFFSET), CTEs, JOINs, set operations (UNION/INTERSECT/EXCEPT), subqueries, inline expressions |

### Integration point

`RelationConverter::convert_sql()` (in `crates/connect-server/src/converter/relation_converter.rs`)
now routes all `spark.sql()` strings through `SparkSqlParser::parse()` instead of the old
`preprocess_spark_sql()` → `SqlRelation` verbatim path. Unrecognised constructs return
`Status::unimplemented` to force explicit coverage decisions.

---

## 2. TPC-DS Differential Test Suite

Added `tests/integration/tpcds_dataframe/tpcds_dataframe_queries.py` covering all 126 TPC-DS
queries (Q1–Q99, with variants Q14a/b, Q23a/b, Q39a/b). Tests run against a DuckDB-native TPC-DS
dataset generated at SF=0.01.

Four correctness bugs were discovered and fixed to reach 126/126:

---

## 3. Bug fixes

### 3a. `count(1)` aggregate alias — Q38, Q87, Q96

**Problem**: `COUNT(*)` in DuckDB auto-generates the column name `count_star()`. Spark uses
`count(1)`. When multiple `SELECT COUNT(*)` queries were joined or aliased, the result schema had
the DuckDB name, causing a `KeyError: 'count(1)'` in test comparisons.

**Root cause**: `render_agg_expr` in `generator/mod.rs` only emitted an explicit `AS` alias when
the aggregate expression was already wrapped in an `Expression::Alias`. Unaliased aggregates were
left bare, relying on DuckDB to name them.

**Fix**: `render_agg_expr` now unconditionally appends an explicit Spark-convention alias for
every unaliased aggregate:

```rust
if !matches!(&ae.func, Expression::Alias(_)) {
    let spark_name = spark_column_name(&ae.func);
    let escaped = spark_name.replace('"', "\"\"");
    s = format!("{s} AS \"{escaped}\"");
}
```

**File**: `crates/core/src/generator/mod.rs` — `render_agg_expr()`

---

### 3b. DECIMAL spacing in auto-generated column names — Q61

**Problem**: DuckDB normalises `DECIMAL(15,4)` to `DECIMAL(15, 4)` (with a space after the comma)
in auto-generated output column names. Spark uses the no-space format. Queries that used an
unaliased CAST to DECIMAL as a top-level projection had a column name mismatch.

**Fix — two-part**:

1. Extended `spark_column_name()` in `logical/mod.rs` to recursively produce Spark-convention
   names for `Cast`, `Binary`, `Unary`, and `Literal` expressions (previously only handled
   `FunctionCall` and `ColumnReference`).

2. Added `spark_type_name()` helper that formats `DECIMAL(p,s)` without spaces:
   ```rust
   DataType::Decimal { precision, scale } => format!("DECIMAL({precision},{scale})")
   ```

3. Added `expr_contains_decimal_cast()` predicate in `generator/mod.rs`. In
   `gen_projection_list`, any non-aliased expression containing a DECIMAL cast now gets an
   explicit `AS "CAST(... AS DECIMAL(p,s))"` alias to prevent DuckDB's spaced normalisation.

**Files**: `crates/core/src/logical/mod.rs`, `crates/core/src/generator/mod.rs`

---

### 3c. Duplicate column `_1` suffix — Q14b, Q39a, Q39b, Q64

**Problem**: DuckDB appends `_1` to duplicate output column names (e.g., two columns both named
`i_item_sk` → `i_item_sk`, `i_item_sk_1`). Spark allows true duplicate column names. The `_1`
suffix was appearing in two places:

- **`execute_plan` results**: Arrow `RecordBatch` column names came from DuckDB's schema.
- **`analyze_plan` schema**: PySpark calls `analyze_plan` to retrieve `.schema` before executing;
  the static `infer_schema()` result had correct names but the DuckDB fallback path was used for
  CTE-heavy queries (where `TableScan.infer_schema()` returns `StructType::empty()`).

**Fix — two-part**:

1. **`rename_to_spark_schema()`** (new function in `service.rs`): Post-processes Arrow
   `RecordBatch` vectors after DuckDB execution. Compares the static `plan.infer_schema()` field
   names against the actual DuckDB output column names; if they differ, reconstructs Arrow schemas
   with the Spark-expected names (keeping DuckDB's inferred types):

   ```rust
   fn rename_to_spark_schema(
       plan: &LogicalPlan,
       batches: Vec<RecordBatch>,
   ) -> Vec<RecordBatch>
   ```

   Called in the `execute_plan` path immediately after DuckDB returns results.

2. **`analyze_plan` schema merging**: When `has_unresolved=true` (CTE references cause
   `TableScan` to return empty schema), the static `infer_schema()` column names are preserved
   but the DuckDB-inferred types are used:

   ```rust
   struct_type = if !has_unresolved || struct_type.fields.len() != duckdb_schema.fields.len() {
       duckdb_schema
   } else {
       // merge: Spark names + DuckDB types
       ...
   };
   ```

**File**: `crates/connect-server/src/service.rs`

---

## 4. `spark_column_name` extensions

`spark_column_name()` in `logical/mod.rs` now handles:

| Expression type | Example output |
|-----------------|---------------|
| `Cast` | `CAST(revenue AS DECIMAL(15,4))` |
| `Binary` | `(revenue * rate)` |
| `Unary` | `(NOT active)`, `(-amount)`, `(x IS NULL)` |
| `Literal` | `1`, `'hello'`, `true` |

Previously only `FunctionCall`, `ColumnReference`, `UnresolvedColumn`, `Alias`, `Window`, and
`CaseWhen` were handled.

---

## Test status

| Suite | Before | After |
|-------|--------|-------|
| TPC-DS differential | 118/126 | **126/126** |
| TPC-H + other differential | 670/670 | **670/670** (unchanged) |
| Core unit tests | 76 | **76** (unchanged) |

**Total differential**: 796 passing, 0 failing.
