# Plan 007 — F5: inline under USING parents; RelScope-qualified hoisted slots

Fixes finding 5 of `tasks/select-block-review-findings.md` (witness
`join-021`): a user alias buried under `AS __td_jl` by the USING parent's
inline refusal, while the empty USING `RelScope` makes the qualifier
vis-exempt — the merge emits an unbindable `e.name`. Emission-side only; do
NOT touch analyzer.rs.

## Why this fix (architect decision, recorded)

- Merge-path qualifier stripping is REJECTED: scope-unbound qualifiers on
  the merge path include correlated outer references (the sq-* green
  cluster rides exactly that exemption); no emission-side fact separates
  them from buried locals. Do not attempt it.
- The pre-branch renderer INLINED the nested join under USING parents and
  was green (review F5 evidence, old `emit_flat_chain`). The refusal exists
  only because the hoisted-slot list used to require single-alias sides.
  Cycle 1's `DefaultSlot` machinery removes that constraint: qualify each
  side field by the alias covering its index in the side's stamped
  `RelScope` (`side.scope.aliases`, ranges relative to the side's schema).

## Changes (crates/core/src/transpiler_v2/emission.rs only)

### 1. Per-field slot qualifiers from the RelScope stamp

New helper near `build_join`:
```rust
/// Per-field qualifier list for one join side: which alias qualifies each
/// column of `side.resolved_schema` in the enclosing FROM scope. Single-
/// alias items qualify every field with that alias; a multi-alias (inlined
/// chain) side derives the alias from the side's stamped RelScope — the
/// FIRST alias whose range covers the field index (TableScan binds table
/// and alias to the SAME range; first-match is canonical). Returns None if
/// any field index is covered by no alias (un-hoistable — caller must keep
/// that side wrapped under its synthetic alias instead).
fn side_slot_quals(side: &TypedAst, item: &FromItem) -> Option<Vec<String>>
```
- `item.exposed().as_slice() == [only]` → `Some(vec![only; n_fields])`.
- Else: for each `i in 0..side.resolved_schema.len()`, find the first
  `(name, range)` in `side.scope.aliases` with `range.contains(&i)`; any
  miss → `None`.

### 2. build_join_side — allow nested-Join inline under USING parents when hoistable

In the `FromItem::Join { .. }` arm of `inline_ok`, replace
`&& !parent_has_using` with
`&& (!parent_has_using || side_scope_covers(side))` where
`side_scope_covers` is the boolean form of change 1's coverage check
(extract a shared helper so the two stay one predicate — e.g.
`side_slot_quals` on a provisional item, or a `scope_covers_fields(side)`
fn both call). Everything else in the guard (left-side only, plain ON
child, non-semi/anti, non-lateral, USING-child refusal, Raw never) is
UNCHANGED. Non-USING parents: behavior byte-identical to today.

### 3. build_join — default_slots via per-field qualifiers

Rework the `default_slots` block (~369-430): replace the
`single_alias(left)/single_alias(right)` gate with `side_slot_quals(left,
&left_item)` / `side_slot_quals(right, &right_item)`:
- Non-USING joins (`using_columns.is_empty()`): PRESERVE today's behavior —
  build defaults only when BOTH participating sides are single-alias (the
  multi-alias plain-chain `SELECT *` order already matches the schema; do
  not churn those shapes). Implement as: if using is empty and either
  needed side is multi-alias → `None`, exactly as now.
- USING joins: use the per-field qualifier vecs. Slots: USING cols first as
  bare `quote_ident(c)` (bare keys are valid over USING even with a
  multi-relation left), then left non-USING fields as `{qual_i}.{col}`,
  then (when `need_right`) right non-USING fields likewise. `is_empty` →
  `None` guard unchanged.
- Invariant to assert in a comment: under a USING parent, change 2 only
  inlines a multi-alias side when coverage holds, so `side_slot_quals` is
  `Some` for every inlined side of a USING join by construction; if it is
  ever `None` there (right side is never inlined under USING and stays
  single-alias), defaults fall back to `None` — do NOT panic.

## Invariants

- ADR-022 single path; analyzer untouched (USING RelScope stays EMPTY —
  resolution semantics unchanged; only emission's FROM shape changes).
- No unwrap/expect on new paths; /// docs on new helpers.
- F2's pin (`using_join_side_wrap_preserves_hoisted_slots`) and the
  semi/anti chain-break guard must still hold (child-op guard untouched).

## Tests (emission.rs mod tests; helpers scan/aliased_scan/qcol/
base_types_emp_dept; nums table exists in analyzer fixtures? use emp/dept/
emp2 shapes from this test module only; tap_guard; triage with --exact)

New pins:
1. `alias_ref_above_using_parent_inlines_and_binds` — join-021 shape:
   Project([qcol("e","name")]) over Join{ left: Join{aliased_scan(emp,"e"),
   aliased_scan(dept,"d"), ON e.dept_id=d.dept_id}, right: scan(emp2)?,
   using: ["dept_id"] } — pick tables so `dept_id` exists on the left and
   right (emp2 has dept_id in this module's fixtures? if not, build a
   suitable second table via the existing helpers; the SHAPE is what
   matters). Assert: SQL contains `AS e` in the top-level FROM (alias not
   buried), contains `e.name` in the SELECT, does NOT contain `__td_jl`.
2. `using_parent_hoisted_slots_qualify_by_covering_alias` — same input,
   projection-free dispatch of the Join: default list starts with the bare
   USING key, then `e.`-qualified fields, then `d.`-qualified fields, in
   schema order.
3. `using_parent_with_uncoverable_side_still_wraps` — left side = plain ON
   join of two Project-over-scan children (each child re-scopes → the
   nested join's RelScope aliases are empty → coverage fails): assert the
   left side still renders wrapped (`AS __td_jl` present) and the query is
   otherwise well-formed. Defaults inside the wrap remain intact.
4. Existing pins must pass UNCHANGED (list in the change log if any needed
   re-baselining and why — expected: none; the only new shapes are
   USING-parent inlines that previously wrapped).

## Verification (before returning)

`cargo check -p thunderduck-core`; `cargo test -p thunderduck-core --lib`
all green; scoped `rustfmt --check --edition 2021` on changed files; no new
clippy warnings on touched lines. No corpora, no commits, no release build.

## Acceptance (orchestrator)

witness-progress.sh: REGRESSIONS 0; flips now 7/8 including join-021 (F5).
join-022 (F7) stays red. Residual (record in report, do not fix here): a
USING parent whose left side has an UNCOVERABLE scope still wraps and a
qualified ref above it still strands loudly — narrowed, un-witnessed.
