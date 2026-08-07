# τ Architecture Review — Work Checklist

Working branch: `refactor/tau-architecture-review` (worktree `thunderduck-rs-wt3`, rebased onto
`main` @ `cc94673`).

Findings and full rationale: [`tau-architecture-review-2026-08.md`](tau-architecture-review-2026-08.md).
**Read the target entry there before starting an item** — each carries file:line evidence, the
mechanical proposal, and the specific corpus clusters to re-run. This file is only for tracking.

## Legend

- `CONFIRMED` — evidence and proposal both verified by an adversarial pass.
- `PLAUSIBLE ⚠️` — survived verification but with a **substantially narrowed or partly wrong
  proposal**. Do NOT execute as originally written; the report's closing section lists exactly
  which sub-claim is wrong.

## Gate for every item (CLAUDE.md)

`cargo fmt --check` → `cargo clippy -- -D warnings` → `cargo test` → corpus differential
(no previously-green case may regress). An item is not done if any step is red.

---

## Lane A — error model

Own commits; no collisions with τ internals.

- [ ] **A1 · (a)1 `AnalyzerError::Other` exits as Thunderduck-boundary** — CONFIRMED — *the only
      finding with product consequence.* 66 sites. **Audit the 66 first** for τ-internal ones
      (`analyzer.rs:2542` belongs in `EmissionError::Internal`). Fold in (c)4's two mechanical
      pieces: hoist `SPARK_EMULATED_PREFIX`/`TDCK_BOUNDARY_PREFIX` consts, and add
      `AnalyzerError::category()` as one exhaustive match, using it — not
      `spark_class().is_some()` — as the bridge's branch condition. Convert the three prose-class
      sites (`:1992`, `:1997`, `:2278`) to real tokens. Flips gRPC UNIMPLEMENTED →
      INVALID_ARGUMENT for these errors. −25/+18
- [x] **A2 · (a)4 delete `ThunderduckError::TranspilerV2Emission`** — CONFIRMED — no production
      constructor; 7 grep hits are decl + a same-named variant on a different type + tests. −45
- [x] **A3 · (a)9 `map_ddl_error` `TABLE_OR_VIEW_NOT_FOUND` triplication** — CONFIRMED —
      `service.rs:1151/1165/1177`. **Do not touch** the `*_ALREADY_EXISTS` pair (one occurrence
      each, materially different verbatim Spark texts). Message text must stay byte-identical.
      −30/+12
- [ ] **A4 · (c)3 `ConnectError::SparkRuntime` `class` field** — PLAUSIBLE ⚠️ — **keep the
      variant** (deleting it is a live regression hazard for six start-anchored `expected_error`
      cases). Restrict to the phantom `class` field: drop it, or use it in a `tracing::debug!`.
      Do not touch the `ConnectError` → `Status` mapping.

## Lane B — emission

All touch `emission.rs`. **Serialise these**; order matters.

- [x] **B1 · (a)5 delete the two `#[allow(dead_code)]` anchor renderers** — CONFIRMED —
      `render_tail` (no `Tail` variant exists anywhere) and `spark_aggregate_return_cast` (rule
      lives wired at `type_inference.rs:320-360`). Re-point the two ADR-checklist anchors at live
      code rather than dropping them. Confirm with owner first: already flagged in
      `tasks/archive/v2-architecture-review-2026-07-03.md:56` and not acted on. Do this first —
      it removes one of the 11 `dispatch_op` sites before B2 enumerates them. −60
- [ ] **B2 · (a)13 `child_sql` helper; legacy renderers stop recursing through `dispatch_op`** —
      CONFIRMED — 11 non-test sites. Keep `dispatch_op` public (4 external callers). Add a
      grep-barrier + strengthen both tap tests to a **non-leaf legacy plan** (`Sample` over
      `TableScan`) — today they pass only because both use `CommonOp::SingleRow`, a leaf. −11/+8
- [x] **B3 · (a)12 move exhaustiveness into `build_unit`** — CONFIRMED — `:199` catch-all forces
      `legacy_render` to restate 21 variant names to reach `unreachable!`. −11
- [ ] **B4 · (a)15 `#[cfg(test)]`-gate `EMIT_TAP`** — CONFIRMED — safe half; removes the global +
      the atomic RMW from release at zero risk. **Decide B4-vs-(b)3 before B2**, since it changes
      what B2 strengthens. ≈0
  - [ ] *Alternative* **(b)3 delete `EMIT_TAP` entirely** — CONFIRMED, structural bet — also
        deletes 135 `let _g = tap_guard();` lines and de-serialises the emission test module.
        ≈−230. Costs INV2's routing check. **Team decision required.**
- [ ] **B5 · (a)11 set-op `field_by_name` + `.expect` in library code** — CONFIRMED —
      `emission.rs:2078-2090`. Prefer the `CAST(NULL …)` pad over panicking the session thread.
      −10/+1
- [ ] **B6 · (b)1 three Raw → `SelectBlock` conversions** — CONFIRMED, structural bet — last in
      the lane; **the only item here that changes emitted SQL**, so it wants a clean corpus
      baseline. `build_with_columns_renamed`, `render_sample`, `render_pivot`. Leave
      `render_sample_by` Raw (volatile `RANDOM()`) and comment why. The other 7 Raw renderers are
      genuinely statement-shaped — leave them. −45/+30
- [ ] **B7 · (a)21 `pub type Schema = ResolvedSchema` rename** — CONFIRMED — **absolutely last,
      alone, after the lane is quiet.** ~90 in-place renames in the file every other item edits.
      −6/+2

## Lane C — analyzer / identity / frontends

- [ ] **C1 · (a)2 `analyze_to_df` omits `source_quals.clear()`** — CONFIRMED — *real parity fix.*
      One line at `analyzer.rs:1958-1960` + the two ToDf mirror tests. Leniency-only regression
      class. Do this first and independently. −11/+8
- [x] **C2 · (a)7 `ColumnReference::from_attr` / `From<Attribute>`** — CONFIRMED (found by 2
      lenses) — **the single highest-leverage item in the review.** Put both doors in
      `expression.rs`, NOT `schema.rs` (would create a `schema → expression → schema` cycle).
      Rewrite only the 8 hand-rolled copy-outs. **Do not** extend the literal ban to
      `ColumnReference` — `:4039/4381/4482/4578/4703` must stay literals (type/nullability come
      from the nested field, not the column). −40/+15
- [ ] **C3 · (b)2 `ColumnReference::expr_id: Option<ExprId>` → `ExprId`** — CONFIRMED, structural
      bet — requires C2's constructor first, plus its own review pass. Needs restructuring
      `resolve_column`'s tier ladder (three arms use `(Unresolved, false, None)` as a
      fall-through sentinel) into an explicit `enum Resolution` — an `.expect()` is not an option
      under the no-unwrap rule. Hinge is `emission.rs:1170`. −15/+8
- [x] **C4 · (a)6 extract `merge_join_scopes`** — CONFIRMED — `for_join_condition` re-implements
      `RelScope::of`'s Join arm; only divergence is `keep_right`. Independent of C1–C3, can run in
      parallel. **Keep the `using_columns` gate at the call site.** Use `RightSide::{Keep, Drop}`,
      not a bare bool. Push order is content- and order-sensitive — copy exactly. −45/+5
- [x] **C5 · (a)10 delete `CommonOp::TableScan.alias`** — CONFIRMED — unreachable in production;
      forces three live branches. **Not a pure re-pin:** `analyzer.rs:12344` and `:12360`
      deliberately pin τ's *non-Spark* alias-shadowing deviation and must be deleted or rewritten
      to the Spark-correct assertion — call this out in the commit message. −45/+15
- [ ] **C6 · (a)14 decimal `MAX_PRECISION` clamp duplicated across both frontends** — CONFIRMED —
      add `MAX_DECIMAL_PRECISION` + `clamp_decimal_shape` to `expression.rs`. **Do not** add the
      `s > p` bump to the DDL sites — that would be a behaviour change. −18/+10
- [x] **C7 · (a)18 inline the duplicated `validate_values_resolve`** — PLAUSIBLE ⚠️ — 4 of the 5
      claimed duplications **do not exist**. Only `analyzer.rs:4977-4984` survives. **Do not** add
      `names_except`/`validate_names` to `ResolvedSchema`. Preserve error *ordering*. −15
- [x] **C8 · (a)20 un-gate `UnresolvedColumn::bare`** — PLAUSIBLE ⚠️ — the proposal's second half
      is **wrong**: routing `v2_lowering.rs:2745-2750` (`Expr::CompoundIdentifier`) through `bare`
      would silently drop the qualifier and regress dotted-reference resolution (cx-004). Take
      only the un-gate + `unresolved_col` deletion. −20/+5
- [ ] **C9 · (b)4a `generator_alias_pair`** — PLAUSIBLE ⚠️ — a **third** builder site was missed
      (`v2_lowering.rs:1354-1375`); leave it alone. Share only between
      `v2_relation_converter.rs` and `multi_alias.rs`. −25/+15
  - [ ] *Separately, NOT a refactor* — (b)4b add `posexplode`/`posexplode_outer` to
        `dispatch_multi_alias`. This is a **scoped feature** with corpus verification; the current
        gap is a documented, test-pinned `[TDCK-BOUNDARY]` per ADR-022.

## Lane D — hygiene

Any time, zero collisions.

- [x] **D1 · (a)3 delete `StreamingConfig`** — CONFIRMED — entirely inert; its one field is never
      read yet threaded through 32 sites, and its "clamped to [1024, 65536]" doc is false. −55
- [x] **D2 · (a)16 `relation_converter.rs` → `json_schema.rs`; dead `parse_spark_type_strict`;
      two visibilities** — CONFIRMED (2 lenses) — a bare deletion of `parse_spark_type_strict`
      **will not compile**: rewrite the test helper `fn strict` at `spark_ddl.rs:301` to call
      `parse_type(s, false)` so the grammar battery keeps running. −30/+5
- [ ] **D3 · (a)17 `rewrites.rs` — fill or delete, pick one** — CONFIRMED (2 lenses, opposing
      proposals) — **decision required.** (A) move `crosstab_to_aggregate` (`analyzer.rs:5433`,
      a pure CommonAst→CommonOp rewrite and analyzer.rs's only `pub` item of its kind) into it and
      reword the module doc; or (B) delete the stub and fold the intent into `mod.rs`'s doc.
      Four stale doc refs to fix either way.
- [x] **D4 · (a)19 dead halves of the `StructType`/`ResolvedSchema` API** — CONFIRMED —
      `StructType::merge` has no production caller. **Do not merge the two types** — `StructType`
      is the deliberately id-free boundary type. −30/+8
- [x] **D5 · (a)8 narrowed `invariants.rs` trim** — PLAUSIBLE ⚠️ — the original −260 proposal is
      **substantially refuted and must not be executed**: ADR CV.5.1 defines the four
      `#[ignore] todo!()` stubs as *expected* DEFER ownership markers, and INV8/INV9 govern the
      Delta-write slice. Delete only `inv10_service_rs_is_in_walk_scope` and
      `inv10_filtered_root_only_walks_named_files`; **keep** the two walker-vacuity guards.
      −70, not −260
- [ ] **D6 · (c)2 doc fix — record the production/test line split** in
      `docs/context/architecture.md`'s file inventory (analyzer.rs 6062/10161, emission.rs
      7176/10262; ~32k of ~67k lines across the 15 biggest files are inline tests) so raw counts
      stop distorting structural judgement. **Plus one real bug this lens found:**
      `passthrough_schema_arm`'s doc (`analyzer.rs:1018-1019`) lists `Sort` as a user, but the
      Sort arm calls `analyze_node` directly because it may *extend* the schema. One line.
- [ ] **D7 · (c)1 record the corrected `TypedOp` census** on `analyzer.rs:376-381` — **23
      identical / 8 narrowed / 2 pre-analysis-only** (the finding's own 22/9/2 was wrong;
      `Aggregate` is field-for-field identical). Move the cached-scope rationale from the
      `analyze_sort` `debug_assert` comment to the field itself. Closes the question so it is not
      re-litigated. +12 doc lines
- [ ] **D8 · (c)5 free half of the `TypeInferenceEngine` cleanup** — PLAUSIBLE ⚠️ — the tuple →
      named-struct change is **not** worth it (`resolve_in` has 5 callers, not 2; `column_info` is
      already private). Take only: delete `column_info_in`, demote the four `pub` fns to
      `pub(super)`, drop the `mod.rs:51` re-export.

## Explicitly closed — do not re-litigate

Rationale in section (c) of the report.

- **`CommonOp`/`TypedOp` duality** — keep. Rust cannot express variant *presence* in one
  `Op<Phase>`; a generic parameterisation forces `unreachable!()` into all 70 `TypedOp::` match
  sites in emission. Cached `TypedAst::scope` also earns its keep (Join clones both children's
  vectors → on-demand derivation is O(subtree) per lookup, 48 read sites).
- **Splitting `analyzer.rs` / `emission.rs`** — no. Two lenses independently concluded the
  colocated tests' `super::*` access to private helpers is load-bearing. The line counts are a
  documentation problem (D6), not a structural one.
- **`[SPARK-EMULATED]` / `[TDCK-BOUNDARY]` Display prefixes** — keep. Documented as a deliberate
  grep-classification affordance; removing them from `AnalyzerError` leaves the same multiplicity
  one layer down (`emission.rs:4416/4429/4439` hand-write `[TDCK-BOUNDARY]`). Only the
  const-hoisting + `category()` kernel is worth taking — folded into A1.
- **The `name_fold` re-export** (`mod.rs:27-33`) — drop the finding; its rationale is
  self-refuting (it claims the comment misleads about a ban the comment explicitly disclaims).
  Fold in opportunistically only if editing those five imports anyway.
- **7 of the 11 `Raw` renderers** — genuinely statement-shaped (`WITH RECURSIVE`, `WITH …
  MATERIALIZED` + UNION ALL, DuckDB `UNPIVOT … ON … INTO`, bare UNION chains). Not migration debt.

---

## Totals

- **Clear wins: ≈450 net production lines removed**, excluding the ~230 from the `EMIT_TAP`
  structural bet and excluding test-helper churn.
- **Findings with product consequence: 2** — A1 (error category on the wire) and C1 (τ accepts a
  qualified reference Spark rejects). Both independently spot-checked against the code.

## Landed 2026-08-04

13 items landed (ticked above), selected as those whose budgeted LOC gives a net reduction ≥ 10
lines *and* a deleted:new ratio ≥ 2:1 — **A2, A3, B1, B3, C2, C4, C5, C7, C8, D1, D2, D4, D5**.
Measured result: **net −330 lines**; deleted:added 2.6:1 counting only changed lines.

Gate at original landing (pre-rebase): `cargo fmt --check` clean; `cargo clippy` clean; `cargo test` clean;
DataFrame corpus 413/413 and SQL corpus 416/416 green. (After the rebase, corpora still need re-run.)

**Not taken, and why** — every remaining item misses one of the two LOC thresholds on the
review's own figures: A1 (−25/+18), A4, B2 (−11/+8), B4 (≈0), B5 (−10/+1, net −9), B6
(−45/+30 = 1.5:1), B7 (−6/+2), C1 (−11/+8), C3 (−15/+8), C6 (−18/+10 = 1.8:1), C9
(−25/+15 = 1.7:1), D3 (net ~0), D6/D7 (net positive — docs), D8.
**A1 and C1 are the review's two findings with product consequence and are still open.**

One deviation to note: C5 deleted the two assertions that pinned τ's non-Spark alias-shadowing
divergence. With `TableScan.alias` gone, `emp AS e1 JOIN emp AS e2` referenced by the bare table
name `emp` now yields `UnknownColumn`, matching Spark's `UNRESOLVED_COLUMN.WITH_SUGGESTION`, so
the test was rewritten to the Spark-correct assertion rather than deleted
(`self_join_referenced_by_shadowed_table_name_is_unresolved`). Eight further scope/lineage
assertions were updated the same way: an alias no longer adds a second binding alongside the base
table name.
