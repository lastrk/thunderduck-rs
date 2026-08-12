# ADR-002 — Emit-level delegation: own only the slice where Spark diverges from DuckDB

**Status:** Proposed
**Depends on:** ADR-000, the DuckDB-correctness assumption (formalized as LB2 in §CV)
**Depended on by:** ADR-003, ADR-005, ADR-007, ADR-013, ADR-026

**Context.** Resolution work (name binding, scope resolution, star expansion, column-to-table association, type inference, nullability) could be reimplemented in thunderduck or delegated to DuckDB's binder. DuckDB's binder is correct for dialect-agnostic SQL semantics; its *type* and *nullability* answers are not Spark's. The positioning (ADR-000) pushes toward delegating whatever DuckDB already does correctly, to keep the owned surface minimal.

**Decision.** thunderduck delegates structural resolution to DuckDB at the *emission* level — it emits `SELECT *`, emits unqualified/qualified names, and lets DuckDB's binder expand and resolve — and reimplements only the slice where Spark's semantics diverge observably from DuckDB's. That divergent slice is type inference and nullability (ADR-005).

**Amendment (2026-08-10, ADR-026).** Spark Connect DataFrame-reference `plan_id` lookup is one bounded structural-resolution exception because DuckDB has no Connect plan tree or error contract to delegate to. τ mirrors Catalyst for that lookup only; ordinary SQL/name binding remains delegated at emission.

**Consequences.**
- (+) Every piece of resolution thunderduck doesn't do is a piece that can't diverge and needs no maintenance.
- (+) Minimal analyzer surface: thunderduck reimplements the *smallest* slice that achieves parity — and justifies keeping the IR (ADR-003) short of full Catalyst, since the resolution machinery a full LogicalPlan would carry is delegated.
- (−) The delegation boundary is clean only where the delegated (structural) result is not an *input* to the owned (semantic) computation — and type inference *does* depend on resolved structure (Tension T1).
- (neutral) "Wherever DuckDB's semantics already match" is an assumption that must be validated empirically, not asserted (Load-bearing LB3, validated via ADR-015's AnalyzePlan diff).

**Refinement hooks.** The exact membership of "the divergent slice" is the keystone of the whole architecture (LB1). It is {type inference, nullability} plus ADR-026's enumerated Connect plan-ID exception. Any further structural divergence requires another explicit boundary amendment.

**Delegation boundary at v2's edge.** Where v2 does not delegate (owns) and does not implement, τ produces a typed error rather than emit partial or synthetic SQL. Two categories: **Spark-emulated errors** (Spark itself would reject the input; τ matches Spark's error semantics) and **Thunderduck-boundary errors** (Spark accepts the input but τ has not implemented it — honest "not implemented in Thunderduck," where Thunderduck-specificity leaks through the Spark Connect facade deliberately). See ADR-022 for the full contract.

---

