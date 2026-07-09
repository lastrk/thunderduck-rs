# SELECT-block emission branch — max-effort review findings (2026-07-09)

Scope: all 10 commits on `feat/select-block-emission` (base `feat/v2-transpiler`
@ 5b2edb3) plus the then-uncommitted item-3 work (wrap-boundary qualifier
rewriting, committed immediately after this review). Process: 10 independent
finder angles (5 correctness, reuse/simplification/efficiency/altitude/
conventions), 1-vote adversarial verification per deduped candidate (7 of the
verdicts reproduced end-to-end against a live server + PySpark Connect
client), and a final gap sweep (returned empty). 15 findings survived,
ranked most-severe first.

Severity legend: **corruption** = silently wrong data on the wire;
**regression** = green before this branch, red after; **divergence** =
τ succeeds/errs where Spark does the opposite; **infra** = gate integrity.

## Confirmed findings

### 1. `drop()` over a USING join returns silently mislabeled data — corruption (CONFIRMED, empirical)
`emission.rs:1413` — `build_drop_columns` merges `* EXCLUDE (…)` onto the
USING-join block, and `set_projections` shadows the hoisted
`default_projections` that alone enforce Spark's key-first order
(`sql_block.rs` `to_sql`: `projections.or(default_projections)`). DuckDB's
`*` keeps the key at its natural left position; the positional Arrow
re-stamp then mislabels every column left of the key.
Repro: `emp.join(dept, on='dept_id').drop('budget')` → live server returned
`Row(dept_id=<id value>, id=<name value>, …)` with no error.

### 2. Nested USING-join side loses its hoisted slots inside the synthetic wrap — corruption + regression (CONFIRMED, empirical)
`emission.rs:311` — `build_join_side`'s non-inlinable path rebuilds the side
via `SelectBlock::from_item(item)` after `into_pure_from` has dropped
`default_projections` ("dropped with the block shell"), rendering
`(SELECT * FROM a JOIN b USING (k)) AS __td_jr` in DuckDB natural order.
Repro: `t1.join(t2, id==id2).join(a.join(b, on='k'), id==k)` → live server
returned `Row(k=10, x=1, …)` — k/x data swapped vs Spark. The pre-branch
renderer always baked explicit hoisted slots into every wrap.

### 3. Lateral view over range()/join silently drops generated columns — corruption (CONFIRMED, empirical)
`sql_block.rs:242` — `extend_from` widens FROM + scope but leaves
`default_projections` stale; `pure_from()` ignores defaults so the merge
proceeds; `to_sql` prefers the stale defaults over `*`.
Repro: `SELECT * FROM range(3) LATERAL VIEW explode(array(1,2)) t AS c` →
schema declares `struct<id,c>`, wire batch has 1 column (c missing).
Corpus lateral cases all sit over TableScan (no defaults) — untested gap.

### 4. Multi-slot star over a USING join mislabels columns — corruption, pre-existing (CONFIRMED, empirical)
`emission.rs:731` — `select('*', extra)` bypasses the lone-star identity
branch; the bare `Star` contributes no qualifier so the merge proceeds and
the raw `*` shadows the hoisted list.
Repro: `l(a,k).join(r, on='k').select('*', lit(1).alias('one'))` → live
`Row(k=10, a=1, …)` vs Spark `Row(k=1, a=10, …)`. The 5b2edb3 renderer had
the identical flaw (lone-star-only delegate) — live defect, not a regression.

### 5. USING-join scope exemption strands buried user aliases on the merge path — regression (CONFIRMED, empirical)
`emission.rs:606` — a USING join's `RelScope` is deliberately empty, so
`exprs_visible_in`'s `scope_binds` filter exempts a user alias that
`build_join_side` (parent_has_using) just buried under `AS __td_jl`; the
merge proceeds with an unbindable qualifier.
Repro: `df1.alias('e').join(df2.alias('d'), e.x==d.y).join(df3, on='k')
.select('e.name')` → `Referenced table "e" not found! Candidate tables:
"__td_jl"`. The pre-branch flatten path (`emit_flat_chain`) kept `e`
visible — genuine green→red.

### 6. Runner build-failure guard is inert — infra (CONFIRMED)
`tests/scripts/run-differential-tests.sh:222` — `if ! cargo build … 2>&1 |
tail -20` tests **tail's** exit status (`set -e`, no `pipefail`). A failed
rebuild with a stale binary present silently gates against the old build —
the exact false-green class the 6bd9f07 fix claimed to close.

### 7. Duplicate `__td_jr` when an inlined stamped child join collides with a parent wrap (CONFIRMED, empirical)
`emission.rs:352` — the duplicate-alias guard re-wraps the colliding right
side under the **same** `TD_JOIN_RIGHT` name, so a collision against an
inlined child join that itself exposes `__td_jr` is never fixed. DuckDB
tolerates unreferenced duplicates; a qualified `__td_jr` reference (e.g. an
ancestor ambiguous plan_id ref merged into the block) fails:
`Ambiguous reference to table "__td_jr" (duplicate alias)` vs Spark success.

### 8. Strip legitimizes bogus qualifiers — divergence, in item 3 (CONFIRMED)
`emission.rs:647` — `strip_stranded_qualifiers` never checks the qualifier
actually bound the referenced column, so a qualifier kept only by the
analyzer's tier-(f) name-only fallback gets stripped instead of failing.
`df.alias('e').select(col('dept_id').alias('k')).filter(col('e.k')==101)`:
Spark raises UNRESOLVED_COLUMN (a select-created alias carries no
qualifier); τ now silently returns rows. Root cause is the over-permissive
tier-(f) fallback (`qualified_column_info` ignores the relation qualifier);
the strip converts its loud failure into silent divergence.

### 9. Aggregate/lateral wrap fallbacks don't strip — strand class survives (CONFIRMED)
`emission.rs:512` — `build_aggregate` renders slots/GROUP BY/HAVING before
the wrap decision, unstripped (`build_lateral_view` likewise).
`emp.alias('e').orderBy('id').limit(5).groupBy(col('e.dept_id')).count()` →
`Referenced table "e" not found` while Spark succeeds — same class as the
filt-016 witness item 3 fixes.

### 10. Strand class also leaks through the MERGE path (CONFIRMED)
`emission.rs:753` — a scope-unbound qualifier is vis-exempt, so a second
filter merges WHERE-onto-WHERE onto an already-wrapped block; no wrap ⇒ no
strip. `emp.alias('e').select('e.dept_id','e.name').distinct()
.filter(e.dept_id==101).filter(e.name=='x')` → second conjunct emits
`e.name` over `__td_sub` → binder error while Spark succeeds.

### 11. Duplicate user alias on both join sides now silently binds LEFT — divergence window (PLAUSIBLE)
`emission.rs:359` — the new dup-alias guard re-wraps the right side, so
`x.salary` binds the left where the old code failed loudly (`AS x … AS x` →
DuckDB duplicate-alias error). When the name exists on BOTH sides the
qualified tier-(f) path has no ambiguity check (the `AmbiguousColumn` guard
runs only for unqualified refs) → τ returns left-side data where Spark
raises AMBIGUOUS_REFERENCE. (Name-unique case matches Spark — the guard is
a fix there.)

### 12. Stranded `q.*` emits an opaque engine error across the wire — error category (CONFIRMED, empirical)
`analyzer.rs:4004` — qualified-star now resolves through passthrough
scopes, but stars are never rewritten at wrap boundaries, so
`df.alias('e').orderBy('id').limit(2).select('e.*')` sends gRPC INTERNAL
with the raw `Binder Error: Referenced table "e" not found` — the deleted
gate returned a clean UnknownColumn and its doc predicted exactly this
(ADR-016/ADR-022). Both old and new fail the Spark-valid query; the new
failure mode is the forbidden opaque one.

### 13. `mark_node` debug_assert reachable from untrusted input (CONFIRMED)
`analyzer.rs:926` — `resolve_column` treats any user qualifier literally
named `__td_jl`/`__td_jr` as synthetic and keeps it; `own_expr_demands`
raises a pending demand that reaches the re-scoping arm's
`debug_assert!(!pending_jl && !pending_jr)` → session-thread panic in any
debug-assertions build (`df.select('x').filter(col('__td_jl.x') > 0)`).
Release builds silently drop the demand (loud binder error).

### 14. Third hand-rolled qualifier walker — reuse (CONFIRMED)
`emission.rs:652` — `strip_stranded_qualifiers::walk` re-encodes the
qualifier-leaf variant list + subquery-opacity convention already encoded
in `expr_qualifiers` and the analyzer's `synthetic_uses`. Divergence
between the collector and the rewriter reintroduces the strand class; one
shared visitor removes the drift surface.

### 15. `SelectBlock.scope` double-bookkeeps `FromItem::exposed()` — simplification (CONFIRMED)
`sql_block.rs:203` — the merge-visibility authority (`exposes()`) is a
stored copy maintained by hand in `from_item`/`extend_from`; a future
mutation of `from` without mirroring `scope` silently mis-merges. Deriving
`exposes()` from the `FromItem` removes the invariant.

## Verified clean (for the record)

SetOp cast barriers/keyword table/nesting; DISTINCT ON row-choice
(merge rules provably order-equivalent to the old wraps); Sort bare-key
merge over an occupied select list (empirically binds the select alias,
matching Spark); WHERE conjunct parenthesization; Values/LocalRelation/
FileScan/range leaf quoting and rename contracts; EMIT_TAP/INV2 accounting;
cross-crate surface (no `TypedAst{}` literal bypasses, no stale
`derived_*_schema` consumers, `analyze()` sole flag producer,
`build_file_reader_sql` shape unchanged); no SQL-string post-processing,
no `Display`-built SQL, no undocumented new public items.

Refuted along the way: `FROM emp AS emp` qualified-star rejection
(TableScan{alias:Some} unreachable from both front-ends); quadratic
child re-emission (every parent builds its child exactly once).

## Cross-cutting note: corpus blind spot

None of findings 1–5, 7 are covered by the corpora — no case combines a
USING join with `drop`/multi-slot star/nested-join-side/buried-alias
shapes, and no lateral case sits over a defaults-carrying input. That is
why the branch's zero-regression gate stayed green through all of them.
Witness cases for the USING/default_projections cluster should precede any
fix (ADR-001 evidence discipline), mirroring the filt-016/017 pattern.

## Deferred cleanup candidates (not in the ranked 15)

- `max_clause` derivable from slot occupancy (sql_block.rs).
- `block_with_projections` closure-bool signature vs `build_project`'s
  hand-rolled twin; shared open-block-or-wrap prelude helper.
- `build_join_side` 5 params (CLAUDE.md 4-param rule; `JoinParts` precedent)
  — same for `build_aggregate`/`build_set_op` (6, carried over).
- Strip clone-always + sort-key double clone; `exprs_visible_in` dedup;
  `RelScope::of` passthrough clones (consider `Rc`)/join-scope reuse;
  `SqlUnit::into_sql(self)`; fuse `mark_join_alias_requirements` walks.
- `trace_stranded_qualifiers` predicate drift vs strip (emit the trace from
  inside the strip decision); `scope_binds` as a `RelScope` method; shared
  exactly-one-match helper (4 copies of Spark's unambiguity rule).
- `hoisted_join_slots` extraction to retire the `expect("checked above")`.
