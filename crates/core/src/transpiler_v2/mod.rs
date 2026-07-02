//! τ — the Spark → DuckDB transliterator (v2 substrate).
//!
//! ADR-021 (τ owns substrate) + ADR-022 (τ is the only path).
//!
//! At Slice C.1 this module tree carries the type substrate, the stable
//! [`CommonAst`] + [`BaseTypes`] surface, the analyzer that turns a
//! `CommonAst` into a [`TypedAst`] with resolved schemas and stamped column
//! types / nullabilities, AND the emission substrate — [`emission::dispatch_op`]
//! turns a [`TypedAst`] into a DuckDB SQL string for the wired operator arms
//! (`SingleRow`, `TableScan`, `Values`, `LocalRelation`, `FileScan`, `Project`,
//! `Filter`, `Sort`, `Limit`). Unwired arms (`Aggregate`, `Join`, `SetOp`,
//! `TableFunction`, `Unnest`) return Thunderduck-boundary `EmissionError`s per
//! ADR-022.

pub mod analyzer;
#[cfg(test)]
pub(super) mod analyzer_fixtures;
pub mod ast;
pub mod base_types;
pub mod emission;
pub mod error;
pub mod expression;
pub mod invariants;
pub mod rewrites;
pub mod type_inference;

pub use analyzer::{
    AnalyzerError, HasSchema, Schema, SetOpKind, TypedAst, TypedAttr, TypedOp, analyze,
    has_resolved_schema,
};
pub use ast::{CommonAst, CommonOp};
pub use base_types::BaseTypes;
pub use error::EmissionError;
pub use expression::Expression;
pub use type_inference::TypeInferenceEngine;

/// τ's top-level entry point.
///
/// **Slice C.1 behavior:** invokes the analyzer via [`analyze`], then
/// dispatches through [`emission::dispatch_op`]. Errors from the analyzer
/// surface with their two-category classification preserved (Spark-emulated
/// errors carry the `[SPARK-EMULATED]` Display prefix; Thunderduck-boundary
/// errors carry `[TDCK-BOUNDARY]`). Emission errors for un-wired arms are
/// Thunderduck-boundary per ADR-022.
pub fn generate(plan: &CommonAst, base_types: &BaseTypes) -> Result<String, EmissionError> {
    let typed = analyzer::analyze(plan.clone(), base_types)
        .map_err(analyzer::analyzer_error_to_emission_error)?;
    emission::dispatch_op(&typed.op, &typed.resolved_schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_single_row_emits_select() {
        // Slice C.1: SingleRow now dispatches through emission and returns
        // the real SQL string. The prior `<slice-b-analyzer-ok>` marker is
        // retired.
        let plan = CommonAst::new(CommonOp::SingleRow);
        let base_types = BaseTypes::empty();
        let sql = generate(&plan, &base_types).expect("SingleRow should dispatch");
        assert_eq!(sql, "SELECT");
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
