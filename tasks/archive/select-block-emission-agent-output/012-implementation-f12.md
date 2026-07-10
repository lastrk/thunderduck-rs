# Implementation log — Plan 012 / F12 (qualified-star wrap-boundary strand)

**File touched:** `crates/core/src/transpiler_v2/emission.rs` only, as required. No other
file was edited by this task.

## What changed

Added `expand_stranded_whole_relation_star` (new fn, right before `build_project`) and
wired it into `build_project`'s wrap path (the `else` branch after the merge-visibility
check fails), immediately before the existing `strip_stranded_qualifiers` call:

```rust
let projections: Vec<Expression> = projections
    .iter()
    .map(|p| expand_stranded_whole_relation_star(p, &block, input))
    .map(|p| strip_stranded_qualifiers(&p, &block, &input.resolved_schema))
    .collect();
```

`expand_stranded_whole_relation_star(expr, block, input)`: if `expr` is
`Expression::Star(StarExpression { qualifier: Some(q) })` AND the PRE-wrap `block`
exposes `q` (`block.exposes(q)`) AND `q` binds EXACTLY ONE entry in
`input.scope.aliases` whose range equals the full `0..input.resolved_schema.len()`,
rewrite it to `Expression::Star(StarExpression { qualifier: None })` (bare `*`).
Otherwise return the expression unchanged (falls through to
`strip_stranded_qualifiers` as before, which never touches `Star` at all).

## Deviation from the plan — the exposure-check polarity

The plan's predicate as literally written was `!block.exposes(q)` ("the pre-wrap block
does NOT expose q"). Per the task's own instruction to empirically validate before
committing to the test/fix shape, I added a temporary probe test
(`temp_f12_probe_qualified_star_reaches_build_project`, since removed) that built the
proj-016 shape directly (`AliasedRelation(emp,"e")` → `Sort(order=[e.id], limit=2)` →
`Project([Star{qualifier: Some("e")}])`), ran it through `analyze` + `dispatch_op`, and
printed both the typed AST and the dispatched SQL.

Findings from the probe:
- `analyze` keeps the projection as `Star(StarExpression { qualifier: Some("e") })` all
  the way into `build_project` — confirms the finding's premise, no analyzer-side
  pre-expansion to reason about.
- The pre-wrap `block` in `build_project` (built via `open_block(input)` over the
  `Sort` node) has `FromItem::Relation`-shaped FROM `emp AS e`, so
  `block.exposes("e")` is **`true`** at exactly the point the strand happens — this
  mirrors `strip_stranded_qualifiers`'s own condition for its `ColumnReference`/
  `UnresolvedColumn` arms, where `block.exposes(q) == true` is the trigger for
  stripping (that's precisely the alias the wrap is about to bury behind `__td_sub`,
  per that function's own doc comment: "the pre-wrap `block` actually exposed `q`").
- With the plan's literal `!block.exposes(q)` gate, the probe's dispatched SQL was
  unchanged: `SELECT e.* FROM (SELECT * FROM emp AS e ORDER BY e.id ASC NULLS FIRST
  LIMIT 2) AS __td_sub` — the bug remained unfixed, because the gate never opened for
  the witnessed case.
- Flipping the gate to `block.exposes(q)` (positive, matching
  `strip_stranded_qualifiers`'s convention) produced the correct fix:
  `SELECT * FROM (SELECT * FROM emp AS e ORDER BY e.id ASC NULLS FIRST LIMIT 2) AS
  __td_sub` — no stranded qualifier, `e.*` correctly expanded to bare `*`.

I implemented the corrected (positive) gate and updated the doc comment's rationale to
match (the "documented residual" for partial-range join-side stars is unchanged in
substance, just re-derived off the corrected gate — see the doc comment on
`expand_stranded_whole_relation_star`). All other elements of the plan (range-equality
check via `input.scope.aliases`, "exactly one match" via a two-`.next()` iterator
pattern, never touching the merge path / `strip_stranded_qualifiers` / `render_star`)
are implemented exactly as specified.

The temporary probe test was removed after use, per instructions, and replaced with the
two permanent tests the plan specifies.

## Tests added

1. `qualified_star_over_limit_wrap_expands_to_bare_star` — mirrors proj-016 exactly
   (`AliasedRelation(emp,"e")` → `Sort(order=[e.id asc], limit=2)` →
   `Project([Star{qualifier: Some("e")}])`). Asserts the dispatched SQL equals
   `SELECT * FROM (SELECT * FROM emp AS e ORDER BY e.id ASC NULLS FIRST LIMIT 2) AS
   __td_sub` and does not contain `e.*`.
2. `qualified_star_that_merges_keeps_alias` — regression pin for the merge path:
   `Project([Star{Some("e")}])` directly over `AliasedRelation(emp,"e")` (no
   intervening occupied slot) merges into the still-open block (which still exposes
   `e`), asserting the dispatched SQL equals `SELECT e.* FROM emp AS e` (today's
   correct rendering) with no `__td_sub` wrap — pins that the fix's wrap-only gate does
   not perturb the merge path.

Both derived empirically by running the exact dispatch and reading back the produced
SQL string (`cargo test -p thunderduck-core --lib qualified_star -- --nocapture
--test-threads=1`), then hard-pinning the observed strings.

All pre-existing star tests (`render_star_and_qualified_star`, the analyzer's
`qualified_star_*` suite, the `strip_stranded_qualifiers`/wrap-boundary suite —
`filter_above_limit_strips_stranded_alias_qualifier`,
`sort_above_limit_strips_stranded_alias_qualifier`,
`project_above_limit_strips_stranded_alias_qualifier`,
`with_columns_above_limit_strips_stranded_alias_qualifier`,
`ambiguous_output_name_survives_wrap_qualified`,
`unexposed_qualifier_survives_wrap_verbatim`, etc.) remain green, unperturbed.

## Verification (all green; no corpora run, no commit made)

- `cargo check -p thunderduck-core` — clean.
- `cargo test -p thunderduck-core --lib` — `test result: ok. 979 passed; 0 failed; 5
  ignored; 0 measured; 0 filtered out` (977 baseline + 2 new; no
  `EMIT_TAP_MUTEX`-poison burst observed in a clean full run).
- `rustfmt --check --edition 2021 crates/core/src/transpiler_v2/emission.rs` — initially
  flagged the two new tests' single-line `assert!` calls (rustfmt line-length wrap);
  ran `rustfmt` once on the file to normalize (only whitespace inside the two new test
  bodies changed), then confirmed `--check` clean.
- `cargo clippy -p thunderduck-core --lib -- -D warnings` — 2 pre-existing errors
  remain, both unrelated to this change and to `emission.rs`:
  `too_many_arguments` on `reject_unsupported_view_clauses`
  (`crates/core/src/parser_v2/v2_lowering.rs`) and `map_entry` on
  `crates/core/src/runtime/session.rs`. `cargo clippy -p thunderduck-core --lib`
  (without `-D warnings`) shows zero warnings anywhere in `emission.rs`.

## Scope discipline

Only `crates/core/src/transpiler_v2/emission.rs` was edited. The merge path in
`build_project`, `strip_stranded_qualifiers`, and `render_star` are untouched. No
corpora were run; no commit was made.
