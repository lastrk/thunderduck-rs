# F13 implementation — synthetic-qualifier debug_assert panic

All edits confined to `crates/core/src/transpiler_v2/analyzer.rs`, per the
plan at `.agent-output/011-plan-f13-synthetic-qualifier.md`.

## Part 1 — `resolve_column`, `is_synthetic_join_qualifier` arm (~analyzer.rs:3803)

Replaced the `match ctx.scoped_range(q).and_then(|range| column_info_in(...))
{ Some => .., None => field_by_name-fallback }` shape with a two-level match
that mirrors tier (e) exactly:

```rust
match ctx.scoped_range(q) {
    Some(range) => match TypeInferenceEngine::column_info_in(&u.name, &ctx.schema.fields[range]) {
        Some(info) => info,
        None => return Err(AnalyzerError::UnknownColumn { name: u.name.clone(), qualifier: u.qualifier.clone() }),
    },
    None => return Err(AnalyzerError::UnknownColumn { name: u.name.clone(), qualifier: u.qualifier.clone() }),
}
```

Both misses now reject with `AnalyzerError::UnknownColumn` instead of
falling back to `ctx.schema.field_by_name(&u.name)` (name-only, drops the
scope check entirely — the over-permissive path this finding is about).

**Control-flow note (the thing the plan flagged to watch):** the enclosing
statement is `let (dt, nullable) = if is_synthetic_join_qualifier { ... }
else if ... else { ... };` — every other arm of that `if/else if/else`
chain evaluates to a `(DataType, bool)` tuple (or itself early-`return`s an
`Err`, exactly like tier (e) already did at line ~3838). The synthetic arm
now follows the identical shape: the one success path
(`Some(range)` → `Some(info)`) yields the tuple `info` and lets the
`let (dt, nullable) = ...` binding proceed normally into the rest of the
function (tier (g) outer-scope fallback, final `Ok(ColumnReference {..})`
construction); the two miss paths `return Err(..)` directly out of
`resolve_column`, never reaching the `let` binding at all. This is exactly
tier (e)'s existing control-flow pattern (a `match` whose *some* arms
produce a value and whose *other* arms `return Err` early) — no new
control-flow shape was introduced, and the function still type-checks
(`cargo check` confirms).

**Ownership note:** `q = u.qualifier.as_deref().unwrap_or_default()`
borrows `u.qualifier`; the two `Err` constructions therefore build with
`u.name.clone()` / `u.qualifier.clone()` (identical style to tier (e)'s
existing `None` arm at line ~3838), not a move, since `q` is still a live
borrow inside the match scrutinee.

Updated the arm's doc comment to explain the misses now reject rather than
degrade, and why (F13 — Spark itself raises `UNRESOLVED_COLUMN` for
`col("__td_jl.x")`; τ's `UnknownColumn` matches).

## Part 2 — `mark_node`: removed the two `debug_assert!`s

- ~analyzer.rs:926 (`Project | Aggregate | WithColumns | Pivot | Unpivot |
  DropColumns | WithColumnsRenamed | Describe | Summary | FreqItems` arm):
  deleted `debug_assert!(!pending_jl && !pending_jr);`, kept
  `mark_node(input, own_jl, own_jr);` unchanged (already ignores the pending
  demand on the recursive call).
- ~analyzer.rs:930 (`SetOp` arm): deleted the same `debug_assert!`, kept
  `mark_node(child, false, false);` for each child unchanged.

Added a doc comment on the first arm (and a short pointer comment on the
second) explaining: the session thread must never panic on untrusted,
already-parsed user input; Part 1 closes the column-ref path at resolution
time, but a stray `pending_jl`/`pending_jr` demand reaching one of these
re-scoping arms is now tolerated (dropped) rather than asserted, since other
paths to a reserved qualifier (e.g. a qualified star, F12 territory) are not
necessarily closed by Part 1 alone.

## Tests added (analyzer.rs `#[cfg(test)] mod tests`, appended at the end
of the module, right before the closing brace)

Both build `Filter(condition: qcol("__td_jl"/"__td_jr", "id") > 0)` over
`Project(["id"])` over `scan("emp")` — a `Filter`'s condition resolves via
`ResolveContext::of_input` (not `for_join_condition`), so `__td_jl`/`__td_jr`
here is unambiguously the "user typed it, no scope was ever stamped" case:

1. `user_typed_td_jl_qualifier_is_unknown_column_not_panic` — asserts
   `analyze(...)` returns
   `Err(AnalyzerError::UnknownColumn { name: "id", qualifier: Some("__td_jl") })`.
   The returned `Err` (vs. a test-harness panic) is itself the F13 regression
   proof.
2. `user_typed_td_jr_qualifier_is_unknown_column` — same shape with
   `__td_jr`.

No third (regression) test was added — plan explicitly says not to
duplicate coverage; the existing join-condition tests
(`plan_id_disambiguates_self_join_project_above`,
`plan_id_disambiguates_filter_above_join`, `plan_id_three_way_nested_join`,
`plan_id_unique_column_omits_qualifier`, `rel_scope_join_composes_children_
with_right_offset`, etc. — all of which route `qcol("__td_jl"/"__td_jr",
...)` through actual `Join` conditions, i.e. through
`ResolveContext::for_join_condition` where `scoped_range` is always `Some`)
already cover the legitimate stamped path and all still pass unmodified.

## Verification

- `cargo check -p thunderduck-core` — clean.
- `cargo test -p thunderduck-core --lib` — **977 passed; 0 failed; 5
  ignored** (975 baseline + 2 new). No `EMIT_TAP_MUTEX` poison-cascade
  observed on this run. Re-ran scoped to
  `transpiler_v2::analyzer::tests` alone: **197 passed; 0 failed** — every
  join-condition / plan_id-disambiguation test (`plan_id_disambiguates_*`,
  `plan_id_three_way_nested_join`, `plan_id_unique_column_omits_qualifier`,
  `rel_scope_*`, `using_join_qualifier_resolution_stays_on_legacy_path_no_
  panic`) still green, confirming legitimate stamped `__td_jl`/`__td_jr`
  paths and the separate `u.qualifier.is_none()` plan_id branch (line ~3748,
  untouched — never re-enters this arm since it only ever handles
  `qualifier.is_none()` refs) are unaffected.
- `rustfmt --check crates/core/src/transpiler_v2/analyzer.rs` — clean, no
  output.
- `cargo clippy -p thunderduck-core --lib -- -D warnings` — 2 pre-existing
  errors, both outside this file (`too_many_arguments` in
  `parser_v2/v2_lowering.rs`, `map_entry` in `runtime/session.rs`); nothing
  flagged in `analyzer.rs`. Plain `cargo clippy -p thunderduck-core --lib`
  (no `-D warnings`) confirms zero warnings anywhere in `analyzer.rs`.

No corpora run, no commit made, per instructions.

## Deviations from the plan

None. The plan's suggested code shape (skip `.clone()`, use bare `u.name` /
`u.qualifier` moves) was adjusted to `.clone()` on both `Err` arms because
`q` borrows `u.qualifier` for the duration of the `match ctx.scoped_range(q)
{ .. }` scrutinee — an unconditional move of `u.qualifier` inside that match
would conflict with the live borrow. This mirrors the exact pattern tier
(e)'s pre-existing `None` arm already uses (`u.name.clone()` /
`u.qualifier.clone()`) for the identical reason (its `q` borrow is alive
across the whole `else if let Some(q) = u.qualifier.as_deref()` block).
Everything else matches the plan as written.

**Post-review fix:** updated the doc comment at analyzer.rs:3739-3745 (above the `is_synthetic_join_qualifier` computation) to reflect the new behavior — misses are now `UnknownColumn`, not a legacy name-only fallback. Tests still 977 passed / 0 failed, `rustfmt --check` clean.
