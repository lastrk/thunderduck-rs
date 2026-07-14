# spark_try_cast — resolution: NATIVE

**Verdict:** native. DuckDB's `TRY_CAST` matches Apache Spark 4.1.1 on all corpus-relevant
inputs; **no extension function written.**

- **RS emission mapping:** try_cast(e AS T) -> TRY_CAST(e AS T)   (emission-side; no extension function)
- **Corpus unblocked:** cast-012
- **Oracle:** Apache Spark 4.1.1 via `test/spark_oracle/verify_first.spark.sql`
  vs DuckDB `test/spark_oracle/verify_first.duckdb.sql`.
- **Parity evidence:** bad string / bigint->int overflow / malformed date / NULL -> NULL; '123'->123. Spark==DuckDB on all 6 probes.
- **Regression guard:** `test/sql/native_try_cast_parity.test` (asserts the native
  function equals the captured Spark 4.1.1 golden; runs in `make test` on the
  pinned DuckDB v1.5.0).
