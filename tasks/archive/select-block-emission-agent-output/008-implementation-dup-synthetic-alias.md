# Implementation log — Plan 008 / F7: never inline a join side that exposes synthetic aliases

Cycle 3 of `tasks/goal-implement-review-findings.md` (witness `join-022`).
Scope honored: `crates/core/src/transpiler_v2/emission.rs` ONLY (+ inline
tests). `analyzer.rs` untouched (read-only, used only to confirm the
`mark_join_alias_requirements` / `qualify_plan_id_refs` / `join_flags_*` test
pattern before writing the new pins).

## Changes

### 1. `exposes_synthetic_alias` helper + `inline_ok` conjunct (`build_join_side`)

Added, exactly per plan:

```rust
/// Whether a lowered FROM item exposes one of the synthetic join-side
/// aliases (`__td_jl` / `__td_jr`, exact match — they are emission-
/// generated). Such an item must never inline into an enclosing FROM
/// scope: the enclosing join may need the same name for its own side
/// (the duplicate-alias guard cannot rename, because ancestor references
/// are stamped with these literal qualifiers), so the child keeps its
/// derived wrap and its synthetic names stay confined to the sub-scope.
fn exposes_synthetic_alias(item: &FromItem) -> bool {
    item.exposed()
        .iter()
        .any(|a| a == TD_JOIN_LEFT || a == TD_JOIN_RIGHT)
}
```

Added the conjunct `&& !exposes_synthetic_alias(item)` to the
`item @ FromItem::Join { .. }` arm of `inline_ok` in `build_join_side`,
alongside the existing nested-join guard conditions (`may_inline_nested_join`,
the USING-parent `scope_covers_fields` check, and the plain-ON/CROSS-only
`matches!`). The Relation/Derived/Raw arms are unchanged.

Extended `build_join_side`'s ladder doc comment (item 2) to describe the new
refusal and why a synthetic-exposing child must stay wrapped even under a
non-USING parent (previously `!parent_has_using` short-circuited the ONLY
exposure check that existed — `scope_covers_fields` — so a plain-ON parent
never checked exposure at all; that was the gap `join-022` hit).

### 2. Doc note on the duplicate-alias guard (`build_join`)

Extended the `collides` block's comment in `build_join` to record that, with
change 1 in place, a collision can no longer involve `__td_jr`/`__td_jl`
leaking out of an inlined child — so the guard's same-name re-wrap is sound
for the remaining case (USER alias collisions, or a user alias colliding with
this join's own synthetic wrap). Noted the one remaining pathological case
(a USER alias literally named `__td_jl`/`__td_jr`) is out of scope here,
tracked with finding 13's untrusted-`__td_*` class.

## Tests added (`crates/core/src/transpiler_v2/emission.rs::tests`)

New section `── Plan 008 F7: never inline a join side that exposes synthetic
aliases ──`, inserted right after `using_parent_with_synthetic_scoped_side_stays_wrapped`
(the cycle-2 USING-parent exposure pin) and before
`render_project_over_join_hoists_user_aliases`. Also added a `pcol` test
helper (mirrors `analyzer::tests::pcol`) next to the existing `qcol` helper.

### 1. `synthetic_exposing_child_join_never_inlines` (the join-022 witness shape)

Built as CommonAst: an INNER join of `scan("emp")`/`scan("emp2")` whose
condition carries plan_id-tagged `dept_id` refs (`pcol("dept_id", 101)` /
`pcol("dept_id", 102)`, `left_plan_ids: [101]`, `right_plan_ids: [102]`) —
this is the exact `plan_id_join`/`join_flags_set_when_condition_carries_plan_id_ambiguity`
construction mirrored from `analyzer.rs`'s tests, so `qualify_plan_id_refs`
stamps the condition `__td_jl.dept_id = __td_jr.dept_id` and
`mark_join_alias_requirements` sets the INNER join's own
`left_requires_synthetic`/`right_requires_synthetic` to `true` (asserted as
a premise check after `analyze`). This inner join is the LEFT side of an
OUTER plain-ON `Join` whose RIGHT side is `Project { input: scan("dept"),
projections: [dept_name] }` — a non-star column list, so it is never
`pure_from()` and independently wraps as `(...) AS __td_jr` regardless of
this fix.

**Pre-fix SQL captured** (temporarily reverted the `!exposes_synthetic_alias(item)`
conjunct, ran the test, read the panic message):

```
SELECT * FROM (SELECT * FROM emp) AS __td_jl INNER JOIN (SELECT * FROM emp2) AS __td_jr ON (__td_jl.dept_id) = (__td_jr.dept_id) INNER JOIN (SELECT * FROM (SELECT dept_name FROM dept) AS __td_jr) AS __td_jr ON (1) = (1)
```

Structure: the inner join's flat `FromItem::Join` (exposing `["__td_jl",
"__td_jr"]`) inlined straight into the OUTER's left slot; the OUTER's own
right slot independently wrapped as `__td_jr`; `build_join`'s duplicate-alias
guard then saw `"__td_jr"` in both the (now-flattened) left's `exposed()` and
the right's `exposed()`, and re-wrapped the OUTER right a SECOND time under
the SAME name — three `AS __td_jr` occurrences total, two of them the SAME
literal alias nested inside one another (`(...) AS __td_jr) AS __td_jr`).

**Post-fix SQL** (fix restored):

```
SELECT __td_jl.id, __td_jl.name, __td_jl.dept_id, __td_jl.salary, __td_jl.id, __td_jl.dept_id, __td_jl.country, __td_jr.dept_name FROM (SELECT __td_jl.id, __td_jl.name, __td_jl.dept_id, __td_jl.salary, __td_jr.id, __td_jr.dept_id, __td_jr.country FROM (SELECT * FROM emp) AS __td_jl INNER JOIN (SELECT * FROM emp2) AS __td_jr ON (__td_jl.dept_id) = (__td_jr.dept_id)) AS __td_jl INNER JOIN (SELECT dept_name FROM dept) AS __td_jr ON (1) = (1)
```

The inner join now renders as ONE confined derived wrap
`(SELECT ... ) AS __td_jl`; its own `__td_jr` (for `emp2`) stays inside that
sub-scope, and the OUTER's own `__td_jr` (for `dept`) is a sibling at the
outer scope — no collision, no redundant re-wrap.

**Chosen assertions** (fail pre-fix, pass post-fix, not dependent on
incidental formatting beyond the stable binary-condition-parenthesization
and identifier-quoting conventions already relied on throughout this test
module):

1. `assert_eq!(sql.matches(" AS __td_jr").count(), 2, ...)` — pre-fix = 3
   (the redundant re-wrap), post-fix = 2 (the two legitimate, non-colliding
   `__td_jr` wraps in different scopes). Directly encodes the collision
   symptom count rather than an incidental substring.
2. `assert!(sql.contains("AS __td_jl INNER JOIN (SELECT dept_name FROM dept)"), ...)`
   — true only when the OUTER's own left-slot wrap boundary (`AS __td_jl`,
   the alias `build_join_side` assigns the OUTER's left call) sits
   immediately before the OUTER join's own keyword and right side. Pre-fix,
   the inlined child's flat `FromItem::Join` meant the OUTER's own
   `AS __td_jl` wrap never existed at all, so this substring is absent;
   post-fix it is present exactly once, at the OUTER join boundary.

Both were derived empirically: assertion set was picked, the conjunct was
temporarily removed, the test was run to read the actual pre-fix SQL from
the panic message (`--nocapture`, `--exact`), the two assertions were tuned
against that captured string, then the conjunct was restored and the test
re-run to confirm both pass post-fix. Verified both directions:
- pre-fix (conjunct removed): assertion 1 fails (`left: 3, right: 2`).
- post-fix (conjunct restored): both assertions pass.

### 2. `plain_on_child_join_without_synthetics_still_inlines` (over-refusal guard)

No pre-existing pin covered exactly this outer-ON-parent-with-plain-nested-join
shape with a plan_id-free condition and a third table on the outer level (the
closest existing pins — `render_project_over_join_hoists_user_aliases`,
`render_aggregate_over_join_hoists_user_aliases_no_td_agg_or_td_jl` — dispatch
a single 2-table join, not a join-of-a-join), so a new test was added rather
than renaming an existing one. Shape: `emp e JOIN dept d ON e.dept_id =
d.dept_id` (no plan_id stamping, matches
`render_project_over_join_hoists_user_aliases`'s nested-join construction) as
the LEFT side of an outer `INNER JOIN emp2 ON e.id = emp2.id` (also
plan_id-free). Asserts the nested join still inlines flat:
`sql.contains("emp AS e INNER JOIN dept AS d")`, and that no `__td_jl` /
parenthesized wrap (`"(SELECT"`) appears anywhere.

## Verification

| Step | Command | Result |
|---|---|---|
| Compile | `cargo check -p thunderduck-core` | **PASS** — clean |
| Format | `git diff --name-only HEAD -- '*.rs' \| xargs -r rustfmt --check --edition 2021` | **PASS** — no output (clean) |
| Unit tests | `cargo test -p thunderduck-core --lib` | **PASS** — 966 passed, 0 failed, 5 ignored |
| Clippy (scoped) | `cargo clippy -p thunderduck-core --lib --tests -- -D warnings` | Baseline-red (pre-existing, unrelated): 2 `map_entry` + 1 `unnecessary_get_then_check` in `runtime/session.rs`, 2 `collapsible_match` in `transpiler_v2/analyzer.rs` (lines ~10751/10845, pre-existing test code, not touched by this change). **No clippy diagnostics on any line of `emission.rs`** — no new warnings introduced by this change. |

No corpora run, no commits, no release build (per task scope).

## Deviations from the plan

None. `analyzer.rs` was read-only (used only to confirm the
`mark_join_alias_requirements`/`qualify_plan_id_refs`/`join_flags_*` test
pattern before writing the emission.rs pins, per the mandatory-reads
instruction) — never modified.
