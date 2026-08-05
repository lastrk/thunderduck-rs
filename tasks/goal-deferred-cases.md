# Goal seed — consolidated deferred cases

Single work-list folded from three ledgers, for a `/goal`-driven session:
- `tasks/transpiler-hardening-deferred.md` (sqlwrap cluster, from the JVM last-25 walk)
- `tasks/pretty-name-parity-deferred.md` (prettyname cluster, same walk)
- `tasks/v2-corpus-followups.md` (corpus-to-100 deferred ledger)

Items are grouped by **shared fix locus** so one pass closes a whole cluster.
Only **open** items are listed; the "Cleared" section of the corpus-followups
ledger is omitted (already fixed). Class legend: `red-case` (failing
differential witness exists), `latent` (correct for the corpus today but wrong
for an unexercised shape — no witness), `hygiene`, `arch` (deferred design, no
red witness).

Suggested order: Cluster 1 → 2 → 3 close real red witnesses / user-visible
parity; 4–6 are latent/hygiene/arch and can follow.

---

## Cluster 1 — Output-column naming parity (`analyzer::pretty_name` + DataFrame naming path)

**Fix locus:** `crates/core/src/transpiler_v2/analyzer.rs` `pretty_name` /
`expression_output_name`; DataFrame-path default-name normalization (mirror the
SQL path's `sparksql_default_select_name`, `v2_lowering.rs:2424`).
**Why one cluster:** every item is the same root symptom — the name τ stamps on
an *unaliased* projection diverges from Spark's `toPrettySQL`; `_compare_schemas`
flags the column-name mismatch.

| id | witness / case | class | source | defect / fix sketch |
|----|----------------|-------|--------|---------------------|
| ~~prettyname-001~~ | ~~`select(when(age>=40,1).otherwise(0))`~~ | ~~red-case~~ | pretty-name doc | Fixed: `CaseWhen` arm added in `analyzer::pretty_name`. |
| ~~prettyname-002~~ | ~~chained `when…when…otherwise`~~ | ~~red-case~~ | pretty-name doc | Fixed: same `CaseWhen` arm covers chained form. |
| ~~prettyname-003~~ | ~~`when` WITHOUT `otherwise` (nullable)~~ | ~~red-case~~ | pretty-name doc | Fixed: same `CaseWhen` arm covers nullable form. |
| prettyname-004 | `select(row_number().over(W.orderBy("id")))` | red-case (deferred) | pretty-name doc | `Window` hits the same `_ => "expr"` arm; Spark `row_number() OVER (…)`. Add a `Window` arm. |
| F-count-distinct-name | — (latent) | latent | P21 (M1) | `pretty_name` ignores `FunctionCall.distinct`: unaliased `count(DISTINCT x)` → `count(x)` vs Spark `count(DISTINCT x)`. Render DISTINCT. |
| F-countstar-name | — (latent) | latent | P21 (M2) | DataFrame `F.count("*")` → `count(*)` vs Spark `count(1)`. DataFrame path lacks the SQL path's `sparksql_default_select_name` normalization. |
| F-upper-fn-name | — (latent) | latent | P21 (M3) | SQL uppercase calls (`SUM(x)`) keep verbatim name → `SUM(x)` vs Spark lowercase `sum(x)`. Lowercase in `lower_function` (`v2_lowering.rs:3413`) or at naming. |

**Acceptance:** `pretty_name` gets real arms for `Window`
(and, ideally, subqueries / complex-type literals it still falls back on);
DISTINCT and `count(*)`/uppercase-fn naming normalized on the DataFrame path.
prettyname-001..003 are now green (CaseWhen arm landed in this PR); prettyname-004 flips green **after golden re-record** once a `Window` arm is added.
Add witnesses for the three latent naming gaps as part of the pass.

## Cluster 2 — Nested `RelType::Sql` leaf routing (`sqlwrap`)

**Fix locus:** `crates/connect-server/src/service.rs` `relation_to_common_ast`
dispatches on the root relation only; `v2_relation_converter.rs` bails on a
nested `RelType::Sql`. Route a nested SQL leaf through `parser_v2` (or intercept
before `V2RelationConverter`), mirroring the JVM `convertSQL` fix.

| id | witness / case | class | source | pins |
|----|----------------|-------|--------|------|
| sqlwrap-001 | `SELECT … FROM v ORDER BY id`.limit(3) | red-case (deferred) | JVM PR#58/#60 (root cause) | base witness — nested `Sql` under `Limit`. |
| sqlwrap-002 | backtick idents under `.limit(3)` | red-case (deferred) | JVM PR#58 | τ never reaches the parser that normalizes backticks. |
| sqlwrap-003 | `WITH c AS (…) SELECT …` under `.limit(3)` | red-case (deferred) | JVM PR#60 | CTE under a wrapper (JVM double-rendered it). |
| sqlwrap-004 | `SELECT …`.filter(id<=3) | red-case (deferred) | JVM PR#58 (adjacent) | gap is not limit-specific — any DataFrame op. |
| sqlwrap-005 | `SELECT …`.offset(2).limit(2) | red-case (deferred) | JVM PR#59/#62 | `Root→Limit→Offset→Sql` plan shape. |
| (optional) DESCRIBE/SHOW/EXPLAIN | metadata stmt, bare + wrapped | red-case (not yet coded) | JVM PR#59/#61/#62 | same nested-`Sql` bail; **needs Spark to record the metadata-shaped golden** before coding. |

**Acceptance:** all five `sqlwrap-*` flip green **after golden re-record**;
optionally add the DESCRIBE/SHOW/EXPLAIN cases once a Spark golden is recordable.

## Cluster 3 — Function emission / semantics correctness

**Fix locus:** `emission.rs` / `type_inference.rs` per item.

| id | witness / case | class | source | defect / fix sketch |
|----|----------------|-------|--------|---------------------|
| F-explode-map | `test_explode_map` | **red-case** | P26 | unaliased `explode(map)` must emit two default cols `key`,`value`; τ's multi-col map-explode expansion (`emission.rs:3850`, `type_inference.rs:829`) fires only on explicit alias. Schema-aware Project pre-pass generator expansion (mirror `expand_json_tuple`/`stack_projections`; dispatch Array=1col vs Map=2cols). |
| F-nondistinct-multicount | — | latent | P20 | non-DISTINCT multi-arg `count(a,b)` (Spark counts rows where all args non-null) still emits invalid DuckDB `count(a,b)`. Only the DISTINCT multi-arg path was fixed. |
| F-json-keys-nonobject | — | latent | P18 | `json_object_keys`→`json_keys` returns `[]` on non-object/non-null JSON where Spark returns NULL (corpus exercises object inputs only). |
| F-negative-emit | — | latent | P19 | `negative`/`negate` has a type-inference arm but NO emission arm → would emit invalid DuckDB `negative(x)`. |
| F-unary-math-nullable | — | latent | P25 | sqrt/cbrt/sin/cos & rest of Spark's `UnaryMathExpression` family share the always-nullable override but aren't in `function_call_nullable`'s always-null arm. |

**Acceptance:** F-explode-map green; each latent item gets a witness + fix.

## Cluster 4 — Hygiene

| id | class | source | note |
|----|-------|--------|------|
| F-dead-macros | hygiene | P17 | `session.rs` macros fully shadowed by emission rewrites: `size`, `array_except`, `array_distinct`, `array_union`, `_spark_reverse`, `_spark_size`. Safe to delete. |

## Cluster 5 — Deferred architecture (no red witness)

| id | class | source | note |
|----|-------|--------|------|
| F-decimal-sum-route | arch | P13 | `sum(Decimal)` left native; strict ADR-020 fidelity would route to shipped `spark_sum`. No red witness. |
| F-orderby-ordinal | arch | P22/P27 | ORDER BY ordinal parity (`ORDER BY <int>`, `ORDER_BY_POS_OUT_OF_RANGE`) — increment 3 of ORDER BY design. Error-parity only; no red witness. |

---

## Goldens — RECORDED authoritative (2026-07-15, live Spark 4.1.1)

The `sqlwrap-*` and `prettyname-*` goldens under
`tests/integration/differential/goldens/dataframe/` have been **re-recorded
from live Apache Spark 4.1.1** in the Linux devcontainer (2026-07-15) — they are
authoritative, not hand-authored. All 9 are **verified born-red** against those
goldens (τ boundary-errors for sqlwrap; `Test='expr'` name mismatch for
prettyname). To regenerate after an input-fixture / Spark-pin change:

```bash
THUNDERDUCK_WORKTREE_ROOT=/workspace ./tests/scripts/run-differential-tests.sh \
  --record core -k "sqlwrap or prettyname"
```

> Devcontainer note: this checkout is a relocated worktree whose `.git` gitfile
> points at an absent main repo, so the harness needs
> `THUNDERDUCK_WORKTREE_ROOT=/workspace` (a graceful fallback was added to
> `tests/integration/utils/test_env.py::worktree_root`). TPC-H/TPC-DS SF0.01
> parquet must exist under `tests/integration/{tpch,tpcds}_sf001/` (the
> `corpus_inputs_*` fixture registers all TPC views for every case);
> auto-gen uses system `python3` which lacks `duckdb`, so generate with the
> venv: `.venv/bin/python3 scripts/generate_tpcds_via_duckdb.py --sf 0.01
> --output tests/integration/tpcds_sf001` (and the analogous DuckDB `dbgen`
> for TPC-H).

Manifests: `tests/integration/sql_wrap_witness_manifest.json`,
`tests/integration/pretty_name_witness_manifest.json` (both `"deferred": true`;
cases are **not** in `select_block_corpus_baseline.txt`, so their redness is not
a regression).

## Explicitly NOT in scope (analyzed, no action)

- **JVM PR#55** (bare-`Cast` column naming) — deliberate divergence *from*
  Spark; τ matches Spark, so it is green (not a bug). Surfaced Cluster 1 instead.
- **JVM PR#57** (delta-as-parquet read guard) — file-read-path behaviour, not a
  Spark-oracle differential witness.
