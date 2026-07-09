# v2 Corpus-Driven Pipeline — Final Report

**Corpus:** `tests/integration/differential/sql_corpus.py` (`sql_v2`, 270 `spark.sql` cases) —
the fitness function for τ's SQL front-end, per `CLAUDE.md` / ADR-022.

**Final state: 265/270 (98.1%).** This is the achievable ceiling for the corpus as currently
authored — the remaining 5 cases are confirmed invalid Spark SQL on the pinned Spark 4.1.1
reference and cannot pass the differential oracle regardless of τ implementation work.

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

Full per-pass root cause / fix / ADR citations in `tasks/v2-corpus-driven-pass-log.md`.
Every pass ran the complete diagnostician → architect → coder → reviewer → perf-reviewer
loop with zero DEFERred Medium/Low findings, and zero regressions on either corpus
(`sql_v2` or the 329-case DataFrame `core_v2` corpus) at every step.

## The 5 confirmed-unsolvable cases

Documented in full in `.agent-output/unsolvable.md` (gitignored working-tree artifact —
summarized here since that file is not committed):

1. **sq-011** — `salary > ALL (subquery)` — Spark 4.1.1 rejects quantified-comparison
   subquery predicates at parse (`PARSE_SYNTAX_ERROR`); Spark's `ALL`/`ANY`/`SOME` apply
   only to array expressions, not subqueries.
2. **sq-012** — `salary > ANY (subquery)` — same class as sq-011.
3. **sq-013** — `dept_id = ANY (subquery)` — same class as sq-011.
4. **sq-016** — doubly-nested correlated subquery skipping a scope level
   (`e3`'s subquery references the outermost `e`, not the immediate parent `e2`) — Spark's
   own correlation resolution reaches only one level up; the reference session itself
   throws `UNRESOLVED_COLUMN.WITH_SUGGESTION`. (Separately noted: τ is currently MORE
   permissive than Spark on this exact shape — an accidental-permissiveness latent bug,
   tracked but out of scope since it produces no corpus-visible symptom.)
5. **pv-006** — `stack(2, 'age', age, 'salary', salary)` mixing an INT and a DOUBLE column
   in the same output slot — Spark 4.1.1's analyzer itself rejects this with
   `DATATYPE_MISMATCH.STACK_COLUMN_DIFF_TYPES`.

In every case the REFERENCE Spark session throws before τ is ever exercised or compared —
these are corpus-authoring issues (assuming SQL-standard or type-lenient behavior Spark
4.1.1 does not actually have), not τ gaps. A human maintainer should either delete these
5 cases or rewrite them to valid Spark-accepted equivalents; doing so is out of scope for
a transpiler-implementation pipeline.

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

No further corpus-witnessed fixable cases remain. Future work on the SQL front-end should
either (a) wait for new corpus cases that exercise genuinely unimplemented τ surface, or
(b) have a human correct the 5 mis-authored cases above so the ceiling can rise past
265/270. This pipeline invocation terminates here.
