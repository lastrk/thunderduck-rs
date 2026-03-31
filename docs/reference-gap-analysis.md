# Reference Gap Analysis — Updated Snapshot

Verified comparison of the Java reference implementation (`.reference/`) against the Rust port
(`crates/core/`). All findings are confirmed against actual source files.

**Date**: 2026-03-31 (updated after Phase 6 Wave 1)
**Reference**: 210 Java source files, 4091-line `SQLGenerator.java`, 1776-line `FunctionRegistry.java`

Phases 3, 4, 5, and 6 Wave 1 are complete. Every item originally classified as **Critical** or
**Important** in the 2026-03-18 analysis has been implemented. Full suite results as of
2026-03-31: **825 passing, 5 failing, 6 skipped** (836 total). Phase 6 Wave 1 closed +106 tests
(DDL, HOF/lambdas, complex type accessors, VALUES, bit_get, collect_list/set, string functions,
TPC-DS Q25/Q29 join alias fix, selectExpr column naming fix). Remaining 5 failures: map key
access ×3, json_tuple (Wave 2), TPC-DS Q17 (flat join chain).

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

### 6.3 Complex type constructors and accessors — PARTIALLY CLOSED

- **Closed**: struct field access (`Expr::CompoundFieldAccess` → chained `ExtractValue`),
  array index (`Expr::Subscript` → `ExtractValue`)
- **Remaining (3 failures)**: map key access (`map['key']`) and map explode — DuckDB map
  subscript semantics differ from Spark; needs investigation in Wave 2

### 6.4 Array / explode functions — PARTIALLY CLOSED

- **Closed**: `COLLECT_LIST` → `LIST()`, `COLLECT_SET` → `LIST(DISTINCT)` in FunctionRegistry
- **Remaining**: `SPLIT`, `EXPLODE`, `SIZE` in the SparkSQL parser path still fail
  (`test_split_function`, `test_explode_function`, `test_size_in_select_expr` etc.)

### 6.5 JSON functions — OPEN (Wave 2)

`FROM_JSON`, `JSON_TUPLE` not yet implemented. 1 remaining failure (`test_json_tuple`).

### 6.6 TPC-DS DataFrame queries 17, 25, 29 — PARTIALLY CLOSED

- **Closed**: Q25 and Q29 — natural flat join fix: user-aliased `AliasedRelation` on right side
  now uses alias directly instead of `__td_jr_X__` subquery wrapping
- **Remaining (1 failure)**: Q17 — date_dim aliases applied via `.filter()` after joins; requires
  flat join chain generation (larger refactor, deferred)

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

## Section 7 — Remaining failures (5 as of 2026-03-31)

| Test | Root cause | Target |
|------|-----------|--------|
| `test_map_string_key` | DuckDB map subscript syntax vs Spark semantics | Wave 2 |
| `test_map_missing_key` | Same | Wave 2 |
| `test_explode_map` | `explode(map_col)` lateral expansion for map type | Wave 2 |
| `test_json_tuple` | `JSON_TUPLE(col, 'k1', 'k2')` lateral expansion | Wave 2 |
| `test_tpcds_dataframe_query[17]` | Q17: date_dim aliases in `.filter()` after joins | Deferred |
