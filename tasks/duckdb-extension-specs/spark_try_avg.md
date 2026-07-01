# `spark_try_avg` — DuckDB extension specification

**Status:** Pending (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5.

## Function name

`spark_try_avg` — aggregate exported by `thdck_spark_funcs`.

## Spark equivalent

Spark 3.5+'s `try_avg(col)` (`org.apache.spark.sql.catalyst.expressions.aggregate.TryAverage`). Overflow-safe average: identical to `spark_avg` under non-overflow conditions; returns `NULL` on overflow instead of raising ANSI error.

## Signature

- Fixed arity: 1 argument.
- Aggregate.
- Input type: numeric.
- Return type: same as `spark_avg(col)` (already in ext4) — DOUBLE for integer/float inputs, DECIMAL with appropriate precision for DECIMAL inputs. Always nullable.

## Semantic contract

Same as `spark_avg(col)` under non-overflow. On accumulator overflow, return `NULL`. NULLs in input skip normally. Empty group → NULL.

## Corpus test cases unblocked

- `agg2-004` (paired with `spark_try_sum`).

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/Average.scala` — search `TryAverage`.
- Existing ext4 precedent: `spark_avg(col)` — extend with an overflow flag.
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Composes with `spark_avg`. Reuse `spark_try_sum`'s overflow-detection pattern.

## Testing notes

```sql
SELECT spark_try_avg(lng) FROM nums_at_bigint_max;   -- expect NULL
SELECT spark_try_avg(lng) FROM nums;                 -- expect the normal avg
```
