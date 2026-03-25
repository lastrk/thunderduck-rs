# ADR-19: SQL Generation Correctness Rules (Non-Negotiable)

These constraints are inherited from the Java reference and are architecture-level invariants:

1. **All SQL and expression snippets must be built from the typed AST.** No string manipulation on SQL text outside of `to_sql()` implementations. *(Exception: `preprocess_spark_sql` — see ADR-10.)*
2. **No post-processing of generated SQL strings.** SQL built from the typed AST (DataFrame path) is never mutated after generation. Pre-processing of *incoming* raw SQL strings (the `spark.sql()` pass-through path) is the narrow exception carved out in ADR-10.
3. **`to_sql()` is for SQL generation only.** `Display` / `Debug` implementations are for human-readable debug output — never used to build SQL strings sent to DuckDB.
4. **Sealed plan + expression enums enforce exhaustiveness.** All new node types must be handled in `SqlGenerator` — the compiler enforces this.
5. **Type inference is centralised in `TypeInferenceEngine`.** No ad-hoc type guessing scattered through converters.

---

← [Back to Architecture Overview](../architecture.md)
