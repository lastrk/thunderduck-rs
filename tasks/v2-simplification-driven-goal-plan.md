# v2 Simplification-Driven `/goal` Plan

**Purpose.** Execute the 57 actionable simplification opportunities from
`.agent-output/simplification-plan.md` as an iterative `/goal` sequence.
DROPPED opportunities from that plan's Tier 4 (`I`, `N`, `T`, `UU`, `TT`,
`VV`, `DD`, `Z`, `III`, `EEE`, `HH`) are omitted here.

Companion of `tasks/v2-corpus-driven-goal-prompt-template.md`. Unlike the
corpus-driven pipeline (where each pass picks a target from the failure
cluster), this pipeline processes a **predetermined queue** of refactor
opportunities in dependency order.

**Two documents form the simplification-driven pair:**
- **This file** — the pass sequence + `/goal` prompt template. Kept ≤4000
  chars in the template block that follows.
- **`.agent-output/simplification-plan.md`** — the source-of-truth
  analysis (16 passes, 68 identified opportunities, 12 dropped, LOC/arm
  quantification).

## Primary goal

All 51 numbered passes below green, in order, with:
- **Corpus non-regression**: `tests/scripts/v2-progress.sh` PASSED count
  monotone across every pass — a simplification pass MUST NOT drop a
  previously-green case.
- **Quality Gate green per pass** (CLAUDE.md §Quality Gate): `cargo check`
  on touched crate, `cargo fmt --check` on touched files, `cargo test -p
  <touched-crate> --lib --tests`.
- **No new compiler warnings** on files a pass modifies.
- **Estimated total simplification**: −2500 LOC (of which −1600 dead-code
  deletion in Phase 1), −300 match arms collapsed, one durable design
  invariant (`Expression::map_children` walker) removing 5×29 = 145
  arm-lines of duplicated recursion.

## Secondary goal (non-negotiable)

Zero DEFER items. Every review + perf finding surfaced during a pass is
closed **in that pass**. No "future pass" TODOs. No `#[allow(dead_code)]`
added to silence warnings — either wire the code or delete it.

## Design authority

- `docs/thunderduck-rearchitect-ADRs.md` — ADR-000..ADR-022. Every
  refactor preserves the observable behavior these ADRs pin.
- `crates/core/src/transpiler_v2/invariants.rs` — INV1..INV10. No pass
  may regress an active invariant; INV3 (grep barrier) and INV10
  (substrate independence) matter especially for the type-crate changes.
- **The corpus is the fitness gate**. A pass that drops a corpus case is
  not a simplification — it's a regression.

## Methodology — queue processing, not case picking

Each pass = one simplification opportunity applied end-to-end and
committed. There is no "diagnostic" step — the diagnostic already lives
in `.agent-output/simplification-plan.md` under the OPP-X heading. The
per-pass loop is:

1. **Read** `.agent-output/simplification-plan.md` § OPP-X (the source
   analysis for this pass — problem, proposal, quantified impact).
2. **Architect** (via `/new-feature`) — architect reads the OPP-X entry
   plus the target files listed for the pass; produces a concrete
   file-by-file change plan; cites applicable ADR(s) / invariants; names
   the ordering constraints against prior/subsequent passes.
3. **Implement** — coder applies the plan. Runs Quality Gate. Runs
   `v2-progress.sh` at start (baseline) and end (must not regress).
   No new compiler warnings on touched files.
4. **Review** — reviewer verifies (a) OPP-X's intended shape actually
   landed, (b) no invariant regressed, (c) no dead-arm added, (d)
   instrumentation cleaned up, (e) no stray `println!` / `dbg!`.
5. **Perf** — perf agent runs. Simplification passes rarely surface
   perf findings (refactors are semantics-preserving) but HIGH/MEDIUM
   findings still close in-pass.
6. **Close findings — zero DEFER.** Per corpus-driven methodology.
7. **Log** — append one row to `tasks/v2-simplification-pass-log.md`
   (parallel to `v2-corpus-driven-pass-log.md`).
8. **Commit** — one commit per pass, message per §Commit templates below.
   Do NOT commit without user approval (CLAUDE.md).

### Iteration budget per pass

Same as corpus-driven: 5 fix iterations max. Exceeding → HALT-AND-FLAG-1
(diagnostic in the source plan is wrong; re-read OPP-X and adjacent OPPs
for context I missed).

### Ordering rules

The queue below is ordered to respect dependencies. Do not reorder unless
you understand what breaks:

- **Phase 1 (dead-code deletion) MUST run first.** Simplifying files
  scheduled for deletion is wasted work; deleting the file is the
  simplification.
- **OPP-L (map_children walker) MUST run before Phase 3 analyzer work**
  — several later passes lean on the walker being available.
- **OPP-D (bail_boundary! macros) MUST run before OPP-H (EmissionError
  union)** — the macro hides the enum name from callers, so the H
  migration reduces to a one-line macro-body change plus mechanical
  call-site rewrites.
- **OPP-A (rename table) MUST run before OPP-G (split by class)** — the
  table classifies which arms leave `render_function_call` for a
  sub-dispatcher.
- **OPP-MM (`wrap_child_with_tail`) MUST run before OPP-BBB (NA-family
  passthrough uses MM's helper).**

## Terminate when

- All 51 passes below in `completed` state.
- Corpus `v2-progress.sh` PASSED count ≥ pre-plan baseline.
- INV1..INV10 active tests green.
- Quality Gate green across the workspace.
- Zero DEFER items outstanding.
- Zero `TODO: simplify` comments referencing this plan's OPP-Xs in the
  code.

## On termination

1. Update `tests/integration/v2_progress.md` with the post-plan row.
2. Add a dev journal entry: `docs/dev_journal/YYYY-MM-DD-simplification-sweep.md`
   summarizing per-phase LOC deltas and any deviations from this plan.
3. Extend `docs/dev-journal-toc.md`.
4. Archive this file to `tasks/archive/v2-simplification-driven-goal-plan.md`.

## Non-goals

- Behavior changes. Every pass is refactor-only; the corpus signal
  must not change value (up or down). If a pass produces a delta on
  `v2-progress.sh`, that's a bug in the refactor.
- Adding features / new function coverage.
- Silencing compiler warnings via `#[allow(...)]` when the fix is
  trivial.
- Legacy transpiler resurrections (deleted 2026-07-05).
- Commits without user approval.
- Cross-pass conflation — one OPP per commit for traceability.

---

# Pass queue

Format per pass:
- **Pass N — OPP-X [size]** target headline.
- **Rationale**: one-line why (from source plan).
- **Files**: touchpoints.
- **Verify**: pass-specific check on top of the base gate.
- **Commit**: `refactor(<scope>): OPP-X <headline>` — one-line stem.

Base verification (every pass): `cargo fmt --check` on touched files;
`cargo check -p <touched-crate>`; `cargo test -p <touched-crate> --lib
--tests`; `tests/scripts/v2-progress.sh` PASSED count monotone.

## Phase 1 — Delete dead code (~1600 LOC)

Run first; simplifying files scheduled for deletion is wasted work.

### Pass 1 — OPP-JJJ [L]  Delete `crates/core/src/types/type_inference.rs`

- **Rationale.** 1313 LOC, 175 match arms; zero callers grep-verified.
  Legacy v1 leftover that survived the 2026-07-05 cleanup because
  `types/` legitimately exports `DataType` etc. INV10 (substrate
  independence) barrier already lists the deleted symbol; deletion is
  behavior-preserving.
- **Files.** Delete `crates/core/src/types/type_inference.rs`. Drop
  `mod type_inference;` and `pub use type_inference::TypeInferenceEngine;`
  from `crates/core/src/types/mod.rs`.
- **Verify.** `git grep 'thunderduck_core::types::TypeInferenceEngine'`
  returns only the INV10 disallowed-imports list (mechanical barrier).
- **Commit.** `refactor(core): delete legacy TypeInferenceEngine (−1313 LOC)`.

### Pass 2 — OPP-LLL [S]  Delete `crates/core/src/types/type_mapper.rs`

- **Rationale.** 71 LOC. `TypeMapper` (Spark→DuckDB type-string mapper
  for CAST/DDL) has zero external callers; τ's emission uses its own
  `render_data_type` in `transpiler_v2/emission.rs`.
- **Files.** Delete `crates/core/src/types/type_mapper.rs`. Drop
  `mod type_mapper;` and `pub use type_mapper::TypeMapper;` from
  `crates/core/src/types/mod.rs`.
- **Verify.** `git grep 'thunderduck_core::types::TypeMapper'` returns
  zero non-self hits.
- **Commit.** `refactor(core): delete unused TypeMapper (−71 LOC)`.

### Pass 3 — OPP-MMM [XS]  Delete SchemaInferrer + tests-only helper

- **Rationale.** `runtime/schema_inferrer.rs` (117 LOC) + its test
  (`crates/core/tests/runtime_integration.rs::struct_field_name_case_is_preserved`,
  ~60 LOC) are used only by that test. The load-bearing property
  (STRUCT field-name round-trip through DuckDB's Arrow schema) is
  already covered by the differential corpus (arr-*, struc-*, map-*
  cases). Redundant surface.
- **Files.** Delete `crates/core/src/runtime/schema_inferrer.rs`. Drop
  `mod schema_inferrer;` + `pub use schema_inferrer::SchemaInferrer;` from
  `crates/core/src/runtime/mod.rs`. Delete the single test in
  `crates/core/tests/runtime_integration.rs`.
- **Verify.** Base gate.
- **Commit.** `refactor(core): delete SchemaInferrer test helper (−180 LOC)`.

### Pass 4 — OPP-NNN [XS]  Delete unused `PipeIfUnresolved` trait

- **Rationale.** `crates/core/src/types/data_type.rs:104-116` defines a
  trait with zero callers (compiler already warns: "trait
  `PipeIfUnresolved` is never used").
- **Files.** Delete trait definition + `DataType` impl (~15 LOC) in
  `crates/core/src/types/data_type.rs`.
- **Verify.** `cargo build -p thunderduck-core` warning count decreases
  by 1.
- **Commit.** `refactor(core): delete unused PipeIfUnresolved trait`.

### Pass 5 — OPP-OOO [XS]  Audit `#[allow(dead_code)]` sites

- **Rationale.** ~9 sites tagged `#[allow(dead_code)]` with a "wired
  when TypedOp::X lands" rationale. If a variant has no scheduled
  landing (no ADR / no open decision), the placeholder is
  dead-forever and should be deleted. If landing is scheduled,
  attach an ADR / decision citation as a comment.
- **Files.** Audit `emission.rs:598, 609, 1537, 5181, 5855`;
  `expression.rs:965`; `analyzer.rs:3119`; `service.rs:425, 584, 620`.
  Delete or annotate per audit.
- **Verify.** Every remaining `#[allow(dead_code)]` in `crates/core`
  and `crates/connect-server` carries an ADR / decision reference in
  the adjacent comment.
- **Commit.** `refactor(core, connect-server): audit dead-code allow markers`.

## Phase 2 — Foundation refactors (~350 LOC + unlock)

Enable Phase 3+ by extracting shared substrate.

### Pass 6 — OPP-L [M]  `Expression::children()` + `map_children()` walker

- **Rationale.** Five full-tree walkers (`resolve_and_stamp`,
  `expression_is_fully_resolved`, `Expression::data_type`,
  `Expression::nullable`, `stamp_column_reference`) each duplicate
  the 29-arm variant enumeration. Every new `Expression` variant
  requires 5 updates. Extract a single `map_children` and each walker
  overrides only the arms it cares about, defaulting to child recursion.
- **Files.** `crates/core/src/transpiler_v2/expression.rs` (add
  `impl Expression { fn children(&self) -> Box<dyn Iterator<Item = &Expression> + '_>; fn map_children<E>(self, f: impl FnMut(Expression) -> Result<Expression, E>) -> Result<Expression, E> }`).
  Rewrite `analyzer.rs::resolve_and_stamp`,
  `analyzer.rs::expression_is_fully_resolved`,
  `expression.rs::data_type`, `expression.rs::nullable`,
  `analyzer.rs::stamp_column_reference` to consume `map_children` for
  default recursion.
- **Verify.** Add a unit test that constructs a nested `Expression`
  (Alias > FunctionCall > Binary > CaseWhen > Literal) and verifies
  `map_children` reaches each leaf exactly once. `analyzer.rs` LOC
  drops ≥ 100.
- **Commit.** `refactor(transpiler): OPP-L Expression::map_children walker`.

### Pass 7 — OPP-D [S]  `bail_boundary!` macros

- **Rationale.** ~40 emission arms + ~30 analyzer arms return an
  `Unsupported*` error via 4-6 lines of struct-literal boilerplate.
  Macros collapse each to one line and centralize the wire-message
  format.
- **Files.** New `crates/core/src/transpiler_v2/macros.rs`: `bail_boundary_fn!`,
  `bail_boundary_op!`, `bail_boundary_expr!`, `bail_boundary_rule!`,
  `bail_boundary_proto!`. Rewrite call sites in `emission.rs`,
  `analyzer.rs`, `parser_v2/v2_lowering.rs`,
  `converter/v2_relation_converter.rs`.
- **Verify.** Existing tests pass verbatim (error strings unchanged).
  `git grep 'return Err(EmissionError::Unsupported'` decreases by ≥ 40.
- **Commit.** `refactor(transpiler): OPP-D bail_boundary! macros`.

### Pass 8 — OPP-HHH [S]  `require_proto` helper for missing-field guards

- **Rationale.** 20 sites write `.ok_or_else(|| UnsupportedProtoShape
  { shape: "X::field::None", reason: "..." })?`. Extract a `Result`
  extension trait so each site becomes
  `x.require_proto("shape-name", "reason")?`. Complements OPP-D by
  covering the option-unwrap idiom.
- **Files.** `crates/core/src/transpiler_v2/macros.rs` (or a new
  `proto_ext.rs`) — add `ProtoFieldExt` trait. Rewrite sites in
  `converter/v2_relation_converter.rs`.
- **Verify.** Wire messages unchanged. ~60 LOC saved across ~20 sites.
- **Commit.** `refactor(connect-server): OPP-HHH require_proto trait`.

### Pass 9 — OPP-H [S]  Merge `EmissionError::Unsupported*` variants

- **Rationale.** 4 variants (`UnsupportedOp`, `UnsupportedExpression`,
  `UnsupportedFunction`, `UnsupportedProtoShape`) share identical
  `(name, reason)` shape, differ only in Display prefix. Collapse to
  `Unsupported { kind: UnsupportedKind, name, reason }` where the enum
  carries the Display prefix. Depends on OPP-D — call-site cost
  amortizes through the macros.
- **Files.** `crates/core/src/transpiler_v2/error.rs`. Update the
  `bail_boundary_*!` macro bodies (single place after Pass 7).
- **Verify.** `git grep 'EmissionError::Unsupported'` reduces to one
  variant name. Tests assert on `err.kind` where they previously
  matched on variant.
- **Commit.** `refactor(transpiler): OPP-H unify EmissionError::Unsupported variants`.

### Pass 10 — OPP-C [S]  `SparkError` enum + `ansi_throw_if` helper

- **Rationale.** Passes 94/95 introduced two near-identical CASE-guarded
  error-emission helpers (`ansi_zero_guard`, `array_index_error_expr`).
  Every future ANSI throw (cast overflow, `to_number` mismatch,
  element_at OOB variants) would spawn a third. Unify to a
  `SparkError` enum + `ansi_throw_if(cond_sql, err, args, fallback)`
  helper.
- **Files.** New `crates/core/src/transpiler_v2/spark_errors.rs` (see
  Pass 11). `emission.rs` — rewrite both existing sites to consume
  `SparkError::{DivideByZero, RemainderByZero, InvalidArrayIndex, …}`.
- **Verify.** Emitted SQL byte-identical for the existing corpus cases
  covering `[SPARK-DIVIDE_BY_ZERO]`, `[SPARK-REMAINDER_BY_ZERO]`,
  `[SPARK-INVALID_ARRAY_INDEX_IN_ELEMENT_AT]`.
- **Commit.** `refactor(transpiler): OPP-C SparkError enum + ansi_throw_if`.

### Pass 11 — OPP-J [XS]  Move Spark error constants to `spark_errors` module

- **Rationale.** `SPARK_DIVIDE_BY_ZERO_MSG` and friends are scattered
  in `emission.rs`. Adjacent to Pass 10's `SparkError` enum, they
  become associated data.
- **Files.** `crates/core/src/transpiler_v2/spark_errors.rs` — hold
  the const strings. `emission.rs` — replace inline consts with
  `spark_errors::*` references.
- **Verify.** Base gate.
- **Commit.** `refactor(transpiler): OPP-J move Spark error consts to spark_errors module`.

### Pass 12 — OPP-O [XS]  Extract shared `dedup_names` to `types/`

- **Rationale.** PySpark `_dedup_names` parity helper exists in two
  places: `arrow_schema_stamp.rs` and `emission.rs::render_data_type`.
  Rule-of-two duplication with a documented sync invariant — extract.
  Value-level `Vec<String>` in/out — INV10-safe.
- **Files.** New `crates/core/src/types/pyspark_parity.rs` — free
  function `dedup_names(names: &[String]) -> Vec<String>`. Both
  callers import it. `types/mod.rs` exports.
- **Verify.** Both call sites go from private-helper call to
  `crate::types::pyspark_parity::dedup_names(...)`. Existing tests
  covering the dedup edge cases stay green.
- **Commit.** `refactor(core): OPP-O extract shared dedup_names helper`.

## Phase 3 — Analyzer normalization (~350 LOC)

### Pass 13 — OPP-V [S]  Uniform `analyze_node` arm shape (extract 5 fattest)

- **Rationale.** `analyze_node` has 32 arms; median 14 LOC, but SetOp
  (165 LOC), Join (88), NaFill (47), WithColumns (44), ToDf (37)
  inline complex logic. Extract to `analyze_set_op` / `analyze_join`
  / `analyze_na_fill` / `analyze_with_columns` / `analyze_to_df`.
  `dispatch_op` (emission.rs:70-200) is the shape template — every
  arm becomes a 2-3 line delegation. `analyze_node` shrinks from
  ~450 LOC to ~40 LOC.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs`.
- **Verify.** Full corpus non-regression. Test that a randomly
  malformed variant still surfaces the same `AnalyzerError`.
- **Commit.** `refactor(transpiler): OPP-V uniform analyze_node arm shape`.

### Pass 14 — OPP-WW [S]  Passthrough schema helper (9 sites)

- **Rationale.** Filter, Sort, Limit, Deduplicate, NaFill, NaDrop,
  NaReplace, Sample, SampleBy, AliasedRelation all do
  `analyze_input(...)?; passthrough resolved_schema; wrap in new op`.
  Extract `passthrough_schema_arm(input, base_types, |typed_input|
  build_op)` — 9 arms shrink from ~15 LOC to ~3 LOC each. ~120 LOC
  saved.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs`.
- **Verify.** Base gate + full corpus non-regression.
- **Commit.** `refactor(transpiler): OPP-WW passthrough schema helper (9 sites)`.

### Pass 15 — OPP-II [S]  Bounded schema-passthrough resolver

- **Rationale.** Filter, Sort, Limit each need `resolve_expr_of_type +
  schema_passthrough`. Generic `analyze_input_with_schema_passthrough<T>`
  helper accepts an op-specific `T` (condition / order / limit) and
  a closure to build the specific TypedOp variant. ~30 LOC saved
  across 3 arms.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs`.
- **Verify.** Base gate + full corpus non-regression.
- **Commit.** `refactor(transpiler): OPP-II bounded schema-passthrough resolver`.

## Phase 4 — Emission tables + helpers (~250 LOC)

### Pass 16 — OPP-A [S]  Data-driven fn-name rename tables

- **Rationale.** ~15 pure-rename arms in the `_ => &name_lower`
  fallthrough of `render_function_call`. Extract two tables:
  `NATIVE_RENAMES: &[(&str, &str)]` (`nvl → coalesce`, `substr →
  substring`, ...) and `EXTENSION_ROUTES: &[(&str, &str)]` (`hash →
  spark_hash`, `try_divide → spark_try_divide`, ...). Adding a Spark
  alias becomes a one-line table entry.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** Corpus non-regression. `git grep '=> "coalesce"'` etc.
  reduces to table entries.
- **Commit.** `refactor(emission): OPP-A native-rename + extension-route tables`.

### Pass 17 — OPP-BB [S]  Data-driven `type_inference` fn-name arms

- **Rationale.** `type_inference::function_return_type` has 41 arms;
  ~20 are `"name1" | "name2" => TypeConst` pure-map. Extract
  `SCALAR_FN_RETURN_TYPE: &[(&[&str], DataType)]`. Arg-dependent arms
  (sum/avg with decimal promotion) and compound-type synthesis
  (arrays_zip, inline_field) stay as arms.
- **Files.** `crates/core/src/transpiler_v2/type_inference.rs`.
- **Verify.** Corpus non-regression on function return types.
- **Commit.** `refactor(type-inference): OPP-BB scalar-fn return-type table`.

### Pass 18 — OPP-GG [S]  `null_propagate(guard, body)` helper

- **Rationale.** 20 emission arms hand-write `CASE WHEN ({expr}) IS
  NULL THEN NULL ELSE <fn>({expr}, ...) END` for Spark-parity null
  propagation. Nine also handle empty-array with a `WHEN len({arr})
  = 0 THEN ...` clause. Extract `null_propagate(guard, body)` +
  `null_or_empty_propagate(arr, empty, body)`. ~40 LOC saved.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** SQL byte-identical on the 20+ affected corpus cases.
- **Commit.** `refactor(emission): OPP-GG null_propagate helper`.

### Pass 19 — OPP-FF [XS]  `check_arity` helper

- **Rationale.** ~30 function arms hand-write
  `if f.args.len() != N { return Err(...) }`. Extract
  `check_arity(f, &[expected]) -> Result<(), EmissionError>`.
  Reduces boilerplate 4-5 lines × 30 sites.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** Wire error messages unchanged (macro uses
  `bail_boundary_fn!` from OPP-D under the hood).
- **Commit.** `refactor(emission): OPP-FF check_arity helper`.

### Pass 20 — OPP-MM [S]  `wrap_child_with_tail` helper

- **Rationale.** `render_filter`, `render_sort`, `render_limit`,
  `render_tail` share `SELECT * FROM ({child}) AS __td_<name> {tail}`
  boilerplate. Extract `wrap_child_with_tail(input, alias, tail)`.
  ~25 LOC saved. Names the "wrap child in subquery" pattern in one
  place; future alias-convention changes edit one line.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** Emitted SQL byte-identical.
- **Commit.** `refactor(emission): OPP-MM wrap_child_with_tail helper`.

### Pass 21 — OPP-BBB [S]  Emission passthrough for NA family

- **Rationale.** `render_na_fill`, `render_na_drop`, `render_na_replace`
  share the "wrap child + apply transform" shape. Post-OPP-MM,
  they consume `wrap_child_with_tail`. ~30 LOC saved.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** SQL byte-identical on NA family corpus cases.
- **Commit.** `refactor(emission): OPP-BBB NA-family emission via wrap helper`.

### Pass 22 — OPP-SS [S]  `render_expr_list_{join,comma}` helpers

- **Rationale.** ~15 emission loops iterate `Vec<Expression>` +
  comma-join. Extract `render_expr_list_join(exprs, schema, sep)` +
  specialization `render_expr_list_comma(exprs, schema)`. ~40 LOC
  saved.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** SQL byte-identical.
- **Commit.** `refactor(emission): OPP-SS render_expr_list helpers`.

### Pass 23 — OPP-PP [XS]  `render_args_ordered` helper (marginal)

- **Rationale.** ~50 emission arms write `let a = render_expr(&f.args[0],
  schema)?; let b = render_expr(&f.args[1], schema)?;`. Extract
  `render_args_ordered(f, schema) -> Result<Vec<String>>` that renders
  all args in one pass. Save 1 line per arg × ~50 sites. Marginal —
  callable per-arm choice.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** SQL byte-identical.
- **Commit.** `refactor(emission): OPP-PP render_args_ordered helper`.

### Pass 24 — OPP-EE [XS]  Normalize `FunctionCall.name` at ingress

- **Rationale.** 13 emission sites call `f.name.to_ascii_lowercase()`
  before match. Push normalization to `V2ExpressionConverter` + `parser_v2`
  so `FunctionCall.name` is lowercase from ingress. Downstream drops
  all `to_ascii_lowercase()` calls. ~15 LOC + one footgun removed.
- **Files.** `crates/connect-server/src/converter/v2_relation_converter.rs`,
  `crates/core/src/parser_v2/v2_lowering.rs`,
  `crates/core/src/transpiler_v2/{emission,type_inference}.rs`.
- **Verify.** Corpus non-regression; error messages lowercase which
  matches Spark's error text.
- **Commit.** `refactor(converter, transpiler): OPP-EE normalize FunctionCall.name at ingress`.

### Pass 25 — OPP-G [M]  Split `render_function_call` by class

- **Rationale.** `render_function_call` mixes aggregate, scalar,
  generator, hash, math, string, temporal, complex-type, JSON,
  regexp — ~2000 lines. After OPP-A and OPP-BB extract the tables,
  route the rest through class-specific dispatchers:
  `render_aggregate_fn`, `render_string_fn`, `render_math_fn`,
  `render_temporal_fn`, `render_complex_type_fn`, `render_json_fn`.
  Classification via a top-level function-name → class table.
- **Files.** `crates/core/src/transpiler_v2/emission.rs` — split into
  `emission/functions/{aggregate,string,math,temporal,complex,json}.rs`
  or hold as internal fns behind `emission.rs`. Author's choice per
  architect step.
- **Verify.** Corpus non-regression. Duplicate-classification produces
  compile error (a function name in two classes fails to compile).
- **Commit.** `refactor(emission): OPP-G split render_function_call by class`.

## Phase 5 — Converter normalization (~250 LOC)

### Pass 26 — OPP-F [M]  `V2RelationConverter::convert` uniform delegation

- **Rationale.** 30+ `RelType::X(x) => self.convert_x(x)` arms — some
  are 2-line delegations, others are inlined 5-30 line handlers.
  Normalize to uniform 2-line delegation shape (`RelType::X(x) =>
  self.convert_x(x)`). Complements OPP-V.
- **Files.** `crates/connect-server/src/converter/v2_relation_converter.rs`.
- **Verify.** Base gate + full corpus non-regression.
- **Commit.** `refactor(connect-server): OPP-F uniform RelType delegation`.

### Pass 27 — OPP-GGG [S]  `V2ExpressionConverter::convert` uniform delegation

- **Rationale.** Same shape as OPP-F for the expression converter.
  `V2ExpressionConverter::convert` has inline handlers for
  UnresolvedStar, LambdaFunction, Window, UnresolvedExtractValue,
  ExpressionString, UpdateFields, UnresolvedRegex. Extract into
  per-variant `convert_X`. ~200 LOC moved out of central dispatch.
- **Files.** `crates/connect-server/src/converter/v2_relation_converter.rs`.
- **Verify.** Base gate + full corpus non-regression.
- **Commit.** `refactor(connect-server): OPP-GGG uniform ExprType delegation`.

### Pass 28 — OPP-U [XS]  `convert_input` unified via macro or trait

- **Rationale.** Almost every `convert_X` starts with
  `let input = self.convert_input(x.input.as_deref(), "X")?;`.
  Extract a macro or generic trait that hoists this to the top-level
  dispatch. Saves one line per method × ~30 methods.
- **Files.** `crates/connect-server/src/converter/v2_relation_converter.rs`.
- **Verify.** Base gate.
- **Commit.** `refactor(connect-server): OPP-U hoist convert_input call`.

### Pass 29 — OPP-M [S]  `PUNTED_OPERATOR_MAP` table

- **Rationale.** Multiple `RelType::X` arms return a canned "punted
  operator" error with the reason embedded in the arm. Extract
  `PUNTED_OPERATOR_MAP: &[(&str, &str)]` (reltype name → reason);
  arms that only punt collapse to a single fallthrough
  `RelType::* => return Err(punted_op(t))`.
- **Files.** `crates/connect-server/src/converter/v2_relation_converter.rs`.
- **Verify.** Wire error messages unchanged.
- **Commit.** `refactor(connect-server): OPP-M PUNTED_OPERATOR_MAP table`.

### Pass 30 — OPP-X [S]  Arrow primitive-type macro in `local_relation_to_values_sql`

- **Rationale.** Large match on Arrow `DataType` variants for LocalRelation
  payloads. Primitives (Int8/16/32/64, UInt8/…, Float32/64) share
  identical arm shape. Extract via a macro; loud-fail semantics
  (CLAUDE.md gotcha #9) preserved. ~30 LOC saved.
- **Files.** `crates/connect-server/src/converter/v2_relation_converter.rs`.
- **Verify.** Corpus non-regression on LocalRelation payload cases
  (`values_row_*` fixtures).
- **Commit.** `refactor(connect-server): OPP-X Arrow primitive macro for LocalRelation`.

## Phase 6 — Small helpers + polish (~200 LOC)

### Pass 31 — OPP-K [XS]  `Literal::is_zero()` / `as_i64()` methods

- **Rationale.** `is_nonzero_literal` walks `LiteralValue` variants
  (Byte/Short/Int/Long/Float/Double/Decimal) to check for zero. Move
  to `impl LiteralValue { fn is_zero(&self) -> bool; fn as_i64(&self)
  -> Option<i64> }` next to the `LiteralValue` definition. Future
  predicates (is_negative, is_positive) share the same locus.
- **Files.** `crates/core/src/transpiler_v2/expression.rs`.
- **Verify.** Base gate.
- **Commit.** `refactor(transpiler): OPP-K LiteralValue predicate methods`.

### Pass 32 — OPP-QQ [XS]  Extend `is_nonzero_literal` into `LiteralValue`

- **Rationale.** Companion to OPP-K. Move `is_nonzero_literal` out of
  `emission.rs` into `impl LiteralValue`. Adjacent to enum definition.
- **Files.** `crates/core/src/transpiler_v2/expression.rs`, `emission.rs`.
- **Verify.** Base gate.
- **Commit.** `refactor(transpiler): OPP-QQ inline is_nonzero_literal into LiteralValue`.

### Pass 33 — OPP-RR [XS]  `SortDirection::sql()` + `NullOrdering::sql()`

- **Rationale.** `render_sort_key` hand-writes 2-variant matches for
  each. Move to `impl SortDirection { fn sql(&self) -> &'static str }`
  and same for `NullOrdering`. Callers become `so.direction.sql()`.
- **Files.** `crates/core/src/transpiler_v2/expression.rs`, `emission.rs`.
- **Verify.** Base gate.
- **Commit.** `refactor(transpiler): OPP-RR SortDirection.sql() + NullOrdering.sql()`.

### Pass 34 — OPP-CCC [XS]  `SetOpKind::sql_keyword()`

- **Rationale.** Same shape as OPP-RR for `SetOpKind`. `render_set_op`
  matches the enum → SQL keyword; move to
  `impl SetOpKind { fn sql_keyword(&self) -> &'static str }`.
- **Files.** `crates/core/src/transpiler_v2/expression.rs`, `emission.rs`.
- **Verify.** Base gate.
- **Commit.** `refactor(transpiler): OPP-CCC SetOpKind::sql_keyword()`.

### Pass 35 — OPP-P [XS]  Extract JSONPath-unsafe-char predicate

- **Rationale.** Same predicate (JSONPath unsafe-char check) lives in
  `analyzer.rs::expand_json_tuple_projections` and
  `emission.rs::json_tuple_field`. Extract to one locus.
- **Files.** `crates/core/src/transpiler_v2/{analyzer,emission}.rs`, or
  new sibling.
- **Verify.** Base gate.
- **Commit.** `refactor(transpiler): OPP-P extract JSONPath predicate`.

### Pass 36 — OPP-Y [XS]  Move struct-field dedup name synthesis to `types/`

- **Rationale.** Extension of OPP-O. `render_data_type::Struct` calls
  `dedup_struct_field_names`; the copy in `arrow_schema_stamp` needs
  to stay in sync. After OPP-O extracts `dedup_names`, this pass
  wires `render_data_type` to consume it via the shared locus.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`,
  `crates/core/src/types/pyspark_parity.rs`.
- **Verify.** Base gate + corpus non-regression on struc-* cases.
- **Commit.** `refactor(emission): OPP-Y wire render_data_type dedup through pyspark_parity`.

### Pass 37 — OPP-AA [XS]  Canonical boundary-message templates

- **Rationale.** ~30 boundary-reject arms invent their own phrasing.
  Introduce a small set of canonical templates
  (`unsupported_by_parser`, `not_yet_implemented`,
  `requires_session_hook`, `arrow_conversion_gap`) as `const &str`.
  Every arm picks one; users see recognizable phrasing across arms.
- **Files.** `crates/core/src/transpiler_v2/{analyzer,emission}/messages.rs`
  (new). Rewrite arms.
- **Verify.** Base gate. Wire messages remain informative;
  user-visible reasons stay Spark-parity.
- **Commit.** `refactor(transpiler): OPP-AA canonical boundary-reject templates`.

### Pass 38 — OPP-JJ [XS]  Boundary-reject class discipline helpers

- **Rationale.** Some sites use `AnalyzerError::Other` (Spark-emulated)
  for what should be Thunderduck-boundary and vice versa. Extract
  `analyzer_boundary_reject(rule, reason)` + `spark_emulated_error(reason)`
  so the class discipline shows at each call site.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs`, `error.rs`.
- **Verify.** Base gate + reviewer audits ADR-022 category tags on
  call sites.
- **Commit.** `refactor(analyzer): OPP-JJ boundary-reject class discipline helpers`.

### Pass 39 — OPP-KK [XS]  Audit `analyze_{describe,summary,freq_items}`

- **Rationale.** Confirm the three stats analyzers share
  `build_stats_output_schema`; verify no leftover duplication.
  Pass 5 flagged as "not worth further extraction" but the audit
  itself pins the invariant.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs`.
- **Verify.** Audit note in the pass log; no code change if not
  warranted.
- **Commit.** `refactor(analyzer): OPP-KK audit stats-analyzer sibling helpers`
  (or drop-pass if audit shows no change needed — note in pass log).

### Pass 40 — OPP-ZZ [XS]  Unify `materialise_stats_cols` default policy

- **Rationale.** `analyze_summary` uses `DEFAULT_SUMMARY_STATS`;
  `analyze_describe` uses input cols. Extend `materialise_stats_cols`
  with a default-policy parameter (`AllInputColumns` vs
  `DefaultList(&[&str])`). Callers share the helper.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs`.
- **Verify.** Corpus non-regression on summary/describe cases.
- **Commit.** `refactor(analyzer): OPP-ZZ unify materialise_stats_cols default policy`.

### Pass 41 — OPP-DDD [XS]  `CaseInsensitiveNameMap` helper (assessment pass)

- **Rationale.** Multiple analyzer arms (WithColumns, WithColumnsRenamed,
  DropColumns, Aggregate) hand-roll `to_lowercase()` for
  case-insensitive lookup. Weakly recommend a helper; may not
  justify the surface area. This pass is an **assessment**: audit
  the sites, extract if the shape repeats ≥3 times cleanly, else
  drop.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs`.
- **Verify.** Base gate. If dropped, log the reasoning.
- **Commit.** `refactor(analyzer): OPP-DDD case-insensitive name map helper`
  (or drop-pass — note in pass log).

### Pass 42 — OPP-W [XS]  Extract `is_flat_join_boundary` predicate

- **Rationale.** CLAUDE.md gotcha #4 flags the semi/anti flat-chain
  break as a recurring bug class. Extract `is_flat_join_boundary(jt:
  JoinType) -> bool` predicate + a matching unit test to pin the
  invariant in one place.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** Unit test covers semi/anti + inner/outer/left/right.
- **Commit.** `refactor(emission): OPP-W extract is_flat_join_boundary predicate`.

### Pass 43 — OPP-CC [XS]  Fold `render_expr` unresolved-reject arms

- **Rationale.** 5 arms in `render_expr` are structurally identical
  boundary-rejects (`UnresolvedColumn`, `UnresolvedRegex`,
  `InSubquery`, `ExistsSubquery`, `ScalarSubquery`). Extract
  `unresolved_expr_reject(shape) -> EmissionError`. ~10 LOC saved.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** Wire messages unchanged.
- **Commit.** `refactor(emission): OPP-CC unify unresolved-expr rejects`.

### Pass 44 — OPP-XX [XS]  Function-name arm cluster comments

- **Rationale.** Add `// ── Pure renames ──`, `// ── Arity-gated ──`,
  `// ── Null-propagate ──` cluster headers above the arm-groups
  post-OPP-A/OPP-FF/OPP-GG. Comment-only — clarifies the file
  structure. Weak (may be skipped if OPP-G's class split already
  clarifies).
- **Files.** `crates/core/src/transpiler_v2/emission.rs` (or the
  per-class files after OPP-G).
- **Verify.** Base gate.
- **Commit.** `docs(emission): OPP-XX function-arm cluster headers`.

### Pass 45 — OPP-YY [XS]  `FN_CATEGORY` table classifier (assessment)

- **Rationale.** Pass 8 marked as "marginal — prefer OPP-A + OPP-GG
  as separate simplifications." Assessment pass: post-Passes
  16/18/25, does a top-level `FN_CATEGORY: &[(&str, FnCategory)]`
  table add value over the class dispatchers OPP-G established?
  If yes, extract; if no, log as consciously-dropped.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** Base gate.
- **Commit.** `refactor(emission): OPP-YY FN_CATEGORY classifier`
  (or drop-pass note).

### Pass 46 — OPP-R [XS]  `lower_binary_op` table (parser_v2)

- **Rationale.** `parser_v2::v2_lowering::lower_binary_op` has 17
  arms of `BinaryOperator::Plus => BinaryOp::Add` shape. Extract
  `BINARY_OP_MAP: &[(sqlparser::BinaryOperator, BinaryOp)]`. Adding
  a new operator becomes a one-line table entry.
- **Files.** `crates/core/src/parser_v2/v2_lowering.rs`.
- **Verify.** Base gate + parser tests.
- **Commit.** `refactor(parser): OPP-R BINARY_OP_MAP table`.

### Pass 47 — OPP-FFF [S]  `parse_type_str` primitive table

- **Rationale.** `parse_type_str` in `type_converter.rs` parses Spark
  DDL type strings. 12 primitive names flat-matched; extract
  `PRIMITIVE_TYPE_MAP: &[(&[&str], DataType)]` (case-insensitive
  alias table). Decimal + Array parsing stay as helpers.
- **Files.** `crates/connect-server/src/converter/type_converter.rs`.
- **Verify.** Base gate. Adding a primitive → one-line table entry.
- **Commit.** `refactor(connect-server): OPP-FFF PRIMITIVE_TYPE_MAP table`.

### Pass 48 — OPP-B [S]  Summary-stat agg-format table

- **Rationale.** `emission.rs::stat_to_agg_expr` has 5 arms all
  shaped `"<stat>" => format!("CAST(<AGG>({quoted_col}) AS
  VARCHAR)")`. Extract `SUMMARY_STAT_AGG: &[(&str, &str)]` + a
  small formatter.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`.
- **Verify.** SQL byte-identical on summary/describe cases.
- **Commit.** `refactor(emission): OPP-B SUMMARY_STAT_AGG table`.

### Pass 49 — OPP-PPP [XS]  Delete `type Schema = StructType` alias

- **Rationale.** `analyzer.rs:63` defines `pub type Schema =
  StructType`. Used inconsistently: 24 `Schema` sites, 38 `StructType`
  sites. Delete the alias; use `StructType` everywhere — one type
  name, no aliased indirection.
- **Files.** `crates/core/src/transpiler_v2/analyzer.rs` + 24 call sites.
- **Verify.** Base gate.
- **Commit.** `refactor(transpiler): OPP-PPP delete Schema type alias`.

### Pass 50 — OPP-RRR [XS]  Retire outdated section-reference comments

- **Rationale.** `emission.rs` module docstring references `§5.3`,
  `§5.4`, `§5.6`, `§4.2 first item`, `checklist §4.2`, `Decision
  13-A` that point at retired planning docs. Rewrite to reference
  active ADRs (ADR-009, ADR-021, ADR-022) or delete.
- **Files.** `crates/core/src/transpiler_v2/emission.rs`, plus any
  siblings that carry retired section pointers.
- **Verify.** `git grep -nE '§[0-9]+\.[0-9]+' crates/core` returns
  zero hits or only well-known active references.
- **Commit.** `docs(transpiler): OPP-RRR retire outdated section-reference comments`.

### Pass 51 — OPP-QQQ [XS]  Test-fixture pattern audit

- **Rationale.** `analyzer_fixtures.rs` (535 LOC, 19 fns) — audit
  whether callers duplicate setup. Extract a `TestCase` builder if
  the shape repeats ≥3 times cleanly, else drop the pass. Test-code
  cleanup is worth it only when it lowers the barrier to writing new
  tests.
- **Files.** `crates/core/src/transpiler_v2/analyzer_fixtures.rs` +
  callers.
- **Verify.** Base gate. If dropped, log reasoning.
- **Commit.** `refactor(test): OPP-QQQ analyzer fixtures builder`
  (or drop-pass note).

---

# `/goal` prompt template

Paste into `/goal`. ≤4000 chars per template block.

```
/goal Drive Thunderduck v2 simplification queue to zero via iterated /new-feature (refactor-first) passes

**Primary goal.** All 51 numbered passes in `tasks/v2-simplification-driven-goal-plan.md` §Pass queue complete, in order. `tests/scripts/v2-progress.sh` PASSED count monotone across every pass (simplification is refactor-only; corpus signal must not change).

**Secondary goal (non-negotiable).** Zero DEFER. Every review + perf finding closes in the pass it surfaces. No new compiler warnings on files a pass modifies. No `#[allow(dead_code)]` added to silence warnings.

**Methodology (mandatory, read at pass start):** `tasks/v2-simplification-driven-goal-plan.md` §Methodology. Queue processing, not case picking — each pass is a predetermined refactor from `.agent-output/simplification-plan.md`.

**Design authority:** `docs/thunderduck-rearchitect-ADRs.md` (ADR-000..ADR-022) + `crates/core/src/transpiler_v2/invariants.rs` (INV1..INV10). Every refactor preserves observable behavior; the corpus is the fitness gate.

**Baseline:** current git HEAD; run `v2-progress.sh` at start; record PASSED count. That count is the floor for the entire plan.

**Loop (per pass — see plan file for the OPP-X entry):**
1. **Read.** `.agent-output/simplification-plan.md` § OPP-X (problem, proposal, quantified impact).
2. **Architect.** Dispatch `/new-feature` skill. Architect reads OPP-X + target files listed for the pass; produces concrete file-by-file change plan; cites ADRs / invariants; names ordering constraints against prior/subsequent passes.
3. **Implement.** Coder applies plan. Quality Gate (CLAUDE.md §Quality Gate) green. `v2-progress.sh` PASSED count ≥ baseline. No new compiler warnings on modified files.
4. **Review + Perf.** Both agents run. Reviewer verifies OPP-X's intended shape actually landed, invariants intact, no dead-arm added. ALL findings CLOSE_NOW_IN_THIS_PASS.
5. **Log.** Append entry to `tasks/v2-simplification-pass-log.md` (Pass N — OPP-X, files touched, LOC delta, corpus baseline, corpus after, warning delta).
6. **Commit** per §Commit templates in the plan file. Do NOT commit without user approval (CLAUDE.md).

**Per-pass HALT-AND-FLAG:**
- 5 fix iterations without target refactor landing cleanly → re-read OPP-X + adjacent OPPs for missed context.
- Corpus regression detected → HALT. Refactor is not behavior-preserving; roll back and re-plan.
- OPP-X target file has been mutated by an out-of-band commit → re-read to reconcile before proceeding.
- Ordering constraint violation surfaced (e.g., attempting Pass 20 before Pass 6 landed the walker) → jump back to the missing dependency.

**Terminate when:** all 51 passes green; corpus `v2-progress.sh` PASSED count ≥ baseline; INV1..INV10 active + green; Quality Gate green; zero DEFER items outstanding.

**Non-goals:** behavior changes (refactor-only), feature additions, dead-code arms, silencing warnings via `#[allow(...)]`, legacy resurrections, commits without user approval, cross-pass conflation (one OPP per commit).

**On termination:** update `v2_progress.md`; add dev journal entry `docs/dev_journal/YYYY-MM-DD-simplification-sweep.md`; extend `docs/dev-journal-toc.md`; archive this plan to `tasks/archive/`. **Do NOT commit without user approval** (CLAUDE.md).
```

---

# Design notes

**Why queue processing, not case picking.** Corpus-driven work picks
targets from the failure cluster (highest cascade wins). Simplification
work has a predetermined queue — the source plan already identified,
quantified, and ordered the 68 opportunities across 16 refinement
passes. The `/goal` loop consumes the queue; the "pick" step reduces to
"read the next OPP-X entry."

**Why the specific ordering.** Dependencies are load-bearing:
- Phase 1 (dead-code deletion) must run first because simplifying files
  scheduled for deletion is wasted work.
- OPP-L (`map_children` walker) unblocks Phase 3 analyzer work by
  removing the "5-walker sync" tax on `analyzer.rs` refactors.
- OPP-D (`bail_boundary!` macros) must land before OPP-H
  (`EmissionError` union) because the macros hide the enum name from
  ~70 call sites — the H migration reduces to a one-line macro-body
  change.
- OPP-A + OPP-BB (fn-name tables) must land before OPP-G (class split)
  because the tables classify which arms leave `render_function_call`
  for a sub-dispatcher.
- OPP-MM (`wrap_child_with_tail`) must land before OPP-BBB (NA-family
  passthrough consumes MM's helper).

**Why 51 passes for 57 opportunities.** Some opportunities merge with
others rather than getting their own pass:
- OPP-E (analyzer per-op trait) — merged into OPP-V. The trait
  approach loses exhaustive-match; OPP-V's uniform-arm approach keeps
  it. E is retired at write-time.
- OPP-LL (dispatch_op is template) — observation supporting OPP-V, not
  a separate action.
- OPP-AAA (`build_stats_output_schema` audit) — observation supporting
  OPP-KK, not a separate action.

**Why some passes are "assessment" passes.** OPP-DDD, OPP-KK, OPP-YY,
OPP-QQQ, OPP-XX are weakly recommended in the source plan. The
assessment pass runs the audit; if the extraction doesn't warrant the
surface area, the pass is closed as a drop-pass with the reasoning
logged. This keeps the queue honest — "we looked" is a valid
completion state.

**Why one commit per pass.** Traceability. When a later corpus
regression surfaces, `git bisect` on this range identifies the
offending refactor immediately. The alternative (multiple OPPs per
commit) makes bisect useless.

**Why the corpus is the fitness gate for a refactor-only plan.**
Simplification passes are behavior-preserving by contract. A corpus
delta on `v2-progress.sh` — up OR down — means the refactor changed
observable behavior. Down is a regression. Up is a bug fix hiding
inside a refactor — which is fine to keep, but should be flagged
in the pass log and split into its own commit before the refactor
commit lands.

## References

- Source analysis: `.agent-output/simplification-plan.md`.
- Sibling `/goal`: `tasks/v2-corpus-driven-goal-prompt-template.md`.
- Methodology backdrop: `tasks/v2-corpus-driven-iteration-methodology.md`.
- Design authority: `docs/thunderduck-rearchitect-ADRs.md`.
- Invariants: `crates/core/src/transpiler_v2/invariants.rs`.
- Quality Gate: `CLAUDE.md` §Quality Gate.
- Pass log (created on first pass): `tasks/v2-simplification-pass-log.md`.
