# v2 corpus — deferred follow-ups ledger

Fixable defects/parity gaps DEFERRED during the corpus-to-100 loop
(`tasks/goal-corpus-to-100.md`), with their source pass and a fix sketch. This
is the counterpart to `.agent-output/unsolvable.md`: that file is for ADR-022
**unsolvable / Thunderduck-boundary** cases; THIS file is for things that ARE
fixable but were consciously deferred (out of a pass's one-root-cause scope,
witness-free latent gaps, or bigger structural work). Clear an item when a later
pass fixes it; add its pass number in the STATUS.

Legend — **class**: `red-case` (a currently-failing differential case),
`latent` (correct today for the corpus but wrong for an unexercised shape —
witness-free), `hygiene` (non-functional), `arch` (deferred design decision).

## Open — real RED cases (have a failing witness)

| id | case(s) | source | class | defect / fix sketch |
|----|---------|--------|-------|---------------------|
| F-explode-map | test_explode_map | P26 | red-case | unaliased `explode(map)` must emit two default-named cols `key`,`value`; τ's multi-col map-explode expansion (emission.rs:3850, type_inference.rs:829) fires only on an explicit alias. Needs a schema-aware Project pre-pass generator expansion (mirror `expand_json_tuple/stack_projections`, dispatch Array=1col vs Map=2cols). |

## Open — latent / witness-free correctness & parity gaps

| id | source | class | defect / fix sketch |
|----|--------|-------|---------------------|
| F-count-distinct-name | P21 (M1) | latent | `pretty_name` ignores `FunctionCall.distinct`: unaliased `count(DISTINCT x)` → name `count(x)` vs Spark `count(DISTINCT x)`. Render DISTINCT in `pretty_name`. |
| F-countstar-name | P21 (M2) | latent | DataFrame `F.count("*")` → name `count(*)` vs Spark `count(1)`. SQL path normalizes via `sparksql_default_select_name` (v2_lowering.rs:2424); the DataFrame path has no equivalent. |
| F-upper-fn-name | P21 (M3) | latent | SQL uppercase function calls (`SUM(x)`) keep the verbatim name → output `SUM(x)` vs Spark lowercase `sum(x)`. Lowercase the function name in `lower_function` (v2_lowering.rs:3413) or at naming. |
| F-nondistinct-multicount | P20 | latent | non-DISTINCT multi-arg `count(a,b)` (Spark ACCEPTS it — counts rows where all args non-null) still emits invalid DuckDB `count(a,b)`. Only the DISTINCT multi-arg path was fixed. |
| F-json-keys-nonobject | P18 | latent | `json_object_keys`→`json_keys` returns `[]` on non-object/non-null JSON where Spark returns NULL (corpus exercises object inputs only). |
| F-negative-emit | P19 | latent | `negative`/`negate` has a type-inference arm but NO emission arm → would emit invalid DuckDB `negative(x)`. |
| F-unary-math-nullable | P25 | latent | sqrt/cbrt/sin/cos and the rest of Spark's `UnaryMathExpression` family have the same always-nullable override (empirically) but aren't in `function_call_nullable`'s always-null arm. |

## Open — hygiene

| id | source | class | note |
|----|--------|-------|------|
| F-dead-macros | P17 | hygiene | session.rs macros now fully shadowed by emission rewrites: `size`, `array_except`, `array_distinct`, `array_union`, `_spark_reverse`, `_spark_size`. Safe to delete. |

## Open — deferred architecture

| id | source | class | note |
|----|--------|-------|------|
| F-decimal-sum-route | P13 | arch | `sum(Decimal)` left native (DuckDB sum-decimal is coherent enough); strict ADR-020 fidelity would route to the shipped `spark_sum`. No red witness. |
| F-orderby-ordinal | P22/P27 | arch | ORDER BY ordinal parity (`ORDER BY <int>`, `ORDER_BY_POS_OUT_OF_RANGE`) — increment 3 of the ORDER BY design. Error-parity only; no current red witness. |

## Cleared (fixed by a later pass)
- F-orderby-computed-groupkey (P27/P33) → FIXED by the N7 fold-at-construction pass (2026-07-13 reconciliation): grouped_aggregate folds grouping++agg_exprs at construction, deleting the offset/duality machinery; the witness test_order_by_grouping_expression_over_multikey_aggregate was de-xfailed (unique sort keys) and is green.
- F-decimal-div-dup-logic (P32) → RESOLVED by N4 (2026-07-13 reconciliation): render_binary's duplicated decimal-widening re-derivation was DELETED — coercions are materialized as explicit Cast nodes by materialize_binary_coercions at analysis; no lockstep pair remains.
- F-sourcequals-restamp (P27) → RESOLVED by N9 increment 3 (2026-07-13 reconciliation): the unconditional analyze_sort re-stamp was DELETED (lineage lives on the Attribute and moves by value); the described interaction no longer exists.
- `toDF(a,b)`/SQL `AS t(a,b)` over a child with DUPLICATE column names (F-todf-dupname, P31) → FIXED Pass E2 (2026-07-13, commit-pending): root cause was `build_with_columns_renamed`'s by-name `rename_map` collapsing positional pairs (e.g. `[("id","a"),("id","b")]` → last-wins `id→b`, emitted `__td_wcr(b,b)` vs tracked schema `[a,b]`, an N8 tracked==emitted violation). Both producers of `TypedOp::WithColumnsRenamed` (`analyze_to_df`'s positional zip, and the explicit `WithColumnsRenamed` analyzer arm) already stamp a correctly-renamed `resolved_schema` on the OUTER `TypedAst`; emission now mirrors that tracked schema POSITIONALLY instead of computing any rename_map — `build_with_columns_renamed` takes the dispatch-level (outer, already-renamed) `schema` and re-emits `SELECT * FROM (<child_sql>) AS __td_wcr(<schema field names, in order>)`. Witness added: DataFrame corpus case self-joining `emp` with itself (both sides projecting `id`), `.toDF("a","b")`, then `.select("a")`.
- `grouping_already_folded` heuristic (F-groupfold-nested + F-agg-folded-flag, P10/P27) → RETIRED P33: replaced by an explicit `AggregateProjection {Folded,Grouped}` flag on the Aggregate node (SQL=Folded, DataFrame=Grouped); q085 green (SQL corpus 100%); Pass-29 unfold_ungrouped_aggregate_subquery deleted as dead; also closed the Pass-10 `.groupBy(k1,k2).agg(k1,…)` DataFrame edge.
- `substr(...)` output name rendered `substring(...)` (F-substr-name, P27) → FIXED P28 (preserve sqlparser `shorthand` name; emission rename normalizes DuckDB). q062/q079/q099 green.
- array_except NULL-element parity (P17 note) → FIXED P23 (null-safe `list_position`).
- array_distinct hash-reorder (P17 note) → FIXED P23 (order-preserving `list_filter`).
