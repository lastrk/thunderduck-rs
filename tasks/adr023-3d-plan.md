# ADR-023 chunk 3d — resolver consults source_quals lineage (the F8/F10 flip)

Flips **filt-018** (F10, red→SUCCEED) and **filt-019** (F8, red→ERROR
`UNRESOLVED_COLUMN.WITH_SUGGESTION`), while keeping USING joins, correlated
subqueries (tbl-005/sq-*), and all 10 strand witnesses green, 0 regressions.
May also flip **sq-023** (SQL, same F8 token) as a bonus. Files:
`crates/core/src/transpiler_v2/analyzer.rs` (resolver + tracked flag), and the
`spark_class` for the error token (also analyzer.rs, ~line 1017). Emission needs
NO change (bare-name binds over `__td_sub`; correlation keeps its qualifier).

## The witnesses (exact shapes)
- filt-018 (F10, SUCCEED): `emp.alias("e").select("e.dept_id","e.name").distinct()
  .filter(e.dept_id==101).filter(e.name=="x")`. Both cols are projected-through
  (source_quals `{e}`). 2nd filter merges WHERE-onto-WHERE onto an already-wrapped
  block → today `e.name` strands over `__td_sub`. Fix: resolve `e.name` to
  qualifier=None (projected-through) → emission renders bare `name` → binds.
- filt-019 (F8, ERROR): `emp.alias("e").select(col("dept_id").alias("k"))
  .filter(e.k==101)`, `expected_error="UNRESOLVED_COLUMN.WITH_SUGGESTION"`. Col
  `k` is created (source_quals ∅). Today τ's permissive tier-(f) name-only
  fallback resolves `e.k` and silently returns rows. Fix: authoritative-∅ ⇒
  UnknownColumn.

## The safety mechanism: `source_quals_tracked` (authoritative vs deferred ∅)
3c left USING/Star/SetOp/WithColumns/etc. lineage as a size-correct EMPTY
fallback. So ∅ is ambiguous: "created column (authoritative, reject a bad
qualifier)" vs "lineage we punted on (must keep legacy behavior)". Add a
per-node bool so the resolver only acts where lineage is KNOWN:

### 1. Add to `RelScope` (~line 138), next to `source_quals`
```rust
/// ADR-023 3d: `true` iff `source_quals` is AUTHORITATIVE for every output
/// column of this node — an empty set then means "created, inherits no
/// qualifier" (reject a stranded qualifier). `false` for operators whose
/// lineage 3c deferred (USING joins, Star projections, SetOp, WithColumns/
/// Renamed/Drop, LateralView, and the terminal sources) — the resolver then
/// keeps the legacy name-only fallback for them. Derived; EXCLUDED from
/// PartialEq (extend the hand-written impl — do NOT add to the compared fields).
pub source_quals_tracked: bool,
```
Default `false` (via `#[derive(Default)]` — bool defaults false; the four
`RelScope { .. }` literals in `of` + the ResolveContext composite literal need
`source_quals_tracked: false` added to compile — they're overwritten in new()).

### 2. Compute it where `source_quals_of` runs (TypedAst::new / a sibling fn)
Return `(Vec<BTreeSet<String>>, bool)` from `source_quals_of`, or add a parallel
`source_quals_tracked_of`. Per-arm `tracked`:
- `TableScan`, `AliasedRelation` → `true`.
- scope_passthrough! (Filter/Sort/Limit/Sample/…) → `input.scope.source_quals_tracked`.
- `Project` → `input.scope.source_quals_tracked && projections-contain-no-Star`
  (the Star-fallback arm is deferred). If the length-guard fallback fires → `false`.
- `Join` non-USING → `left.tracked` for LeftSemi/LeftAnti, else `left.tracked &&
  right.tracked`. USING join → `false`.
- `Aggregate` → `input.scope.source_quals_tracked` when the length guard passed
  (grouping++aggregate aligned); the fallback-to-empty branch → `false`.
- Every deferred arm (SetOp, LateralView, WithColumns, WithColumnsRenamed,
  DropColumns, and ALL terminal sources Values/LocalRelation/SingleRow/FileScan/
  TableFunction/Unnest/Describe/Summary/FreqItems/Unpivot/Pivot/RecursiveCte) →
  `false`.
Populate in `TypedAst::new` right after `scope.source_quals = …`.

## 3. Resolver change — tier (f), the `None =>` arm inside the qualified,
## non-synthetic branch (analyzer.rs ~line 3881, the
## `match ctx.scoped_range(q) { … None => qualified_column_info(...) }`)
Replace the `None =>` arm with:
```rust
None => {
    if ctx.scopes.source_quals_tracked {
        // ADR-023 3d: authoritative lineage for this node. `q` binds no
        // local scope, so consult per-output-column source_quals.
        let hits: Vec<usize> = ctx.schema.fields.iter().enumerate()
            .filter(|(i, f)| f.name.eq_ignore_ascii_case(&u.name)
                && ctx.scopes.source_quals.get(*i)
                    .is_some_and(|s| s.iter().any(|qq| qq.eq_ignore_ascii_case(q))))
            .map(|(i, _)| i)
            .collect();
        match hits.len() {
            1 => {
                // Projected-through (F10): resolve by ORDINAL, DROP the
                // qualifier so emission renders the bare column (binds
                // positionally over any wrapper — no strand).
                let k = hits[0];
                let f = &ctx.schema.fields[k];
                return Ok(Expression::ColumnReference(ColumnReference {
                    name: u.name,
                    qualifier: None,
                    ordinal: Some(k),
                    data_type: Some(f.data_type.clone()),
                    nullable: Some(f.nullable),
                }));
            }
            n if n >= 2 => {
                return Err(AnalyzerError::AmbiguousColumn {
                    name: u.name,
                    candidates: hits.iter().map(|&i| ctx.schema.fields[i].name.clone()).collect(),
                });
            }
            // 0 hits under authoritative lineage: NOT projected-through.
            // Degrade to Unresolved so the shared tier-(g) tail tries the
            // OUTER scope (correlation, tbl-005/sq-*) and otherwise raises
            // UnknownColumn (F8) — NO permissive name-only fallback here.
            _ => (DataType::Unresolved, false),
        }
    } else {
        // Deferred lineage (USING / Star / SetOp / …): keep the legacy
        // name-only fallback so those cases stay green (retired in 3e as
        // their lineage is filled in).
        TypeInferenceEngine::qualified_column_info(&u.name, Some(q), ctx.schema)
    }
}
```
Notes:
- The existing tier-(g) tail (`if matches!(dt, Unresolved) { if outer … correlated
  keep-qualified; else UnknownColumn }`) is REUSED unchanged for the 0-hit
  authoritative case — this is what preserves tbl-005/sq-* (outer wins) and
  produces F8's UnknownColumn (no outer).
- Do NOT touch the struct-qualifier tier (d), the `scoped_range Some` tier (e),
  the plan_id path, the synthetic-join path, or the unqualified path.
- `u.name` is moved into the returned struct on the projected-through path —
  clone where needed to satisfy the borrow checker (mirror the existing arms).

## 4. Error token — `spark_class` (analyzer.rs ~line 1017-1021)
Change `Self::UnknownColumn { .. } => Some("UNRESOLVED_COLUMN")` to
`Some("UNRESOLVED_COLUMN.WITH_SUGGESTION")` and update the doc comment at ~1014.
Justification: the harness compares the FULL dotted token EXACTLY
(`dataframe_diff.py:187/193`), Spark emits `.WITH_SUGGESTION` whenever candidate
columns exist (always, for these shapes), and NO green corpus case expects plain
`UNRESOLVED_COLUMN` (only filt-019 + sq-023, both red, expect the sub-class). If
the witness gate shows a regression from this, revisit (make it conditional on a
non-empty schema), but corpus inventory says it is safe.

## Do NOT
- Do NOT change emission (bare-name over `__td_sub` already binds; correlation
  keeps its qualifier via the outer path).
- Do NOT fill USING/Star lineage this chunk (tracked=false keeps them on the
  legacy path — that is 3e's job as it retires the fallback).
- Do NOT remove `strip_stranded_qualifiers` / the wrap rewrites (3e).

## Tests
Analyzer `#[cfg(test)]`:
- projected-through: over `AliasedRelation(emp,"e") → Project[e.dept_id,e.name]`,
  resolving `e.name` at a Filter yields `ColumnReference{qualifier:None,
  ordinal:Some(1)}` (F10).
- created-alias reject: over `… → Project[col(dept_id).alias(k)]`, resolving
  `e.k` yields `Err(UnknownColumn)` (F8).
- correlation preserved: a resolve where `q` is absent from source_quals but
  present in the OUTER scope keeps `qualifier: Some(q)` (tbl-005 shape).
- untracked passthrough: over a USING join (tracked=false), a qualified ref
  still resolves via the legacy path (no F8 error).
- `spark_class`: `UnknownColumn` → `"UNRESOLVED_COLUMN.WITH_SUGGESTION"`.
Reuse existing fixtures (`emp_dept_aliased_join`, `qcol`, `alias_expr`,
`base_types_with_emp_dept`, the 3c source_quals tests). Add a `#[cfg(test)]`
that also asserts `source_quals_tracked` is true for Project-of-columns and
false for a USING join.

## Verify (coder — NO commit)
`cargo check -p thunderduck-core`; `cargo test -p thunderduck-core --lib` (green;
EMIT_TAP_MUTEX cascade → isolate a real failure with `--test-threads=1`);
`cargo fmt`. Revert on failure (`git checkout -- crates/core/src/transpiler_v2/
analyzer.rs`). Never touch `.claude/`, never `git reset --hard`, never commit.

## Acceptance (orchestrator runs the gate — NOT the coder)
`export SPARK_HOME=/workspace/.spark/spark-4.1.1 THUNDERDUCK_VENV_DIR=/workspace/
.venv; ./tests/scripts/witness-progress.sh`: **filt-018 + filt-019 flip
red→PASSED**, the 12 currently-green witnesses stay green, **REGRESSIONS 0**
(watch especially USING joins, correlated sq-*/tbl-005). sq-023 flipping too is a
welcome bonus. tpcds-q095 can hang the run near 96% under load — if so, kill and
read partial logs; q095 is not in the baseline.
