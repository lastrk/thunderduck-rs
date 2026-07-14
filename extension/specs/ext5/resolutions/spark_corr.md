# spark_corr — resolution: NATIVE

**Verdict:** native. DuckDB's `CORR` matches Apache Spark 4.1.1 on all corpus-relevant
inputs; **no extension function written.**

- **RS emission mapping:** corr(x,y) -> CORR(x, y)
- **Corpus unblocked:** agg-012
- **Oracle:** Apache Spark 4.1.1 via `test/spark_oracle/verify_first.spark.sql`
  vs DuckDB `test/spark_oracle/verify_first.duckdb.sql`.
- **Parity evidence:** corr=0.9072647087 (Spark==DuckDB). Empty group -> NULL both.
- **Regression guard:** `test/sql/native_spark_corr_parity.test` (asserts the native
  function equals the captured Spark 4.1.1 golden; runs in `make test` on the
  pinned DuckDB v1.5.0).

**Divergence caveat (not corpus-hit):** at zero variance in a divisor column,
Spark 4.1.1 (ANSI on by default) raises `[DIVIDE_BY_ZERO]`, while DuckDB returns
`nan`. The corpus case runs on real `emp` data with non-zero variance, so this
edge is not exercised. If a future corpus case can hit zero variance, this must
be revisited (the native mapping would then diverge and require a wrapper).
