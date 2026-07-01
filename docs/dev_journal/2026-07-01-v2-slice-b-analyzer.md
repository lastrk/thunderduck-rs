# Dev Journal — 2026-07-01 — v2 Slice B + Slice C.1 (Substrate)

## Slice B — Type & Nullability Analyzer

### Summary

Slice B of the rearchitecture readiness map (`tasks/v2-adr-readiness-map.md` §1) landed as a
substrate-only change inside `crates/core/src/transpiler_v2/`. The v2 transpiler now has a real
common AST + analyzer to build emission on top of. Legacy transpiler untouched;
`transpiler_v2::generate` still returns `Unsupported` — no dispatch wiring in this slice.

**Progress signal**: unchanged from baseline (12/324 in `tests/integration/v2_progress.md`) —
the analyzer alone can't move differential counts without emission. Expected delta on next
`tests/scripts/v2-progress.sh` after Slice C wires dispatch: +5 to +15.

**Pipeline artifacts**: `.agent-output/001-architecture-plan.md` through
`.agent-output/006-docs-update-log.md`. Multi-agent `/new-feature` run
(architect / coder / reviewer / perf-reviewer / perf-optimizer / docs).

---

### What landed

- **`ast.rs`**: `CommonAst` grew from a unit struct to a 15-operator enum + `Punt`
  (Project / Filter / Join / Aggregate / Sort / Limit / Tail / Union / Intersect / Except /
  Distinct / WithColumns / DropColumns / AliasedRelation / TableScan / LocalRelation /
  RangeRelation). Expression slots reuse `crate::expression::Expression` verbatim per plan §4.3
  — no parallel `V2Expression` in this slice. Local `JoinKind` enum + `AggregateCall` struct.

- **`analyzer.rs`**: `TypedAst`, `TypedOp` (17 variants), `TypedAttr { data_type, nullable }`,
  `Schema` / `BaseTypes` aliases, `AnalyzerError` (six `thiserror` variants), sealed `HasSchema`
  trait, `pub(crate) analyze()`, `pub has_resolved_schema()`, `pub inference_smoke()`, and three
  bounded passes: `resolve` (bottom-up structural, delegates star-expansion / USING dedup /
  alias flatten to the shape logic from `LogicalPlan::infer_schema()`), `assign_types`
  (bottom-up + one downward sub-sweep for `Union` type widening per ADR-006 line 168),
  `derive_nullability` (outer-join nullability + grouping-sets column widening). All type /
  nullability decisions delegate to `Expression::data_type(&schema)` /
  `Expression::nullable(&schema)` and `TypeInferenceEngine::{unify_types, promote_numeric,
  aggregate_return_type, ...}` — zero rule re-derivation, per ADR-015 discipline.

- **`analyzer_fixtures.rs` (new)**: five literal `StructType` fixtures matching
  `tests/integration/differential/dataframe_corpus.py::build_inputs`
  (`emp` 14 fields with two-level-nested `address.geo` struct, `dept` 5, `emp2` 6, `nums` 7,
  `raw` 5); five mini `CommonAst` fixtures (`smoke_type_001`, `smoke_cond_003`, `smoke_agg_013`,
  `smoke_type_011` for outer-join right-side widening, `smoke_type_019` for union widening);
  `run_all()` panics with per-field diffs on mismatch. Wired as `#[path]` submodule of
  `analyzer` to avoid editing `mod.rs`.

- **`invariants.rs`**: `inv4_inference_isolation` and `inv5_no_unresolved_after_analyzer` now
  have real assertion bodies. INV4 hands off to `inference_smoke()`. INV5 runs `analyze` on a
  real fixture, then plants a `DataType::Unresolved` at `projection_types[0]` — with the
  top-level schema left clean — to prove the walker inspects every slot, not just surface
  schemas. Both `TODO INV4` / `TODO INV5` markers deleted. `inv7_both_frontends_produce_same_ast`
  rewritten around the new `CommonAst { root }` shape; still a placeholder for the eventual
  two-front-end corpus test.

---

### Notable coder decisions

- **`smoke_type_019` expected value corrected** from the plan's guess `Decimal(11,2)` to
  `Decimal(10,2)` — `TypeInferenceEngine::unify_decimal(5,0,10,2)` computes precision =
  `min(max(5-0, 10-2) + max(0,2), 38) = 10`. The legacy engine is the oracle per ADR-015; the
  plan document is not. Textbook ADR-015 discipline: LLM-extracted rule is untrusted until the
  oracle validates.
- **`analyzer_fixtures` wired as a `#[path]` submodule of `analyzer`** rather than a sibling of
  `mod.rs`, to satisfy the "no `mod.rs` edits" constraint. Physical file at
  `crates/core/src/transpiler_v2/analyzer_fixtures.rs` per plan §8.
- **`GroupingSets` reused from `crate::logical`** directly (already `pub`). The two helpers the
  analyzer needs (`grouping_expr_name`, `grouping_column_names`) are mirrored as private
  free functions in `analyzer.rs` — 15 lines of intentional duplication to preserve module
  isolation without a cross-module edit.
- **Perf M2 applied**: `HashSet<String>` + per-field `.to_lowercase()` case-fold lookups replaced
  with `.iter().any(|n| f.name.eq_ignore_ascii_case(n))` linear scans at five sites (matches
  legacy `field_by_name` conventions). Zero heap allocations on the case-fold path.

---

### What's still open (Slice C follow-ups)

Non-blocking review findings queued for the Slice C emission work:

- **M1** — `has_resolved_schema` reports `false` for any Project containing a bare `Star`. No
  fixture exercises Star today; Slice C's first `SELECT *` will trip INV5. Fix: skip Star slots
  in the walker's projection_types check, or fill with a schema-derived sentinel.
- **M2** — RIGHT join with USING keeps the USING key on the left side; Pass 3 then marks it
  incorrectly nullable. Latent correctness bug; no fixture triggers it.
- **M3** — Pass 2's `Union` widening updates each child's `schema` and `projection_types` but
  not the deeper `Expression`-slot types. Doc-comment marker recommended in `analyzer.rs:1376`.
- **M4** — `Union` fields keep left-side names (`unionByName` not modeled). Add `by_name: bool`
  to `ast::Union` — arguably a Slice C task.
- **M5** — `AnalyzerError::AmbiguousColumn` and `::TypeMismatch` are never constructed. Either
  drop or comment which future pass will construct them.
- **M6** — `resolve_project` seeds `projection_types` with `Unresolved` that Pass 2 completely
  overwrites. Doc-comment or skip seeding.

Slice C will also wire the `LogicalPlan → CommonAst` adapter and flip `transpiler_v2::generate`
off `Unsupported`. Until then, `THUNDERDUCK_TRANSPILER=v2` still hard-errors per session.

---

### Quality-gate output

- `cargo check -p thunderduck-core` — clean.
- `cargo fmt --check` on touched files — clean.
- `cargo test -p thunderduck-core --lib --tests` — **215 passed / 0 failed** (1 runtime
  integration test passed, 5 pre-existing ignored, unrelated to this slice).
- Differential suite (`core_v2`) intentionally not re-run — excluded from the agent-pipeline
  gate per `CLAUDE.md` (`## Quality Gate`); it's the v2-transpiler progress signal, measured
  separately via `tests/scripts/v2-progress.sh` after Slice C.

**Tests**: 215 unit + lib · differential unchanged (not re-run)

---

## Slice C.1 — Substrate (pass 1 of Slice C)

### Summary

Slice C.1 landed as the first sub-slice of Slice C per the architect's proposed within-slice
split (`.agent-output/001-architecture-plan.md` §0, honored by
`tasks/v2-slice-iteration-methodology.md`). C.1 delivers the substrate — lowering adapter,
emission choke-point, dispatch wiring, Slice-B M1-M6 closed, INV2 + INV3 activated. C.2 (next
pass) will deliver the scalar-expression declarative rows that turn Spark-parity CAST cases
green. Baseline: `f5a54c3` (Slice B substrate).

**Progress signal**: not re-measured — per iteration methodology, `tests/scripts/v2-progress.sh`
runs only at final Slice-C termination. `tests/integration/v2_progress.md` still reads 12/324.

**Pipeline artifacts**: `.agent-output/001-architecture-plan.md` through
`.agent-output/005-summary.md`. Two review iterations (iteration 1 → `NEEDS_CHANGES` with 2
Critical / 2 High; iteration 2 → `APPROVED`).

### What landed

- **`lowering.rs` (new file)**: `pub fn lower(&LogicalPlan) -> Result<CommonAst, LoweringError>`
  maps all 29 `LogicalPlan` variants. The 15 variants covered by Slice B's `CommonOp` set map
  1:1; the remaining variants (Sample, SqlRelation, WithCte, DdlStatement, ToDataFrame,
  SingleRow, ShowString, NADrop/NAFill/NAReplace, Unpivot/Pivot, Stat*, ApproxQuantile, Describe,
  Summary, `LocalDataRelation`) produce `CommonOp::Punt { kind, reason }` with stable strings.
  Own `LoweringError` type composed into `ThunderduckError::V2Lowering`.
- **`emission.rs`**: grown from stub into the C.1 emission surface. `dispatch_op` is a
  hand-written `match` over `TypedOp` discriminants dispatching to per-op renderers
  (`render_project`, `render_filter`, `render_sort`, `render_limit`, `render_tail`,
  `render_distinct`, `render_with_columns`, `render_drop_columns`, `render_aliased_relation`,
  `render_table_scan`, `render_local_relation`, `render_range_relation`, `render_union`,
  `render_intersect`, `render_except`, `render_aggregate`). Scalar expressions delegate to
  `SqlGenerator::gen_expr` via a `render_expr` helper — marked as the **C.2 seam** to be
  drained. `EmittedSql` newtype with module-private `emit()` constructor gives INV2 teeth by
  type construction. `EmissionError` (`UnsupportedOp` / `ChildFailed` / `MissingField` /
  `LegacyRenderFailed`). `quote_ident` fast-path (OPT-M1, mirroring legacy).
- **`mod.rs`**: new `pub fn generate(plan, base_types) -> Result<String, ThunderduckError>`
  composing `lower → analyze → dispatch`. `set_serializer_tap` renamed to `set_emit_tap`
  (`#[deprecated]` alias retained for source-compat); `EMIT_TAP` atomic; `pub mod lowering`
  added. INV1 TODO rewritten to point at the differential-harness slice.
- **`ast.rs`**: `Union` gained `by_name: bool` (M4).
- **`analyzer.rs`**: M1 walker Star fix in `walk_resolved`. M2 RIGHT-USING dedup source +
  Pass-3 ordinal logic rewrite. M4 `by_name` reorder moved to Pass 2 (Pass 1 no longer has a
  populated right-child schema after M6). M5 `AmbiguousColumn` (exhaustive
  `ensure_no_ambiguous_columns` walker with subquery-body punt to Slice G and UpdateFields
  punt to C.2) + `TypeMismatch` (Filter predicate boolean check). M6 Pass-1 seed removal for
  `resolve_project` / `resolve_aggregate` / `resolve_with_columns`.
- **`invariants.rs`**: INV2 (`inv2_dispatch_is_only_sql_writer`) installs a counting emit tap
  and asserts exactly-once. INV3 (`inv3_emission_table_single_source_of_truth`) rewritten as
  grep-based build assertion — rejects `use crate::functions::FunctionRegistry` and
  glob-imports of `crate::generator::*` in `emission.rs`; positive coverage anchors for every
  `render_<op>` helper and `dispatch_op` / `pub fn dispatch(`.
- **`error.rs`**: `V2Lowering(#[from] LoweringError)`, `V2Analyzer(#[from] AnalyzerError)`,
  `V2Emission(#[from] EmissionError)`.
- **`service.rs`**: `TranspilerPath::V2` arm now dispatches through `transpiler_v2::generate`;
  synchronous `build_base_types_from_plan` walks `TableScan` / `InMemoryRelation` nodes;
  `is_v2_fallback_eligible` predicate accepts `PuntedOperator` / `UnknownTable` / `UnsupportedOp`
  and hands off to legacy — everything else surfaces as `ConnectError::Unsupported`.
- **`generator/mod.rs`**: added `pub fn with_schema_for_v2(self, schema: StructType) -> Self`
  so `emission::render_expr` can seed the legacy generator for schema-aware rendering. Zero
  legacy-behavior change.

### Notable coder decisions

- **C.1 / C.2 sub-split honored.** The architect's plan §0 proposed splitting Slice C. The
  coder attempted only C.1's substrate; scalar-expression declarative rows deferred to C.2.
  Under the iteration methodology, C.2 runs as pass 2 of Slice C (not a punt to a later slice).
- **`EMISSION_TABLE` scaffolding deleted, not filled.** Iteration 1 built out
  `EmissionRow` / `Template` / `SlotKind` / `EMISSION_TABLE` as data structures without an
  interpreter — the emission was still hand-written `render_*` helpers, and the declarative
  table was dead code. Iteration 1's reviewer flagged this as Critical (C1). Iteration 2 closed
  it by **deleting the scaffolding** rather than adding an interpreter. The honest C.1 shape is
  now named in the module docstring: hand-written `match` over `TypedOp` discriminants. The
  declarative table shape lands with C.2's per-function rows, when it has actual clients.
- **`SqlGenerator::gen_expr` seam explicitly marked for C.2 drain.** `render_expr` allocates a
  fresh `SqlGenerator` and clones the operator schema per expression call. This is the C.2 seam
  — the module docstring, the `render_expr` docstring, and INV3's docstring all name it. When
  C.2 lands per-function rows, `SqlGenerator` gets removed from `emission.rs` entirely and INV3
  tightens to reject the import.
- **INV3 rewritten to enforce the honest invariant.** Iteration 1's INV3 test read as if
  `SqlGenerator` were fully forbidden, but the same file imported it. Iteration 2 renamed the
  invariant: `FunctionRegistry` remains forbidden; the transitive pull via `SqlGenerator` is a
  documented seam that C.2 removes.
- **OPT-M1 (`quote_ident` no-quote fast path) bundled into this pass.** Pattern copied verbatim
  from legacy `SqlGenerator::quote_ident` (single allocation of `String::with_capacity(len + 2)`
  on the common no-quote path). OPT-M2 (schema clone in `render_expr`) and OPT-M3
  (`build_base_types` unconditional clones) deferred to C.2.
- **`set_serializer_tap` kept as a `#[deprecated]` alias**, not deleted. Some out-of-tree
  callers (differential harness dev branches) may reference the old name.
- **BaseTypes builder synchronous, not async.** The plan considered making `generate_sql` async
  to consult `session.get_view_schema(...).await` per `TableScan`; the coder took the leaner
  synchronous walk over the plan tree and lets `AnalyzerError::UnknownTable` fall back to
  legacy (which has its own DuckDB-based schema fallback).

### Review iterations

- **Iteration 1 → `NEEDS_CHANGES`** (2 Critical / 2 High / 6 Medium / Low): C1
  `EMISSION_TABLE` scaffolding-without-interpreter, C2 `AggregateCall.is_distinct` dropped by
  `render_aggregate`, H1 aliased-relation alias not emitted, H2 INV3 grep test dishonest.
- **Iteration 2 → `APPROVED`**, 0 Critical / 0 High: C1 closed by deleting the scaffolding, C2
  closed via `inject_distinct` helper mirroring legacy, H1 closed by emitting `AS <ident>` +
  column-alias list, H2 closed by tightening the grep to `use ...FunctionRegistry` form and
  adding `TODO Slice C.2:` seam markers.

### Carryover to Slice C.2

**DEFER_LATER_SLICE** (unchanged from iteration 1, verified iteration 2):
- **M5** — global `EMIT_TAP` not test-isolated (latent flake).
- **M6** — `render_tail` embeds `child_sql` twice (legacy has same shape).
- **L1** — `render_expr` allocates fresh `SqlGenerator` per call (dies with the seam).

**Newly named this pass:**
- **UpdateFields** walking in `ensure_no_ambiguous_columns` (`TODO Slice C.2:`).
- **Subquery-body** walking for ambiguity (`TODO Slice G:`).
- **Union per-column CAST wrapper** for widened schema (`TODO Slice C.2:` above `render_union`).
- **`SqlGenerator::gen_expr` seam drain** — C.2 removes the `SqlGenerator` import entirely and
  tightens INV3 to reject it.

**Perf deferred to C.2:** OPT-M2 (schema clone in `render_expr` — dies with the seam), OPT-M3
(`build_base_types` review — needs a `BaseTypes` overlay semantics decision).

### Quality-gate output

- `cargo check -p thunderduck-core` — clean.
- `cargo check -p thunderduck-connect-server` — clean.
- `cargo fmt --check` on touched files — clean.
- `cargo test -p thunderduck-core --lib --tests` — **230 passed / 0 failed** (delta 215 → 230
  from Slice B's baseline: +15 net across analyzer regressions, emission unit tests, lowering
  tests, invariant tests, and one top-level generate test).
- `cargo test -p thunderduck-connect-server --tests` — **14 passed / 0 failed**; 14 differential
  ignored per pipeline gate.
- Differential suite (`core_v2`) intentionally not re-run — per iteration methodology, only at
  final Slice-C termination.

**Tests**: 230 core + 14 connect-server · differential unchanged (not re-run)
