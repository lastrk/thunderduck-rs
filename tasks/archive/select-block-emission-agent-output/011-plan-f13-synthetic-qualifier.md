# Plan 011 — F13 (mark_node debug_assert reachable from untrusted input) — EXECUTABLE

**Tree:** `feat/v2-transpiler` in `/workspace`. Edit
`crates/core/src/transpiler_v2/analyzer.rs` only. Unit-test pinned (the report
says so — this is a debug-build panic / release binder-error, no clean corpus
shape). Independent of the F8/F10/F11/F12 lineage cluster.

## Bug
A user-typed reserved qualifier `col('__td_jl.x')` / `col('__td_jr.x')`:
- `resolve_column` (analyzer.rs ~3803) sets `is_synthetic_join_qualifier` by
  pure string match, then resolves `(dt, nullable)` via
  `ctx.scoped_range(q).and_then(column_info_in)`; on the `None` arm it FALLS
  BACK to `ctx.schema.field_by_name(&u.name)` and SUCCEEDS (keeps the
  qualifier). So the ref resolves instead of erroring.
- `mark_join_alias_requirements` → `mark_node` then sees the `__td_jl`
  qualifier via `synthetic_uses`/`own_expr_demands`, raises `pending_jl`, and
  the re-scoping arm's `debug_assert!(!pending_jl && !pending_jr)`
  (analyzer.rs:926 Project/Aggregate/... and :930 SetOp) PANICS on the session
  thread in debug builds (silently drops → DuckDB binder error in release).

**Spark 4.1.1 (empirically confirmed):** both raise
`UNRESOLVED_COLUMN.WITH_SUGGESTION` (`cannot be resolved. Did you mean [id]`).
So τ's target is a clean `AnalyzerError::UnknownColumn` (Spark-emulated cat-1,
ADR-022) — analysis fails BEFORE `mark_node` runs.

## Why the fix is targeted (do not disturb legitimate paths)
- Legitimate stamped `__td_jl`/`__td_jr` only appear in a JOIN CONDITION
  (set by `qualify_plan_id_refs`), resolved under a `for_join_condition`
  context whose `scopes` ALWAYS contains `__td_jl` (0..left_len) and
  `__td_jr` (left_len..). There, `scoped_range(q)` is `Some` → unaffected.
- Post-join ambiguous plan_id refs are produced by the SEPARATE
  `u.qualifier.is_none()` plan_id branch (~3748) as already-resolved
  `ColumnReference`s — they never re-enter `resolve_column` as `__td_jl`-
  qualified `UnresolvedColumn`s. Untouched.
- Therefore an `UnresolvedColumn` arriving WITH a `__td_jl`/`__td_jr`
  qualifier and NO `scoped_range` is necessarily user-typed → reject.

## Fix (analyzer.rs)

### Part 1 — resolve_column: unscoped synthetic qualifier → UnknownColumn
In the `is_synthetic_join_qualifier` arm (~3803-3819), restructure the
`(dt, nullable)` computation to mirror tier (e): distinguish the two misses.
Replace the `match ctx.scoped_range(q).and_then(...) { Some => .., None =>
field_by_name-fallback }` with:
```rust
match ctx.scoped_range(q) {
    Some(range) => match TypeInferenceEngine::column_info_in(&u.name, &ctx.schema.fields[range]) {
        Some(info) => info,
        // `q` is a real synthetic side scope but `name` is not on it.
        None => return Err(AnalyzerError::UnknownColumn { name: u.name, qualifier: u.qualifier }),
    },
    // `q` is `__td_jl`/`__td_jr` by SPELLING but binds no synthetic side
    // scope — a user-typed reserved qualifier (the analyzer only stamps these
    // inside a join condition, where `for_join_condition` always scopes them).
    // Spark raises UNRESOLVED_COLUMN; match it rather than silently resolving
    // by name-only and later tripping the mark_node invariant (F13).
    None => return Err(AnalyzerError::UnknownColumn { name: u.name, qualifier: u.qualifier }),
}
```
Keep the surrounding doc comment; note the removed `field_by_name` fallback
was the over-permissive path this finding is about. (This arm returns early
with the resolved tuple today; the two new arms early-RETURN an Err instead —
confirm the function's control flow still type-checks: the `let (dt,nullable)
= if ... {..} else {..};` binding must still receive a tuple on the success
path. Simplest: make the whole `is_synthetic_join_qualifier` case compute the
tuple where it can and `return Err` on the misses — matching how tier (e)
already does `return Err` inline.)

### Part 2 — mark_node: soften the two debug_asserts to defensive non-panic
At analyzer.rs:926 and :930, replace `debug_assert!(!pending_jl &&
!pending_jr)` with a plain continuation (drop the pending demand — matching
release behavior) plus a doc comment: a session thread must never panic on
untrusted input; Part 1 rejects the column-ref path at resolution, but a
user MAY still route a reserved qualifier through a path Part 1 does not
catch (e.g. a qualified star `__td_jl.*` — F12 territory), so this stays a
tolerant drop, not an assert. For the Project/Aggregate/... arm keep
`mark_node(input, own_jl, own_jr)` (already ignores pending). For the SetOp
arm keep `mark_node(child, false, false)`. i.e. just delete the two
`debug_assert!` lines and add the explanatory comment.

## Tests (analyzer.rs `#[cfg(test)] mod tests`)
1. `user_typed_td_jl_qualifier_is_unknown_column_not_panic` — build
   `Filter(condition: col("__td_jl.id") > 0)` over `Project(["id"])` over
   `scan("emp")` (mirror the Spark probe). `analyze(...)` returns
   `Err(AnalyzerError::UnknownColumn { qualifier: Some("__td_jl"), name: "id" })`.
   Assert it does NOT panic (a returned `Err` inherently proves no panic).
2. `user_typed_td_jr_qualifier_is_unknown_column` — same with `__td_jr`.
3. (regression) confirm a legitimate DataFrame plan_id join-condition case
   still analyzes+dispatches — reuse/point at an existing green join test
   (e.g. `render_join_from_dataframe_plan_id_contract_keeps_td_jl_no_flatten`
   in emission.rs is the emission side; ensure no analyzer join-condition
   test regresses). Do not add a redundant one if coverage exists.

Use the existing analyzer test helpers (`scan`, `qcol`, `base_types_*`,
`analyze`); grep the analyzer test module for the exact
`AnalyzerError::UnknownColumn` assertion pattern already in use.

## Verification (coder — NO corpora, NO commits)
- `cargo check -p thunderduck-core`
- `cargo test -p thunderduck-core --lib` ALL green (975 + new)
- `rustfmt --check` clean on analyzer.rs
- `cargo clippy -p thunderduck-core --lib` — no new warnings on touched lines
- Log to `.agent-output/011-implementation-f13.md`

## Acceptance (orchestrator)
Unit tests green (the panic is gone; clean UnknownColumn). Then the full
`witness-progress.sh`: 0 regressions, 9/9 prior witnesses still green (F13 is
unit-pinned, adds no corpus witness).
