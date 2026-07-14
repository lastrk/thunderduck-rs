# spark_count_if — resolution: NATIVE

**Verdict:** native. DuckDB's `COUNT_IF` matches Apache Spark 4.1.1 on all corpus-relevant
inputs; **no extension function written.**

- **RS emission mapping:** count_if(x) -> COUNT_IF(x)
- **Corpus unblocked:** agg2-006
- **Oracle:** Apache Spark 4.1.1 via `test/spark_oracle/verify_first.spark.sql`
  vs DuckDB `test/spark_oracle/verify_first.duckdb.sql`.
- **Parity evidence:** count_if=3 with a NULL row (NULL treated as FALSE) — Spark==DuckDB. Empty group -> 0 both.
- **Regression guard:** `test/sql/native_count_if_parity.test` (asserts the native
  function equals the captured Spark 4.1.1 golden; runs in `make test` on the
  pinned DuckDB v1.5.0).
