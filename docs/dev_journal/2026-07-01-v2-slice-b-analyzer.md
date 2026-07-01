# Dev Journal — 2026-07-01 — v2 Slice B + Slice C.1 + Slice C.2 + Slice D Phase 1

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

---

## Slice C.2 — Scalar-expression rows + `SqlGenerator::gen_expr` seam drain (pass 2 of Slice C)

### Summary

Slice C.2 lands as pass 2 of Slice C per the architect's within-slice sub-split. C.2 drains the
Pass-1 seam through which `emission::render_expr` still delegated scalar rendering to legacy
`SqlGenerator::gen_expr`, promotes scalar-expression handling into `emission.rs` as an exhaustive
`match` over all 27 `Expression` variants, and hand-copies ~130 lowercased-name arms from
`FunctionRegistry` into `render_function_call`. All CLOSE_NOW carryovers from Pass 1 (M5 EMIT_TAP
test isolation, M6 `render_tail` CTE, UpdateFields walker, Union per-column CAST, OPT-M2, OPT-M3)
closed in the same pass. Baseline: `208e9b1` (Slice C.1 substrate).

**Progress signal**: not re-measured — per iteration methodology,
`tests/scripts/v2-progress.sh` runs only at final Slice-C termination.
`tests/integration/v2_progress.md` still reads 12/324.

**Pipeline artifacts**: `.agent-output/001-architecture-plan.md` through
`.agent-output/006-docs-update-log.md`. Two review iterations (iteration 1 → `APPROVED` with 2
CLOSE_NOW-in-this-pass Mediums; iteration 2 → both closed).

### Approach A (chosen)

Per plan §1, the architect chose **Approach A**: hand-written per-`Expression`-variant match
arms in `render_expr`; for `FunctionCall`, fan out to a per-function `match` on the lowercased
name. The dead-data lesson from Pass 1 iteration 1 is the deciding constraint — the ~50
non-trivial function shapes are 3-to-5-line `format!` strings, not enough interpreter substrate
to justify a declarative row table. Approach B (row substrate with interpreter) was rejected
for the same reason Pass 1's `EmissionRow` scaffolding was deleted; Approach C (narrow
`FunctionRegistry` accessor) would have regressed Pass 1's INV3 tightening. ADR-009 explicitly
permits interpreted-vs-compiled as an implementation choice; a declarative substrate becomes
motivated when Slice D adds `spark_*` extension rows or Slice F adds ~30 complex-type function
rows — not at C.2's row count.

### What landed

- **`emission.rs` (~1850 lines net)**: `render_expr` reworked as an exhaustive 27-variant match
  with per-variant helpers (`render_literal`, `render_column_ref`, `render_unresolved_column`,
  `render_binary`, `render_unary`, `render_cast`, `render_case_when`, `render_star`,
  `render_expr_paren`, `binop_precedence`) and the fan-out `render_function_call` (~130 name
  arms across string, math, dt, cond, aggregate-shape, and misc clusters). Slice-D/F variants
  (Window, InSubquery, ExistsSubquery, ScalarSubquery, Lambda, LambdaVariable, ArrayLiteral,
  MapLiteral, StructLiteral, Between, InList, Like, Interval, IsDistinctFrom, ExtractValue,
  RowConstructor, UpdateFields) surface as `EmissionError::UnsupportedExpression`; unknown
  function names surface as `EmissionError::UnsupportedFunction`. `RawSql` passes through
  unchanged. `EmissionError::LegacyRenderFailed` fully removed (net-positive vs. the plan —
  grep confirmed zero remaining references). `spark_return_cast` handles projection-slot
  Spark-parity CASTs (int/int Div → DOUBLE, incl. the aliased-Div case closed in iteration 2);
  `spark_aggregate_return_cast` handles integer SUM/AVG return-type wrapping inside
  `render_aggregate`. `render_tail` rewritten with a `WITH __td_child AS (...)` CTE (M6 closure).
  `maybe_wrap_widened_child` gives `render_union` / `render_intersect` / `render_except` a
  per-column CAST wrapper when the analyzer's widened schema diverges from a child's schema.
  Function-level `grouping` / `grouping_id` CASTs live inside `render_function_call` (matching
  legacy shape).

- **`invariants.rs`**: INV3 grep rejects `use crate::generator::SqlGenerator`,
  `use crate::generator;`, `use crate::generator::*`, `use crate::functions::FunctionRegistry`,
  `use crate::functions::*`, `SqlGenerator::new()`, `.gen_expr(`, `.with_schema_for_v2(` (8
  rejections). `REQUIRED_RENDERERS` coverage anchor grew to 26 entries covering every renderer
  helper plus `spark_return_cast` / `spark_aggregate_return_cast`. Docstring rewritten to
  reflect the drained-seam invariant. M5 closed via a module-scoped `EMIT_TAP_MUTEX`
  (`static Mutex<()>` + `lock_emit_tap()` helper with poison recovery) acquired by both
  `inv1_...` and `inv2_dispatch_is_only_sql_writer` tests; no new dep (the `serial_test`
  option from plan §OQ6 was rejected in favor of the eight-line mutex).

- **`analyzer.rs`**: `ensure_no_ambiguous_columns` now recurses into
  `UpdateFieldsExpression::struct_expr` and its optional `value` (Pass-1 `TODO Slice C.2:`
  marker deleted); doc-comment on `pub type BaseTypes` documents the fallback-only overlay
  contract (`resolve_table_scan` prefers the AST-carried schema; overlay is consulted only when
  the AST schema is empty) so future readers don't reintroduce eager seeding.

- **`service.rs`**: `is_v2_fallback_eligible` accepts `EmissionError::UnsupportedExpression`
  and `EmissionError::UnsupportedFunction` (additive-only contract preserved).
  `build_base_types_from_plan` gains an OPT-M3 short-circuit via a new `plan_has_empty_scan`
  predicate walk — in the common case where every scan carries a populated schema, the walk
  returns early and no `BaseTypes` entries are cloned.

- **`generator/mod.rs`**: doc comment on `with_schema_for_v2` records that the method is no
  longer called by `emission.rs` after C.2's seam drain. Legacy body unchanged (out of scope).

### Iteration 1 vs. iteration 2

- **Iteration 1 → `APPROVED`** with zero Critical / zero High and 6 Mediums (2
  CLOSE_NOW-in-this-pass, 3 DEFER_LATER_SLICE, 1 doc-only). CLOSE_NOW items: **M1** —
  `render_projection_slot`'s `Expression::Star(_)` arm collapsed qualified `Star` to bare `"*"`
  instead of routing through `render_star` (dropped the qualifier); **M4** —
  `render_projection_slot`'s `Expression::Alias(a)` arm rendered `<inner> AS <alias>` without
  consulting `spark_return_cast`, so an aliased `Binary(Div, int, int)` emitted
  `"a" / "b" AS "r"` instead of `CAST("a" / "b" AS DOUBLE) AS "r"`. **M2** was a log-only
  correction (the implementation log incorrectly claimed `LegacyRenderFailed` was "kept for
  source stability" when grep confirmed it was fully removed).
- **Iteration 2 → both CLOSE_NOW closed**. M1 fix: replace `Ok("*".to_string())` with
  `Ok(render_star(s))` in the projection-slot Star arm. M4 fix: consult
  `spark_return_cast(&a.expr, schema)` in the Alias arm and wrap the inner render in
  `CAST(... AS <duckdb type>)` when it returns `Some(dt)` (same idiom already used three lines
  below in the "other" arm). Two regression tests added:
  `qualified_star_in_projection_slot_preserves_qualifier` and `alias_of_int_div_gets_double_cast`.
  Iteration-2 test count: 269 core + 14 connect-server.

### Perf verdict — OPTIMIZED

Perf reviewer verdict `OPTIMIZED` with 0 HIGH + 0 MEDIUM. The perf agent explicitly noted the
seam drain **silently absorbed** OPT-M2 (per-expression `SqlGenerator::new().with_schema_for_v2(schema.clone())`
allocation) and Pass-1's L1 (schema clone in `render_expr`) — both die naturally with the
`SqlGenerator` import removal. Six LOWs documented for post-conformance benchmarking
(`arg_refs: Vec<&str>` intermediate, `to_ascii_lowercase` allocation, `data_type()`
recomputation, `spark_column_name` per-node String allocs, per-node `format!` allocation
pattern, `RawSql` clone-on-passthrough); none crosses the "clear win" bar at Slice C.2's scale.
The M4 fix explicitly does **not** re-render expressions — `spark_return_cast` walks types
only, splicing the already-rendered inner SQL into the wrap.

### Carryover DEFER to future slices

- Extension functions (`spark_*`, `try_cast`, `try_divide`, `spark_sum`/`spark_avg` on
  decimal) — Slice D.
- Full join cluster (`Join` remains `UnsupportedOp`) — Slice E.
- Complex types (Array/Map/Struct literals, HOF lambdas, ExtractValue, RowConstructor) —
  Slice F.
- Verticals (Window, subqueries, Interval, Between, InList, Like, IsDistinctFrom,
  `to_utc_timestamp` / `from_utc_timestamp`, `extract` spark4) — Slice G.
- Writes and `UpdateFields` emission, `na.fill` / `na.drop` / `na.replace` operator arms —
  Slice H (or an earlier operator-level slice).
- INV1 full activation — new differential-harness slice.
- INV2 escape-hatch dimension (`C_ESCAPE_HATCHES: &[]`) — ADR-007 slice.
- Subquery-body walking in `ensure_no_ambiguous_columns` — Slice G.
- Reviewer M3 (alias-in-fn-args), M5 (Binary CAST precedence for DATE+INTERVAL), M6 (non-agg
  DISTINCT check) — parity-with-legacy hardening; DEFER.

### Quality-gate output

- `cargo check -p thunderduck-core` — clean.
- `cargo check -p thunderduck-connect-server` — clean.
- `cargo fmt --check` on touched files — clean.
- `cargo test -p thunderduck-core --lib --tests` — **269 passed / 0 failed** (delta 230 → 269
  from Slice C.1's baseline: +42 new tests including the two iteration-2 regression tests).
- `cargo test -p thunderduck-connect-server --tests` — **14 passed / 0 failed**; 14
  differential ignored per pipeline gate.
- Differential suite (`core_v2`) intentionally not re-run — final Slice-C termination will
  measure via `tests/scripts/v2-progress.sh`.

**Tests**: 269 core + 14 connect-server · differential unchanged (not re-run)

### Files changed (Pass 2)

- `/workspace/crates/core/src/transpiler_v2/emission.rs` — ~130 function arms +
  per-Expression helpers, seam drain, M1 / M4 iteration-2 fixes, Union CAST wrapper,
  M6 render_tail CTE.
- `/workspace/crates/core/src/transpiler_v2/invariants.rs` — INV3 tightening (8 grep
  rejections + 26-entry coverage anchor), `EMIT_TAP_MUTEX`.
- `/workspace/crates/core/src/transpiler_v2/analyzer.rs` — UpdateFields walking + BaseTypes
  fallback-only doc contract.
- `/workspace/crates/connect-server/src/service.rs` — fallback-eligible variants + OPT-M3
  `plan_has_empty_scan` short-circuit.
- `/workspace/crates/core/src/generator/mod.rs` — doc-only note that
  `with_schema_for_v2` is no longer called by emission after C.2.

**5 files changed, +1947 / -125 lines.** 42 tests added.

---

## Slice D Phase 1 — Extension dispatch (ext4 subset)

### Summary

Slice D Phase 1 lands as a two-file diff (`emission.rs` + `invariants.rs`) inside
`crates/core/src/transpiler_v2/`. INV6 (extension-target existence) gains real teeth over the
ext4-available function subset; INV3's `REQUIRED_RENDERERS` grows to cover the two new helpers.
Slice D as a whole does **not** terminate here — Phase 2 remains blocked on the
`thunderduck-duckdb-extension` project shipping the `ext5` release and this repo pinning it.

**Progress signal**: **not re-measured this pass** — `tests/integration/v2_progress.md` still
reads 134/324 from Slice C.2. Expected delta at Phase 1 termination: ~140-142.

**Pipeline artifacts**: `.agent-output/001-architecture-plan.md` through
`.agent-output/006-docs-update-log.md`. Two coder iterations (Pass 1 delivered all 8
deliverables; Review Fix Iteration 2 closed M1 + M5). Review verdict `APPROVED` on iteration 1
with 5 Mediums (M1 + M5 CLOSE_NOW closed via iter 2; M2 scoped-differential at Phase 1
termination; M3 + M4 DEFER). Perf verdict `OPTIMIZED` with 5 LOWs all deferred.

### Up-front audit (§0 halt-and-flag)

The architect's §0 audit surfaced three arms already wired in Slice C.2 that the initial
prompt / readiness map listed as "wiring to add":

- `md5` already emits `MD5(...)` (`emission.rs:1112`).
- `sha`/`sha1`/`sha2` already route to `SHA256(...)` (`emission.rs:1113`), matching legacy
  `functions/mod.rs:173-175` as an intentional-parity-gap approximation.
- `stddev`/`stddev_samp`/`stddev_pop`/`variance`/`var_samp`/`var_pop` already wired
  (`emission.rs:1485-1488`).

The audit collapsed the planned edit surface from ~14 arm additions to **6 confirmed + 2
verify-first = 8 arm additions**, saving iterations that would otherwise have discovered the
overlap during coder work.

### What landed

- **`emission.rs::render_function_call`** — 6 new scalar arms: `crc32` → `CRC32(...)`,
  `hash` → `spark_hash(...)`, `xxhash64` → `spark_xxhash64(...)`, `skewness` →
  `spark_skewness(...)`, `percentile_approx` → `approx_quantile(...)`, `median` → `MEDIAN(...)`.
  Plus 2 verify-first arms: `kurtosis` → `KURTOSIS_POP(...)` (native, byte-identical to
  legacy) and `count_if` → `COUNT_IF(...)` (native, semantically identical to legacy's
  pass-through). Plus two synthetic arms `spark_sum` / `spark_avg` (reachable only via
  `spark_aggregate_rewrite`).

- **`emission.rs::render_binary`** — DECIMAL-div branch: `BinaryOp::Div` with a DECIMAL
  operand routes through the new private helper `render_spark_decimal_div`, which mirrors
  legacy `gen_strict_decimal_div` (`generator/mod.rs:1541-1644`) — three sub-branches for
  `(Decimal, Decimal)`, `(Decimal, integral)`, and `(integral, Decimal)`. Non-DECIMAL Div
  falls through to the plain `left / right` path (still wrapped by
  `spark_return_cast(&Div, schema)` for int/int → DOUBLE parity from Slice C.2).

- **`emission.rs::spark_aggregate_rewrite`** — new sibling helper to
  `spark_aggregate_return_cast`. For `SUM(Decimal{p,s})` returns
  `(FunctionCall{name: "spark_sum", distinct: false, ...}, Decimal{min(p+10,38), s})`; for
  `AVG(Decimal{p,s})` / `mean(Decimal{p,s})` returns
  `(FunctionCall{name: "spark_avg", ...}, Decimal{min(p+4,38), min(min(s+4,18), new_p)})`.
  `render_aggregate` calls this before rendering; the rewritten call's `distinct` is
  always `false` (M1 iter-2 fix — DISTINCT is injected at the aggregate level, propagating
  it would emit `spark_sum(DISTINCT DISTINCT x)`).

- **`emission.rs::extension_targets()`** — replaced `&[]` with a 6-entry allow-list:
  `spark_hash`, `spark_xxhash64`, `spark_skewness`, `spark_sum`, `spark_avg`,
  `spark_decimal_div`. `TODO INV6:` marker removed from the doc comment.

- **`emission.rs` module docstring** — Slice D paragraph names the two non-obvious
  dispatch sites (M5 iter-2 fix): `spark_decimal_div` is dispatched from `render_binary`'s
  DECIMAL `/` branch, not a `render_function_call` arm; DECIMAL `sum`/`avg`/`mean` are
  rewritten inside `spark_aggregate_rewrite`.

- **`invariants.rs`** — `TODO INV6:` marker removed; INV6 test body (already ~95% written)
  now runs as a real containment check against `duckdb_functions()` and **turns green**
  with the 6-entry allow-list. INV3's `REQUIRED_RENDERERS` list gained
  `"fn render_spark_decimal_div"` and `"fn spark_aggregate_rewrite"` so future refactors
  can't silently rename or remove them.

### Verify-first verdicts

Both wired native pending scoped-differential confirmation at Phase 1 termination:

- **`kurtosis` → `KURTOSIS_POP(...)`** — byte-identical to legacy `functions/mod.rs:422-425`;
  if legacy differential is green, v2's arm emits the same SQL.
- **`count_if` → `COUNT_IF(...)`** — semantically identical to legacy's pass-through
  (legacy falls through `translate`'s pass-through arm and emits literal `count_if(...)`,
  case-insensitive at DuckDB parse time). Type-inference already maps `count_if` to `Long`.

Both fall back to `EmissionError::UnsupportedFunction` → legacy path if a future differential
run flags them red; spec files (`tasks/duckdb-extension-specs/spark_kurtosis.md`,
`spark_count_if.md`) remain authoritative for the extension replacement.

### Review Fix Iteration 2

Closed 2 CLOSE_NOW Mediums from iteration 1's review:

- **M1** (`emission.rs:1888-1895`) — `spark_aggregate_rewrite` synthesized call had
  `distinct: call.distinct`; changed to `distinct: false` with an inline comment naming
  the aggregate-level injection site.
- **M5** (`emission.rs:27-36`) — module doc Slice D paragraph extended to name the two
  non-obvious dispatch sites.

Remaining Mediums: M2 (scoped-differential at Phase 1 termination — not a code change);
M3 (`render_spark_decimal_div` re-renders operand SQLs before the guard) and M4 (`Ok(None)`
fallback on `decimal_div_type` non-Decimal) both DEFER.

### Carryover to Phase 2

- 5-8 `spark_*` functions requiring new C++ work (spec files pre-drafted under
  `tasks/duckdb-extension-specs/`): `spark_try_divide`, `spark_try_cast`, `spark_corr`,
  `spark_covar_samp`, `spark_regr_slope`, `spark_regr_r2`, `spark_try_sum`, `spark_try_avg`
  (definite); potentially `spark_kurtosis` and `spark_count_if` (verify-first only if
  scoped-differential flags them).
- Phase 1 defer items (M3, M4, all 5 perf LOWs) are all internal micro-refinements; none
  affects Phase 2's C++ extension surface.

### Quality-gate output

- `cargo check -p thunderduck-core` — clean.
- `cargo check -p thunderduck-connect-server` — clean.
- `cargo fmt --check` on touched files — clean.
- `cargo test -p thunderduck-core --lib --tests` — **269 passed / 0 failed** (unchanged
  from Slice C.2 baseline; two-file diff added no new unit tests by design — the
  load-bearing coverage is INV6 turning green).
- **INV6** (`inv6_extension_targets_exist_in_loaded_extension`) — **PASSED with the 6-entry
  allow-list**. This is the single most important coverage signal for Phase 1.
- Differential suite (`core_v2`) intentionally not re-run — Phase 1 termination is the next
  step, but not this pass's responsibility.

**Tests**: 269 core + 14 connect-server · differential unchanged (not re-run)

### Files changed (Phase 1)

- `/workspace/crates/core/src/transpiler_v2/emission.rs` — 6 scalar arms + 2 verify-first arms +
  `render_binary` DECIMAL-div branch + `render_spark_decimal_div` helper +
  `spark_aggregate_rewrite` helper + `extension_targets()` populated + module doc + Slice-D
  TODO cleanup.
- `/workspace/crates/core/src/transpiler_v2/invariants.rs` — INV6 marker cleanup + INV3
  `REQUIRED_RENDERERS` list extended.

**2 files changed, +333 / -26 lines.** Zero new unit tests (INV6 is the coverage signal).
