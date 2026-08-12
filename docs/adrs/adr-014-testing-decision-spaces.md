# ADR-014 — Two decision spaces to test; three failure-attribution buckets

**Status:** Proposed
**Depends on:** ADR-005, ADR-009, ADR-016, the DuckDB-correctness assumption (LB2)
**Depended on by:** ADR-015

**Context.** A Spark-vs-thunderduck result divergence could originate in resolution, in translation, or in DuckDB execution. A result-level diff alone cannot localize which.

**Decision.** There are two distinct decision spaces, each instrumented with its own coverage claim: the **translation decision space** (the emission table, ADR-009) and the **resolution decision space** (Spark's type/nullability inference rules, ADR-005/006). Failure attribution has three buckets: resolver bug, translator bug, or — excluded by assumption — DuckDB execution. DuckDB is assumed correct on valid SQL (the assumption underlying ADR-002).

**Consequences.**
- (+) Validating the two decision spaces separately makes failures attributable rather than ambiguous.
- (+) The resolution decision space is small and tractable (Spark's type/nullability rules, not all of Catalyst).
- (−) Explicit scope limitation: the suite will not isolate a correct-SQL / DuckDB-mis-execution case; such a case surfaces as a diff but is misattributed unless triaged.
- (neutral) Triage branches on attribution first, supported by emitted-SQL capture (ADR-015).

**Refinement hooks.** Define the resolution-decision instrumentation (which inference rule fired). Decide how much trust to place in DuckDB's own test suite for the excluded bucket. Specify the triage decision tree.

**Seam-and-drain pattern for cross-cuts.** A pass may deliberately keep a specific cross-cut to an upstream unimplemented feature as an acknowledged seam **iff the next pass's core deliverable is to drain that seam**. Constraints: (a) the seam MUST be marked in source with a `TODO:` comment naming the drain; (b) any invariant relaxed to permit the seam MUST be tightened back to full strength when the drain completes; (c) an unnamed drain is contamination, not a seam. There is no cross-implementation seam because there is no other implementation (ADR-022).

**INV3 + INV10 bracket τ's substrate boundary.** INV3 (the *emission-side* single-source-of-truth rule) and INV10 (the *input-side* barrier: τ imports only value-level types from outside its own module tree) together enforce that τ's substrate stays clean. See §CV.5 for the grep checks.

---

