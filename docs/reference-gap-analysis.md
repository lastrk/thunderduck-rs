# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 830 passed, 0 failed, 6 skipped (836 total) — 100%
**Strict mode**: 829 passed, 6 failed, 1 skipped (836 total) — 99.2%

**History**: 508 → 686 → 695 → 716 → 720 → 723 → 736 → 741 → 744 → 755
→ 779 → 790 → 796 → 802 → 806 → 815 → 819 → 828 → 829

---

## Relaxed Mode: 100% Pass Rate

No failures. Q14b fixed (SchemaOf subquery wrapping).

---

## Strict Mode Failures (6)

### Statistical aggregates — value mismatch (3)

DuckDB uses different kurtosis/skewness formulas than Spark. Requires
`spark_kurtosis`/`spark_skewness` extension functions (not yet implemented).

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS SQL — residual decimal (3)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]` — scalar subquery table scans not enriched
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]` — CASE WHEN decimal in relaxed-mode code path
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]` — ROLLUP + COALESCE precision

---

## Summary

| Category | Count | Fixable in Rust? |
|----------|-------|-----------------|
| kurtosis/skewness algorithm | 3 | No — needs DuckDB extension |
| Scalar subquery enrichment | 1 | Yes — expression-level recursion in enrich_table_scans |
| CASE WHEN decimal (relaxed path) | 1 | Complex — relaxed mode intentionally skips enrichment |
| ROLLUP + COALESCE precision | 1 | Complex — nested type inference gap |
