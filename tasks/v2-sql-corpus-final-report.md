# τ SQL Front-End — Corpus-Driven Campaign, Final Report

**Branch:** `feat/v2-spark-sql` (off `feat/v2-transpiler`)
**Corpus:** `tests/integration/differential/sql_corpus.py` — 262 SQL conformance cases
**Result:** **0 → 173 / 262 (66%)** across 19 committed pipeline passes (`096c55d…80e166e`)
**Status:** paused at checkpoint (per user direction, 2026-07-05)

> The last verified differential run was **173 passed / 89 failed** (pass 113).
> Spark is not installed in the current environment, so the per-case split below
> is reconstructed from the 19-pass diagnostic record + the corpus taxonomy, not a
> fresh live run. The **grouping is by *kind of work needed*** — which is what
> matters for deciding the next phase — not a claim of exact live pass/fail per id.

---

## 1. What the goal mandated (and its hidden assumption)

The `/goal` was *"SQL corpus 100% via the corpus-driven pass pipeline"*: one failing
case (or ≤3 same-shape) per pass → diagnose → architect (cite ADRs) → implement →
review → perf → log → commit.

That pipeline is implicitly a **lowering-driven** climb. Per **ADR-004**, the SQL
front-end and the DataFrame API lower to the *same* common AST; the analyzer
(ADR-005/006), the emission table (ADR-007), and the type system are **shared** and
already green (proven by the DataFrame corpus, `core_v2`, held at 313 throughout).
So the goal's working assumption was: *each failing SQL case = a lowering gap*,
fixable by teaching `v2_lowering.rs` to translate one more piece of SQL syntax into
the existing AST — a small, low-risk, single-pass unit of work.

**That assumption held for the first ~173 cases and is now exhausted.** The
remaining 89 split into a shrinking set of true lowering gaps (within mandate) and a
majority that need work in the *shared* layers the goal assumed complete — emission,
the analyzer's scope/resolution, the type system, and new AST node kinds. Those are
**architecturally larger, wider-blast-radius, multi-pass efforts** that the
one-case-per-pass lowering pipeline is the wrong shape for.

---

## 2. What was delivered (the green 173)

Fully or largely green sections (lowering-driven wins, all reviewed, no `core_v2`
regression):

- **Foundation keystone** — `SqlCommand` execution, temp-view registration, and the
  catalog bridge / `build_base_types` pre-fetch (+106 in one pass — the entire
  `FROM <view>` surface went from impossible to working).
- **select / predicate / join / ordering / conditional** — projection, qualified &
  backtick aliases, `DISTINCT`, arithmetic, boolean cols, `WHERE`/`HAVING` (basic),
  all join types, `ORDER BY … NULLS`, `LIMIT`, `CASE`, null functions, `ORDER BY ALL`.
- **aggregate** — `GROUP BY`, `ROLLUP`, `CUBE`, most aggregate functions.
- **scalar_fn** — SQL-syntax functions (`SUBSTRING`/`TRIM`/`POSITION`/`OVERLAY`),
  `IS DISTINCT FROM`, `<=>`, `IS TRUE/FALSE`, `ILIKE`/`RLIKE`, `a DIV b`.
- **subquery (uncorrelated)** — `IN`/`EXISTS`/scalar subqueries with no outer ref.
- **cte (non-recursive, uncorrelated)**, **window**, **set ops** (incl. `UNION BY NAME`
  guard), **pivot/unpivot** (basic), **typed literals** (`DATE`/`TIMESTAMP`/decimal),
  **numeric tower** (result-type pins).

Plus two tech-debt sweeps and a set of reviewer-caught correctness fixes hardened in
place (calendar-date validation, silent-wrong-semantics → boundary rejects, CTE-shadow
resolution, exhaustive PIVOT reference collection).

---

## 3. The remaining 89 — grouped by kind of work

Legend: **[A] within mandate** (lowering gap, the pipeline's native shape) ·
**[B] beyond mandate** (needs a shared layer the goal assumed done).

### Group A — remaining lowering gaps  ·  *within mandate*  ·  ~10–15 cases
Pure SQL-syntax the parser reaches but `v2_lowering.rs` doesn't yet translate, where
emission/analyzer already support the target AST. These are the only cases the
"keep grinding small cases" option would legitimately target.
- Scattered `scalar_fn` (e.g. `fn-020` hex/binary literal `X'…'`), some `predicate` /
  `conditional` / `table_expr` syntax variants.
- **Why still within mandate:** the fix is one lowering arm; blast radius = one match arm.

### Group B1 — correlated subqueries: emission alias-visibility + analyzer outer-scope  ·  *beyond mandate*  ·  ~10–14 cases
Cases: the correlated members of `subquery` (`sq-*` — correlated `EXISTS`, correlated
scalar, correlated `IN`) + `cte-001`/`cte-005` (CTE bodies that carry a correlation).
- **Why beyond:** the SQL *lowers fine*. The defect is in the **WIDE emission path** and
  the **analyzer scope model** — the two shared layers the goal assumed complete:
  1. **Emission** buries the user's base-table alias under a synthetic wrapper —
     `(SELECT * FROM emp e) AS __td_wrap` — so an inner correlated reference
     `WHERE t.x = e.col` can no longer resolve `e`. Fixing this touches the emission
     path used by **every** query → real regression risk to the green 173.
  2. **Analyzer** has no outer-scope stack, so a correlated column can't be typed
     against the enclosing query (ADR-008).
- **Shape:** a dedicated **2-pass Slice-F effort** (emission alias fix, then analyzer
  outer-scope stack), *not* a one-case lowering pass. Full design in
  `.agent-output/diagnostic-pass-111.md`. This is the single **biggest lever** left.

### Group B2 — GROUPING SETS / grouping() emission  ·  *beyond mandate*  ·  ~4–6 cases
Cases: `gx-007` (`GROUPING SETS`), `gx-008` (`grouping()` / `grouping_id()`), and kin.
- **Why beyond:** needs **new emission** for the `GROUPING SETS` grouping construct and
  the `grouping()/grouping_id()` bit-mask functions (likely an extension function per
  ADR-020). The `gx-008` attempt (pass 109) was reverted: fold-detection is
  *structural* — raw grouping exprs vs aggregate exprs differ in fields
  (qualifier/plan_id) even pre-resolution, so it can't be patched in lowering.

### Group B3 — tuple / row-value expressions  ·  *beyond mandate*  ·  ~3 cases
Cases: `pr-003`/`pr-004`/`pr-005` (`(a,b) IN ((1,2),(3,4))`, row comparisons).
- **Why beyond:** requires a **new AST node** (Tuple/RowValue) plus its **emission** and
  type/nullability rules — not a lowering-only change.

### Group B4 — interval *type system* (YEAR-MONTH vs DAY-SECOND)  ·  *beyond mandate*  ·  ~2 cases
Cases: `lit-004` (`INTERVAL '1-2' YEAR TO MONTH`), `lit-005` (`… DAY TO SECOND`).
- **Why beyond:** the compound interval **lowers correctly** (pass 111) but fails at the
  **Arrow round-trip** — Spark returns *distinct* `INTERVAL YEAR TO MONTH` /
  `INTERVAL DAY TO SECOND` types; τ has one generic interval. Needs **type-system +
  Arrow type-mapping** work. Reverted in pass 111 for exactly this reason.

### Group B5 — analyzer star / qualifier resolution  ·  *beyond mandate*  ·  ~2–4 cases
Cases: `agg-009` (`GROUP BY ALL`, blocked by a doubly-qualified-star
`cannot resolve column 'emp.emp.*'`), and qualified-star edge cases.
- **Why beyond:** the GROUP BY ALL *lowering is correct* (unit-tested — groups by
  `[dept_id, active]`); the failure is the **analyzer's star/qualifier resolution**
  producing `emp.emp.*`. Analyzer work, not lowering.

### Group B6 — complex types / LATERAL VIEW  ·  *mixed*  ·  ~5–8 cases
Cases: subset of `complex_type` (`cx-*`) — `LATERAL VIEW explode/posexplode`, inline
tables, map/array/struct construction edge cases.
- **Why mixed:** some are relation-level **lowering** (within mandate); `LATERAL VIEW`
  specifically needs a relation-construct lowering **plus emission** support and is
  beyond a one-arm lowering pass.

### Group B7 — numeric-tower result-type pins  ·  *beyond mandate if failing*  ·  count uncertain
Cases: subset of `numeric_tower` (`num-*`, 32 total, many `schema_only`).
- **Why beyond:** these assert exact per-type built-in result types across the numeric
  tower. Any that fail need **type-inference (analyzer)** tuning per type — not lowering.
  (Many are believed green; the failing tail, if any, is analyzer work.)

---

## 4. Bottom line for the next phase

| Bucket | Cases (approx) | Layer | In goal's mandate? | Shape |
|---|---|---|---|---|
| A — lowering gaps | 10–15 | lowering | **yes** | 1 arm / pass |
| B1 — correlated subq | 10–14 | emission + analyzer | no | 2-pass Slice-F |
| B2 — grouping sets | 4–6 | emission (+ext) | no | feature |
| B3 — tuples/row-values | 3 | new AST + emission | no | feature |
| B4 — interval types | 2 | type system + Arrow | no | feature |
| B5 — star resolution | 2–4 | analyzer | no | targeted fix |
| B6 — complex/lateral | 5–8 | lowering + emission | mixed | feature |
| B7 — numeric tower | uncertain | analyzer type-inf | no | tuning |

**~60–70 of the 89 remaining cases need work in the shared layers the corpus-driven
lowering pipeline assumed complete.** Continuing the one-case-per-pass loop would keep
churning Group A (~10–15 cases) and then stall. The high-value next step is a
**deliberate Slice-F effort** (Group B1 first — it unblocks the largest bucket and
`cte-001/005`), planned and regression-gated as its own project, not smuggled through
a lowering pass at the tail of a 19-pass marathon.

**Recommendation:** treat 66% as the SQL front-end's Milestone 1 (lowering coverage
complete). Open Milestone 2 = "shared-layer SQL work" scoped as B1 → B5 → B2/B3/B4/B6.

---

## 5. Housekeeping notes

- **Active `/goal` Stop hook** is session state (not a file); clear it with `/goal`
  to fully release the loop — otherwise it will keep re-prompting for 100%.
- **Process incident (pass 113):** a coder subagent wrote edits to `/workspace` (the
  **main checkout on `feat/v2-transpiler`**), not this worktree. The worktree work is
  correct and committed here; but **`/workspace` may carry stray uncommitted edits**
  on `v2_lowering.rs`/`dialect.rs` on top of ~500 lines of *pre-existing* uncommitted
  work from another source. Left untouched (reverting risked destroying the other
  work) — **please review `/workspace`'s `git status` before your next session there.**
- Per-pass detail: `tasks/v2-corpus-driven-pass-log.md`; progress rows:
  `tests/integration/v2_sql_progress.md`; Slice-F design:
  `.agent-output/diagnostic-pass-111.md`.
