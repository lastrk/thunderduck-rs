# Differential Test Failure Tracker

**Date**: 2026-04-05
**Relaxed mode**: 830 passed, 0 failed, 6 skipped (836 total) — 100%
**Strict mode**: 835 passed, 0 failed, 1 skipped (836 total) — 100%

**History**: 508 → 686 → 695 → 716 → 720 → 723 → 736 → 741 → 744 → 755
→ 779 → 790 → 796 → 802 → 806 → 815 → 819 → 828 → 829 → 832 → 835

---

## ALL TESTS PASSING

Both relaxed and strict modes achieve 100% pass rate on the differential test suite.

### Key milestones:
- **+178**: CaseWhen `unify_types()` — Spark-compatible type unification
- **+24**: SQL view schema cache + nested struct resolution
- **+11**: Decimal literal precision + mixed arithmetic
- **+12**: Function nullable categories (math always-nullable, isnull non-nullable)
- **+9**: Value-based decimal precision for literals + when() parity
- **+6**: Pivot nullable override in AnalyzePlan
- **+3**: COALESCE type unification + scalar subquery enrichment
- **+3**: kurtosis_pop + spark_skewness extension functions

### Skipped tests:
- **Relaxed**: 6 tests skipped (pre-existing exclusions, not failures)
- **Strict**: 1 test skipped (pre-existing exclusion)
