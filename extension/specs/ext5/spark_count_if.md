# `spark_count_if` — DuckDB extension specification

**Status:** Pending, verify-first (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5, IF verification confirms divergence.
**Verify-first note:** DuckDB has native `COUNT_IF(bool_expr)`. Verify whether it matches Spark's `count_if` NULL-handling (Spark treats `NULL` as `FALSE`; DuckDB may treat it as unknown). If native matches, resolve to native wiring.

## Function name

`spark_count_if` — aggregate exported by `thdck_spark_funcs` IF DuckDB's native `COUNT_IF` diverges from Spark.

## Spark equivalent

Spark's `count_if(expr)` (`org.apache.spark.sql.catalyst.expressions.aggregate.CountIf`). Counts rows where the boolean expression evaluates to `TRUE`. **`NULL` is treated as `FALSE`** (i.e., NULL rows are NOT counted, same as if the row had `FALSE`).

## Signature

- Fixed arity: 1 argument.
- Aggregate.
- Input type: `BOOLEAN` (or nullable boolean).
- Return type: `BIGINT`, **non-nullable** (count-like functions in Spark return non-nullable Long).

## Semantic contract

`count_if(x)` = `count(*) filter (where x is true)` in Spark semantics. Verify DuckDB's `COUNT_IF` uses the same convention. Empty group → 0 (not NULL — this is a count-like function).

## Corpus test cases unblocked

- `agg2-006` (`count_if`, plus filtered `avg`).

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/Count.scala` — search `CountIf`.
- DuckDB reference: `COUNT_IF(bool)`.
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Trivial aggregate state (a single counter). No shared infrastructure needed.

## Testing notes

Verification-first checklist:

```sql
-- Test with NULLs to distinguish Spark (NULL = FALSE) from ambiguous:
SELECT COUNT_IF(active) FROM emp;
-- Spark: SELECT count_if(active) FROM emp

-- Also with NULLs explicitly:
SELECT COUNT_IF(salary > 90000) FROM emp;
```

If DuckDB's `COUNT_IF` handles `NULL` as Spark does (NULL = FALSE, i.e., row is not counted), close this spec as "wire native `COUNT_IF`". Otherwise implement `spark_count_if` per the trivial-counter pattern.
