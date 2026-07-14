# `spark_regr_r2` — DuckDB extension specification

**Status:** Pending, verify-first (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5.
**Verify-first note:** DuckDB has `REGR_R2` in newer versions. Verify against the pinned build.

## Function name

`spark_regr_r2` — aggregate exported by `thdck_spark_funcs` IF native REGR_R2 unavailable or diverges.

## Spark equivalent

Spark 3.5+'s `regr_r2(y, x)` (`org.apache.spark.sql.catalyst.expressions.aggregate.RegrR2`). Coefficient of determination (R²) for the OLS regression `y = slope * x + intercept`. Formula: `r2 = corr(y, x)^2` when x is not constant; else NULL. Range: [0, 1].

Arg order: **`(y, x)`**.

## Signature

- Fixed arity: 2 arguments.
- Aggregate.
- Input types: `(numeric, numeric)`.
- Return type: `DOUBLE`, nullable.

## Semantic contract

R² per Spark's regression formula. NULLs skip. Empty group / constant x → NULL.

## Corpus test cases unblocked

- `agg2-003` (paired with `spark_regr_slope`).

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/linearRegression.scala`.
- DuckDB reference: `REGR_R2(y, x)`.
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Same paired-sums state as `spark_regr_slope`; shares infrastructure.

## Testing notes

```sql
SELECT REGR_R2(salary, age) FROM emp;
-- Spark: SELECT regr_r2(salary, age) FROM emp
```
