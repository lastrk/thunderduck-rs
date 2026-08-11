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
//! Crosstab, Stat family, TableFunction, ...) except `Unnest`, which the
//! analyzer rejects as a Thunderduck-boundary `PuntedOperator` per ADR-022;
//! shapes τ has not implemented surface the same way.

pub mod analyzer;
#[cfg(test)]
mod analyzer_fixtures;
pub mod ast;
pub mod base_types;
pub mod emission;
pub mod error;
pub mod expression;
pub mod function_catalog;
pub mod generator;
pub mod identifier;
pub mod invariants;
pub mod macros;
/// Re-export, not a module: [`name_fold`](crate::types::name_fold) moved to
/// `crate::types` (below `transpiler_v2` in the layering; INV10 does not ban
/// `crate::types::*`) so `types::struct_type::field_by_name` — BELOW
/// `transpiler_v2` — can use the same single case-folding authority instead
/// of duplicating `canon_char`. Keeps every existing
/// `use super::name_fold::{eq_fold, fold_key}` in this subtree unchanged.
pub(crate) use crate::types::name_fold;
pub mod rewrites;
pub mod schema;
pub(crate) mod spark_errors;
pub(crate) mod sql_block;
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
pub use generator::{Generator, GeneratorKind};
pub use identifier::{parse_multipart_identifier, parse_sql_multipart_identifier, Qualifier};
pub use schema::{Attribute, ExprId, ResolvedSchema};
pub use statement::{render_ddl, DdlStatement, SqlStatement};
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
    // One of τ's two sanctioned `to_struct_type()` doors (see
    // `ResolvedSchema::to_struct_type` doc) — the Arrow-schema-stamp
    // boundary only needs the value shape, not column identity.
    Ok((sql, typed.resolved_schema.to_struct_type()))
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
    // The second of τ's two sanctioned `to_struct_type()` doors — see
    // `generate_with_schema` above.
    Ok(typed.resolved_schema.to_struct_type())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Spark-emulated error re-clothed with its Spark class token;
        // `UnknownTable` has a known `spark_class()`
        // (`TABLE_OR_VIEW_NOT_FOUND`) — rather than any τ-boundary marker.
        let plan = CommonAst::new(CommonOp::TableScan {
            table: Qualifier::single("no_such_table"),
        });
        let base_types = BaseTypes::empty();
        let result = generate(&plan, &base_types);
        match result {
            Err(EmissionError::SparkEmulated { class, message }) => {
                assert_eq!(class, Some("TABLE_OR_VIEW_NOT_FOUND"));
                assert!(
                    !message.contains("[SPARK-EMULATED]"),
                    "message must not double the internal prefix, got: {message}",
                );
                let display = EmissionError::SparkEmulated { class, message }.to_string();
                assert!(
                    display.starts_with("[TABLE_OR_VIEW_NOT_FOUND]"),
                    "expected leading Spark class token, got: {display}",
                );
            }
            other => panic!("expected EmissionError::SparkEmulated, got: {other:?}"),
        }
    }

    #[test]
    fn generate_surfaces_ambiguous_column_with_spark_class_leading() {
        // An ambiguous-column plan surfaces
        // `EmissionError::SparkEmulated { class: "AMBIGUOUS_REFERENCE", .. }`
        // whose Display leads with the exact Spark error-class token, so the
        // client-side differential harness's `spark_error_class` extracts
        // `AMBIGUOUS_REFERENCE`.
        use crate::transpiler_v2::ast::JoinType;
        use crate::transpiler_v2::expression::UnresolvedColumn;
        use crate::types::{DataType, StructField, StructType};

        let emp_schema = StructType::new(vec![
            StructField::not_null("id", DataType::Integer),
            StructField::not_null("dept_id", DataType::Integer),
        ]);
        let dept_schema = StructType::new(vec![
            StructField::not_null("dept_id", DataType::Integer),
            StructField::nullable("dept_name", DataType::String),
        ]);
        let base_types = BaseTypes::empty()
            .with_entry(&Qualifier::single("emp"), emp_schema)
            .with_entry(&Qualifier::single("dept"), dept_schema);

        let scan = |table: &str| {
            CommonAst::new(CommonOp::TableScan {
                table: Qualifier::single(table),
            })
        };
        // `dept_id` is present on both sides of the join — unqualified
        // reference is ambiguous.
        let ambiguous_condition = Expression::UnresolvedColumn(UnresolvedColumn {
            name_parts: vec!["dept_id".to_owned()],
            plan_id: None,
            is_metadata_column: false,
        });
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: Some(ambiguous_condition),
            using_columns: vec![],
            natural: false,
            lateral: false,
        });

        let err = generate(&plan, &base_types).expect_err("ambiguous column must error");
        match err {
            EmissionError::SparkEmulated { class, message } => {
                assert_eq!(class, Some("AMBIGUOUS_REFERENCE"));
                let display = EmissionError::SparkEmulated {
                    class,
                    message: message.clone(),
                }
                .to_string();
                assert!(
                    display.starts_with("[AMBIGUOUS_REFERENCE]"),
                    "expected leading Spark class token, got: {display}",
                );
                assert!(
                    message.contains("dept_id"),
                    "message should still name the ambiguous column, got: {message}",
                );
            }
            other => panic!("expected EmissionError::SparkEmulated, got: {other:?}"),
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
