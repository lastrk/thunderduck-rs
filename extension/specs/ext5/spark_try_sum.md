# `spark_try_sum` — DuckDB extension specification

**Status:** Pending (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5.

## Function name

`spark_try_sum` — aggregate exported by `thdck_spark_funcs`.

## Spark equivalent

Spark 3.5+'s `try_sum(col)` (`org.apache.spark.sql.catalyst.expressions.aggregate.TrySum`). Overflow-safe sum: if arithmetic overflow occurs during accumulation, return `NULL` instead of wrapping or raising an ANSI error.

## Signature

- Fixed arity: 1 argument.
- Aggregate.
- Input type: numeric (INT, BIGINT, DECIMAL, FLOAT, DOUBLE).
- Return type: same as `spark_sum(col)` (which already exists in ext4) — BIGINT for integer inputs, DECIMAL with appropriate precision for DECIMAL inputs, DOUBLE for float inputs. Always nullable.

## Semantic contract

Same behavior as `spark_sum(col)` under non-overflow conditions. On overflow (accumulated value exceeds the return type's max/min), the result is `NULL`. NULLs in input skip normally.

## Corpus test cases unblocked

- `agg2-004` (`try_sum`, `try_avg`).

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/Sum.scala` — search `TrySum`.
- Existing ext4 precedent: `spark_sum(col)` — its overflow-detection state can be extended with a "return-null-on-overflow" flag.
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Depends on / composes with `spark_sum` (already in ext4). Recommended: share the underlying accumulator; add an overflow-check that flips a NULL flag.

## Testing notes

```sql
-- On values where non-try version overflows:
SELECT spark_try_sum(lng) FROM nums_at_bigint_max;   -- expect NULL
SELECT spark_try_sum(lng) FROM nums;                 -- expect the normal sum
```
