# Phase 3a design — unify join-CONDITION plan_id resolution; delete qualify_plan_id_refs

Base: feat/v2-transpiler @ 2b3d06c (Phase 2+2.1). No ADR amendment required.

## Empirical pin (H1, done — do not re-derive)
- Live classic PySpark 4.1.1: `df.join(df, df.x==df.x)` SUCCEEDS (condition binds; later `.select(df.x)` raises _LEGACY_ERROR_TEMP_1182).
- Spark CONNECT (the corpus driver; source-derived from sql/catalyst ColumnResolutionHelper::resolveDataFrameColumnByPlanId v4.1.1): both sides carry the same plan_id, both resolve depth-0, the merge fold throws → error class **AMBIGUOUS_COLUMN_REFERENCE** (sqlState 42702) AT CONDITION ANALYSIS. (Distinct from AMBIGUOUS_REFERENCE 42704, the bare-name class.)
- DECISION: the unified path raises Spark-emulated AMBIGUOUS_COLUMN_REFERENCE for same-plan_id-both-sides condition refs. τ's current silent-left-bind is a parity bug, fixed here. The oracle matches tokens EXACTLY (dataframe_diff.py:187/193) and self-checks the reference side, which validates the pin against real Spark Connect at corpus-run time.

## Changes (all analyzer.rs unless stated)
### 1. New error variant — AnalyzerError (~1120), spark_class (~1197), bridge (~1609)
```rust
/// A plan_id-tagged reference binds the SAME join-side plan_id on BOTH sides
/// of one join (the un-realiased self-join `df.join(df, …)`). Spark cannot
/// tell which side is meant. Distinct Spark class from `AmbiguousColumn`
/// (bare-name ambiguity): AMBIGUOUS_COLUMN_REFERENCE (42702) vs
/// AMBIGUOUS_REFERENCE (42704).
#[error("[SPARK-EMULATED] column `{name}` is ambiguous — the same DataFrame is joined on both sides")]
AmbiguousColumnReference { name: String },
```
- spark_class: `Self::AmbiguousColumnReference { .. } => Some("AMBIGUOUS_COLUMN_REFERENCE"),`
- analyzer_error_to_emission_error: add `| AnalyzerError::AmbiguousColumnReference { .. }` to the SparkEmulated flow (Display leads with the [TOKEN]).

### 2. ResolveContext::for_join_condition (~3993) binds the join's OWN plan_ids
Signature: add `left_plan_ids: &[i64], right_plan_ids: &[i64]`. Body: build plan_ids OWN-first then children (mirrors RelScope::of Join arm ~337-357 so lookup first-match picks the nearest side); seed ambiguous_plan_ids with the own-intersection then extend with both children's:
```rust
let left_range  = 0..left_len;
let right_range = left_len..left_len + right_len;
let mut plan_ids = Vec::new();
for &pid in left_plan_ids  { plan_ids.push((pid, left_range.clone(),  JoinSide::Left)); }
for &pid in right_plan_ids { plan_ids.push((pid, right_range.clone(), JoinSide::Right)); }
plan_ids.extend(left.scope.plan_ids.iter().cloned());
plan_ids.extend(right.scope.plan_ids.iter().map(|(pid,r,side)| (*pid, offset(r), *side)));
let mut ambiguous_plan_ids: Vec<i64> =
    left_plan_ids.iter().filter(|p| right_plan_ids.contains(p)).copied().collect();
ambiguous_plan_ids.extend(left.scope.ambiguous_plan_ids.iter().copied());
ambiguous_plan_ids.extend(right.scope.ambiguous_plan_ids.iter().copied());
```
Keep the existing aliases construction (incl. the __td_jl/__td_jr alias pushes ~4021-4022 — removing them is Phase 3b). DELIBERATE divergence from RelScope::of: NO keep_right gate — the condition resolves against the full merged schema regardless of join type (semi/anti included); both sides' own plan_ids + own-intersection bound unconditionally (Spark runs the fold over both children irrespective of join type).
Call site (~2595): pass `&left_plan_ids, &right_plan_ids` (already bound in analyze_join ~2473-2474).

### 3. DELETE qualify_plan_id_refs (~3898-3916) + its sole call site (~2594)
The condition expr now flows to resolve_and_stamp carrying UnresolvedColumn{qualifier:None, plan_id:Some(N)} → resolve_column's plan_id arm. Comment updates: block comment 2578-2591 (describe the plan_id arm, drop "pre-process/synthesize"); mark_join_alias_requirements doc 1231-1238 (drop the qualify_plan_id_refs mention); for_join_condition doc 3990-3992; plan_id-arm comment 4253 ("the condition and above-join paths now share this arm").

### 4. resolve_column plan_id-ambiguous arm (~4264-4272): swap error
Before: Err(AmbiguousColumn { name, candidates: vec![__td_jl.name, __td_jr.name] })
After:  Err(AmbiguousColumnReference { name: u.name.clone() })
(This arm is the unification point — reached from BOTH paths. Rest of the plan_id arm 4273-4315 unchanged.)

## Post-unification invariants (verified in design; encode as tests)
- For non-ambiguous condition refs the plan_id arm stamps ordinal ALWAYS (merged ordinal) and the side qualifier ONLY when the name is ambiguous in the merged schema — byte-identical to the deleted path post-Phase-1 (unique → None+ordinal; ambiguous → Some(side)+ordinal). Emission (requalify_join_condition) and mark_node see the same refs → unaffected.
- The synthetic resolver arm becomes UNREACHABLE from the condition path in production (only user-typed __td_jl → F13 UnknownColumn, and two contrived unit tests 12206/12252 that hardcode qcol("__td_jl",…) — keep the alias pushes so they still pass; retiring that surface is 3b).
- Behavior deltas: (1) direct self-join same-pid → AMBIGUOUS_COLUMN_REFERENCE (intended, Spark-matching); (2) shared-subtree self-join (df.filter(a).join(df.filter(b), df.x==df.x)) → same error (matches Spark; not in corpus); (3) name-missing-in-side → UnknownColumn with qualifier None instead of Some(__td_jl) — SAME τ class (UNRESOLVED_COLUMN.WITH_SUGGESTION); (4) unknown plan_id → unchanged fallthrough; (5) aliased-self-join at different depths (df.join(df.alias("d2"),…)) → τ over-flags (pre-existing over-approximation ALREADY on the above-join path via RelScope::of own_ambiguous; inherited for path-consistency; Spark resolves — do NOT add a witness; depth-aware fix is a future item).
- Corpus-safe: plan_id condition refs appear only in join-002 and join-022 (distinct inputs); all other self-joins use .alias+F.col (no plan_id); SQL corpus has zero plan_id. Neither delta 2 nor 5 reachable from a green case.

## Tests
Changed existing:
- plan_id_binds_both_sides_of_same_join_is_ambiguous (~12299): match AmbiguousColumnReference { name: "id" } instead of AmbiguousColumn{candidates.len()==2}.
- spark_class test (~8956): add AmbiguousColumnReference → Some("AMBIGUOUS_COLUMN_REFERENCE"); KEEP AmbiguousColumn → AMBIGUOUS_REFERENCE.
- join_flags_* / adr023_phase1_*_condition_* (~6256-6370): must pass UNCHANGED (distinct pids → own-intersection empty; refs byte-identical). Do not edit.
New analyzer unit tests:
1. Condition self-join same plan_id both sides: emp(pid=1) ⋈ emp(pid=1), condition pcol("id",1)==pcol("id",1) → analyze Err(AmbiguousColumnReference{name:"id"}).
2. for_join_condition own-plan_id binding, distinct pids: emp(pid=1) ⋈ dept(pid=2), pcol("dept_id",1)==pcol("dept_id",2) → resolves left→__td_jl right→__td_jr with ordinals 0 and 6 (the anti-regression sentinel proving the own-plan_id binding replaced qualify_plan_id_refs).
3. Bridge: analyzer_error_to_emission_error(AmbiguousColumnReference) → SparkEmulated with Display leading "[AMBIGUOUS_COLUMN_REFERENCE]" (mirror mod.rs:143 test).
New corpus witness (tests/integration/differential/dataframe_corpus.py, born red pre-3a, flips green):
- case "join-024", group "join": `I["emp"].join(I["emp"], I["emp"]["id"] == I["emp"]["id"])` with expected_error="AMBIGUOUS_COLUMN_REFERENCE" (follow the existing case(...) style near join-022 ~476; the oracle self-checks the reference side, validating the pin against live Spark Connect).
