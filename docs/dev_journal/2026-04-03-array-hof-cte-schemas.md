# Dev Journal — 2026-04-03 — Array containsNull, HOF Types, CTE Schema Propagation

## Summary

Three major fixes addressing strict-mode type inference: (1) array `containsNull` and HOF
return types via lambda schema augmentation (+9 tests), (2) Unpivot ID column nullable
propagation, (3) CTE schema propagation enabling decimal precision CASTs for SQL-path queries
(+9 tests). Extension dependency bumped to `duckdb1.5.1-ext2`.

**Strict mode**: 697 → 716 passed (+19). **Relaxed mode**: 822 (no regressions).

---

## Array containsNull + HOF Return Types (8.4 + 8.5)

**Problem**: Thunderduck defaulted `containsNull=true` for all array types. HOF functions
(`exists`, `forall`, `aggregate`) returned the input array type instead of Boolean/accumulator.
Lambda variables resolved as `Unresolved` because they had no schema binding.

**Fixes**:
- `augment_schema_with_lambda_params()` helper in `TypeInferenceEngine` — binds lambda params
  to array element type so lambda body expressions resolve via schema lookup
- `transform` HOF: `containsNull` = `lambda.body.nullable(&augmented_schema)`
- `filter` HOF: returns first arg's data_type (preserves input containsNull)
- `exists`/`forall` → `BooleanType` in `function_return_type()`
- `aggregate`/`reduce` → accumulator type (init arg); finish lambda resolved if present
- `LambdaVariable.data_type()` → `column_type(&name, schema)` (was `Unresolved`)
- `LambdaVariable.nullable()` → `column_nullable(&name, schema)` (was `false`)
- Lambda param name collision: filter existing schema fields before appending bindings
- HOF nullable rules: transform/filter/exists/forall propagate input, aggregate always nullable

**Review**: Approved after 1 fix iteration (C1: exists/forall nullable propagation, H1:
aggregate finish lambda, M1: function_return_type aliases, M2: param name collision).

---

## Unpivot Nullable Propagation (8.3)

**Problem**: Unpivot `infer_schema()` hardcoded all ID columns as `nullable=true`.

**Fix**: Look up each ID column in input schema via `field_by_name()` to preserve original
type + nullable. Value column nullable = OR of all input value columns' nullable flags.

**Pivot**: Investigated partial schema approach (emit grouping columns only, let DuckDB merge
fill pivot value columns). Caused relaxed-mode regressions (column count mismatch in
`rename_to_spark_schema`). Reverted to `StructType::empty()`.

---

## CTE Schema Propagation (8.1)

**Root cause**: SQL-path queries (TPC-DS) use CTEs heavily. `enrich_table_scans()` didn't
traverse `WithCte` nodes → CTE-referenced TableScans had empty schemas → `apply_agg_type_casts()`
couldn't detect decimal types → no Spark-correct CASTs emitted → DuckDB returned native
precision (e.g., `DECIMAL(38,2)` instead of `DECIMAL(27,2)`).

**Fixes** (all in `relation_converter.rs`):
- `enrich_table_scans()`: added `WithCte` arm to traverse into CTE definitions and body
- `propagate_cte_schemas()`: post-enrichment pass that processes CTE definitions in order,
  infers each CTE's schema via `infer_schema()`, and builds a name-to-schema map. Later CTEs
  see earlier CTE schemas (cascading support for TPC-DS Q2 pattern: `wscs` → `wswscs`).
- `apply_cte_schemas()`: recursive tree walk replacing empty TableScan schemas with
  CTE-derived schemas. Covers all plan variants with child nodes.
- Wired into `convert_sql()` strict-mode branch after `enrich_table_scans()`.

**Result**: +9 strict mode tests. Decimal CASTs now fire for CTE-heavy TPC-DS queries.

---

## Extension Dependency Bump

Updated `duckdb1.5.1-ext1` → `duckdb1.5.1-ext2` in:
- `crates/core/build.rs` (RELEASE_TAG)
- `CLAUDE.md` (2 references)
- `docs/reference-gap-analysis.md` (1 reference)

---

## Test Status

- **Unit tests**: 87 passing (+3 new: exists_returns_boolean, aggregate_returns_init_type,
  augment_schema_adds_lambda_params)
- **Relaxed mode**: 822 passing, 8 pre-existing failures (6 map + Q40 + Q66), 6 skipped
- **Strict mode**: 716 passing, 119 failed, 1 skipped
