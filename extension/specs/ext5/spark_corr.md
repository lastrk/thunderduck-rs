# `spark_corr` — DuckDB extension specification

**Status:** Pending, verify-first (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5, IF verification confirms divergence.
**Verify-first note:** DuckDB has native `CORR`. Verify whether it matches Spark's `corr` (Pearson correlation, sample-based, NULL handling on empty groups / zero variance). If native matches, resolve to native wiring.

## Function name

`spark_corr` — aggregate exported by `thdck_spark_funcs`, IF DuckDB's native `CORR` diverges from Spark.

## Spark equivalent

Spark's `corr(x, y)` (`org.apache.spark.sql.catalyst.expressions.aggregate.Corr`). Pearson correlation coefficient. Formula: `Corr(x,y) = Cov(x,y) / (StdDev(x) * StdDev(y))`. Sample-based (divides by `n-1` in the covariance and stddev).

## Signature

- Fixed arity: 2 arguments.
- Aggregate.
- Input types: `(numeric, numeric)`.
- Return type: `DOUBLE`, nullable.

## Semantic contract

Sample Pearson correlation. NULLs in either argument skip the row. Empty group → NULL. Zero-variance in either input → NULL (or NaN — verify Spark behavior on this edge case).

## Corpus test cases unblocked

- `agg-012` (`corr(x, y)` + `covar_samp(x, y)`) — the primary Slice D target. This spec covers the correlation half; `spark_covar_samp.md` covers the covariance half.

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/Corr.scala`.
- DuckDB reference: `CORR(x, y)` — verify semantics match.
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Aggregate state: paired sums (x, y, x², y², xy) and count. Same primitives as `spark_covar_samp`.

## Testing notes

Verification-first checklist:

```sql
SELECT CORR(age, salary) AS duckdb_corr FROM emp;
-- Spark: SELECT corr(age, salary) FROM emp
```

Compare on multiple distributions (linear, non-linear, constant, one-null). If numerically identical across all, resolve this spec as "wire native `CORR`". If any divergence, implement `spark_corr` per the paired-sums pattern.
