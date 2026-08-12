# ADR-010 — Extension functions are a minimal gap-filler for Spark/DuckDB divergence, implemented in the C++ extension project

**Status:** Proposed
**Depends on:** ADR-005
**Depended on by:** ADR-015, ADR-016

**Context.** Most Spark expressions translate directly to native DuckDB ops. A minority cannot, for one of two reasons. Either DuckDB's native behavior diverges from Spark in the *value* it computes — rounding mode, decimal arithmetic, ANSI overflow, specific date/string semantics — or DuckDB diverges in the *result type* it returns, e.g. decimal-divided-by-decimal returns `double` in DuckDB but a `decimal` (with Spark's precision/scale) in Spark. Or DuckDB lacks the operator entirely.

**Decision.** τ uses thunderduck-provided extension functions *only* where direct translation cannot match Spark — predominantly numerical semantics, return-type divergences, and nullability. They are emission *outcomes* for a small subset of dispatch cells, not the coverage scope. The extension functions are **implemented in C++ as part of the [`thunderduck-duckdb-extension`](https://github.com/lastrk/thunderduck-duckdb-extension) DuckDB extension project**, a separate build artifact loaded into DuckDB; τ's emission of an `Extension(name)` call is correct only if the corresponding C++ function exists, is loaded, and faithfully implements Spark semantics.

**Consequences.**
- (+) Keeps the bespoke Spark-reimplementation surface as small as possible — extension functions are the highest-risk-per-cell component, so minimizing them minimizes risk.
- (+) Each extension function exists for a specific semantic mismatch; the *why* annotation names the edge values that matter for testing it (ADR-015).
- (+) Return-type divergences are handled *jointly* by inference (ADR-005 must infer the Spark result type — e.g. that decimal÷decimal is decimal with Spark's precision formula) and emission (ADR-010 must emit a function that produces both the right value and that Spark type), tightening the ADR-005 ↔ ADR-010 coupling (Tension T3).
- (−) Extension functions are bespoke reimplementations of Spark semantics and must be covered across their full input edge-value set, with the same differential validation as everything else (edge ADR-010 → ADR-015).
- (−) Introduces a new external build/version dependency: the C++ extension's exported function set and the dispatch table's `Extension(...)` targets must agree (INV6), and version coordination now spans three artifacts — Spark 4.1.1, the dispatch table, and the C++ extension (edge ADR-010 → ADR-016).
- (neutral) Coverage is over the *whole* translation surface; extensions are a minority of the emission outcomes within it.

**Refinement hooks.** The boundary between "a cast fixes the mismatch (stay native)" and "this needs an extension function" is a sub-decision living between ADR-005 and ADR-010 (Tension T3): prefer casts where a cast sequence reproduces Spark semantics *exactly* (both value and type); use an extension function only where no cast sequence does — which is exactly the return-type-divergence case, since a cast on a wrong-typed native result may not recover Spark's value. Document, per divergence, which mechanism is used and why; annotate every extension function with the mismatch it addresses. Define how the C++ extension's function set is kept in lockstep with the dispatch table's `Extension(...)` targets (INV6) and how the extension's behavior is differentially validated against Spark.

> **2026-07-14 note (historical text above kept as-is):** the C++ extension's source has been absorbed in-tree at `extension/` (imported from `nubank/thunderduck-duckdb-extension`; the mirror `lastrk/thunderduck-duckdb-extension` no longer exists as a separate build dependency). "The C++ extension project" above now refers to this in-tree directory, not an external repository. See `extension/README.md`'s Provenance section, `extension/BUILD_PINS.toml`, and `docs/context/extension-archival-checklist.md`. The build/version-dependency edge to ADR-016 in Consequences above is now a submodule + vendored-binary lock (`extension/duckdb`, `extensions/vendored/MANIFEST.toml`) rather than a dependency on an externally-hosted project's availability.

---

