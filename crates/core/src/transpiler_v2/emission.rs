//! Declarative emission table (ADR-009) — INV3's "single source of truth".
//! Also hosts the external-emit enumeration that INV8 walks.

/// The dispatch table that maps analyzed AST nodes to DuckDB SQL emission.
///
/// [INV3 §CV.5] requires this be the **sole** generation+coverage artifact.
/// The unit-struct shape today reserves the symbol; the
/// [`super::invariants::inv3_emission_table_single_source_of_truth`] test
/// asserts no sibling module declares a competing table.
///
/// TODO INV3: replace with the real declarative table per ADR-009.
#[derive(Debug, Default)]
pub struct EmissionTable;

/// A specific external-table emission path, enumerated by [INV8 §CV.5]'s
/// allow-list (see ADR-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalEmit {
    /// `read_parquet(...)` path-scan.
    ReadParquet,
    /// `iceberg_scan(...)` path-scan.
    IcebergScan,
    /// `delta_scan(...)` path-scan.
    DeltaScan,
    /// `ATTACH … TYPE iceberg` attachment.
    AttachIceberg,
    /// Unity Catalog (`uc_catalog`) attachment.
    UcCatalog,
}

/// Every v2 emit path classified as "external".
///
/// **Stub.** Empty today; the [`super::invariants::inv8_external_access_is_delegated`]
/// test loops over the empty slice and passes vacuously, but the closed
/// set of legal kinds is encoded in [`ExternalEmit`].
///
/// TODO INV8: populate as ADR-013 emit paths land.
pub fn external_emit_paths() -> &'static [ExternalEmit] {
    &[]
}

/// `Extension(name)` targets the dispatch table declares. [INV6 §CV.5]
/// requires every name resolve to a function exported by `thdck_spark_funcs`.
///
/// **Stub.** Empty today; entries are added as ADR-010 extension targets
/// are wired into the table.
///
/// TODO INV6: populate as ADR-010 extension targets are declared.
pub fn extension_targets() -> &'static [&'static str] {
    &[]
}
