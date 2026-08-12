# SQL Generation Correctness Rules

> **Status: current — cross-cutting SQL-generation invariants.** These constraints are echoed by [`cross-validation.md`](../cross-validation.md) and [ADR-001](../adr-001-transliterator-not-optimizer.md). Active ADR index: [`../README.md`](../README.md).

These constraints are inherited from the Java reference and are architecture-level invariants:

1. **All SQL and expression snippets must be built from the typed AST.** No string manipulation on SQL text outside of `to_sql()` implementations.
2. **No pre- or post-processing of SQL strings.** SQL built from the typed AST is never mutated after generation, and incoming raw SQL from `spark.sql(...)` is *parsed* into the plan representation (see [SparkSQL Parser Strategy](sparksql-parser.md) and rearchitect ADR-004) — never rewritten by text substitution. (The former `preprocess_spark_sql` text-rewrite pass has been removed.)
3. **`to_sql()` is for SQL generation only.** `Display` / `Debug` implementations are for human-readable debug output — never used to build SQL strings sent to DuckDB.
4. **Sealed plan + expression enums enforce exhaustiveness.** All new node types must be handled in `SqlGenerator` — the compiler enforces this.
5. **Type inference is centralised in `TypeInferenceEngine`.** No ad-hoc type guessing scattered through converters.

---

← [Back to ADR Index](../README.md)
