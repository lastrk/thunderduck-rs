# spark_try_divide — resolution: IMPLEMENTED

**Verdict:** implemented (scalar). Corpus unblocked: **math-016** (`try_divide(a, b)` on INT columns).

- **Where:** `src/thdck_spark_funcs_extension.cpp` — `SparkTryDivideDoubleExec` +
  `BindSparkTryDivide`; registered in `LoadInternal`. `null_handling = SPECIAL_HANDLING`.
- **Semantics (verified vs Spark 4.1.1, `test/spark_oracle/try_funcs.spark.sql`):**
  - Any DECIMAL operand → DECIMAL division, delegates to `BindSparkDecimalDiv` /
    `SparkDivExec` (Spark 4.1 precision rules; already returns NULL on zero).
    `try_divide(1.5, 0.5 :: dec(3,1))` → `3.000000` `DECIMAL(9,6)`.
  - All other numeric operands → **DOUBLE** division (Spark: int/int → double).
    `try_divide(10, 2)` → `5.0` (DOUBLE); `try_divide(7::BIGINT, 2::BIGINT)` → `3.5`.
  - Divisor `== 0` (incl. `0.0`) → NULL; NULL operand → NULL.
- **Test:** `test/sql/spark_try_divide.test` (real Spark 4.1.1 goldens).
- **RS emission mapping:** `try_divide(a, b)` → `spark_try_divide(a, b)`.
- **Note:** the spec guessed int/int → BIGINT; the Spark 4.1.1 oracle shows int/int
  and bigint/bigint → **DOUBLE**. Implemented to match the oracle, not the spec.
