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
  [`duckdb1.5.1-ext1`](https://github.com/lastrk/thunderduck-duckdb-extension/releases/tag/duckdb1.5.1-ext1)
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

## Section 8 — Strict mode failures (327 as of 2026-04-02)

Strict mode baseline before nullable fix: **405/836**. After nullable inference overhaul: **508/836**.
Relaxed mode unaffected: **830/836** (6 skipped, 0 failed).

### 8.1 Source-column nullable not propagated — ~170 tests

**Root cause**: DuckDB does not expose `NOT NULL` constraint metadata for Parquet-scanned columns.
`DESCRIBE` results and Arrow schema from DuckDB both report all scanned columns as `nullable=true`.
This means `TypeInferenceEngine::column_nullable()` always returns `true` for source columns.

Affects: virtually every test that projects source columns — TPC-H, TPC-DS, dataframe functions,
window functions, joins, aggregations. Spark marks Parquet columns NOT NULL when the Parquet schema
says `REQUIRED`; Thunderduck cannot recover this without reading Parquet metadata directly.

**Specific symptoms** (from test output):
- `Column 'l_quantity': nullable mismatch - Reference=False, Test=True`
- `Column 'cnt': nullable mismatch - Reference=False, Test=True` (COUNT result — **FIXED** by nullable overhaul)
- `Column 'o_orderkey': nullable mismatch - Reference=False, Test=True`

### 8.2 `spark_sum` / `spark_avg` decimal precision wrong — ~40 tests

`spark_sum(DECIMAL(p,s))` should return `DECIMAL(min(p+10,38), s)` but DuckDB's extension
function returns `DECIMAL(38,2)` for TPC-H columns (which are `DECIMAL(15,2)`; expected
`DECIMAL(25,2)`). `spark_avg(DECIMAL(p,s))` should return `DECIMAL(min(p+4,38), min(s+4,18))`
but returns `DoubleType`.

**Affected tests**: TPC-H Q1 SQL + DataFrame, basic aggregation decimal tests, TPC-DS queries.

### 8.3 `collect_list` / `collect_set` return scalar — 4 tests

`collect_list(int_col)` returns `IntegerType()` instead of `ArrayType(IntegerType(), False)`.
The DuckDB `LIST()` aggregate function returns an array but the schema inference path emits the
element type instead of the array type. `aggregate_return_type("collect_list", IntegerType)`
returns `ArrayType(IntegerType, false)` — the type is correct but something downstream strips
the array wrapper.

### 8.4 Array element nullability — ~20 tests

`ArrayType(T, True)` (Thunderduck) vs `ArrayType(T, False)` (Spark). The `containsNull` flag
inside the array type is not tracked. Affects: `split`, array function results, lambda outputs.

### 8.5 Type mismatches (not nullable) — ~60 tests

Pre-existing type issues not related to nullable inference:
- **CaseWhen type**: `StringType` vs `BooleanType` — type inference for CASE WHEN branches
  not fully resolved (pre-existing, not a nullable issue)
- **`id` column Integer vs Long**: Range / source schema INTEGER vs Spark's Long preference
- **Math functions nullable**: functions like `abs`, `round`, `ceil` return `nullable=true`
  from Thunderduck but Spark marks them non-nullable when input is non-null (blocked on 8.1)
- **`size()` return type**: returns `LongType` instead of `IntegerType`

### 8.6 Decimal arithmetic precision — ~30 tests (TPC-DS, TPC-H)

Complex decimal expressions in TPC-DS produce wrong precision/scale. Related to `spark_sum`
precision issues (8.2) cascading through multi-step expressions.
