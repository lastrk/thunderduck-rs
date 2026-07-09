# Implementation log — Plan 007 (F5): inline under USING parents; RelScope-qualified hoisted slots

Scope: `crates/core/src/transpiler_v2/emission.rs` only. `analyzer.rs` untouched
(verified `git diff HEAD -- crates/core/src/transpiler_v2/analyzer.rs` empty
throughout). No `sql_block.rs`, `parser_v2`, connect-server, corpus, or script
changes.

## Changes (all in `crates/core/src/transpiler_v2/emission.rs`)

### Plan item 1 — per-field slot qualifiers from the RelScope stamp

Added two new private functions, placed immediately before `build_join_side`:

- `fn scope_covers_fields(side: &TypedAst) -> bool` — shared coverage
  predicate: every field index of `side.resolved_schema` is covered by some
  `(alias, range)` in `side.scope.aliases`.
- `fn side_slot_quals(side: &TypedAst, item: &FromItem) -> Option<Vec<String>>`
  — per-field qualifier list. Single-alias `item` (via `item.exposed()`)
  short-circuits to `Some(vec![only; n])`; otherwise defers to
  `scope_covers_fields` and, on success, looks up the first covering alias
  per field index (mirrors `RelScope::lookup`'s first-match convention);
  returns `None` on any gap.

Both are doc-commented (`///`), no `unwrap`/`expect`, and `side_slot_quals`'s
multi-alias branch never panics — it returns `None` early via
`scope_covers_fields` before doing the per-index lookup.

### Plan item 2 — `build_join_side`'s `inline_ok`: USING-parent nested-Join gate

In the `FromItem::Join { .. }` arm of `inline_ok`, replaced
`&& !parent_has_using` with `&& (!parent_has_using || scope_covers_fields(side))`.
Every other guard (left-side-only call site via `may_inline_nested_join`,
plain ON/CROSS child, non-semi/anti, non-lateral, `Raw`-never) is unchanged.
Updated the doc comment on `build_join_side` to describe the new USING-parent
coverage exception.

### Plan item 3 — `build_join`'s `default_slots`: per-field qualifiers under USING

Replaced the single `single_alias`-gated block with an `if
using_columns.is_empty() { .. } else { .. }.filter(|slots| !slots.is_empty())`
split:

- **Non-USING branch**: byte-identical logic to before F5 (same
  `single_alias` closure, same `need_right` gating, same field-loop shape) —
  moved as-is into the `if` arm. The `if slots.is_empty() { None } else {
  Some(slots) }` guard that used to live inline is now the shared
  `.filter(...)` applied uniformly to both branches (behavior-preserving:
  the non-USING branch never actually hits empty since `left.resolved_schema`
  is non-empty in every constructible shape, matching prior behavior).
- **USING branch (new)**: computes `side_slot_quals(left, &left_item)` and,
  when `need_right`, `side_slot_quals(right, &right_item)`; on `Some` for the
  needed sides, builds the slot list as USING-cols-bare-first, then left
  non-USING fields qualified per-field by `lq[i]`, then (when needed) right
  non-USING fields qualified per-field by `rq[i]`. On any `None`, falls back
  to `None` (bare `*`) — never panics. Comment documents the "always `Some`
  by construction" invariant from the plan (change 2 only inlines a
  coverable multi-alias left side under USING; the right side never inlines
  under USING and stays single-alias) and explicitly states the `None`
  fallback is defensive, not a proof this function can make locally.

## Tests added (4 new; 3 new pins + 1 residual left for corpus/witness tracking, per plan)

All added to `crates/core/src/transpiler_v2/emission.rs`'s `mod tests`,
directly before `render_project_over_join_hoists_user_aliases` (adjacent to
the existing Plan-006 F1–F4 pins), guarded with `tap_guard()`:

1. `alias_ref_above_using_parent_inlines_and_binds` — join-021 shape:
   `Project([e.name])` over `Join{ left: Join{aliased_scan(emp,"e"),
   aliased_scan(dept,"d"), ON e.dept_id=d.dept_id}, right: scan(emp2),
   using: ["dept_id"] }`. Asserts the SQL contains `AS e` (alias not
   buried), contains `e.name`, and does **not** contain `__td_jl`.
2. `using_parent_hoisted_slots_qualify_by_covering_alias` — same input,
   dispatched as the bare `Join` op (no enclosing `Project`). Asserts the
   rendered SQL starts with `SELECT dept_id, e.id, e.name, e.salary,
   d.dept_name` (bare USING key, then `e.`-qualified emp fields in schema
   order — both `dept_id` occurrences correctly excluded by the USING
   dedup — then `d.`-qualified dept fields) and that the left side still
   inlines (`!contains("__td_jl")`).
3. `using_parent_with_uncoverable_side_still_wraps` — the residual gap the
   plan calls out to track, not fix: nested join's children are each a
   `Project` over a scan (re-scoping empties the nested join's own
   `RelScope`), so `scope_covers_fields` fails and change 2's guard falls
   back to the pre-F5 wrap. Asserts `AS __td_jl` is present.

Table/schema fixtures used: existing `emp_schema`/`dept_schema`/`emp2_schema`
via `base_types_emp_dept_emp2` (already present from the F2 pin — `dept_id`
exists on `emp`, `dept`, and `emp2`, satisfying the plan's "USING key exists
on both sides" requirement without adding new fixtures). No new helper
functions were needed; `scan`, `aliased_scan`, `qcol`, `int_lit`,
`ColumnReference::untyped` were all already present.

## Re-baselining

None. No existing test assertion was changed. Ran the full
`transpiler_v2::emission` test module plus the whole `thunderduck-core --lib`
suite — all pre-existing tests pass unchanged (see Verification below),
confirming the plan's stated expectation ("the only new shapes are
USING-parent inlines that previously wrapped" — i.e. additive, not
regressive).

## Deviations from the plan

None. Implemented per plan items 1–3 exactly; helper names
(`scope_covers_fields`, `side_slot_quals`) match the plan's suggested names.

## Residual (tracked, not fixed here — per plan's Acceptance note)

A USING parent whose left side has an UNCOVERABLE scope (e.g. both nested
children are `Project`/`Aggregate`/etc. that re-scope) still wraps under
`AS __td_jl`, and a qualified reference above it can still strand loudly
against the empty USING `RelScope` (same class of bug as F5, narrower,
un-witnessed in the current corpus). Test 3 pins the wrap continues to
happen; no attempt was made to widen coverage further, per plan scope.

## Verification

1. **`cargo check -p thunderduck-core`** — PASS (clean, no errors/warnings
   from this change).
2. **`cargo test -p thunderduck-core --lib`** — PASS: `963 passed; 0 failed;
   5 ignored`. Ran twice (before and after `rustfmt`), both green; no
   `EMIT_TAP_MUTEX` poison cascade observed. Also ran the 3 new tests in
   isolation with `--exact` first — all passed individually before the full
   run.
3. **`git diff --name-only HEAD -- '*.rs' | xargs -r rustfmt --check
   --edition 2021`** — initially found formatting drift in the new code
   (long closures / assert! calls exceeding the line-width default); ran
   `rustfmt --edition 2021` on the single touched file to fix, then
   `rustfmt --check` — PASS (clean).
4. **`cargo clippy -p thunderduck-core --lib --tests`** — ran full crate
   clippy; the only warnings emitted are pre-existing, in files this task
   did not touch (`runtime/session.rs`, `transpiler_v2/analyzer.rs`,
   `parser_v2/mod.rs`). Zero warnings attributable to
   `transpiler_v2/emission.rs`.

No corpora were run, no commits were made, no release build was performed —
per task constraints.

## Fix round 1 (review APPROVE-WITH-FIXES; `crates/core/src/transpiler_v2/emission.rs` only)

Applied both findings from the plan-007 review.

### [Medium] Exposure-aware coverage predicate

`scope_covers_fields`/`side_slot_quals` previously trusted the analyzer
`RelScope`'s LOGICAL alias names alone. That's unsound for an inlined
nested-join side whose OWN sub-sides are synthetic-wrapped
(`left/right_requires_synthetic`): the nested join's stamped `RelScope`
still reports the logical alias (`RelScope::of`'s `Join` arm derives purely
from the children's scopes, independent of the synthetic-wrap emission
decision), but the nested join's *emitted* `FromItem` renders those children
under `__td_jl`/`__td_jr` instead — a derived slot like `emp.col` would not
bind against that FROM shape.

Fix: extracted one shared predicate, `covering_alias(side: &TypedAst, item:
&FromItem, i: usize) -> Option<String>`, that finds the first `RelScope`
alias covering field `i` AND requires that alias be present in
`item.exposed()` (case-insensitive). Both call sites now route through it:

- `scope_covers_fields(side, item)` — signature widened to also take
  `item`; body is now `(0..n).all(|i| covering_alias(side, item, i).is_some())`.
- `side_slot_quals(side, item)` — multi-alias branch now
  `(0..n).map(|i| covering_alias(side, item, i)).collect()` (the separate
  `scope_covers_fields` early-return was folded away — `Option::collect`
  already short-circuits to `None` on the first miss, so the logic is
  identical without the redundant pre-check).

Call-site update in `build_join_side`'s `inline_ok`: the `FromItem::Join {
.. }` match arm now binds the scrutinee (`item @ FromItem::Join { .. }`) so
`scope_covers_fields(side, item)` has the actual emitted `FromItem` for this
side available — exactly the item `block.from_ref()` already exposed at
that point, so no new borrow or clone was needed.

Also honestly restated the "always `Some` by construction" comment in
`build_join`'s USING-branch `default_slots`: the guarantee still holds (an
inlined multi-alias left side is checked with the SAME exposure-aware
`scope_covers_fields(side, &item)` against the SAME `item` inside
`build_join_side`, so `side_slot_quals` can't fail once `build_join_side`
decided to inline), but the comment now explains that the guarantee is
conditioned on `item` exposure, not RelScope range coverage alone, and
calls out the shape that used to slip through before this fix (a
synthetic-wrapped sub-side).

New pin: `using_parent_with_synthetic_scoped_side_stays_wrapped` — a plain
ON nested join (`emp` × `dept`) whose own condition carries plan_id-tagged
`dept_id` refs ambiguous across both sides (mirrors
`analyzer::tests::join_flags_set_when_condition_carries_plan_id_ambiguity`'s
`plan_id_join` construction), which the analyzer's
`mark_join_alias_requirements` pass stamps as the nested join's OWN
`left_requires_synthetic`/`right_requires_synthetic = true`. That join is
placed as the LEFT side of an outer `USING(dept_id)` join against `emp2`.
The test asserts (with an explicit premise check on the stamped flags
first): the SQL contains `AS __td_jl` (the side stays wrapped at the outer
level rather than inlining), and neither `emp.` nor `dept.` appears
anywhere in the emitted SQL (no stranded logical-alias qualifier is ever
produced from the now-invisible RelScope names). Before this fix, the old
RelScope-only predicate would have inlined this side and qualified its
hoisted USING-branch default slots with `emp.`/`dept.` — unbindable against
the actual `__td_jl`/`__td_jr` FROM shape.

### [Low] Misattached doc comment

The build_join_side ladder doc (steps 1-3 plus the duplicate-alias-guard
paragraph) had drifted onto `fn scope_covers_fields` when the F5 helpers
were inserted ahead of `build_join_side`, leaving `fn build_join_side`
itself undocumented. Moved the ladder doc back to directly above `fn
build_join_side` (step 2's wording updated to say "an alias the nested
join's emitted `FromItem` actually exposes", matching the tightened
predicate). Gave `scope_covers_fields` and the new `covering_alias` helper
their own accurate, short `///` docs in place, and updated
`side_slot_quals`'s doc to reference `covering_alias` instead of the old
in-body RelScope-only lookup description.

### Verification (fix round 1)

1. **`cargo check -p thunderduck-core`** — PASS, clean.
2. **`cargo test -p thunderduck-core --lib`** — PASS: `964 passed; 0 failed;
   5 ignored` (963 pre-existing + 1 new pin). Also ran
   `transpiler_v2::emission::tests::` in isolation (275 passed) and the new
   test alone with `--exact` first — both green before the full run.
3. **`git diff --name-only HEAD -- '*.rs' | xargs -r rustfmt --check
   --edition 2021`** — PASS, clean (no drift).
4. **`cargo clippy -p thunderduck-core --lib --tests`** — zero warnings
   attributable to `transpiler_v2/emission.rs` (grepped the output for the
   file path — no hits); pre-existing warnings in other files unchanged.

No `.unwrap()`/`.expect()` were added outside `mod tests`. No files other
than `crates/core/src/transpiler_v2/emission.rs` were touched. No corpora
run, no commit, no release build — per task constraints.
