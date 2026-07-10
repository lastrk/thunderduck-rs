# ADR-023 Tier-3 resolver — design for review (2026-07-10)

Status: **design, no code**. Written after the reverted F10 emission attempt
hit the tbl-005 correlated trap. Committed so far: tiers 1, 2, 3a, 3b
(feat/v2-transpiler; F11 witnesses join-023/jn-024 green, 0 regressions).
Remaining deferred witnesses: **filt-018 (F10), filt-019 (F8)**.

## 1. Executive finding — this REVISES ADR-023

ADR-023 claims ordinals make F8/F10 "correct by construction … no per-column
lineage needed." **That claim is too strong.** The ordinal model is excellent
for what it was derived from (Calcite / SQL): it eliminates the *strand* class
(F5/F9/F12), gives F11 ambiguity (3b), and preserves correlation. But F8 vs
F10 is a **Spark-DataFrame attribute-lineage** semantic that the pure
ordinal/SQL model does not replicate:

- **F10** `emp.alias('e').select('e.dept_id','e.name').distinct().filter(e.name=='x')`
  → Spark **SUCCEEDS**. Spark keeps `e` usable on the *projected-through*
  output column `name` (attribute lineage).
- **F8** `df.alias('e').select(col('dept_id').alias('k')).filter(col('e.k')==101)`
  → Spark **ERRORS** (UNRESOLVED_COLUMN). `k` is a *created* alias; it inherits
  no qualifier, so `e.k` is invalid.

Both present the analyzer the *identical* shape: `e.<col>` where `e` binds no
local scope. A **strict** ordinal/SQL resolver (`e` is not a namespace above
the SELECT → error) gets **F8 right, F10 wrong** — and additionally regresses
**USING joins** (`e.name` over `emp e JOIN dept d USING(dept_id)`, whose
`RelScope` is empty by design, resolves today via the permissive name-only
fallback). Keeping the resolver **permissive** (today's tier-(f)) gets F10
resolvable but F8 wrongly succeeds (silent divergence — ADR-022 forbids), and
was the root of the strand/tbl-005 problems.

**Therefore:** distinguishing F8 from F10 (while keeping USING + correlation
green) *requires* per-output-column source-qualifier information — Spark's
attribute lineage. Ordinals do NOT substitute for it here. The reverted
string-lineage ADR was not redundant; it was **complementary and necessary**
for this cluster. Ordinals (done) handle strand-elimination + emission;
lineage handles resolution parity. We need both.

## 2. The distinction the resolver must make

For a reference `q.name` (after ambiguity handled in 3b):

| case | condition | outcome |
|---|---|---|
| local, unique | `q` binds exactly one local scope range | resolve locally (unchanged) |
| **projected-through** | `q` binds no local scope, but output col `name`'s **source-qualifier set contains `q`** | resolve locally by ordinal, drop `q` at emission — **F10, USING joins** |
| **correlated** | `q` binds no local scope, not in output lineage, but `q` bound in an **outer** scope | keep `q` qualified, resolve outward (tier-g) — **tbl-005, sq-\*** |
| **created / unknown** | `q` binds no local scope, not in output lineage, not outer | `UnknownColumn` — **F8, typos** |

The current tier-(f) collapses rows 2+4 into "resolve by name" (F8 wrongly
succeeds), and row-3 only works because *emission* currently keeps the
qualifier (which is exactly what my reverted F10 emission change destroyed).

## 3. Design — per-output-column source-qualifier lineage (attribute lineage)

Add a derived per-node fact, computed structurally like `RelScope` (NOT a
hand-maintained string set threaded ad-hoc — derived once per operator from
its own structure):

`source_quals: Vec<SmallSet<String>>` — one entry per output column (by
ordinal), the set of relation qualifiers that column inherits. Derivation:

- `TableScan{table, alias}`: every col → `{table} ∪ {alias?}`.
- `AliasedRelation{alias}`: every col → `{alias}` (replaces child's sets).
- **`Project`**: per projection expr — a **passthrough** `ColumnReference` →
  inherit that source column's set; an **`Alias(_, k)`** or any computed expr
  → **empty set** (created). ← the F8/F10 hinge.
- passthrough ops (`Filter`/`Sort`/`Limit`/`Deduplicate`/…): child's sets
  verbatim.
- `Join`: left sets ++ right sets (offset). USING output: coalesced-key cols →
  union of both key sources; rest as concatenated (the 3c USING map).
- `Aggregate`: grouping cols inherit source; aggregate cols → empty (created).
- `SetOp`: Spark's rule (first child's, positionally).

`resolve_column` consults `source_quals` in the "q binds no local scope" arm
(before the outer-scope/tier-g and before any name-only fallback): if the
uniquely-named output column's `source_quals[k]` contains `q` → resolve to it
(stamp ordinal `k`, F10/USING); elif outer binds `q` → correlated (tier-g,
ordinal None); else → `UnknownColumn` (F8).

### Why this avoids the tbl-005 trap by construction
tbl-005 inner `e.dept_id`: `e` binds no local scope; output col `dept_id`'s
`source_quals` = `{e2}` (NOT `e`) → not projected-through-`e` → check outer →
`e` bound outer → **correlated**, keep qualified, ordinal None → emission
leaves `e.dept_id` for DuckDB's LATERAL binder. Correlation preserved because
the decision is made at **analysis time** with both lineage and outer-scope in
hand — the exact information emission lacked.

## 4. Sub-chunk sequence (each coder→reviewer→corpus-gated, committed green)

- **3c — add `source_quals`** per node, derived per operator (incl. the USING
  output→input map). ADDITIVE: `resolve_column` does NOT yet consult it;
  emission unchanged → corpus-neutral. (Like 3a: pure plumbing + unit tests
  asserting the derivation per operator, esp. Project passthrough-vs-alias.)
- **3d — resolve_column consults `source_quals`** in the no-local-scope arm:
  flip **F8** (created → UnknownColumn) and **F10** (projected-through →
  resolve by ordinal). Preserve USING (lineage carries the key sources) and
  correlation (outer check before UnknownColumn). Gate: filt-018 + filt-019
  flip, tbl-005/sq-\*/USING stay green, 0 regressions.
- **3e — retire the emission strand machinery**: with refs resolved correctly
  at analysis time (dead qualifiers dropped or bound by ordinal; correlated
  kept), `strip_stranded_qualifiers`, the F5/F9/F12 wrap-boundary rewrites,
  `exprs_visible_in`'s qualifier exemptions, and F14's walkers become dead —
  delete as verified. Gate: 0 regressions, all witnesses stay green.

## 5. Alternative considered — pure ordinal, F8/F10 as boundary divergences
Skip lineage; make resolver strict. Fixes F8 but leaves F10 (and USING) as
τ-errors-where-Spark-succeeds divergences. Rejected: those are the
Spark-accepts-τ-rejects divergences ADR-022 wants minimized, and USING-join
regression is unacceptable.

## 6. Cost / risk
Substantial: `source_quals` on every node + per-operator derivation (must be
Spark-exact — the risk area, esp. Project/Aggregate/SetOp/USING) + resolver
rework + strip retirement. 3 gated sub-chunks. The differential corpus is the
arbiter for the derivation's correctness (silent wrong-column is the hazard).

## 7. Recommendation
Adopt the hybrid: keep the committed ordinal work (strand + F11 + correlation),
ADD per-output-column source-qualifier lineage for F8/F10 resolution. Amend
ADR-023 to record that ordinals and attribute-lineage are complementary (the
"ordinals alone suffice for F8/F10" claim was wrong). Proceed 3c → 3d → 3e.
