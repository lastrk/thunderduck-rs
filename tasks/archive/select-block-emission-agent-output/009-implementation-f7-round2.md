# F7 round 2 — duplicate `__td_jr` join-side alias collision — implementation report

Branch/tree: `feat/v2-transpiler` in `/workspace`. All edits confined to
`crates/core/src/transpiler_v2/emission.rs`. No other file touched.

## Changes landed

### Change 2 — non-USING joins never build a hoisted default-slot list

`build_join`'s `default_slots` computation: the non-USING (`using_columns.is_empty()`)
arm is now `None` unconditionally (was: a `single_alias`-keyed name-qualified list).
Deleted the old `single_alias`/`left_alias`/`right_alias` closure/bindings. DuckDB's
bare `SELECT *` over a plain ON/CROSS/semi/anti join expands left-then-right in the
analyzer's resolved-schema order, so the hoisted list added nothing there and, over a
duplicate-name single-alias side, actively corrupted output (round-1 residual:
`__td_jl.id` emitted twice, silently dropping the second column).

### Change 3 — USING join over a side that would double-bind under one qualifier is a boundary error

Landed, but **refined from the plan's literal spec** after empirical verification
surfaced a false positive (see "Deviation from plan" below). Final shape:

- Helper `has_unsafe_qualified_duplicate<'a>(fields: impl Iterator<Item = (&'a str, &'a str)>) -> bool`
  (emission.rs, next to `side_slot_quals`) — detects whether two `(name, qualifier)`
  pairs collide case-insensitively, i.e. whether two schema fields would be hoisted
  under the literal SAME `qualifier.name` reference.
- In the USING arm of `default_slots`, immediately after `left_quals`/`right_quals`
  are computed (and before building the slot list), a `side_unsafe` closure filters
  each side's schema fields to the non-USING-key ones (the only ones a side actually
  contributes a qualified slot for) and checks for a real collision via the helper
  above. If either needed side is unsafe, `bail_boundary_op!("Join", "USING join over
  an input with duplicate column names is not supported (per-field qualified slots
  would double-bind the duplicate and `*` breaks USING key order — both silently
  corrupt data)")`.

**Deviation from plan**: the plan specified schema-level duplicate-*name* detection
(`has_duplicate_names(schema)`, a raw case-insensitive name collision check,
independent of qualifiers). Implementing it exactly as specified caused a **real
regression**: `alias_ref_above_using_parent_inlines_and_binds` (an existing,
previously-green F5 regression pin) failed, because its LEFT side — a plain-ON
`emp e JOIN dept d ON e.dept_id = d.dept_id` inlined multi-alias under a USING(dept_id)
parent — legitimately has `dept_id` appearing twice in its raw schema (once from `e`,
once from `d`), but each occurrence is hoisted under its OWN distinct covering alias
(`e.dept_id` vs `d.dept_id` via `side_slot_quals`'s multi-alias path), so there is no
actual double-bind. The plan's raw-name check is too coarse and would turn this
legitimate, previously-working F5 shape into a false-positive boundary error.

Root-caused via the "verification before done" full-suite run (see below), fixed by
keying the check on the *qualified* `(qualifier, name)` pair (what will actually be
emitted into the hoisted slot list) rather than the raw name — this is exactly what
distinguishes the genuine round-1 corruption case (single-alias wrap: every field
gets the SAME qualifier, so two same-named fields collide) from the legitimate F5
multi-alias case (distinct qualifiers per field, no collision even though names
repeat). `has_duplicate_names` was never committed as a standalone function; it was
replaced by `has_unsafe_qualified_duplicate` before this file reached a green state.

### Change 4 — duplicate-alias guard: rename-when-free, retry-left on contract collision

`build_join`'s duplicate-alias guard (previously an unconditional right-side rewrap)
reworked to two branches on collision:
- `right_requires_synthetic == false` (free to move): rewrap the right side under the
  first non-colliding name in `__td_jr`, `__td_jr_2`, … `__td_jr_64` (bounded, no
  `unwrap`, defensive fallback `__td_jr_64` if all 64 collide — never observed).
- `right_requires_synthetic == true` (contract, cannot move) and the left's exposure
  contains `__td_jr`: rebuild the LEFT via
  `build_join_side(left, TD_JOIN_LEFT, left_requires_synthetic, /*may_inline_nested_join=*/false, has_using)`,
  collapsing it to a single derived `AS __td_jl` wrap that confines its own inner
  `__td_jr` to a nested sub-scope.
- The mirror case (left flag-demanded, right exposing `__td_jl`) and the residual
  (rebuilt left ALSO exposing an ancestor-referenced user alias) are documented in the
  guard's doc comment as out-of-scope / un-witnessed, per the plan — no code added for
  either.

`left_item` is now `mut` to support the retry.

## Tests

### 1. `contract_collision_wraps_left_keeps_right_name` (NEW) — join-022 mirror

Fixture: `inner = emp.join(emp2, plan_id(1).dept_id == plan_id(2).dept_id)` (own
condition self-demands both sides `__td_jl`/`__td_jr`); `d3 = dept.select(dept_id,
dept_name)`; `outer = inner.join(d3, dept_name == 'Data')` with `right_plan_ids: [3]`
on the outer join; `filter(plan_id(3).dept_id == 20)` as ancestor, forcing
`right_requires_synthetic == true` on the outer join's right (`d3`).

SQL derived empirically (printed then removed):
```
SELECT * FROM (SELECT * FROM (SELECT * FROM emp) AS __td_jl INNER JOIN
(SELECT * FROM emp2) AS __td_jr ON (__td_jl.dept_id) = (__td_jr.dept_id))
AS __td_jl INNER JOIN (SELECT dept_id, dept_name FROM dept) AS __td_jr
ON (dept_name) = ('Data') WHERE (__td_jr.dept_id) = (20)
```
Assertions pin this by structure (`starts_with` the collapsed-left prefix,
`contains` the outer right's wrap + ON clause, `ends_with` the WHERE clause) rather
than a raw `AS __td_jr` occurrence count — two literal `AS __td_jr` occurrences are
correct here (one per FROM scope: the inner join's own buried pair, and the outer's
contract-demanded wrap), not a bug. (First attempt used a count-based assertion
expecting exactly 1; running the test showed 2, both structurally correct once the
scopes were examined — fixed to the structural assertions above.)

### 2. `free_collision_renames_right_wrap` (NEW)

Same shape without the ancestor filter (`right_plan_ids: []` on the outer join, no
`Filter` above) — `right_requires_synthetic == false`. Empirically confirmed:
`sql.contains("AS __td_jr_2")` and `sql.matches("AS __td_jr ").count() == 1` (the
inner join's own `__td_jr` survives unrenamed; the outer's free-to-move wrap is
renamed to `__td_jr_2`).

### 3. `plain_join_over_dup_name_side_renders_star` (NEW) — join-022 round-1 corruption pin at unit level

Fixture: `inner = emp e JOIN emp2 e2 ON e.dept_id = e2.dept_id` placed as the RIGHT
side of a plain-ON outer join against `dept` (`may_inline_nested_join` is hardcoded
`false` for the right side in `build_join_side`, so it always wraps single-alias
`__td_jr` over the duplicate-name schema regardless of contract flags). Empirically
confirmed: `sql.starts_with("SELECT * FROM")` (Change 2: no hoisted slot list at all
for a non-USING join) and `!sql.contains("__td_jr.id") && !sql.contains("__td_jr.dept_id")`
(no name-qualified double-bind).

### 4. `using_join_over_dup_name_side_is_boundary_error` (NEW, re-derived — see below)

Fixture: `inner = emp e JOIN emp2 e2 ON e.dept_id = e2.dept_id` placed as the RIGHT
side of a USING(dept_id) outer join against `dept`. As in test 3, the right side never
inlines under a USING parent (`may_inline_nested_join` is `false` for the right,
unconditionally), so `inner`'s schema (both `id`s and both `dept_id`s from `emp`/`emp2`)
gets one single-alias qualifier `__td_jr` via `side_slot_quals`'s fast path — a real
double-bind. `dispatch_op(...).expect_err("must bail")` +
`expect_unsupported(err, UnsupportedKind::Op, "Join", &["duplicate column names"])`.

**Re-derivation note**: the fixture in the plan's spec (LEFT side carrying the
duplicate-name join, aliased `e`/`e2`) does NOT trigger the (correctly, more precise)
qualified-duplicate check, because as the outer's LEFT it is eligible for multi-alias
inlining (`e`/`d2`... `e`/`e2` both real, distinct RelScope-covering aliases), exactly
like the `alias_ref_above_using_parent_inlines_and_binds` case above — no actual
collision, so `dispatch_op` now succeeds instead of erroring, and the original
`expect_err` panicked. Rebuilt onto the RIGHT-side placement (same trick as test 3,
extended to a USING parent), which unconditionally forces the single-alias wrap that
is the actual corruption shape Change 3 exists to catch.

### 5. KEEP (unchanged, still pass)

- `render_project_over_nested_join_flattens_three_way_chain`
- `render_project_over_nested_join_duplicate_alias_refuses_flatten`
- `render_join_from_dataframe_plan_id_contract_keeps_td_jl_no_flatten`

### 6. Re-baseline

No unit test asserted an explicit qualified default-slot SELECT list over a
NON-USING (plain ON/CROSS) join — none needed re-baselining. The USING-slot pins
(`using_join_side_wrap_preserves_hoisted_slots`,
`using_parent_hoisted_slots_qualify_by_covering_alias`,
`using_parent_with_synthetic_scoped_side_stays_wrapped`,
`using_parent_with_uncoverable_side_still_wraps`, and
`alias_ref_above_using_parent_inlines_and_binds`) all pass unchanged — confirmed
individually and in the full serialized suite run below.

## Verification (all green, in order)

1. `cargo check -p thunderduck-core` — clean.
2. `cargo test -p thunderduck-core --lib -- --test-threads=1`:
   ```
   test result: ok. 973 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 0.27s
   ```
   (Also reconfirmed green under the default parallel runner: `973 passed; 0 failed`.)
3. `rustfmt --edition 2021 --check crates/core/src/transpiler_v2/emission.rs` — clean
   (ran `cargo fmt -p thunderduck-core -- crates/core/src/transpiler_v2/emission.rs`
   once to settle two line-wrap diffs introduced by the new helper signature and the
   `using_lower` binding, then re-checked clean).
4. `cargo clippy -p thunderduck-core --lib -- -D warnings` — 2 pre-existing,
   unrelated errors surface (`parser_v2/v2_lowering.rs::reject_unsupported_view_clauses`
   too-many-arguments, `runtime/session.rs` map-entry) — neither touches
   `emission.rs`. A non-`-D warnings` `cargo clippy -p thunderduck-core --lib` run
   confirms **zero** warnings anywhere in `emission.rs`.

## Note on the EMIT_TAP_MUTEX poison-cascade during verification

An intermediate full-suite run (before the Change 3 refinement above) showed 104
failures. Root-caused per the project's documented pattern: exactly one test
(`alias_ref_above_using_parent_inlines_and_binds`) panicked with a real assertion
failure (my Change 3, as originally written per the plan, incorrectly bailed on its
legitimate F5 shape) while holding `EMIT_TAP_MUTEX`; every subsequent
`tap_guard()`-using test in that same sequential process then failed with
"EMIT_TAP_MUTEX poisoned" rather than its own result — 102 of the 104 failures were
this cascade, not independent regressions. Isolating the one genuine failure (by
grepping for panic locations other than `tap_guard`'s `emission.rs:6158:31`) led
directly to the Change 3 refinement and the test 4 re-derivation documented above.
After the fix, a second full run surfaced two more genuine failures — my own test 4
(fixture no longer collision-eligible under the refined check) and
`transpiler_v2::invariants::inv2_dispatch_is_only_sql_writer` (confirmed, on
individual re-run, to be pure cascade noise, not a real regression) — both resolved
by fixing test 4's fixture; the final full run is clean at 973/973.

## Summary

All four changes from the plan landed in `crates/core/src/transpiler_v2/emission.rs`
only. Change 3's detection predicate was refined from a raw schema-name check to a
qualified-`(name, qualifier)`-pair check after empirical verification caught a false
positive against an existing green F5 test; this also required rebuilding test 4's
fixture (right-side placement instead of left-side) to exercise the actual corruption
shape. 4 new unit tests added, 3 existing regression pins kept and reconfirmed
passing, no test needed re-baselining. `cargo check`, `cargo test --lib` (973/973,
both serialized and parallel), `rustfmt --check`, and `cargo clippy` (zero warnings on
`emission.rs`; only pre-existing, unrelated errors elsewhere) are all green. No
corpora run, no commits, no pushes, no release builds, per the task's constraints.

**Post-review addendum**: added `debug_assert!(!collides(&left_item, &right_item), ...)` immediately after the retry-left `left_item` rebuild (Change 4's `else if` branch) per reviewer request — safety net ensuring the collision is resolved after the rebuild, active in test builds (`debug_assertions` on). 973/973 confirmed green.
