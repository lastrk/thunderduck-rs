# Slice A Scope — τ substrate

**Slice A** is foundational: it establishes τ's substrate (types, plan, protobuf converter, SparkSQL front-end, dispatch relocation) so subsequent slices have something to build on. It unlocks zero DataFrame-corpus cases directly.

## §Targets

**No case-ID unlocks.** Slice A is foundational substrate. The load-bearing gates for termination are the §Success criteria below, not a pytest `-k` filter.

**Pre-slice `core_v2` state:** 153/324 (measured at commit `e604193`). This number reflects the current post-morph-track-deletion state where `THUNDERDUCK_TRANSPILER=v2` is a no-op and all requests fall through to the legacy path, which handles 153 corpus cases. This fallthrough is what ADR-022 removes — τ is the only path once Slice A.3 relocates dispatch.

Cumulative per-sub-slice targets (per Iteration Methodology §CV.7 / Open Decision 10):
- **A.1 termination:** 153/324, no change (A.1 adds types substrate without touching dispatch).
- **A.2 termination:** 153/324, no change (A.2 adds plan substrate + protobuf converter + SparkSQL front-end without touching dispatch).
- **A.3 termination:** ≤12/324 (empirically determined). A.3 relocates dispatch so every request routes to τ; τ's `generate()` returns `EmissionError::UnsupportedOp` for every op (Thunderduck-boundary error per ADR-022); previous legacy-served cases surface as boundary errors. The drop from 153 → ≤12 is the *designed* shape of Slice A, not a regression — it is τ becoming the only path per ADR-022. Subsequent slices (B/C/D/E/F/G) then climb from this floor toward 324 by growing τ's coverage.

## §ADRs

- **ADR-003** (common AST) — τ's CommonAst as the shared IR fed by both front-ends.
- **ADR-004** (protobuf-boundary dispatch) — SQL and DataFrame both lower to CommonAst; relation-vs-command decided by parse-tree root.
- **ADR-021** (τ owns its substrate) — protobuf converter, `Expression`, `TypeInferenceEngine` are τ's own; only value-level types (`DataType`/`StructType`/`StructField`) are shared.
- **ADR-022** (τ is the only path) — no fallback machinery; no `THUNDERDUCK_TRANSPILER` dispatch flag; unsupported inputs surface Thunderduck-boundary errors directly to the caller.

Constraining ADRs (not directly owned by Slice A but must be honored):
- **ADR-020** — extension mandatory; strict-only target (already landed).
- **ADR-014** — INV3 + INV10 bracket τ's substrate boundary.

## §Inheritance-checklist sections

Pass 1's architect plan MUST cite these; reviewer verifies presence:

- **§1.1** — `count_if` aggregate: include in `aggregate_return_type` count family (`Long`), in `FunctionCall::nullable` non-nullable list, in `aggregate_is_non_nullable` list.
- **§1.2** — `hash` / `murmur3` / `xxhash64` in `FunctionCall::nullable` non-nullable literal list.
- **§1.3** — `corr` / `covar_samp` / `covar_pop` / `regr_slope` / `regr_r2` / `regr_intercept` / `regr_avgx` / `regr_avgy` / `regr_sxx` / `regr_sxy` / `regr_syy` in `aggregate_return_type` → `Double` arm AND in `aggregate_is_always_nullable`.
- **§2.1** — Exhaustive typed Arrow-value dispatch in `V2RelationConverter`. No catch-all `Ok("NULL")`; every unhandled Arrow type is a loud `Err`. Structured `Decimal128(p, s)` handling — no synthesized SQL text.
- **§2.2** — No `Sql` opaque variant in CommonAst. Six structural CommonAst variants replacing the shortcut categories: `Values`, `LocalRelation` (Arrow-IPC parsed rows), `TableFunction`/`Unnest`, `FileScan`. Catalog metadata operations routed as service-layer library calls, not plan nodes.
- **§2.3** — Plan-ID as first-class field on `Join` (`left_plan_ids: Vec<i64>`, `right_plan_ids: Vec<i64>`) and on `UnresolvedColumn` (`plan_id: Option<i64>`). No stringly-encoded qualifiers.
- **§5.5** — `plan_has_empty_scan` short-circuit in `service.rs::build_base_types_from_plan` — return empty overlay without walking the session catalog when no `TableScan` has an empty schema.

Deferred to later slices (do NOT scope into Slice A):
- **§3.1** (sha/sha1/sha2 arg-stripping) → Slice C.2.
- **§3.2** (percentile_approx FLOAT CAST) → Slice C.3.
- **§4** (extension arms + native parity + verify-native-first) → Slice D.
- **§5.1** (spark_return_cast vs spark_aggregate_return_cast separation) → Slice C.1.
- **§5.3** (EMIT_TAP + EMIT_TAP_MUTEX) → Slice C.1.
- **§5.4** (render_tail CTE rewrite) → Slice C.1.
- **§5.6** (quote_ident no-quote fast path) → Slice C.1.
- **§5.7** (spark_aggregate_rewrite for DECIMAL widening) → Slice C.3.

## §Sub-slice sketch

Slice A is naturally sequential — each sub-slice's deliverables depend on the prior's substrate. Pass 1's architect MAY re-scope but the dependency chain A.1 → A.2 → A.3 is inherent.

### A.1 — Types substrate

Deliverables:
- `crates/core/src/transpiler_v2/expression.rs` — τ's `Expression` enum, 21 variants covering Spark's expression surface at the reference version. Each variant carries `data_type(&Schema) -> DataType` and `nullable(&Schema) -> bool` methods per Spark parity.
- `crates/core/src/transpiler_v2/type_inference.rs` — τ's `TypeInferenceEngine`: aggregate return-type table, coercion lattice, nullability derivations, decimal formulas. **MUST include inheritance-checklist §1.1, §1.2, §1.3 on day 1.**
- `crates/core/src/transpiler_v2/mod.rs` — module scaffold + `pub fn generate(...)` stub returning `EmissionError::UnsupportedOp` for every input (a Thunderduck-boundary error per ADR-022 — surfaces directly, no fallback).
- `crates/core/src/transpiler_v2/invariants.rs` — INV1–INV10 stubs per §CV.5.1 marker convention (INV7 is not stubbed — it does not exist).
- Unit tests in `type_inference.rs::tests` covering every checklist §1.1/§1.2/§1.3 name — mandatory red-line proving inheritance discipline.

### A.2 — Plan substrate + protobuf converter + SparkSQL front-end

Depends on A.1. Deliverables:
- `crates/core/src/transpiler_v2/ast.rs` — τ's `CommonAst`/`CommonOp` enum carrying τ's `Expression` payload. Structured variants for `Values`, `LocalRelation` (Arrow-IPC parsed rows), `TableFunction`/`Unnest`, `FileScan`, `Join { left_plan_ids: Vec<i64>, right_plan_ids: Vec<i64>, ... }`, `UnresolvedColumn { plan_id: Option<i64>, ... }`.
- `crates/connect-server/src/converter/v2_relation_converter.rs` — `V2RelationConverter` producing τ's CommonAst directly from Spark Connect protobuf. Exhaustive typed dispatch for Arrow values (checklist §2.1). No `Sql` opaque variant emitted (checklist §2.2). Plan-ID as first-class field (checklist §2.3). Scope of proto shapes covered per Open Decision 2 hybrid: structured shapes (Project/Filter/Sort/Limit/primitive Aggregate) covered in this pass; complex shapes (Join, complex types, table functions) grow with their owning slice. Un-handled shapes produce Thunderduck-boundary errors — never silent shortcuts, never `Punt` to any other path.
- `crates/core/src/parser_v2/` — τ's SparkSQL front-end module tree (Open Decision 1 Option 1b). Contains `dialect.rs` (sqlparser-rs SparkDialect), `mod.rs` (parser entry), `v2_lowering.rs` (parse tree → CommonAst). SparkSQL front-end sets `UnresolvedColumn::plan_id = None` (Open Decision 12).
- `crates/core/src/transpiler_v2/base_types.rs` — τ's `BaseTypes` overlay (Open Decision 8: per-path, seeded independently from the DuckDB catalog by `V2RelationConverter` at request time). Applies `plan_has_empty_scan` short-circuit (checklist §5.5).

### A.3 — Dispatch relocation

Depends on A.2. Deliverables:
- `crates/connect-server/src/service.rs` — dispatch site routes ALL Spark Connect requests to τ. No `THUNDERDUCK_TRANSPILER` env var. No fallback machinery, no `V2FallbackEligible` trait, no attribution instrumentation (ADR-022).
- Any non-τ source in the workspace stays as reference material only — not compiled as a service backend, not exercised by tests. Slice K owns deletion of non-τ source when the reviewer confirms nothing references it.

## §Non-goals (slice-specific)

- **No analyzer implementation.** τ's `analyze()` and the three coordinated passes (resolve, assign_types, derive_nullability), set-op widening, outer-join nullability — Slice B.
- **No emission arms.** τ's `dispatch_op`, `render_project`/`filter`/`sort`/etc., scalar function arms, aggregate emission — Slice C.
- **No extension dispatch.** `extension_targets()` allow-list, ext6 arms, native-parity routing, INV6 activation — Slice D.
- **No join / set-op emission.** Join emitter + set-op CAST wrapper — Slice E.
- **No complex-type emission** (arrays/maps/structs/HOFs) — Slice F.
- **No vertical extensions** (temporal, grouping, windows, JSON, parsing) — Slice G.
- **No writes** (Command arm, catalog overlay, external/lakehouse) — Slice H.
- **No differential-harness activation** — Slice I.
- **No escape-hatch enumeration** — Slice J.
- **No non-τ source deletion.** Slice K is the source-cleanup slice; A does not delete anything.

## §Success criteria beyond §Targets

Reviewer verifies at termination:

1. **INV10 grep passes:** `git grep -E 'use crate::(logical|expression|generator|functions)::|use crate::types::TypeInferenceEngine' crates/core/src/transpiler_v2/ crates/connect-server/src/converter/v2_relation_converter.rs crates/core/src/parser_v2/` returns zero.
2. **Every inheritance-checklist item scoped above is present in the diff.** Unit tests in `type_inference.rs::tests` cover every checklist §1.1/§1.2/§1.3 name.
3. **Stub `generate` returns `EmissionError::UnsupportedOp` for every input** at A.1 termination; subsequent sub-slices do not weaken this (Slice A does not implement emission).
4. **No `THUNDERDUCK_TRANSPILER` env var** anywhere in `crates/connect-server/`. Grep confirms.
5. **No `V2FallbackEligible` trait, no fallback machinery** — grep confirms.
6. **`crates/core/src/parser_v2/` module tree exists** at A.2 termination.
7. **τ's per-path `BaseTypes` overlay in `crates/core/src/transpiler_v2/base_types.rs`** at A.2 termination.
8. **Quality Gate green** at each sub-slice landing (per CLAUDE.md §Quality Gate).
9. **`tests/scripts/v2-progress.sh` reports 12/324** at every sub-slice termination — no regression, no unexpected movement.
