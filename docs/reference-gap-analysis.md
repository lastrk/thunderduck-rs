# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 828 passed, 2 failed, 6 skipped (836 total)
**Strict mode**: 790 passed, 45 failed, 1 skipped (836 total)

---

## Relaxed Mode Failures (2)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]` — Q11: row count mismatch (decimal literal precision changes HAVING threshold)
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]` — Q14b: column naming issue

---

## Strict Mode Failures (45)

### TPC-H Q11 — row count from decimal HAVING threshold (1 test)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]`

### JSON functions (1 test)

- [ ] `test_json_functions_differential.py::TestJsonConversion_Differential::test_schema_of_json`

### Pivot — nullable from DuckDB fallback (7 tests)

- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_simple`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_with_values`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_multiple_agg`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_avg`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_max_min`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_multiple_groupby`
- [ ] `test_multidim_aggregations.py::TestAdvancedAggregations::test_pivot_then_aggregate`

### Cube/Rollup — nullable mismatch (3 tests)

- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping`
- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping_id`
- [ ] `test_multidim_aggregations.py::TestRollupFunctions::test_rollup_with_grouping`

### Statistical aggregates — nullable mismatch (3 tests)

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS SQL — decimal precision/scale + nullable (17 tests)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[5]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[12]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[20]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[27]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[47]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[53]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[57]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[58]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[61]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[63]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[70]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[77]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[86]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[89]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[98]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14a]`

### TPC-DS DataFrame — decimal precision (11 tests)

- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q12_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q20_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q62_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q98_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q99_dataframe`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[12]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[20]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[40]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[62]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[98]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[99]`

---

## Summary by Root Cause

| Category | Relaxed | Strict | Notes |
|----------|---------|--------|-------|
| TPC-DS/TPC-H decimal precision | 1 | ~28 | CTE + expression-level gaps |
| Pivot grouping nullable | 0 | 7 | Pivot schema → DuckDB fallback |
| Cube/Rollup grouping nullable | 0 | 3 | GROUPING/GROUPING_ID semantics |
| Statistical aggregate nullable | 0 | 3 | kurtosis/skewness |
| Column naming | 1 | 0 | Q14b |
| JSON schema_of_json | 0 | 1 | Return type |
