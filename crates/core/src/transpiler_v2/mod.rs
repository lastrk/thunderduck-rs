//! τ — the Spark → DuckDB transliterator (v2 substrate).
//!
//! ADR-021 (τ owns substrate) + ADR-022 (τ is the only path).
//!
//! This module tree carries the type substrate, the stable [`CommonAst`] +
//! [`BaseTypes`] surface, the analyzer that turns a `CommonAst` into a
//! [`TypedAst`] with resolved schemas and stamped column types /
//! nullabilities, AND the emission substrate — [`emission::dispatch_op`]
//! turns a [`TypedAst`] into a DuckDB SQL string. Every [`CommonOp`] variant
//! is wired end-to-end (leaves, Project/Filter/Sort/Limit, Aggregate incl.
//! Rollup/Cube/GroupingSets, Join, SetOp, WithColumns, NA family, Pivot /
//! Crosstab, Stat family, TableFunction, ...) except `Unnest`, whose emission
//! arm still returns a Thunderduck-boundary `EmissionError` per ADR-022;
//! shapes τ has not implemented surface the same way.

pub mod analyzer;
#[cfg(test)]
pub(super) mod analyzer_fixtures;
pub mod ast;
pub mod base_types;
pub mod emission;
pub mod error;
pub mod expression;
pub mod invariants;
pub mod macros;
pub mod rewrites;
pub(crate) mod spark_errors;
pub mod statement;
mod struct_names;
pub mod type_inference;

pub use analyzer::{
    analyze, has_resolved_schema, AnalyzerError, Schema, SetOpKind, TypedAst, TypedOp,
};
pub use ast::{CommonAst, CommonOp};
pub use base_types::BaseTypes;
pub use error::EmissionError;
pub use expression::Expression;
pub use statement::{DdlStatement, SqlStatement};
pub use type_inference::TypeInferenceEngine;

/// τ's top-level entry point.
///
/// **τ's emission substrate behavior:** invokes the analyzer via [`analyze`], then
/// dispatches through [`emission::dispatch_op`]. Errors from the analyzer
/// surface with their two-category classification preserved (Spark-emulated
/// errors carry the `[SPARK-EMULATED]` Display prefix; Thunderduck-boundary
/// errors carry `[TDCK-BOUNDARY]`). Emission errors for un-wired arms are
/// Thunderduck-boundary per ADR-022.
pub fn generate(plan: &CommonAst, base_types: &BaseTypes) -> Result<String, EmissionError> {
    generate_with_schema(plan, base_types).map(|(sql, _schema)| sql)
}

/// τ's fused emit-and-schema entry point.
///
/// Runs [`analyze`] once and returns BOTH the emitted DuckDB SQL AND the
/// analyzer's root `resolved_schema`. Used by the Spark Connect ExecutePlan
/// streaming-query path in `connect-server::service::execute_streaming_query`
/// so it can drive the post-execute Arrow-schema stamp (see
/// `connect-server::arrow_schema_stamp`) without re-running the analyzer.
///
/// The lone-`analyze`-plus-dispatch shape is identical to [`generate`], so
/// error semantics (Spark-emulated vs Thunderduck-boundary, per ADR-022) are
/// preserved bit-for-bit. Callers that don't need the schema should stay on
/// [`generate`]; callers that only need the schema should use
/// [`analyze_schema`]. The three entry points share the same analyzer.
pub fn generate_with_schema(
    plan: &CommonAst,
    base_types: &BaseTypes,
) -> Result<(String, crate::types::StructType), EmissionError> {
    let typed = analyzer::analyze(plan.clone(), base_types)
        .map_err(analyzer::analyzer_error_to_emission_error)?;
    let sql = emission::dispatch_op(&typed.op, &typed.resolved_schema)?;
    Ok((sql, typed.resolved_schema))
}

/// τ's schema-analyze entry point.
///
/// Runs [`analyze`] and returns the resolved schema of the root node. Used by
/// the Spark Connect `AnalyzePlan(Schema)` RPC — clients call this after
/// `createDataFrame(...)` / `.select(...)` to discover column names + types
/// before scheduling `ExecutePlan`. Errors surface via the same
/// two-category-preserving [`analyzer_error_to_emission_error`] bridge that
/// [`generate`] uses.
pub fn analyze_schema(
    plan: &CommonAst,
    base_types: &BaseTypes,
) -> Result<crate::types::StructType, EmissionError> {
    let typed = analyzer::analyze(plan.clone(), base_types)
        .map_err(analyzer::analyzer_error_to_emission_error)?;
    Ok(typed.resolved_schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler_v2::error::UnsupportedKind;

    #[test]
    fn generate_single_row_emits_subquery_safe_select() {
        // SingleRow emits `SELECT 1` (one row, one column). Callers that use
        // SingleRow as a subquery input (Project) wrap as
        // `SELECT expr FROM (SELECT 1) __td_proj` — DuckDB accepts this. A
        // bare `SELECT` would fail inside the subquery wrapper. See
        // `emission::render_single_row` for the analyzer-schema note.
        let plan = CommonAst::new(CommonOp::SingleRow);
        let base_types = BaseTypes::empty();
        let sql = generate(&plan, &base_types).expect("SingleRow should dispatch");
        assert_eq!(sql, "SELECT 1");
    }

    #[test]
    fn generate_surfaces_analyzer_error_not_pipeline_marker() {
        // A plan referencing an unknown table must surface the analyzer's
        // Spark-emulated error, not the τ's analyzer marker.
        let plan = CommonAst::new(CommonOp::TableScan {
            table: "no_such_table".to_owned(),
            alias: None,
        });
        let base_types = BaseTypes::empty();
        let result = generate(&plan, &base_types);
        match result {
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::Expression,
                name,
                reason,
            }) => {
                assert_eq!(name, "analyzer-spark-emulated");
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
