# `spark_try_divide` — DuckDB extension specification

**Status:** Pending (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5 (or later).

## Function name

`spark_try_divide` — a scalar function exported by the `thdck_spark_funcs` DuckDB extension.

## Spark equivalent

Spark 3.5+'s `try_divide(dividend, divisor)` (`org.apache.spark.sql.catalyst.expressions.TryDivide`). Returns `NULL` when `divisor == 0` (or otherwise indicates division would overflow / error), instead of raising an ANSI error or wrapping. Differs from a plain `dividend / divisor` (which errors in Spark ANSI mode on division-by-zero) and from DuckDB's `TRY(a/b)` (which has different null-vs-error semantics).

## Signature

- Fixed arity: 2 arguments.
- Scalar (not aggregate).
- Input types: `(numeric, numeric)`. Numeric = any of `TINYINT`, `SMALLINT`, `INTEGER`, `BIGINT`, `FLOAT`, `DOUBLE`, `DECIMAL(p,s)`.
- Return type: Spark's division result type per Spark's promotion rules:
  - `int / int` → `BIGINT` (nullable).
  - `float / float` → `FLOAT` (nullable).
  - `decimal(p1,s1) / decimal(p2,s2)` → `DECIMAL(min(38, ...), ...)` per Spark's decimal division rule (see `spark_decimal_div` in `ext4` for the existing decimal-division precedent).
  - Mixed → `DOUBLE` (nullable).
- Always nullable (returns NULL on divisor == 0).

## Semantic contract

Behavior: if `divisor == 0` (numerically, including `0.0`, `-0.0`, and zero-valued Decimal), return `NULL`. Otherwise return `dividend / divisor` using Spark's type-promotion rule for the division. `NULL` in either operand propagates to `NULL` (standard SQL semantics). Overflow is NOT the concern here (Spark's `try_divide` is specifically for division-by-zero); overflow-safe arithmetic belongs to `spark_try_sum` / `spark_try_avg`.

## Corpus test cases unblocked

- `math-016` (`try_divide(a, b)`) — the primary Slice D target.
- Potentially indirect cases in later slices using `try_divide` inside `chain-*` compositions.

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/arithmetic.scala` — search `TryDivide`.
- Legacy `crate::functions::FunctionRegistry`: NOT present today (falls through to unresolved).
- Existing pattern: `spark_decimal_div` (already in ext4) is the closest analogue — it wraps a division with Spark-specific rounding for the DECIMAL case. `spark_try_divide` extends the pattern with the additional divisor==0 check.

## Dependencies

- No dependency on other `spark_*` functions.
- DuckDB internals: standard scalar function template (single row → single row). For the DECIMAL branch, may want to compose with `spark_decimal_div`'s logic to avoid duplicating precision/scale handling.

## Testing notes

Minimal SQL exercising the function once implemented (extension session runs this against a local DuckDB build with the extension loaded):

```sql
SELECT spark_try_divide(10, 2)          AS normal_int,    -- 5 (BIGINT)
       spark_try_divide(10, 0)          AS zero_div_int,  -- NULL
       spark_try_divide(10.0, 0.0)      AS zero_div_dbl,  -- NULL
       spark_try_divide(NULL::INT, 2)   AS null_num,      -- NULL
       spark_try_divide(10, NULL::INT)  AS null_den,      -- NULL
       spark_try_divide(1.5::DECIMAL(3,1), 0.5::DECIMAL(3,1)) AS dec_div;  -- 3.0 (DECIMAL)
```

Differential check against Spark 4.1.1:
- Spin up a Spark session with the same values, run `SELECT try_divide(...)`.
- Row-by-row match required (types AND values).
