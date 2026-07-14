# Spark Specification Lookup

> **Scope: τ (the only production path per ADR-022).** How to determine "correct"
> when a task involves Spark compatibility — type inference, nullability, decimal
> precision, function behavior, schema propagation, or error semantics.

The authoritative specification is **Apache Spark 4.1.1 in ANSI mode**
(`spark.sql.ansi.enabled=true`, per ADR-016).

- **Spark source is authoritative.** Use `WebSearch` / `WebFetch` on the `apache/spark` GitHub repo for the relevant source (`DecimalPrecision.scala`, `TypeCoercion.scala`, `HiveResult.scala`, `ArithmeticExpression.scala`, `UpdateFields.scala`, `higherOrderFunctions.scala`, etc.). Spark's behavior in these files defines "correct" for τ. Where a quick empirical check is cheaper, the vendored Spark at `/workspace/.spark/spark-4.1.1` can be run directly.
- **`.reference/` is the Java Thunderduck implementation** (only if present in the working copy). When looking there for equivalent behaviour, note that its structure and function boundaries may differ from τ's — use it as a Spark-parity cross-check, not a template.
- **Spark parity wins over DuckDB-native ergonomics** (ADR-015). If DuckDB offers a shorter emission that changes Spark's observable behavior (return type, nullability, error class, precision, sort order), don't take the shortcut.
- **ANSI-mode error semantics matter.** Division / mod by zero, `element_at` OOB, cast overflow, and `to_number` format mismatches THROW in ANSI mode (see ADR-016 error-emulation contract). τ must re-wrap DuckDB engine throws with Spark's error class before crossing the wire — never surface an opaque DuckDB error string.

See also `dependencies.md` (the `thdck_spark_funcs` extension implements the
Spark-precise numerical semantics DuckDB lacks) and `architecture.md` (Spark
Parity Requirements — the exact-match contract the differential oracle checks).
