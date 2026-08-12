# ADR-012 — thunderduck owns a narrow catalog overlay; commands write it, resolution reads it

**Status:** Proposed
**Depends on:** ADR-011 (writers), ADR-005 (reader/seed) — see the ADR-005/012 cycle in §CV
**Depended on by:** ADR-005, ADR-013, ADR-015

**Context.** Resolution (ADR-005) needs Spark types of base columns to seed inference. Commands (ADR-011) mutate what relations later resolve against. A full Spark shadow catalog is more than needed, since DuckDB's catalog handles existence and binding.

**Decision.** thunderduck maintains a *narrow* overlay of Spark types on base relations, consulted by the inference pass and updated by commands. It is shared state between the command arm and the relation arm. DuckDB's catalog continues to handle existence and binding; the overlay carries only the Spark-semantics facts the divergent slice needs.

Command-side cache mutation is a closed `SchemaCacheEffect` attached optionally to a batch command. The session thread applies that effect exactly once, only after DuckDB accepts the statement, so catalog and overlay updates share the connection's serialization boundary.

The overlay additionally records, per base relation, its **access provenance** — `native` (a DuckDB-store table), `path-scan` (a Hive-Parquet / bare Delta or Iceberg path), or `attached-catalog` (an Iceberg-REST / Unity-Catalog attachment) — and, for the non-native cases, the **format** (Parquet / Delta / Iceberg / UC). Provenance is load-bearing because it determines *both* the emission surface (ADR-013) and the **type-source**: for `native` relations the overlay maps DuckDB catalog types → Spark types; for format-backed relations the source of truth is the *format's own* schema (Iceberg and Delta each have their own type systems), which should be mapped **directly** to the Spark type Spark would assign when reading that table, not laundered through a DuckDB-type intermediate that may not round-trip precision/timezone/nullability faithfully.

**Consequences.**
- (+) Minimality again — own only the catalog facts the divergent slice requires.
- (−) This overlay is the seam where the Spark-type ↔ DuckDB-type mapping lives and where command/relation synchronization must be kept consistent; the most insidious resolution-divergence bugs will hide here.
- (−) Format-backed relations introduce a *second* type-source (the format schema) and a direct format-type → Spark-type mapping distinct from the DuckDB-type mapping — more mapping tables to maintain and validate.
- (neutral) Establishes the differential-harness precondition: identical Spark base-column types pinned on both engines (ADR-015).

**Refinement hooks.** Decide whether the overlay is materialized alongside DuckDB's catalog or computed by mapping DuckDB types back to Spark types on read. Define the Spark↔DuckDB type mapping table explicitly, and separately the direct Iceberg-type→Spark-type and Delta-type→Spark-type mappings for format-backed relations (ADR-013). Specify how commands keep overlay and DuckDB catalog in sync. Record access provenance + format per relation.

**`BaseTypes` overlay is fallback-only for the analyzer's read side.** Rule: `resolve_table_scan` reads the analyzed AST's own `TableScan.schema` field first; the `BaseTypes` overlay is consulted only when the AST-carried schema is unresolved (empty or all-`Unresolved`). Concretely: `resolve_table_scan(&TableScan, &BaseTypes) -> Result<Schema>` returns AST schema if non-empty; else BaseTypes lookup by name; else `AnalyzerError::UnknownTable`. Request-handler seeding of `BaseTypes` short-circuits — if no `TableScan` in the plan has an empty `schema`, `build_base_types_from_plan` returns an empty overlay without walking the session catalog. Rationale: keeps the analyzer's tree-local reasoning intact (schemas travel with nodes) while still supporting deferred session-catalog resolution for scans the front-end couldn't schema-thread. The command-arm write contract (this ADR's core) is unaffected — the rule scopes only the analyzer's read side.

---

