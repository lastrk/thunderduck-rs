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

## Pass 2 — 2026-07-01 (follow-up /fix-bug for C.3-6b)

- **Prompt:** `/fix-bug` for C.3-6b — FLOAT/DOUBLE quantile-arg emission for `percentile_approx`.
- **Verdict:** APPROVED (0 Critical + 0 High, 1 review iteration).
- **Progress signal:** **152 → 152 core_v2 passing (+0).** Second dormant v2 fix — `agg-013` remains RED.
- **Outcome — C.3-6b (percentile_approx FLOAT CAST):** LANDED DORMANT. V2 fix at `crates/core/src/transpiler_v2/emission.rs:1676-1696` wraps `approx_quantile`'s arg-1 in `CAST(... AS FLOAT)` (diagnostician's option (a) — single-site DuckDB-idiosyncrasy adapter, INV3-preserving, blast radius = exactly one arm). Regression test `percentile_approx_wraps_quantile_arg_in_cast_as_float` committed (non-tautological — pre-fix arm did not emit the CAST substring). `agg-013` remains RED because the runtime routes through legacy fallback: v2 lowering at `lowering.rs:225-230` punts on `AggregateSelectOrder`; legacy `FunctionRegistry` at `functions/mod.rs:459-465` has the identical latent bug. Non-goals forbid touching legacy `FunctionRegistry`, so the fix is dormant until Slice E wires `AggregateSelectOrder` on the v2 common-AST surface. Second instance of the "dormant v2 fix" pattern (anchored via C.3-1) — lesson stands in `tasks/lessons.md`.
- **Files changed:** 1 (`crates/core/src/transpiler_v2/emission.rs` — arm change + regression test).
- **Tests added:** 1 (non-tautological).
- **Quality Gate:** GREEN (279 core tests pass; legacy TPC-H 51/51 unregressed).
- **Commit SHA:** 797893e.

## Termination — user-authorized halt-and-flag

Slice C.3 remaining terminates in a **user-authorized halt-and-flag state** (2026-07-01) per methodology §"Hard cap" escalation clause (the clause fires at Pass 5+; the user invoked it early at Pass 2 because the slice boundary was empirically proven wrong by two consecutive dormant v2 fixes).

**Resolution path taken.** After Pass 2 produced a second dormant fix, the assistant surfaced the deadlock via `AskUserQuestion`: continued `/new-feature` or `/fix-bug` iteration cannot close hash-002 or agg-013 within the /goal's stated non-goals (no legacy `FunctionRegistry` modifications), because the runtime routes both cases through legacy and the identical bug lives there. The user selected **"Terminate the /goal in halt-and-flag state (accept partial)"** — an explicit invocation of the methodology's Hard-cap escalation.

**Reassignment.** Both dormant v2 fixes have been formally reassigned to **Slice E** per readiness-map §Slice E scope extension (added same day). Slice E now owns `LogicalPlan::SqlRelation` lowering (activates C.3-1's dormant sha arg-strip → hash-002 green) and `LogicalPlan::AggregateSelectOrder` lowering (activates C.3-6b's dormant `approx_quantile` FLOAT CAST → agg-013 green). Both v2-side fixes are landed with regression-test lock-in; both will flip green automatically when Slice E lands the lowering substrate.

**Slice C.3 final cumulative** (across all sub-fixes C.3-1 through C.3-6b):

| Sub-fix | Delta | Outcome |
|---|---|---|
| C.3-3 (count_if) | +2 | Landed (prior /goal) |
| C.3-4 (LocalRelation Decimal128) | +15 | Landed (prior /goal) |
| C.3-5 (sum/avg decimal verify-only) | +0 | Landed (prior /goal) |
| C.3-1 (sha arg-strip) | +0 | Dormant v2 fix → Slice E |
| C.3-2 (hash nullability) | +1 | Landed (`hash-003` green) |
| C.3-6b (approx_quantile FLOAT) | +0 | Dormant v2 fix → Slice E |
| **Total** | **+18** | 134 → 152 |

**Slice D Phase 1 formal termination.** Depends on Slice E's dormant-fix activation (hash-002 + agg-013 are the only remaining Phase 1 target case IDs). Slice D Phase 2 (post-`ext5` pin) is the natural next `/goal` on the Slice D axis; a draft prompt is prepared in-conversation (readiness-map §Slice D Phase 2 anchor).

**Lesson recorded.** The dormant-v2-fix pattern now has two instances anchoring it — C.3-1 (sha2 arg-strip) and C.3-6b (`approx_quantile` FLOAT). Any future case with the shape "v2 fix correct + unit-tested + INV-preserving, but corpus routes through legacy where the identical bug lives" can cite this precedent for a legitimate dormant-landing outcome.
