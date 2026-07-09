# v2 Corpus-Driven Pipeline — Final Report

**Corpus:** `tests/integration/differential/sql_corpus.py` (`sql_v2`) —
the fitness function for τ's SQL front-end, per `CLAUDE.md` / ADR-022.

**Final state: 269/269 (100%).** Passes 4-18 (below) closed every genuinely-fixable τ gap,
reaching 265/270 with 5 cases documented as confirmed-invalid Spark SQL on the pinned Spark
4.1.1 reference. A follow-up pass 19 (corpus-correctness, not a τ fix) had `rust-architect`
empirically re-verify all 5 against live Spark: 4 had a semantically-faithful, Spark-valid
rewrite that still exercises genuinely distinct τ machinery (not near-duplicates of existing
coverage), and 1 (sq-013) had no useful rewrite and was deleted. All 4 rewrites pass τ with
zero new implementation work — see `tasks/v2-corpus-driven-pass-log.md` pass 19 and
`.agent-output/unsolvable.md` for full live-verification detail and exact rewritten SQL.

## Trajectory (passes 4→18, this pipeline invocation)

| Pass | Case(s) | Δ | Before → After |
|------|---------|---|-----------------|
| 4  | jn-006 | +1 | 246 → 247 |
| 5  | tech-debt sweep | +0 | 247 → 247 |
| 6  | jn-013, jn-015, sq-015 | +3 | 247 → 250 |
| 7  | jn-008 | +1 | 250 → 251 |
| 8  | fn-018, lit-006 + SQL-harness error-parity | +2 | 251 → 253 |
| 9  | pr-007 | +1 | 253 → 254 |
| 10 | tech-debt sweep | +0 | 254 → 254 |
| 11 | cx-007, cx-008, cx-009 | +3 | 254 → 257 |
| 12 | cx-011 | +1 | 257 → 258 |
| 13 | tbl-007, tbl-012 | +2 | 258 → 260 |
| 14 | pv-002 | +1 | 260 → 261 |
| 15 | tech-debt sweep | +0 | 261 → 261 |
| 16 | sq-010 | +1 | 261 → 262 |
| 17 | tbl-005 (JOIN LATERAL) | +1 | 262 → 263 |
| 18 | cte-009, cte-010 (WITH RECURSIVE) | +2 | 263 → 265 |
| 19 | corpus correction: sq-011/012/016, pv-006 rewritten; sq-013 deleted | +4 / −1 total | 265 → 269 (269 total) |

Full per-pass root cause / fix / ADR citations in `tasks/v2-corpus-driven-pass-log.md`.
Every pass ran the complete diagnostician → architect → coder → reviewer → perf-reviewer
loop with zero DEFERred Medium/Low findings, and zero regressions on either corpus
(`sql_v2` or the 329-case DataFrame `core_v2` corpus) at every step.

## The corpus-correctness pass (pass 19)

Passes 4-18 left 5 cases documented as confirmed-invalid Spark SQL — in each, the REFERENCE
Spark session itself threw before τ was ever exercised or compared, so no amount of τ work
could make them pass. Rather than accept 265/270 as a permanent ceiling, `rust-architect` was
tasked with empirically determining (against the live vendored Spark 4.1.1 reference, not
assumption) which of the 5 had a genuinely-fixable rewrite that still tested the original τ
capability, vs. which should just be deleted as not worth fixing:

1. **sq-011/sq-012** (`salary > ALL/ANY (subquery)`) — Spark 4.1.1 has no quantified-comparison-
   over-subquery grammar at all. Rewritten to the exact relational decorrelation (`NOT EXISTS`/
   `EXISTS` with a **non-equi** correlation) — semantically faithful including the empty-set
   edge case, and exercising anti-/semi-join decorrelation machinery no other case covered
   (existing correlated NOT EXISTS/EXISTS cases correlate on pure equality only).
2. **sq-013** (`dept_id = ANY (subquery)`) — **deleted**. `= ANY (subquery)` is definitionally
   `IN (subquery)`; its only faithful rewrite is a near-verbatim duplicate of already-green
   sq-008. Total corpus size 270 → 269.
3. **sq-016** (doubly-nested correlated subquery skipping a scope level) — rewritten so each
   nesting level correlates exactly one level up (Spark's actual rule), which is genuinely new
   coverage (no other case nests a correlated scalar subquery inside another). The *original*
   level-skipping shape is separately known to expose a latent τ over-permissiveness bug
   (accidental qualifier-fallback mis-binding) — NOT fixed by this rewrite, explicitly flagged
   and tracked in `.agent-output/unsolvable.md` with the original SQL preserved for a future
   Spark-emulated error-parity case.
4. **pv-006** (`stack()` mixing INT/DOUBLE column types) — rewritten with an explicit
   `CAST(age AS DOUBLE)` to unify the stack value slot, preserving the original unpivot intent.
   Remains the corpus's only `stack()` coverage.

All 4 rewrites were verified live against Spark 4.1.1 (non-degenerate row counts, not
vacuously-true/false predicates) before being committed to `sql_corpus.py`, and all 4 pass τ
with **zero new implementation work** — confirming they test real, already-covered machinery
rather than exposing new gaps. Full live-verification transcripts and exact SQL in
`.agent-output/unsolvable.md` (gitignored) and `tasks/v2-corpus-driven-pass-log.md` pass 19.

## Headline mechanisms built this pipeline

- **`QualifierScopes`/`ResolveContext`** (pass 4) — alias→field-range column-qualifier
  binding, reused by nearly every subsequent pass touching multi-relation resolution.
- **`render_alias_transparent_from`** (pass 6) — avoids synthetic wrapper aliases burying
  user-visible qualifiers; the single most-recurring bug class this pipeline hit (pass 7,
  11/13, and 17 each independently rediscovered a variant of "a synthetic wrapper buries a
  qualifier a correlated/positional reference needs").
- **`OuterScope`** (pass 16) — one-level-only correlated-subquery outer-column resolution;
  deliberately has no `outer` field so grandparent-correlation is unrepresentable by
  construction. Reused directly by pass 17's JOIN LATERAL support.
- **`BaseTypes::with_entry`** (pass 18, building on pass 11's `CteScope` precedent) —
  clone+insert whole-table schema injection, used for recursive CTE self-reference
  resolution.
- **`CommonOp::RecursiveCte`** (pass 18) — the final new AST node this pipeline added;
  closes out `WITH RECURSIVE` support with a two-phase anchor-first analyzer and native
  DuckDB `WITH RECURSIVE` emission.

## Recommendation

The SQL corpus is 269/269 (100%) and internally consistent — no case that the reference
Spark session itself cannot execute remains. Future work on the SQL front-end should wait for
new corpus cases that exercise genuinely unimplemented τ surface. One known latent τ bug
remains tracked outside the corpus: sq-016's original (level-skipping) shape exposes an
over-permissive qualifier-resolution fallback in the analyzer; see `.agent-output/unsolvable.md`
for the fix + future error-parity-case follow-up. This pipeline invocation terminates here.
