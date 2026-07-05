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

## Pass 5 — OPP-OOO (2026-07-05)

Audit the ~11 `#[allow(dead_code)]` sites in `crates/core` and
`crates/connect-server`. Per the plan disposition rule: sites with no
scheduled landing (no ADR / no open decision) are dead-forever and get
deleted; sites with scheduled landings receive an ADR / invariant
citation on the annotation.

Site-by-site disposition:

| File:line | Symbol | Disposition |
|-----------|--------|-------------|
| `emission.rs:63` | `EMIT_TAP_MUTEX` | ANNOTATE — INV2 companion (rearchitect ADR-009 test tap) |
| `emission.rs:598` | `render_tail` | KEEP — Decision 13-A (dev journal 2026-07-02) |
| `emission.rs:609` | `render_distinct` | KEEP — Decision 13-A |
| `emission.rs:1537` | `render_range_relation` | KEEP — Decision 13-A |
| `emission.rs:5181` | `spark_aggregate_return_cast` | ANNOTATE — §5.1 anchor test requires the item; extension-delegated aggregates (ADR-020) make it unwired |
| `emission.rs:5855` | `extension_targets` | ANNOTATE — INV6 activator (currently DEFER) |
| `expression.rs:965` | `is_non_nullable_function_name` | **DELETE** — pub(crate) wrapper of already-used `_lower`; zero callers; no scheduled landing |
| `analyzer.rs:3119` | `_STAR` | KEEP — module doc anchor (comment above already documents) |
| `service.rs:425` | `PlanKind::Ddl` | KEEP — "reintroduced when DDL classification lands" (see `classify_plan` docstring) |
| `service.rs:584` | `bool_batch_responses` | ANNOTATE — DDL classification helper |
| `service.rs:620` | `sql_command_result_response` | ANNOTATE — DDL classification helper |

- **Files touched.**
  - `crates/core/src/transpiler_v2/expression.rs` — delete
    `is_non_nullable_function_name` (−13 LOC) and fold its docstring
    (§1.1/§1.2 anchor) onto the `_lower` sibling.
  - `crates/core/src/transpiler_v2/emission.rs` — three annotation
    updates (`EMIT_TAP_MUTEX`, `spark_aggregate_return_cast`,
    `extension_targets`).
  - `crates/connect-server/src/service.rs` — two annotation updates
    (`bool_batch_responses`, `sql_command_result_response`).
- **LOC delta.** −13 (delete) + neutral annotation updates.
- **Corpus.** 314 → 314 (unchanged — audit is annotation-and-delete of
  dead code).
- **Warnings.** No delta (all sites remain properly-annotated dead
  code or become deleted code; no new warnings).
- **Gate.** `cargo check -p thunderduck-core -p thunderduck-connect-server`
  clean. `cargo test -p thunderduck-core --lib --tests` → 446 pass / 0
  fail (unchanged) + 0 pass / 4 ignored (runtime_integration).
  `cargo test -p thunderduck-connect-server --tests` → 14 ignored
  (differential harness — expected).
- **Fmt drift note.** Scoped `rustfmt --edition 2021 --check` on
  touched files reports 3 pre-existing drift blocks (emission.rs:4602,
  service.rs:530/545). Baseline-drift comparison: block counts are
  identical between HEAD and working tree, so Pass 5 introduces zero
  new drift and does not own any drift per CLAUDE.md § Quality Gate.
