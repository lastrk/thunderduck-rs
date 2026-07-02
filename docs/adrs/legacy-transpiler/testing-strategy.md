# Testing Strategy

> **Status: existing implementation — runs behind `--transpiler legacy` (the default).** The authoritative v2 architecture supersedes this file where they conflict; the two paths coexist, so do not delete the legacy path to make room for v2. This file's v2 successor is listed in the legacy→v2 map in [`../README.md`](../README.md); the v2 spine is [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Unit tests**: Rust `#[test]` in each module — type inference rules, SQL generation for each plan node and expression type, function registry mappings.

**Differential tests**: The Python pytest framework from the Java reference is imported into `tests/integration/`. The `server_manager.py` is adapted to launch the Rust binary (`target/release/thunderduck-connect-server`) instead of the Java JAR; the differential suites (TPC-H, TPC-DS, joins, aggregations, window functions, etc.) run against the Rust server. Run via `./tests/scripts/run-differential-tests.sh {tpch|all}`.

**v2 testing architecture**: The rearchitected path is validated by the same differential principle but with sharper instrumentation — two separate decision spaces (translation vs resolution) and an AnalyzePlan-based inference oracle that validates type/nullability in isolation. See rearchitect ADR-014 (two decision spaces, failure-attribution buckets) and ADR-015 (differential + AnalyzePlan oracle). The `core_v2` conformance signal is tracked separately via `tests/scripts/v2-progress.sh`.

---

← [Back to ADR Index](../README.md)
