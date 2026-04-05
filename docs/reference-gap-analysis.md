# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 828 passed, 2 failed, 6 skipped (836 total)
**Strict mode**: 819 passed, 16 failed, 1 skipped (836 total)

**History**: 508 → 686 → 695 → 716 → 720 → 723 → 736 → 741 → 744 → 755
→ 779 → 790 → 796 → 802 → 806 → 815 → 819

---

## Relaxed Mode Failures (2)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]` — Q11: row count
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]` — Q14b: column naming

---

## Strict Mode Failures (16)

### TPC-H Q11 — row count (1)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]`

### Pivot — nullable (1)

- [ ] `test_multidim_aggregations.py::TestAdvancedAggregations::test_pivot_then_aggregate`

### Statistical aggregates — nullable (3)

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS SQL — decimal precision/nullable (10)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[66]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[67]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[83]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[93]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[23a]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[23b]`

### TPC-H DataFrame (1)

- [ ] `test_tpch_differential.py::TestTPCHDifferential::test_q11_dataframe`

---

## Summary by Root Cause

| Category | Relaxed | Strict | Notes |
|----------|---------|--------|-------|
| TPC-DS decimal precision/nullable | 0 | 10 | Residual CTE/subquery type gaps |
| Statistical aggregate nullable | 0 | 3 | kurtosis/skewness |
| TPC-H Q11 row count | 1 | 2 | Decimal HAVING threshold |
| Q14b column naming | 1 | 1 | Pre-existing |
| Pivot nullable | 0 | 1 | pivot_then_aggregate |
