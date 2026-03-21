# Dev Journal — 2026-03-21 — Schema Inference Fixes + Generator Correctness

**Date**: 2026-03-21
**Branch**: main
**Status**: Phase 4/5 gap-closure — differential tests: 665 passing, 4 failing (pre-existing)

---

## Summary

This session closed two independent categories of gaps discovered by a method-by-method comparison
of the Java reference against the Rust port: (1) eight correctness gaps in `LogicalPlan::infer_schema()`
and (2) three generator correctness bugs that caused SQL referencing subquery-aliased columns to fail
at runtime. Additionally, `Describe` and `Summary` plan variants were added to complete the remaining
Phase 4 statistical operations.

All 75 unit tests pass. Differential test pass rate unchanged at 665/4.

---

## 1. Schema inference fixes — `crates/core/src/logical/mod.rs`

`LogicalPlan::infer_schema()` is the static type-inference path used by `AnalyzePlan` and the
`service.rs` schema-of-plan logic. The Rust port had eight gaps vs the Java reference, all now fixed.

### 1a. Join outer nullability

**Problem**: `LEFT JOIN` right-side columns and `RIGHT JOIN` left-side columns were returned with
the child schema's nullability unchanged. Spark makes outer-side columns nullable.

**Fix**: After computing `StructType::merge`, enumerate fields; mark `i < left_len` fields nullable
for `RIGHT`/`FULL` joins and `i >= left_len` fields nullable for `LEFT`/`FULL` joins. `left_len`
is captured before the merge.

### 1b. USING join column ordering

**Problem**: The filter-based dedup (`filter(|f| seen_using.insert(f.name.clone()))`) kept USING
key columns at their left-table position. Spark always puts USING key columns first.

**Fix**: Three-pass rebuild: (1) USING keys in USING order from left schema, (2) left non-USING
fields, (3) right non-USING fields. Reuses `left_schema`/`right_schema` locals from fix 1a.

### 1c. `AliasedRelation` ignores `column_aliases`

**Problem**: `LogicalPlan::AliasedRelation(a) => a.input.infer_schema()` — the `column_aliases`
field was silently dropped.

**Fix**: If `column_aliases` is non-empty and its length matches the child field count, zip-rename
each field to the corresponding alias.

### 1d. Union type widening

**Problem**: `u.left.infer_schema()` — right schema was ignored; no per-column type promotion.
Mixed `INT`/`BIGINT` unions would report the wrong column type.

**Fix**: Zip left and right fields; call `TypeInferenceEngine::promote_numeric` per column pair.
Guard: if `promote_numeric` returns `Double` for a non-numeric pair (the function's fallback),
keep the left type instead. Also OR-reduce nullability per column.

### 1e. ROLLUP/CUBE grouping nullability

**Problem**: `ROLLUP`/`CUBE` produce `NULL` in grouping columns for subtotal rows. The grouping
fields were returned with child-schema nullability (usually non-nullable).

**Fix**: After building the aggregate field list, if `a.grouping_sets.is_some()`, force
`nullable = true` on the first `a.grouping.len()` fields.

### 1f. Unaliased expression naming (`spark_column_name`)

**Problem**: The `other` arm of `projection_to_field` returned `StructField::nullable("expr", dt)`.
Spark generates convention-based names: `count(*)` → `count(1)`, `f(a, b)` → `"f(a, b)"`.

**Fix**: New free function `spark_column_name(expr: &Expression) -> String`:
- `FunctionCall` with any `Star` arg named `"count"` → `"count(1)"`
- Other `FunctionCall` → `"{name}({args comma-joined})"`
- `UnresolvedColumn` / `ColumnReference` → column name
- `Alias` → recurse into inner expr
- fallback → `"expr"`

Used in both `projection_to_field` (`other` arm) and `agg_expr_to_field` (`FunctionCall` arm),
replacing the raw `f.name` which was always `"count"` regardless of arguments.

### 1g. `ToDataFrame` extra-name handling

**Problem**: `zip` silently drops extra entries in `t.column_names` beyond the child field count.

**Fix**: After the zip-rename, append remaining column names as `StructField::nullable(name, String)`.

---

## 2. `Describe` and `Summary` plan variants

`df.describe()` and `df.summary()` were previously unsupported (returned `Unsupported` error from
the converter). They are now fully implemented.

### `crates/core/src/logical/mod.rs`

Two new plan structs:
- `Describe { input: Box<LogicalPlan>, cols: Vec<String> }` — the 5 fixed Spark statistics
  (`count`, `mean`, `stddev`, `min`, `max`) as `VARCHAR` columns.
- `Summary { input: Box<LogicalPlan>, statistics: Vec<String>, cols: Vec<String> }` — configurable
  statistics, same output shape.

### `crates/core/src/generator/mod.rs`

`gen_describe(d)` and `gen_summary(s)` emit DuckDB `SUMMARIZE` + a final `PIVOT`-style union query
that matches Spark's row-oriented output format (`summary`, `col1`, `col2`, ... columns) with
statistics as row values.

Free function `stat_to_agg_expr(stat, col)` maps Spark statistic names to DuckDB aggregate
expressions:

| Spark stat | DuckDB expression |
|---|---|
| `count` | `CAST(COUNT(col) AS VARCHAR)` |
| `mean` | `CAST(AVG(col) AS VARCHAR)` |
| `stddev` | `CAST(STDDEV(col) AS VARCHAR)` |
| `min` | `CAST(MIN(col) AS VARCHAR)` |
| `max` | `CAST(MAX(col) AS VARCHAR)` |
| `25%` / `50%` / `75%` | `CAST(APPROX_QUANTILE(col, 0.25/0.50/0.75) AS VARCHAR)` |

### `crates/connect-server/src/converter/relation_converter.rs`

`convert_describe()` and `convert_summary()` added. Both call `self.infer_columns(&input_plan)`
to resolve the column list when the proto-level column list is empty.

---

## 3. Generator correctness fixes — `crates/core/src/generator/mod.rs`

### 3a. Filter stack subquery wrapping (`extract_filters`)

**Problem**: `gen_project(Project { input: Filter(Filter(Join(l1, l2))) })` was wrapping each
`Filter` in a subquery, making `l1`/`l2` aliases inaccessible from the outer `SELECT`/`WHERE`.

**Fix**: `extract_filters(plan: &LogicalPlan) -> (&LogicalPlan, Vec<&Expression>)` peels all
consecutive `Filter` nodes from the top of the plan tree, returning the base plan and accumulated
conditions. Called at the top of `gen_project`, `gen_aggregate`, and `gen_filter`. Each condition
is wrapped in `()` to avoid precedence bugs (`A AND B OR C` without parens).

### 3b. SEMI/ANTI join qualifier stripping

**Problem**: SEMI/ANTI join conditions reference `l1.col` but after `gen_join` wraps the left side
in a subquery, `l1` is an inner alias invisible from the outer `WHERE EXISTS (...)` clause.

**Fix**: `collect_plan_aliases(plan, &mut HashSet)` recursively collects all subquery aliases from
a plan subtree. `strip_qualifiers_in_expr(expr, aliases)` strips qualifier prefixes matching any
collected alias. Applied to SEMI/ANTI join conditions in `gen_join` before emitting the EXISTS
subquery condition. Handles `Unary(Not, Binary(...))` since `!=` is encoded as `NOT (... = ...)`.

### 3c. USING join column reordering (`gen_using_join_select`)

**Problem**: DuckDB's native `USING` keeps the key column at its natural left-table position.
Spark always outputs USING key columns first.

**Fix**: Free function `gen_using_join_select(using_columns: &[String]) -> String` returns
`SELECT key, * EXCLUDE key` (or `SELECT k1, k2, * EXCLUDE (k1, k2)` for multi-key). Applied in
both `gen_plan(Join)` and `gen_from(Join)` when `!j.using_columns.is_empty()`. The join itself
uses native DuckDB USING; only the outer SELECT reorders.

---

## 4. `withColumn` column ordering fix — `relation_converter.rs`

**Problem**: `convert_with_columns` used a `COLUMNS(lambda)` DuckDB expression which always
appended replaced columns to the end of the output, violating Spark's in-place replacement
semantics.

**Fix**: `self.infer_columns(&input_plan)` resolves the input schema's column list. An explicit
`Project` is built preserving original column positions, with `WithColumns` expressions spliced
in at their original positions.

---

## Bug patterns documented in memory

All patterns above were recorded in `project_generator_gaps.md` and `project_schema_inferrer_gaps.md`
under `/home/vscode/.claude/projects/-workspace/memory/`. Key cross-cutting lessons:

- **`extract_filters` is the canonical fix for filter-stack subquery wrapping**: any new `gen_*`
  method that accepts a `Filter(...)` subtree should call `extract_filters` first.
- **`infer_schema()` for `TableScan` intentionally returns empty**: the DuckDB `LIMIT 0` fallback
  in `service.rs` handles all table-scan-based plans. Do not attempt to populate `TableScan` schema
  at parse/conversion time without passing a `DuckDbSession` reference into the converter.
- **USING joins have two code paths** (`gen_plan` and `gen_from`); both must be updated in tandem.

---

## Test status

- Core unit tests: **75/75** passing
- Release binary: builds clean (3 unused-import warnings, no errors)
- Differential tests: **665 passing, 4 failing** (unchanged)
  - `test_join_empty_with_non_empty` — pre-existing empty LocalRelation join schema mismatch
  - `test_crosstab_basic`, `test_freqitems_basic`, `test_sampleby_preserves_schema` — Phase 5 (unimplemented)
