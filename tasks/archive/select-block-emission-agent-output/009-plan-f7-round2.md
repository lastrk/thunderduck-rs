# Plan 009 — F7 round 2 (duplicate `__td_jr` collision) — EXECUTABLE

**Branch/tree:** `feat/v2-transpiler` in `/workspace` (the old worktree is
gone). All edits in `crates/core/src/transpiler_v2/emission.rs` only. This
supersedes the stashed round-1 attempt (`stash@{0}`) — do NOT apply the
stash; the current tree is already in the correct "reverted" (no refusal)
state.

## The bug (witness join-022, currently RED)

`_join_022`: `inner = emp.join(emp2, emp.dept_id==emp2.dept_id)` (plan_id
refs on ambiguous `dept_id` → inner sides stamped `__td_jl`/`__td_jr`);
`d3 = dept.select("dept_id","dept_name")`;
`inner.join(d3, dept_name=='Data').filter(d3.dept_id==20)`.

The inner join is a plain-ON left side → it inlines into the outer FROM,
exposing `__td_jl` AND `__td_jr`. The outer filter's plan_id ref makes the
outer RIGHT (`d3`) contract-demand `__td_jr` (`right_requires_synthetic ==
true`). Result: two `AS __td_jr` in one FROM scope + a `__td_jr.dept_id`
ref in WHERE → DuckDB `Ambiguous reference to table "__td_jr"`. Spark
succeeds.

Round-1 (stashed) tried a blanket "child exposing a synthetic alias never
inlines" refusal. That FAILED its gate: tpch-q08 regressed (the refusal
wrapped a mixed left chain and buried user alias `n1` referenced by the
outer ON) and join-022 flipped from the binder error into SILENT DATA
CORRUPTION (single-alias `__td_jl` over emp+emp2's DUPLICATE column names
→ name-based default slots emitted `__td_jl.id` twice, double-binding).
Round 2 fixes both root causes and is reactive (only touches the actual
collision shape), so q08's inlining is preserved.

## Changes (emission.rs)

### Change 2 — non-USING joins NEVER build default slots

In `build_join` (currently lines 438–544), the `default_slots`
computation. Replace the whole `if using_columns.is_empty() { <single_alias
branch, lines 438–477> } else { <USING branch> }` so the non-USING arm is
simply `None`:

```rust
let default_slots = if using_columns.is_empty() {
    // Non-USING joins render bare `SELECT *`. DuckDB expands `*` over a
    // plain ON/CROSS/semi/anti join left-then-right in natural order —
    // exactly the analyzer's resolved_schema order — so a hoisted list adds
    // nothing here. Worse, a single-alias side over a DUPLICATE-name schema
    // (e.g. an inlined `emp JOIN emp2` wrapped as one `__td_jl`) makes the
    // name-based slot list emit `__td_jl.id` twice and double-bind the first
    // `id` (silent corruption; join-022 round-1). `*` is positional and
    // never double-binds. The hoisted list is a USING-only device (it alone
    // enforces Spark's key-first output order, which DuckDB's `*` breaks).
    None
} else {
    // USING joins (F5): per-field qualifiers ... <UNCHANGED existing block,
    // lines 478–543, EXCEPT prepend the change-3 dup-name guard below>
    ...
}
.filter(|slots: &Vec<DefaultSlot>| !slots.is_empty());
```

Delete the now-unused `single_alias`/`left_alias`/`right_alias` closure and
bindings. Keep the trailing `.filter(!is_empty)`.

**Consumers already verified safe with `None` for non-USING joins**
(no coder action needed, stated for context): `build_drop_columns`
(emission.rs:1611) falls to `* EXCLUDE (…)` (positionally correct for plain
joins; the key-reorder bug was USING-only); `render_project_merge_slots`
(873) with `None` → `render_projection_slots`, so `select('*', extra)` over
a plain join expands `*` positionally.

### Change 3 — USING join over a duplicate-name side is a boundary error

Inside the `else` (USING) arm of `default_slots`, BEFORE building slots:
for each NEEDED side (`left` always; `right` iff `need_right`), if that
side's `resolved_schema` has case-insensitive duplicate field names, emit:

```rust
bail_boundary_op!(
    "Join",
    "USING join over an input with duplicate column names is not supported \
     (per-field qualified slots would double-bind the duplicate and `*` \
     breaks USING key order — both silently corrupt data)"
);
```

Match the existing `bail_boundary_op!` usage in this file (it early-returns
`Err(EmissionError::…)`; `build_join` returns `Result`, so `?`/return is
fine). Doc-comment the rationale at the site. Rationale: name-qualified
slots double-bind duplicates and `*` breaks USING key order — both silent
corruption; the pre-branch renderer had the same latent flaw. An honest
ADR-022 boundary error is the correct interim (no baseline-green case
exercises the shape — the gate verifies).

Helper (local): case-insensitive duplicate detection over
`schema.fields.iter().map(|f| &f.name)` using a `HashSet<String>` of
lowercased names (`HashSet` already imported in this file).

### Change 4 — duplicate-alias guard: rename-when-free, retry-left on contract collision

Rework the guard (currently lines 411–427). Collision detection stays
(case-insensitive intersection of `left_item.exposed()` /
`right_item.exposed()`). On collision:

- **If `right_requires_synthetic == false`** (the right's `__td_jr` wrap
  name is NOT analyzer-demanded): rewrap the right under the first
  NON-COLLIDING name in the sequence `__td_jr`, `__td_jr_2`, `__td_jr_3`, …
  (checked case-insensitively against `left_item.exposed()`; bounded loop
  `2..=64` then a defensive fallback — no `unwrap`, no unbounded loop). For
  ordinary user-alias collisions this degenerates to today's `__td_jr`.

- **Else** (`right_requires_synthetic == true`, name is the contract and
  cannot move) **and left's exposure contains `__td_jr`
  (`TD_JOIN_RIGHT`, case-insensitive)**: rebuild the LEFT by re-calling
  `build_join_side(left, TD_JOIN_LEFT, left_requires_synthetic, /*may_inline_nested_join=*/false, has_using)`
  so the left chain collapses to a single `AS __td_jl` derived wrap and its
  internal synthetics (`__td_jr`) are confined to the sub-scope. Outermost-
  first plan_id stamping guarantees no ancestor demands the buried inner
  names (established in review 008). Re-run the collision check after the
  rebuild for the doc-comment's sake; record — do NOT fix — the residual:
  a flag-demanded collision where the rebuilt left chain ALSO exposes user
  aliases referenced by ancestor expressions would strand those loudly
  (triple-rare, un-witnessed).

- The mirror case (left flag-demanded, right exposes `__td_jl`) is
  reachable only via a user alias literally named `__td_jl` — out of scope
  (F13 class); note in the doc, no code.

Keep the guard a single well-doc-commented block. `left_item` must become
`mut` for the retry.

## Tests (emission.rs `#[cfg(test)] mod tests`)

Derive exact string assertions by printing pre/post SQL (as round 1 did);
document the derivation in `.agent-output/009-implementation-f7-round2.md`.

1. **NEW `contract_collision_wraps_left_keeps_right_name`** — the join-022
   shape: inner `emp JOIN emp2` with plan_id-stamped condition (inlinable
   plain-ON left), outer right a `Project` over `dept` that is
   flag-demanded `__td_jr` via an ancestor plan_id filter (construct so
   `right_requires_synthetic == true`). Assert: exactly one `AS __td_jr` at
   the outer scope, its body the right side; the left rendered as a single
   derived wrap (`) AS __td_jl INNER JOIN`); the filter's `__td_jr.` qualifier
   present in WHERE. Mirrors `_join_022`.

2. **NEW `free_collision_renames_right_wrap`** — same shape WITHOUT the
   ancestor filter (right NOT flag-demanded): assert `AS __td_jr_2` present
   and the old duplicate `__td_jr` absent at the colliding scope.

3. **NEW `plain_join_over_dup_name_side_renders_star`** — a single-alias
   side with duplicate names (alias/wrap over `emp JOIN emp2`) under a
   plain-ON parent: assert the outer SELECT is `*` / default-free (no
   `__td_jl.id` name-qualified list) — the join-022 corruption pin at unit
   level.

4. **NEW `using_join_over_dup_name_side_is_boundary_error`** — a USING join
   whose left is a dup-name derived side → expect the boundary error
   (`EmissionError::Unsupported`/whatever `bail_boundary_op!` yields), using
   the existing boundary-error assertion pattern in this file.

5. **KEEP** `render_project_over_nested_join_flattens_three_way_chain`
   (7747), `render_project_over_nested_join_duplicate_alias_refuses_flatten`
   (7796), `render_join_from_dataframe_plan_id_contract_keeps_td_jl_no_flatten`
   (7849) — the revert restores plain-chain inlining; these must still pass.

6. **Re-baseline** any unit test that asserts an explicit qualified default-
   slot SELECT list over a NON-USING (plain ON/CROSS) join to the `*` shape,
   with a one-line justification each. (Search found none asserting the bare
   plain-join slot list, but confirm by running the suite — USING-slot pins
   at 6993/7034/7095/7137 MUST NOT change.)

## Verification (coder, before returning — NO corpora, NO commits)

- `cargo check -p thunderduck-core`
- `cargo test -p thunderduck-core --lib` ALL green
- scoped `rustfmt --check` on emission.rs
- no NEW clippy warnings on touched lines (`cargo clippy -p thunderduck-core
  --lib` — note the 9 pre-existing repo-wide errors are unrelated; only your
  touched lines must be clean)
- log SQL-derivation + results to `.agent-output/009-implementation-f7-round2.md`

## Acceptance (orchestrator, after review)

`tests/scripts/witness-progress.sh`: **REGRESSIONS: 0** (tpch-q08 stays
PASSED) and **WITNESS FLIPS: 8/8** (join-022 green with CORRECT DATA, not
merely no-error).
