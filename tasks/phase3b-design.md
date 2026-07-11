# Phase 3b design — retire above-join __td_jl/__td_jr ref stamps + the synthetic-qualifier resolution surface

Base: feat/v2-transpiler @ b85590e (3a landed, join-024 green).

## Governing invariant (new, load-bearing)
A ColumnReference{qualifier:None, ordinal:Some(k)} whose name is duplicated (>=2, case-insens) in the emitting operator's input.resolved_schema is produced ONLY by tiers that stamped k against that same schema (plan_id arm; tier-f tracked-lineage drop ~4520-4534). Emission may bind it positionally: input.resolved_schema.fields[k] IS the referenced column. Guard: the ordinal/name-agreement debug_assert at every rewrite site (Phase 2's H8 assert-1 pattern, emission.rs:504).

## 1. Analyzer (analyzer.rs)
### 1.1 plan_id arm (~4289-4320): DELETE the is_ambiguous→side stamp
Return qualifier:None + ordinal ALWAYS (range.start+i over ctx.schema); data_type/nullable unchanged. UnknownColumn miss (4322-4327) and plan_id_is_ambiguous→AmbiguousColumnReference gate (4284-4288) untouched. Delete the is_ambiguous computation + comment 4295-4302; replace with the governing invariant.
### 1.2 DELETE enum JoinSide (~137-148) — now dead (clippy -D warnings forces it)
- RelScope.plan_ids: Vec<(i64, Range<usize>, JoinSide)> → Vec<(i64, Range<usize>)> (~169)
- lookup_plan_id (244) + plan_id_lookup (4088) → Option<Range<usize>>
- RelScope::of Join pushes (349,353) + for_join_condition pushes (4012,4015) drop the tag
- Tests: rel_scope_join_plan_ids_outermost_first drops .2 asserts (6196); DELETE join_side_qualifier_maps_to_synthetic_emission_alias (6203).
### 1.3 for_join_condition: DELETE the __td_jl/__td_jr alias pushes (4032-4033)
Update fn doc (~3999) + RelScope comment 4040-4048 (scope now exists only to offset-merge sides' aliases/plan_ids).
### 1.4 Synthetic resolver arm (4346-4413): collapse to unconditional F13 rejection
```rust
if is_synthetic_join_qualifier {
    // F13: reserved emission-namespace qualifier. The analyzer never binds a
    // scope for __td_jl/__td_jr (Phase 3b); reject unconditionally (Spark
    // parity: UNRESOLVED_COLUMN), never fall through to tier (f)'s
    // permissive name-only fallback.
    return Err(AnalyzerError::UnknownColumn { name: u.name, qualifier: u.qualifier });
}
```
Do NOT fold into tier (e): for_join_condition sets source_quals_tracked:false, so an unbound __td_jl.x would reach tier (f)'s PERMISSIVE untracked fallback and silently resolve — the F13 hazard. Accepted corner D5: a user alias literally spelled __td_jl no longer resolves via qualified ref (reserved namespace; no coverage; documented). Update comments 4248-4258. F13 tests (~12550-12630) stay green by construction.
### 1.5 mark_node / *_requires_synthetic / rung-1: LEAVE FOR PHASE 4 (decided)
Post 1.1+1.4, synthetic_uses can fire only on a Star whose qualifier is a user alias literally spelled __td_jl/jr (bound-homonym passes project_schema 4725). 3b's only touch: update mark_join_alias_requirements doc (1231-1243) to "vestigial post-3b; Phase 4 deletes the pass, the two TypedOp::Join flags, build_join_side rung 1, apply_duplicate_alias_guard's flag params". NO debug_assert probe (never-panic-on-untrusted precedent).

## 2. Emission (emission.rs)
### 2.1 Merge path — ONE fused helper at 4 builders
```rust
/// Merge visibility + ordinal requalification, fused (ADR-023 Phase 3b).
/// Some(rewritten) iff (a) every scope-bound qualifier is exposed by the
/// block's FROM (the exprs_visible_in contract) AND (b) every bare
/// duplicate-name ordinal-carrying ref binds through a UNIQUE covering
/// alias — rewritten to it. None → the caller wraps.
fn requalify_visible<'e>(
    exprs: impl IntoIterator<Item = &'e Expression>,
    block: &SelectBlock,
    input: &TypedAst,
) -> Option<Vec<Expression>>
```
Internals: existing qualifier check verbatim (expr_qualifiers/scope_binds/block.exposes) + clone-map_children walk (subquery bodies excluded, like requalify_expr :427) applying the per-ref rule.
PER-REF RULE for ColumnReference{qualifier:None, ordinal:Some(k), name} with k<input.resolved_schema.len() and name duplicated (>=2 ci) in input.resolved_schema (ALL other refs pass through untouched — ordinal:None correlated/deferred, real qualifiers, unique names):
1. debug_assert name-agreement with fields[k] (H8 assert 1).
2. covering binding = first (a,r) in input.scope.aliases with r.contains(&k). Rewrite qualifier=Some(a) iff ALL:
   (i) a names exactly ONE aliases entry (ci) — homonym-alias hazard H8-2;
   (ii) a appears exactly ONCE in block.from_ref().exposed() (ci);
   (iii) name occurs exactly ONCE within fields[r] — internally-dup span would leftmost-bind wrong.
3. Any ref failing 2 → return None (whole set → wrap path).
Do NOT reuse FromScope::alias_for (single-exposed fast path violates iii; exposure guard can't see i). New method:
```rust
/// Phase 3b merge-path binding for ordinal `i` of a bare duplicate `name`:
/// the covering alias iff it is the unique aliases-entry for its name,
/// uniquely exposed, and `name` is unique within its span. Deliberately NOT
/// alias_for: no single-exposed fast path (an internally-dup span must
/// reject), and analyzer-binding uniqueness is required, not just exposure.
fn unique_binding_alias(&self, i: usize, name: &str, schema: &Schema) -> Option<&str>
```
Builders migrated (4): build_project (1375), build_filter (1397), build_sort (1418-1423, ONLY the select_free() branch — the occupied-select keys_bind branch 1425-1427 untouched), build_aggregate (948-956, one call over grouping ⧺ rewritten_aggregates ⧺ having chained, positional split back by lengths; grouping_already_folded computes over the chosen set as today).
NOT migrated (documented residuals): build_lateral_view (1056), block_with_projections/build_with_columns (2032) keep exprs_visible_in (lateral = SQL path no plan_ids; WithColumns over dup-name join already broken today, same binder-error class before/after). Delete exprs_visible_in doc's synthetic-names sentence (1134-1136).
### 2.2 Wrap path — ordinal arm in reproject_qualifiers (1217-1258)
ColumnReference arm gains else-branch (mutually exclusive with qualifier-keyed rewrite):
```rust
} else if c.qualifier.is_none() {
    if let Some(k) = bare_dup_ordinal(c, schema) {   // shared predicate fn
        c.name = uniquified[k].clone();              // positional bind through the reprojected wrap
    }
}
```
fn bare_dup_ordinal(c,&schema)->Option<usize>: Some(k) iff qualifier None && ordinal Some(k) && k<schema.len() && name duplicated in schema; carries the H8 debug_assert. SHARE this predicate with requalify_visible (single authority). UnresolvedColumn has no ordinal — arm unchanged. Soundness: wrap_reprojected exposes uniquified positionally over the block output whose order == input.resolved_schema order at every wrap site; output_uniquified fires by premise so reproject_or_clone's Some branch is always taken; no change at its 4 call sites.
### 2.3 Condition-path cleanup — requalify_column_ref case (a) dies (472-543)
Delete q_is_jl/q_is_jr/is_synthetic (484-492), H8 assert 2 (511-519), the is_left synthetic split (520-524) → `let is_left = k < left_len;`. Output byte-identical (assert 2 guaranteed agreement). Update docs 376-392, 462-471. SideNeedsAlias/fixpoint/fresh_alias_wrap/apply_duplicate_alias_guard/USING slots untouched. TD_JOIN_* consts REMAIN (wrap-alias names + F13 spelling).

## 3. Behavior deltas (corpus-audited)
D1 above-join dup-name plan_id ref w/ uniquely-bound alias → merge-rewrite, sides inline (WHERE emp2.dept_id), fewer wraps, same data. D2 side w/o analyzer alias (join-022 outer filter): right still wraps AS __td_jr (rung-3 non-pure-FROM), filter can't merge → reprojected __td_sub wrap, WHERE dept_id_3 = 20 — data-identical; contract test re-pinned. D3 unique-name (join-002) unchanged. D4 condition dup-name refs: bare+ordinal → case (b) — emitted SQL BYTE-IDENTICAL (strongest pin: all Phase-2 condition tests must pass unchanged). D5 documented corner. D6 test_joins_differential LocalRelation dup joins → wrap path __td_sub(id, key, val1, id_2, …) — data-identical, tests stay green (data-level asserts). D7 homonym-alias join → merge REJECTED by rule (i) → reprojected wrap, correct side positionally (mandatory unit test).
Corpus: above-join plan_id refs only in join-002 (D3) + join-022 (D2); SQL corpus/TPC no plan_ids; join-023 (deferred red) + join-024 (error case) unaffected — must stay in current states.
H8 hazards pinned: merged-cond-schema vs join-output-schema (different fns/params, structural); homonym aliases (rule i); internally-dup span (rule iii); stale FromScope (build from block.from_ref() after open_block, before mutation); tier-f incidental catches (harmless improvement, occupied ORDER BY untouched); walker convention (map_children, subquery bodies excluded).

## 4. Tests
Analyzer changed: adr023_phase1_dup_name_condition_still_wraps (~6335) → qualifier None, ordinals Some(2)/Some(4), rename …resolves_bare_ordinal; adr023_phase1_self_join_condition_still_wraps (~6357) → None + Some(0)/Some(4); join_flags_propagate_from_ancestor_through_passthrough (~6407) + join_flags_do_not_leak_into_nested_joins (6449-6510) → flags (false,false) at every level + ancestor ref bare+ordinal (vestigial-pass witnesses); retarget the condition-qcol("__td_jl",…) tests (6812, 12269, 12313, 12470, 12483, 12518, 12608) to pcol plan_id refs with qualifier asserts None + exact ordinal; rel_scope_join_plan_ids_outermost_first → 2-tuples; DELETE join_side_qualifier_maps_to_synthetic_emission_alias. Must-stay-green: user_typed_td_jl/jr_* (F13), 3a AmbiguousColumnReference suite, join_flags_set_when_condition_carries_plan_id_ambiguity.
Emission changed: contract_collision_wraps_left_keeps_right_name (9464) → FROM chain unchanged; filter becomes reprojected wrap `… ) AS __td_sub(id, name, dept_id, salary, …, dept_id_3, dept_name) WHERE (dept_id_3) = (20)` (comment: data-identical). ALL Phase-2 condition tests (8939-9360) assert-UNCHANGED.
Emission new: (1) D1 filter merge WHERE (emp2.dept_id), no __td_jl/jr/sub; (2) D1 project merge SELECT emp.id,…; (3) aggregate merge GROUP BY emp.dept_id; (4) sort merge; (5) D7 homonym rejection → wrap binds uniquified right name; (6) internally-dup span rejection (rule iii); (7) reproject_qualifiers ordinal-arm unit (bare dup ordinal → uniquified[k]; bare unique + ordinal-None untouched).
Corpus new (green-by-construction, data-level H8): join-025/join-026 — I["emp"].join(I["emp2"], I["emp"]["dept_id"]==I["emp2"]["dept_id"]).filter(I["emp2"]["dept_id"]==20) and a left-side/select variant; asymmetric per-side values so a wrong-side bind flips rows.
