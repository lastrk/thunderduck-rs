# ADR-000 — Positioning: instant-start, single-node, vertically-scaled, no-JVM DuckDB-backed Spark replacement

**Status:** Proposed
**Depends on:** — (premise)
**Depended on by:** ADR-001, ADR-002, ADR-003, ADR-004, ADR-005, ADR-006, ADR-013 (it is the premise that selects their shape)

**Context.** thunderduck-rs can occupy a specific niche: a drop-in Spark Connect server that, for workloads that fit on one large multi-core machine, replaces Spark without the JVM and without the Spark runtime — starting instantly, scaling vertically, and fully exploiting DuckDB's advanced vectorized execution engine. This positioning is not incidental; it *selects the entire implementation strategy*, because it rules out anything that puts a JVM or the Spark runtime in the serving path.

**Decision.** thunderduck is built as a **single-node, vertically-scaled, instant-start, no-JVM** Spark Connect server backed by DuckDB's vectorized engine. Of the three implementation strategies considered (below), this selects **Alternative 2: reimplement the necessary front-end and analyzer slice in Rust, target a Rust IR, and translate to DuckDB SQL**, which is the state thunderduck-rs is already in.

**Alternatives considered (and rejected).**
- *Alternative 1 — embed a minimal Spark front-end (SparkSQL parser + Catalyst analyzer) on the JVM, parse to a real Catalyst plan, translate Catalyst → DuckDB SQL in Java.* Tempting on pure-correctness grounds: it deletes the analyzer-reimplementation problem entirely (you get Spark's real type inference and nullability for free, and SQL/DataFrame provably converge on one representation). **Rejected** because it puts a Spark-Catalyst-bearing JVM in the serving path, violating the instant-start / no-JVM / no-Spark-runtime premise at the root; it also couples hard to non-public Catalyst internals across versions. The correctness win does not survive the positioning constraint.
- *Alternative 2 — reimplement a minimal subset in Rust (SparkSQL parser + the divergent analyzer slice), target a Rust IR, translate IR → DuckDB SQL.* **Chosen.** Maximum control, minimum runtime footprint, pure Rust, in-process DuckDB, instant start. Cost: thunderduck owns a parser and a faithful-to-Spark type/nullability analyzer (the hardest components), validated forever against Spark — accepted as the price of the positioning.
- *Alternative 3 — hook DuckDB into Spark as a native execution backend (Gluten/Comet-style), offloading Spark's physical operators.* **Rejected** as a different product entirely: it keeps the full Spark runtime (Spark does parse/analyze/optimize down to a physical plan), discards the "DuckDB SQL as the target, DuckDB as the optimizer" thesis, and targets DuckDB's execution internals (which DuckDB is not architected to expose the way Velox is). Maximizes Spark fidelity at the total cost of the lightweight, no-JVM positioning.

**Non-goals (scope fences this premise establishes).** No distributed / multi-node execution; no shuffle across machines; no JVM in the serving path; no Spark runtime dependency at serving time; no RDD-style or low-level Spark APIs. Future "does feature X fit?" questions resolve by appeal to this ADR: X must serve the single-node, vertical, instant-start, no-JVM goal.

**Consequences.**
- (+) Selects Alternative 2 decisively and records *why* the more correctness-convenient Alternative 1 is nonetheless rejected, so it need not be relitigated.
- (+) Gives every downstream ADR a premise to appeal to; in particular it is the reason thunderduck *owns* the analyzer slice (ADR-005/006) rather than embedding Spark's.
- (−) Commits thunderduck to reimplementing Spark's type/nullability semantics in Rust (ADR-005/006) — the hardest, highest-risk work — because the cheap alternative (real Catalyst on the JVM) is positioning-incompatible.
- (neutral) The niche is explicitly "fits on one big machine"; workloads that genuinely need distributed execution are out of scope, not a failure.

**Refinement hooks.** Confirm the single-node ceiling is acceptable for the target workloads. If a JVM in the serving path ever becomes acceptable, Alternative 1 should be reconsidered, since it would delete ADR-005/006's entire reimplementation burden. Confirm there is no requirement that forces distributed execution.

---

