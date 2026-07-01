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
