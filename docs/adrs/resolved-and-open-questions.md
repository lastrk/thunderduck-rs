# Resolved & Open Questions

## OQ-1 — Raw-SQL `spark.sql("…")` handling — RESOLVED by ADR-004

**Status: RESOLVED.** Raw SQL is parsed by Thunderduck's SparkSQL parser into the common AST (ADR-003) and flows through the same analyzer and τ as DataFrame plans; relation-vs-command is decided by parse-tree root (ADR-004 → ADR-011). Option (a) reject-raw-SQL is rejected (incompatible with the `spark.sql` hard requirement); option (c) string/regex transliteration is rejected (tried in practice, produced an unmaintainable regex chasing dialect mismatches). The chosen option (b) — parse to common AST, single path — gives raw-SQL `SELECT`s the same Spark-parity guarantees as DataFrame `SELECT`s, at the cost of owning a SparkSQL parser (accepted under ADR-000). Front-end agreement between the SparkSQL parser and the protobuf converter is transitive from each front-end's independent Spark-parity validation via ADR-015's oracle.

## OQ-2 — External / lakehouse table WRITE paths — PARTIALLY ADDRESSED; remainder deferred per-format

**Status: partially addressed.** Write paths are being tackled one format/operation at a time, each as its own ADR. **Delta append is now specified by ADR-017** (`INSERT`/append into a pre-existing attached Delta table; one action → one transaction → one version). The remainder are still deferred, each gated on DuckDB's evolving write surface:

- **Delta `DELETE` / `MERGE` / `UPDATE` / overwrite / CTAS-create** — rejected by ADR-017 today (DuckDB-Delta has no delete/update/merge and no confirmed table-creation DDL). Each becomes its own ADR (or an amendment to ADR-017) when the Delta extension ships the corresponding capability; ADR-017 carries the revisit-triggers.
- **Iceberg writes into UC-managed Iceberg** (`CREATE TABLE AS`, `INSERT`, `DELETE`, `MERGE`) — specified and **verified (June 2026)** in **ADR-018** (via the attached UC Iceberg REST endpoint, single-table, merge-on-read deletes). The three drafting-time gates resolved favorably (external-engine writes supported with DuckDB named; write-scoped credential vending; CTAS without pre-creation); residual per-table conditions are merge-on-read mode and DuckDB-supported partitioning. Iceberg writes against *non-UC* REST catalogs (Polaris, Lakekeeper, R2, S3 Tables, Glue) would be a near-identical but less-constrained variant, not yet drafted. The end-to-end stance — read inputs in their native format, write results as Iceberg, both via UC — is now fixed by **ADR-019**, which composes the read path (ADR-013) and the Iceberg write path (ADR-018) and deliberately avoids both Delta writes and cross-format single-table access.
- **Unity-Catalog-managed writes** — follow the format split (UC→Delta via `uc_catalog`; UC→Iceberg via the REST endpoint) and inherit the same per-format maturity, with UC managed-table writes additionally subject to the open `uc_catalog` issues noted earlier.

*No other open questions remain at this time.*
