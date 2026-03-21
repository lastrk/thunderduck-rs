# Dev Journal — 2026-03-21 — Phase 5: StatCrosstab, StatFreqItems, StatSampleBy

**Date**: 2026-03-21
**Branch**: main
**Status**: Phase 5 complete — differential tests: **669 passing, 0 failing**

---

## Summary

Implemented the three remaining Phase 5 statistical plan variants:
`StatCrosstab`, `StatFreqItems`, and `StatSampleBy`. These were the last three
failing differential tests. The implementation follows the same 3-file pattern
established for `StatCov`/`StatCorr`/`ApproxQuantile` in Phase 4.

All 75 unit tests continue to pass.

---

## 1. LogicalPlan additions — `crates/core/src/logical/mod.rs`

### New enum variants

```rust
StatCrosstab(StatCrosstab),
StatFreqItems(StatFreqItems),
StatSampleBy(StatSampleBy),
```

### New structs

```rust
pub struct StatCrosstab {
    pub input: Box<LogicalPlan>,
    pub col1: String,
    pub col2: String,
}

pub struct StatFreqItems {
    pub input: Box<LogicalPlan>,
    pub cols: Vec<String>,
    pub support: f64,   // default 0.01 if not supplied by proto
}

pub struct StatSampleBy {
    pub input: Box<LogicalPlan>,
    pub col_expr: Expression,
    pub fractions: Vec<(Literal, f64)>,
    pub seed: Option<i64>,
}
```

### `infer_schema` arms

| Variant | Schema |
|---|---|
| `StatCrosstab` | `StructType::empty()` — pivot columns depend on runtime data |
| `StatFreqItems` | One `Array<String>` field per input col, named `"{col}_freqItems"` |
| `StatSampleBy` | Delegates to `s.input.infer_schema()` — same schema as input |

---

## 2. Converter additions — `crates/connect-server/src/converter/relation_converter.rs`

Three new arms in `convert()` before the `_` catch-all:

```rust
Some(RelType::Crosstab(c))  => self.convert_stat_crosstab(c),
Some(RelType::FreqItems(f)) => self.convert_stat_freq_items(f),
Some(RelType::SampleBy(s))  => self.convert_stat_sample_by(s),
```

`convert_stat_sample_by` extracts stratum literals from
`stat_sample_by::Fraction` using `self.expr_conv.convert_literal()`, then
pattern-matches to unwrap `Expression::Literal`.

---

## 3. Generator additions — `crates/core/src/generator/mod.rs`

### `gen_stat_crosstab`

DuckDB dynamic PIVOT. Auto-discovers distinct `col2` values (no `IN` clause).
Outputs rows sorted by `col1`, with the combined `col1_col2` header column first.

```sql
SELECT c1 AS "col1_col2", * EXCLUDE (c1)
FROM (
  PIVOT (
    SELECT CAST("col1" AS VARCHAR) AS c1, CAST("col2" AS VARCHAR) AS c2
    FROM <input>
  ) ON c2 USING COUNT(*) GROUP BY c1
) _crosstab
ORDER BY c1
```

**Key gotcha**: DuckDB rejects `FROM PIVOT (...)` directly; the `PIVOT` clause
must be wrapped in a subquery: `FROM (PIVOT (...)) alias`.

### `gen_stat_freq_items`

CTE-based. One correlated subquery per column filters to values appearing in
at least `support * total_count` rows, then returns them as a sorted list.

```sql
WITH _stat_input AS (SELECT * FROM <input>)
SELECT
  (SELECT LIST("col" ORDER BY "col") FROM (
    SELECT "col", COUNT(*) AS cnt FROM _stat_input AS _inner
    WHERE "col" IS NOT NULL GROUP BY "col"
    HAVING COUNT(*) >= 0.01 * (SELECT COUNT(*) FROM _stat_input AS _total)
  ) AS _freq) AS "col_freqItems"
```

### `gen_stat_sample_by`

Stratified sampling via `RANDOM()`. Each stratum gets its own `col = val AND
RANDOM() < fraction` condition OR-ed together. Seed is injected as a
side-effecting scalar subquery — `setseed()` returns `NULL` so `IS NULL` is
always true and adds no filtering:

```sql
SELECT * FROM <input> AS _stat_input
WHERE (SELECT setseed(0.123456)) IS NULL AND (col = 'a' AND RANDOM() < 0.5 OR ...)
```

`seed.rem_euclid(1_000_000)` normalises the seed to `[0, 1)` regardless of
sign, matching DuckDB's `setseed` contract.

---

## Test status

- Core unit tests: **75/75** passing
- Release binary: builds clean
- Differential tests: **669 passing, 0 failing**
  - `test_describe_numeric_columns` passes in isolation; occasionally flaky under
    full-suite load due to server contention — pre-existing, unrelated to this work
