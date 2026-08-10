# Feasibility dossier: attribute-identity unification (for adversarial review)

**Worktree:** `/workspace/.claude/worktrees/attr-identity` (branch `feat/v2-attr-identity`, off `992fca0`)
**Task under implementation:** `tasks/v2-attribute-identity-unification.md`
**Baseline:** `cargo test -p thunderduck-core --lib` → 1141 passed / 0 failed.
**Context change since the task was written:** `55577ef` already retired
`grouping_already_folded` via `AggregateProjection` — the task's companion fix R2-1 is DONE
upstream; E1/E2's root is fixed. The remaining payoff of this task is identity/lineage/type
unification only.

## Claim A (soundness): the task's "minimal Step 1" is architecturally unsound

Step 1 says: add `ExprId` + `expr_id: Option<ExprId>` on `ColumnReference`, populate at leaf
scans / alias creation / `resolve_column`, prefer id-equality in `semantic_eq`.

1. For `resolve_column` (analyzer.rs:4597) to stamp an id, the id must be readable off the
   producing node — i.e. per-node `output_ids` (there is no other store). Options: (i) derive
   them in `RelScope` (like `source_quals`), or (ii) store them in `resolved_schema`.
2. Option (i) is unsound: `RelScope` is a **pure derivation** of `(op, resolved_schema)`
   recomputed by every `TypedAst::new` (analyzer.rs:99). Fresh-id **minting is stateful**, so a
   derived `output_ids_of` re-MINTS on every re-derivation.
3. Re-derivation over an existing subtree happens on the exact path this machinery serves:
   `analyze_sort` (analyzer.rs:3641-3643) `mem::replace`/`mem::take` + `TypedAst::new` re-stamps
   the Sort's child — an **Aggregate/Project, the id-minting node classes** — after
   `promote_aggregate_subtree`/`promote_project_subtree` (analyzer.rs:3932/3983) mutate
   `schema.fields` in place. Computed outputs get NEW ids after sort keys/select entries were
   already bound → stale-id mismatches, inert in Step 1 but a landmine for Step 3
   (bind-by-id emission).
4. Sound fixes: (a) store ids **in the schema** — ids then move through re-stamps by value; but
   that IS Step 2's type change, so Step 1 cannot precede Step 2; or (b) an out-of-band
   "re-stampers must preserve ids" convention — recreating the hand-maintained-invariant
   anti-pattern (cf. `source_quals.len()==resolved_schema.len()`) the task exists to remove.

## Claim B (cost): Step 2's real entry cost

- `resolved_schema` textual touches: **483** across `transpiler_v2/*` + `connect-server`.
- `schema: &StructType`/`&Schema` fn params in transpiler_v2: **94** (alias `pub type Schema =
  StructType`, analyzer.rs:66 — a lever, but construction sites and `.fields` accesses remain).
- `ColumnReference {` literal constructions: **74** (field-add churn).
- Module sizes: analyzer 13,857 / emission 16,678 / expression 3,230 lines.
- Step 3 (bind-by-id emission): **31** `fn build_*` block builders need an id→alias map.
- Deletions bought: `source_quals_of` 204 + `source_quals_tracked_of` 63 +
  `ordinals_compatible` 14 + roster/walk ~33 + emission positional dedup ~90 ≈ **~400 lines**
  against an estimated 1.5–3k-line diff.

## Claim C (overpromise): Attribute-on-schema does NOT deliver R2-4

The task's §3 claims the ~218 `data_type(schema)`/`nullable(schema)` re-walks become field
reads. False for computed nodes (Binary/FunctionCall/CaseWhen/…): their types live on the
**expression**, not the schema. Fixing R2-4 needs a typed expression tree — a separate, larger
refactor. Attribute-on-schema only cheapens the ColumnReference leaf lookups (which already
carry `data_type: Option<DataType>`).

## Claim D (verification gap): the mandatory gate is unavailable here

CLAUDE.md requires the corpus differential (no previously-green case regresses) before done.
In this environment the differential harness could not start both servers (verified failure:
"Failed to start both servers", earlier this session; known shared-worktree/Spark-startup
contention). A 33.7k-line-module schema-type swap validated only by lib tests is exactly the
"silent wrong-column" hazard the
[retired ADR-023](../docs/adrs/retired/adr-023-ordinal-reference-resolution.md)
flags as the migration risk.

## Questions for the reviewer (adversarial: try to REFUTE)

1. Is Claim A's re-mint-on-re-stamp argument correct? Check `TypedAst::new` (analyzer.rs:99),
   `analyze_sort` (3641), the promotion sites (3932/3983), and whether any other mechanism
   would keep derived ids stable.
2. Is there a **sound** variant under ~500 lines that gets stable `ExprId`s WITHOUT changing
   `resolved_schema`'s type? Specifically evaluate: (a) `expr_id: Option<u64>` added to the
   wire `StructField` (crate::types) — preserves `resolved_schema: StructType`, ids move by
   value through re-stamps; weigh the INV10/value-type-purity violation, `StructField` literal
   construction churn repo-wide, and PartialEq/wire-boundary effects; (b) minting at the
   CommonAst/converter stage; (c) anything else.
3. Verify Claim B's numbers by your own greps; estimate the true diff size for Step 2.
4. Verdict: GO (name the sound chunk order) or NO-GO (abort per the task's risk clause), given
   the corpus gate is unavailable in this environment and a concurrent session is actively
   landing incremental fixes on the parent branch (`55577ef` retired the fold heuristic
   already).


---

## Addendum (2026-07-12, post-N9): landed via the Step-2-first plan

The NO-GO verdict above was environment- and design-scoped, and both blockers were later
cleared: the corpus harness became reliable (serial runs), and N7/N8 shrank the sort-rebind
machinery. N9 then landed in four increments on `feat/v2-transpiler` per the revised
Step-2-first plan — ids as STORED schema state (Claim A's landmine avoided by construction,
regression-pinned), `ordinals_compatible` and the `source_quals` parallel machinery deleted,
ADR-024 written. Claim C (Attribute-on-schema does not deliver R2-4) held: the typed
expression tree remains future work. HEAD blast-radius numbers at landing: 494
`resolved_schema` touches migrated mechanically; connect-server needed zero changes (wire
boundary = two `mod.rs` functions).
