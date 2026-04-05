# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 829 passed, 1 failed, 6 skipped (836 total)
**Strict mode**: 829 passed, 6 failed, 1 skipped (836 total)

**History**: 508 → 686 → 695 → 716 → 720 → 723 → 736 → 741 → 744 → 755
→ 779 → 790 → 796 → 802 → 806 → 815 → 819 → 828 → 829

---

## Relaxed Mode Failures (1)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]` — Q14b: column naming

---

## Strict Mode Failures (6)

### Statistical aggregates — nullable (3)

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS SQL — residual decimal/nullable (3)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]` — scalar subquery
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]` — CASE WHEN decimal
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]` — ROLLUP + COALESCE

---

## Summary by Root Cause

| Category | Relaxed | Strict | Notes |
|----------|---------|--------|-------|
| Statistical aggregate nullable | 0 | 3 | kurtosis/skewness always-nullable |
| TPC-DS decimal/subquery | 0 | 3 | Q9 subquery, Q40 CASE WHEN, Q80 ROLLUP |
| Q14b column naming | 1 | 0 | Pre-existing |
