# SQL Generation Correctness Rules

> **Status: current — cross-cutting SQL-generation invariants.** These constraints govern the legacy path and are echoed, for v2, by the spine's cross-cutting invariants **INV1–INV10** (esp. INV2 node-locality, INV3 single-source-of-truth) and ADR-001. ADR index: [`../README.md`](../README.md) · v2 spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

These constraints are inherited from the Java reference and are architecture-level invariants:

1. **All SQL and expression snippets must be built from the typed AST.** No string manipulation on SQL text outside of `to_sql()` implementations.
2. **No pre- or post-processing of SQL strings.** SQL built from the typed AST is never mutated after generation, and incoming raw SQL from `spark.sql(...)` is *parsed* into the plan representation (see [SparkSQL Parser Strategy](sparksql-parser.md) and rearchitect ADR-004) — never rewritten by text substitution. (The former `preprocess_spark_sql` text-rewrite pass has been removed.)
3. **`to_sql()` is for SQL generation only.** `Display` / `Debug` implementations are for human-readable debug output — never used to build SQL strings sent to DuckDB.
4. **Sealed plan + expression enums enforce exhaustiveness.** All new node types must be handled in `SqlGenerator` — the compiler enforces this.
5. **Type inference is centralised in `TypeInferenceEngine`.** No ad-hoc type guessing scattered through converters.

---

← [Back to ADR Index](../README.md)
