# Reference Gap Analysis — Phase 1 & 2

Verified comparison of the Java reference implementation (`.reference/`) against the Rust port
(`crates/core/`). All findings are confirmed against actual source files.

**Date**: 2026-03-18
**Reference**: 210 Java source files, 4091-line `SQLGenerator.java`, 1776-line `FunctionRegistry.java`

---

## Expression variants missing from Rust

The Java `Expression` interface requires every implementor to provide `toSQL()`. Each of these
exists in `.reference/core/.../expression/` with a full `toSQL()` implementation but has no
corresponding Rust `Expression` variant:

| Java class | What it represents | Generated SQL |
|---|---|---|
| `LikeExpression` | `LIKE` / `NOT LIKE` / `ILIKE` (negation + case-insensitive flags) | `(val LIKE pattern)` |
| `IntervalExpression` | `INTERVAL` literals — YEAR_MONTH, DAY_TIME, CALENDAR sub-types | Composite `INTERVAL 'n' DAY + INTERVAL 'n' HOUR` etc. |
| `ExtractValueExpression` | `struct.field`, `array[idx]`, `map[key]` subscript access | `struct['field']`, `list[idx+1]`, `map['key']` |
| `UpdateFieldsExpression` | `withField()` / `dropFields()` on struct columns | `struct_pack(...)` / `EXCLUDE` |
| `RowConstructorExpression` | Tuple `(a, b, c)` — e.g. `WHERE (x,y) IN ((1,2))` | `(expr1, expr2, ...)` |
| `FieldAccessExpression` | Nested field access via `.` chain | Struct field subscript |
| `IsDistinctFromExpression` | `IS DISTINCT FROM` / `IS NOT DISTINCT FROM` | `IS DISTINCT FROM` |
| `RegexColumnExpression` | `rlike` / column regex filter | `REGEXP_MATCHES(col, pattern)` |

**Critical**: `LikeExpression` (ubiquitous), `IntervalExpression` (date arithmetic).
**Important**: `ExtractValueExpression` (struct/array/map access), `IsDistinctFromExpression`,
`RowConstructorExpression`.

---

## LogicalPlan variants missing from Rust

| Java class | Purpose | Generator behaviour |
|---|---|---|
| `SingleRowRelation` | No-FROM queries: `SELECT 1`, `SELECT ARRAY(1,2,3)` | Generator skips `FROM` clause: `if (!(plan.child() instanceof SingleRowRelation))` |

**Critical** — any `spark.sql("SELECT ...")` with no table reference uses this. It is the default
relation for constant expressions and scalar UDFs.

---

## Session initialisation gaps

Reference `DuckDBRuntime.configureConnection()` applies settings the Rust session does not:

```
SET allocator_background_threads=true   -- Linux only, 8+ cores; jemalloc perf
SET enable_progress_bar=false           -- suppress log noise
SET preserve_insertion_order=true       -- deterministic row order
SET TimeZone='<system TZ>'              -- Rust hardcodes 'UTC' ← correctness bug
allow_unsigned_extensions=true          -- required for bundled thdck_spark_funcs
```

Then `registerSparkCompatMacros()` runs at startup. Currently one macro:

```sql
-- Spark's initcap treats only whitespace as word boundaries.
-- DuckDB's built-in initcap also capitalises after punctuation — wrong.
CREATE OR REPLACE MACRO initcap(s) AS
  regexp_replace(lower(s), '(^|\\s)(\\S)', '\\1' || upper('\\2'), 'g')
```

**Correctness bug**: The hardcoded `'UTC'` timezone causes `hour()`, `dayofweek()`,
`date_trunc()` and similar timestamp functions to return wrong results for non-UTC environments.
Fix: use `std::env::var("TZ")` or platform timezone detection in `session.rs`.

---

## Arrow schema handling gap

`ArrowInterchange.java` provides `arrowTypeToSQLType()` mapping DuckDB Arrow output types to
SQL type strings, and handles `Utf8` → `VARCHAR`, `LargeUtf8` → `VARCHAR`, type coercion at
the DuckDB/Spark boundary. The Rust port collects `RecordBatch` from `query_arrow()` with no
schema fixup.

Most type mismatches are pre-empted at SQL generation time via explicit CASTs (e.g. the
`HUGEINT` → `BIGINT` rule for `SUM(integer)`). A schema-fixup pass becomes important in Phase 3
when the gRPC layer compares the emitted Arrow schema against the Spark-declared schema.

---

## Polymorphic function resolution — missing

The reference generator has a `resolvePolymorphicFunctions()` pass that inspects the **child
schema** to dispatch overloaded Spark functions to type-specific DuckDB equivalents:

| Spark call | Array type | String type |
|---|---|---|
| `reverse(x)` | → `list_reverse(x)` | → `reverse(x)` |
| `sort_array(x)` | → `list_sort(x)` | N/A |
| `size(x)` | → `len(x)` | → `length(x)` |

The Rust `FunctionRegistry::translate()` has no schema context. `reverse(array_col)` will
generate `reverse(array_col)` (the string function) instead of `list_reverse(array_col)`.

---

## Function registry size

| | Registrations | Source lines |
|---|---|---|
| Reference `FunctionRegistry.java` | ~303 | 1776 |
| Rust `functions/mod.rs` | ~296 | 1129 |

Counts are close but the reference covers JSON functions, extended string functions, and
edge-case spark compatibility mappings the Rust registry may be missing. A direct diff is
warranted before Phase 4.

---

## Spark Connect converter — Phase 3 targets

These `RelationConverter` handlers exist in the reference but have no Rust equivalent yet.
All are Phase 3 concerns (the gRPC converter layer does not exist yet):

| Relation type | DataFrame operation |
|---|---|
| `DROP_NA` | `df.dropna()` — filter rows with null values |
| `FILL_NA` | `df.fillna()` — replace nulls with a default |
| `REPLACE` | `df.replace()` — value substitution |
| `UNPIVOT` | `df.unpivot()` — wide-to-long transform |
| `TO_SCHEMA` | Cast DataFrame to a target schema |
| `DESCRIBE` | `df.describe()` — summary statistics |
| `SUMMARY` | `df.summary()` — extended statistics |
| `COV` / `CORR` | Covariance and correlation |

---

## Priority summary

| Item | Severity | Phase |
|---|---|---|
| `LikeExpression` | **Critical** | 3 |
| `SingleRowRelation` | **Critical** | 3 |
| `IntervalExpression` | **Critical** | 3 |
| Timezone hardcoded `'UTC'` | **Critical** | 3 |
| `preserve_insertion_order=true` | **Important** | 3 |
| `initcap` macro registration | **Important** | 3 |
| Polymorphic function resolution | **Important** | 3 |
| `ExtractValueExpression` | **Important** | 3 |
| `IsDistinctFromExpression` | **Important** | 3 |
| `RowConstructorExpression` | **Important** | 3 |
| Arrow schema fixup pass | **Important** | 3 |
| Spark Connect converter gaps | **Important** | 3 |
| `UpdateFieldsExpression` | Nice-to-have | 4 |
| `RegexColumnExpression` | Nice-to-have | 4 |
| `FieldAccessExpression` | Nice-to-have | 4 |
