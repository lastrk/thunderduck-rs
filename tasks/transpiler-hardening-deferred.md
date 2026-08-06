# Transpiler hardening — DEFERRED witnesses from the JVM Thunderduck backlog

Branch `fix/transpiler-hardening`. Deferred corpus witnesses derived by
analyzing the **last 25 commits of the JVM Thunderduck** (`nubank/thunderduck`,
tip `8f15ffbffb`, 2026-05-21 → 2026-05-24) for bugfixes that reproduce in
τ (thunderduck-rs) at the current tip.

## Method

Walked the 25 commits; kept only bugfixes (dropped merges, docs, diagnostics,
the agent-config refactor, and the delta-as-parquet guard). The bugfixes
cluster into one theme — **raw `spark.sql(...)` wrapped by DataFrame ops** —
plus the metadata-statement sub-case:

| JVM commit | PR | Bug |
|---|---|---|
| `bd1e018952` | #58 | Backticked SparkSQL leaks into DuckDB when wrapped by LIMIT/OFFSET (`convertSQL` didn't run the raw SQL through `SparkSQLParser`). |
| `9eabfa7d32` | #60 | Duplicate CTE rendering when `WithCTE` is nested under another operator (`spark.sql("WITH …").limit(N)`). |
| `fdb1a657c1` / `2eb4597134` / `eced7e870d` | #59/#61/#62 | NPE on `DESCRIBE`/`DESC`/`SHOW`/`EXPLAIN` — as a bare metadata statement, via the deprecated `SqlCommand.sql` field, and (execute + analyze) under a `Root → Limit → Offset → SQL` wrapper. |

## Reproduction in τ (code-confirmed)

`crates/connect-server/src/service.rs` `relation_to_common_ast` dispatches on
the **root** relation only: a top-level `RelType::Sql` → `parser_v2`; anything
else → `V2RelationConverter`. The moment `spark.sql(...)` is wrapped by any
DataFrame op the root becomes a structured relation (`Limit`/`Offset`/`Filter`/
… → `Sql`). The converter runs; `convert_limit`/`convert_offset`/
`convert_filter` all recurse through `convert_input`, reach the nested
`RelType::Sql` leaf, and hit
`crates/connect-server/src/converter/v2_relation_converter.rs`:

```rust
RelType::Sql(_) => bail_boundary_proto!(
    "RelType::Sql",
    "SQL text is owned by parser_v2, not V2RelationConverter",
),
```

→ a Thunderduck-boundary error. Spark accepts all of these. So τ has the JVM's
root cause with a **wider blast radius**: the whole nested-SQL shape fails, not
just the three JVM symptoms (backticks / CTE / DESCRIBE). Only a *top-level*
`RelType::Sql` reaches the parser today.

> This analysis and the case additions were done on a **macOS host**; the
> project's toolchain (libduckdb, the `thunderduck-connect-server` binary) and
> the vendored Spark 4.1.1 + venv are **Linux-only** (devcontainer). τ and the
> differential harness could not be executed here, so reproduction is
> established by code inspection (the single `RelType::Sql` bail arm is
> unambiguous), not by a live run.

## Witnesses added (DataFrame corpus, category `sql_wrap`, all DEFERRED)

`tests/integration/differential/dataframe_corpus.py` (see `_sqlwrap` helper).
Pinned in `tests/integration/sql_wrap_witness_manifest.json` (`"deferred": true`).

| case | shape | JVM origin |
|---|---|---|
| `sqlwrap-001` | `SELECT id,name FROM v ORDER BY id` `.limit(3)` | root cause (base witness) |
| `sqlwrap-002` | backtick ident `` `id` `` under `.limit(3)` | #58 backtick leak |
| `sqlwrap-003` | `WITH c AS (…) SELECT …` under `.limit(3)` | #60 duplicate CTE |
| `sqlwrap-004` | `SELECT …` under `.filter(id<=3)` | adjacent — not limit-specific |
| `sqlwrap-005` | `SELECT …` under `.offset(2).limit(2)` | #59/#62 Limit→Offset→Sql shape |

Each is born RED: τ boundary-errors at analyze/execute while Spark returns rows.
They are **not** in `select_block_corpus_baseline.txt`, so the redness is not a
regression.

## PR #55 (bare-Cast column naming) — analyzed, does NOT reproduce

The original walk's Method list did not classify **PR #55** (`092a616a35`,
"Preserve source column name for bare `Cast(col(c), T)` projections"). It has
now been analyzed: it is a **deliberate divergence FROM Spark** in the JVM
(renames a bare `col(c).cast(T)` to `c` instead of Spark's auto-name
`CAST(c AS T)`), and τ's Spark-parity contract means τ already matches Spark
(`pretty_name` → `CAST(c AS BIGINT)`, forced into emission by `ensure_named`).
So there is no Spark-oracle red witness for PR #55 — same category as the
delta-as-parquet guard. Analyzing it DID surface a real adjacent τ gap
(unaliased `CaseWhen`/window projections named `expr`); that is tracked
separately in [`pretty-name-parity-deferred.md`](pretty-name-parity-deferred.md)
(cluster `prettyname-*`).

## Deliberately NOT added as coded cases

- **`DESCRIBE`/`SHOW`/`EXPLAIN` metadata statements** (bare and wrapped, from
  #59/#61/#62). These reproduce too — wrapped forms hit the same nested-`Sql`
  bail, and a bare `spark.sql("DESCRIBE v")` depends on whether `parser_v2`
  lowers the statement at all — but their **reference result is a
  metadata-shaped row set** (Spark's `col_name/data_type/comment` vs DuckDB's
  6-column shape) that cannot be hand-authored with confidence. Add them once
  Spark is available to `--record` an authoritative golden. `sqlwrap-005`
  already pins the exact `Root → Limit → Offset → Sql` plan shape those bugs
  rode in on.

## Goldens — RECORDED authoritative (2026-07-15, live Spark 4.1.1)

The `sqlwrap-*` goldens under
`tests/integration/differential/goldens/dataframe/` were **re-recorded from
live Apache Spark 4.1.1** in the Linux devcontainer (2026-07-15) — they are no
longer hand-authored guesses. To regenerate after an input-fixture or Spark-pin
change:

```bash
THUNDERDUCK_WORKTREE_ROOT=/workspace ./tests/scripts/run-differential-tests.sh \
  --record core -k "sqlwrap"
```

The one previously-uncertain detail is now confirmed authoritative: `id`
through a temp-view + `spark.sql` round-trip is **`long`, non-null** (matching
the earlier `proj-001` assumption).

**Verified born-red (2026-07-15):** golden-mode run of all five cases fails with
τ's `unsupported proto shape RelType::Sql: SQL text is owned by parser_v2, not
V2RelationConverter` — the exact documented root cause, red for the right
reason.

## Acceptance gate (the fix — out of scope here; do NOT implement)

Route a nested `RelType::Sql` leaf through `parser_v2` inside the converter (or
intercept it before `V2RelationConverter`), mirroring the JVM's `convertSQL`
fix, so `spark.sql(...)` wrapped by DataFrame ops lowers the SQL through the
parser instead of bailing. When that lands, all five `sqlwrap-*` cases flip
green (after the golden re-record).
