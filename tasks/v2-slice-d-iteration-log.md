# Slice D Iteration Log

**Baseline:** commit `f712b06` (Slice C landed at 134/324 core_v2 + audit-driven specs committed).
**Methodology:** `tasks/v2-slice-iteration-methodology.md`.
**Initial prompt:** `tasks/v2-slice-d-initial-prompt.md`.
**Extension-work handoff specs:** `tasks/duckdb-extension-specs/`.
**Hard cap:** 5 passes for Phase 1.

**Phase structure** (per 2026-07-01 up-front audit):
- **Phase 1** (this iteration): ext4 wiring + native DuckDB primitives + verify-first resolution. Target 134 → ~140-142.
- **Phase 2** (blocked externally on ext5 pin): follow-up /goal invocation. Target ~142 → ~145-148.

Slice D as a whole terminates only when Phase 2 lands.

---

## Pass 1 — 2026-07-01 (halt-and-flag: re-audit required)

- **Prompt:** `tasks/v2-slice-d-initial-prompt.md` (verbatim).
- **Verdict:** APPROVED (0 Critical + 0 High, 2 review iterations).
- **Architect proposed further split:** NO.
- **CLOSE_NOW closed this pass:** M1 (spark_aggregate_rewrite double-DISTINCT risk) + M5 (module docstring undersells) via iteration 2 mini-fix.
- **CLOSE_NOW_HYGIENE carried forward:** none (both hygiene items bundled into iter 2).
- **DEFER_LATER_SLICE:** M3 (render_spark_decimal_div double-walk on non-decimal), M4 (Ok(None) fallback masking), L1-L3 (style).
- **Verify-first resolutions:** `kurtosis` → wired native (`KURTOSIS_POP`, byte-identical to legacy); `count_if` → wired native (`COUNT_IF`, semantic-equivalent).
- **INV6 activation:** COMPLETE. `extension_targets()` populated with 6 entries; `inv6_extension_targets_exist_in_loaded_extension` performs a real containment check against `duckdb_functions()`; `TODO INV6:` markers deleted; reverse-direction check converted to `DEFER INV6 → ADR-015 coverage-denominator activation:`.
- **Quality Gate:** GREEN (cargo check both crates, rustfmt clean, 269 core + 14 connect-server tests pass, TPC-H legacy 51/51 unregressed).
- **Progress signal:** **134 → 134 (Δ = 0)** on `core_v2`. Below the audit's +5-7 estimate.
- **Commit SHA:** `f5756b5` (feat: Slice D Phase 1 — ext4 wiring + INV6 activation).

### Phase 1 termination — halt-and-flag

Per the /goal's stated constraint:

> If the ext5 spec set proves incomplete during Pass 1 (e.g., verify-first case resolves as needing extension work AND a new missing function surfaces), halt and flag — that's a re-audit, not iteration.

**Termination criterion "All Phase 1 target case IDs pass on core_v2" is NOT met.** Halt-and-flag applies.

**Diagnostic finding (from scoped differential run 2026-07-01):** 9 of the Phase 1 target case IDs (`hash-001`, `hash-002`, `hash-003`, `agg-007`, `agg-013`, `agg-020`, `agg2-006`, `math-011`, `type-005`) FAIL under the v2 path. The failures are NOT caused by Slice D Phase 1's code — they are pre-existing **Slice C.2 latent bugs** the 2026-07-01 up-front audit misclassified as "already-passing native":

- **`hash-002` (`F.sha2("name", 256)`)**: v2 emits `SHA256("name", 256)` at `emission.rs:1277`; DuckDB's `SHA256` is single-arg only. Result: `Binder Error: No function matches sha256(VARCHAR, INTEGER_LITERAL)`. Legacy's `FunctionRegistry::translate_typed` strips the bit-length arg before emission; v2's `render_function_call` does not. **Slice C.2 latent bug.**
- **`hash-003` (`F.hash(...)`, `F.xxhash64(...)`)**: Nullability mismatch — Spark returns non-nullable INT/BIGINT; v2 analyzer marks these nullable. **Slice B/C.2 analyzer gap.**
- **`hash-001`, `math-011`, `type-005`, `agg-007`, `agg-013`, `agg-020`, `agg2-006`**: Various latent bugs (differential-tolerance for the sha1→SHA256 legacy-parity approximation; Decimal-to-Boolean type inference; aggregate-context type resolution).

**The audit's core misclassification:** it treated "arm is wired in emission.rs" as equivalent to "case passes differential." But some arms are wired *incorrectly* (arg-count mismatch, wrong return-nullability) — they emit SQL but produce wrong results or hit DuckDB binder errors that don't trigger v2 fallback.

**Slice D Phase 1's real value delivered:**
- INV6 activated with real teeth (6-entry `extension_targets()` containment check).
- ext4 wiring landed cleanly (all six `spark_*` functions correctly named).
- `render_spark_decimal_div` and `spark_aggregate_rewrite` helpers land as substrate for the DECIMAL routing.
- Legacy path unregressed (TPC-H 51/51).

**Slice D Phase 1's honest progress delta: 0.** The audit's +5-7 was wrong.

### Recommended re-audit (Slice C.3 or Slice D Phase 1 scope expansion)

A follow-up mini-slice is needed to close the pre-Slice-D-audit-missed Slice C.2 latent bugs before the Phase 1 target case IDs can turn green. Candidate scope:

- Fix `sha`/`sha1`/`sha2` arg-stripping in `emission.rs::render_function_call` (drop args beyond arg 0 for these specific arms; add regression test that would have caught hash-002).
- Fix nullability inference for `hash`/`xxhash64` return types (analyzer-side change in Slice B substrate — return-type of `spark_hash` / `spark_xxhash64` is non-nullable INT / BIGINT).
- Fix aggregate-context Decimal-to-Boolean inference for `count_if` (needs analyzer-side investigation).
- Fix Div-in-`type-005` / `math-011` — verify the new `render_spark_decimal_div` branch actually fires when expected; possibly the `Binary(Div)` guard is too narrow.

Recommendation: NEW SLICE `Slice C.3 — Slice C.2 latent-bug corrections` (or equivalent), positioned in the readiness map between Slice C and Slice D. Fixes these before Slice D Phase 1's target cases can meaningfully turn green.

**Slice D Phase 1 iteration terminates in a halt-and-flag state; Slice D as a whole is NOT declared complete.** Phase 2 (ext5) remains blocked externally. The next `/goal` for Slice D should either (a) drive Slice C.3 first, or (b) expand Phase 1's scope to include the corrections.

### Phase 1 termination update (2026-07-01) — C.3-4 closed, larger unblock than expected

Slice C.3-4 landed as a `/fix-bug` pass (commit forthcoming). The diagnostician overturned the C.3-4 scope: the failing rows were NOT caused by `emission.rs::render_binary` (which the diagnostician verified was byte-correct against the analyzer's typed shape), but by a silent-NULL catch-all in `crates/connect-server/src/converter/relation_converter.rs:2513` — the `local_relation_to_values_sql::val()` catch-all silently mapped `Decimal128` (and every other unhandled Arrow type) to SQL literal `"NULL"`, corrupting every DECIMAL cell in `createDataFrame` payloads. Fix landed there, not on the v2 substrate.

**Progress signal delta: 134 → 149 core_v2 passing (+15)** — far above the +3 minimum prediction from `type-003/004/005`. The corpus contained many more silently-NULL'd decimal-payload cases than the halt-and-flag audit had visibility into. Legacy TPC-H 51/51 unregressed. `type-005` closed; `math-011` remains a reference-side Spark 4.x ANSI `DIVIDE_BY_ZERO` (not a Thunderduck bug).

**C.3-4 is now closed.** The remaining five Slice C.3 fixes (C.3-1 sha/sha1/sha2 arg-stripping, C.3-2 hash/xxhash64 nullability, C.3-3 count_if predicate typing, C.3-5 sum(decimal) routing verification, C.3-6 percentile_approx/median shape verification) still gate the Phase 1 hash/count_if/sum/percentile target cases. Slice D Phase 1's remaining target case IDs can turn green after those five land.

### Phase 1 termination update (2026-07-01) — C.3-3 closed, +2 delta

Slice C.3-3 landed as a `/fix-bug` pass (commit forthcoming). The initial prompt speculated the `salary > 90000` predicate inside `count_if` was being routed as Decimal; corpus-first reading (`agg-020` uses `F.count_if(F.col("active"))` — argument is a Boolean *column*, not a Decimal comparison; `agg2-006` compares `salary > 90000` which yields Boolean) narrowed to a **symmetric-omission pattern** across two files. Both `TypeInferenceEngine::aggregate_return_type` (returning arg-type instead of `Long`) and `Expression::FunctionCall::nullable` (marking the result nullable) enumerated the count family (`count`, `count_distinct`, `grouping`, `grouping_id`) and both omitted `count_if`; iteration 1 closed the type-inference half, iteration 2 closed the nullability half. Sibling `aggregate_is_non_nullable` extended in parallel to prevent future drift. 4 regression tests added.

**Progress signal delta: 149 → 151 core_v2 passing (+2)** — exactly the two direct target-case unblocks (`agg-020`, `agg2-006`). Failed count dropped 175 → 173 with no other case flipping. Legacy TPC-H 51/51 unregressed.

**C.3-3 is now closed.** The remaining four Slice C.3 fixes (C.3-1 sha/sha1/sha2 arg-stripping, C.3-2 hash/xxhash64 nullability, C.3-5 sum(decimal) routing verification, C.3-6 percentile_approx/median shape verification) still gate the Phase 1 hash/sum/percentile target cases.

### Phase 1 termination update (2026-07-01) — C.3-5 closed verify-only, +0 delta

Slice C.3-5 landed as a **verify-only** `/fix-bug` pass. The diagnostician's "rerun first" preflight caught the case-already-green state: `agg-007` was already GREEN on v2 as of the composition of C.3-4 (Decimal128 `LocalRelation` marshalling) + Slice D Phase 1 (`spark_aggregate_rewrite` routing for DECIMAL SUM/AVG). No production code change was needed. Two regression unit tests were added to `crates/core/src/transpiler_v2/emission.rs::tests` locking in the routing invariant (`sum_of_decimal_routes_through_spark_sum`, `avg_of_decimal_routes_through_spark_avg`); both would have failed against pre-Slice-D-Phase-1 emission.

**Progress signal delta: 151 → 151 core_v2 passing (+0)** — `agg-007` was already inside the 151 baseline from prior landings; no counter movement expected or observed. Legacy TPC-H 51/51 unregressed.

**C.3-5 is now closed.** The remaining three Slice C.3 fixes (C.3-1 sha/sha1/sha2 arg-stripping, C.3-2 hash/xxhash64 nullability, C.3-6 percentile_approx/median shape verification) still gate the Phase 1 hash/percentile target cases.

### Phase 1 termination update (2026-07-01) — C.3-1 dormant, C.3-2 +1, C.3-6 HALT-AND-FLAG

The Slice C.3 remaining fixes landed as a `/new-feature` pass with three distinct outcomes:

- **C.3-2 (hash/xxhash64 non-nullable) — LANDED, +1 delta (151 → 152) closing `hash-003`.** Single-file fix in `crates/core/src/expression/mod.rs`: extended `FunctionCall::nullable`'s non-nullable literal list to include `"hash" | "murmur3" | "xxhash64"`. `murmur3` bundled in as a Spark synonym already grouped with `hash` at `type_inference.rs:733`. `Expression::nullable` is a shared code path (both v2 and legacy consult it), so the fix closes `hash-003` even though the corpus case routes through the legacy `SqlRelation` fallback. 1 regression test.

- **C.3-1 (sha/sha1/sha2 arg-strip) — LANDED DORMANT, +0 delta.** V2 fix in `crates/core/src/transpiler_v2/emission.rs:1277` + regression test committed, but `hash-002` remains RED. Debug logging revealed the runtime path routes through the legacy `SqlRelation` fallback for the `emp` DataFrame (`spark.createDataFrame(...)` plan contains `SqlRelation`, which `AnalyzerError::PuntedOperator` classifies as fallback-eligible); legacy's `FunctionRegistry` maps `sha2 → SHA256` name-only and forwards all args, so the DuckDB `Binder Error` reproduces on legacy. The plan's "matches legacy behavior" claim was factually incorrect (legacy does NOT strip the bit-length). Non-goals forbid touching legacy `FunctionRegistry`; per the coder-agent HALT-AND-FLAG invariant the coder kept the v2 fix + test in place. `hash-002` will flip green immediately when Slice D/E wires `SqlRelation` on the v2 common-AST surface. Recorded as a new "dormant v2 fix" discipline instance in `tasks/lessons.md`.

- **C.3-6 (percentile_approx / median shape verify) — HALT-AND-FLAG, +0 delta.** Preflight showed `agg-013` RED (not GREEN as the plan predicted). Root cause: DuckDB's `approx_quantile` requires FLOAT for the quantile arg but v2 emits `0.5::DOUBLE` (`Binder Error: approx_quantile(DOUBLE, DOUBLE) — Candidate: approx_quantile(DOUBLE, FLOAT) -> DOUBLE`). Emission-side literal-type-suffix bug; not the verify-only shape C.3-6 was scoped for. No production change or regression tests added for `agg-013`. Tracked as **C.3-6b** for a follow-up `/fix-bug` invocation.

**Cumulative Slice C.3 outcome:** 134 → 152 core_v2 passing (+18 across C.3-4, C.3-3, C.3-2; C.3-5 verify-only, C.3-1 dormant, C.3-6 halted). Two Phase 1 target case IDs remain outstanding: `hash-002` (dormant on Slice D/E `SqlRelation` handling) and `agg-013` (blocked on C.3-6b). Slice D Phase 1 as a whole still awaits both — plus Phase 2 (ext5 pin).
