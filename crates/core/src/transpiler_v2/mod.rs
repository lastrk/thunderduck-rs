//! Rearchitected transpiler (`τ`) — work in progress.
//!
//! This module is the home of the principled Spark → DuckDB transliterator
//! described in [`docs/thunderduck-rearchitect-ADRs.md`]. It is being built
//! *alongside* the existing [`crate::generator`] path and is selected at
//! startup via [`TranspilerPath`] (CLI `--transpiler v2` /
//! `THUNDERDUCK_TRANSPILER=v2`); the default remains the legacy path so
//! existing behavior is unchanged until a caller opts in.
//!
//! The eventual pipeline is: common AST (ADR-003/004) → type & nullability
//! analyzer (ADR-005/006) → declarative emission table (ADR-009) → DuckDB SQL.
//! Today [`generate`] is a stub that returns [`ThunderduckError::Unsupported`].

use crate::error::{Result, ThunderduckError};
use crate::logical::LogicalPlan;

/// Which transpiler path a request is routed to. Set once at startup.
///
/// Resolution: a CLI flag wins, otherwise [`TranspilerPath::from_env`] is
/// consulted, defaulting to [`TranspilerPath::Legacy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranspilerPath {
    /// The existing [`crate::generator::SqlGenerator`] path (default).
    #[default]
    Legacy,
    /// The rearchitected path in this module (not yet implemented).
    V2,
}

impl TranspilerPath {
    /// Parse a path selector from a string (case-insensitive).
    ///
    /// `"v2"` selects [`TranspilerPath::V2`]; everything else — including
    /// `"legacy"`, the empty string, and any unrecognized value — selects
    /// [`TranspilerPath::Legacy`], so the safe default is the existing path.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "v2" => TranspilerPath::V2,
            _ => TranspilerPath::Legacy,
        }
    }

    /// Read `THUNDERDUCK_TRANSPILER` from the environment.
    /// Defaults to [`TranspilerPath::Legacy`] when unset or unrecognized.
    pub fn from_env() -> Self {
        Self::parse(&std::env::var("THUNDERDUCK_TRANSPILER").unwrap_or_default())
    }
}

/// Translate a [`LogicalPlan`] into DuckDB SQL via the rearchitected pipeline.
///
/// **Stub.** The v2 pipeline (ADR-003 → ADR-010) is not yet implemented; this
/// returns [`ThunderduckError::Unsupported`] so a request that opts into the
/// v2 path fails loudly rather than silently using the legacy path.
pub fn generate(_plan: &LogicalPlan) -> Result<String> {
    Err(ThunderduckError::Unsupported(
        "transpiler v2 (rearchitecture path) is not yet implemented; \
         use --transpiler legacy (the default) or unset THUNDERDUCK_TRANSPILER"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_selects_v2_only_for_v2_token() {
        assert_eq!(TranspilerPath::parse("v2"), TranspilerPath::V2);
        assert_eq!(TranspilerPath::parse("V2"), TranspilerPath::V2);
        assert_eq!(TranspilerPath::parse("  v2  "), TranspilerPath::V2);
    }

    #[test]
    fn parse_defaults_to_legacy() {
        assert_eq!(TranspilerPath::parse(""), TranspilerPath::Legacy);
        assert_eq!(TranspilerPath::parse("legacy"), TranspilerPath::Legacy);
        assert_eq!(TranspilerPath::parse("garbage"), TranspilerPath::Legacy);
        assert_eq!(TranspilerPath::default(), TranspilerPath::Legacy);
    }

    #[test]
    fn generate_is_unimplemented() {
        let plan = LogicalPlan::SingleRow(crate::logical::SingleRowRelation);
        let err = generate(&plan).unwrap_err();
        assert!(matches!(err, ThunderduckError::Unsupported(_)));
    }
}

pub mod analyzer;
pub mod ast;
pub mod emission;
pub mod provenance;

#[cfg(test)]
mod invariants;

/// Reviewed exception list for [INV2 §CV.5]: every τ decision that is not
/// node-local after the analyzer pass MUST appear here under a stable name.
///
/// Empty today; entries are added by the rearchitecture work in ADR-007
/// (B-layer structural transliterations) and ADR-009 (C escape hatches).
/// The [`invariants::inv2_node_local_or_labeled_escape_hatch`] test asserts
/// every entry is non-empty and unique.
///
/// TODO INV2: populate as ADR-007/ADR-009 land specific escape hatches.
pub const C_ESCAPE_HATCHES: &[&str] = &[];

/// Runtime tap used by the differential harness to record the post-serialize
/// payloads sent to each engine, satisfying [INV1 §CV.5]
/// (serialize-once-send-twice). The harness sets one tap at suite startup.
///
/// **Stub.** No serializer exists in v2 today, so the tap stores nothing and
/// the [`invariants::inv1_both_engines_receive_byte_identical_input`] test
/// reads an empty payload list. The tap's *signature* is fixed now so the
/// future serializer can plug in without churning the test.
///
/// TODO INV1: wire to the v2 serializer once ADR-015's harness lands.
pub fn set_serializer_tap(_tap: fn(&[u8])) {
    // intentional no-op stub
}
