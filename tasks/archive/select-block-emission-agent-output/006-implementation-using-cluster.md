# Implementation log — Plan 006 (F1+F2+F3+F4: structured hoisted slot list)

Scope: `crates/core/src/transpiler_v2/sql_block.rs`, `crates/core/src/transpiler_v2/emission.rs` (and their inline `#[cfg(test)] mod tests`) only. `analyzer.rs`, `parser_v2/`, `connect-server/`, corpus files, and scripts were not touched.

## Files changed

- `crates/core/src/transpiler_v2/sql_block.rs` (+63/-8 relative to HEAD before this work started)
- `crates/core/src/transpiler_v2/emission.rs` (+411/-59 relative to HEAD before this work started, including 6 new tests)

## Plan items → what was done

### 1. sql_block.rs — structured slots
- Added `pub(crate) struct DefaultSlot { name: String, sql: String }` (Debug, Clone) with a doc comment, placed right after `WRAP_ALIAS`.
- Changed `SelectBlock.default_projections` from `Option<String>` to `Option<Vec<DefaultSlot>>`; updated its doc comment to note the new named-filter/extend capability.
- `set_default_projections` signature changed to accept `Vec<DefaultSlot>`.
- Added `default_slots(&self) -> Option<&[DefaultSlot]>` (delegates to `self.default_projections.as_deref()`).
- Added `extend_default_projections(&mut self, extra: Vec<DefaultSlot>)` — extends only when `Some`, no-op on `None` (a fresh/wrapped block already renders `*`).
- Added `from_ref(&self) -> &FromItem` — read-only peek used by `build_join_side` to check inline-eligibility before consuming the block. Carries `#[allow(clippy::wrong_self_convention)]` with a one-line justification (name mirrors the `from` field being peeked, not a `From`-trait conversion) since the plan specifies this exact method name and renaming it would be a design change outside scope.
- `into_pure_from`'s doc comment reworded to state defaults are dropped ONLY on a true inline, pointing at `from_ref` for the pre-consumption peek.
- `to_sql()`: builds `let default_list: Option<String> = self.default_projections.as_ref().map(|slots| slots.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>().join(", "))` and uses `.or(default_list.as_deref())` in place of the old `.or(self.default_projections.as_deref())`.
- No existing sql_block.rs unit tests referenced `default_projections`/`set_default_projections` directly (confirmed by grep before editing), so no adjustments were needed there.

### 2. emission.rs — producers build named slots
- `build_join`'s default-slots block now builds `Vec<DefaultSlot>`: USING columns → `{name: c.clone(), sql: quote_ident(c).into_owned()}`; left/right non-USING fields → `{name: f.name.clone(), sql: format!("{alias_q}.{col_q}")}`. Same ordering and `is_empty → None` guard as before.
  - Incidentally removed a pre-existing `.expect("checked above")` on `ra` while rewriting this exact block (replaced with `if let Some(ra) = &ra { ... }`), satisfying the "no new `.expect()`" constraint for code touched in this change.
- `build_table_function`'s `range` arm: `block.set_default_projections(vec![DefaultSlot { name: "id".to_owned(), sql: "id".to_owned() }])`.

### 3. build_join_side keeps the block on the wrap path (F2)
- Restructured to peek eligibility on `&block` first via `block.pure_from() && matches!(block.from_ref(), ...)` (same `Relation|Derived` → inline, `Join{..}` → existing `may_inline_nested_join && !parent_has_using && ...` guard verbatim, `Raw` → never), BEFORE consuming the block.
- If eligible: `block.into_pure_from()`, matched — `Ok(item)` on success; `Err(block)` (cannot actually happen given the peek, documented as a defensive non-panic fallback) falls through to the same wrap construction as the ineligible branch.
- If ineligible: `FromItem::Derived { unit: Box::new(SqlUnit::Select(block)), alias: synthetic_alias.to_owned() }` — wraps the ORIGINAL block (defaults intact) instead of the old `SelectBlock::from_item(item)` rebuild that silently discarded the join's hoisted slot list.

### 4. build_drop_columns filters named slots (F1)
- The `block_with_projections` slots-closure now receives `(block, wrapped)`; when `!wrapped` and `block.default_slots()` is `Some(slots)`, filters slots whose `name` doesn't case-insensitively match any `drop_names` entry; if the remaining list is non-empty, renders it joined; otherwise (degenerate all-dropped) falls through to `* EXCLUDE ({dropped})`. `wrapped` or no-defaults path is unchanged (`* EXCLUDE` verbatim).

### 5. build_project substitutes the star slot (F4)
- Added `render_project_merge_slots(projections, input_schema, default_slots)` — on the merge path, expands a bare unqualified `Star` in a multi-slot projection list to the block's hoisted default slot list (joined `sql`s); every other expression renders via `render_projection_slot` as before. Falls through to `render_projection_slots` verbatim when there are no default slots or no bare star present.
- `build_project`'s merge branch now calls this helper instead of `render_projection_slots` directly. The lone-unqualified-star identity branch at the top of `build_project` (ADR-001 cosmetic collapse) is untouched and still runs first.

### 6. build_lateral_view extends the defaults (F3)
- After the existing `block.extend_from(...)` call, added `block.extend_default_projections(...)` building one `DefaultSlot` per generated column (`name: alias.clone(), sql: format!("{ta_q}.{a_q}")`). Relies on `extend_default_projections`'s own no-op-on-`None` guard: on a block with live defaults (e.g. hoisted USING-join or range `id` bind) this widens the list; on a fresh/plain-scan block (`None`) it's a no-op and `SELECT *` still renders (protects cx-007..009 shape).

## Tests added (all in `emission.rs::mod tests`)

1. `drop_over_using_join_renders_hoisted_slots` — F1 pin. `emp JOIN dept USING(dept_id)` then `DropColumns("dept_name")`. Asserts `sql.starts_with("SELECT dept_id,")`, `!sql.contains("* EXCLUDE")`, `!sql.contains("dept_name")`.
   - **Deviation (naming only, documented per instructions)**: the plan's prose names the dropped column `"budget"`, taken from the review-findings' real-world repro — a column that does not exist in this file's local `emp_schema`/`dept_schema` test fixtures. Using the literal name `"budget"` would make the "dropped name absent" assertion trivially true (it's never present regardless of correctness) and would weaken the pin. Modifying the shared `emp_schema`/`dept_schema` helpers to add a `budget` field was rejected as riskier — ~20+ other existing tests depend on those exact schemas/field positions. Instead the test drops an actually-present dept-side field, `"dept_name"`, which preserves the plan's semantic intent (F1 regression: hoisted list survives a drop over a USING join) while keeping the assertion meaningful. No plan-specified struct, method, or algorithm was altered — this is a test-data substitution only.
2. `drop_above_occupied_block_keeps_exclude` — DropColumns over `Limit(5)` over `emp`. Asserts `sql.contains("* EXCLUDE (salary)") && sql.contains("AS __td_sub")`.
3. `multi_slot_star_over_using_join_expands_hoisted_slots` — F4 pin. `Project([Star(None), Alias(1, "one")])` over the USING join. Asserts hoisted list first, `"1 AS one"` present, no `"*,"`.
4. `using_join_side_wrap_preserves_hoisted_slots` — F2 pin. Outer `Join(aliased emp "e" ⋈ nested Join(dept ⋈ emp2 USING dept_id))` with condition `e.dept_id = 1`, wrapped in `Project(projections: vec![])`. Asserts the exact substring `"SELECT dept_id, dept.dept_name, emp2.id, emp2.country FROM dept INNER JOIN emp2 USING (dept_id)) AS __td_jr"` is present, and the old-bug substring `"(SELECT * FROM dept INNER JOIN emp2 USING (dept_id)) AS __td_jr"` is absent.
5. `lateral_view_over_range_appends_generated_column` — F3 pin. LateralView over `TableFunction range(3)`. Asserts `sql.starts_with("SELECT id, t.c FROM range(")`, contains `"AS __td_range(id)"`, contains `"LATERAL (SELECT"`.
6. `lateral_view_over_table_scan_still_renders_star` — F3 no-op guard. LateralView over a plain typed table scan. Asserts `sql.starts_with("SELECT * FROM")`.

## Re-baselines performed

None of the following required source-level changes — verified unmodified (byte-identical assertions still pass against the new structured-slot implementation), confirming the plan's expectation ("shape should be unchanged, verify"):
- `render_project_star_over_using_join_emits_hoisted_slot_list` — unchanged; hoisted-list SQL shape is identical whether built from a joined `String` or joined from `Vec<DefaultSlot>`.
- `dispatch_project_over_range_binds_id_column` — unchanged; `range`'s single `"id"` default slot renders identically as a 1-element `Vec<DefaultSlot>`.
- `bare_range_dispatch_keeps_id_default_projection` — unchanged, same reasoning.
- sql_block.rs unit tests: none referenced `default_projections`/`set_default_projections` directly, so none needed adjustment.

## Additional fix (clippy, not in original plan text but required by the task's own gate)

- `from_ref` triggered `clippy::wrong_self_convention` (from_* methods conventionally take no `self`). Since the plan specifies this exact method name/signature and renaming would be a design change outside my authority, added a scoped `#[allow(clippy::wrong_self_convention)]` with a one-line comment explaining the name refers to the `from` field being peeked, not a `From`-trait-style conversion.

## Verification (final run)

1. `cargo check -p thunderduck-core` — **PASS** (`Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.45s`).
2. `cargo test -p thunderduck-core --lib` — **PASS**. `test result: ok. 960 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out`. All 6 new tests confirmed passing individually via `-- --exact`, plus the 3 re-baseline tests, in one combined run: `test result: ok. 9 passed; 0 failed; 0 ignored`. No `EMIT_TAP_MUTEX` poison-cascade encountered — the full suite was green on the first run after each edit.
3. `git diff --name-only HEAD -- '*.rs' | xargs -r rustfmt --check --edition 2021` — **PASS** (clean after one `rustfmt --edition 2021 <files>` pass; re-verified with a second `--check` run returning exit 0).
4. `cargo clippy -p thunderduck-core --lib --tests` — **PASS**, zero warnings on touched files (`sql_block.rs`, `emission.rs`). Workspace-baseline warnings remain in unrelated files (`parser_v2/mod.rs` unreachable-pattern x2, `runtime/session.rs` map_entry/unnecessary_get_then_check, `analyzer.rs` collapsible_match x2) — none attributable to lines added in this change, left untouched per scope.

## Deviations from the plan

Two, both minor and non-design:
1. Test 1's dropped-column name (`"dept_name"` instead of the plan prose's `"budget"`) — test-data substitution only, justified above.
2. Added `#[allow(clippy::wrong_self_convention)]` on `from_ref` to satisfy the task's own "no new clippy warnings on touched lines" gate without renaming the plan-specified method.

No struct, method signature, algorithm, or file outside the two in-scope files was changed.
