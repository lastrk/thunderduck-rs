# ADR-006 — The analyzer is a bounded sequence of coordinated passes, not an iterate-to-fixed-point engine

> **Amendment (2026-07-12, ADR-024):** the resolve pass additionally mints/propagates attribute `ExprId`s and stamps them onto resolved references alongside ordinals. The 0/1/2+ match-count error semantics are unchanged.

**Status:** Proposed
**Depends on:** ADR-000, ADR-005
**Depended on by:** ADR-007, ADR-024, ADR-026

**Context.** Spark's Catalyst analyzer applies rules in `FixedPoint` batches — it re-runs each batch until the tree stops changing — because Catalyst's rules are deliberately small, self-contained, and uncoordinated, so global behavior emerges only from iteration, and because the optimizer keeps mutating the tree (requiring re-analysis). thunderduck does **neither** of those things: it does not optimize (ADR-001), so there is no optimization-churn re-analysis; and it can write analysis as a single coordinated pass rather than uncoordinated re-scanned rules. The question is therefore which of Catalyst's fixed-points are *essential to the analysis* versus *artifacts of Catalyst's architecture*.

**Decision.** thunderduck implements the analyzer (ADR-005's `infer`) as a **bounded, known sequence of coordinated passes**, not an open-ended iterate-to-fixed-point loop. Most facts flow bottom-up. The explicit non-upward cases are set-operation widening (across siblings, then down), correlated-subquery scoping (outside-in), aggregate/HAVING/ORDER-BY alias dependencies (sideways), and ADR-026 plan-ID lookup (top-down search followed by ancestor-output filtering after child `ExprId`s exist). The pass structure mirrors each rule's information flow, not Catalyst's fixed-point mechanism.

**Consequences.**
- (+) A bounded, coordinated-pass analyzer is far simpler and more analyzable than Catalyst's iterate-to-fixed-point engine, and the number of passes is known a priori, not "run until nothing changes."
- (+) Eliminating optimization-churn re-analysis (ADR-001) removes an entire class of Catalyst's iteration reasons that simply do not apply to thunderduck.
- (−) Getting a rule's information-flow direction wrong (treating a set-op-widening or correlated case as if it were upward-only) produces wrong types/nullability — a real bug class.
- (−) The analyzer is a single coordinated pass only for the upward-flowing majority; each named non-upward rule requires explicit staging or traversal.
- (neutral) When extracting Spark's rules (ADR-005), the rule *content* (formulas, least-common-type table, nullability derivations) transfers, but Catalyst's iteration *mechanism* is deliberately not transferred; what must be preserved is the flow direction each rule needs.

**Refinement hooks.** Enumerate the pass sequence explicitly and classify each rule's information flow. Any new non-upward rule requires an ADR amendment. The AnalyzePlan differential (ADR-015) is the backstop for a wrongly modeled direction.

**Set-op widened schema wins at emission time.** The downward set-op sub-sweep (UNION / INTERSECT / EXCEPT) produces a widened schema on the set-op node. At emission time, that widened schema wins over any child projection's declared cast target. Concretely, if one child projection declares `CAST(a AS DECIMAL(5,0))` and its sibling declares `CAST(b AS DECIMAL(10,2))`, the parent's widened schema is `DECIMAL(10,2)` and the emitter wraps each child's projected column in `CAST(... AS DECIMAL(10,2))` regardless of the child's declared cast target. This is not a "clean-up" cast; it is the load-bearing rule for set-op parity with Spark, whose analyzer computes the widened set-op type before the child projections' types are pinned. Codified: the analyzer's sub-sweep computes the widened schema; the emitter's `render_union` / `render_intersect` / `render_except` applies a per-column CAST wrapper from that widened schema. Neither pass may defer the CAST to the other — the analyzer does not rewrite child projections in place; the emitter reads the widened schema on the parent, not the child-declared cast.

**Resolved reference binding (ADR-024).** The resolve pass performs name resolution exactly once, validator-style: match count 0 → `UnknownColumn`, 1 → bound, 2+ → `AmbiguousColumn`. It stamps the matched attribute's `ExprId` and ordinal on the reference; emission regenerates qualifiers against the current alias. DataFrame plan-ID lookup is the separate ADR-026 path.

---

