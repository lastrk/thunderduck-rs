# Plan 008 — F7: never inline a join side that exposes synthetic aliases

Fixes finding 7 of `tasks/select-block-review-findings.md` (witness
`join-022`): an inlined nested join whose sub-sides are synthetic-wrapped
exposes `__td_jl`/`__td_jr` in the parent FROM; when the parent's right
side independently wraps as `__td_jr` (flags or non-pure block), the
duplicate-alias guard "fixes" the collision by re-wrapping under the SAME
`TD_JOIN_RIGHT` name → two `AS __td_jr` in one scope → DuckDB
`Ambiguous reference to table "__td_jr"` on any qualified `__td_jr` ref
(e.g. an ancestor plan_id-stamped filter merged into the block) while
Spark succeeds. Emission-side only; do NOT touch analyzer.rs.

## Why refusal, not renaming (architect decision, recorded)

The `__td_jl`/`__td_jr` names are the analyzer↔emission contract: ancestor
references are STAMPED with those literal qualifiers
(`mark_join_alias_requirements`), so a renamed wrap would strand every
stamped ref. Refusing the inline instead confines the child join's
synthetic names inside a derived sub-scope: `( ... AS __td_jl JOIN ...
AS __td_jr ON ... ) AS __td_jl` — the inner names cannot collide with the
outer's, and `RelScope::lookup_plan_id` is outermost-first, so refs above
the OUTER join are always stamped with the OUTER side qualifier (which
binds the wrap alias). Demands never cross into sides (`mark_node`
recurses with `false, false`), so nothing inside the wrap is demanded from
outside. Flat-chain inlining is lost only for synthetic-stamped child
joins — a correctness-first trade.

## Changes (crates/core/src/transpiler_v2/emission.rs only)

### 1. build_join_side: synthetic-exposure refusal

In the `FromItem::Join { .. }` arm of `inline_ok`, add a conjunct: the
item must NOT expose a synthetic join alias. Implement as a small helper
with a /// doc:

```rust
/// Whether a lowered FROM item exposes one of the synthetic join-side
/// aliases (`__td_jl` / `__td_jr`, exact match — they are emission-
/// generated). Such an item must never inline into an enclosing FROM
/// scope: the enclosing join may need the same name for its own side
/// (the duplicate-alias guard cannot rename, because ancestor references
/// are stamped with these literal qualifiers), so the child keeps its
/// derived wrap and its synthetic names stay confined to the sub-scope.
fn exposes_synthetic_alias(item: &FromItem) -> bool
```
using `item.exposed().iter().any(|a| a == TD_JOIN_LEFT || a == TD_JOIN_RIGHT)`
(TD_JOIN_LEFT/TD_JOIN_RIGHT are already imported in emission.rs).
Place the conjunct alongside the existing nested-join guard conditions;
the Relation/Derived/Raw arms are unchanged (a Derived's own alias is the
synthetic wrap itself when flag-driven — that path is the `requires_synthetic`
early return and stays as is).

### 2. Doc note on the duplicate-alias guard

Extend the guard's comment (build_join, the `collides` block): with
change 1, a collision can no longer involve `__td_jr` from an inlined
child; the same-name re-wrap is therefore sound for USER alias collisions
(the right side moves under a name the left cannot also expose). Mention
the one remaining pathological case — a USER alias literally named
`__td_jl`/`__td_jr` — is out of scope here (tracked with finding 13's
untrusted-`__td_*` class).

## Invariants

- ADR-022 single path; analyzer untouched; the `__td_jl`/`__td_jr`
  contract (stamped names bind emission wraps) preserved exactly.
- Gotcha 4 (semi/anti chain break) and the cycle-2 USING coverage guard
  unchanged — this adds a refusal, never a new inline.
- No unwrap/expect; /// docs; no SQL-string parsing.

## Tests (emission.rs mod tests; tap_guard; --exact triage)

New pins:
1. `synthetic_exposing_child_join_never_inlines` — build the join-022
   shape as CommonAst: inner Join{scan(emp), scan(emp2), condition with
   plan_id-carrying refs on an AMBIGUOUS column name so the analyzer
   stamps both inner sides requires_synthetic (mirror the existing
   join_flags_* construction pattern: UnresolvedColumn with plan_id +
   left_plan_ids/right_plan_ids on the CommonOp::Join)}, as the LEFT of an
   outer plain-ON Join whose RIGHT is a Project (non-pure → wraps
   `AS __td_jr`). Dispatch the outer join (or a Filter above it whose
   condition is a plan_id ref stamped `__td_jr`). Assert:
   `sql.matches("AS __td_jr").count() == 1` and
   `sql.matches("AS __td_jl").count() == 1` (the child is ONE derived wrap;
   its inner synthetic wraps are inside parentheses — if the inner block
   also contains `AS __td_jl`/`AS __td_jr` strings, count accordingly and
   assert on the OUTER scope by checking the child renders as
   `(SELECT ... ) AS __td_jl` — pick assertions that fail on the OLD code,
   where the inlined child put two `AS __td_jr` in one FROM; e.g. assert
   the total `AS __td_jr` count is 1 when the inner condition demands only
   one side, or construct so old code yields 2 and new code yields 2 but
   in DIFFERENT scopes — SIMPLEST ROBUST ASSERTION: old code emits
   `AS __td_jl INNER JOIN` twice-flattened FROM with both inner wraps as
   siblings of the outer right wrap; new code emits `(SELECT` immediately
   for the left side. Assert the left side is wrapped:
   `FROM (SELECT` prefix present AND `) AS __td_jl INNER JOIN` present,
   and that the string ` AS __td_jr` appears exactly once OUTSIDE the
   parenthesized left body is hard to express — prefer parsing-free
   count: with only the inner RIGHT stamped (condition demanding just
   `__td_jr`), old code emits two `AS __td_jr` (inner side + outer right),
   new code emits exactly one at the outer level plus one INSIDE the
   wrap... still two total. THEREFORE: make the inner join demand ONLY
   `__td_jl` (condition refs stamping just the left side is not possible —
   qualify_plan_id_refs stamps both sides of the condition). FALLBACK
   ASSERTION (correct and old-code-failing): assert
   `sql.contains(") AS __td_jl INNER JOIN")` — the child join is a single
   derived wrap — and `!sql.contains("AS __td_jr INNER JOIN (")` variants
   are brittle; the cleanest old-code-failing assertion is that the
   OUTER FROM's first item is a derived wrap: `FROM (SELECT`.
   Use: `assert!(sql.contains("FROM (SELECT"), ...)` on the outer join's
   SQL AND `assert!(!<flat shape>)` where flat shape =
   `FROM (VALUES`/`FROM (SELECT * FROM emp) AS __td_jl INNER JOIN (SELECT
   * FROM emp2) AS __td_jr INNER JOIN` — the coder should print the OLD
   emitted SQL first (temporarily, before the fix) to pick two assertions
   that (a) fail pre-fix, (b) pass post-fix, and (c) do not depend on
   incidental formatting. Document the chosen assertions in the log.
2. `plain_on_child_join_without_synthetics_still_inlines` — guard against
   over-refusal: the existing flat-chain shape (nested plain-ON join of
   two AliasedRelations, no plan_id stamping) as the LEFT of an ON parent
   still inlines (no `(SELECT` wrap for the left side; `AS e ... AS d ...
   INNER JOIN` flat). May already exist — if an existing pin covers it,
   name it in the log instead of duplicating.

## Verification (before returning)

`cargo check -p thunderduck-core`; `cargo test -p thunderduck-core --lib`
all green; scoped rustfmt --check clean; no new clippy warnings on your
lines. No corpora, no commits, no release build.

## Acceptance (orchestrator)

witness-progress.sh: REGRESSIONS 0; WITNESS FLIPS 8/8 (join-022/F7 green —
the last manifest witness).
