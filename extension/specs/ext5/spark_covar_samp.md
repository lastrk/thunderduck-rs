# `spark_covar_samp` — DuckDB extension specification

**Status:** Pending, verify-first (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5, IF verification confirms divergence.
**Verify-first note:** DuckDB has native `COVAR_SAMP` (and `COVAR_POP`). Verify sample-vs-population + NULL semantics.

## Function name

`spark_covar_samp` — aggregate exported by `thdck_spark_funcs` IF DuckDB's native `COVAR_SAMP` diverges from Spark.

## Spark equivalent

Spark's `covar_samp(x, y)` (`org.apache.spark.sql.catalyst.expressions.aggregate.CovSample`). Sample covariance: `sum((x - mean(x)) * (y - mean(y))) / (n - 1)`. Also exists as `covar_pop` (population, divides by `n`).

## Signature

- Fixed arity: 2 arguments.
- Aggregate.
- Input types: `(numeric, numeric)`.
- Return type: `DOUBLE`, nullable.

## Semantic contract

Sample covariance. NULLs in either arg skip the row. Empty or single-element group → NULL (n-1 = 0).

## Corpus test cases unblocked

- `agg-012` (paired with `spark_corr` above).

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/Covariance.scala`.
- DuckDB reference: `COVAR_SAMP(x, y)`.
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Paired-sums aggregate state (x, y, xy) + count. Same primitives as `spark_corr`.

## Testing notes

```sql
SELECT COVAR_SAMP(age, salary) FROM emp;
-- Spark: SELECT covar_samp(age, salary) FROM emp
```

Verify on the same distributions as `spark_corr`. If DuckDB parity holds, wire native.
