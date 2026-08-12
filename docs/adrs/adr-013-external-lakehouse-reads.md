# ADR-013 — External / lakehouse tables (Hive-Parquet, Delta, Iceberg on S3; Unity Catalog) are read by delegating to DuckDB's storage extensions; read-only for this iteration

**Status:** Proposed
**Depends on:** ADR-000, ADR-002, ADR-012
**Depended on by:** ADR-015, ADR-017, ADR-018, ADR-019

**Context.** thunderduck must read tables that do not live in DuckDB's native store: Hive-partitioned Parquet on S3, Delta tables on S3, Iceberg tables on S3, and Unity-Catalog-managed tables. DuckDB already provides mature reader extensions for all of these (`parquet`, `httpfs`/`s3`, `delta`, `iceberg`, and `uc_catalog`), including Unity Catalog support and Databricks catalog-managed-commit handling on the Delta path. Two facts shape the decision. First, per ADR-000/ADR-002, anything DuckDB already does correctly should be delegated, not rebuilt — and these readers, transaction-log handling, and catalog protocols are exactly that. Second, the four targets split into two structurally different access modes, which the overlay (ADR-012) must model. (A prior pass-through-string approach for SQL was abandoned for the regex-monster reasons recorded in ADR-004; that is unrelated here — external tables are reached through structured scans/attachments, not string rewriting.)

**Decision.** thunderduck reads external/lakehouse tables by **emitting the corresponding DuckDB storage-extension surface and delegating all reading, metadata, and catalog-protocol work to those extensions**. It owns only (a) translating a Spark table/catalog reference + storage descriptor + credentials into the correct DuckDB invocation, and (b) reconciling the resolved schema into Spark types in the overlay (ADR-012). It **never** implements a reader, a transaction log, or a catalog protocol (INV8). Unity Catalog specifically is delegated, and routes by table format: **UC→Delta via the `uc_catalog` extension; UC→Iceberg via the Iceberg REST endpoint** (`ATTACH … TYPE iceberg`). **Writes are out of scope for this iteration** and are deferred per-format (see OQ-2); external tables here are read relations only, so they flow through the normal relation path and τ, *not* the command arm (ADR-011).

The two access modes are the central structural content:
- **Path-based (schema-on-read).** Hive-Parquet (`read_parquet(glob, hive_partitioning=true)` over an `s3` secret) and bare Iceberg/Delta paths (`iceberg_scan(...)` / `delta_scan(...)`, or `ATTACH … TYPE delta`). There is no catalog; the schema is discovered from Parquet footers / format metadata plus partition-column inference from the paths. thunderduck must drive a schema-discovery step, and **partition-column typing is the exposed divergence surface** (Spark's partition-type inference has its own rules).
- **Catalog-attached.** Iceberg REST / S3 Tables / Glue / UC-Iceberg (`ATTACH … TYPE iceberg`) and UC-Delta (`uc_catalog`). Namespaces are exposed as schemas and the schema is resolved by the extension; thunderduck consults a real catalog rather than discovering schema from files.

**Consequences.**
- (+) A clean instance of ADR-002's delegation applied to storage: thunderduck adds no reader/format/protocol code, only reference-and-credential translation plus type reconciliation.
- (+) Reads are precisely where DuckDB's lakehouse support is mature, so the delegated surface is solid for this iteration.
- (+) Read-only keeps ADR-011 untouched — external reads are relations through τ, not `emit_command`.
- (−) The type-source seam (ADR-012): the Spark type for a format-backed column must come from the *format's* schema mapped directly to Spark, not from DuckDB's reported scan type, or precision/timezone/nullability can diverge (LB7).
- (−) Version-coordination surface grows: thunderduck is now implicitly sensitive to specific DuckDB storage-extension versions, whose lakehouse behaviour evolves release-to-release (edge to ADR-016).
- (−) Bound to LB7 (DuckDB's readers must be schema- and value-faithful to Spark on the same table) — a bet partly outside thunderduck's control.

**Refinement hooks.** Define, per format, the mapping table from (Spark reference + storage descriptor) → DuckDB invocation (`read_parquet` glob / `iceberg_scan` / `delta_scan` / `ATTACH TYPE iceberg` / `uc_catalog`). Define credential/secret translation (Spark S3/credential config → DuckDB `CREATE SECRET (TYPE s3, …)` or the catalog OAuth2 secret). Define the direct Iceberg-type→Spark-type and Delta-type→Spark-type mappings (ADR-012), validated by reading the same table in reference Spark and diffing the resolved schema (ADR-015). Record access provenance + format per relation in the overlay. Pin/track the DuckDB extension versions as part of ADR-016. Write paths are deferred to OQ-2 and will each become their own ADR.

---

