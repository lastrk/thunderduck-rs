# `spark_kurtosis` — DuckDB extension specification

**Status:** Pending, verify-first (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5, IF verification confirms divergence.
**Verify-first note:** DuckDB has native `KURTOSIS_POP` (and `KURTOSIS`, which is sample). Verify whether either matches Spark's `kurtosis` (which is *excess* kurtosis, population, Fisher's definition where normal distribution ≡ 0). If native matches, resolve this spec by wiring native; if not, implement `spark_kurtosis`.

## Function name

`spark_kurtosis` — aggregate function exported by `thdck_spark_funcs`. Resolved to a native DuckDB mapping if the verify-first check confirms parity.

## Spark equivalent

Spark's `kurtosis(col)` (`org.apache.spark.sql.catalyst.expressions.aggregate.Kurtosis`). **Excess kurtosis** using the population formula: for a series of `n` observations with mean `μ` and standard deviation `σ`, `kurtosis = (1/n) * sum((x - μ)^4 / σ^4) - 3`. Returns a `DOUBLE`. Nullable (returns NULL for empty groups; may return NULL for n < 4 depending on version).

## Signature

- Fixed arity: 1 argument.
- Aggregate.
- Input type: numeric (typically `DOUBLE`; also accepts `INT`, `BIGINT`, `FLOAT`, `DECIMAL`).
- Return type: `DOUBLE`, nullable.

## Semantic contract

Population excess kurtosis. NULLs skipped (not counted toward `n`). If the group is empty or has zero variance, the result is NULL (or NaN per Spark, tbd — verify).

## Corpus test cases unblocked

- `agg-009` (`skewness` + `kurtosis`) — the primary Slice D target. `spark_skewness` is already in ext4 (per CLAUDE.md); this spec covers the kurtosis half.

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/aggregate/CentralMomentAgg.scala` — the `Kurtosis` case class.
- Existing precedent in ext4: `spark_skewness(col)` — mirror its scaffolding (aggregate state, population-based moment calculation).
- Legacy `FunctionRegistry`: currently maps `kurtosis` → DuckDB `KURTOSIS_POP` (see `crates/core/src/functions/mod.rs` line ~423). This mapping is untested against Spark; that's what the verify-first check resolves.

## Dependencies

- Same aggregate-state / two-pass moment computation as `spark_skewness`. Sharing implementation infrastructure (variance calculation, centered-moment helpers) is encouraged.

## Testing notes

Verification-first checklist (extension session runs before deciding to implement):

```sql
-- Compare against Spark on the same data. Two implementations to test:
SELECT KURTOSIS_POP(salary) AS duckdb_pop, KURTOSIS(salary) AS duckdb_sample
FROM emp;

-- Spark reference:
-- SELECT kurtosis(salary) FROM emp
```

If `KURTOSIS_POP` matches Spark's `kurtosis` numerically (within tight tolerance across a range of distributions), close this spec as "wire native `KURTOSIS_POP`". If not, implement `spark_kurtosis` per the `spark_skewness` template.
