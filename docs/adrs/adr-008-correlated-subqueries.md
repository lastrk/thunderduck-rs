# ADR-008 — Correlated subqueries are emitted directly as DuckDB correlated subqueries

**Status:** Proposed
**Depends on:** ADR-001
**Depended on by:** ADR-007

**Context.** Spark correlated subqueries could be emitted verbatim or rewritten (e.g. to lateral joins / EXISTS). DuckDB supports correlated subqueries directly.

**Decision.** τ emits Spark's correlated subquery structure verbatim and lets DuckDB handle it. No rewrite to lateral.

**Consequences.**
- (+) Consistent with ADR-001: rewriting to lateral would be a restructuring DuckDB does not require, hence forbidden.
- (+) The correlation non-locality stops being a *transformation* concern entirely.
- (neutral) Correlation remains an *annotation* concern: the analyzer (ADR-005/006) must know a reference is correlated to emit the right qualified name and must stage outer-before-inner resolution (ADR-006) — but it produces no rewrite. This is the concrete case that proves ADR-001's discipline.

**Refinement hooks.** Confirm DuckDB's correlated-subquery support covers every correlation shape Spark produces. If some shape is not expressible, *that specific shape* becomes a forced transliteration (ADR-007 B), and only that shape — not correlation in general.

---

