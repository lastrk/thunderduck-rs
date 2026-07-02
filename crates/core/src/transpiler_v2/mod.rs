//! τ — the Spark → DuckDB transliterator (v2 substrate).
//!
//! ADR-021 (τ owns substrate) + ADR-022 (τ is the only path).
//!
//! At Slice B this module tree carries the type substrate, the stable
//! [`CommonAst`] + [`BaseTypes`] surface, AND the analyzer that turns a
//! `CommonAst` into a [`TypedAst`] with resolved schemas and stamped column
//! types / nullabilities. Emission arms (Slice C) still surface
//! [`EmissionError::UnsupportedOp`] for every successfully-analyzed input —
//! the marker `op` string `<slice-b-analyzer-ok>` signals that the analyzer
//! path landed.

pub mod analyzer;
#[cfg(test)]
pub(super) mod analyzer_fixtures;
pub mod ast;
pub mod base_types;
pub mod error;
pub mod expression;
pub mod invariants;
pub mod type_inference;

pub use analyzer::{
    analyze, has_resolved_schema, AnalyzerError, HasSchema, Schema, SetOpKind, TypedAst, TypedAttr,
    TypedOp,
};
pub use ast::{CommonAst, CommonOp};
pub use base_types::BaseTypes;
pub use error::EmissionError;
pub use expression::Expression;
pub use type_inference::TypeInferenceEngine;

/// τ's top-level entry point.
///
/// **Slice B behavior:** invokes the analyzer via [`analyze`]. Errors from
/// the analyzer surface with their two-category classification preserved
/// (Spark-emulated errors carry the `[SPARK-EMULATED]` Display prefix;
/// Thunderduck-boundary errors carry `[TDCK-BOUNDARY]`). On successful
/// analysis, returns a Thunderduck-boundary
/// `EmissionError::UnsupportedOp { op: "<slice-b-analyzer-ok>", .. }` —
/// Slice C.1 will replace this with real emission dispatch.
pub fn generate(plan: &CommonAst, base_types: &BaseTypes) -> Result<String, EmissionError> {
    let typed = analyzer::analyze(plan.clone(), base_types)
        .map_err(analyzer::analyzer_error_to_emission_error)?;
    // At Slice B the typed plan exists but no emitter runs. The marker
    // string is asserted-on in the mod-level integration test.
    let _ = typed;
    Err(EmissionError::UnsupportedOp {
        op: "<slice-b-analyzer-ok>".to_owned(),
        reason: "τ analyzer succeeded; emission arms land in Slice C".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_single_row_analyzes_then_returns_slice_b_marker() {
        // SingleRow analyzes trivially and returns the Slice B marker
        // `<slice-b-analyzer-ok>` — the anchor that Slice C.1 replaces.
        let plan = CommonAst::new(CommonOp::SingleRow);
        let base_types = BaseTypes::empty();
        let result = generate(&plan, &base_types);
        match result {
            Err(EmissionError::UnsupportedOp { op, .. }) => {
                assert_eq!(op, "<slice-b-analyzer-ok>");
            }
            other => panic!("expected UnsupportedOp(<slice-b-analyzer-ok>), got: {other:?}"),
        }
    }

    #[test]
    fn generate_surfaces_analyzer_error_before_slice_b_marker() {
        // A plan referencing an unknown table must surface the analyzer's
        // Spark-emulated error, not the Slice B marker.
        let plan = CommonAst::new(CommonOp::TableScan {
            table: "no_such_table".to_owned(),
            alias: None,
        });
        let base_types = BaseTypes::empty();
        let result = generate(&plan, &base_types);
        match result {
            Err(EmissionError::UnsupportedExpression { shape, reason }) => {
                assert_eq!(shape, "analyzer-spark-emulated");
                assert!(
                    reason.starts_with("[SPARK-EMULATED]"),
                    "expected `[SPARK-EMULATED]` prefix, got: {reason}",
                );
            }
            other => {
                panic!("expected UnsupportedExpression(analyzer-spark-emulated), got: {other:?}")
            }
        }
    }

    /// Compile-only sanity: `generate()`'s signature accepts `&CommonAst` and
    /// `&BaseTypes`. Fails to compile if the placeholder types come back.
    #[test]
    fn generate_signature_uses_common_ast_and_base_types() {
        fn assert_signature(_f: fn(&CommonAst, &BaseTypes) -> Result<String, EmissionError>) {}
        assert_signature(generate);
    }
}
