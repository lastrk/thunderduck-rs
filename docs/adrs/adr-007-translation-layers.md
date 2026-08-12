# ADR-007 — τ structured as three layers A / B / C; B is retained but currently minimal

**Status:** Proposed
**Depends on:** ADR-001, ADR-002, ADR-005, ADR-006, ADR-008
**Depended on by:** ADR-009

**Context.** Translation decisions vary in how much context they need. Some are functions of the node alone; some depend on resolved facts that can be pushed into the node; a few depend on environment state or surrounding structure. A prior framing held that the no-optimization constraint (ADR-001) collapses the tree-rewrite layer entirely. That collapse is *constraint-dependent, not proven* — there is no mathematical or other guarantee that no genuinely structural transformation will ever be needed (and ADR-003's SQL-driven desugarings, e.g. `GROUPING SETS`, are exactly such cases).

**Decision.** τ is structured as three layers. **A (resolve/annotate)** is the analysis of ADR-005/006, pushing decision-relevant facts (Spark types, nullability, correlation level) into each node so emission decisions become node-local. **B (tree-rewrite / forced transliteration)** is *retained as an explicit layer* even though it is currently empty of cost-driven rules and minimal in forced ones: it contains only expressibility-forced operator transliterations (no correlation rewrite per ADR-008, no optimizations per ADR-001), most of which are operator-identity-triggered and could fold into the flat emission table — but the layer is kept because (a) its emptiness is constraint-dependent not proven, and (b) it is the designated home for genuinely *structural* forced transliterations (consume a subtree, emit a multi-node SQL shape, e.g. `explode` → `UNNEST`, or SQL-driven desugarings per ADR-003) and for any ADR-001 correctness-forcing carve-outs. **C (escape hatch)** is a small, labeled, instrumented set of decisions depending on environment state not carried in the node pattern (e.g. session timezone driving a timestamp cast).

**Consequences.**
- (+) Retaining B preserves the architectural seam at near-zero cost (an empty rule slice), so a future genuinely-structural transliteration (including SQL desugarings) has a home that is not the flat emission table.
- (+) B is stable and slow-growing (bounded by Spark's operator inventory, not by the expression surface), so it stays as hand-written rules rather than a data-driven pattern DSL — the deduplication and frequency arguments for a pattern DSL do not apply at its size.
- (+) B is the single designated container for structural forced transliterations, SQL-driven desugarings, and ADR-001 carve-outs — keeping them out of the flat table where they would break its node-local audit story.
- (−) C is opaque to audit and not auto-coverable by directed synthesis; covering a C decision needs a hand-written witness.
- (neutral) The count of C entries — and now also the count of non-empty B rules — is a metric to watch: growth signals more non-local coupling than the model assumes, signalling design drift.

**Refinement hooks.** Confirm any B entry is either expressibility-forced (operator-identity-triggered, foldable into the table) or a genuinely structural forced transliteration / SQL desugaring (stays in the rule list). Keep C minimal and counted. Decide whether the operator-identity-triggered B entries live in the emission table via a richer `Emission` variant or in the B rule list; the genuinely structural ones (multi-node output) belong in the rule list regardless. Revisit if the B rule count grows past a handful.

---

