# v2 Simplification Pass Log

Companion to `tasks/v2-simplification-driven-goal-plan.md`. One row per
executed pass, in queue order. Records the corpus baseline delta (must be
monotone), files touched, and any deviations from the plan.

**Corpus fitness gate:** `tests/scripts/v2-progress.sh` PASSED count.
**Pre-plan baseline (2026-07-05, commit `6b421b1`):** **314 PASSED / 10 failed / 324 total.**
This is the floor — subsequent passes must remain ≥ 314.

---

## Pre-flight — orphaned v1 test removal (2026-07-05, pre-Pass 1)

**Not an OPP.** The v1 module cleanup (`e8bc04a chore(cleanup): delete
dead legacy v1 transpiler modules`) left one orphaned test behind:
`crates/core/tests/runtime_integration.rs::generator_to_duckdb` (lines
78-115) still imported from the deleted `thunderduck_core::{expression,
generator, logical}` modules. `cargo test -p thunderduck-core --lib
--tests` fails at HEAD with `E0432 unresolved imports` because of this
one test. Removing the test file's stale `#[ignore]`d function unblocks
the Quality Gate for the entire simplification plan.

- **Files touched.** `crates/core/tests/runtime_integration.rs`
  (−38 LOC: one dead `#[tokio::test] #[ignore] async fn
  generator_to_duckdb`).
- **Corpus.** Unchanged (test was `#[ignore]`; not part of corpus).
- **Warnings.** No delta.

## Pass 1 — OPP-JJJ (2026-07-05)

Delete legacy `crates/core/src/types/type_inference.rs` (v1 leftover
that survived the 2026-07-05 cleanup). No production callers.

- **Files touched.**
  - `crates/core/src/types/type_inference.rs` — deleted (−1313 LOC).
  - `crates/core/src/types/mod.rs` — drop `mod type_inference;` and
    `pub use type_inference::TypeInferenceEngine;` (−2 lines).
- **LOC delta.** −1315.
- **Corpus.** Baseline 314 → 314 (unchanged — dead-code deletion is
  behavior-preserving by construction).
- **Warnings.** No delta on touched files. `PipeIfUnresolved`
  never-used warning remains (Pass 4's target — expected).
- **INV10 grep barrier.** `git grep -E 'use
  crate::types::TypeInferenceEngine|use
  thunderduck_core::types::TypeInferenceEngine' crates/` returns only
  the mechanical mentions inside `crates/core/src/transpiler_v2/invariants.rs`
  (the disallowed-imports list). Barrier satisfied.
- **Gate.** `cargo check -p thunderduck-core` clean. Scoped
  `rustfmt --edition 2021 --check` on touched files clean. `cargo test
  -p thunderduck-core --lib --tests` → 448 pass / 0 fail / 4 ignored
  (lib) + 1 pass / 0 fail / 4 ignored (runtime_integration).
