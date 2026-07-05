# τ SQL Front-End — Corpus-Driven Campaign, Final Report

**Branch:** `feat/v2-spark-sql` (off `feat/v2-transpiler`)
**Corpus:** `tests/integration/differential/sql_corpus.py` — 262 SQL conformance cases
**Result:** **0 → 173 / 262 (66%)** across 19 committed pipeline passes (`096c55d…80e166e`)
**Status:** paused at checkpoint (per user direction, 2026-07-05)

> **Numbers verified live.** Ran against reference Spark 4.1.1 on 2026-07-05:
> `sql_v2` = **173 passed / 89 failed / 262**; `core_v2` (DataFrame) = **313 / 324**,
> both exactly on baseline. The 89 failing case IDs below are **ground truth**, not
> reconstruction. **Correction:** an earlier draft estimated the per-category split
> *without* a live Spark run and got several clusters wrong — most importantly it
> called joins and qualified-reference selects "delivered green" when **joins are
> 2/16 (essentially red)** and basic qualified-ref/aliased selects fail. This
> version is corrected against the live run.

---

## 1. What the goal mandated (and its hidden assumption)

The `/goal` was *"SQL corpus 100% via the corpus-driven pass pipeline"*: one failing
case (or ≤3 same-shape) per pass → diagnose → architect (cite ADRs) → implement →
review → perf → log → commit.

That pipeline is implicitly a **lowering-driven** climb. Per **ADR-004**, the SQL
front-end and the DataFrame API lower to the *same* common AST; the analyzer
(ADR-005/006), the emission table (ADR-007), and the type system are **shared** and
green *for the DataFrame surface* (`core_v2` held at 313 throughout). The working
assumption was: *each failing SQL case = a lowering gap*, a small single-pass unit.

**That assumption was only partly right, and the live run shows why.** Many SQL
constructs exercise emission/analyzer paths the DataFrame corpus never reaches —
so "shared and green" did not transfer. The 89 remaining failures are dominated by
whole clusters (joins, correlated subqueries, table expressions) that need
emission/analyzer work, not one-line lowering arms.

---

## 2. What is actually green (173) — by section

Live per-section pass counts (green / total):

| Section | Green | Notes |
|---|---|---|
| **ordering** | **12 / 12** | fully green (incl. `ORDER BY ALL`) |
| **window** | **16 / 16** | fully green |
| numeric_tower | 26 / 32 | most per-type result pins hold |
| scalar_fn | 17 / 20 | SQL-syntax fns, `IS DISTINCT FROM`, `<=>`, `ILIKE`/`RLIKE`, `DIV` |
| predicate (WHERE) | 15 / 16 | |
| aggregate | 12 / 20 | GROUP BY / ROLLUP / CUBE + common aggregates |
| select | 12 / 16 | **4 fail** — see below |
| conditional | 10 / 12 | |
| setop | 9 / 10 | incl. `UNION BY NAME` guard |
| complex_type | 7 / 14 | |
| subquery | 7 / 22 | only the **uncorrelated** ones |
| predicate_adv | 6 / 10 | |
| cte | 5 / 10 | non-recursive, uncorrelated |
| group_ext | 5 / 10 | plain GROUP BY / ROLLUP / CUBE (not GROUPING SETS) |
| typed_literal | 5 / 10 | |
| **table_expr** | **3 / 10** | mostly red |
| **join** | **2 / 16** | **essentially red** (only `jn-007`, `jn-016` pass) |

Plus the foundation keystone that made any of this possible: `SqlCommand`
execution, temp-view registration, and the catalog bridge (`build_base_types`) —
without which every `FROM <view>` case failed. Two tech-debt sweeps; reviewer-caught
correctness fixes (calendar-date validation, silent-wrong-semantics → boundary
rejects, CTE-shadow resolution, exhaustive PIVOT reference collection).

---

## 3. The 89 failures — ground-truth grouping

Legend: **[diagnosed]** root cause known · **[undiagnosed]** cluster confirmed
failing but not yet root-caused (owning layer is a hypothesis).

### Joins — 14  ·  *undiagnosed, highest priority*
`jn-001..006, jn-008..015` (every join type: INNER/LEFT/RIGHT/FULL/CROSS/NATURAL/
SEMI/ANTI, multi-condition, non-equi, three-way, self, join-then-aggregate). Only
`jn-007`/`jn-016` pass. **This is the single biggest surprise** and the biggest
lever. Hypothesis: the SQL front-end's join lowering or **qualified-reference /
table-alias emission** is systematically wrong (the alias-visibility defect flagged
in `.agent-output/diagnostic-pass-111.md`, which also breaks correlated subqueries
and qualified selects). Needs diagnosis before sizing — likely one or two root
causes behind all 14.

### Subqueries — 15  ·  *[diagnosed]* correlated / quantified
`sq-003,004,006,007,010,015,016,017,018,019,021,022` (correlated scalar/EXISTS/IN/
HAVING/nested, TPC-H Q17/Q18 shapes) + `sq-011,012,013` (`> ALL` / `> ANY` / `= ANY`
quantified). Needs the emission alias-visibility fix + analyzer outer-scope stack
(ADR-008); quantified forms need `ALL`/`ANY` desugaring. Ties in `cte-001/005/006`.

### Aggregate — 8  ·  *mixed*
`agg-007` GROUP BY expression, `agg-008` GROUP BY ordinal, `agg-009` GROUP BY ALL
(lowering correct, blocked by star resolution — *[diagnosed]*), `agg-010/011`
HAVING-on-aggregate (emission), `agg-017` aggregate `FILTER (WHERE)` (spark4),
`agg-018` `collect_list`/`collect_set`, `agg-019` `percentile`/`median`.

### Table expressions — 7  ·  *mixed*
`tbl-001` inline `VALUES`, `tbl-002` VALUES→join, `tbl-005` LATERAL derived table,
`tbl-006` `range()` TVF, `tbl-007` `explode()` TVF, `tbl-008` broadcast hint,
`tbl-010` required subquery alias. Spans top-level VALUES lowering, table functions,
LATERAL, hint handling.

### Complex types — 7  ·  *mixed*
`cx-001` array literal+access, `cx-002` map literal+access, `cx-004` struct field
path, `cx-007/008/009` `LATERAL VIEW explode/outer/posexplode`, `cx-011` explode map.
LATERAL VIEW needs relation-construct lowering + emission.

### Numeric tower — 6  ·  *[diagnosed]* result-type pins
`num-001` ceil→bigint, `num-002/003` ceil/floor→decimal, `num-005` round/bround
decimal precision, `num-008` signum type, `num-012` mod type. Type-inference /
emission result-type parity per numeric type.

### Group extensions — 5  ·  *[diagnosed]* GROUPING SETS emission
`gx-003/004` GROUPING SETS, `gx-007` ROLLUP+HAVING, `gx-008` CUBE(3), `gx-010`
Hive `WITH ROLLUP`. Needs GROUPING SETS emission + `grouping()/grouping_id()`
(the `gx-008` fold-detection attempt in pass 109 was reverted — structural).

### CTE — 5  ·  *mixed*
`cte-001/005/006` (correlation tie-in, see Subqueries), `cte-009/010` recursive CTE.

### Typed literals — 5  ·  *[diagnosed]* interval types
`lit-004/005` INTERVAL Y-M / D-S (lower correctly, fail Arrow round-trip: Spark
distinct interval types vs τ's generic — reverted pass 111), `lit-006`
`make_interval`, `lit-008` timestamp−timestamp→interval, `lit-009` string escape.

### Advanced predicates — 4  ·  *[diagnosed]* tuples / quantified LIKE
`pr-003` LIKE ANY, `pr-004` LIKE ALL, `pr-005` multi-column IN (new Tuple/RowValue
AST node + emission), `pr-007` lateral column alias (spark4).

### Select — 4  ·  *[diagnosed]* qualified-ref / naming
`sel-003` qualified star, `sel-013` qualified column refs, `sel-015` table alias
(all qualified-reference — same suspected root cause as joins), `sel-008`
unaliased-expression output column naming (Spark-vs-DuckDB column-name parity).

### Scalar functions — 3
`fn-017` round/abs/ceil/floor, `fn-018` int/int→double, `fn-020` binary `X'..'`/hex.

### Remaining singles/pairs — 6
`cnd-002` simple CASE (expr WHEN val), `cnd-009` `IF()`, `pv-002` PIVOT count,
`pv-006` `stack()`, `set-009` 3-way UNION ALL, `whr-007` BETWEEN.

---

## 4. Bottom line

| Cluster | Cases | Status | Owning layer (hypothesis where undiagnosed) |
|---|---|---|---|
| Joins | 14 | undiagnosed | SQL join lowering / qualified-ref emission — **diagnose first** |
| Correlated + quantified subqueries | 15 | diagnosed | emission alias fix + analyzer outer-scope (ADR-008) |
| Aggregate (mixed) | 8 | mixed | GROUP BY expr/ordinal lowering; HAVING/FILTER emission; agg fns |
| Table expressions | 7 | mixed | VALUES lowering; TVFs; LATERAL; hints |
| Complex types / LATERAL VIEW | 7 | mixed | literal access lowering; LATERAL VIEW emission |
| Numeric tower | 6 | diagnosed | type-inference/emission result-type pins |
| GROUPING SETS family | 5 | diagnosed | new emission (+ `grouping()` extension) |
| CTE (correlated + recursive) | 5 | mixed | ties to subqueries; recursive CTE |
| Interval types | 5 | diagnosed | type system + Arrow mapping |
| Tuples / quantified LIKE | 4 | diagnosed | new AST node + emission |
| Qualified-ref / naming selects | 4 | diagnosed | qualified-ref emission; column-name parity |
| Scalar / conditional / pivot / setop / predicate | 9 | mixed | assorted lowering + emission |

**~30 of the 89 are diagnosed** (correlated/quantified subqueries, GROUPING SETS,
intervals, tuples, numeric pins, GROUP BY ALL/star, qualified-ref selects). **~59
are in clusters that need diagnosis** — above all the **join cluster (14)**, whose
root cause is unknown and which alone would move the number substantially.

**Recommendation:** the next phase is *not* the one-case-per-pass lowering loop.
Start by **diagnosing the join cluster** (14 cases, likely 1–2 root causes in
qualified-reference/alias emission — which may also unlock the qualified-ref selects
and correlated subqueries). Then the correlated-subquery emission fix (ADR-008),
then GROUPING SETS / interval-type / tuple features. Treat 66% as SQL front-end
Milestone 1 (partial lowering coverage); Milestone 2 = shared-layer SQL work.

---

## 5. Housekeeping

- **Active `/goal` Stop hook** is session state; clear it with `/goal` to release the loop.
- Per-pass detail: `tasks/v2-corpus-driven-pass-log.md`; progress rows:
  `tests/integration/v2_sql_progress.md`; subquery emission design:
  `.agent-output/diagnostic-pass-111.md`.
- Live regression check (2026-07-05): the slice-taxonomy purge + deferred-comment
  strip + the feat/v2-transpiler cleanup merge introduced **no regressions** —
  `core_v2` 313, `sql_v2` 173.
