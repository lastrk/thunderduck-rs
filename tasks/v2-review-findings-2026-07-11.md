# τ code-review findings — 2026-07-11

**Branch:** `feat/v2-transpiler` · two reviews over the same recent commit range.

- **Review 1 — correctness / parity** (range `HEAD~25..HEAD`, Passes 16–27):
  10 finder angles → verification against the DuckDB binary
  (`/workspace/.duckdb-delta/build/release/duckdb`) + Spark 4.1.1 docs → sweep.
  Findings that hinge on DuckDB/Spark semantics were checked by executing the
  *actual emitted SQL* on the binary (the Rust unit tests only assert the
  generated SQL string, not its runtime behavior).
- **Review 2 — "missing-local-info" complexity** (range `HEAD~26..HEAD`; the
  extra 26th commit is a chore, so the code diff is identical): loci where an AST
  node lacks a local field, forcing the code to rederive / guess / look it up,
  and where a stamped field would collapse the branching.

> **Line numbers are for committed HEAD.** A concurrent session was editing these
> Rust files live during Review 2 (see the ⚑ note below), so the working tree
> may have drifted off these anchors.

---

## ⚑ Recommended fix #1 (highest ROI) — thread an Aggregate output-layout flag

**This single fix resolves Review 1 findings 2 (E1) + 3 (E2) AND is Review 2's
headline (R2-1) — the same root.**

**Problem.** `CommonOp::Aggregate` / `TypedOp::Aggregate` carry no
`folded`/output-layout flag, so the front-end-constant fact *"is the output
`aggregates` (SQL-folded) or `grouping ++ aggregates` (DataFrame)?"* is
**re-derived at 6 sites** by the fragile any-match `grouping_already_folded`
heuristic:

| # | Site (committed HEAD) | Branch it drives |
|---|---|---|
| 1 | `analyzer.rs:577` (`source_quals_of`) | lineage prefix |
| 2 | `analyzer.rs:667` (`source_quals_tracked_of`) | `computed_len` |
| 3 | `analyzer.rs:1593` (schema construction) | prepend grouping? |
| 4 | `analyzer.rs:3745` (`rebind_over_aggregate`) | `offset = if already_folded {0} else {grouping.len()}` → **E2** |
| 5 | `analyzer.rs:4274` (`unfold_ungrouped_aggregate_subquery`) | strip prepend |
| 6 | `emission.rs:966` (`build_aggregate`) | `keys = if already_folded {&[]} else {&grouping_r}` → **E1** |

The heuristic is **non-monotonic under the ORDER-BY promotion mutation** (a
promoted grouping expr appended to `aggregates` flips the any-match
`false→true`), which is exactly why E1 (binder error) and E2 (spurious
`UnknownColumn`) occur. The `grouping_already_folded` doc itself says: *"the
robust fix is to thread an explicit `folded: bool` flag from each front-end
through `CommonOp::Aggregate` … instead of inferring it structurally here."*

**Fix.**
1. Add a per-front-end **constant** layout flag to `CommonOp::Aggregate` and
   `TypedOp::Aggregate` (e.g. `enum AggregateProjection { Folded, Grouped }`):
   SQL front-end (`v2_lowering`) sets `Folded`; every DataFrame construction
   site (`v2_relation_converter::convert_aggregate` / `convert_cov` /
   `convert_corr` / `convert_approx_quantile`, `crosstab_to_aggregate`) sets
   `Grouped`.
2. Thread it through `build_unit` → `build_aggregate` (emission currently drops
   it on destructure).
3. Replace all 6 `grouping_already_folded(...)` calls with a `match` on the flag
   and **delete `grouping_already_folded`** (~12-line body + ~52-line doc).
4. With the layout now an immutable constant, `bind_aggregate_slot`'s `offset`
   is fixed regardless of promotion, so it merges with `bind_project_slot`
   (Review 2 finding R2-cleanup).

**Effect.** E1 and E2 disappear at the root (the two `xfail(strict)` tests
`test_order_by_grouping_expression_over_multikey_aggregate` and the E2 repro
flip green); ~64 lines + 6 lockstep-maintained mirror sites collapse to a field
read.

> **⚑ Coordinate first.** During this review a **concurrent Claude session in
> this shared worktree** was already implementing exactly this — an
> uncommitted `AggregateProjection{Folded,Grouped}` enum whose doc says it
> "replaced the `grouping_already_folded` heuristic," with `rebind_over_aggregate`
> and the schema/source_quals arms already migrated to read it (emission + the
> `unfold` subquery arm were still on the heuristic at last look). Check `git
> status` / the other session before starting so you don't collide.

---

# Review 1 — correctness / parity

## Covered by new differential tests (added earlier this change)

Both added as `@pytest.mark.xfail(strict=True)` — green-as-xfail until fixed,
flip to a loud XPASS when the fix lands.

1. **max_by/min_by drop the NULL value at the extreme-ordering row** — CONFIRMED.
   `emission.rs` (`"max_by" => ("arg_max", …)`). `arg_max`/`arg_min` skip rows
   whose *value* arg is NULL; Spark returns the value at the max/min-ordering
   row even when NULL. Fix: `arg_max_null`/`arg_min_null`.
   Test: `test_new_aggregates_differential.py::…::test_max_by_min_by_null_value_at_extreme`.

2. **ORDER BY a grouping expression over an unfolded aggregate → binder error** — CONFIRMED.
   `analyzer.rs` (`promote_aggregate_subtree` grouping branch) + `emission.rs:966`
   (`grouping_already_folded` → `keys=&[]`). Root: see Recommended fix #1.
   Verified: emits `SELECT sum(b),(a+c) … GROUP BY a+c,d`, trim Project references
   dropped `d` → DuckDB `Binder Error: Referenced column "d" not found`.
   Test: `test_sorting_differential.py::…::test_order_by_grouping_expression_over_multikey_aggregate`.

## Remaining correctness / parity findings (not yet turned into tests)

3. **E2 — earlier promoted grouping expr poisons a later ORDER BY key** — CONFIRMED.
   `analyzer.rs::rebind_over_aggregate` (`~L3745`). After key 1 promotes a grouping
   expr, `already_folded` recomputes true for key 2 → `offset` collapses
   `grouping.len()→0` → `offset+aggregates.len() != schema.len()` guard trips →
   spurious `UnknownColumn`. **Same root as finding 2 → Recommended fix #1.**
   Repro (DataFrame): `groupBy(a+c).agg(max(b)).orderBy(a+c, max(b))`.

4. **to_char(numeric, picture) → raw DuckDB binder error** — CONFIRMED (ADR-022).
   `emission.rs:~3798` (`"to_char" if f.args.len()==2`). The arm assumes a datetime
   format for any 2-arg call; the numeric-format-model form emits `strftime(DECIMAL,…)`.
   Verified: `strftime(78.12,'99.99')` → `Binder Error: No function matches …
   strftime(DECIMAL(4,2), STRING_LITERAL)`. Should be an honest `Unsupported*`
   boundary error (or implement the numeric picture). Repro SQL: `SELECT to_char(78.12,'99.99')`.

5. **json_object_keys on invalid/non-object input** — CONFIRMED (ADR-022).
   `emission.rs:~4009` (`"json_object_keys" => "json_keys"`). Spark returns NULL for
   invalid JSON and NULL for a non-object; DuckDB `json_keys` *raises* `Malformed JSON`
   on invalid input and returns `[]` for a non-object. Verified: `json_keys('not json')`
   → `Invalid Input Error: Malformed JSON`. Latent (fixture holds only valid objects).
   Repro SQL: `SELECT json_object_keys('not json')`, `SELECT json_object_keys('[1,2,3]')`.

6. **Date + DayTimeInterval forced to DATE, dropping time-of-day** — PLAUSIBLE (parity).
   `expression.rs:891` (`binary_data_type` Date±interval → Date for ALL kinds) +
   `emission.rs:~6013` (new `render_binary` DATE cast). Spark promotes
   `Date + DayTimeInterval` to TIMESTAMP (`date + INTERVAL '25' HOUR` → `…01:00:00`);
   τ truncates to date. The guard keys on operand types, not the Spark result type.
   Not a green regression (binary_data_type already mis-typed it), but the CAST cements it.
   Needs a real-Spark reference check before asserting in the corpus.

7. **promote_project_subtree does not uniquify the promoted column name** — PLAUSIBLE.
   `analyzer.rs::promote_project_subtree` (~L3972). Unlike the aggregate path
   (`unique_hidden_output_name`), it names the promoted column via `expression_output_name`
   directly. `SELECT y AS x FROM t ORDER BY t.x` (t has x and y) → projections
   `[y AS x, x]`, schema `[x, x]`. Verified the emitted shape RUNS in DuckDB (picks
   one `x`) → silently-wrong risk, not a crash.

8. **bit_get/getbit: no pos-bounds check** — PLAUSIBLE (Spark-emulated error parity).
   `emission.rs:~4192`. `CAST(((x >> pos) & 1) AS TINYINT)` with no guard; Spark raises
   `INVALID_PARAMETER_VALUE` for `pos<0` or `pos>=bit-width`. τ returns a value / raw
   shift error. Latent (test uses in-range positions). Repro: `bit_get(1L, 64)`.

9. **Trim Project buries the top-level ORDER BY in a derived table** — PLAUSIBLE.
   `analyzer.rs` Sort arm (~L1547). Emits `SELECT <trim> FROM (… ORDER BY … [LIMIT n]) sub`
   with no outer ORDER BY (build_project wraps once OrderBy is set). Row order then
   relies on DuckDB preserving a derived-table's ORDER BY — not SQL-guaranteed.
   Empirically preserved today (q078/q098 green), so it's a fragility, not a current
   break; hard to expose deterministically. Fix: hoist sort keys onto the trim Project
   or re-emit a top-level ORDER BY.

## Efficiency / altitude / cleanup (non-corpus)

10. **array set-ops are O(n²) per row; array_union regressed from O(n·m)** — efficiency.
    `emission.rs` `order_preserving_distinct` / `array_union` (~L4515). `list_concat(a,b)`
    is inlined twice and rebuilt per element, with an O(len) `list_position` per element.
    Correct output; runtime-cost regression on large arrays. Consider a CTE/lateral to
    materialize the list once.

11. **DATE-cast scattered across 4+ sites; no type-driven choke point** — altitude.
    See Review 2 finding **R2-5** (superset). `render_binary`, `add_months`, `date_add`,
    `date_sub`, DATE literal each re-inline `CAST(… AS DATE)`; `trunc(date,fmt)` was
    forgotten and ships a TIMESTAMP-promoting result (`date_trunc`→TIMESTAMP, verified).

12. **Duplicated Aggregate/Project helper pairs** — simplification.
    `analyzer.rs`: `bind_project_slot` == `bind_aggregate_slot` with offset 0;
    `contains_aggregate_call`/`contains_nondeterministic_call` and the
    `promote_*_subtree`/`rebind_over_*` pairs are near-duplicate walks differing only in
    a predicate/offset. (The `offset` split dissolves under Recommended fix #1.)

## Verified NOT bugs (recorded to avoid re-litigation)

- **Array set-op NULL handling is correct.** `list_position([1,NULL,2], NULL)` returns
  `2` on the DuckDB binary; `array_distinct`/`union`/`except`/`intersect` produce
  `[1,NULL]` / `[NULL,2]` / `[1,NULL,2]`, matching Spark. (Refutes a loud finder claim.)
- **dayname/monthname.** Docs say Spark returns 3-letter names while DuckDB returns full
  names, but Pass 16 reports `test_dayname`/`test_monthname` green against the real-Spark
  differential — a data-independent difference cannot pass, so real Spark 4.1.1 matches
  DuckDB here. Refuted by execution evidence.
- **ceil/floor/round/bround/exp/ln/log always-nullable** matches Spark 4.1.1 source.
- **next_day** (all weekdays incl. same-day) and **substring_index** main cases verified
  correct on the DuckDB binary. (substring_index with an *empty-string* delimiter
  diverges — pathological, not tracked.)

---

# Review 2 — "missing-local-info" complexity

Each locus is a fact the analyzer computes but doesn't stamp on the node, so it
is rederived/guessed later — usually by a heuristic or a parallel structure kept
in sync by hand. Ranked by how many branches/conditions collapse if stamped.

**R2-1. Aggregate output-layout flag missing** — CONFIRMED. `analyzer.rs:3745`
(+ 5 more sites). **See Recommended fix #1 above.** Biggest-ROI lever: deletes
`grouping_already_folded` + 6 mirrors, fixes E1+E2.

**R2-2. Sort-key restatement resolution — essential algorithm, ~2.4× overweight** —
CONFIRMED as complexity, **CORRECTED 2026-07-12** on the remedy. `analyzer.rs:1509`
(Sort arm): the **~600-line** ORDER-BY resolver (`analyze_sort` / `analyze_sort_key` /
`rebind_sort_key` / `promote_*_subtree` / `bind_*_slot` / trim-Project wrap / re-stamp).
The original claim — "if each key carried its exprId-based target, the machine collapses
to a slot lookup" — was an **overstatement**: the restatement target *cannot* be stamped
upstream because it does not exist until resolution computes it (the key arrives as
text / an unresolved tree), and Spark itself runs the same walk-match-promote-trim
algorithm (`ResolveReferencesInSort` + `ResolveAggregateFunctions#buildAggExprList` +
the trim-`Project` pattern — τ's own block comment at analyzer.rs:3588 cites all three).
The machine is **essential**; what makes Spark's version ~150 lines instead of ~600 is
three *ambient invariants* τ lacks: (1) Aggregate is always folded (`RelationalGroupedDataset`
folds `grouping ++ aggExprs` at construction — no offset/duality); (2) every SELECT entry
is already a `NamedExpression` (`UnresolvedAlias`→`Alias`, so binding = `entry.toAttribute`,
no mid-flight alias-pinning); (3) ids in immutable nodes (no re-stamp hazard, leaf
`semanticEquals` = id compare). Realistic outcome with all three: **~600 → ~250 lines**
plus de-fragilization — the walk, promote-append, and trim Project remain because they are
the algorithm. See the task file's revised plan for the sequencing. Downstream of R2-1
(done upstream, `55577ef`) + R2-3.

**R2-3. Expressions carry no stable identity (exprId)** — CONFIRMED.
`expression.rs:222` (`ColumnReference`). The ~160-line semantic-equality cluster
(`semantic_eq` + `canonicalize_for_semantic_eq` + `ordinals_compatible` +
`contains_nondeterministic_call` + `NONDETERMINISTIC_FN_NAMES`) rederives "same
logical expression?" structurally, because `ColumnReference::eq` deliberately
excludes `ordinal` (so a second `ordinals_compatible` re-walk re-adds it). Two
fragilities share this root: (a) `ordinals_compatible` degrades to name-only when
a side lacks an ordinal (correlated / struct-qualifier refs) → reopens the
`t1.x`-vs-`t2.x` wrong-column bind; (b) `NONDETERMINISTIC_FN_NAMES` is
non-exhaustive → a UDF/other Nondeterministic expr is treated deterministic and
two such keys may be wrongly deduped. With an exprId, `semantic_eq` is an integer
compare.

**R2-4. Computed nodes carry no resolved (DataType, nullable)** — CONFIRMED.
`expression.rs:628` (`data_type`) / `:730` (`nullable`). Binary/FunctionCall/
Unary/CaseWhen/Window discard their inferred type, so ~218 call sites re-run
`data_type/nullable(schema)` (full subtree re-walks): `render_binary` walks
operands 4×; emission dispatch re-inspects arg types (size/reverse/element_at/
unix_timestamp/ExtractValue); output-schema build re-walks each column twice.
Stamping `(DataType, nullable)` once turns all of it into field reads and makes
emission provably unable to drift from analysis.

**R2-5. "Undo DuckDB's DATE+INTERVAL→TIMESTAMP promotion" re-encoded at ~7
emission sites** — CONFIRMED (subsumes R1-11). `emission.rs:6309`
(`spark_return_cast`) is the existing type-read choke point but handles only 3
ad-hoc cases, so the Date coercion is re-hardcoded per-arm (add_months, date_add,
date_sub, to_date, render_binary, DATE literal, next_day) — and **`trunc` was
forgotten**: typed Date but emits bare `date_trunc(fmt,d)`, and DuckDB
`date_trunc` returns TIMESTAMP (verified) → ships wrong type. Generalizing
`spark_return_cast` to "coerce to `expr.data_type(schema)` when DuckDB's native
type diverges" subsumes all 7 casts + trunc; the *complete* fix needs R2-4 so the
coercion is also correct when the date fn is nested.

**R2-6. Output name not stamped** — CONFIRMED. `analyzer.rs:5777`
(`expression_output_name`). A ~150-line naming family (`pretty_name` /
`pretty_binary_symbol` / `pretty_unary` / `pretty_literal` / `spark_type_sql`)
rederives the Spark `toPrettySQL` name with a growing special-case list:
window/session_window magic strings (**`session_window` is DEAD** — only in this
arm + its test; τ cannot produce that call), a `Cast` arm calling `spark_type_sql`
(a THIRD `DataType→string` renderer, deliberately drifting from
`emission::render_data_type`), and `FunctionCall.name` OVERLOADED to carry both
substrate identity and pretty spelling (substr vs substring) forcing an emission
remap. Stamp the name at conversion → the tree collapses to a field read.

**R2-7. No source qualifier on ColumnReference / no uniqueness flag on
StructType** — CONFIRMED. `analyzer.rs:166` (`source_quals`). A parallel
`source_quals: Vec<BTreeSet<String>>` + `source_quals_tracked: bool` is rederived
bottom-up by `source_quals_of` (~200-line match) + `source_quals_tracked_of`
(a mirror match kept in lockstep), with a hand-maintained invariant
`source_quals.len()==resolved_schema.len()` that forces `analyze_sort` to rebuild
the whole `TypedAst` after any in-place schema growth. Emission separately
rederives name-uniqueness (`output_uniquified` / `bare_dup_ordinal` /
`unique_binding_alias`, ~90 lines, 4 sites). A stamped per-field source qualifier
+ a `StructType` uniqueness flag removes the parallel structure, the mirror, the
invariant, the re-stamp, and the emission reconstruction.

**R2-8. FunctionCall carries no FunctionKind (is_aggregate / is_nondeterministic)**
— CONFIRMED. `analyzer.rs:4061`. `contains_aggregate_call` re-scans `AGG_SPECS`
per node; `promote_aggregate_subtree` scans again per subtree;
`contains_nondeterministic_call` walks against a second hand-synced roster. A
stamped `FunctionKind` turns all three walks into a field read (ties R2-3).

**R2-9. `function_return_type` is first-arg-centric → split type-inference home**
— CONFIRMED. `type_inference.rs:490`. Rules needing other args' types are
rederived in a second resolver (`Expression::function_call_data_type`):
`map_from_arrays` got a fast-path there that only works by shadowing the wrong
hard-coded `Map<String,String,true>` default, and `date_trunc` defaults to
Timestamp because it "can't see arg[1]" (the root of R2-5's trunc bug). Use the
full arg types in the one resolver → the two homes + the map/array_intersect
special-cases merge.

**R2-10. BinaryExpression(Div) doesn't carry which operand was decimalized** —
PLAUSIBLE. `emission.rs:6016`. Emission re-runs the analyzer's widening
(`Expression::decimalize`, made `pub(crate)` purely so emission can reach it) to
rebuild the per-operand `DECIMAL(p,s)`. If the analyzer inserted an explicit
`Cast(operand, Decimal)` node when it decimalizes (Spark's `ImplicitCast` does),
emission would just render the casts and the `(Some,None)/(None,Some)` branch +
the `pub(crate)` leak disappear.

**R2-11. No single source of truth for expression opacity** — CONFIRMED
(simplification). `analyzer.rs:3879` (`opaque_to_subtree_promotion`) hand-
re-enumerates `resolve_and_stamp`'s opacity list (Window/subquery/Lambda/RawSql/
Interval); its own doc admits it "mirrors" that list. Two hand-maintained
enumerations drift as new `Expression` variants are added. Make opacity one method
on `Expression`, read by both.

**Also noted (smaller, same theme):** `build_with_columns_renamed` assumes
τ's tracked (Spark) name == DuckDB's emitted name, which breaks for compound
exprs → positional `__td_wcr` workaround (a distinct *DuckDB-emitted-name* axis);
`array_intersect`'s `_ => containsNull=false` "should not happen" fallback exists
only because element-nullability isn't guaranteed present.
