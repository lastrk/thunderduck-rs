# SQL corpus pipeline — final report (passes 95–100)

**Result: SQL corpus 0 → 137 / 262 (52%).** Terminated at the pass-100 cap (not
100%). Branch `feat/v2-spark-sql`, pipeline start `096c55d`.

## What this run delivered

The τ SQL front-end (`spark.sql(...)`) went from **completely non-functional**
(every query failed before execution) to **52% of a 262-case conformance
corpus** green — spanning projection, predicates, joins, aggregates, ordering,
conditionals, scalar functions, the numeric tower, set operations, window
functions, and ROLLUP/CUBE.

| Pass | Case | Layer | Δ | Result |
| --- | --- | --- | --- | --- |
| 95 | sel-007 | connect-server | +2 | SqlCommand execution (lazy echo) — `spark.sql()` runs at all |
| 96 | sel-002 | connect-server + τ base_types | +106 | Temp-view registration + catalog bridge (the keystone) |
| 97 | set-001 | parser_v2 | +6 | Set operations (UNION/INTERSECT/EXCEPT/MINUS) lowering |
| 98 | win-001 | parser_v2 | +18 | Window functions + interval literals lowering |
| 99 | — | connect-server | 0 | Tech-debt sweep (helper extraction, stale-comment fixes) |
| 100 | gx-001 | parser_v2 | +5 | ROLLUP / CUBE lowering |

**0 → 2 → 108 → 114 → 132 → 137.** Progress trend: `tests/integration/v2_sql_progress.md`.

## Architecture observations

- **The catalog bridge (pass 96) was the keystone** — one pass greened 106 cases.
  Almost all table-backed SQL was blocked on two analyzer stubs (view registration
  + the `|_| None` catalog closure), and the session already had the machinery.
- **Most passes were lowering-only.** Per ADR-004 (SQL and DataFrame lower to the
  same common AST), the analyzer + emission substrate for set ops, windows,
  intervals, and ROLLUP/CUBE was already built and green via the DataFrame corpus.
  The SQL front-end just had to stop rejecting the syntax and construct the same
  `CommonOp`/`Expression`. Net τ-core change was tiny (one additive
  `empty_scan_tables` helper); all new code is in `parser_v2/v2_lowering.rs` and
  `connect-server/src/service.rs`.
- **Loud-fail discipline held (ADR-022).** Every deferred shape (GROUPING SETS,
  nested ROLLUP terms, UNION BY NAME, unrepresentable intervals, GROUPS frames,
  unresolved named windows) returns a Thunderduck-boundary error rather than a
  silent wrong result. Two reviewer High/Medium findings were exactly this class,
  fixed in-pass.

## The remaining 130 (roadmap, not blockers)

Fully enumerated in `.agent-output/unsolvable.md`. Nothing is unsolvable; each is
a known feature with a clear path:
- **Lowering gaps** (highest ROI next): the `sql::expr::other` grab-bag (~20 —
  `<=>`, IS [NOT] DISTINCT FROM, IS TRUE/FALSE, multi-col IN, RLIKE), CTEs (~9),
  DATE/TIMESTAMP + compound interval literals (~7), DISTINCT, GROUP BY/ORDER BY ALL.
- **Two analyzer-nullability fixes** (~5-6 cases): set-op DISTINCT and ROLLUP/CUBE
  grouping-key nullability — likely one shared fix.
- **Larger substrate work**: subqueries (~19, ADR-008), GROUPING SETS, PIVOT/UNPIVOT,
  LATERAL VIEW, WITH ROLLUP dialect.

## How to run

`./tests/scripts/run-differential-tests.sh sql_v2` (or `tests/scripts/v2-sql-progress.sh`
to record a trend row). Per-pass detail in `tasks/v2-corpus-driven-pass-log.md`
(passes 95–100).
