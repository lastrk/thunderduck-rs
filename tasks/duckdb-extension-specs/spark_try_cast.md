# `spark_try_cast` — DuckDB extension specification

**Status:** Pending (identified in the Slice D up-front audit, 2026-07-01).
**Target release:** `thdck_spark_funcs` ext5.
**Verify-first note:** DuckDB has native `TRY_CAST`; verify at implementation time whether Spark's `try_cast` semantics diverge (edge cases: overflow, malformed strings, timezone-adjacent types, decimal overflow). If they match, this spec can be closed with a "map to native `TRY_CAST`" resolution instead of a new extension function.

## Function name

`spark_try_cast` — scalar function exported by `thdck_spark_funcs`. If verification shows DuckDB's native `TRY_CAST` already matches Spark, this spec resolves to a Slice D emission-side wiring (map `try_cast(expr, T)` → `TRY_CAST(expr AS T)`) with NO new extension function needed.

## Spark equivalent

Spark 3.4+'s `try_cast(expr AS targetType)` (`org.apache.spark.sql.catalyst.expressions.TryCast`). Safely casts a value, returning `NULL` on any conversion failure (out-of-range, malformed input, incompatible type). Semantically stricter than `CAST` (which raises errors under ANSI mode).

## Signature

- Fixed arity: 2 arguments (value + target type marker).
- Scalar.
- Input types: (any type, target `LogicalType`). The target type is a compile-time / plan-time constant, not a runtime column — represented in Spark's plan as part of the expression tree, not as a data argument.
- Return type: the target type (always nullable).

**Implementation caveat.** DuckDB scalar functions don't naturally take a target type as a runtime argument. Options:
1. Register one `spark_try_cast_<type>` per target type (e.g. `spark_try_cast_int`, `spark_try_cast_string`) — coder-friendly at emission time but explodes the extension surface.
2. Compose the target type into the function invocation as a string literal argument: `spark_try_cast('123', 'INT')` — one function, but the extension parses the type string.
3. Use DuckDB's `TRY_CAST(expr AS type)` syntax natively if it matches Spark. **Preferred if it works.**

The verify-first checkpoint decides between these.

## Semantic contract

Behavior on invalid input:
- String → Int: `try_cast('abc' AS INT)` → `NULL`.
- Overflow: `try_cast(1e100 AS INT)` → `NULL` (Spark) vs. `NULL` (DuckDB, presumed — verify).
- Malformed date: `try_cast('not-a-date' AS DATE)` → `NULL`.
- `NULL` in → `NULL` out.
- Compatible types with valid values → the cast value.

Verify Spark parity on the ANSI edge cases listed in `org.apache.spark.sql.catalyst.expressions.Cast.canCast`.

## Corpus test cases unblocked

- `cast-012` (`try_cast(<bad string> AS int)` — the primary Slice D target).

## Reference implementation pointer

- Spark source: `sql/catalyst/src/main/scala/org/apache/spark/sql/catalyst/expressions/Cast.scala` — search `TryCast`.
- DuckDB reference: `TRY_CAST` in DuckDB core — if it's semantic-identical, this spec closes without new C++ work.
- Legacy `FunctionRegistry`: NOT present.

## Dependencies

- Depends on how DuckDB handles cast failures internally. If verify-first shows `TRY_CAST` matches, no C++ work needed; Slice D wires the emission mapping directly.

## Testing notes

Verification-first checklist (run before deciding to implement):

```sql
-- Do these two produce identical results (row-by-row + types) against reference Spark?
SELECT TRY_CAST('abc' AS INT), TRY_CAST('123' AS INT), TRY_CAST(NULL AS INT),
       TRY_CAST(1e100 AS INT), TRY_CAST('not-a-date' AS DATE);
```

If yes: spec resolves to "wire native `TRY_CAST`" in Slice D emission; no ext5 work needed for this function.

If no: implement `spark_try_cast` per the option-1 or option-2 pattern above.
