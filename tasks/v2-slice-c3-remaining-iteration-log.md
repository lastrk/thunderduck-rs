# Slice C.3 Remaining (C.3-1, C.3-2, C.3-6) Iteration Log

**Baseline:** commit `610618b` (Slice C.3-5 verify-only landed, 151/324 core_v2).
**Methodology:** `tasks/v2-slice-iteration-methodology.md`.
**Initial prompt:** `tasks/v2-slice-c3-initial-prompt.md`.
**Hard cap:** 5 passes.

**Scope this /goal drives:**
- C.3-1: sha/sha1/sha2 arg-strip (unblocks hash-002).
- C.3-2: hash/xxhash64 non-nullable return (unblocks hash-003).
- C.3-6: percentile_approx / median shape verify (agg-013 — likely verify-only).

---

## Pass 1 — 2026-07-01

- **Prompt:** `tasks/v2-slice-c3-initial-prompt.md` sections C.3-1, C.3-2, C.3-6.
- **Verdict:** APPROVED (0 Critical + 0 High, 1 review iteration).
- **Progress signal:** **151 → 152 core_v2 passing (+1).** `hash-003` green via C.3-2. `hash-002` and `agg-013` stay RED per two HALT-AND-FLAG outcomes below.
- **Outcomes:**
  - **C.3-1 (sha/sha1/sha2 arg-strip):** LANDED DORMANT. V2 fix at `crates/core/src/transpiler_v2/emission.rs:1277` + regression test `sha2_with_bit_length_strips_extra_args` committed, but `hash-002` remains RED — the runtime path routes through the legacy `SqlRelation` fallback (the `emp` DataFrame's `spark.createDataFrame(...)` plan contains `SqlRelation`, which `AnalyzerError::PuntedOperator` classifies as fallback-eligible), and legacy's `FunctionRegistry` maps `sha2 → SHA256` name-only with the same bug. The plan's "matches legacy behavior" claim was factually incorrect (legacy does NOT strip the bit-length). Non-goals forbid touching legacy `FunctionRegistry`; per the coder-agent HALT-AND-FLAG invariant the coder kept the v2 fix + test in place. `hash-002` will flip green immediately when Slice D/E wires `SqlRelation` on the v2 common-AST surface. New "dormant v2 fix" discipline instance recorded in `tasks/lessons.md`.
  - **C.3-2 (hash/xxhash64 non-nullable):** LANDED, +1 delta closing `hash-003`. Single-file fix in `crates/core/src/expression/mod.rs`: extended `FunctionCall::nullable`'s non-nullable literal list from `"count" | "count_distinct" | "count_if" | "grouping" | "grouping_id"` to include `"hash" | "murmur3" | "xxhash64"`. `murmur3` bundled in as a Spark synonym already grouped with `hash` at `type_inference.rs:733` — pre-empts a latent bug. `Expression::nullable` is a shared code path (both v2 and legacy consult it), so the fix closes `hash-003` regardless of fallback routing. 1 regression test with a sanity anchor asserting the arg columns ARE nullable so the fix is non-tautological.
  - **C.3-6 (percentile_approx / median shape verify):** HALT-AND-FLAG. Preflight showed `agg-013` RED (not GREEN as the plan predicted). Root cause: DuckDB's `approx_quantile` requires FLOAT for the quantile arg but v2 emits `0.5::DOUBLE` (`Binder Error: approx_quantile(DOUBLE, DOUBLE) — Candidate: approx_quantile(DOUBLE, FLOAT) -> DOUBLE`). Emission-side literal-type-suffix bug; out of C.3-6's verify-only scope. No production change or regression tests added. Tracked as **C.3-6b** — needs a follow-up `/fix-bug` invocation for the FLOAT/DOUBLE quantile-arg emission fix.
- **Files changed:** 2
  - `/workspace/crates/core/src/transpiler_v2/emission.rs` — sha arm arg-strip + regression test.
  - `/workspace/crates/core/src/expression/mod.rs` — non-nullable literal list extension + regression test.
- **Tests added:** 2 (both non-tautological).
- **Quality Gate:** GREEN (`cargo check -p thunderduck-core` clean; `cargo fmt --check` clean on touched files; `cargo test -p thunderduck-core --lib --tests` 278 passed / 0 failed).
- **Commit SHA:** pending
- **Deviation from plan:** the C.3-1 plan §2.1 claimed the fix "matches legacy behavior" — the diagnostic-first surface at implementation time proved this false. Legacy `FunctionRegistry` mapping is name-only (`sha2 → SHA256`); it does not strip the bit-length arg. Handled via HALT-AND-FLAG per the coder-agent invariant rather than silently rewriting legacy.

## Termination

Slice C.3 remaining terminates in a **partial state**:

- C.3-2 fully closes (+1, `hash-003` green).
- C.3-1 lands as a dormant v2 fix; `hash-002` blocked on future Slice D/E `SqlRelation` handling.
- C.3-6 halts; needs a **C.3-6b** follow-up `/fix-bug` for the FLOAT/DOUBLE quantile-arg emission.

Cumulative Slice C.3 delta across all sub-slices (C.3-3 + C.3-4 + C.3-5 + C.3-1 + C.3-2 + C.3-6): 134 → 152 core_v2 passing (+18). Slice D Phase 1's remaining outstanding target case IDs are `hash-002` (dormant) and `agg-013` (blocked on C.3-6b).
