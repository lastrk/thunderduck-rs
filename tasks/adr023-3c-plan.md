# ADR-023 chunk 3c — per-output-column source-qualifier lineage (ADDITIVE, corpus-neutral)

Implements ADR-023's `source_quals` fact (docs/thunderduck-rearchitect-ADRs.md, lines
641-643, 650). ADDITIVE ONLY: `resolve_column` does NOT consult it (that is 3d), emission
unchanged. Provably corpus-neutral → the correctness bar is analyzer unit tests; the full
witness gate is enforced at 3d. File: `crates/core/src/transpiler_v2/analyzer.rs`.

## 1. The field (RelScope, ~line 138)
Add alongside `aliases`/`plan_ids`/`ambiguous_plan_ids`:
```rust
/// ADR-023 tier 3: per-OUTPUT-column source-qualifier lineage (Spark attribute
/// lineage). `source_quals[i]` = the set of relation qualifiers output column `i`
/// inherits. A passthrough ColumnReference inherits its source column's set; an
/// Alias/computed column inherits ∅. Populated by `source_quals_of` in
/// `TypedAst::new`; derived, so EXCLUDED from PartialEq. Invariant:
/// `source_quals.len() == resolved_schema.len()` for every node.
pub source_quals: Vec<std::collections::BTreeSet<String>>,
```
`RelScope` currently `#[derive(Debug, Clone, Default, PartialEq)]`. Drop `PartialEq` from
the derive and hand-write it comparing ONLY `aliases` + `plan_ids` + `ambiguous_plan_ids`
(mirrors how ColumnReference's hand-written PartialEq excludes `ordinal`). Keep
Debug/Clone/Default derived (BTreeSet + Vec are Default).

## 2. Compile fix in `RelScope::of` (do NOT change its logic)
`of` builds `aliases`/`plan_ids` and MUST stay behavior-identical. It has four explicit
`Self { aliases, plan_ids, ambiguous_plan_ids }` literals (TableScan, AliasedRelation,
Join, LateralView) — add `source_quals: Vec::new()` to each (a comment: "populated by
source_quals_of in TypedAst::new"). The `Self::default()` arms and the `scope_passthrough!
=> input.scope.clone()` arm already compile (Default / clone). Whatever `of` leaves in
`source_quals` is OVERWRITTEN in step 3 — do not try to make `of` compute lineage.

## 3. Populate in `TypedAst::new` (~line 99)
```rust
pub fn new(op: TypedOp, resolved_schema: StructType) -> Self {
    let mut scope = RelScope::of(&op, &resolved_schema);
    scope.source_quals = source_quals_of(&op, &resolved_schema);
    Self { op, resolved_schema, scope }
}
```

## 4. `fn source_quals_of(op: &TypedOp, resolved_schema: &StructType) -> Vec<BTreeSet<String>>`
Free fn (or assoc). Children are already stamped, so read `input.scope.source_quals`,
`left.scope.source_quals`, `right.scope.source_quals`. Return EXACTLY
`resolved_schema.len()` sets. End with `debug_assert_eq!(out.len(), resolved_schema.len())`.

Exhaustive `match op` (no `_`, so a new TypedOp variant is a compile error to classify):

**MUST be exact (pinned by tests — the F8/F10 hinge + core shapes):**
- `TableScan { table, alias }` → every col: `{table} ∪ {alias?}`.
- `AliasedRelation { alias, .. }` → every col: `{alias}` (replaces child's — mirrors how
  `of` re-scopes to `alias`).
- scope_passthrough! class (`Filter`/`Sort`/`Limit`/`Sample`/`SampleBy`/`Deduplicate`/
  `NaFill`/`NaDrop`/`NaReplace`) → `input.scope.source_quals.clone()` (verbatim).
- `Project { input, projections }` — the hinge. If ANY projection is `Expression::Star(_)`,
  fall back to `vec![∅; resolved_schema.len()]` (SAFE — see Star note). Otherwise map each
  projection expr 1:1 to one output col:
  - `Expression::ColumnReference(cr)` with `cr.ordinal == Some(k)` and
    `k < input.scope.source_quals.len()` → `input.scope.source_quals[k].clone()` then
    insert `cr.qualifier` if `Some` (belt-and-suspenders; usually already present).
  - `Expression::Alias(_)` → `∅` (created column). ← filt-019/F8 hinge.
  - any other expr (Binary, FunctionCall, Cast, Literal, ColumnReference with `ordinal:
    None`, …) → `∅` (created/computed). ← filt-018 passthrough is a ColumnReference with
    `ordinal: Some`, so it inherits `{e}` above; F10 hinge.
  - Guard: if the mapped count ≠ `resolved_schema.len()` (defensive), fall back to
    `vec![∅; resolved_schema.len()]`.
- `Join { using_columns, left, right, join_type, .. }`:
  - non-empty `using_columns` → `vec![∅; resolved_schema.len()]` (SAFE fallback; USING
    output reorders/dedups — lineage for USING is filled in 3d/3e before the USING legacy
    resolution path is retired; a TODO comment saying so).
  - else `left.scope.source_quals` ++ (for `LeftSemi`/`LeftAnti`: left only; otherwise
    ++ `right.scope.source_quals`). Length = left_len (+ right_len).
- `Aggregate { input, grouping, aggregates, .. }` → the resolved_schema is grouping cols
  then aggregate cols (match `CommonOp::Aggregate` ordering). For each grouping expr: if it
  is a `ColumnReference{ordinal: Some(k)}` into the input → inherit
  `input.scope.source_quals[k]`, else `∅`. Every aggregate output col → `∅` (created). If
  the alignment is uncertain, fall back to `vec![∅; resolved_schema.len()]`.

**SAFE empty (size-correct, neutral — real derivation deferred, add a one-line TODO each):**
- `SetOp` (ideally first child's `source_quals` positionally — do that if trivial, else ∅),
  `LateralView` (`input.source_quals` ++ `∅`×generated), `WithColumns`/`WithColumnsRenamed`/
  `DropColumns` (ideally positional passthrough — do if trivial, else ∅), and ALL remaining
  variants (`SingleRow`/`Values`/`LocalRelation`/`FileScan`/`TableFunction`/`Unnest`/
  `Describe`/`Summary`/`FreqItems`/`Unpivot`/`Pivot`/`RecursiveCte`) → `vec![∅;
  resolved_schema.len()]`.

**Star note:** precisely expanding `Star`/`q.*` to a column count is deferred; the
size-correct empty fallback keeps the invariant AND stays corpus-neutral (3d reads ∅ →
conservative correlated/unknown path, never a wrong bind). Filt-018/019 (the 3d targets)
do NOT use stars, so this does not block the F8/F10 flip. USING + star lineage are
completed in 3d/3e before the legacy paths they'd replace are retired.

## Do NOT (this chunk)
- Do NOT change `resolve_column` (no consultation yet).
- Do NOT change emission.
- Do NOT change `RelScope::of`'s aliases/plan_ids logic (only add the field to its literals).
- Do NOT flip any witness (corpus-neutral).

## Tests (analyzer `#[cfg(test)]`) — assert `typed.scope.source_quals`
- `AliasedRelation(emp, "e")` → every col `{e}`.
- `select('e.dept_id','e.name')` over it → those cols `{e}` (passthrough via ordinal).
- `select(col('dept_id').alias('k'))` over it → col `k` → `{}`. ← F8/F10 hinge.
- plain join `emp e JOIN dept d` → left cols `{e}`, right cols `{d}`.
- `groupBy('dept_id').count()` → dept_id `{...source}`, count `{}`.
- `source_quals.len() == resolved_schema.len()` on a few shapes (incl. a Star projection
  and a USING join → all `∅`, length correct).
Look at existing analyzer `#[cfg(test)]` builders (e.g. around `rel_scope_*` /
`qualified_star_*` tests) and reuse their fixture helpers — do NOT invent a new harness.

## Verify (coder — NO commit)
`cargo check -p thunderduck-core`; `cargo test -p thunderduck-core --lib` (ALL green —
additive; nothing pre-existing should move; +N new pins); `cargo fmt`. Revert on failure
(`git checkout -- crates/core/src/transpiler_v2/analyzer.rs`). Never touch `.claude/`,
never `git reset --hard`.
