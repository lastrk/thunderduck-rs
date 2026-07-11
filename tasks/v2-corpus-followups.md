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
| F-groupfold-nested | tpcds-q085 | P27 | red-case | `grouping_already_folded` false-NEGATIVE when the GROUP BY key appears only NESTED in a SELECT expr (e.g. `substr(key,1,20)`) → grouping col wrongly prepended → column-count 5 vs 4. Fix: detect a grouping key referenced inside a projection expr, not just bare. Touches the Pass-10 heuristic core. |
| F-explode-map | test_explode_map | P26 | red-case | unaliased `explode(map)` must emit two default-named cols `key`,`value`; τ's multi-col map-explode expansion (emission.rs:3850, type_inference.rs:829) fires only on an explicit alias. Needs a schema-aware Project pre-pass generator expansion (mirror `expand_json_tuple/stack_projections`, dispatch Array=1col vs Map=2cols). |

## Open — latent / witness-free correctness & parity gaps

| id | source | class | defect / fix sketch |
|----|--------|-------|---------------------|
| F-orderby-computed-groupkey | P27 | latent (hard-error) | ORDER BY fallback over a NON-folded DataFrame Aggregate with a COMPUTED grouping key (`groupBy(a+b).agg(sum).orderBy(a+b)`) appends a hidden output instead of binding the grouping-prefix column → column-count desync (downstream hard error, not wrong data). Fix: extend the whole-key/subtree match to the grouping-prefix output columns (schema `0..offset`) before promoting. |
| F-count-distinct-name | P21 (M1) | latent | `pretty_name` ignores `FunctionCall.distinct`: unaliased `count(DISTINCT x)` → name `count(x)` vs Spark `count(DISTINCT x)`. Render DISTINCT in `pretty_name`. |
| F-countstar-name | P21 (M2) | latent | DataFrame `F.count("*")` → name `count(*)` vs Spark `count(1)`. SQL path normalizes via `sparksql_default_select_name` (v2_lowering.rs:2424); the DataFrame path has no equivalent. |
| F-upper-fn-name | P21 (M3) | latent | SQL uppercase function calls (`SUM(x)`) keep the verbatim name → output `SUM(x)` vs Spark lowercase `sum(x)`. Lowercase the function name in `lower_function` (v2_lowering.rs:3413) or at naming. |
| F-nondistinct-multicount | P20 | latent | non-DISTINCT multi-arg `count(a,b)` (Spark ACCEPTS it — counts rows where all args non-null) still emits invalid DuckDB `count(a,b)`. Only the DISTINCT multi-arg path was fixed. |
| F-json-keys-nonobject | P18 | latent | `json_object_keys`→`json_keys` returns `[]` on non-object/non-null JSON where Spark returns NULL (corpus exercises object inputs only). |
| F-negative-emit | P19 | latent | `negative`/`negate` has a type-inference arm but NO emission arm → would emit invalid DuckDB `negative(x)`. |
| F-unary-math-nullable | P25 | latent | sqrt/cbrt/sin/cos and the rest of Spark's `UnaryMathExpression` family have the same always-nullable override (empirically) but aren't in `function_call_nullable`'s always-null arm. |
| F-decimal-div-dup-logic | P32 | hygiene | the decimal-widening operand logic (decimal_parts + 3-arm decimalize match) is now duplicated verbatim in `binary_data_type` (expression.rs) and `render_binary`'s Div arm (emission.rs) and MUST stay in lockstep (a one-sided edit silently desyncs analyzer type vs emission). Extract a shared `pub(crate)` helper. |
| F-todf-dupname | P31 | latent | `toDF`/`withColumnsRenamed` over a child with DUPLICATE column names collapses via a last-wins by-name rename map in `analyze_to_df` (analyzer.rs:2184) → e.g. two `x` cols + `toDF(a,b)` maps both to `b`. Pre-existing (emission positional-rename fix didn't introduce it); resolved_schema stays correct. Fix: carry a POSITIONAL name list in analyze_to_df, not a by-name map. |
| F-sourcequals-restamp | P27 | latent (inert) | `analyze_sort` re-stamps unconditionally after alias-pinning; `source_quals_of` re-derives from bare ColumnReference only, so an alias-pinned passthrough grouping col loses inherited `source_quals`. Inert for current emission (schema/ordinal-driven), differs from committed inc-1 behavior. |

## Open — hygiene

| id | source | class | note |
|----|--------|-------|------|
| F-dead-macros | P17 | hygiene | session.rs macros now fully shadowed by emission rewrites: `size`, `array_except`, `array_distinct`, `array_union`, `_spark_reverse`, `_spark_size`. Safe to delete. |

## Open — deferred architecture

| id | source | class | note |
|----|--------|-------|------|
| F-agg-folded-flag | P10/P13 | arch | The `grouping_already_folded` any-match heuristic + the SQL-vs-DataFrame folding ambiguity have a robust fix: thread an explicit `folded: bool` from each front-end through `CommonOp::Aggregate` (ast.rs:107 TODO). Would also subsume F-groupfold-nested. |
| F-decimal-sum-route | P13 | arch | `sum(Decimal)` left native (DuckDB sum-decimal is coherent enough); strict ADR-020 fidelity would route to the shipped `spark_sum`. No red witness. |
| F-orderby-ordinal | P22/P27 | arch | ORDER BY ordinal parity (`ORDER BY <int>`, `ORDER_BY_POS_OUT_OF_RANGE`) — increment 3 of the ORDER BY design. Error-parity only; no current red witness. |

## Cleared (fixed by a later pass)
- `substr(...)` output name rendered `substring(...)` (F-substr-name, P27) → FIXED P28 (preserve sqlparser `shorthand` name; emission rename normalizes DuckDB). q062/q079/q099 green.
- array_except NULL-element parity (P17 note) → FIXED P23 (null-safe `list_position`).
- array_distinct hash-reorder (P17 note) → FIXED P23 (order-preserving `list_filter`).
