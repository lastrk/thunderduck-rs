# Phase 2 design — ordinal requalifier + demand-driven join wrap (retire __td_jl/jr forced-wrap for CONDITION refs)

## Goal
Move join-CONDITION requalification from a forced synthetic wrap (build_join_side rung 1) to an emission-time, ordinal-driven rewrite. After Phase 1, only NAME-AMBIGUOUS (count>=2 in merged cond schema) condition refs still carry __td_jl/__td_jr, each with a stamped merged-schema ordinal. Phase 2 rewrites those to each side's REAL alias positionally, so the side emits naturally (inline where possible) and only gets a fresh-alias wrap when a demanded ordinal has no unambiguous covering alias.

## New types/fns (emission.rs)
```rust
struct SideNeedsAlias { left: bool, right: bool }  // never both-false (that path returns Ok)

/// Rewrite each join-CONDITION ColumnReference to the qualifier the emitted
/// sides make true, resolved POSITIONALLY from the ref's ordinal:
///   name unique in cond_schema → bare (qualifier None); ambiguous → side alias via FromScope::alias_for.
/// Leaves untouched: refs with ordinal None (correlated/deferred — tbl-005),
/// and refs with a non-synthetic Some(qualifier) (real user alias).
/// Err(SideNeedsAlias) when a demanded ambiguous ordinal has no unambiguous covering alias.
fn requalify_join_condition(
    cond: &Expression, left: &TypedAst, right: &TypedAst,
    left_item: &FromItem, right_item: &FromItem, cond_schema: &StructType,
) -> Result<Expression, SideNeedsAlias>
```
Returns an OWNED rewritten Expression (clone-then-map_children).

## FromScope::alias_for upgrade (NARROW — single-exposed fast path ONLY)
Prepend to alias_for's body, BEFORE the covering lookup:
```rust
if let [only] = self.exposed.as_slice() {
    return (i < self.width).then_some(only.as_str());
}
```
Rationale: a single-exposed item (a Derived wrap, incl. a synthetic/fresh wrap alias absent from the analyzer's logical scope.aliases) is addressable only by that one alias covering its whole width. `covering`/`covers_all`/`slot_quals` stay BYTE-FOR-BYTE unchanged. This flips ONLY `from_scope_derived_wrapped_side` (alias_for(0): None→Some("__td_jl"); its covers_all()==false and slot_quals()==[__td_jl;n] assertions are UNCHANGED). It does NOT flip using_parent_with_synthetic_scoped_side_stays_wrapped / using_parent_with_uncoverable_side_still_wraps (they use covers_all/slot_quals). Do NOT do the full recursive item-tree rewrite (that WOULD flip the USING tests).

## build_join control flow (rewrite ~emission.rs:419)
1. has_using / semi_or_anti as today.
2. left_item = build_join_side(left, TD_JOIN_LEFT, left_requires_synthetic, true, has_using)?;
   right_item = build_join_side(right, TD_JOIN_RIGHT, right_requires_synthetic, false, has_using)?;
   (*_requires_synthetic is now ANCESTOR-ONLY — see analyzer change. A side wrapped here is for an above-join demand; its alias is single-exposed → alias_for fast-path returns it.)
3. cond_schema = StructType::merge(left.rs, right.rs); left_len = left.resolved_schema.len();
4. Bounded fixpoint (<=2 passes):
   a. DUPLICATE-ALIAS GUARD (unconditional; also covers no-condition CROSS self-join): if left_item & right_item expose a common name (case-insens), wrap the MOVABLE side (the one whose *_requires_synthetic is false) under the first fresh name in __td_jr/__td_jr_2/... (or __td_jl/... for left) the other side does NOT expose; continue.
   b. if condition.is_none() (USING/NATURAL/CROSS) → break (no ON clause; requalifier no-op).
   c. rewritten = requalify_join_condition(cond, left, right, &left_item, &right_item, &cond_schema):
        Ok(expr) → condition := expr; break.
        Err(needs) → wrap each flagged side under a fresh non-colliding alias; continue.
   debug_assert!(pass < 2, "requalifier + collision guard must reach fixpoint in <=2 passes");
5. clause = render_join_clause(join_type, rewritten.as_ref(), using_columns, &cond_schema)?;
6. USING-only default_slots branch UNCHANGED (slot_quals / has_unsafe_qualified_duplicate); non-USING None.
7. assemble FromItem::Join (+ optional slots) unchanged.
Fresh-alias wrap: FromItem::Derived { unit: Box::new(SqlUnit::from(SelectBlock::from_item(side_item))), alias: fresh } — reuse the current guard's rewrap.
COLLAPSE join-022: DELETE the "case 2 __td_jr self-collides with inlined nested join's buried inner __td_jr, rebuild-left inlining-off" branch (~493-509). Under Phase 2 an inner join never wraps its children from its OWN condition, so no buried inner __td_jr exists. Keep case-1 (ordinary cross-side name collision) as step 4a.

## Analyzer change (single, surgical) — mark_node Join arm (~1303)
Keep `let (own_jl, own_jr) = own_expr_demands(&mut node.op);` (side-effect: mark_expr_subplans over condition subqueries) but bind `_own_jl/_own_jr` and set:
```rust
*left_requires_synthetic  = pending_jl;   // was pending_jl || own_jl
*right_requires_synthetic = pending_jr;   // was pending_jr || own_jr
```
Update the field docs (~796-805) + mark_join_alias_requirements doc (~1218) to: ANCESTOR demands only; condition demands handled at emission by requalify_join_condition.

## The rewrite predicate (H8 boundary) — walk cond with map_children (excludes subquery bodies)
For each ColumnReference c, let k = c.ordinal. REWRITE iff k==Some(k) && k<cond_schema.len() AND:
  (a) c.qualifier is synthetic (eq_ignore_ascii_case TD_JOIN_LEFT/RIGHT); OR
  (b) c.qualifier==None AND c.name is ambiguous (count>=2, case-insens) in cond_schema.
LEAVE VERBATIM: ordinal==None (correlated/deferred — tbl-005); Some(non-synthetic) (real user alias, already binds); None + unique name (already bare).
For a rewritten ref: side = (a) from qualifier spelling / (b) from k<left_len; local = k (left) or k-left_len (right). Target: if cond_schema name-count==1 → qualifier=None; else FromScope::of(side_ast, side_item).alias_for(local): Some(a)→qualifier=Some(a); None→record SideNeedsAlias{side}. If any side flagged → Err; else Ok(rewritten).

## debug_assert invariants (MANDATORY, inside requalify_join_condition / build_join)
1. cond_schema.fields[k].name.eq_ignore_ascii_case(&c.name)  // ordinal/name agreement — the load-bearing one
2. (q_is_jl) == (k < left_len)  // synthetic side ⇔ ordinal side (case a)
3. local < side_ast.resolved_schema.len()
4. pass < 2 in build_join
Do NOT add a mark_node-style assert on untrusted qualifiers (F13 lesson).

## Termination: cond_schema/left_len/ordinals computed once, invariant under wrapping. A fresh-unique-alias wrap makes a side single-exposed Derived → alias_for fast-path covers every local < width → no re-flag. Bound = 2.

## Corner cases: USING/NATURAL → condition None → step-4b break (no-op). Semi/anti → right_item built regardless → cond_schema includes right → handled. F11 same-plan_id-both-sides → AmbiguousColumn at resolution, emission never sees it. Distinct-plan_id self-join (emp JOIN emp) → both inline as emp → guard wraps movable one → requalifier binds emp/__td_jr; alias_for returns None for >1-exposed → retry wraps not mis-binds.

## H8 hiding places: (1) merged-vs-local ordinal confusion (assert 2+3 + asymmetric witnesses); (2) ordinal/name drift (assert 1 + asymmetric data); (3) alias_for fast-path i>=width (i<width guard); (4) stale FromItem — build FromScope from CURRENT pass items; (5) tier-f first-match ordinal is left-biased for ambiguous — predicate excludes qualified-non-synthetic, bare-ambiguous already errored at resolution; (6) user-alias ref over ancestor-wrapped side — non-occurring (SQL path no plan_ids; DF path no user aliases), documented Phase-3/4 residual.

## Tests / witnesses
Analyzer: mark_join — a join whose ONLY synthetic demand is its own ambiguous condition now stamps (false,false); ancestor-demand case still (true,true) (keep existing assertion ~6386).
Emission (each a witness):
1. dup-name plan_id cond over two bare tables → `FROM emp INNER JOIN dept ON (emp.dept_id) = (dept.dept_id)`, no __td_jl/jr, no __td_sub. (UPDATE render_join_side_plan_id_condition_overrides_aliased_relation_hoist ~8577 to this form; verify data parity.)
2. asymmetric-schema data-correctness (2 variants, left-heavier & right-heavier; side-swap changes DATA). Corpus-level row parity proves ordinal→alias map.
3. within-side-duplicate that's a boundary/double-bind today → green (distinct real aliases positionally).
4. over a wrapped side (SideNeedsAlias retry): outer cond referencing a re-scoping nested-join side (children Projects → empty nested RelScope) → requalifier flags → wrap fresh __td_jl → retry binds; <=2 passes.
5. join-022 collapse: UPDATE contract_collision_wraps_left_keeps_right_name ~8727 + the join-022 corpus case to the collapsed form (inner inlines real aliases; outer right keeps ancestor __td_jr; outer filter binds __td_jr.dept_id). Verify same data.
6. pass-through (stay green): correlated cond ref ordinal:None renders verbatim; real user-alias cond ref (e.dept_id) still inlines (render_project_over_join_hoists_user_aliases ~7972); adr023_phase1_unique_name_plan_id_condition_inlines_both_sides ~8527 unchanged.
Also UPDATE from_scope_derived_wrapped_side ~7396: alias_for(0) None→Some("__td_jl"); drop the "Phase 1 will flip" comment; keep covers_all/slot_quals asserts.

## What STAYS on synthetic path after Phase 2 (Phase 3/4 surface)
Above-join (ancestor) plan_id demands still stamp __td_jl/jr (resolve_column plan_id arm ~4271); pending_jl/jr still drives *_requires_synthetic → rung-1 still force-wraps for ancestor demands. All analyzer synthetic machinery (TD_JOIN consts, qualify_plan_id_refs, for_join_condition scopes, is_synthetic_join_qualifier, mark_join_alias_requirements/mark_node/synthetic_uses/own_expr_demands) RETAINED. reproject_qualifiers/output_uniquified/exprs_visible_in untouched (separate items).

---

# Phase 2.1 AMENDMENT — USING-parent guard for a duplicated key name in an inlinable join side

## Verified fact base (live DuckDB 1.5.0 probes)
- Flat `emp INNER JOIN dept ON (emp.dept_id)=(dept.dept_id) INNER JOIN emp2 USING (dept_id)` → Binder Error: Ambiguous reference "dept_id". Same with user aliases (e/d) — meaning the currently-GREEN test `using_parent_hoisted_slots_qualify_by_covering_alias` (~7765) pins DuckDB-INVALID SQL today (string-asserted, never executed).
- Single outer wrap `(SELECT * FROM emp INNER JOIN dept ON …) AS __td_jl INNER JOIN emp2 USING (dept_id)` → prepares OK. Old full shape (inner __td_jl/jr wraps) → also OK. So old validity does NOT depend on the inner wraps or hoisted slots — a single Derived wrap collapses the side to ONE binding; the USING binder errors only when the key resolves across two SIBLING bindings in one flat scope. Data: DuckDB picks the LEFTMOST duplicate inside a wrapped input (identical to pre-Phase-2 behavior → 2.1 is behavior-preserving).
- Key unique in the nested side (USING (id), id only in emp + emp2) over the flat chain → prepares OK (so the guard must be key-specific, not blanket).

## Mechanism: (A) emission-side inline guard (B mark_node-threading REJECTED: wrong layer; misses the transitive case where the dup-key join is nested deeper than a direct child; re-entrenches the machinery being retired)
Invariant stated directly: a side may inline flat under a USING parent only if every parent USING-key name is unambiguously bindable within it. side.resolved_schema is the side's full flattened output → duplicates at ANY nesting depth are caught.

## Exact sites (emission.rs)
1. build_join_side (~351): signature `parent_has_using: bool` → `parent_using: &[String]` (parent's using_columns; emptiness reproduces the bool). Update ladder doc rung 2.
2. inline_ok, the `item @ FromItem::Join {..}` arm: the USING conjunct becomes
   `!parent_has_using || (FromScope::of(side, item).covers_all() && !using_key_duplicated(&side.resolved_schema, parent_using))`
   with `parent_has_using = !parent_using.is_empty()`. On violation → rung 3 wrap (the probe-validated single outer wrap).
3. New helper next to has_unsafe_qualified_duplicate (~322):
   `fn using_key_duplicated(schema: &StructType, using: &[String]) -> bool` — true iff any c ∈ using matches >=2 schema fields by name, eq_ignore_ascii_case. Doc with the binder fact + probe result.
4. build_join call sites (~435, ~441): pass using_columns instead of has_using.
NOT touched: USING default_slots machinery (~522-632) — wrapped dup-key side is single-alias → slot_quals fast path → slot list byte-identical to old shape. Non-USING parents: empty parent_using → vacuous. Right side: may_inline_nested_join=false already blocks; guard harmless. Relation/Derived inline arms need no guard (single relation can't duplicate; Derived is one binding). NATURAL desugars to using_columns upstream → covered.

## Tests (2.1)
1. using_parent_with_synthetic_scoped_side_stays_wrapped (~7872) — UPDATE: premise assert flips to !left_requires_synthetic && !right_requires_synthetic (Phase-2 mark_node); wrap re-pinned as GUARD-driven; keep `sql.contains("AS __td_jl")`; replace blanket !contains("emp.")/!contains("dept.") (inner ON now legitimately reads `ON (emp.dept_id) = (dept.dept_id)` INSIDE the derived body) with: outer slot list is __td_jl-qualified and no emp./dept.-qualified slot in the OUTER select list.
2. using_parent_with_uncoverable_side_still_wraps (~7813) — holds unchanged (empty RelScope fails covers_all AND dup dept_id trips the guard).
3. using_parent_hoisted_slots_qualify_by_covering_alias (~7765) — SPLIT: (a) retarget to USING (id) (id unique in nested emp⋈dept; emp2 has id) preserving the per-field covering-alias purpose, now DuckDB-valid; non-key slots include BOTH e.dept_id and d.dept_id (distinct quals → not unsafe). (b) new dup-key sibling for the old USING (dept_id) shape asserting the WRAP (AS __td_jl; slot list `SELECT dept_id, __td_jl.id, …` then emp2-qualified right slots) + comment that the flat form is a DuckDB binder error.
4. NEW transitive-case test: USING parent ← ON-join ← nested dup-key ON-join, all inlinable → assert the outer-left side WRAPS (guard reads the whole flattened resolved_schema).
5. Corpus witnesses (optional, oracle-gated): SQL-path dup-key USING shape; a data variant where the two left dept_id columns differ per row (pins leftmost vs Spark). If Spark itself errors on this class, that's a FUTURE analyzer-side Spark-emulated error — out of 2.1 scope; the guard remains the DuckDB-validity backstop and τ's leftmost behavior is exactly pre-Phase-2, so no NEW divergence.

## Everything else in the Phase-2 spec above stands unchanged.
