# 2026-07-01 — v2 Slice D Phase 2 landing

**Baseline:** commit `296598a` (152/324 core_v2 post-C.3 close + docs correction).
**Delta:** 152 → 153 core_v2 (+1). Legacy TPC-H 51/51 unregressed.
**Termination state:** partial-green + halt-and-flag (2 dormant items reassigned to Slice E).

## What shipped

**Emission arms (5 new + 3 verify-only regression tests) in `crates/core/src/transpiler_v2/emission.rs`:**

- **Ext6-arm cluster (3, unconditional pass-throughs at ~l.1701):**
  - `try_divide` → `spark_try_divide(...)`
  - `try_sum` → `spark_try_sum(...)`
  - `try_avg` → `spark_try_avg(...)`
- **Native aggregate cluster (4, at ~l.1748):**
  - `corr` → `CORR(...)`
  - `covar_samp` → `COVAR_SAMP(...)`
  - `regr_slope` → `REGR_SLOPE(y, x)` (direction-sensitive, Spark ↔ DuckDB convention agree)
  - `regr_r2` → `REGR_R2(y, x)` (direction-sensitive)
- **`extension_targets()` extended** from 6 → 9 entries (INV6 tightens over the 3 new ext arms).
- **8 new unit tests** in `emission.rs::tests`: 5 new-arm tests + 3 verify-first regression tests (`fn_try_cast_still_uses_try_cast_syntax`, `fn_kurtosis_still_maps_to_kurtosis_pop`, `fn_count_if_still_maps_to_native_count_if`) — each doc-comments the emission-line-under-test and uses `assert_eq!` on the exact emission string so refactor drift is caught mechanically.

**Analyzer symmetric-omission fix (in-scope per plan §10 Q1 + C.3-3 precedent):**

`crates/core/src/types/type_inference.rs::aggregate_return_type` at l.361-370 had `stddev`/`variance`/`skewness`/`kurtosis` in the `→ Double` arm but omitted `corr`/`covar_samp`/`covar_pop`/`regr_*`. Data-only extension added all 11 correlation/covariance/regression names. `agg-012` corpus case flipped GREEN post-fix.

## Corpus outcomes on the 7 Slice D Phase 2 targets

| Case ID | Function(s) | State post-Pass |
|---|---|---|
| `cast-012` | `try_cast` → native `TRY_CAST` | GREEN (already wired; regression test added) |
| `agg-009 kurtosis half` | `kurtosis` → `KURTOSIS_POP` | GREEN (already wired; regression test added) |
| `agg-012` | `corr` / `covar_samp` → `CORR` / `COVAR_SAMP` | GREEN (wired + analyzer symmetric-omission fixed) |
| `agg2-003` | `regr_slope` / `regr_r2` → native | GREEN (wired; was already green via legacy fallback pass-through, now routes through v2 with regression test) |
| `agg2-006` | `count_if` → `COUNT_IF` | GREEN (C.3-3 landing; regression test added) |
| `math-016` | `try_divide` → `spark_try_divide` | **RED DORMANT** — reassigned to Slice E |
| `agg2-004` | `try_sum` / `try_avg` → `spark_*` | **RED DORMANT** — reassigned to Slice E |

## Dormant-v2-fix pattern — instances #3 and #4

`math-016` and `agg2-004` v2 arms are correct + regression-tested but corpus stays RED because the runtime routes through legacy fallback: `nums` uses `spark.createDataFrame(...)` whose LogicalPlan contains `SqlRelation`, which v2 lowering punts. Legacy `FunctionRegistry` has no `try_divide`/`try_sum`/`try_avg` mapping, so it passes the names through unchanged → DuckDB "Catalog Error: Scalar Function `try_divide` does not exist".

Same pattern shape as C.3-1 (`hash-002` sha2 arg-strip) and C.3-6b (`agg-013` approx_quantile FLOAT CAST). The pattern is now four-anchored in `tasks/lessons.md`. Both cases are formally reassigned to Slice E via readiness-map §Slice E scope extension.

## Verify-native-first validated

The 2026-07-01 ext6 audit (recorded in `tasks/lessons.md` §"Extension-spec discipline") turned out to be prescient: of the 10 originally-drafted ext5 specs, ext6 shipped 3 (`spark_try_divide`, `spark_try_sum`, `spark_try_avg`) and 7 resolved via native DuckDB (`TRY_CAST`, `CORR`, `COVAR_SAMP`, `REGR_SLOPE`, `REGR_R2`, `KURTOSIS_POP`, `COUNT_IF`) — 70% of pre-drafted specs were unnecessary. The lesson's rule ("query `duckdb_functions()` on a live session before speccing an extension arm") would have avoided this waste at ext5-draft time.

## Slice D formal completion depends on Slice E

Slice D Phase 2 lands 5 of the 7 target case IDs GREEN. The remaining 2 (`math-016`, `agg2-004`) are v2-side complete but await Slice E's `SqlRelation` lowering activator — at which point they flip GREEN automatically, along with `hash-002` (C.3-1) and `agg-013` (C.3-6b). Slice D as a whole is not formally terminated until Slice E lands.

## References

- Iteration log: `tasks/v2-slice-d-phase2-iteration-log.md`.
- Architecture plan: `.agent-output/001-architecture-plan.md`.
- Implementation log: `.agent-output/002-implementation-log.md`.
- Review findings: `.agent-output/003-review-findings.md` (APPROVED, 0 Critical + 0 High).
- Perf findings: `.agent-output/004-perf-findings.md` (OPTIMIZED, 0 HIGH + 0 MEDIUM).
- Readiness map §Slice D Phase 2 for the terminal per-case state.
