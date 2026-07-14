# spark_regr_r2 — resolution: NATIVE

**Verdict:** native. DuckDB's `REGR_R2` matches Apache Spark 4.1.1 on all corpus-relevant
inputs; **no extension function written.**

- **RS emission mapping:** regr_r2(y,x) -> REGR_R2(y, x)  (arg order y,x)
- **Corpus unblocked:** agg2-003
- **Oracle:** Apache Spark 4.1.1 via `test/spark_oracle/verify_first.spark.sql`
  vs DuckDB `test/spark_oracle/verify_first.duckdb.sql`.
- **Parity evidence:** regr_r2=0.8231292517 (Spark==DuckDB). REGR_R2 present in DuckDB.
- **Regression guard:** `test/sql/native_regr_r2_parity.test` (asserts the native
  function equals the captured Spark 4.1.1 golden; runs in `make test` on the
  pinned DuckDB v1.5.0).

**Divergence caveat (not corpus-hit):** at zero variance in a divisor column,
Spark 4.1.1 (ANSI on by default) raises `[DIVIDE_BY_ZERO]`, while DuckDB returns
`nan`. The corpus case runs on real `emp` data with non-zero variance, so this
edge is not exercised. If a future corpus case can hit zero variance, this must
be revisited (the native mapping would then diverge and require a wrapper).
