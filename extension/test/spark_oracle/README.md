# Spark 4.1.1 parity oracle

Captures reference values from **real Apache Spark 4.1.1** so the extension's
`test/sql/*.test` goldens are validated against actual Spark output (not
approximations). This is the parity oracle referenced by the ext5 specs.

## Spark 4.1.1

Reuses the Spark install from the sibling transpiler repo:

```
SPARK_HOME=/Users/laszlo.torok/dev/thunderduck-rs/.spark/spark-4.1.1
```

If absent, install once with
`/Users/laszlo.torok/dev/thunderduck-rs/tests/scripts/setup-differential-testing.sh`
(pins Spark 4.1.1), or `pip install pyspark==4.1.1`.

Spark 4.1.x runs with **ANSI mode ON by default** — this matters: `try_*`
functions return NULL on error, but plain arithmetic / variance-normalizing
aggregates (`corr`, `regr_*`) **raise** `[DIVIDE_BY_ZERO]` at zero variance
instead of returning NULL/NaN. Capture with defaults to reflect Spark 4.1.1.

## Running

```bash
# DuckDB native values (instant):
duckdb -init /dev/null < test/spark_oracle/verify_first.duckdb.sql

# Spark 4.1.1 values (slow startup ~1 min; drop the two zero-variance cases,
# which raise ANSI DIVIDE_BY_ZERO, before running the batch):
"$SPARK_HOME/bin/spark-sql" --master 'local[1]' --conf spark.ui.enabled=false \
  -f test/spark_oracle/verify_first.spark.sql 2>/dev/null
```

Doubles are compared as `CAST(round(x, 10) AS VARCHAR)` so the SQLLogicTest
text match is exact (avoids `query R`'s 3-decimal rounding).

## Files
- `verify_first.duckdb.sql` / `verify_first.spark.sql` — the verify-first triage
  batch (native candidates for the 7 verify-first specs). Findings recorded in
  `specs/ext5/resolutions/*.md`.
- Add `<fn>.sql` pairs here when capturing goldens for newly-implemented
  functions (`spark_try_divide`, `spark_try_sum`, `spark_try_avg`).
