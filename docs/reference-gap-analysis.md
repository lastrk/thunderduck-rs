# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 830 passed, 0 failed, 6 skipped (836 total) — 100%
**Strict mode**: 832 passed, 3 failed, 1 skipped (836 total) — 99.6%

**History**: 508 → 686 → 695 → 716 → 720 → 723 → 736 → 741 → 744 → 755
→ 779 → 790 → 796 → 802 → 806 → 815 → 819 → 828 → 829 → 832

---

## Relaxed Mode: 100% Pass Rate

All 830 tests pass. 6 skipped (pre-existing exclusions).

---

## Strict Mode Failures (3)

All 3 remaining failures require **DuckDB extension functions** — the algorithm
used by DuckDB for kurtosis/skewness differs from Spark's formula. These cannot
be fixed in the Rust port without implementing `spark_kurtosis`/`spark_skewness`
in the C++ DuckDB extension.

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis` — value diff: DuckDB excess kurtosis vs Spark sample kurtosis
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness` — value diff: DuckDB vs Spark sample skewness formula
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped` — same as above with GROUP BY
