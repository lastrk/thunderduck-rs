# ADR-015 — Differential oracle against reference Spark; all variation-suppression is test-side; inference validated in isolation via AnalyzePlan

**Status:** Proposed
**Depends on:** ADR-001, ADR-004, ADR-005, ADR-009, ADR-010, ADR-012, ADR-013, ADR-014, ADR-016, ADR-017, ADR-018
**Depended on by:** ADR-026

**Context.** The oracle is differential against Apache Spark. Equivalence/variation-suppression could be done in production τ or in the test harness. Inference (ADR-005/006) needs an independent validation oracle so that translation-test failures are attributable (ADR-014).

**Decision.** Generate a plan, serialize once, send identical bytes to both engines, canonicalize *results* (row order, float ULP, NaN/NULL, decimal precision, UTC, collation, map-key order), diff. Variation suppression and equivalence reduction happen test-side at generation time and in result canonicalization — never in production τ. Additionally, the resolution layer is validated *in isolation* against Spark Connect's `AnalyzePlan` RPC, which returns ground-truth resolved schemas; thunderduck's inferred schema is diffed column-by-column (type and nullability) *before* the inference pass has to be correct in concert with the emitter. The AnalyzePlan diff also validates front-end agreement (INV7): the same SQL string parsed by thunderduck and sent to Spark should yield matching resolved schemas.

**Consequences.**
- (+) Validating inference separately collapses the attribution problem: with inference independently confirmed, a result divergence is a translation bug, not a possible upstream type bug.
- (+) The AnalyzePlan diff is cheaper and more localizing than the result diff (no DuckDB execution in the loop) and doubles as the *implementation oracle* for ADR-005/006 and the *front-end-agreement* check for ADR-003/004.
- (+) Identical-bytes-to-both-engines (parity-via-identical-bytes) falls out of serialize-once-send-twice (Invariant INV1).
- (−) Requires a reference Spark 4.1.1 instance and a catalog fixture pinning identical base-column Spark types on both engines.
- (neutral) Canonicalization is test-side because any normalization baked into production τ would itself be a divergence from Spark — this is why the production-canonicalizer idea was rejected during design.

**Refinement hooks.** Specify the result-canonicalization rules precisely (the float/NaN/decimal/collation/map handling). Build the AnalyzePlan schema-diff first, as validation harness, inference implementation oracle (ADR-005/006), and front-end-agreement check (INV7). Define the catalog fixture and how it is established identically on both engines. Implement the tri-state error comparison mandated by ADR-016: on any given case, the oracle accepts **both-succeed** (compare rows) OR **both-throw-with-matching-Spark-error-class** as a PASS; anything else is a divergence. This makes ANSI-throw cases (e.g. `a / 0`, `element_at(empty, 1)`) first-class corpus witnesses of τ's error emulation rather than blanket-failures.

**The oracle is the parity contract.** ADR-015's differential oracle (AnalyzePlan schema diff + result differential) validates τ against **reference Spark**. Correctness is a test-time property enforced by the oracle, not a compile-time property inherited from any substrate. LB9 (§CV.4) is the load-bearing form of this stance.

---

