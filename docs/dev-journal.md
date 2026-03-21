# Development Journal

---

## 2026-03-21 — Section 3 Generator Correctness Gaps

**Differential tests**: 669 → 670 passing, 0 failing
**Unit tests**: 75/75

Closed four medium-priority generator correctness gaps from `docs/reference-gap-analysis.md`
Section 3. All changes are in `crates/core/src/generator/mod.rs`.

### GROUPING/GROUPING_ID return type

DuckDB's `grouping()` and `grouping_id()` aggregate functions return `INTEGER`. Spark returns
`TINYINT` and `BIGINT` respectively. Added early-return arms at the top of the
`gen_function_call` match block (same pattern as `like`/`ilike`/`rlike`/`in`) that wrap the
call in an explicit `CAST(... AS TINYINT)` / `CAST(... AS BIGINT)`.

### DECIMAL SUM/AVG CASTs

`cast_integer_sum` previously only handled integer SUM → BIGINT. Extended to:

- `SUM(Decimal{p,s})` → `CAST(SUM(...) AS DECIMAL(min(p+10, 38), s))` — Spark widens
  precision by 10 for SUM of decimals.
- `AVG(integer)` → `CAST(AVG(...) AS DOUBLE)` — Spark AVG on integers returns DOUBLE.
- `AVG(Decimal{p,s})` → `CAST(AVG(...) AS DECIMAL(min(p+4, 38), s+4))` — Spark widens
  precision by 4 and scale by 4 for AVG of decimals.

Both aggregate paths in `gen_aggregate` (default order and `select_order`) already call
`cast_integer_sum`, so AVG is covered by extending the function.

### Union type widening CASTs

Schema inference on `Union` already promotes numeric types via `TypeInferenceEngine::promote_numeric`.
The generator was not emitting corresponding CASTs, so DuckDB would infer its own (potentially
mismatched) column types.

`gen_union` now checks both side schemas: when any column type pair differs, it wraps each
side in `SELECT CAST(col AS target_type) AS col, ... FROM (...)` using the promoted type as
the target. Unresolved types are left uncasted to avoid breaking schema-unknown paths. Column
name aliases (`AS col`) are mandatory — without them DuckDB names the column after the CAST
expression and downstream `ORDER BY col` breaks.

First attempt omitted the `AS col` alias, causing 5 new failures in `TestUnionTypeCoercion`
(`Referenced column "id" not found in FROM clause`). Fixed before merge.

### ROLLUP/CUBE NULLS FIRST

Spark always sorts ROLLUP/CUBE subtotal rows (which have NULL in the grouping columns) with
`NULLS FIRST`. DuckDB's default varies. `gen_sort` now detects when the base input (after
peeling any Filter nodes via `extract_filters`) is an `Aggregate` with `GroupingSets::Rollup`
or `GroupingSets::Cube`, and forces `NullOrdering::NullsFirst` on all sort orders for that
sort node.

Note: the proto expression converter already maps `SORT_NULLS_UNSPECIFIED` to `NullsFirst`, so
this fix only matters when a sort explicitly carries `NullsLast` over a ROLLUP/CUBE result.

### Deferred: Auto-alias unaliased expressions

The plan also listed "auto-alias unaliased expressions" as a Section 3 gap. This is marked
**deferred** — adding `AS "spark_name"` aliases to complex projection expressions risks
renaming columns that all 670 differential tests currently accept, with no dedicated test
coverage to catch regressions. Address when test coverage for this path is added.

### Doc update

`docs/reference-gap-analysis.md`: moved all four implemented gaps and the previously-stale
"Distinct column subset" entry from Section 3 to Section 1 (closed gaps). Marked
auto-alias as deferred in Section 3 with rationale. Section 5 priority table updated.
