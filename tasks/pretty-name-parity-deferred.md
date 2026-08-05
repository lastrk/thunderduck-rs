# Unaliased projection auto-naming (`toPrettySQL` parity) — DEFERRED witnesses

Deferred corpus witnesses derived from the **last 25 commits of the JVM
Thunderduck** (`nubank/thunderduck`, tip `8f15ffbffb`) — the same walk that
produced the `sqlwrap-*` cluster (see
[`transpiler-hardening-deferred.md`](transpiler-hardening-deferred.md)). This
doc covers the one bugfix that walk did **not** classify: **PR #55**
(`092a616a35`, "Preserve source column name for bare `Cast(col(c), T)`
projections"), and the real τ gap that analyzing it surfaced.

## PR #55 does NOT reproduce in τ

The JVM fix reinterprets an un-aliased `df.select(col("c").cast(T))` — which
PySpark Spark Connect sends as a bare `Cast` proto with no surrounding `Alias`
— as if the user had written `.alias(c)`, so the output column is named `c`
instead of Spark Catalyst's auto-name `CAST(c AS T)`. The commit message is
explicit that this is a **deliberate divergence from Spark** (Spark's
pretty-name is a footgun when the projection feeds a parquet write and the
name becomes the on-disk column).

τ (thunderduck-rs) does **not** have this bug, because τ's whole contract is
Spark parity (the differential oracle IS Spark):

- `crates/core/src/transpiler_v2/analyzer.rs` `pretty_name` renders a bare
  `Cast` as `CAST(<child> AS <TYPE>)` with the UPPERCASE Catalyst type
  spelling — matching Spark's `Cast.sql` / `toPrettySQL` exactly
  (`CAST(age AS BIGINT)`).
- `ensure_named` (wired into `Project` / `Aggregate` output lists) wraps every
  non-`Star`, non-bare-reference entry in a named `Alias` using that same
  name, so emission emits `CAST(...) AS "CAST(age AS BIGINT)"` and DuckDB
  returns that exact column name.

So a bare-cast projection is **green** against the Spark oracle — τ matches
Spark, which is the opposite horn of the JVM's intentional divergence. There
is no red witness to add for PR #55 itself. (This is the same category as the
delta-as-parquet guard from PR #57: a JVM behaviour that is not a
Spark-oracle divergence in τ.)

## The real adjacent gap PR #55 surfaced

`pretty_name` carries an explicit **Thunderduck-boundary fallback**. Its own
doc-comment:

> Variants Spark renders in a shape τ does not yet match exactly (`CaseWhen`,
> windows, subqueries, complex-type literals, …) keep the Thunderduck-boundary
> fallback name `"expr"`.

`CaseWhen`, `Window`, and `ScalarSubquery` are distinct `Expression` variants
with **no `pretty_name` arm** — they fall through to `_ => "expr"`. When a
projection of one of these shapes is **unaliased**, τ names the output column
`expr`, while Spark's `toPrettySQL` names it `CASE WHEN ... END` /
`row_number() OVER (...)` / etc.

The differential harness compares column names:
`tests/integration/utils/dataframe_diff.py` `_compare_schemas` reports a
`name mismatch` as a hard schema failure. So the divergence is
corpus-witnessable — but every existing `cond-*` / `win-*` case aliases its
projection (`.alias(...)`, or `withColumn(name, ...)` which always names the
column), so **this unaliased auto-naming path was untested**.

## Reproduction in τ (code-confirmed)

`analyzer.rs` `pretty_name`:

```rust
// Variants Spark renders in a shape τ does not yet match exactly (`CaseWhen`,
// windows, subqueries, complex-type literals, …) keep the Thunderduck-
// boundary fallback name `"expr"`.
fn pretty_name(expr: &Expression) -> String {
    match expr {
        // … ColumnReference / Literal / Binary / Unary / FunctionCall / Cast / Star / ExtractValue …
        _ => "expr".to_owned(),   // ← CaseWhen / Window / ScalarSubquery land here
    }
}
```

`ensure_named` names an unaliased projection entry via
`expression_output_name` → `pretty_name`, so a `select(when(...).otherwise(...))`
output column is named `expr`. Spark names it `CASE WHEN ... END`. Schema-name
mismatch → red.

> This analysis ran on a host with **no runnable τ/Spark** (no vendored Spark
> 4.1.1, no `thunderduck-connect-server` binary), so reproduction is
> established by code inspection (the `_ => "expr"` fallback and the
> `_compare_schemas` name check are both unambiguous), not by a live run.

## Witnesses added (DataFrame corpus, category `pretty_name`, all DEFERRED)

`tests/integration/differential/dataframe_corpus.py` (section 32c). Pinned in
`tests/integration/pretty_name_witness_manifest.json` (`"deferred": true`).

| case | shape | τ name | Spark `toPrettySQL` name |
|---|---|---|---|
| `prettyname-001` | `select(when(age>=40,1).otherwise(0))` | `expr` | `CASE WHEN (age >= 40) THEN 1 ELSE 0 END` |
| `prettyname-002` | chained `when...when...otherwise` | `expr` | `CASE WHEN (age < 30) THEN 0 WHEN (age < 45) THEN 1 ELSE 2 END` |
| `prettyname-003` | `when` WITHOUT `otherwise` (nullable) | `expr` | `CASE WHEN active THEN salary END` |
| `prettyname-004` | `select(row_number().over(W.orderBy("id")))` | `expr` | `row_number() OVER (...)` |

Each is born RED: the output column name diverges (τ `expr` vs Spark's
pretty-name) and `_compare_schemas` fails before any row comparison. They are
**not** in `select_block_corpus_baseline.txt`, so their redness is not a
regression. `001`–`003` cover the `CaseWhen` fallback; `004` covers the
`Window` fallback (a second shape hitting the same `_ => "expr"` arm).

## Goldens — RECORDED authoritative (2026-07-15, live Spark 4.1.1)

The `prettyname-*` goldens under
`tests/integration/differential/goldens/dataframe/` were **re-recorded from
live Apache Spark 4.1.1** in the Linux devcontainer (2026-07-15). The
hand-authored column names turned out to be exactly right — the recording
additionally captured Spark's `"__autoGeneratedAlias": "true"` field metadata
and confirmed `prettyname-004`'s full frame clause
(`row_number() OVER (ORDER BY id ASC NULLS FIRST ROWS BETWEEN UNBOUNDED
PRECEDING AND CURRENT ROW)`). To regenerate:

```bash
THUNDERDUCK_WORKTREE_ROOT=/workspace ./tests/scripts/run-differential-tests.sh \
  --record core -k "prettyname"
```

**Verified born-red (2026-07-15):** golden-mode run of all four cases fails with
`Column 0: name mismatch - Reference='CASE WHEN …', Test='expr'` — the exact
documented `pretty_name` `_ => "expr"` gap, red for the right reason. (Row order
is immaterial: the differential compares row multisets order-insensitively, so a
no-`orderBy` window/CASE case is red purely on the column name.)

## Acceptance gate (the fix — out of scope here; do NOT implement)

Give `pretty_name` real arms for `CaseWhen` (`CASE WHEN <c> THEN <v> … [ELSE
<e>] END`), `Window` (`<fn>(<args>) OVER (<spec>)`), and the other
currently-`expr` shapes, matching Spark's `toPrettySQL`. When that lands, all
four `prettyname-*` cases flip green (after the golden re-record).
