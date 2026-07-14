# F9 implementation report — aggregate wrap-path strand

**File touched:** `crates/core/src/transpiler_v2/emission.rs` only (confirmed via
`git diff --stat` — no other file in the working tree was edited by this task;
the pre-existing diffs in `.claude/*`, `tasks/select-block-review-findings.md`,
`tests/integration/differential/dataframe_corpus.py`, and
`tests/integration/select_block_witness_manifest.json` were already present in
the working tree before this task started).

## The reorder

`build_aggregate` (emission.rs, was ~637) previously rendered `slots`,
`having_sql`, and `group_body` from the **original** grouping/aggregate/having
expressions, then only afterwards opened the child block and decided
merge-vs-wrap. When it had to wrap, the already-rendered SQL still carried the
original qualifiers (e.g. `e.dept_id`), which strand behind the freshly
introduced `__td_sub`.

Reordered to match `build_filter`/`build_sort`/`build_project`'s discipline:

1. Compute `rewritten_aggregates` (grouping-id splice) against the **original**
   `grouping`, unchanged — this must happen before the visibility check since
   the check chains over it.
2. `open_block(input)?` and compute `vis = exprs_visible_in(grouping.chain(rewritten_aggregates).chain(having), &block, &input.scope)` and
   `merge = block.can_accept(Clause::GroupBy) && vis` — **before** any rendering,
   over the ORIGINAL (unstripped) expressions and the PRE-wrap block.
3. Choose the expression set to render from via a `maybe_strip` closure: on
   the merge path, clone the originals verbatim (no cosmetic churn); on the
   wrap path, `strip_stranded_qualifiers(e, &block, input_schema)` against the
   still-pre-wrap `block` — producing `grouping_r`, `aggregates_r`, and
   `having_r` (having is spliced with the original `grouping` first, then
   conditionally stripped, same pattern).
4. `trace_stranded_qualifiers` is called only on the wrap path (post-strip),
   mirroring `build_filter`/`build_project`'s post-strip diagnostic call.
5. `grouping_already_folded`, `emit_group_by`, and all rendering (`slots`,
   `having_sql`, `group_body`) now run over the chosen (`_r`) expression sets
   instead of the raw `grouping`/`aggregates`/`having` — stripping only drops
   a qualifier (structure-preserving), so these are unchanged in value on the
   merge path and correctly reflect the stripped shape on the wrap path.
6. `if !merge { block = SelectBlock::wrap(block.into()); }` moved to occur
   **after** the strip (the strip predicate needs the pre-wrap block that
   still exposes the aliases) and after all rendering is complete.
7. `set_group_by`/`set_having`/`set_projections` unchanged.

`with_grouping_id_spliced` splices `grouping_id()`/`grouping()` call args
against the **original** `grouping` (not `grouping_r`) while computing
`rewritten_aggregates`, i.e. before the merge/wrap decision exists yet. This
is intentionally safe: `strip_stranded_qualifiers` is a pure structural walk
over `ColumnReference`/`UnresolvedColumn` nodes that is agnostic to whether
those nodes were reached via the flat grouping list or via a spliced
`grouping_id()` argument, and it recurses into `FunctionCall` args the same
way it recurses into an `Alias` wrapper. Since `aggregates_r[i] =
strip(rewritten_aggregates[i])` walks into the spliced args exactly like
`grouping_r[i] = strip(grouping[i])` walks the flat list, and
`strip(unaliased(x))` and `unaliased(strip(x))` render to the same SQL text
(the qualifier-drop is unaffected by an outer `Alias`/splice-unalias step),
the GROUP BY body and any `grouping_id()` call args stay textually consistent
after the wrap-path strip. Verified by inspection of `rewrite_grouping_id`
(`f.args = grouping.iter().map(|g| g.unaliased().clone()).collect()`) and by
the full green run of the existing `rewrite_grouping_id_*`/grouping-sets
aggregate tests (unperturbed).

`build_lateral_view` was NOT touched, per the plan — its wrap branch is
unreachable from both front-ends (verified previously; left as a one-line
note in the plan, no code change here).

## Tests added (both in `crates/core/src/transpiler_v2/emission.rs`, `mod tests`)

Both derived empirically: a temporary `#[test] fn tmp_derive_*` printed the
dispatched SQL via `println!` + `--nocapture`, then was deleted and replaced
by the real pinned test with the exact substrings observed.

### 1. `aggregate_over_limit_strips_stranded_alias_on_wrap` (wrap-path pin, mirrors agg-025)

Shape: `AliasedRelation(emp, "e")` → `Sort(order=[id asc], limit=5)` →
`Aggregate(grouping=[e.dept_id], aggregates=[e.dept_id, count(*) AS count])`.

Derived SQL (pre-fix would have stranded `e.dept_id` under `__td_sub` and
DuckDB would reject it with `Referenced table "e" not found`; post-fix):

```
SELECT dept_id, count(*) AS count FROM (SELECT * FROM emp AS e ORDER BY id ASC NULLS FIRST LIMIT 5) AS __td_sub GROUP BY dept_id
```

Assertions:
- `sql.contains("AS __td_sub GROUP BY dept_id")` — confirms the wrap fired
  AND the GROUP BY key was stripped to the bare output name.
- `!sql.contains("e.dept_id")` — confirms no stranded qualifier survived
  anywhere in the emitted SQL (SELECT list or GROUP BY).

### 2. `aggregate_over_aliased_relation_merges_keeps_alias` (merge-path regression pin)

Shape: `AliasedRelation(emp, "e")` → `Aggregate(grouping=[e.dept_id],
aggregates=[e.dept_id, count(*) AS count])` (no occupied clause above the
scan, so the aggregate merges directly).

Derived SQL:

```
SELECT e.dept_id, count(*) AS count FROM emp AS e GROUP BY e.dept_id
```

Assertions:
- `sql.contains("emp AS e")` and `sql.contains("GROUP BY e.dept_id")` — the
  alias is still exposed and used verbatim (no cosmetic churn introduced by
  the reorder on the merge path).
- `!sql.contains("__td_sub")` — confirms no spurious wrap.

## Verification results

- `cargo check -p thunderduck-core` — clean.
- `cargo test -p thunderduck-core --lib` — **975 passed; 0 failed; 5 ignored;
  0 measured; 0 filtered out** (973 pre-existing + 2 new, matching the plan's
  expectation exactly). No `EMIT_TAP_MUTEX` poison cascade observed — a
  single full-suite run was green outright, no isolation pass was needed.
- `cargo fmt -p thunderduck-core -- --check` — clean after running `rustfmt`
  once on the edited region (two multi-line expressions needed reflow: the
  `having_r` let-binding and the `trace_stranded_qualifiers` chain call);
  re-verified clean afterward.
- `cargo clippy -p thunderduck-core --lib -- -D warnings` — 2 pre-existing
  errors remain (`too_many_arguments` in
  `crates/core/src/parser_v2/v2_lowering.rs:925`, `map_entry` in
  `crates/core/src/runtime/session.rs:957`), neither in `emission.rs` and
  both untouched by this change. `cargo clippy -p thunderduck-core --lib`
  (without `-D warnings`) produces zero mentions of `emission.rs` — no new
  warnings on the touched lines.

## Deviations from the plan

None of substance. One cosmetic note: the plan's suggested test assertion
`!sql.contains("\"e\".")` for the wrap-path pin was dropped after empirical
derivation showed the alias `e` in this shape is never double-quoted (DuckDB
identifier quoting is need-based and `e`/`emp` need no quoting), so that
substring would never appear regardless of correctness — `!sql.contains("e.dept_id")`
is the substring that actually pins the strand-vs-strip behavior and was kept.
