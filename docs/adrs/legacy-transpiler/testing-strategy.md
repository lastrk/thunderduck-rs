# Testing Strategy

> **SUPERSEDED — DO NOT USE AS GUIDANCE — HISTORICAL REFERENCE ONLY.**
> This ADR describes the retired legacy v1 transpiler. The corresponding Rust modules were deleted on 2026-07-05. Kept in-tree as historical reference only. Active ADR index: [`../README.md`](../README.md).

**Unit tests**: Rust `#[test]` in each module — type inference rules, SQL generation for each plan node and expression type, function registry mappings.

**Differential tests**: The Python pytest framework from the Java reference is imported into `tests/integration/`. The `server_manager.py` is adapted to launch the Rust binary (`target/release/thunderduck-connect-server`) instead of the Java JAR; the differential suites (TPC-H, TPC-DS, joins, aggregations, window functions, etc.) run against the Rust server. Run via `./tests/scripts/run-differential-tests.sh {tpch|all}`.

**v2 testing architecture**: The rearchitected path is validated by the same differential principle but with sharper instrumentation — two separate decision spaces (translation vs resolution) and an AnalyzePlan-based inference oracle that validates type/nullability in isolation. See rearchitect ADR-014 (two decision spaces, failure-attribution buckets) and ADR-015 (differential + AnalyzePlan oracle). The `core_v2` conformance signal is tracked separately via `tests/scripts/v2-progress.sh`.

---

← [Back to ADR Index](../README.md)
