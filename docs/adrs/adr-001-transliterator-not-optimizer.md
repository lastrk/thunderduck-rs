# ADR-001 — τ is a transliterator, not an optimizer

**Status:** Proposed
**Depends on:** ADR-000
**Depended on by:** ADR-007, ADR-008, ADR-011, ADR-015, ADR-026

**Context.** The same query result can be produced by many query plans. A translator could either emit the plan as given and let the engine optimize, or perform its own cost-driven rewrites. thunderduck sits in front of DuckDB, whose optimizer is assumed competent (ADR-014 assumption), and the positioning (ADR-000) is to lean on DuckDB's engine rather than rebuild planning.

**Decision.** τ performs no cost-driven transformations: no predicate pushdown, no join reordering, no constant folding, no common-subexpression elimination, no decorrelation-for-efficiency. It changes operator structure when forced by expressibility (a Spark DataFrame operator has no direct DuckDB surface form), and it *may* additionally perform **result-irrelevant cosmetic simplifications** — transformations that produce strictly less SQL (fewer nodes; no operator reordering; no change to what DuckDB's optimizer can subsequently see) and that DuckDB would discard anyway. All cost-driven optimization is delegated to DuckDB.

So the permitted transformations fall in three categories: *expressibility-forced* (required), *cosmetic* (optional, result-irrelevant, strictly-reducing, syntactic), and *enumerated correctness-forcing carve-outs* (rare; see Refinement hooks). Everything cost-motivated is forbidden.

**Guardrail for cosmetic simplifications.** A cosmetic simplification must be (a) provably result-irrelevant, (b) strictly node-reducing, (c) purely *syntactic* — operating on the SQL/plan shape without consulting cost or statistics — and (d) non-reordering (it may not move an operator relative to another, since that is the optimizer's job). Qualifying examples: nested-alias elimination, collapsing `SELECT *` over `SELECT *`, redundant-parenthesis removal. Disqualifying: anything that relocates an operator (predicate pushdown shapes, projection pruning that changes what reaches the optimizer).

A Connect node carrying `plan_id` is not cosmetic: ADR-026 makes that boundary observable during reference resolution even when removing it would leave equivalent SQL.

**Consequences.**
- (+) Reduces τ to a near-mechanical mapping whose decisions are mostly node-local, making them enumerable and testable (ADR-009, ADR-014).
- (+) Cosmetic simplification improves emitted-SQL readability and snapshot-test stability without affecting results.
- (+) The engine assumed correct does the cost-driven work it is best at.
- (−) Emitted SQL may still be naïve in shape; correctness depends on DuckDB's optimizer actually handling it well (acceptable under the ADR-014 assumption).
- (−) The bright line is now three clauses rather than one (forbidden cost-driven; permitted cosmetic; enumerated carve-outs), so the cosmetic set and the carve-out set must each be enumerable and reviewed, or ADR-001 erodes silently.
- (neutral) Forces a discipline: every structural transformation must be justified as expressibility-forced, result-irrelevant-cosmetic, or a recorded carve-out — never "this is faster."

**Refinement hooks.** A narrow **carve-out** is permitted for transformations that are *correctness-forcing yet optimization-shaped* — where DuckDB produces a wrong-vs-Spark *result* (not merely a slower one) without the transformation. Each carve-out requires: (a) a written justification that it is necessary for correctness, (b) confirmation that no expressibility-forced or cosmetic framing achieves the same end, and (c) recording as a *named exception in this ADR*, so the carve-out set is enumerable and reviewable (the same discipline as the Option-C escape hatches in ADR-007). If the carve-out set grows past a handful, that is a signal the transliterator framing is wrong and this ADR should be revisited. (See Tension T2.) As the problem structure becomes better understood, expect this ADR to be revised.

*Carve-out register (currently empty):* — none.

---

