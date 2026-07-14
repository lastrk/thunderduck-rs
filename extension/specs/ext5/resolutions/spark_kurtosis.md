# spark_kurtosis — resolution: NATIVE

**Verdict:** native. DuckDB's `KURTOSIS_POP` matches Apache Spark 4.1.1 on all corpus-relevant
inputs; **no extension function written.**

- **RS emission mapping:** kurtosis(c) -> KURTOSIS_POP(c)   (NOT DuckDB kurtosis(), which is sample)
- **Corpus unblocked:** agg-009
- **Oracle:** Apache Spark 4.1.1 via `test/spark_oracle/verify_first.spark.sql`
  vs DuckDB `test/spark_oracle/verify_first.duckdb.sql`.
- **Parity evidence:** Spark kurtosis()=-1.2242424242 == DuckDB kurtosis_pop()=-1.2242424242 (excess population). DuckDB kurtosis() (sample) = -1.2 diverges.
- **Regression guard:** `test/sql/native_kurtosis_parity.test` (asserts the native
  function equals the captured Spark 4.1.1 golden; runs in `make test` on the
  pinned DuckDB v1.5.0).
