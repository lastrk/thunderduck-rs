> **RETIRED 2026-07-13** — fully executed/reconciled by the N1–N10 + D1–D4 + E-series passes (`992fca0..HEAD`). Open remainders were folded into [`tasks/v2-open-work.md`](../v2-open-work.md); this copy is historical.

# Task: Attribute-identity unification (retire `ordinal` + `source_quals` + the type/identity re-derivations)

**Status:** ⛔ **ABORTED 2026-07-12 (implementation attempt)** — see the addendum at the top; the
design remains valid as a future, corpus-gated, Step-2-first effort. · **Created:** 2026-07-11 ·
**Branch:** `feat/v2-transpiler`

---

## ⛔ Implementation attempt — abort report (2026-07-12)

An implementation was attempted on `feat/v2-attr-identity` (worktree off `992fca0`) and
**aborted before any code landed**, per the risk clause, after a feasibility probe + an
independent adversarial Opus review ([dossier](attr-identity-feasibility-dossier.md)) confirmed:

1. **§6 Step 1 as written is architecturally unsound.** `resolve_column` can only stamp an
   `expr_id` from per-node `output_ids`; deriving those in `RelScope` re-MINTS fresh ids on
   every `TypedAst::new` re-derivation — and `analyze_sort` (analyzer.rs:3641-3643) re-stamps
   the mutated Aggregate/Project on exactly the sort-rebind path the ids serve → stale-id
   landmine for Step 3. The only sound stores are (a) the schema itself — i.e. Step 2 must come
   FIRST, Step 1 cannot precede it — or (b) a non-derived parallel `Vec<ExprId>` on `TypedAst`
   (reviewer's variant, ~300-450 lines), which re-creates the length-invariant parallel-vector
   anti-pattern this task exists to delete.
2. **Step 1's payoff evaporated upstream.** `55577ef` (concurrent session, Pass 33) retired
   `grouping_already_folded` via `AggregateProjection` — E1/E2's root is fixed, so id-equality
   in `semantic_eq` buys nothing observable today.
3. **§3's R2-4 line is an overpromise.** Attribute-on-schema does NOT eliminate the ~215
   computed-node `data_type(schema)` re-walks (resolved `ColumnReference`s already carry inline
   types; the walks type *operands*). That needs a typed expression tree — bigger, separate.
4. **The mandatory gate was unavailable.** The corpus differential (CLAUDE.md hard gate) cannot
   run in this environment (verified server-start contention); a 33.7k-line-module type swap
   (483 `resolved_schema` touches, 74 `ColumnReference` literals, 31 emission builders) on lib
   tests alone is the silent-wrong-column hazard §7 names.

**Reviewer verdict: NO-GO** (adversarial Opus review; claims A–D CONFIRMED, one enumeration
refinement noted above). Branch/worktree dismantled without commits; baseline was green (1141/0).

**Revised plan when this is picked up again:**
- Sequence AFTER the active incremental stream on `feat/v2-transpiler` quiets (merge-collision
  surface: aggregate/emission paths).
- Execute **Step-2-first** (introduce `ResolvedSchema(Vec<Attribute>)` directly; ids stored in
  the schema move through re-stamps by value) — delete §6's Step 1, it is not a valid de-risk.
- Require a working corpus-differential environment for every increment (the oracle IS the gate).
- Drop the R2-4 claim from §3, or re-scope it as a separate typed-expression-tree task.
- R2-6 (stamp `toPrettySQL` output name at conversion) is the one orthogonal, lib-test-gated
  piece that can proceed independently at any time.

**Correction to R2-2's remedy (2026-07-12), and the three-invariant sequence.**
The ~600-line ORDER-BY resolver does NOT "collapse to a slot lookup" — the restatement
target is *created by* resolution (the key arrives as text / an unresolved tree), and Spark
runs the same walk-match-promote-trim algorithm (`ResolveReferencesInSort` +
`buildAggExprList` + trim `Project`; τ's comment at analyzer.rs:3588 cites all three).
The machine is essential; it is ~2.4× overweight because τ lacks three ambient invariants
that make Spark's version ~150 lines. Establish them in this order (each corpus-gated):

1. **Fold at both front-ends, Spark-style.** Construct DataFrame aggregates as
   `grouping ++ aggregates` at the converter (`RelationalGroupedDataset.toDF` precedent),
   making every Aggregate "folded." Deletes the `offset` arithmetic, the
   `offset + aggregates.len() != schema.len()` guard, and merges
   `rebind_over_aggregate`/`rebind_over_project`. ⚠️ This retires the `AggregateProjection`
   flag added by `55577ef` (Pass 33) — same fact moved one level earlier, into construction,
   where Spark keeps it. Coordinate with that stream first.
2. **Alias-every-entry at resolution** (Spark's `UnresolvedAlias` invariant): every
   SELECT-list entry gets its `toPrettySQL` name as an `Alias` when the Project/Aggregate is
   analyzed. `bind_*_slot`'s mid-flight alias-pinning mutation collapses to "reference the
   entry's name." This is R2-6 arriving from a different direction — doing R2-6 as
   alias-at-resolution satisfies both.
3. **Stored-id `Attribute` schema** (Step 2 above): `semantic_eq` leaf = id compare;
   `ordinals_compatible` and the re-stamp hazard gone (ids ride through `analyze_sort`'s
   rebuild by value).

Expected outcome for the sort machinery: **~600 → ~250 lines**; the top-down walk, the
promote-and-append, and the trim `Project` remain — they are the algorithm itself, and τ's
end state (bare reference at a schema position under a trim Project) is already
shape-identical to Spark's (`SortOrder(AttributeReference)` under `Project(child.output)`).
**Origin:** [`v2-review-findings-2026-07-11.md`](v2-review-findings-2026-07-11.md) Review 2 (R2-3, R2-4, R2-7) + the `ordinal`-subsumption analysis.
**Governing principle:** CV **INV2** — "every τ decision is node-local (post-A) or a labeled C escape hatch … push the fact into the node."

---

## 1. Thesis

Three of Review 2's findings, plus the ADR-023 `ordinal` field, are **three encodings
of one missing fact**: *"which stable attribute is this column, and what does it emit as?"*

| Encoding today | Finding | What it compensates for |
|---|---|---|
| `ColumnReference.ordinal` (position in producing node's output) | ADR-023 | no attribute identity → positional surrogate |
| `RelScope.source_quals` + `source_quals_tracked` (parallel `Vec`, ordinal-indexed) | R2-7 | no per-attribute qualifier lineage |
| `semantic_eq` / `canonicalize_for_semantic_eq` / `ordinals_compatible` / nondeterministic roster | R2-3 | no exprId → structural identity guess |
| `data_type(schema)` / `nullable(schema)` re-walks (~218 sites) | R2-4 | no resolved type/nullability on the node |
| emission `output_uniquified` / `bare_dup_ordinal` / `requalify_column_ref` | R2-7 | no unique physical binding → positional dedup |

Give every resolved column a **stable attribute identity** carrying its own type,
nullability, and source-qualifier lineage, and bind references **by identity → unique
emitted alias**, and all five collapse. A point `exprId` (for `semantic_eq` alone) does
**not** subsume `ordinal` — `ordinal` also serves lineage inheritance (`source_quals_of`,
analyzer.rs:458/566) and emission positional binding (`requalify_column_ref`:513,
`bare_dup_ordinal`:1155). Only the full unification retires all three roles.

## 2. Design

Introduce a **τ-owned resolved-schema type** distinct from the wire `StructType`
(INV10: `StructType`/`StructField` stay the value/Arrow-boundary types; the analyzer's
schema becomes τ's own):

```rust
// crates/core/src/transpiler_v2/  (τ-owned; NOT crate::types)
pub struct ExprId(u64);

pub struct Attribute {
    pub name: String,                       // Spark verbatim (duplicates preserved)
    pub data_type: DataType,                // resolved (was recomputed via data_type(schema))
    pub nullable: bool,                     // resolved (was recomputed via nullable(schema))
    pub expr_id: ExprId,                    // stable identity (subsumes ordinal-as-identity)
    pub source_quals: BTreeSet<String>,     // lineage, now intrinsic (was RelScope parallel Vec)
}
// TypedAst.resolved_schema: StructType  →  ResolvedSchema(Vec<Attribute>)
// ColumnReference.{ordinal, data_type, nullable} → ColumnReference.target: ExprId
```

**Allocation & propagation** (bottom-up, deterministic counter threaded through the analyzer):
- Leaf sources mint one fresh `ExprId` per output column.
- Passthrough ops propagate child attributes unchanged (so a projected-through column keeps
  its `expr_id` **and** its `source_quals` — this is exactly ADR-023's F10-vs-F8 distinction,
  now intrinsic rather than ordinal-indexed).
- `Alias`/computed columns mint a fresh `ExprId` with `source_quals = ∅` (created ⇒ inherits
  no qualifier — ADR-023 F8).
- `Join` concatenates both sides' attributes verbatim — ids are globally unique, so no
  renaming (this is where identity beats string qualifiers).

**Resolution** (`resolve_column`) binds an `UnresolvedColumn` to the matched output
attribute and stamps its `ExprId` (replacing the `ordinal` stamp). Match count 0/1/2+ →
`UnknownColumn`/bound/`AmbiguousColumn` is unchanged (still a scope property, not an ordinal
property). `plan_id` resolution unchanged.

**Emission** binds by identity: each block assigns every output attribute a **unique alias**
derived from its `ExprId`; a reference emits that alias. Because no two attributes share an
alias, DuckDB binds unambiguously and the positional duplicate-name machinery
(`bare_dup_ordinal`, `requalify_column_ref`, `output_uniquified`) is unnecessary. Human
qualifiers are still regenerated at emission from the current alias (ADR-023 unchanged).

**`semantic_eq`** collapses to: strip aliases → leaves compare `expr_id`; computed nodes
recurse structurally (Spark's `Canonicalize` also stays structural for computed exprs).
Nondeterministic calls each mint a fresh `ExprId` ⇒ never equal ⇒ delete the roster.

## 3. What it retires (concrete)

- `ColumnReference.ordinal` field + `ordinals_compatible` (analyzer.rs:4166) + the
  `ColumnReference::eq` ordinal-exclusion hack.
- `RelScope.source_quals` + `source_quals_tracked` + `source_quals_of` (~200-line match) +
  `source_quals_tracked_of` (its lockstep mirror) + the `source_quals.len()==resolved_schema.len()`
  invariant + the unconditional `analyze_sort` re-stamp it forces.
- `canonicalize_for_semantic_eq` shrinks to alias-strip; `contains_nondeterministic_call` +
  `NONDETERMINISTIC_FN_NAMES` deleted.
- emission `output_uniquified` / `bare_dup_ordinal` / `requalify_column_ref` positional
  binding + their load-bearing asserts.
- the ~218 `data_type(schema)`/`nullable(schema)` re-walks become `attr.data_type`/`.nullable`
  reads; the R2-5 scattered DATE casts become one coercion driven by the attribute's type.

## 4. What it MUST preserve (ADR-023 decision outcomes — the oracle gates these)

F8 (created alias errors) · F10 (projected-through succeeds) · F11 (ambiguity by match
count, incl. plan_id binding both join sides) · correlated outer refs (tbl-005, `sq-*`) ·
plan_id resolution · **verbatim duplicate names** in `resolved_schema` and the Arrow wire
schema (uniquification is emission-only) · emission-time qualifier regeneration (never carry
strings) · ordinal remaps only at fixed structural points, no optimizer (ADR-001).

## 5. ADR work required

### NEW — **ADR-024: τ resolves references to a stable attribute identity carrying its own type, nullability, and lineage; emission binds by identity via unique aliases**
- **Depends on:** ADR-005 (schema-threading), ADR-006 (single resolve pass), ADR-021 (τ owns the substrate), ADR-022 (error categories). **Supersedes:** ADR-023.
- Records the decision above, the rejected alternatives (point-`exprId`-only — leaves `ordinal`
  + `source_quals` standing; keep-ordinal-add-id — two identity encodings), and the migration.
- Cites **INV2** as the governing principle (this is INV2 applied to identity/type/lineage:
  the current re-derivations are the non-local decisions INV2 warns against) and **strengthens
  INV5** (τ's resolved schema becomes strictly richer than the wire schema).

### SUPERSEDED — **ADR-023** ("(source-qualifier lineage, ordinal) at analysis time")
- Mark **Superseded by ADR-024**. ADR-023's *representation* (ordinal + parallel `source_quals`,
  positional emission binding) is replaced; its *decision outcomes* (§4) are carried forward
  verbatim as ADR-024's must-preserve constraints. ADR-023 is currently **Proposed** and only
  partially built (tiers 1+2 committed, tier-3 reverted per the dev journal), so this is a
  representation swap before full ratification, not a rip-out of shipped design.
- Rationale for supersede-not-amend: ADR-023's core framing is "ordinal *is* the mechanism
  (Calcite `RexInputRef` model)"; ADR-024 inverts that to "attribute identity is the mechanism,"
  which reverses ADR-023's own rejected-alternative analysis — a new decision, not a tweak.

### AMENDMENTS
- **ADR-005** (schema-threading analysis): the threaded schema is now `ResolvedSchema(Vec<Attribute>)`
  carrying resolved `(type, nullable)` per attribute — the analyzer stops discarding what it inferred.
  Add a note that `data_type(schema)`/`nullable(schema)` re-derivation is replaced by attribute reads.
- **ADR-006** (single resolve pass): the resolve pass now assigns/propagates `ExprId`s in addition
  to (replacing) ordinals; match-count error semantics unchanged.
- **INV10 clarification** (one sentence): `Attribute`/`ResolvedSchema`/`ExprId` are τ-owned analysis
  types living in `transpiler_v2`; `StructType`/`StructField` remain the verbatim value/Arrow-wire
  types, converted at the emission/wire boundary. No new cross-boundary import.

### COMPANION node-local-fact fixes — **no separate ADR** (INV2-governed code changes)
- **Aggregate output-layout flag** (Recommended fix #1 / R2-1): a front-end constant
  (`AggregateProjection{Folded,Grouped}`) already exists on the node; finish threading it so
  `grouping_already_folded` is deleted. Fixes E1/E2. **A concurrent session is already doing
  this — coordinate before touching the aggregate path.** At most a one-line note in ADR-024
  (same "push the fact into the node" family); no ADR of its own.
- **Stamped output name** (R2-6): also INV2 (push the Spark `toPrettySQL` name onto the node at
  conversion). Distinct fact from identity; track separately, no ADR.

## 6. Migration plan (incremental, oracle-gated at each step)

1. Add `ExprId` + `expr_id: Option<ExprId>` to `ColumnReference` (keep `ordinal` alongside).
   Populate at leaf scans / alias creation / `resolve_column`. Switch `semantic_eq`'s leaf arm
   to prefer id-equality, falling back to the ordinal path when either id is `None`. → deletes
   `ordinals_compatible`'s fragility on resolved paths without touching `StructType`. **Flips the
   E2 repro; de-risks the model.**
2. Introduce `Attribute`/`ResolvedSchema`; change `TypedAst.resolved_schema` type; carry
   `(type, nullable, source_quals)` on attributes; delete `RelScope.source_quals(_tracked)` and
   the parallel derivation. Convert to `StructType` at the wire boundary.
3. Switch emission to bind-by-id + unique aliasing; delete `bare_dup_ordinal` /
   `requalify_column_ref` / `output_uniquified`.
4. Delete `ColumnReference.ordinal`, the `data_type/nullable(schema)` re-walks (read attributes),
   and the R2-5 scattered DATE casts (one type-driven coercion).

## 7. Risks & gates

- **Silent wrong-column is the hazard** (ADR-023's own noted risk). The ADR-014/015 differential
  oracle is the gate — run the full DataFrame + SQL corpora green at every migration step; the
  join / duplicate-name / correlated (`sq-*`, tbl-005) and F8/F10/F11 cases are the sharp ones.
- **Emission bind-by-id is the large surface** (every block builder threads an id→alias map) — and
  it overlaps the emission code the concurrent aggregate-flag session is editing. Sequence after,
  or coordinate.
- Deterministic `ExprId` allocation (a threaded counter) — verify identical plan ⇒ identical ids
  so analyzer snapshot tests stay stable.

## 8. Acceptance criteria

- Corpora green (DataFrame 384 + SQL 396), incl. the two `xfail(strict)` repros flipping
  (`test_order_by_grouping_expression_over_multikey_aggregate`, the E2 repro).
- `ordinal`, `source_quals`, `source_quals_tracked`, `ordinals_compatible`,
  `canonicalize_for_semantic_eq` (bulk), `NONDETERMINISTIC_FN_NAMES`, `output_uniquified`,
  `bare_dup_ordinal`, `requalify_column_ref` deleted; INV2/INV3 invariant checks pass.
- ADR-024 written & ratified; ADR-023 marked Superseded; ADR-005/006 amendments landed.
