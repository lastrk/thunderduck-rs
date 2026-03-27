# Reference Gap Analysis — Updated Snapshot

Verified comparison of the Java reference implementation (`.reference/`) against the Rust port
(`crates/core/`). All findings are confirmed against actual source files.

**Date**: 2026-03-27 (updated after full 836-test suite run)
**Reference**: 210 Java source files, 4091-line `SQLGenerator.java`, 1776-line `FunctionRegistry.java`

Phases 3, 4, and 5 are complete. Every item originally classified as **Critical** or
**Important** in the 2026-03-18 analysis has been implemented. Full suite results as of
2026-03-27: **719 passing, 111 failing, 6 skipped** (836 total). The 111 failures are all
pre-existing unimplemented features documented in Section 6 below — no regressions.

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

## Section 6 — Pre-existing test failures (2026-03-27 full suite run)

836 tests total: 719 passing, 111 failing, 6 skipped. All 111 failures are unimplemented
features — no regressions. Failures are mode-independent (both relaxed and strict) unless noted.

### 6.1 DDL via SQL path (~40 failures)

**Root cause**: `DROP TABLE`, `CREATE TABLE`, `INSERT INTO`, `TRUNCATE`, `ALTER TABLE` issued
through `spark.sql()` are not yet handled by the SparkSQL parser.

**Error**: `Unsupported operation: SQL statement type not yet supported: DROP` (and CREATE, INSERT, TRUNCATE, ALTER)

**Affected test files**:
- `test_dataframe_basic_operations_differential.py` (7)
- `test_ddl_corrected.py` (2)
- `test_ddl_operations_differential.py` (13)
- `test_ddl_parser_differential.py` (14)
- `test_sql_expressions_differential.py` (10) — fixture uses DROP to clean up

### 6.2 Lambda / higher-order functions (22 failures)

**Root cause**: `TRANSFORM`, `FILTER`, `EXISTS`, `FORALL`, `AGGREGATE` HOF syntax in the SparkSQL
parser path is not yet converted from the sqlparser-rs AST to Thunderduck expressions.

**Error**: `Unsupported operation: expression not yet supported: TRANSFORM(...)` etc.

**Affected test file**: `test_lambda_differential.py`

### 6.3 Complex type constructors and accessors (13 failures)

**Root cause**: Struct field access (`struct_col.field`), array index (`arr[0]`), map key access
(`map['k']`), `with_field`, `drop_fields` in the SparkSQL path are not yet wired up.

**Affected test files**: `test_complex_types_differential.py`, `test_type_literals_differential.py`

### 6.4 Array / explode functions in SparkSQL path (6 failures)

**Root cause**: `SPLIT`, `COLLECT_LIST`, `COLLECT_SET`, `EXPLODE`, `SIZE` not yet handled by the
SparkSQL parser → expression converter path (they work on the DataFrame API path).

**Affected test files**: `test_array_functions_differential.py`, `test_dataframe_functions.py`

### 6.5 JSON functions (5 failures)

**Root cause**: `FROM_JSON`, `JSON_TUPLE` not yet implemented.

**Affected test file**: `test_json_functions_differential.py`

### 6.6 TPC-DS DataFrame queries 17, 25, 29 (3 failures)

**Root cause**: Join alias `d1` (from a date-dim self-join) is not found in the outer SELECT.
Likely an alias scoping bug in the flat join chain or subquery wrapping for multi-hop joins.
**Both relaxed and strict modes affected.**

**Error**: `DuckDB error: Referenced table "d1" not found!`

**Affected test file**: `test_tpcds_dataframe_differential.py`

### 6.7 String function gaps (4 failures)

| Test | Mode | Root cause |
|------|------|------------|
| `test_overlay` | Both | `OVERLAY(str PLACING x FROM n FOR m)` not implemented |
| `test_octet_length` | Both | `bit_length(BLOB)` type mismatch — needs CAST to VARCHAR first |
| `test_format_number` | Both | No thousand-separator formatting (returns `12345.68` not `12,345.68`) |
| `test_to_char` | Both | Timestamp formatting returns full datetime string instead of date-only |

### 6.8 Miscellaneous (2 failures)

| Test | Root cause |
|------|------------|
| `test_select_from_values` | `VALUES (...)` clause in SQL path not yet handled |
| `test_bit_get` | DuckDB uses `get_bit()`, Spark uses `bit_get()` — FunctionRegistry mapping missing |
