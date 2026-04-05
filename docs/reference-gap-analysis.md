# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 828 passed, 2 failed, 6 skipped (836 total)
**Strict mode**: 806 passed, 28 failed, 1 skipped (836 total)

**History**: 508 → 686 → 695 → 716 → 720 → 723 → 736 → 741 → 744 → 755
→ 779 → 790 → 796 → 802 → 806

---

## Relaxed Mode Failures (2)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]` — Q11: row count (decimal HAVING threshold)
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]` — Q14b: column naming

---

## Strict Mode Failures (28)

### TPC-H Q11 — row count (1)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]`

### JSON (1)

- [ ] `test_json_functions_differential.py::TestJsonConversion_Differential::test_schema_of_json`

### Cube/Rollup/Pivot — nullable (4)

- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping`
- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping_id`
- [ ] `test_multidim_aggregations.py::TestRollupFunctions::test_rollup_with_grouping`
- [ ] `test_multidim_aggregations.py::TestAdvancedAggregations::test_pivot_then_aggregate`

### Statistical aggregates — nullable (3)

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS SQL — decimal precision (8)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[12]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[20]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[58]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[61]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[98]`

### TPC-DS DataFrame — decimal precision (11)

- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[12]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[20]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[40]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[62]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[98]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[99]`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q12_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q20_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q62_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q98_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q99_dataframe`

---

## Summary by Root Cause

| Category | Relaxed | Strict | Notes |
|----------|---------|--------|-------|
| TPC-DS decimal precision (SQL + DF) | 0 | 19 | Residual CTE/window/subquery type gaps |
| Cube/Rollup/Pivot nullable | 0 | 4 | pivot_then_aggregate, GROUPING semantics |
| Statistical aggregate nullable | 0 | 3 | kurtosis/skewness |
| TPC-H Q11 row count | 1 | 1 | Decimal HAVING threshold |
| Q14b column naming | 1 | 0 | Pre-existing |
| JSON schema_of_json | 0 | 1 | Return type |
