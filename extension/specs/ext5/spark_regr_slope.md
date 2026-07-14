# `spark_regr_slope` — DuckDB extension specification

**Status:** Pending, verify-first (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5.
**Verify-first note:** DuckDB has `REGR_SLOPE` in newer versions (verify against the pinned build). If native matches Spark, resolve to native wiring.

## Function name

`spark_regr_slope` — aggregate exported by `thdck_spark_funcs` IF native REGR_SLOPE unavailable or diverges.

## Spark equivalent

Spark 3.5+'s `regr_slope(y, x)` (`org.apache.spark.sql.catalyst.expressions.aggregate.RegrSlope`). Slope of the ordinary least-squares (OLS) regression line `y = slope * x + intercept`. Formula: `slope = covar_pop(y, x) / var_pop(x)`.

Arg order: **`(y, x)`** — dependent first, independent second (SQL standard).

## Signature

- Fixed arity: 2 arguments `(dependent, independent)`.
- Aggregate.
- Input types: `(numeric, numeric)`.
- Return type: `DOUBLE`, nullable.

## Semantic contract

Population-based OLS slope. NULLs in either arg skip the row. Empty group or zero variance in `x` → NULL.

## Corpus test cases unblocked

- `agg2-003` — regression aggregates (paired with `spark_regr_r2`).

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/linearRegression.scala`.
- DuckDB reference: `REGR_SLOPE(y, x)` — verify presence in the pinned DuckDB build (may be relatively recent).
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Paired-sums aggregate state; may share primitives with `spark_covar_samp` / `spark_corr` (all three compute similar sums).

## Testing notes

```sql
SELECT REGR_SLOPE(salary, age) FROM emp;
-- Spark: SELECT regr_slope(salary, age) FROM emp
```
