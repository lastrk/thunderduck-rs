//! Provenance tracking for external relations (ADR-012, ADR-013, ADR-017).

/// How an external relation was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Bare path-scan. Read-only by construction per [INV9 §CV.5].
    PathScan,
    /// Reached via `ATTACH … TYPE delta|iceberg` or `uc_catalog`. Writable.
    Attached,
}

/// Minimal placeholder for an external relation; [INV9 §CV.5]'s gate needs
/// only `provenance` to decide.
///
/// TODO INV9: replace with the real overlay-backed relation type (ADR-012).
#[derive(Debug)]
pub struct ExternalRelation {
    /// How the relation was reached (see [`Provenance`]).
    pub provenance: Provenance,
    /// Human-readable identifier used in error messages.
    pub name: String,
}

/// Errors emitted by the v2 provenance gate. Local to this submodule so the
/// experimental shape does not leak into [`crate::error`] until ADR-017
/// stabilizes.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// [INV9 §CV.5]: a write was requested against a path-scan relation.
    #[error("relation `{name}` has path-scan provenance and is read-only")]
    ReadOnlyProvenance {
        /// Name of the offending relation.
        name: String,
    },
}

/// Attempt to emit a write command for `rel`. [INV9 §CV.5] requires
/// [`Provenance::PathScan`] produce [`Error::ReadOnlyProvenance`] and
/// [`Provenance::Attached`] succeed (stub today: empty SQL).
///
/// TODO INV9: produce real SQL once ADR-017 lands.
pub fn emit_write(rel: &ExternalRelation) -> Result<String, Error> {
    match rel.provenance {
        Provenance::PathScan => Err(Error::ReadOnlyProvenance {
            name: rel.name.clone(),
        }),
        Provenance::Attached => Ok(String::new()),
    }
}
