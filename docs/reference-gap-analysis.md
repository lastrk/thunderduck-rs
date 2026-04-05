# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 828 passed, 2 failed, 6 skipped (836 total)
**Strict mode**: 802 passed, 33 failed, 1 skipped (836 total)

**History**: 508 → 686 (CaseWhen) → 695 (struct) → 716 (array/HOF/CTE) → 720 (agg delegation)
→ 723 (map fix) → 736 (nullable categories) → 741 (map type) → 744 (bin/flatten) → 755 (interval/map)
→ 779 (view cache/nested struct) → 790 (decimal mixed arith) → 796 (ROLLUP/GROUPING/div guard)
→ 802 (pivot nullable/when unification)

---

## Relaxed Mode Failures (2)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]` — Q11: row count (decimal HAVING threshold)
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]` — Q14b: column naming

---

## Strict Mode Failures (33)

### TPC-H Q11 — row count (1 test)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]`

### JSON (1 test)

- [ ] `test_json_functions_differential.py::TestJsonConversion_Differential::test_schema_of_json`

### Multidim aggregations — pivot/cube/rollup (4 tests)

- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping`
- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping_id`
- [ ] `test_multidim_aggregations.py::TestRollupFunctions::test_rollup_with_grouping`
- [ ] `test_multidim_aggregations.py::TestAdvancedAggregations::test_pivot_then_aggregate`

### Statistical aggregates (3 tests)

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS DataFrame (6 tests)

- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[12]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[20]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[40]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[62]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[98]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[99]`

### TPC-DS SQL (15 tests)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[5]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[12]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[20]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[47]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[53]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[57]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[61]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[63]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[77]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[89]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[98]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14a]`

### TPC-DS DataFrame (via old test file, 3 tests)

- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q62_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q98_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q99_dataframe`

---

## Summary by Root Cause

| Category | Relaxed | Strict | Notes |
|----------|---------|--------|-------|
| TPC-DS decimal precision cascades | 1 | ~21 | CTE + expression type inference gaps |
| Cube/Rollup/Pivot nullable | 0 | 4 | GROUPING semantics, pivot_then_aggregate |
| Statistical aggregate nullable | 0 | 3 | kurtosis/skewness |
| Column naming | 1 | 0 | Q14b |
| JSON schema_of_json | 0 | 1 | Return type |
| TPC-H Q11 row count | 0 | 1 | Decimal HAVING threshold |
