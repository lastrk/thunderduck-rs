# ADR-005 — thunderduck owns Spark type and nullability inference (the divergent slice), as a schema-threading analysis over the common AST

> **Amendment (2026-07-12, ADR-024):** the threaded analyzer schema is now the τ-owned `ResolvedSchema(Vec<Attribute>)` — each attribute carries its resolved type, nullability, stable `ExprId`, and source-qualifier lineage. `StructType` remains the wire/value type, produced at τ's public entry points.
>
> **Boundary (2026-08-10, ADR-026):** plan-ID lookup is a separate, protocol-specific structural rule. It consumes ADR-005's resolved attributes but does not expand the type/nullability lattice owned here.

**Status:** Proposed
**Depends on:** ADR-000, ADR-002 (defines the boundary), ADR-003 (the IR it annotates), ADR-004 (must serve both front-ends), ADR-012 (catalog seed)
**Depended on by:** ADR-006, ADR-007, ADR-009, ADR-010, ADR-012, ADR-014, ADR-015, ADR-017, ADR-018, ADR-024, ADR-025

**Context.** Every dispatch decision in τ keys on resolved Spark types (ADR-009). The common AST (ADR-003) arrives unresolved (`UnresolvedAttribute`, `UnresolvedRelation`) from both front-ends; types and nullability live in the catalog and in Spark's analyzer rules, not in the AST. DuckDB's native inference gives DuckDB types, which diverge from Spark. Per ADR-000, embedding real Catalyst (which would supply correct types) is rejected, so thunderduck must reimplement this slice.

**Decision.** thunderduck implements a schema-threading analysis `infer : (CommonAST, BaseTypes) → TypedAST` that propagates a Spark-typed schema through every operator, so operand types and nullability are known at every expression node, for plans from *either* front-end. Two named sub-units carry the Spark-specific weight: the **type-coercion lattice** (implicit casts, least-common-type, decimal precision/scale propagation) and the **nullability derivation** (outer-join null-extension, CASE/COALESCE, aggregate nullability). The pass *knows* the schema everywhere (it internally resolves references and expands `*` for type-tracking) even though it *emits* delegated structure (ADR-002). This ADR fixes the *scope* (what thunderduck owns); ADR-006 fixes the *structure* (how the analysis runs).

**Consequences.**
- (+) Makes the emission table correct: dispatch keys on Spark-accurate types. This is the precondition for everything downstream — it is foundational correctness, not a feature.
- (+) Confines the reimplemented analyzer to the minimal divergent slice (ADR-002), and serves SQL and DataFrame uniformly because both are the same common AST (ADR-003/004).
- (−) This is the largest and most correctness-critical component, and it must match *Spark's* analyzer specifically, not merely be internally consistent.
- (−) Per-operator schema-derivation rules (esp. outer-join nullability rewrite and aggregate nullability) are subtle and diverge from a naïve "just track types" implementation.
- (neutral) Commits to emit-level delegation, analysis-level ownership. The internal resolver/star-expander exists for schema derivation only and must never be removed on the grounds that resolution/star-expansion is delegated (Invariant INV5).

**Refinement hooks.** Decompose the coercion lattice and the nullability derivation as separately-testable named units. Validate against ground truth (ADR-015's AnalyzePlan schema diff) *before* the emitter has to be correct in concert. Highest-risk paths: a typed expression above a delegated, unexpanded structural construct (e.g. `amount * 1.1` over a starred join) — the pass must thread `amount`'s Spark type through internally. Extraction of the coercion/decimal/nullability rules from Spark sources may be LLM-accelerated, but no rule enters thunderduck until the AnalyzePlan diff is green for it (this is INV4 applied to rule provenance).

**Symmetric-omission discipline.** The analyzer's function-name enumerations must be kept internally consistent: any function name added to `aggregate_return_type` must also appear (or be justified absent) in `aggregate_is_non_nullable` / `aggregate_is_always_nullable`, and vice versa. Same rule applies for the SQL parser's `is_aggregate_function` classifier. Missing entries in one enumeration but present in another produce silent wrong types or nullability. This is a codified design constraint, not a lint — reviewers of any change touching these tables must confirm parallel updates.

---

