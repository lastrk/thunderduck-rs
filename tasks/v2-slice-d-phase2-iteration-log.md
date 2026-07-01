# Slice D Phase 2 Iteration Log

**Baseline:** commit `296598a` (152/324 core_v2 post-C.3 close + docs correction).
**Methodology:** `tasks/v2-slice-iteration-methodology.md`.
**Goal prompt (session hook):** Slice D — drive Phase 2 to full termination via iterated /new-feature passes.

## Pass 1 — 2026-07-01

- **Prompt:** direct /new-feature invocation via Skill tool (see `.agent-output/001-architecture-plan.md` for the architect's scope elaboration).
- **Architect verdict:** APPROVED (proposed no further sub-split).
- **Coder verdict:** implemented per plan; 1 file changed (`emission.rs`); 8 tests added; Quality Gate green except pre-existing `inv6_extension_targets_exist_in_loaded_extension` failure (libduckdb v1.5.1 vs ext6 v1.5.4 runtime mismatch — present on baseline `296598a`, not caused by this pass).
- **Reviewer verdict:** APPROVED (0 Critical + 0 High, 1 iteration).
- **Perf verdict:** OPTIMIZED (0 HIGH + 0 MEDIUM). No opportunities — 7 new O(1) match arms, uniform allocations mirroring precedent.
- **Docs verdict:** UPDATED (`tasks/v2-adr-readiness-map.md` §Slice D Phase 2 acceptance updated with actual outcomes).
- **Post-Pass corpus measurement:** initial `tests/scripts/v2-progress.sh` showed 152 → 152 (Δ 0). Investigation of the 7 target case IDs revealed:
  - **5 GREEN** post-Pass: `cast-012`, `agg-009 kurtosis`, `agg2-003 regr_*`, `agg2-006 count_if`, plus after in-scope analyzer fix `agg-012 corr/covar_samp`.
  - **2 RED (dormant v2 fixes):** `math-016 try_divide`, `agg2-004 try_sum/try_avg`. V2 arms + regression tests correct, but runtime routes through legacy fallback because the `nums` fixture uses `spark.createDataFrame(...)` whose plan contains `SqlRelation` (v2 lowering punts). Legacy `FunctionRegistry` has no `try_divide`/`try_sum`/`try_avg` mapping, so passes the names unchanged → DuckDB "Catalog Error: Scalar Function `try_divide` does not exist".
- **In-scope diagnostic-driven fix (plan §10 Q1 + C.3-3 precedent):** `agg-012` initially RED with schema-type mismatch (Integer vs Double). Root cause: `TypeInferenceEngine::aggregate_return_type` at `types/type_inference.rs:361-370` had `stddev/variance/skewness/kurtosis` in the `→ Double` arm but omitted the `corr`/`covar_samp`/`covar_pop`/`regr_*` family (symmetric-omission, same pattern as C.3-3's `count_if`). Data-only extension added — arm now covers all 11 correlation/covariance/regression names. `agg-012` flipped GREEN.
- **Post-fix measurement:** `152 → 153 core_v2 (+1)`.
- **HALT-AND-FLAG:** 2 dormant items (`math-016`, `agg2-004`) reassigned to **Slice E** per plan §10 Q5 (third and fourth instances of the "dormant v2 fix" pattern after C.3-1 and C.3-6b).
- **Files changed:** 2
  - `crates/core/src/transpiler_v2/emission.rs` — 3 new `extension_targets()` entries; 7 new arms in `render_function_call` (3 ext6 pass-throughs + 4 native aggregates); 8 new unit tests.
  - `crates/core/src/types/type_inference.rs` — data-only extension to `aggregate_return_type` `→ Double` arm to include correlation/covariance/regression family.
- **Tests added:** 8 (all in `emission.rs::tests`, all pass).
- **Quality Gate:** GREEN (`cargo check`, `cargo fmt --check`, `cargo test -p thunderduck-core --lib --tests` all pass except the pre-existing INV6 environmental failure noted above).
- **Legacy TPC-H differential:** 51/51 unregressed (preflight run + post-Pass run both green).

## Termination

Slice D Phase 2 terminates in a **partial-green + halt-and-flag state** (2026-07-01):

- **5 of 7 target case IDs GREEN.** `cast-012`, `agg-009 (kurtosis half)`, `agg-012`, `agg2-003`, `agg2-006` all pass on `core_v2`.
- **2 dormant v2 fixes reassigned to Slice E.** `math-016` and `agg2-004` v2 arms are correct + regression-tested but corpus routing through legacy fallback (SqlRelation punt on `nums` createDataFrame) blocks them. Same substrate blocker as `hash-002` (C.3-1) and `agg-013` (C.3-6b) — all four cases now await Slice E's `SqlRelation` lowering.
- **Cumulative Slice D delta:** 134 → 153 (+19) across Phase 1 substrate + C.3 corrections + Phase 2 wiring + analyzer symmetric-omission fix.
- **INV6 tightening:** allow-list extended from 6 to 9 entries. The unit test `inv6_extension_targets_exist_in_loaded_extension` continues to fail in the devcontainer with a pre-existing libduckdb v1.5.1 vs ext6 v1.5.4 runtime mismatch — not caused by this pass; tracked as a separate environmental issue.
- **Every new Slice-D DEFER item names an owning slice:** the 2 dormant fixes are reassigned to Slice E per readiness-map §Slice E scope extension.

**Slice D Phase 2 formal completion depends on Slice E's `SqlRelation` lowering activator** (same dependency as C.3-1 and C.3-6b). Once Slice E lands, `math-016` and `agg2-004` flip GREEN automatically via the already-committed v2 arms.

## Lesson strengthened

The **"dormant v2 fix" pattern** now has **four documented instances** anchoring it in `tasks/lessons.md`:
1. **C.3-1** — sha2 arg-strip (unblocks `hash-002`).
2. **C.3-6b** — approx_quantile FLOAT CAST (unblocks `agg-013`).
3. **Slice D Phase 2** — try_divide → spark_try_divide (unblocks `math-016`).
4. **Slice D Phase 2** — try_sum/try_avg → spark_try_sum/spark_try_avg (unblocks `agg2-004`).

Pattern shape (unchanged across instances): v2 emission arm correct + unit-tested + INV-preserving, but the corpus case routes through legacy fallback where the same or worse bug lives (or where the legacy pass-through produces a DuckDB catalog error). Fix is "landed" from a substrate perspective; corpus movement waits for substrate expansion (Slice E).

## What this pass validated in the methodology

- **Verify-native-first** (added to lessons.md this session) worked: 7 of the 10 pre-drafted ext5 specs turned out to be unnecessary because native DuckDB matched Spark semantics. Only 3 needed extension implementation (ext6). If verify-native-first had been applied at ext5-spec-draft time, 7-of-10 wasted spec work would have been avoided.
- **Rerun-first preflight** worked: the post-Pass corpus measurement immediately surfaced the dormant-fix + symmetric-omission issues, avoiding a false-positive termination on unit tests alone.
- **In-scope diagnostic-driven fix** (plan §10 Q1 → C.3-3 precedent): the analyzer symmetric-omission for `corr`/`covar_samp` was resolved via a data-only `TypeInferenceEngine` extension in the same pass, without a follow-up `/fix-bug`. Correct application of the precedent.
