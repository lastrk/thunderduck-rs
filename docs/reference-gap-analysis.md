# Reference Gap Analysis — Updated Snapshot

Verified comparison of the Java reference implementation (`.reference/`) against the Rust port
(`crates/core/`). All findings are confirmed against actual source files.

**Date**: 2026-04-02 (updated after strict mode bring-up + nullable inference fix)
**Reference**: 210 Java source files, 4091-line `SQLGenerator.java`, 1776-line `FunctionRegistry.java`

Phases 3, 4, 5, and 6 (Wave 1 + Wave 2) are complete. Every item originally classified as
**Critical** or **Important** in the 2026-03-18 analysis has been implemented. Full suite
results as of 2026-03-31: **829 passing, 0 reproducible failures, 6 skipped** (836 total; 1
pre-existing flaky test occasionally fails in full-suite runs but passes in isolation).

Phase 6 Wave 2 closed +4 tests: map key access (×2), map explode, json_tuple.
TPC-DS Q17 (the final Wave 1 deferred item) was also fixed via flat join chain extension.

**2026-04-02 update — strict mode + nullable inference:**

- DuckDB upgraded to 1.10501.0 (DuckDB 1.5.1); arrow/arrow-ipc bumped to 58.
- Strict mode fully operational: pre-built extension binary downloaded from
  [`duckdb1.5.1-ext2`](https://github.com/lastrk/thunderduck-duckdb-extension/releases/tag/duckdb1.5.1-ext2)
  at build time via `cargo build --release --features bundled-extension`.
- Nullable inference overhauled (mode-agnostic, matching Java reference):
  `projection_to_field`, `agg_expr_to_field`, `infer_with_columns_schema` now call
  `expr.nullable(schema)`; `Cast` delegates to inner expr; `CaseWhen` analyses branches;
  `FunctionCall` uses `aggregate_is_always_nullable()` for SUM/AVG/MIN/MAX etc.
  Result: relaxed suite **830/830** (no regressions); strict suite **508/836** (+103 vs baseline).
- Remaining strict-mode failures: **327** — see Section 8 for full breakdown.

---

## Section 1 — Closed gaps

All items from the original 2026-03-18 analysis, now implemented:

| Item | Closed in | Location |
|---|---|---|
| `LikeExpression` | Phase 3 | `expression/mod.rs:173`, `generator/mod.rs:1398` |
| `SingleRowRelation` | Phase 3 | `logical/mod.rs` |
| `IntervalExpression` | Phase 3 | `expression/mod.rs:174`, `generator/mod.rs:1410` |
| Timezone hardcoded `'UTC'` | Phase 3 | `session.rs` — `detect_timezone()` |
| `preserve_insertion_order=true` | Phase 3 | `session.rs:117` |
| `initcap` macro | Phase 3 | `session.rs:181` |
| Polymorphic function resolution | Phase 2 | `generator/mod.rs` — `_spark_reverse`, `size`, `sort_array` |
| `ExtractValueExpression` | Phase 3 | `expression/mod.rs:176`, `generator/mod.rs:1496` |
| `IsDistinctFromExpression` | Phase 3 | `expression/mod.rs:175`, `generator/mod.rs:1485` |
| `RowConstructorExpression` | Phase 3 | `expression/mod.rs:177`, `generator/mod.rs:1522` |
| `UpdateFieldsExpression` | Phase 4 | `expression/mod.rs`, `generator/mod.rs` |
| `RegexColumnExpression` | Phase 3 | `generator/mod.rs` — `gen_function_call` special-case |
| `FieldAccessExpression` | Phase 3 | via `ExtractValueExpression` |
| Arrow schema fixup | Phase 3 | `arrow_ipc.rs`, `service.rs` schema inference path |
| NADrop / NAFill / NAReplace / Unpivot | Phase 3 | `relation_converter.rs` |
| Describe / Summary | Phase 4 | `relation_converter.rs` |
| Pivot / StatCov / StatCorr / ApproxQuantile | Phase 4 | `relation_converter.rs` |
| StatCrosstab / StatFreqItems / StatSampleBy | Phase 5 | `relation_converter.rs`, `generator/mod.rs`, `logical/mod.rs` |
| NAReplace NULL literal (`IS NULL` vs `= NULL`) | Phase 5 bugfix | `generator/mod.rs` — `gen_na_replace` |
| Empty LocalRelation join schema collapse | Phase 5 bugfix | `logical/mod.rs` — `Join` arm of `infer_schema()` |
| WriteOperation / CTE (WithRelations) | Phase 3/4 | `relation_converter.rs` |
| Distinct column subset | Phase 3/4 | `generator/mod.rs` — `gen_distinct` uses `ROW_NUMBER() OVER (PARTITION BY ...)` |
| GROUPING/GROUPING_ID return type | Section 3 | `generator/mod.rs` — `gen_function_call`; DuckDB INTEGER → CAST to TINYINT/BIGINT |
| DECIMAL SUM/AVG precision | Section 3 | `generator/mod.rs` — `cast_integer_sum`; decimal SUM/AVG with Spark precision rules |
| Union type widening CASTs | Section 3 | `generator/mod.rs` — `gen_union`; emits explicit CASTs when left/right types differ |
| ROLLUP/CUBE NULLS FIRST | Section 3 | `generator/mod.rs` — `gen_sort`; forces `NULLS FIRST` for ROLLUP/CUBE sort orders |
| Backtick identifier quoting | 2026-03-25 | `generator/mod.rs` — `rewrite_backtick_identifiers()` in `preprocess_spark_sql`; converts `` `col` `` → `"col"` for DuckDB compatibility |
| SparkSQL parser (sqlparser-rs) | 2026-03-26 | `crates/core/src/parser/` — `SparkSqlParser`, `SparkDialect`, `SqlConverter`; replaces text-preprocessing path for `spark.sql()` with a proper parse-and-convert pipeline (ADR-21) |
| `count(1)` aggregate alias | 2026-03-26 | `generator/mod.rs` — `render_agg_expr()` now always emits an explicit Spark-convention `AS` alias for unaliased aggregates; prevents DuckDB's `count_star()` naming |
| DECIMAL spacing in column names | 2026-03-26 | `logical/mod.rs` — `spark_column_name()` extended for Cast/Binary/Unary/Literal; `spark_type_name()` formats `DECIMAL(p,s)` without spaces; `generator/mod.rs` — `gen_projection_list()` adds explicit alias for any projection containing a DECIMAL cast |
| Duplicate column `_1` suffix | 2026-03-26 | `service.rs` — `rename_to_spark_schema()` post-processes Arrow RecordBatch column names to match `plan.infer_schema()`; `analyze_plan` path merges Spark names with DuckDB-inferred types when schema contains CTE references |

---

## Section 2 — Active bugs

None. All known bugs are resolved. The last two (NAReplace NULL literal and empty
LocalRelation join schema) were fixed in the Phase 5 session (commit `936f229`).

---

## Section 3 — Generator correctness gaps (low priority)

| Gap | File / approx line | Severity | Notes |
|---|---|---|---|
| Auto-alias unaliased expressions | `generator/mod.rs` | Low | **Partially closed** — unaliased aggregates (`render_agg_expr`) and DECIMAL-cast projections (`gen_projection_list`) now emit explicit Spark-convention aliases (2026-03-26). Remaining gap: arbitrary complex non-cast expressions in SELECT still rely on DuckDB's naming. No test failures observed in 796 differential tests; broader fix deferred until a failing test surfaces. |

---

## Section 4 — Missing optimisations (low priority)

| Gap | Notes |
|---|---|
| `generateFlatJoinChainWithMapping` | Rust emits nested subqueries; Java reference builds a flat `FROM t1, t2, t3` with an alias map — avoids extra subquery layers and alias resolution issues |
| `tryGenerateFlatSemiAntiJoin` | Stacked SEMI/ANTI join chains are not flattened; each hop wraps in an EXISTS subquery |
| WithColumns strict-mode CAST | ~~`withColumn` replacement columns are not explicitly CAST to the declared type in strict mode~~ **CLOSED** — `try_strict_decimal_cast` in `gen_projection_list` wraps computed DECIMAL projections in strict mode |
| Sample with replacement | ~~`df.sample(withReplacement=True)` silently uses `SYSTEM` sampling~~ **CLOSED** — `gen_sample` now returns `Unsupported` error for `with_replacement=true` |

---

## Section 5 — Priority summary

| Item | Severity | Status |
|---|---|---|
| Auto-alias complex projections | **Low** | Partially closed (aggregates + DECIMAL casts done; arbitrary expressions deferred) |
| Flat join chain / flat SEMI/ANTI | **Low** | Open |
| WithColumns strict-mode CAST | **Low** | **Closed** (2026-03-27) |
| Sample with replacement error | **Low** | **Closed** (2026-03-27) |

---

## Section 6 — Phase 6 Wave 1 closures (2026-03-31)

Phase 6 Wave 1 closed 106 of the 111 pre-existing failures.

836 tests total: **825 passing, 5 failing, 6 skipped** (as of 2026-03-31).

### 6.1 DDL via SQL path — CLOSED

All DDL statement types (`DROP TABLE`, `CREATE TABLE`, `CREATE VIEW`, `INSERT INTO`, `TRUNCATE`,
`ALTER TABLE RENAME COLUMN`) are now handled by `sql_converter.rs`. `VALUES (...)` clause also
implemented. All test files in this group now pass.

### 6.2 Lambda / higher-order functions — CLOSED

`TRANSFORM`, `FILTER`, `EXISTS`, `FORALL`, `AGGREGATE` HOF syntax is now converted via
`Expr::Lambda → Expression::Lambda`. SparkDialect now enables lambda parsing. All 22 tests pass.

### 6.3 Complex type constructors and accessors — CLOSED

- struct field access, array index, map key access, map explode all pass.
- Root cause of map test failures: `preprocess_spark_sql` was double-processing DDL SqlRelations,
  causing `MAP(['a','b','c'], [1,2,3])` → `MAP([['a','b','c']], [[1,2,3]])`. Fixed by skipping
  `preprocess_spark_sql` for DDL in `gen_sql_relation`.
- Map explode column naming fixed: `spark_column_name` now strips double-quote wrapping from
  `AS "key"` aliases in RawSql expressions.

### 6.4 Array / explode functions — PARTIALLY CLOSED

- **Closed**: `COLLECT_LIST` → `LIST()`, `COLLECT_SET` → `LIST(DISTINCT)` in FunctionRegistry
- **Remaining**: `SPLIT`, `EXPLODE`, `SIZE` in the SparkSQL parser path still fail
  (`test_split_function`, `test_explode_function`, `test_size_in_select_expr` etc.)

### 6.5 JSON functions — CLOSED (Wave 2)

`json_tuple(col, 'k1', 'k2') AS (name, age)` — Spark generator function syntax that sqlparser-rs
cannot parse. Fixed with a pre-parse rewrite in `SparkSqlParser::parse` that expands it to
individual `json_extract_string(col, '$.k') AS col_alias` items before the parser runs.

### 6.6 TPC-DS DataFrame queries 17, 25, 29 — CLOSED

- Q25/Q29: natural flat join (right `AliasedRelation` uses alias directly)
- Q17: extended flat join to also cover plain `TableScan`/`InMemoryRelation` on right side when
  left subtree contains user-facing AliasedRelations. Adds `plan_contains_user_alias()` and
  `right_plan_natural_name()` helpers in `generator/mod.rs`.

### 6.7 String function gaps — CLOSED

All four string function gaps resolved:
- `overlay` → DuckDB `OVERLAY` passthrough via `sql_converter.rs`
- `octet_length` → session macro `octet_length(s) AS MACRO (BIT_LENGTH(s) / 8)`
- `format_number` → `format('{:,.<d>f}', n)` with thousand-separator
- `to_char` → `strftime` with Spark→DuckDB format string mapping

### 6.8 Miscellaneous — CLOSED

- `test_select_from_values` — `SetExpr::Values` now handled in `sql_converter.rs`
- `test_bit_get` — fixed: `((CAST(x AS BIGINT) >> pos) & 1)` (was `GET_BIT` on BIT type, always 0)

---

## Section 7 — Remaining failures (0 reproducible as of 2026-03-31)

All previously catalogued failures are now closed. The suite runs at **829 passing / 6 skipped**.
One test (`test_statistics_differential.py::TestStatSummary_Differential::test_summary_default_stats`)
occasionally fails in the full suite due to a pre-existing ordering-dependent flakiness; it passes
in isolation and in sub-suite runs. Not a code regression.

---

## Section 8 — Strict mode failures (119 as of 2026-04-03)

**History**: Strict mode baseline **405/836** → nullable inference overhaul **508/836** →
CaseWhen `unify_types` fix + review fixes **686/836** → decimal precision fixes **687/836** →
struct field nullable **695/836** → unpivot nullable + array containsNull + HOF types +
CTE schema propagation **716/836**.

Relaxed mode: **824/836** (6 skipped, 6 pre-existing map failures, 2 pre-existing TPC-DS).

**Critical finding (2026-04-02)**: The original 8.1 hypothesis — that failures stem from DuckDB
not exposing Parquet `REQUIRED`/`OPTIONAL` metadata — was **wrong**. Spark's `DataSource.scala`
calls `dataSchema.asNullable` when building `HadoopFsRelation`, which recursively forces all
`nullable` flags to `true`, all `ArrayType.containsNull` to `true`, and all
`MapType.valueContainsNull` to `true`. This means both engines agree on source column
nullability. The 149 remaining failures are all in the **type derivation layer** — schema
computation for expressions, functions, and complex types.

DuckDB's `parquet_schema()` table function (which exposes `repetition_type` = REQUIRED/OPTIONAL/
REPEATED) is available if needed in the future, but is **not the fix** for these failures.

### 8.1 Decimal precision/scale mismatches — ~40-50 tests (PARTIALLY CLOSED 2026-04-03)

**Fixes applied (2026-04-02)**:
- `decimal_div_type()` wired into expression `data_type()` (was defined but never called)
- `decimal_mod_type()` added for Spark-correct modulo precision
- AVG scale cap fixed: `min(s+4, 38)` → `min(min(s+4, 18), precision)` (matches Spark)
- `integral_to_decimal` promotion for mixed Decimal÷Integer operations
- Strict mode: `spark_decimal_div()` for decimal division SQL generation
- Strict mode: `spark_sum()` extension for decimal SUM (was native sum + CAST)

**Fixes applied (2026-04-03)**:
- CTE schema propagation: `enrich_table_scans()` now traverses `WithCte` nodes;
  `propagate_cte_schemas()` infers CTE schemas in definition order and populates
  CTE-referencing TableScan schemas. Handles cascading CTEs (CTE2 → CTE1). (+9 tests)
- Root cause: CTE-referenced TableScans had empty schemas → `apply_agg_type_casts()` and
  `gen_strict_decimal_div()` couldn't detect decimal types → no Spark-correct CASTs emitted

**Remaining**: ~35 TPC-DS/TPC-H queries still have decimal precision mismatches from
expression-level type inference gaps (ROUND scale, nested division cascades, implicit
aggregation in Project without GROUP BY) and extension return type mismatches.

**Affected tests**: TPC-H Q8, TPC-DS Q2/Q5/Q9/Q12/Q20 and ~30 other TPC-DS queries.

### 8.2 Struct field nullable — ~35-40 tests (PARTIALLY CLOSED 2026-04-02)

**Fixes applied (2026-04-02)**:
- `StructLiteral.data_type()` now resolves field types and nullable flags from value expressions
  (was returning `DataType::Unresolved`)
- `named_struct`/`struct` FunctionCall special handling added — parses alternating name/value
  pairs and builds proper StructType with per-field nullable flags
- `function_return_type()` returns `Unresolved` for struct/named_struct (was `String`),
  deferring to the special handling

**Result**: +8 strict mode tests fixed. Remaining struct failures likely involve nested structs
from other code paths (projections, aggregates) that don't go through StructLiteral.

**Affected tests**: `test_type_literals_differential.py` struct tests, complex nested types,
`test_complex_types_differential.py`.

### 8.3 Simple column nullable propagation — ~30-35 tests (PARTIALLY CLOSED 2026-04-03)

**Fixes applied (2026-04-03)**:
- Unpivot `infer_schema()` now preserves ID column type + nullable from input schema
  (was hardcoded `nullable=true`). Value column nullable = OR of all input value columns.
- Pivot `infer_schema()` investigated but reverted — partial schema approach caused relaxed
  mode regressions (column count mismatch in rename path). Pivot remains DuckDB fallback.

**Remaining**: Pivot grouping columns still lose non-nullable. Arithmetic and window function
results downstream of pivot/unpivot may still propagate incorrect nullable.

**Affected tests**: `test_multidim_aggregations.py` (pivot), timestamp arithmetic,
window function results.

### 8.4 Array containsNull flag — ~25-30 tests (PARTIALLY CLOSED 2026-04-03)

**Fixes applied (2026-04-03)**:
- `augment_schema_with_lambda_params()` helper binds lambda params to array element type,
  enabling lambda body type/nullable resolution via schema lookup
- `transform` HOF: `containsNull` derived from `lambda.body.nullable(&augmented_schema)`
- `filter` HOF: returns first arg's data_type directly (preserves input containsNull)
- `LambdaVariable.data_type()` resolves from schema (was `Unresolved`)
- `LambdaVariable.nullable()` resolves from schema (was `false`)
- Lambda param name collision: existing schema fields filtered before appending bindings

**Result**: +9 strict mode tests (combined with 8.5 HOF fixes).

**Remaining**: ~16 lambda tests still fail — nested transforms, SQL-path lambdas,
combined operations where lambda body type inference is incomplete.

**Affected tests**: `test_lambda_differential.py` remaining failures.

### 8.5 HOF function result types — ~15-20 tests (CLOSED 2026-04-03)

**CaseWhen type inference**: **CLOSED** (2026-04-02). Added `unify_types()`.

**HOF return types**: **CLOSED** (2026-04-03).
- `exists`/`forall` → `BooleanType` (was returning input array type)
- `aggregate`/`reduce` → accumulator type from init arg (was returning array type)
- Finish lambda support: 4th arg to `aggregate` resolved via augmented schema
- HOF nullable rules: `transform`/`filter` propagate input nullable, `exists`/`forall`
  propagate input nullable, `aggregate` always nullable

### 8.6 Math function nullable semantics — ~10-15 tests

**Root cause**: Thunderduck's `Expression::nullable()` default rule
(`f.args.iter().any(|a| a.nullable(schema))`) marks math functions as non-nullable when input
is non-nullable. But Spark marks most math functions (CEILING, FLOOR, LN, LOG, ROUND, etc.)
as `nullable=true` unconditionally — because they can produce null on edge cases (e.g.,
`LN(0)` → null, division by zero).

**Fix direction**: Add these functions to an "always nullable" list in `Expression::nullable()`,
similar to how `aggregate_is_always_nullable()` works for aggregates.

**Affected tests**: `test_math_bitwise_date_differential.py`, scattered math operations in
other test files.

### 8.7 Map type construction — ~5-10 tests

**Root cause**: `MAP_FROM_ARRAYS`, `MAP_KEYS`, `MAP_VALUES`, `MAP_ENTRIES` have incomplete
type handling. Map keys/values get wrapped in extra array layers instead of returning scalars.

**Example**: `MAP_KEYS(map_col)` returns `ArrayType(ArrayType(StringType()))` instead of
`ArrayType(StringType())`.

**Affected tests**: `test_dataframe_functions.py` map tests (also fail in relaxed mode —
pre-existing).

### 8.8 Priority summary

| Category | ~Tests | Status | Notes |
|----------|--------|--------|-------|
| 8.1 Decimal precision | ~35 remaining | Partially closed | CTE propagation fixed +9; expression-level gaps remain |
| 8.2 Struct field nullable | ~30 remaining | Partially closed | StructLiteral fixed +8; nested struct paths remain |
| 8.3 Column nullable propagation | ~25 remaining | Partially closed | Unpivot fixed; Pivot reverted (regression) |
| 8.4 Array containsNull | ~16 remaining | Partially closed | Lambda augmentation fixed +9; nested/SQL-path remain |
| 8.5 HOF result types | 0 | **CLOSED** | exists/forall→Boolean, aggregate→accumulator |
| 8.6 Math function nullable | 10-15 | Open | Add "always nullable" function list |
| 8.7 Map type construction | 5-10 | Open | Pre-existing, also affects relaxed mode |
