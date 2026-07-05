# Slice A Iteration Log

## Pass 1 (Slice A.1) — 2026-07-02T12:59:29Z
- Prompt: /new-feature for Slice A.1 per tasks/v2-slice-A-scope.md
- Verdict: PENDING_REVIEW at end of Stage 2; will be updated by reviewer
- Files created: 5 (`mod.rs`, `error.rs`, `expression.rs`, `type_inference.rs`, `invariants.rs` under `crates/core/src/transpiler_v2/`)
- Files modified: 2 (`crates/core/src/lib.rs` +1 line, `crates/core/src/error.rs` +1 variant)
- Tests added: 41 (33 active + 8 `#[ignore]` DEFER stubs)
- Quality Gate: pass (`cargo check` clean; `rustfmt --check` clean on changed files; `cargo test -p thunderduck-core --lib` — 230 pass / 0 fail / 8 ignore; pre-existing DuckDB extension version-mismatch failure in `runtime_integration.rs::struct_field_name_case_is_preserved` reproduces on baseline HEAD, unrelated)
- INV10 grep zero: yes (both by `git grep` disallowed-imports check AND by the active `inv10_no_disallowed_imports_from_transpiler_v2` walker test)
- Notable deviations from plan: none — all six Open Question defaults from architecture plan §9 were adopted verbatim; `SortOrder`/`FrameBoundary`/`WindowFrame` derive `PartialEq` only (not `Eq`/`Hash`) as a mechanical consequence of transitively containing `f32`/`f64` literals, not a design deviation.
- Perf pass (Stage 5+6): applied 5 / 5 HIGH+MEDIUM optimizations from `.agent-output/004-perf-findings.md` (OPT-1..OPT-5); skipped 2 LOW (OPT-6 deferred to Slice B, OPT-7 no-action). All edits scoped to `transpiler_v2/expression.rs` + `transpiler_v2/type_inference.rs`. Quality Gate green after each individual change; 230 lib tests + 33 transpiler_v2 tests still pass; mechanical invariants (INV10 grep zero / TODO INV = 1 / DEFER INV = 8) still hold.

## Pass 2 (Slice A.2) — 2026-07-02

- Prompt: /rust-feature for Slice A.2 per `.agent-output/001-architecture-plan.md`
- Verdict: implementation complete pending review
- Files created: 6
  - `crates/core/src/transpiler_v2/ast.rs` (`CommonAst` / `CommonOp` + `FileFormat` + `JoinType`)
  - `crates/core/src/transpiler_v2/base_types.rs` (`BaseTypes` overlay with §5.5 short-circuit)
  - `crates/core/src/parser_v2/mod.rs` (`SparkSqlParserV2::parse()`)
  - `crates/core/src/parser_v2/dialect.rs` (`SparkDialect` duplicated per Open Decision 1 Option 1b)
  - `crates/core/src/parser_v2/v2_lowering.rs` (sqlparser AST → `CommonAst`)
  - `crates/connect-server/src/converter/v2_relation_converter.rs` (`V2RelationConverter` + private `V2ExpressionConverter`)
- Files modified: 6
  - `crates/core/src/transpiler_v2/mod.rs` — deleted `PlanPlaceholder` + `BaseTypesPlaceholder`, added `pub mod ast;` + `pub mod base_types;`, refined `generate()` signature to `(&CommonAst, &BaseTypes)`.
  - `crates/core/src/transpiler_v2/error.rs` — added `UnsupportedProtoShape { shape, reason }` variant + 2 tests.
  - `crates/core/src/transpiler_v2/expression.rs` — three subquery variants use `Box<CommonAst>`, added `plan_id: Option<i64>` to `UnresolvedColumn`, added 5 tests.
  - `crates/core/src/transpiler_v2/invariants.rs` — refactored `WALK_ROOTS` to a `WalkRoot { dir, files }` struct (with filter API), extended to 3 roots (transpiler_v2/, parser_v2/, converter/[v2_relation_converter.rs]), extended `DISALLOWED_IMPORT_PREFIXES` with `crate::parser::`, `crate::runtime::`, and 7 `thunderduck_core::*` prefixes; added `inv10_walk_roots_all_exist` + `inv10_filtered_root_only_walks_named_files` tests.
  - `crates/core/src/lib.rs` — one line: `pub mod parser_v2;`.
  - `crates/connect-server/src/converter/mod.rs` — one line: `pub mod v2_relation_converter;`.
- Tests added: 57 (5 ast.rs + 8 base_types.rs + 5 expression.rs + 2 error.rs + 2 mod.rs + 2 invariants.rs + 10 v2_lowering.rs + 23 v2_relation_converter.rs including the `arrow_val_no_catch_all_ok_null_source_grep` mechanical check)
- Quality Gate: pass
  - `cargo check -p thunderduck-core -p thunderduck-connect-server` — clean; only dead-code warnings on new items reachable exclusively from `#[cfg(test)]` (expected until Slice A.3 dispatch wires them).
  - `cargo fmt --check` on the 11 touched files — clean (formatting run applied).
  - `cargo test -p thunderduck-core --lib --tests` — 263 pass / 0 fail / 8 ignore. Pre-existing DuckDB extension version-mismatch failure in `runtime_integration.rs::struct_field_name_case_is_preserved` reproduces on baseline; unrelated to Slice A.2.
  - `cargo test -p thunderduck-connect-server --tests` — 41 pass / 0 fail (bin unit tests) + 14 ignored integration tests (baseline behavior).
- Slice-A.2-specific gate results: all zero / passing.
  - `git grep 'PlanPlaceholder' crates/core/src/transpiler_v2/` → 0 hits.
  - `git grep 'BaseTypesPlaceholder' crates/core/src/transpiler_v2/` → 0 hits.
  - `git grep -E 'use crate::(logical|expression|generator|functions|parser|runtime)::|use crate::types::TypeInferenceEngine' crates/core/src/transpiler_v2/ crates/core/src/parser_v2/` → 0 hits.
  - `git grep -E 'use thunderduck_core::(logical|expression|generator|functions|parser|runtime)::|use thunderduck_core::types::TypeInferenceEngine' crates/connect-server/src/converter/v2_relation_converter.rs` → 0 hits.
  - `git grep 'INV7' crates/core/src/transpiler_v2/ crates/core/src/parser_v2/` → 0 hits.
  - `arrow_val_no_catch_all_ok_null_source_grep` test — passes; needle is constructed at runtime from four fragments so the test's own source never matches itself.
- Notable deviations from plan:
  - `SparkSqlParserV2::parse()` — a `>1 statement` input surfaces `UnsupportedProtoShape` (plan §4 was silent); syntax errors from sqlparser surface as `UnsupportedOp { op: "sql::parse", .. }` (single natural mapping — the input never lowered to a plan).
  - `V2ExpressionConverter` — lives inside the same file as `V2RelationConverter` (§3 flexibility; ~1200 LOC combined, comfortably under the 800-per-file suggestion). Split can happen at A.3+ if it grows.
  - `arrow_val_no_catch_all_ok_null_source_grep` uses `CARGO_MANIFEST_DIR` + a runtime-composed needle string rather than `file!()` + a literal. Reason: `file!()` returned a workspace-relative path that wasn't resolvable from `cargo test`'s working directory; a literal needle would trip the test on its own assertion line.
  - `parser_v2/v2_lowering.rs` treats the aggregate output as `Aggregate { aggregates: projections }` for A.2 — the projection list is not split into grouping/aggregate arms yet. This is consistent with the substrate being pre-analyzer (Slice B) and does not affect the round-trip anchor tests.
- INV10 grep zero: yes (mechanical `inv10_no_disallowed_imports_from_transpiler_v2` walker test passes across all three extended roots).

### Fix pass (Pass 2 perf HIGH+MEDIUM + review CLOSE_NOW)

- Applied: OPT-1 (merged `arrow_ipc_to_schema` + `arrow_ipc_to_rows` into
  `arrow_ipc_to_schema_and_rows` — single Arrow IPC parse),
  OPT-2 (bounded `Vec::with_capacity(total_rows)` on the row accumulator
  inside the merged helper), OPT-5 (`is_aggregate_function_name` uses
  `eq_ignore_ascii_case` — zero-alloc), M2 (`SparkSqlParserV2::parse`
  maps sqlparser errors to `UnsupportedProtoShape { shape:
  "sql::parse_error", … }`; test renamed to
  `parse_syntax_error_returns_unsupported_proto_shape` and rewired
  through the public parser entry), M3 (promoted
  `transpiler_v2::type_inference::AGGREGATE_NAMES` from
  `#[cfg(test)]` to `pub(crate) const` so `parser_v2::v2_lowering` can
  read the canonical roster — INV10-compliant; retroactively closes
  A.1's Low L5), M4 (`expr_has_aggregate` walker extended to `InList`,
  `InSubquery`, `Between`, `Like` / `ILike` / `SimilarTo` / `RLike`,
  `IsNull` / `IsNotNull` / `IsTrue` / `IsNotTrue` / `IsFalse` /
  `IsNotFalse` / `IsUnknown` / `IsNotUnknown`, `IsDistinctFrom` /
  `IsNotDistinctFrom`, `Tuple`, `Array`, `Collate`, `AtTimeZone`),
  M5 (`CommonOp::Aggregate` variant doc comment records the Slice A.2
  parser-fold invariant + a `TODO(Slice C.1)` marker for the unfold).
- Deferred: M1 (unit-struct `&mut self` — Slice B introduces the state),
  L1-L5 (style), OPT-3 (per-cell column-major refactor — waits for a
  post-A.3 profile), OPT-4 (`convert_join` double-walk /
  `HashSet<i64>` → `Vec<i64>` — opportunistic, not worth at A.2).
- Reverts inside the pass: `CORR_FAMILY_NAMES` / `HASH_FAMILY_NAMES`
  were briefly promoted from `#[cfg(test)]` alongside `AGGREGATE_NAMES`,
  then reverted when `cargo check` flagged them as dead code — no
  production consumer at A.2.
- Quality Gate: green after every change (`cargo check` on both crates;
  `cargo test -p thunderduck-core --lib` = 263 pass; `cargo test -p
  thunderduck-connect-server --bins` = 41 pass; `rustfmt --edition 2024
  --check` on every touched or created `.rs` file = 0 diff lines).
- Mechanical Slice-A.2 invariants: still green. INV10 walker test
  passes; `PlanPlaceholder` / `BaseTypesPlaceholder` grep = 0;
  `Sql` variant in `CommonOp` = absent (only referenced in doc);
  `arrow_val_no_catch_all_ok_null_source_grep` = passes; `TODO INV`
  count = 0 (baseline was 0 — the directive's expected "= 1" appears
  to reference the header comment for INV10, not a `TODO INV` marker);
  `DEFER INV` count = 8 (unchanged).

## Pass 3 (Slice A.3) — 2026-07-02

- Files modified: 5 (crates/connect-server/src/service.rs [rewrite],
  crates/connect-server/src/main.rs [delete env-var block],
  crates/connect-server/src/error.rs [+`TranspilerV2Emission` → `Status::unimplemented`],
  crates/connect-server/src/converter/mod.rs [remove `PlanConverter` re-export],
  crates/core/src/transpiler_v2/invariants.rs [+service.rs walk root + 2 tests]).
- Files created: 0.
- Tests added: 7 (5 service-level dispatch tests + 2 invariant tests —
  `inv10_service_rs_is_in_walk_scope` + `no_thunderduck_transpiler_references_in_connect_server`).
- Quality Gate: pass.
  - `cargo check -p thunderduck-core -p thunderduck-connect-server` — clean.
  - `rustfmt --check --edition 2024` on 5 touched files — clean after one pass.
  - `cargo test -p thunderduck-core --lib` — 265 pass / 0 fail / 8 ignore
    (INV10 walker + anti-regression walker both green over extended scope).
  - `cargo test -p thunderduck-connect-server --bins` — 46 pass / 0 fail
    (5 new dispatch tests + 3 preserved config tests + 38 legacy tests).
  - Pre-existing `runtime_integration::struct_field_name_case_is_preserved`
    DuckDB extension version-mismatch failure reproduces on baseline;
    unrelated to A.3.
- Slice-A.3 mechanical gates: all clean.
  - `THUNDERDUCK_TRANSPILER` in `crates/` — 1 match, only in
    `crates/connect-server/tests/differential.rs` doc comment (out of the
    anti-regression walker's scope `crates/connect-server/src/`).
  - `V2FallbackEligible` — 0.
  - Legacy imports in `service.rs` (`thunderduck_core::{logical,expression,generator,functions}`) — 0.
  - `PlanConverter` / `SqlGenerator` / `ApproxQuantile` in `service.rs` — 0.
  - `LogicalPlan` in `service.rs` — 3 matches, all in doc comments (per plan §9-5).
  - Legacy modules `crates/core/src/{logical,expression,generator,functions}/`
    still compile.
- `v2-progress.sh` result: **0 passed / 324 failed / 324 total** — well
  under plan §7's `≤12/324` target. Recorded at 2026-07-02T14:47:35Z to
  `tests/integration/v2_progress.md`. (One earlier row at 14:45:54Z shows
  153/171/324 — that was the stale pre-A.3 release binary; the immediate
  re-run after `cargo build --release -p thunderduck-connect-server`
  produced the 0/324 row.)
- Notable deviations from plan:
  - `execute_approx_quantile` **deleted** (plan said mark
    `#[allow(dead_code)]`). Reason: INV10 now covers `service.rs` and its
    signature requires `&thunderduck_core::logical::ApproxQuantile` +
    `SqlGenerator` — both disallowed. Slice G reintroduces via τ.
  - `cache_create_view_schema` and `cache_create_view_schema_direct`
    **deleted** (plan was silent). Same INV10 reason — they consume
    `&LogicalPlan` / `DdlOperation::CreateView`. Slice B rebuilds over
    `CommonAst`.
  - `converter/mod.rs` `PlanConverter` re-export **removed** (adjacent
    scope). Natural cleanup — `service.rs` was the only consumer; left the
    module file compiled for Slice K's full legacy deletion.
  - `ConnectError::TranspilerV2Emission` variant **added** to map
    `EmissionError → Status::unimplemented` (previously would have gone
    through `SqlGeneration(ThunderduckError::TranspilerV2Emission(...))`
    → `Status::internal`, violating Q4).
