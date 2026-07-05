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

## Pass 6 — OPP-L (2026-07-05)

Extract the `Expression::children()` iterator and the
`Expression::map_children()` structural map into `transpiler_v2/
expression.rs`, then rewrite `resolve_and_stamp` and
`expression_is_fully_resolved` in `transpiler_v2/analyzer.rs` to
consume the walker via a wildcard default arm. Unlocks Phase 3.

**Scope deviation from the plan (documented, not silent):**
The plan lists 5 walkers for the rewrite: `resolve_and_stamp`,
`expression_is_fully_resolved`, `Expression::data_type`,
`Expression::nullable`, `stamp_column_reference`. Only the first two
are natural fits — the last three are not walker rewrites:
- `Expression::data_type` and `Expression::nullable` have variant-
  specific type-derivation logic for almost every arm (Binary type
  promotion, FunctionCall type inference, Cast fixed type, Window
  type from the underlying agg, etc.). There is no natural "default
  recursion" they can fall through to — the arms are not duplicated
  recursion, they are per-variant type-derivation logic.
- `stamp_column_reference` operates on `ColumnReference` alone; it is
  not a full-tree walker.

The plan's stated LOC target ("analyzer.rs LOC drops ≥ 100") is met
by the two-walker rewrite: analyzer.rs drops **182 LOC** (5907 →
5725). The maintenance-cost reduction ("every new variant needs 5
updates") lands proportionally — 2 of the 5 walkers now delegate to
`map_children`, so new variants require updating only 3 of the
original 5.

- **Files touched.**
  - `crates/core/src/transpiler_v2/expression.rs` — add
    `Expression::children()` and `Expression::map_children()` (walker
    substrate; +226 LOC including 2 new unit tests exercising the
    Alias > FunctionCall > Binary > CaseWhen > Literal shape and the
    Window frame-boundary-skip invariant).
  - `crates/core/src/transpiler_v2/analyzer.rs` — rewrite
    `resolve_and_stamp` (~180 LOC → ~50 LOC with UpdateFields
    validation preserved) and `expression_is_fully_resolved` (~70 LOC
    → ~20 LOC via `expr.children().all(...)`).
- **LOC delta.** analyzer.rs −182; expression.rs +226 (walker +
  tests). Net +44 LOC across the two files. The walker substrate is
  the point — future walkers (Phase 3 analyzer normalization,
  Passes 13-15) reuse it instead of duplicating recursion.
- **Corpus.** Baseline verified at 314 pre-commit (running
  `v2-progress.sh` in the pass session). Behavior-preserving:
  `map_children`'s per-variant recursion matches the deleted walker
  arms 1-for-1, and both walkers explicitly custom-case the same
  punt set (`InSubquery`, `ExistsSubquery`, `ScalarSubquery`,
  `Lambda`, `LambdaVariable`, `RawSql`, `Interval`,
  `UnresolvedRegex`).
- **Warnings.** `cargo check -p thunderduck-core` clean, zero
  warnings. No delta.
- **Gate.** `cargo test -p thunderduck-core --lib --tests` → 448
  pass / 0 fail / 4 ignored (+2 vs Pass 5 = new walker tests) + 0
  pass / 4 ignored (runtime_integration).
- **Fmt drift note.** Scoped `rustfmt --edition 2021 --check` reports
  1 pre-existing drift block (`analyzer.rs:710` FileScan error
  branch). HEAD-vs-WT block counts identical (analyzer.rs = 1 / 1,
  expression.rs = 0 / 0), so Pass 6 introduces zero new drift.

## Pass 7 — OPP-D (2026-07-05)

Introduce five `bail_boundary_*!` macros in a new
`crates/core/src/transpiler_v2/macros.rs`, then rewrite ~160
hand-written `return Err(EmissionError::Unsupported*)` (and
tail-`Err(...)` match arm) sites in `emission.rs`,
`parser_v2/v2_lowering.rs`, `parser_v2/mod.rs`,
`converter/v2_relation_converter.rs`, plus the three
`AnalyzerError::UnsupportedRule` sites in `analyzer.rs`. Wire error
`Display` output is byte-identical: each macro `$field.to_owned()`s
its arguments and expands to the same struct literal the callers
wrote by hand. The macros expand to `return Err(...)` **without**
trailing semicolon so they compose both at statement position and as
a match-arm tail expression (`Foo => bail_boundary_op!(...)` —
`return` is `!` and coerces).

**`bail_boundary_rule!` decision.** Added. `analyzer.rs` has exactly
3 clean `return Err(AnalyzerError::UnsupportedRule { rule, reason })`
sites — meets the ≥ 3-clean-site threshold. `PuntedOperator` (2
sites) uses a different field shape (`op/reason` not `rule/reason`)
and 2 < 3, so no dedicated `bail_boundary_punt!` was added.

- **Files touched.**
  - `crates/core/src/transpiler_v2/macros.rs` — **new**, +98 LOC.
    Five `#[macro_export]` macros: `bail_boundary_op!`,
    `bail_boundary_expr!`, `bail_boundary_fn!`,
    `bail_boundary_proto!`, `bail_boundary_rule!`. Each accepts a
    trailing comma (`$(,)?`) so `rustfmt`-multi-line invocations
    parse.
  - `crates/core/src/transpiler_v2/mod.rs` — +1 line: `mod macros;`.
  - `crates/core/src/transpiler_v2/emission.rs` — 108 rewrites (102
    return sites + 6 tail-`Err` match arms). INV10 positive test
    `inv10_emission_imports_are_typed` widened to accept
    `use crate::bail_boundary_*` alongside the existing
    `use crate::types::*` allow-list.
  - `crates/core/src/parser_v2/v2_lowering.rs` — 28 rewrites (16
    return + 12 tail-`Err`).
  - `crates/core/src/parser_v2/mod.rs` — 2 rewrites (1 return + 1
    tail-`Err`).
  - `crates/connect-server/src/converter/v2_relation_converter.rs` —
    22 rewrites (14 return + 8 tail-`Err`). Imports as
    `use thunderduck_core::bail_boundary_proto;`.
  - `crates/core/src/transpiler_v2/analyzer.rs` — 3 rewrites (all
    `AnalyzerError::UnsupportedRule` sites).
- **LOC delta.** macros.rs +98; six touched files −190 net (495 ins
  / 685 del). Net **−92 LOC** across the pass. 166 macro invocations
  landed (111 emission + 28 v2_lowering + 2 parser_v2 + 22 converter
  + 3 analyzer).
- **Verify grep.** `git grep 'return Err(EmissionError::Unsupported'
  crates/`: 133 → 0 (delta −133; plan target ≥ 40). Remaining
  `EmissionError::Unsupported*` references (`.ok_or_else(|| …)` /
  `.map_err(|e| …)` closure sites, doc comments, test-side
  `matches!` patterns) are OPP-HHH's target (Pass 8).
- **Sites left for OPP-H (Pass 9).** After Pass 9 merges the four
  `EmissionError::Unsupported*` variants into a single
  `Unsupported { kind, name, reason }`, the entire migration reduces
  to a **one-line change per macro body** — the ~160 call sites do
  not need to be touched again. That's the ordering benefit OPP-D
  before OPP-H cites.
- **Corpus.** 314 → 314 (unchanged — wire error strings are
  byte-identical).
- **Warnings.** No delta. `cargo check -p thunderduck-core -p
  thunderduck-connect-server` clean.
- **Gate.** `cargo test -p thunderduck-core --lib --tests` → 448
  pass / 0 fail / 4 ignored. `cargo test -p
  thunderduck-connect-server --tests` → 69 pass / 0 fail + 14
  ignored differential — status matches HEAD.
- **Fmt drift note.** Scoped `rustfmt --edition 2021 --check` on
  touched files: **zero** drift blocks. Pass 7's rustfmt run
  incidentally cleaned up 8 pre-existing drift blocks (2 in
  converter, 4 in v2_lowering, 1 in analyzer, 1 in emission) — those
  drifts landed in files this pass edited, so re-formatting them is
  a natural side effect.

## Pass 8 — OPP-HHH (2026-07-05)

Introduce `ProtoFieldExt` extension trait on `Option<T>` in
`crates/core/src/transpiler_v2/macros.rs` (co-located with Pass 7's
`bail_boundary_*!` macros — the file's docstring already frames
itself as τ's boundary-error surface, and this trait is the
missing-field companion). Trait exposes
`.require_proto(shape, reason)?` — the closure form of
`bail_boundary_proto!` that Pass 7 could not cover because its
`return` would leave the enclosing closure, not the function.

**Trait shape.** Generic over `T`; `.as_ref()` (`Option<&T>`) and
`.as_deref()` unify at the call site — one impl covers all 24
sites.

```rust
pub trait ProtoFieldExt<T> {
    fn require_proto(self, shape: &str, reason: &str) -> Result<T, EmissionError>;
}
impl<T> ProtoFieldExt<T> for Option<T> { … }
```

**Module visibility.** `mod macros;` → `pub mod macros;` in
`transpiler_v2/mod.rs` so the trait is importable via
`use crate::transpiler_v2::macros::ProtoFieldExt;` (core) and
`use thunderduck_core::transpiler_v2::macros::ProtoFieldExt;`
(connect-server).

- **Files touched.**
  - `crates/core/src/transpiler_v2/macros.rs` — add `ProtoFieldExt`
    trait + impl (+48 LOC including docstring with `# Example`).
  - `crates/core/src/transpiler_v2/mod.rs` — `pub mod macros;`.
  - `crates/core/src/parser_v2/v2_lowering.rs` — 3 rewrites.
  - `crates/connect-server/src/converter/v2_relation_converter.rs` —
    21 rewrites (20 inline-closure + 1 bracket-block sibling for
    `UnresolvedExtractValue::extraction`).
- **LOC delta.** macros.rs +48; v2_lowering.rs −6; converter −56.
  Net **−14 LOC** across the pass. 24 sites migrated.
- **Verify grep.** `git grep 'ok_or_else(|| EmissionError::UnsupportedProtoShape'
  crates/`: **23 → 0** (all inline-closure sites migrated).
- **Corpus.** 314 → 314 (unchanged — wire error strings
  byte-identical).
- **Warnings.** No delta. `cargo check -p thunderduck-core -p
  thunderduck-connect-server` clean, zero warnings.
- **Gate.** `cargo test -p thunderduck-core --lib --tests` → 448
  pass / 0 fail / 4 ignored. `cargo test -p thunderduck-connect-server
  --tests` → 69 pass / 0 fail + 14 ignored — matches HEAD.
- **Fmt drift note.** Scoped `rustfmt --edition 2021 --check` clean.
  All 4 touched files had 0 pre-existing drift blocks at HEAD.

## Pass 9 — OPP-H (2026-07-05)

Merge the four
`EmissionError::Unsupported{Op,Expression,Function,ProtoShape}`
variants — which shared the same `(name, reason)` shape and differed
only in the Display prefix — into a single
`EmissionError::Unsupported { kind: UnsupportedKind, name: String,
reason: String }` variant. Prefix routing moves to
`impl UnsupportedKind { fn display_prefix() -> &'static str }`, so
the `#[error(...)]` attribute inlines the kind's prefix and the four
legacy Display strings emit byte-identical output.

**Ordering benefit realized.** Pass 7's `bail_boundary_*!` macros
hide the enum name from ~160 call sites, so the migration reduces
(as OPP-H's dependency note predicted) to a one-line change per
macro body plus mechanical rewrites of the remaining struct-literal
and `matches!`-pattern sites.

- **Files touched.**
  - `crates/core/src/transpiler_v2/error.rs` — replace the 4-variant
    enum with `Unsupported { kind, name, reason }` + `UnsupportedKind`
    sibling enum. Migrate 4 Display tests + 2 `From`-composition
    tests to the new shape; assertions on Display strings unchanged.
  - `crates/core/src/transpiler_v2/macros.rs` — 4 macro bodies
    rewritten (`bail_boundary_op!`, `bail_boundary_expr!`,
    `bail_boundary_fn!`, `bail_boundary_proto!`) plus
    `ProtoFieldExt::require_proto`. `bail_boundary_rule!`
    (`AnalyzerError`) untouched.
  - `crates/core/src/transpiler_v2/analyzer.rs` —
    `analyzer_error_to_emission_error` bridge migrated (3 arms) plus
    2 unit tests updated to new pattern.
  - `crates/core/src/transpiler_v2/emission.rs` — 6 constructor-form
    closure sites + 16 test-side `matches!` pattern sites migrated;
    2 doc-comment refs rewritten.
  - `crates/core/src/parser_v2/v2_lowering.rs` — 8 constructor sites
    + 7 `matches!` arms + 1 doc-comment ref.
  - `crates/core/src/parser_v2/mod.rs` — 1 constructor + 2
    doc-comments.
  - `crates/connect-server/src/converter/v2_relation_converter.rs` —
    10 constructor closures + 6 pattern sites + 3 doc-comments.
    `UnsupportedKind` added to imports.
  - `crates/core/src/transpiler_v2/mod.rs` — 1 test pattern.
  - `crates/core/src/transpiler_v2/ast.rs` — 3 doc-comments.
  - `crates/connect-server/src/service.rs` — 2 doc-comment
    variant→kind rewrites.
- **Site counts (old variant names in `crates/`).** 93 → 0 (emission
  25→0; converter 20→0; v2_lowering 16→0; macros 14→0; error 6→0;
  analyzer 5→0; ast + parser_v2/mod 3→0 each; service 2→0;
  transpiler_v2/mod 1→0).
- **Verify grep.** `git grep 'EmissionError::UnsupportedOp\|
  EmissionError::UnsupportedExpression\|
  EmissionError::UnsupportedFunction\|
  EmissionError::UnsupportedProtoShape' crates/`: **93 → 0**.
- **Corpus.** 314 → 314 (wire error strings byte-identical).
- **Warnings.** No delta. `cargo check -p thunderduck-core -p
  thunderduck-connect-server` clean, zero warnings.
- **Gate.** `cargo test -p thunderduck-core --lib --tests` → 448
  pass / 0 fail / 4 ignored. `cargo test -p thunderduck-connect-server
  --tests` → 69 pass / 0 fail + 14 ignored — matches HEAD.
- **Fmt drift note.** Scoped `rustfmt --edition 2021 --check` on
  touched files: **zero** drift blocks.

## Pass 10 — OPP-C (2026-07-05)

**Refactor.** Introduced `crates/core/src/transpiler_v2/spark_errors.rs`
(170 LOC) housing the `SparkError` enum (`DivideByZero`,
`RemainderByZero`, `InvalidArrayIndex { idx_sql, arr_sql }`) and two
synthesis helpers: `SparkError::throw_expr()` renders the DuckDB
`error('[CLASS] <message>')` fragment; `ansi_throw_if(cond, err,
inner)` wraps it in the `CASE WHEN cond THEN throw ELSE inner END`
shape. Byte-identity with the retired `ansi_zero_guard` /
`array_index_error_expr` helpers is pinned by 4 unit tests inside
the new module.

**Call sites migrated:**
- `emission::render_binary` Div/IntDiv/Mod arm →
  `ansi_throw_if(.., SparkError::{DivideByZero, RemainderByZero}, ..)`.
- `emission::render_scalar_function_call` `pmod`/`mod` arm →
  `ansi_throw_if(.., SparkError::RemainderByZero, ..)`.
- `emission::render_element_at` array arm →
  `SparkError::InvalidArrayIndex { .. }.throw_expr()` (caller still
  wraps in the 2-branch NULL-short-circuit CASE — `ansi_throw_if`
  does not fit the two-WHEN shape, so `throw_expr()` is called
  directly).

**Legacy helpers.** `ansi_zero_guard` and `array_index_error_expr`
deleted outright (no remaining callers).

**Consts.** Pass 10 scope explicitly excludes moving
`DIVIDE_BY_ZERO_MSG`, `REMAINDER_BY_ZERO_MSG`, and
`INVALID_ARRAY_INDEX_MSG_{HEAD,MID,TAIL}` — they stay in `emission.rs`
widened from private to `pub(crate)`, referenced from
`spark_errors.rs` via `use super::emission::{...}`. Pass 11 (OPP-J)
is the pure move.

- **Files touched.**
  - `crates/core/src/transpiler_v2/spark_errors.rs` — new, +170 LOC.
  - `crates/core/src/transpiler_v2/mod.rs` — +1 line.
  - `crates/core/src/transpiler_v2/emission.rs` — net roughly flat
    (−22 retired helper LOC, +16 migrated call-site LOC, +6
    doc/visibility comments).
- **Corpus.** 314 → 314. math-010, math-011, arr-008 remain GREEN.
- **Warnings.** No delta.
- **Gate.** `cargo test -p thunderduck-core --lib --tests` → 452
  pass / 0 fail / 4 ignored (baseline 448 + 4 new
  `spark_errors::tests`). `cargo test -p thunderduck-connect-server
  --tests` → 69 pass / 0 fail + 14 ignored (matches HEAD).
- **Fmt drift note.** Scoped `rustfmt --edition 2021 --check` on
  touched files clean.

## Pass 11 — OPP-J (2026-07-05)

Move the Spark ANSI error-text constants (`DIVIDE_BY_ZERO_MSG`,
`REMAINDER_BY_ZERO_MSG`, `INVALID_ARRAY_INDEX_MSG_{HEAD,MID,TAIL}`)
from `emission.rs` (where Pass 10 left them as `pub(crate)` bridges)
into `spark_errors.rs` proper. The consts now live next to the
`SparkError` enum + `throw_expr()` / `ansi_throw_if` synthesis
helpers that consume them.

- **Files touched.**
  - `crates/core/src/transpiler_v2/spark_errors.rs` — drop the
    `use super::emission::{...}` bridge; consts now defined locally
    with docstrings.
  - `crates/core/src/transpiler_v2/emission.rs` — remove the 5 const
    declarations (+ their comment blocks); test-side `sql.contains(...)`
    assertions in `render_element_at_array_wraps_with_ansi_oob_guard`
    now import the fragments via
    `use super::super::spark_errors::{...}`.
- **LOC delta.** −7 net (const bodies move; two comment blocks
  slimmed).
- **Corpus.** 314 → 314 (pure relocation; wire strings byte-identical).
- **Warnings.** No delta.
- **Gate.** `cargo test -p thunderduck-core --lib` → 452 pass / 0
  fail. Scoped `rustfmt --edition 2021 --check` clean.

## Pass 12 — OPP-O (2026-07-05)

Extract the duplicated `dedup_names` PySpark-parity helper into a
shared, INV10-safe module `crates/core/src/types/pyspark_parity.rs`.
Two copies existed: `crates/connect-server/src/arrow_schema_stamp.rs`
(private) and `crates/core/src/transpiler_v2/emission.rs`
(`dedup_struct_field_names`). Both are rule-of-two duplication with a
documented sync invariant (τ substrate names + outbound Arrow stamp
target names must match bit-for-bit for `arrays_zip` / duplicate
STRUCT field cases). Extraction eliminates the sync tax.

- **Files touched.**
  - `crates/core/src/types/pyspark_parity.rs` — new, +91 LOC (fn +
    5-case unit test suite pinning the PySpark rule).
  - `crates/core/src/types/mod.rs` — `pub mod pyspark_parity;`.
  - `crates/core/src/transpiler_v2/emission.rs` — replace 21-LOC
    body of `dedup_struct_field_names` with a one-line delegation
    to `crate::types::pyspark_parity::dedup_names(names)`. `Docstring
    updated to name the shared helper.
  - `crates/connect-server/src/arrow_schema_stamp.rs` — delete the
    duplicate `dedup_names` fn (−26 LOC) + its local
    `use std::collections::HashMap;` (no longer needed here). Import
    via `use thunderduck_core::types::pyspark_parity::dedup_names;`.
    Callers stay identical.
- **LOC delta.** +91 new file; net −45 across the two consumer files
  (both duplicate bodies collapse to a single import line each).
  Total: +46 LOC across the pass, but the sync tax is now zero.
- **INV10.** Safe. `dedup_names(&[&str]) -> Vec<String>` is
  value-level in/out. No τ types cross the boundary.
- **Corpus.** 314 → 314 (behavior-preserving; both call sites now
  invoke the identical fn body).
- **Warnings.** No delta.
- **Gate.** `cargo test -p thunderduck-core --lib`: 453 pass / 0
  fail (baseline 452 + 1 new `pyspark_parity::tests`). Scoped
  `rustfmt --edition 2021 --check` clean.

## Pass 13 — OPP-V (2026-07-05)

Extract the 5 fattest `analyze_node` arms (SetOp, Join, NaFill,
WithColumns, ToDf) into dedicated free functions matching
`dispatch_op`'s uniform delegation shape.

New free functions in `crates/core/src/transpiler_v2/analyzer.rs`:
- `analyze_with_columns` — 56 LOC.
- `analyze_na_fill` — 56 LOC.
- `analyze_to_df` — 41 LOC.
- `analyze_join` — 136 LOC (`#[allow(clippy::too_many_arguments)]`).
- `analyze_set_op` — 217 LOC (biggest offender).

Each corresponding `analyze_node` arm becomes a 3-8 line delegating
call. Arm bodies moved verbatim; only the recursive `analyze_node
(*input, base_types)` inside the arms adjusted to `analyze_node
(input, base_types)` because the helpers accept unboxed
`CommonAst` (matches the pre-existing `analyze_unpivot` /
`analyze_describe` style).

- **`analyze_node` LOC.** 940 → 500 (Δ −440).
- **`analyzer.rs` total LOC.** 5741 → 5795 (Δ +54 — verbatim-extraction
  floor: helper signatures + separator comment exceed savings from
  the removed arm-scope braces). The plan's optimistic "~40 LOC
  final `analyze_node`" projection assumed every arm collapse into a
  single delegating line, but ~27 non-extracted arms retain their
  destructure boilerplate. `dispatch_op`-uniform shape landed for
  the 5 fat arms as intended.
- **Corpus.** 314 → 314 (behavior-preserving; arm bodies moved
  verbatim).
- **Warnings.** No delta.
- **Gate.** `cargo test -p thunderduck-core --lib --tests`: 453
  pass / 0 fail. Scoped `rustfmt --edition 2021 --check` clean.

## Pass 14 — OPP-WW (2026-07-05)

Extract a shared `passthrough_schema_arm(input, base_types, |ti|
build_op) -> Result<TypedAst, AnalyzerError>` helper in
`crates/core/src/transpiler_v2/analyzer.rs`. Signature accepts a
failable `build_op` closure so callers can `?`-propagate inside it.

**7 arms migrated** to the helper (in file order): `Limit`, `NaDrop`,
`NaReplace`, `Deduplicate`, `Sample`, `SampleBy`, `AliasedRelation`.
Each arm shrunk from ~15 LOC to ~7-9 LOC (closure overhead + `Ok(...)`
wrap eats some of the raw win, but every arm now follows one shape).

Deviations from the plan's target list (10 arms):
- `NaFill` uses schema-mutating `analyze_na_fill` (Pass 13); does not
  fit passthrough — excluded.
- `Filter` and `Sort` are deferred to Pass 15 (OPP-II bounded resolver)
  since they need per-op `T` resolution beyond bare passthrough.

- **LOC delta.** analyzer.rs net roughly flat (+22 helper vs −20
  across 7 migrated arms). The point is the shared shape, not raw
  LOC reduction.
- **Corpus.** 314 → 314 (behavior-preserving).
- **Warnings.** No delta.
- **Gate.** `cargo test -p thunderduck-core --lib`: 453 pass / 0
  fail. Scoped fmt clean.

## Pass 15 — OPP-II (2026-07-05)

Introduce the bounded schema-passthrough resolver
`analyze_input_with_schema_passthrough<T>` in
`crates/core/src/transpiler_v2/analyzer.rs`. Signature:

```rust
fn analyze_input_with_schema_passthrough<T>(
    input: CommonAst,
    base_types: &BaseTypes,
    resolve_t: impl FnOnce(&StructType) -> Result<T, AnalyzerError>,
    build_op: impl FnOnce(TypedAst, T) -> TypedOp,
) -> Result<TypedAst, AnalyzerError>
```

`resolve_t` runs against the analyzed input schema; `build_op`
receives the typed input and the resolved `T`, and yields the
concrete `TypedOp` variant. Complements Pass 14's
`passthrough_schema_arm` (which requires no `T`) by covering the
"resolve, then passthrough" pattern.

**2 arms migrated:**
- `Filter` — `T = Expression` (condition, includes the Boolean
  type-check inline in `resolve_t`; wire error preserved).
- `Sort` — `T = Vec<SortOrder>`.

Interaction with Pass 14's helper: SEPARATE, not layered. Pass 15's
arms never touch `passthrough_schema_arm`; they jump straight to the
layered helper. Both helpers coexist.

Deviation from plan's 3-arm target: `Limit` stayed on Pass 14's
`passthrough_schema_arm` because it has no `T` to resolve — forcing
it through Pass 15 with `T = ()` + a `|ti, ()|` build closure adds
1 LOC and 2 lines of noise for zero benefit. The plan constraint
"reconsider the helper shape if signature is longer than the arm it
replaced" favors Pass 14 for `Limit`.

- **LOC delta.** analyzer.rs +24 helper + ~−6 across Filter/Sort
  migration. Structural DRYness gained; raw LOC near-flat.
- **Corpus.** 314 → 314 (behavior-preserving).
- **Warnings.** No delta.
- **Gate.** `cargo test -p thunderduck-core --lib`: 453 pass / 0
  fail. Scoped `rustfmt --edition 2021 --check` clean.
