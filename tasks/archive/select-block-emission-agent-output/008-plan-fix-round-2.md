# Plan 008 addendum — fix round 2 (gate failure analysis + redesign)

Round-1's inline refusal FAILED the witness gate: tpch-q08 regressed
(PASSED→FAILED: the refusal wrapped a mixed left chain and buried user
alias `n1`, which the outer ON condition references → `Referenced table
"n1" not found`) and join-022 turned from binder error into SILENT DATA
MISMATCH (with the child wrapped, the outer plain-ON join's left side is
single-alias `__td_jl` over a schema with DUPLICATE column names — emp and
emp2 both have id/name/age/salary — and the name-based default slots emit
`__td_jl.id` twice, double-binding the first `id`). Both root causes are
addressed together; the refusal is REVERTED and replaced.

## Changes (emission.rs only; revert/replace round-1's refusal)

### 1. REVERT the `!exposes_synthetic_alias(item)` conjunct in inline_ok

Nested plain-ON joins inline again exactly as before round 1 (q08's chain
shape must render flat, `n1` visible). Keep cycle-2's USING coverage guard
untouched. The `exposes_synthetic_alias` helper may be kept if used below,
else delete it.

### 2. Non-USING joins: NEVER build default slots

In `build_join`'s `default_slots`: when `using_columns.is_empty()`, always
`None`. DuckDB's `*` over a plain ON/CROSS/semi/anti join expands
left-then-right in natural order — exactly the analyzer's resolved_schema
order — so the hoisted list adds nothing there, and its name-based
qualification is UNSOUND over a side with duplicate output names (the
join-022 double-bind; the class pre-exists this branch for any
`alias-over-join` single-alias side). Deleting plain-join defaults kills
the whole class. Doc-comment this reasoning at the site.
Check existing unit pins asserting explicit slot lists over PLAIN joins
and re-baseline them to the `*` shape with a one-line justification each
(USING-join pins are unaffected and must NOT change).

### 3. USING joins: duplicate-name sides become a boundary error

Keep per-field slots for USING joins. Before building them, if any NEEDED
side's `resolved_schema` has case-insensitive duplicate field names, emit
a Thunderduck-boundary error (`bail_boundary_op!("Join", "USING join over
an input with duplicate column names ...")` — match the existing boundary
macro usage in this file). Rationale (doc-comment): name-qualified slots
double-bind duplicates and `*` breaks USING key order — both are SILENT
corruption; the pre-branch renderer had the same latent flaw. An honest
ADR-022 boundary error is the correct interim; no baseline-green case
exercises the shape (the witness gate verifies).

### 4. Duplicate-alias guard: rename when free, retry-left only on contract collision

Rework the guard in `build_join` (current: on collision, rewrap right
under the SAME `TD_JOIN_RIGHT`):
- Detect collision as today (case-insensitive intersection of
  left/right `exposed()`).
- If `parts.right_requires_synthetic` is FALSE: the right wrap's name is
  NOT demanded by any stamped reference (the mark pass sets the flag for
  every demand), so rewrap right under the first NON-COLLIDING name in
  the sequence `__td_jr`, `__td_jr_2`, `__td_jr_3`, … (checked against
  left's exposed() case-insensitively; bounded loop, no unwrap). For
  plain user-alias collisions this degenerates to today's `__td_jr`.
- If `parts.right_requires_synthetic` is TRUE (the name `__td_jr` is the
  analyzer contract and cannot change) AND left's exposure contains
  `__td_jr`: rebuild the LEFT side by re-calling `build_join_side` for the
  left with nested-join inlining disabled (add a
  `may_inline_nested_join: bool` — it already exists — pass `false` on
  this retry), so the left chain becomes a single `AS __td_jl` derived
  wrap and its internal synthetics are confined to the sub-scope
  (outermost-first plan_id stamping guarantees no ancestor demands the
  buried inner names — established in review 008). Then re-derive
  left-side exposure for the (now impossible) re-collision check.
  Residual (record in the change log, do not fix): a flag-demanded
  collision where the left chain ALSO exposes user aliases that ancestor
  expressions reference will strand those loudly — triple-rare,
  un-witnessed.
- Mirror case (left flag-demanded, right exposes `__td_jl`) is reachable
  only via a user alias literally named `__td_jl` — out of scope (F13
  class), note in the doc.

### 5. Tests

- REWRITE round-1's `synthetic_exposing_child_join_never_inlines` to the
  new contract, renamed e.g.
  `contract_collision_wraps_left_and_keeps_right_name`: the join-022 shape
  (stamped inner join inlin-able left; right Project flag-demanded
  `__td_jr` via an ancestor plan_id filter — construct so
  right_requires_synthetic is true). Assert: exactly one `AS __td_jr` at
  the outer scope with the right side's body following it, left rendered
  as a single derived wrap (`) AS __td_jl INNER JOIN`), and (premise)
  the filter's `__td_jr.` qualifier present in the WHERE. Derive exact
  assertions by printing pre/post SQL as in round 1; document both.
- NEW `free_collision_renames_right_wrap`: same shape WITHOUT the
  ancestor filter (right not flag-demanded): right rewrapped under
  `__td_jr_2`; assert `AS __td_jr_2` present and old dup absent.
- NEW `plain_join_over_dup_name_side_renders_star`: single-alias side
  with duplicate names (alias over join of emp+emp2) under a plain ON
  parent: assert the outer SELECT is `*`/default-free (no `__td_jl.id`
  name-qualified list) — the join-022 corruption pin at unit level.
- NEW `using_join_over_dup_name_side_is_boundary_error`: USING join whose
  left is a dup-name derived side → expect the boundary error, matching
  the existing expect_unsupported/boundary test patterns.
- KEEP `plain_on_child_join_without_synthetics_still_inlines` (must still
  pass — the revert restores inlining).
- Re-baseline any plain-join hoisted-slot pins per change 2 (list them).

## Verification (before returning)

`cargo check -p thunderduck-core`; `cargo test -p thunderduck-core --lib`
ALL green; scoped rustfmt --check; no new clippy warnings on your lines.
No corpora/commits/release builds.

## Acceptance (orchestrator)

witness-progress.sh: REGRESSIONS 0 (tpch-q08 back to PASSED) and
WITNESS FLIPS 8/8 (join-022 green — correct DATA, not just no error).
