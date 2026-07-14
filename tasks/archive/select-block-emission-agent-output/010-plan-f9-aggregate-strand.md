# Plan 010 — F9 (aggregate wrap-path strand) — EXECUTABLE

**Tree:** `feat/v2-transpiler` in `/workspace`. Edit
`crates/core/src/transpiler_v2/emission.rs` only. Witness: `agg-025`
(DataFrame corpus, already added), born red:
`Referenced table "e" not found ... AS __td_sub GROUP BY e.dept_id`.
Empirically Spark 4.1.1 SUCCEEDS (`struct<dept_id:int,count:bigint>`, 3 rows).

## Bug

`build_aggregate` (emission.rs ~637) renders `slots` (grouping keys +
aggregates), `having_sql`, and `group_body` from the ORIGINAL qualified
expressions BEFORE it opens the block and decides whether to wrap. When it
must wrap (`!(can_accept(GroupBy) && vis)`), the pre-rendered SQL still
carries the original qualifiers (`e.dept_id`), which are now stranded behind
`__td_sub`. `build_filter`, `build_sort`, and `build_project` all avoid this
by stripping stranded qualifiers against the PRE-wrap block before rendering
(see `build_filter` emission.rs ~1006: `strip_stranded_qualifiers(condition,
&block, &input.resolved_schema)` then `SelectBlock::wrap`). `build_aggregate`
never strips — that is the whole bug.

## Fix (build_aggregate only)

Reorder so the block is opened and the wrap decision made BEFORE rendering,
then — only on the wrap path — strip the grouping keys, aggregates, and
having against the pre-wrap block, exactly like `build_filter`. Mechanism:

1. Keep the `GroupingSets` boundary guard at the top unchanged.
2. `let mut block = open_block(input)?;` (move up).
3. Compute `rewritten_aggregates` (splice grouping into `grouping_id()` as
   today) and the visibility check over the ORIGINALS:
   `let vis = exprs_visible_in(grouping.iter().chain(rewritten_aggregates.iter()).chain(having), &block, &input.scope);`
   `let merge = block.can_accept(Clause::GroupBy) && vis;`
4. Choose the expression set to render from. When merging, use the originals;
   when wrapping, strip each against the PRE-wrap `block`
   (`strip_stranded_qualifiers(e, &block, input_schema)`), mirroring
   `build_filter`:
   - `grouping_r: Vec<Expression>` — grouping, stripped iff `!merge`.
   - `aggregates_r: Vec<Expression>` — `rewritten_aggregates`, stripped iff
     `!merge`.
   - `having_r: Option<Expression>` — `having.map(with_grouping_id_spliced …)`,
     stripped iff `!merge`.
   Stripping is structure-preserving (only drops a qualifier), so
   `grouping_already_folded` and `emit_group_by` are invariant — compute them
   as today but over `grouping_r`/`aggregates_r` for internal consistency.
5. Render `slots` (keys = `grouping_r` unless already-folded, chained with
   `aggregates_r`), `group_body` (from `grouping_r`), `having_sql` (from
   `having_r`) — same rendering code as today, over the chosen exprs.
6. `if !merge { block = SelectBlock::wrap(block.into()); }` (AFTER strip — the
   strip predicate needs the pre-wrap block that still exposes the aliases).
7. `set_group_by`/`set_having`/`set_projections` as today.

Do NOT change `build_lateral_view` — its wrap branch is unreachable from both
front-ends (SQL `LATERAL VIEW` attaches to a pure-FROM relation; DataFrame
`select(explode(col('e.tags')))` routes through `build_project`). Verified:
the candidate witness for it was green before removal. Leave a one-line note
in the report (already recorded), no code.

## Tests (emission.rs `#[cfg(test)] mod tests`)

Add a unit test mirroring agg-025 at the emission level:
`aggregate_over_limit_strips_stranded_alias_on_wrap` — build
`AliasedRelation(emp, "e")` → `Sort(id, limit=5)` → `Aggregate(grouping=[qcol("e","dept_id")], aggregates=[count])`. Dispatch and assert:
the output wraps (`AS __td_sub`), the GROUP BY references bare `dept_id` (no
`e.` qualifier), and `!sql.contains("\"e\".")` / no stranded `e.dept_id`.
Derive the exact substring empirically (print then remove); document in
`.agent-output/010-implementation-f9.md`.

Also add a MERGE-path regression pin (no wrap):
`aggregate_over_aliased_relation_merges_keeps_alias` — a groupBy directly
over an aliased relation where the alias IS still exposed (merge path), to
confirm the reorder did not perturb the common merge case (e.g. `emp e`
groupBy `e.dept_id` count → single SELECT, GROUP BY binds; no `__td_sub`).

Keep all existing aggregate tests green (the reorder must be behavior-
preserving on the merge path).

## Verification (coder — NO corpora, NO commits)

- `cargo check -p thunderduck-core`
- `cargo test -p thunderduck-core --lib` ALL green (currently 973; +2 new)
- `rustfmt --check` clean on emission.rs
- `cargo clippy -p thunderduck-core --lib` — no new warnings on touched lines
- Log SQL derivation + results to `.agent-output/010-implementation-f9.md`

## Acceptance (orchestrator)

`witness-progress.sh`: REGRESSIONS 0, and agg-025 flips red→PASSED (WITNESS
FLIPS include F9). The 8 prior witnesses stay green.
