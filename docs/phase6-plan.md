# Phase 6 Plan — Closing Pre-Existing Test Failures

> **Status**: Wave 1 COMPLETE (2026-03-31). Result: 825 passing / 5 failing / 6 skipped.
> Wave 2 plan in progress. See `docs/reference-gap-analysis.md` Section 7 for remaining failures.

**Date**: 2026-03-27
**Starting state**: 719 passing / 111 failing / 6 skipped (836 total)
**Target**: 830+ passing / ≤6 failing (skipped-only)

This plan addresses the 111 pre-existing failures catalogued in `reference-gap-analysis.md`
Section 6. Items are ordered by impact (failures closed per unit of effort), with the largest
easy wins first.

---

## Phase 6A — DDL via SQL path (~40 failures, highest count)

**Failures**: `test_dataframe_basic_operations_differential.py` (7),
`test_ddl_corrected.py` (2), `test_ddl_operations_differential.py` (13),
`test_ddl_parser_differential.py` (14), `test_sql_expressions_differential.py` (10)

**Root cause**: `DROP TABLE`, `CREATE TABLE`, `INSERT INTO`, `TRUNCATE TABLE`, `ALTER TABLE`
statements issued through `spark.sql()` are not handled by `SqlConverter` in
`crates/core/src/parser/sql_converter.rs`. The parser returns
`Unsupported: SQL statement type not yet supported: DROP`.

**Implementation**:

### 6A-1: `DROP TABLE [IF EXISTS]`

In `SqlConverter::convert_statement`, match on `sqlparser::ast::Statement::Drop`:
```rust
Statement::Drop { object_type: ObjectType::Table, if_exists, names, .. } => {
    // Emit: DROP TABLE [IF EXISTS] "name"
    Ok(LogicalPlan::SqlRelation(SqlRelation {
        sql: format!("DROP TABLE{} {}", ..., names[0]),
        schema: StructType::empty(),
    }))
}
```
Pass through as a `SqlRelation` with raw DuckDB-compatible SQL (DuckDB supports `DROP TABLE IF
EXISTS`). Schema is empty (DDL statements return no rows).

### 6A-2: `CREATE TABLE [AS SELECT]`

Match `Statement::CreateTable`. Two sub-cases:
- `CREATE TABLE name (col type, ...)` — pass through to DuckDB directly
- `CREATE TABLE name AS SELECT ...` (CTAS) — convert the inner SELECT to a `LogicalPlan`, wrap
  as `SqlRelation("CREATE TABLE name AS (<generated_sql>)")`

### 6A-3: `INSERT INTO`

Match `Statement::Insert`. Convert the `source` (a SELECT or VALUES) to SQL, emit
`INSERT INTO "table" <source_sql>`.

### 6A-4: `TRUNCATE TABLE`

Match `Statement::Truncate`. Emit `DELETE FROM "table"` (DuckDB equivalent; `TRUNCATE` also
works in DuckDB but is less common).

### 6A-5: `ALTER TABLE ... RENAME COLUMN`

Match `Statement::AlterTable`. Handle the `RenameColumn` action — emit
`ALTER TABLE "t" RENAME COLUMN "old" TO "new"`.

**Estimated test closures**: ~40
**Effort**: Medium — mechanical pattern matching in `sql_converter.rs`, no new logical plan
types needed. DDL statements map cleanly to `SqlRelation` passthrough.

---

## Phase 6B — Lambda / HOF in SparkSQL path (22 failures)

**Failures**: `test_lambda_differential.py` (22)

**Root cause**: `TRANSFORM(arr, x -> expr)`, `FILTER(arr, x -> cond)`, `EXISTS(arr, x -> cond)`,
`FORALL(arr, x -> cond)`, `AGGREGATE(arr, zero, (acc, x) -> expr)` are parsed by sqlparser-rs
but not yet converted to `Expression::Lambda` in `SqlConverter::convert_expr`.

**Implementation**:

### 6B-1: Identify sqlparser-rs AST nodes

sqlparser-rs represents HOF calls as regular `Expr::Function` with lambda arguments. Lambda
arguments appear as `Expr::Lambda { params, body }` in the argument list.

### 6B-2: Detect lambda in `convert_function_call`

In `SqlConverter::convert_expr` for `Expr::Function`, after converting argument expressions,
detect `Expr::Lambda` arguments and convert them to `Expression::Lambda`:

```rust
Expr::Lambda { params, body } => Expression::Lambda(LambdaExpression {
    params: params.iter().map(|p| p.to_string()).collect(),
    body: Box::new(self.convert_expr(body)?),
}),
```

The HOF name (`transform`, `filter`, etc.) is already in `FunctionRegistry` for the DataFrame
path. The SparkSQL path should produce the same `Expression::FunctionCall` + `Expression::Lambda`
structure and thus follow the same SQL generation path.

### 6B-3: Two-argument lambdas (AGGREGATE)

`AGGREGATE(arr, zero, (acc, x) -> combine, x -> finish)` has a two-param lambda. Handle
multi-param `LambdaExpression` (already supported in the expression enum).

**Estimated test closures**: 22
**Effort**: Medium — `SqlConverter` change only, no new logical plan types or generator work.

---

## Phase 6C — Complex type constructors and accessors (13 failures)

**Failures**: `test_complex_types_differential.py` (11), `test_type_literals_differential.py` (2)

**Root cause**: SparkSQL path doesn't convert:
- `NAMED_STRUCT('x', 1, 'y', 2)` → `Expression::StructLiteral`
- `arr[0]` / `map['k']` → `Expression::ExtractValue`
- `struct_col.field` → `Expression::ExtractValue`
- `with_field` / `drop_fields` — UpdateFieldsExpression

### 6C-1: `NAMED_STRUCT` / `STRUCT()`

`Expr::Function { name: "named_struct" | "struct", args }` → `Expression::StructLiteral`.
Already handled on the DataFrame path in `ExpressionConverter`. Mirror in `SqlConverter`.

### 6C-2: Array/map subscript `arr[0]` / `map['k']`

`Expr::Subscript { expr, subscript }` → `Expression::ExtractValue(ExtractValueExpression {
    child: convert(expr), extraction: convert(subscript) })`.
Generator already handles `ExtractValue` → `child[key]` DuckDB syntax.

### 6C-3: Struct field access `struct_col.field`

`Expr::CompoundFieldAccess { root, access_chain }` → `Expression::ExtractValue`.
DuckDB uses `struct_col.field` syntax which is already emitted by `gen_extract_value`.

### 6C-4: `with_field` / `drop_fields`

These are Spark DataFrame functions that translate to DuckDB `struct_pack` and struct
reconstruction. Map via `FunctionRegistry` or special-case in `gen_function_call`.

**Estimated test closures**: 13
**Effort**: Low-Medium — mostly mirroring what `ExpressionConverter` already does.

---

## Phase 6D — Array/explode functions in SparkSQL path (6 failures)

**Failures**: `test_array_functions_differential.py` (6), `test_dataframe_functions.py` (1)

**Root cause**: `SPLIT`, `COLLECT_LIST`, `COLLECT_SET`, `EXPLODE`, `SIZE`, `POSEXPLODE` are
not handled in `SqlConverter`. They work on the DataFrame path via `FunctionRegistry`.

### 6D-1: Aggregate functions (`COLLECT_LIST`, `COLLECT_SET`)

These appear inside aggregates. `SqlConverter::convert_aggregate_function` should map:
- `collect_list` → `LIST` (DuckDB aggregate)
- `collect_set` → `LIST(DISTINCT ...)` or `ARRAY_AGG(DISTINCT ...)`

### 6D-2: Scalar functions (`SPLIT`, `SIZE`)

Already in `FunctionRegistry`. The SparkSQL path needs `convert_function_call` to route these
through `FunctionRegistry` instead of treating them as unknown. Check for fallthrough case.

### 6D-3: `EXPLODE` / `POSEXPLODE` in FROM clause

These are table-generating functions. In the SparkSQL path they appear as lateral joins:
`SELECT * FROM t, LATERAL EXPLODE(arr) AS e(col)`. sqlparser-rs represents this as
`TableFactor::Function`. Convert to a DuckDB `UNNEST(arr)` lateral join.

**Estimated test closures**: 7
**Effort**: Low — FunctionRegistry already has most mappings; main work is wiring the SQL path.

---

## Phase 6E — JSON functions (5 failures)

**Failures**: `test_json_functions_differential.py` (5)

**Root cause**: `FROM_JSON(col, schema_str)` and `JSON_TUPLE(json_col, 'k1', 'k2')` not
implemented.

### 6E-1: `FROM_JSON`

Spark's `from_json(col, 'struct<a:int,b:string>')` → DuckDB's `json_extract` or a series of
`json_extract_string` calls. The schema string needs parsing to determine output column types.
This is complex; consider returning a `RawSql` passthrough using DuckDB's `json_transform` for
the common case.

### 6E-2: `JSON_TUPLE`

`json_tuple(json_col, 'k1', 'k2')` → `json_extract_string(json_col, '$.k1') AS k1, ...`.
Table-generating function — similar to EXPLODE, needs lateral expansion handling.

**Estimated test closures**: 5
**Effort**: Medium-High — schema parsing for `FROM_JSON` is non-trivial.

---

## Phase 6F — TPC-DS DataFrame Q17, Q25, Q29 join alias scoping (3 failures)

**Failures**: `test_tpcds_dataframe_differential.py` Q17, Q25, Q29

**Root cause**: `DuckDB error: Referenced table "d1" not found!` — a date-dim table alias
declared in an inner subquery is not visible at the outer SELECT level.

These queries use a pattern where the same `date_dim` table appears multiple times with aliases
`d1`, `d2`, `d3`. The flat join chain optimizer likely breaks the alias chain at a point where
`d1` from one join leg is wrapped in a subquery, making it inaccessible.

**Investigation needed**: Enable `TD_DEBUG_SQL=1` and inspect the generated SQL for Q17 to
locate where `d1` loses scope. Likely fix: either (a) improve `extract_filters` to handle this
multi-alias pattern, or (b) improve the natural-flat-join branch inside `gen_join()` to not break at the date-dim join.

**Estimated test closures**: 3
**Effort**: Medium — requires root cause investigation; fix may be localized to `generator/mod.rs`.

---

## Phase 6G — String function gaps (4 failures)

**Failures**: `test_string_collection_differential.py` (4)

| Test | Fix required |
|------|-------------|
| `test_overlay` | Add `OVERLAY(str PLACING x FROM n FOR m)` — map to DuckDB `OVERLAY(str PLACING x FROM n FOR m)` (same syntax) |
| `test_octet_length` | `OCTET_LENGTH` on VARCHAR — add to FunctionRegistry or emit `OCTET_LENGTH(CAST(col AS BLOB))` |
| `test_format_number` | Implement thousand-separator formatting — `printf('%,.2f', n)` or custom UDF |
| `test_to_char` | Date formatting — `TO_CHAR(ts, 'YYYY-MM-DD')` for date-only format |

**Estimated test closures**: 4
**Effort**: Low — FunctionRegistry additions and small generator tweaks.

---

## Phase 6H — Miscellaneous (2 failures)

| Test | Fix | File |
|------|-----|------|
| `test_select_from_values` | Add `VALUES (...)` clause handling in `SqlConverter` — map to `SELECT unnest([...])` or DuckDB `VALUES` | `sql_converter.rs` |
| `test_bit_get` | Add `bit_get` → `get_bit` mapping to FunctionRegistry | `functions/mod.rs` |

**Estimated test closures**: 2
**Effort**: Trivial for `bit_get`; Low for `VALUES`.

---

## Execution order

| Phase | Failures closed | Effort | Recommended order |
|-------|----------------|--------|-------------------|
| 6H (bit_get, VALUES) | 2 | Trivial | Start here — 5 min warm-up |
| 6A (DDL) | ~40 | Medium | Highest impact |
| 6B (Lambda/HOF) | 22 | Medium | Second highest |
| 6C (Complex types) | 13 | Low-Medium | Third |
| 6D (Array/explode) | 7 | Low | Fourth |
| 6F (TPC-DS join scoping) | 3 | Medium | After 6D — needs investigation |
| 6G (String functions) | 4 | Low | Parallel with 6F |
| 6E (JSON) | 5 | Medium-High | Last — most complex |

**Total**: 111 failures → estimated 100+ closures → **target: 825+ passing**

---

## Non-goals for Phase 6

- No new logical plan types
- No changes to the Arrow streaming path or schema inference
- No strict-mode extension changes
- `tryGenerateFlatSemiAntiJoin` and `generateFlatJoinChainWithMapping` remain deferred (Low,
  no test failures from these)
