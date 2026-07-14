# Plan 006 — F1+F2+F3+F4: make the hoisted slot list structural

Fixes findings 1–4 of `tasks/select-block-review-findings.md` with ONE
mechanism change: `default_projections` becomes a **structured, named slot
list** that every consumer can filter/extend/preserve, and the join-side
wrap path stops discarding the block that carries it. Emission-side only —
**do not touch analyzer.rs**; the analyzer's resolved_schema order is the
authority the slots mirror.

## Background (why these four are one bug)

`SelectBlock.default_projections: Option<String>` (sql_block.rs:186) is the
join builder's hoisted USING-key-first slot list (emission.rs:382-417) and
the range leaf's `id` bind (emission.rs:1994). It dies four ways:
- F1: `build_drop_columns` (emission.rs:1411) sets `* EXCLUDE (…)` — hard
  projections shadow the soft list (`to_sql` at sql_block.rs:385), and
  DuckDB's `*` keeps the USING key at natural position.
- F4: `build_project`'s merge path renders a bare `Star(None)` inside a
  multi-slot list as raw `*` (same shadowing; lone-star identity at
  emission.rs:726-728 is fine and must stay).
- F2: `build_join_side`'s non-inline path (emission.rs:283-315) calls
  `into_pure_from` (drops the shell incl. defaults, sql_block.rs:231-237)
  then rebuilds `SelectBlock::from_item(item)` → bare `SELECT *` inside
  `AS __td_jl/__td_jr`.
- F3: `extend_from` (sql_block.rs:242-249) widens FROM+scope but leaves the
  defaults stale, so a merged LATERAL VIEW's generated columns never reach
  the SELECT list.

## Changes

### 1. sql_block.rs — structured slots

```rust
/// One soft SELECT slot: the analyzer-declared output column name plus its
/// rendered SQL. Named so consumers can filter (DropColumns) or extend
/// (LateralView) the list without parsing SQL text.
#[derive(Debug, Clone)]
pub(crate) struct DefaultSlot {
    pub(crate) name: String, // output column name (analyzer casing)
    pub(crate) sql: String,  // rendered slot, e.g. `e.salary` or `dept_id`
}
```
- Field: `default_projections: Option<Vec<DefaultSlot>>`.
- `set_default_projections(&mut self, slots: Vec<DefaultSlot>)`.
- New `default_slots(&self) -> Option<&[DefaultSlot]>` accessor.
- New `extend_default_projections(&mut self, extra: Vec<DefaultSlot>)`:
  extends ONLY when defaults are `Some` (a `None` block renders `*`, which
  already covers appended lateral columns — cx-007..009 must not change).
- `to_sql`: where it did `.or(self.default_projections.as_deref())`, render
  `Some(v)` as the `", "`-join of the slot `sql`s (build the joined string
  before the `format!`; borrow care: a small `let default_list: Option<String>`
  above the existing chain keeps the code shape).
- `into_pure_from` doc: reword — defaults are dropped ONLY on true inline,
  where the enclosing FROM takes over binding (see change 3).

### 2. emission.rs — producers build named slots

- `build_join` default_slots block (369-417): produce `Vec<DefaultSlot>`:
  USING cols → `{name: c.clone(), sql: quote_ident(c).into_owned()}`; left/
  right non-USING → `{name: f.name.clone(), sql: format!("{alias_q}.{col_q}")}`.
  Same ordering and `is_empty` → `None` guard as today.
- `build_table_function` range arm (1994):
  `vec![DefaultSlot { name: "id".into(), sql: "id".into() }]`.

### 3. emission.rs — build_join_side keeps the block on the wrap path (F2)

Restructure 269-315: never rebuild via `SelectBlock::from_item(item)`.
- Peek eligibility on `&block` first: `block.pure_from()` AND a match on the
  FromItem kind. Add `pub(crate) fn from_ref(&self) -> &FromItem` to
  SelectBlock (doc: read-only peek for inline-eligibility checks).
  `Relation|Derived` → inline; `Join{..}` → existing
  `may_inline_nested_join && !parent_has_using && matches!(side.op, plain
  ON join)` guard verbatim; `Raw` → never.
- If eligible: `block.into_pure_from()` — this now cannot fail (pure_from
  checked); use `.unwrap_or_else` panic-free: match and treat Err as the
  wrap path (defensive, no `.expect`).
- Else: `FromItem::Derived { unit: Box::new(SqlUnit::Select(Box::new(block))), alias }`
  — the block, WITH its defaults, renders inside the synthetic wrap.
  (This also makes a range/Raw-FROM side render `SELECT id FROM range(…)`
  instead of `SELECT * FROM …` — equivalent output, expected in test
  re-baselines.)

### 4. emission.rs — build_drop_columns filters named slots (F1)

In the `block_with_projections` slots-closure (it already receives
`(&SelectBlock, wrapped)`): when `!wrapped` and `block.default_slots()` is
`Some(slots)`, compute `remaining: Vec<&str>` = slot.sql for slots whose
`name` matches NO drop name (`eq_ignore_ascii_case`); if `remaining` is
non-empty, `Ok(remaining.join(", "))`; else (degenerate all-dropped) fall
through to `* EXCLUDE`. When `wrapped` or no defaults: today's
`* EXCLUDE ({dropped})` string exactly (a wrapped child renders its own
defaults inside `__td_sub`, so `*` order there is already correct).

### 5. emission.rs — build_project substitutes the star slot (F4)

On the MERGE path only (wrapped path unchanged — defaults are inside the
wrap): if `block.default_slots()` is `Some(slots)` and any projection is
`Star(StarExpression { qualifier: None })`, build the slot string
per-expression instead of one `render_projection_slots` call: bare star →
`slots.map(.sql).join(", ")`; every other expression →
`render_projection_slot(p, &input.resolved_schema)?`. Keep
`is_unqualified_star_only` identity branch FIRST, unchanged.

### 6. emission.rs — build_lateral_view extends the defaults (F3)

On the merge path (pure_from && vis), after `extend_from(…)`:
`block.extend_default_projections(columns.iter().map(|(alias, _)| DefaultSlot {
name: alias.clone(), sql: format!("{ta_q}.{a_q}", …) }).collect())` where
`ta_q = quote_ident(table_alias)`, `a_q = quote_ident(alias)` — matching
the resolved_schema's appended-columns order. (No-op when defaults None.)

## Invariants

- ADR-022 single path; no fallback rendering.
- All slot construction from typed data (StructField names, quote_ident);
  never parse/split existing SQL strings.
- Analyzer untouched; resolved_schema stays the order authority.
- EMIT_TAP/INV suite green; no new clippy warnings on touched files.
- No `.unwrap()`/`.expect()` on the new paths (see change 3).

## Tests (emission.rs `mod tests` unless noted; use existing helpers
`scan`/`aliased_scan`/`qcol`/`base_types_emp_dept`, `tap_guard()`, and
triage EMIT_TAP_MUTEX cascades with `-- --exact`)

New pins:
1. `drop_over_using_join_renders_hoisted_slots` — DropColumns("budget")
   over `emp JOIN dept USING(dept_id)`-shaped CommonAst: SQL contains the
   explicit hoisted list starting `dept_id` and NOT `* EXCLUDE`; dropped
   name absent.
2. `drop_above_occupied_block_keeps_exclude` — DropColumns over
   Limit(occupied) input: SQL contains `* EXCLUDE` over `__td_sub`.
3. `multi_slot_star_over_using_join_expands_hoisted_slots` —
   Project([Star(None), Alias(1 AS one)]) over USING join: hoisted list
   then `1 AS one`; no `*,`.
4. `using_join_side_wrap_preserves_hoisted_slots` — ON join whose RIGHT is
   a USING join (build via CommonOp::Join nesting): the `AS __td_jr`
   derived body contains the hoisted list, not `SELECT *`.
5. `lateral_view_over_range_appends_generated_column` — LateralView over
   TableFunction range: SQL `SELECT id, t.c FROM range(…) AS __td_range(id), LATERAL …`
   (assert both `id` and the generated alias in the slot list).
6. `lateral_view_over_table_scan_still_renders_star` — LateralView over
   plain scan keeps `SELECT *` (protects cx-007..009 shape).
Re-baseline if needed (shape should be unchanged, verify):
`render_project_star_over_using_join_emits_hoisted_slot_list`,
`dispatch_project_over_range_binds_id_column`,
`bare_range_dispatch_keeps_id_default_projection`; sql_block.rs unit tests
touching default_projections (adjust constructor types only).

## Acceptance

- `cargo check -p thunderduck-core`; scoped `rustfmt --check`
  (`git diff --name-only HEAD -- '*.rs' | xargs -r rustfmt --check --edition 2021`);
  `cargo test -p thunderduck-core --lib` fully green.
- (Orchestrator, after review) `tests/scripts/witness-progress.sh`:
  REGRESSIONS 0; flips MUST include join-018 (F1), join-020 (F2),
  cx-015+cx-016 (F3), join-019+jn-023 (F4). join-021/join-022 stay red
  (later cycles).
