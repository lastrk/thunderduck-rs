# spark_try_sum — resolution: IMPLEMENTED

**Verdict:** implemented (aggregate). Corpus unblocked: **agg2-004** (`try_sum(lng)`, BIGINT).

- **Where:** `src/include/spark_try_aggregates.hpp` — `CreateSparkTrySumFunctionSet`
  (integer overloads TINYINT..BIGINT → BIGINT; DECIMAL overload); registered in `LoadInternal`.
- **Semantics (verified vs Spark 4.1.1):**
  - Integer input → BIGINT; accumulates `int64_t` with `__builtin_add_overflow` /
    `__builtin_mul_overflow`; **overflow → NULL**. `try_sum(1,2,3)` → 6;
    `try_sum(bigint_max, bigint_max)` → NULL.
  - DECIMAL input → `DECIMAL(min(p+10,38), s)` (`ComputeSumType`); accumulates
    `__int128`; **NULL if `|sum| ≥ 10^result_precision`**. `try_sum(1.5,2.5::dec(3,1))`
    → `4.0` `DECIMAL(13,1)`.
  - NULLs skipped; empty group → NULL.
- **Why not native:** DuckDB `SUM(BIGINT)` widens to HUGEINT (no overflow), so it
  diverges from Spark's BIGINT + overflow→NULL. A dedicated function is required.
- **Test:** `test/sql/spark_try_sum.test` (real Spark 4.1.1 goldens).
- **RS emission mapping:** `try_sum(x)` → `spark_try_sum(x)`.
