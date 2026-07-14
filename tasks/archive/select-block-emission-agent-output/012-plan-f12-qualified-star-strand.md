# Plan 012 — F12 (qualified-star wrap-boundary strand) — EXECUTABLE

**Tree:** `feat/v2-transpiler` in `/workspace`. Edit
`crates/core/src/transpiler_v2/emission.rs` only. Witness: `proj-016`
(DataFrame corpus, already added), born red:
gRPC `INTERNAL` / `Binder Error: Referenced table "e" not found! Candidate
tables: "__td_sub"`. Empirically Spark 4.1.1 SUCCEEDS (all 14 columns, 2 rows).

## Bug
`emp.alias('e').orderBy('id').limit(2).select('e.*')`: the `select` cannot
merge below the occupied LIMIT, so `build_project` wraps under `__td_sub`.
The projection is `Star { qualifier: Some("e") }`. `strip_stranded_qualifiers`
explicitly does NOT rewrite stars (a `q.*` has no bare-name equivalent for a
reshaped output, per its doc), and `render_star` emits `"e".*` verbatim —
which strands over `__td_sub` (an opaque DuckDB binder error over gRPC, the
forbidden error mode per ADR-016/ADR-022).

## Fix (build_project wrap path only)
On the wrap path (the `else` branch after the merge check fails, emission.rs
~915-925), before `strip_stranded_qualifiers`, rewrite a stranded
WHOLE-RELATION qualified star to the bare `*`:

For each projection `p`: if `p` is `Expression::Star(StarExpression {
qualifier: Some(q) })` AND the pre-wrap `block` does NOT expose `q`
(`!block.exposes(q)`) AND `q` binds EXACTLY ONE alias entry in `input.scope`
covering the FULL input range (`0..input.resolved_schema.len()`), replace it
with `Expression::Star(StarExpression { qualifier: None })`. Otherwise pass
`p` to `strip_stranded_qualifiers` as today.

Rationale (doc-comment at the site): after the wrap, the block's output IS
exactly the input relation's columns, so a star that covered the whole input
(`e.*`) is semantically the bare `*` over `__td_sub` — it binds positionally
and matches Spark. This is the star analog of the F9/filt strand strip.

**Range check** — `RelScope::lookup` is private to the analyzer; use the pub
`input.scope.aliases` directly (as `covering_alias`/`scope_binds` already do):
`q` covers the whole input iff exactly one entry `(name, range)` has
`name.eq_ignore_ascii_case(q)` and `range == 0..input.resolved_schema.len()`.
Exactly-one matters: a qualifier binding 2+ ranges is ambiguous — leave it
(do not expand).

**Scope (documented residual, do NOT fix):** a PARTIAL-range stranded
qualified star (one side of a join, `range != full`) is left verbatim —
expanding it to bare column names could collide with the other side's
duplicate names under the wrap, and joins normally keep their aliases exposed
so they do not strand here. Un-witnessed, rarer. Note it in the doc-comment.

Do NOT touch the merge path (a `q.*` that merges over a block still exposing
`q` renders `q.*` correctly today) or `strip_stranded_qualifiers` /
`render_star`.

## Tests (emission.rs `#[cfg(test)] mod tests`)
1. `qualified_star_over_limit_wrap_expands_to_bare_star` — mirror proj-016:
   `AliasedRelation(emp,"e")` → `Sort(id, limit=2)` → `Project([Star{Some("e")}])`.
   Dispatch; assert the output is `SELECT * FROM (...) AS __td_sub ...` and
   `!sql.contains("\"e\".*")` / no stranded `e.*`. First confirm (with a
   temporary print) that `analyze` keeps the projection as a `Star{Some("e")}`
   into build_project (the finding confirms emission sees `e.*`); if the
   analyzer pre-expands it, adjust the test to still pin the dispatched SQL.
   Document the derivation in `.agent-output/012-implementation-f12.md`.
2. `qualified_star_that_merges_keeps_alias` (regression) — `emp.alias('e')
   .select('e.*')` with NO limit (merges, block still exposes `e`) → assert
   the star still renders correctly (bare `*` or `e.*`, whichever the merge
   path produces today — pin CURRENT behavior so the fix doesn't perturb it).

Keep all existing star tests green (`render_star_and_qualified_star`, etc.).

## Verification (coder — NO corpora, NO commits)
- `cargo check -p thunderduck-core`
- `cargo test -p thunderduck-core --lib` ALL green (977 + new)
- `rustfmt --check` clean on emission.rs
- `cargo clippy -p thunderduck-core --lib` — no new warnings on touched lines
- Log to `.agent-output/012-implementation-f12.md`

## Acceptance (orchestrator)
`witness-progress.sh`: 0 regressions, and proj-016 flips red→PASSED (Spark
parity — all columns returned). Prior 9 witnesses stay green.
