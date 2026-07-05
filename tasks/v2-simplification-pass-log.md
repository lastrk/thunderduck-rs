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

## Pass 2 — OPP-LLL (2026-07-05)

Delete unused `crates/core/src/types/type_mapper.rs`. `TypeMapper`
(Spark → DuckDB type-string helper for CAST/DDL) has zero non-self
callers; τ's emission uses `render_data_type` in
`transpiler_v2/emission.rs`.

- **Files touched.**
  - `crates/core/src/types/type_mapper.rs` — deleted (−72 LOC,
    including its two unit tests).
  - `crates/core/src/types/mod.rs` — drop `mod type_mapper;` +
    `pub use type_mapper::TypeMapper;` (−2 lines).
- **LOC delta.** −74.
- **Corpus.** 314 → 314 (unchanged — dead-code deletion).
- **Warnings.** No delta on touched files. `PipeIfUnresolved` warning
  persists (Pass 4's target).
- **Verify grep.** `git grep 'TypeMapper' crates/` returns zero hits
  (only dev-journal historical references remain, which are docs).
- **Gate.** `cargo check -p thunderduck-core` clean. Scoped
  `rustfmt --edition 2021 --check` clean. `cargo test -p
  thunderduck-core --lib --tests` → 446 pass / 0 fail / 4 ignored
  (lib, −2 vs Pass 1 = removed TypeMapper unit tests) + 1 pass / 0
  fail / 4 ignored (runtime_integration).

## Pass 3 — OPP-MMM (2026-07-05)

Delete `crates/core/src/runtime/schema_inferrer.rs` and its single
consumer test `crates/core/tests/runtime_integration.rs::struct_field_
name_case_is_preserved`. The load-bearing property (STRUCT field-name
round-trip through DuckDB's Arrow schema) is already covered by the
differential DataFrame corpus (arr-*, struc-*, map-* cases).

- **Files touched.**
  - `crates/core/src/runtime/schema_inferrer.rs` — deleted (−117 LOC).
  - `crates/core/src/runtime/mod.rs` — drop `pub mod schema_inferrer;`
    + `pub use schema_inferrer::SchemaInferrer;` (−2 lines).
  - `crates/core/tests/runtime_integration.rs` — drop
    `struct_field_name_case_is_preserved` (−83 LOC).
- **LOC delta.** −202.
- **Corpus.** 314 → 314 (unchanged — dead-code deletion; the removed
  test was `runtime_integration`, not part of the DataFrame corpus).
- **Warnings.** No delta on touched files. `PipeIfUnresolved` warning
  persists (Pass 4's target).
- **Verify grep.** `git grep 'SchemaInferrer\|schema_inferrer' crates/`
  returns zero hits.
- **Gate.** `cargo check -p thunderduck-core` clean. Scoped
  `rustfmt --edition 2021 --check` clean. `cargo test -p
  thunderduck-core --lib --tests` → 446 pass / 0 fail / 4 ignored
  (lib, unchanged) + 0 pass / 0 fail / 4 ignored
  (runtime_integration, −1 = removed test).

## Pass 4 — OPP-NNN (2026-07-05)

Delete unused `PipeIfUnresolved` trait +
`impl PipeIfUnresolved for DataType` in
`crates/core/src/types/data_type.rs`. Zero callers; the compiler
already surfaced this as a `dead_code` warning.

- **Files touched.**
  - `crates/core/src/types/data_type.rs:102-116` — remove the
    trait declaration + impl (−16 LOC).
- **LOC delta.** −16.
- **Corpus.** 314 → 314 (unchanged — dead-code deletion).
- **Warnings.** `cargo check -p thunderduck-core` now emits **zero
  warnings** (previously 1: "trait `PipeIfUnresolved` is never
  used"). Delta: −1.
- **Verify grep.** `git grep 'PipeIfUnresolved\|pipe_if_unresolved'
  crates/` returns zero hits.
- **Gate.** `cargo check -p thunderduck-core` clean, zero warnings.
  Scoped `rustfmt --edition 2021 --check` clean. `cargo test -p
  thunderduck-core --lib --tests` → 446 pass / 0 fail / 4 ignored
  (lib, unchanged).
