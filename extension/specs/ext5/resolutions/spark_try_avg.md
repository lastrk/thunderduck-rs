# spark_try_avg — resolution: IMPLEMENTED

**Verdict:** implemented (aggregate). Corpus unblocked: **agg2-004** (`try_avg(a)`, INT).

- **Where:** `src/include/spark_try_aggregates.hpp` — `CreateSparkTryAvgFunctionSet`
  (integer/float overloads → DOUBLE; DECIMAL overload reuses spark_avg); registered in `LoadInternal`.
- **Semantics (verified vs Spark 4.1.1):**
  - Integer/float input → **DOUBLE** average via double accumulation. Spark does
    **not** overflow-to-NULL for integer avg: `try_avg(bigint_max, bigint_max)` →
    `9.22e18` (DOUBLE), not NULL — matched (and equals native `AVG`).
    `try_avg(1,2,3)` → `2.0`.
  - DECIMAL input → reuses the ext4 `spark_avg` DECIMAL path (`GetSparkAvgDecimalFunction`
    + `BindSparkAvgDecimal`). An average never exceeds the input range, so Spark's
    overflow-to-NULL case is unreachable for avg — no extra guard needed.
  - NULLs skipped; empty group → NULL.
- **Test:** `test/sql/spark_try_avg.test` (real Spark 4.1.1 goldens).
- **RS emission mapping:** `try_avg(x)` → `spark_try_avg(x)`. (Integer `try_avg`
  also equals native `AVG`; RS may route either way for integer inputs.)
