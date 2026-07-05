//! τ's analyzer — resolve, assign types, derive nullability.
//!
//! Rearchitect ADR-005 / ADR-006 / ADR-021 / ADR-022.
//!
//! **INV10:** this file imports ONLY from `crate::types::{DataType,
//! StructField, StructType}` and `crate::transpiler_v2::*`. NO
//! `crate::logical`, `crate::expression`, `crate::generator`,
//! `crate::functions`, `crate::parser`, `crate::runtime`,
//! `crate::types::TypeInferenceEngine`.
//!
//! # Overview
//!
//! `analyze(ast, base_types)` runs three logical passes over a [`CommonAst`],
//! producing a [`TypedAst`] whose every node carries a fully-resolved schema
//! and whose every [`Expression::ColumnReference`] carries populated
//! `data_type` and `nullable` fields:
//!
//! 1. **resolve** — bottom-up: convert `UnresolvedColumn` → `ColumnReference`
//!    against the current operator's input schema, seed `TableScan` from
//!    `base_types`.
//! 2. **assign_types** — bottom-up: stamp `ColumnReference::data_type` and
//!    run the set-op widening sub-sweep (§5).
//! 3. **derive_nullability** — bottom-up: stamp `ColumnReference::nullable`
//!    and apply outer-join nullability derivation (§6).
//!
//! In the current implementation the three passes are fused into a single
//! bottom-up traversal for efficiency; the section comments below mark
//! where each conceptual pass runs.
//!
//! # Errors
//!
//! [`AnalyzerError`] variants split into two categories per ADR-022:
//!
//! - **Spark-emulated** (`[SPARK-EMULATED]` Display prefix): errors that
//!   reference Spark would also raise — `UnknownTable`, `UnknownColumn`,
//!   `AmbiguousColumn`, `TypeMismatch`, `Other`.
//! - **Thunderduck-boundary** (`[TDCK-BOUNDARY]` Display prefix): errors
//!   that signal Thunderduck's incomplete implementation —
//!   `PuntedOperator`, `UnsupportedRule`.

use std::collections::{HashMap, HashSet};

use super::ast::{CommonAst, CommonOp, FileFormat, JoinType, PivotGrouping, UnpivotIds};
use super::base_types::BaseTypes;
use super::error::EmissionError;
use super::expression::{
    AliasExpression, BinaryExpression, CaseWhenExpression, CastExpression, ColumnReference,
    Expression, ExtractValueExpression, FunctionCall, Literal, LiteralValue, SortOrder,
    StarExpression, SubqueryPlan, UnaryExpression, UnresolvedColumn,
};
use super::type_inference::TypeInferenceEngine;
use crate::types::{DataType, StructField, StructType};

// Re-export SetOpKind so downstream callers can use `analyzer::SetOpKind`.
pub use super::ast::SetOpKind;

/// The eight Spark defaults for `df.summary()` when no statistics list is
/// supplied — matches `Dataset.summary()` in Apache Spark 4.x.
pub(super) const DEFAULT_SUMMARY_STATS: &[&str] =
    &["count", "mean", "stddev", "min", "25%", "50%", "75%", "max"];

/// τ's schema type alias — points at the shared `StructType`.
pub type Schema = StructType;

// ── TypedAst / TypedOp / TypedAttr ──────────────────────────────────────────

/// A typed plan node: an operator plus its resolved output schema.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAst {
    /// The typed operator this node represents.
    pub op: TypedOp,
    /// The schema of the relation produced by this node — every field has
    /// a resolved (non-`Unresolved`) [`DataType`] and a known `nullable` flag.
    pub resolved_schema: StructType,
}

/// τ's typed operator set — the analyzer output shape.
///
/// Structurally mirrors [`CommonOp`] but with `TypedAst` children and any
/// analyzer-derived side-data attached (e.g. `Join` carries per-side derived
/// schemas after outer-join nullability flipping).
#[derive(Debug, Clone, PartialEq)]
pub enum TypedOp {
    /// `SELECT projections FROM input`.
    Project {
        /// The input relation.
        input: Box<TypedAst>,
        /// The projection list. `Star` is preserved verbatim; schema-level
        /// expansion is reflected in the parent's `resolved_schema`.
        projections: Vec<Expression>,
    },
    /// `SELECT * FROM input WHERE condition`.
    Filter {
        /// The input relation.
        input: Box<TypedAst>,
        /// The Boolean-valued predicate.
        condition: Expression,
    },
    /// `SELECT * FROM input ORDER BY order [LIMIT limit] [OFFSET offset]`.
    Sort {
        /// The input relation.
        input: Box<TypedAst>,
        /// Sort keys.
        order: Vec<SortOrder>,
        /// Optional LIMIT.
        limit: Option<i64>,
        /// Optional OFFSET.
        offset: Option<i64>,
    },
    /// `SELECT * FROM input LIMIT limit [OFFSET offset]`.
    Limit {
        /// The input relation.
        input: Box<TypedAst>,
        /// Maximum number of rows.
        limit: i64,
        /// Optional OFFSET.
        offset: Option<i64>,
    },
    /// `SELECT aggregates FROM input GROUP BY grouping`.
    Aggregate {
        /// The input relation.
        input: Box<TypedAst>,
        /// Grouping expressions.
        grouping: Vec<Expression>,
        /// Aggregate expressions (may fold grouping columns
        /// invariant — see [`CommonOp::Aggregate`]).
        aggregates: Vec<Expression>,
        /// GROUP BY variant.
        grouping_kind: crate::transpiler_v2::ast::GroupingKind,
    },
    /// A binary join.
    Join {
        /// The left relation.
        left: Box<TypedAst>,
        /// The right relation.
        right: Box<TypedAst>,
        /// The join type.
        join_type: JoinType,
        /// Optional `ON` condition.
        condition: Option<Expression>,
        /// USING column names.
        using_columns: Vec<String>,
        /// Plan-ids appearing anywhere under the left side.
        left_plan_ids: Vec<i64>,
        /// Plan-ids appearing anywhere under the right side.
        right_plan_ids: Vec<i64>,
        /// The left side's per-column schema **after** outer-join nullability
        /// flipping. Retained for future τ work's join emitter.
        derived_left_schema: StructType,
        /// The right side's per-column schema **after** outer-join
        /// nullability flipping. Retained for future τ work's join emitter.
        derived_right_schema: StructType,
    },
    /// A set operation (UNION / INTERSECT / EXCEPT).
    SetOp {
        /// The kind of set operation.
        kind: SetOpKind,
        /// Whether duplicates are preserved.
        all: bool,
        /// By-name matching.
        by_name: bool,
        /// Mirrors [`CommonOp::SetOp::allow_missing_columns`]. Retained on
        /// the typed AST so the emitter knows the child projections may need
        /// `CAST(NULL AS <ty>) AS <col>` padding.
        allow_missing_columns: bool,
        /// The typed children.
        children: Vec<TypedAst>,
        /// The widened output schema — the analyzer's post-sub-sweep result.
        /// When `allow_missing_columns = true`, this is the ordered union of
        /// column names across all children (LEFT-first, then RIGHT's extras).
        widened_schema: StructType,
    },
    /// A relation with exactly one row and zero columns.
    SingleRow,
    /// A named table scan.
    TableScan {
        /// The table name.
        table: String,
        /// Optional alias.
        alias: Option<String>,
    },
    /// An in-line `VALUES` relation.
    Values {
        /// One expression list per row.
        rows: Vec<Vec<Expression>>,
        /// Column names.
        column_names: Vec<String>,
    },
    /// A `createDataFrame` payload.
    LocalRelation {
        /// The declared schema.
        schema: StructType,
        /// One expression list per row.
        rows: Vec<Vec<Expression>>,
    },
    /// A file-format scan (declared-schema only; schema-less
    /// forms surface as `PuntedOperator("FileScan", "future τ work")`).
    FileScan {
        /// The file format.
        format: FileFormat,
        /// One or more file paths / globs.
        paths: Vec<String>,
        /// The declared schema (required).
        schema: StructType,
        /// Format-specific options.
        options: Vec<(String, String)>,
    },
    /// A table-valued function call — τ's analyzer punts.
    TableFunction {
        /// The function name.
        name: String,
        /// The function arguments.
        args: Vec<Expression>,
        /// Whether to emit an ordinality column.
        with_ordinality: bool,
    },
    /// `UNNEST(expr) [WITH ORDINALITY]` — τ's analyzer punts.
    Unnest {
        /// The array/map expression being unnested.
        expr: Expression,
        /// Whether to emit an ordinality column.
        with_ordinality: bool,
    },
    /// `df.withColumn(name, expr)` / `df.withColumns({...})`. Analyzer
    /// resolves each `expr` against the input schema, then emits the output
    /// schema by replacing input columns whose name matches an assignment
    /// (case-insensitive per Spark) and appending assignments whose name is
    /// new.
    WithColumns {
        /// The input relation.
        input: Box<TypedAst>,
        /// One `(column_name, expression)` per proto Alias, order preserved.
        assignments: Vec<(String, Expression)>,
    },
    /// `df.drop(col1, ...)`. Analyzer computes the output schema as input
    /// schema minus the named columns.
    DropColumns {
        /// The input relation.
        input: Box<TypedAst>,
        /// The names to drop.
        drop_names: Vec<String>,
    },
    /// `df.alias(name)`. Schema-transparent; alias retained for scope.
    AliasedRelation {
        /// The input relation.
        input: Box<TypedAst>,
        /// The alias name.
        alias: String,
    },
    /// `df.withColumnsRenamed({old: new, ...})`. Analyzer computes output
    /// schema by walking input fields and renaming those whose name matches
    /// a `old` (case-insensitive) to the corresponding `new`. Missing
    /// entries are silently ignored per Spark semantics.
    WithColumnsRenamed {
        /// The input relation.
        input: Box<TypedAst>,
        /// Old-name → new-name renames.
        renames: Vec<(String, String)>,
    },
    /// `df.describe(...)`. Analyzer materialises `cols` (empty ⇒ all input
    /// columns in schema order) and stamps the output schema as `summary`
    /// (STRING NOT NULL) + one STRING NULLABLE per materialised col.
    Describe {
        /// The input relation.
        input: Box<TypedAst>,
        /// The materialised column list (never empty here).
        cols: Vec<String>,
    },
    /// `df.summary(...)`. Analyzer materialises both `cols` (always the full
    /// input schema — proto `StatSummary` carries no `cols` field) and
    /// `statistics` (empty ⇒ [`DEFAULT_SUMMARY_STATS`]). Output schema is
    /// identical to [`TypedOp::Describe`].
    Summary {
        /// The input relation.
        input: Box<TypedAst>,
        /// The materialised column list (never empty here).
        cols: Vec<String>,
        /// The materialised statistics list (never empty here).
        statistics: Vec<String>,
    },
    /// `df.stat.freqItems(cols, support)`. Analyzer materialises `cols`
    /// (case-insensitive per Spark) and stamps the output schema as one
    /// `ARRAY<T>` column per input col — where `T` is the source column's
    /// declared [`DataType`] (Spark parity per ADR-015). The output column
    /// name is `{col}_freqItems`, using the caller's casing.
    ///
    /// Analyzer punts `Crosstab` (mirror-image of `Pivot[implicit-values]`)
    /// before ever constructing a `TypedOp` for it — so there is no
    /// `TypedOp::Crosstab` variant. When future τ work lifts the punt, that variant
    /// lands here alongside this one.
    FreqItems {
        /// The input relation.
        input: Box<TypedAst>,
        /// The materialised column list (case-insensitive resolved; never
        /// empty here — the emission stage additionally guards against empty).
        cols: Vec<String>,
        /// The minimum item frequency.
        support: f64,
    },
    /// `df.dropDuplicates` / `df.distinct`. Schema-transparent.
    Deduplicate {
        /// The input relation.
        input: Box<TypedAst>,
        /// Optional subset of columns.
        on_columns: Vec<String>,
    },
    /// `df.na.fill`. Schema-transparent (nullability MAY tighten but we
    /// leave it as-is; emission uses COALESCE which preserves the arg's
    /// declared nullability).
    NaFill {
        /// The input relation.
        input: Box<TypedAst>,
        /// Subset of columns.
        cols: Vec<String>,
        /// Fill values.
        values: Vec<Expression>,
    },
    /// `df.na.drop`. Schema-transparent.
    NaDrop {
        /// The input relation.
        input: Box<TypedAst>,
        /// Subset of columns.
        cols: Vec<String>,
        /// Optional min non-nulls.
        min_non_nulls: Option<i32>,
    },
    /// `df.na.replace`. Schema-transparent.
    NaReplace {
        /// The input relation.
        input: Box<TypedAst>,
        /// Subset of columns.
        cols: Vec<String>,
        /// (old, new) pairs.
        replacements: Vec<(Expression, Expression)>,
    },
    /// `df.unpivot(...)` / `df.melt(...)`. Wide → long. The analyzer
    /// expands empty `values` to "all non-id columns" and stamps the output
    /// schema: `<ids>` (unchanged) + `variable_column_name` (STRING NOT
    /// NULL) + `value_column_name` (Spark-widened common type across the
    /// input `values` columns, nullable if any is nullable).
    Unpivot {
        /// The input relation.
        input: Box<TypedAst>,
        /// Id columns (preserved).
        ids: Vec<String>,
        /// Value columns to unpivot (materialised — never empty here).
        values: Vec<String>,
        /// The name of the output variable column.
        variable_column_name: String,
        /// The name of the output value column.
        value_column_name: String,
    },
    /// `df.sample(...)` post-analysis. Schema-preserving.
    Sample {
        /// The input relation.
        input: Box<TypedAst>,
        /// The inclusive lower bound of the sampling range.
        lower_bound: f64,
        /// The exclusive upper bound of the sampling range.
        upper_bound: f64,
        /// Whether rows may be sampled with replacement.
        with_replacement: bool,
        /// Optional RNG seed.
        seed: Option<i64>,
    },
    /// `df.sampleBy(col, fractions, seed)` post-analysis. `col` is resolved
    /// (ColumnReference); `fractions` remain literal. Schema-preserving.
    SampleBy {
        /// The input relation.
        input: Box<TypedAst>,
        /// The resolved stratum column expression.
        col: Expression,
        /// Per-stratum `(literal, fraction)` pairs.
        fractions: Vec<(Literal, f64)>,
        /// Optional RNG seed.
        seed: Option<i64>,
    },
    /// `df.groupBy(...).pivot(col, [values]).agg(...)`. See
    /// [`CommonOp::Pivot`] for the semantic contract. The analyzer resolves
    /// grouping / pivot column / aggregates against the input schema and
    /// stamps the output schema per Spark: `<grouping>` + one output column
    /// per pivot value × aggregate. When `pivot_values` is empty, DuckDB PIVOT
    /// discovers values at execution time and the analyzer's `resolved_schema`
    /// is intentionally partial (grouping columns only) — the emission stage
    /// emits a `PIVOT` without an `IN (...)` clause and the caller's Arrow
    /// stream carries the actual schema.
    Pivot {
        /// The input relation.
        input: Box<TypedAst>,
        /// Grouping columns (resolved).
        grouping: Vec<Expression>,
        /// The pivot column (resolved).
        pivot_column: Expression,
        /// Explicit pivot value literals (resolved). Empty ⇒ implicit /
        /// DuckDB-eager discovery at execute time.
        pivot_values: Vec<Expression>,
        /// Aggregate expressions (resolved).
        aggregates: Vec<Expression>,
    },
}

/// A typed attribute — the resolved shape of a single output column.
///
/// Currently a projection over [`StructField`] with an optional `qualifier`
/// and `plan_id`. τ's analyzer does not thread `TypedAttr` through the tree — the
/// per-node `resolved_schema: StructType` carries the same information at
/// coarser granularity. `TypedAttr` is retained so future τ work can attach
/// per-column disambiguation metadata when the emitter needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAttr {
    /// The column name.
    pub name: String,
    /// The resolved data type.
    pub data_type: DataType,
    /// The resolved nullability.
    pub nullable: bool,
    /// Optional qualifier (e.g. table alias).
    pub qualifier: Option<String>,
    /// Optional proto plan_id.
    pub plan_id: Option<i64>,
}

// ── HasSchema sealed trait ──────────────────────────────────────────────────

mod sealed {
    /// Sealed marker — external crates cannot implement [`super::HasSchema`].
    pub trait Sealed {}
    impl Sealed for super::TypedAst {}
}

/// A sealed accessor trait — every plan-node type that has a resolved schema.
pub trait HasSchema: sealed::Sealed {
    /// The relation's resolved output schema.
    fn resolved_schema(&self) -> &StructType;
}

impl HasSchema for TypedAst {
    fn resolved_schema(&self) -> &StructType {
        &self.resolved_schema
    }
}

// ── AnalyzerError (ADR-022 two-category split) ──────────────────────────────

/// Errors surfaced by the τ analyzer.
///
/// Two categories per ADR-022:
///
/// - **Spark-emulated** — errors reference Spark would also raise. The client
///   sees the same error under Thunderduck as under Spark.
/// - **Thunderduck-boundary** — errors that signal Thunderduck's incomplete
///   implementation (a plan / rule not yet lowered).
///
/// The Display prefix (`[SPARK-EMULATED]` vs `[TDCK-BOUNDARY]`) enables
/// grep-based classification and reviewer verification.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
pub enum AnalyzerError {
    // ── Spark-emulated ─────────────────────────────────────────────────────
    /// The named table could not be resolved (missing from catalog / base types).
    #[error("[SPARK-EMULATED] table not found: `{name}`")]
    UnknownTable {
        /// The table name that was not resolvable.
        name: String,
    },

    /// A column reference could not be resolved against the input schema.
    #[error("[SPARK-EMULATED] cannot resolve column `{}{name}`", .qualifier.as_deref().map(|q| format!("{q}.")).unwrap_or_default())]
    UnknownColumn {
        /// The column name.
        name: String,
        /// Optional qualifier (e.g. `"emp"` in `"emp.id"`).
        qualifier: Option<String>,
    },

    /// A column name resolves to multiple candidates and cannot be
    /// disambiguated by qualifier or plan_id.
    #[error("[SPARK-EMULATED] column `{name}` is ambiguous, candidates: {candidates:?}")]
    AmbiguousColumn {
        /// The ambiguous column name.
        name: String,
        /// The candidate qualified names.
        candidates: Vec<String>,
    },

    /// A type mismatch — an operand's actual type does not match the expected
    /// type (e.g. Filter condition must be Boolean).
    #[error(
        "[SPARK-EMULATED] type mismatch: expected `{expected:?}`, got `{actual:?}` ({context})"
    )]
    TypeMismatch {
        /// The expected type.
        expected: DataType,
        /// The observed type.
        actual: DataType,
        /// A short context tag (e.g. `"filter-condition"`, `"set-op arity"`).
        context: String,
    },

    /// A catch-all Spark-emulated error not captured by the more specific
    /// variants above.
    #[error("[SPARK-EMULATED] {reason}")]
    Other {
        /// A description of the error.
        reason: String,
    },

    // ── Thunderduck-boundary ───────────────────────────────────────────────
    /// The named operator is not yet supported by the τ analyzer.
    #[error("[TDCK-BOUNDARY] operator `{op}` not yet implemented in analyzer: {reason}")]
    PuntedOperator {
        /// The operator name (e.g. `"FileScan"`, `"TableFunction"`).
        op: String,
        /// A short explanation.
        reason: String,
    },

    /// A specific analyzer inference rule is not yet implemented.
    #[error("[TDCK-BOUNDARY] inference rule `{rule}` not yet implemented: {reason}")]
    UnsupportedRule {
        /// The rule name.
        rule: String,
        /// A short explanation.
        reason: String,
    },
}

// ── analyze() — the top-level entry point ───────────────────────────────────

/// Analyze a plan: resolve columns, assign types, derive nullability.
///
/// Returns a [`TypedAst`] whose every plan node carries a resolved
/// [`StructType`] and whose every `ColumnReference` carries populated
/// `data_type` and `nullable` fields.
pub fn analyze(ast: CommonAst, base_types: &BaseTypes) -> Result<TypedAst, AnalyzerError> {
    // The three logical passes (resolve → assign_types → derive_nullability)
    // are fused into a single bottom-up traversal for efficiency. Section
    // comments below mark where each conceptual pass runs.
    analyze_node(ast, base_types)
}

// ── Public helpers ──────────────────────────────────────────────────────────

/// INV5: walk a [`TypedAst`] and return `true` iff every node carries a
/// fully-resolved schema and every embedded `ColumnReference` carries
/// populated `data_type` / `nullable` fields.
///
/// Returns `false` if any `resolved_schema` contains a field whose type is
/// `DataType::Unresolved`, OR any `Expression::ColumnReference` inside the
/// tree has `data_type = None` OR `nullable = None`.
pub fn has_resolved_schema(ast: &TypedAst) -> bool {
    if schema_has_unresolved(&ast.resolved_schema) {
        return false;
    }
    match &ast.op {
        TypedOp::Project { input, projections } => {
            has_resolved_schema(input) && projections.iter().all(expression_is_fully_resolved)
        }
        TypedOp::Filter { input, condition } => {
            has_resolved_schema(input) && expression_is_fully_resolved(condition)
        }
        TypedOp::Sort { input, order, .. } => {
            has_resolved_schema(input)
                && order.iter().all(|o| expression_is_fully_resolved(&o.expr))
        }
        TypedOp::Limit { input, .. } => has_resolved_schema(input),
        TypedOp::Aggregate {
            input,
            grouping,
            aggregates,
            ..
        } => {
            has_resolved_schema(input)
                && grouping.iter().all(expression_is_fully_resolved)
                && aggregates.iter().all(expression_is_fully_resolved)
        }
        TypedOp::Join {
            left,
            right,
            condition,
            ..
        } => {
            has_resolved_schema(left)
                && has_resolved_schema(right)
                && condition.as_ref().is_none_or(expression_is_fully_resolved)
        }
        TypedOp::SetOp {
            children,
            widened_schema,
            ..
        } => !schema_has_unresolved(widened_schema) && children.iter().all(has_resolved_schema),
        TypedOp::Values { rows, .. } | TypedOp::LocalRelation { rows, .. } => rows
            .iter()
            .all(|row| row.iter().all(expression_is_fully_resolved)),
        TypedOp::TableFunction { args, .. } => args.iter().all(expression_is_fully_resolved),
        TypedOp::Unnest { expr, .. } => expression_is_fully_resolved(expr),
        TypedOp::WithColumns { input, assignments } => {
            has_resolved_schema(input)
                && assignments
                    .iter()
                    .all(|(_, e)| expression_is_fully_resolved(e))
        }
        TypedOp::DropColumns { input, .. } => has_resolved_schema(input),
        TypedOp::AliasedRelation { input, .. } => has_resolved_schema(input),
        TypedOp::WithColumnsRenamed { input, .. } => has_resolved_schema(input),
        TypedOp::Deduplicate { input, .. } => has_resolved_schema(input),
        TypedOp::NaFill { input, .. }
        | TypedOp::NaDrop { input, .. }
        | TypedOp::NaReplace { input, .. }
        | TypedOp::Unpivot { input, .. }
        | TypedOp::Describe { input, .. }
        | TypedOp::Summary { input, .. }
        | TypedOp::FreqItems { input, .. }
        | TypedOp::Sample { input, .. } => has_resolved_schema(input),
        TypedOp::SampleBy { input, col, .. } => {
            has_resolved_schema(input) && expression_is_fully_resolved(col)
        }
        // Pivot: explicit-values Pivot has a fully-stamped schema (group
        // cols + pivot_value × aggregate columns). Implicit-values Pivot
        // never reaches this arm — `analyze_pivot` punts with a
        // Thunderduck-boundary error before constructing the `TypedOp::Pivot`.
        TypedOp::Pivot { input, .. } => has_resolved_schema(input),
        TypedOp::SingleRow | TypedOp::TableScan { .. } | TypedOp::FileScan { .. } => true,
    }
}

/// Bridge an [`AnalyzerError`] into an [`EmissionError`] preserving the
/// two-category classification via the Display prefix (`[SPARK-EMULATED]` /
/// `[TDCK-BOUNDARY]`). Called by `transpiler_v2::generate()`.
pub(super) fn analyzer_error_to_emission_error(e: AnalyzerError) -> EmissionError {
    match e {
        AnalyzerError::UnknownTable { .. }
        | AnalyzerError::UnknownColumn { .. }
        | AnalyzerError::AmbiguousColumn { .. }
        | AnalyzerError::TypeMismatch { .. }
        | AnalyzerError::Other { .. } => EmissionError::UnsupportedExpression {
            shape: "analyzer-spark-emulated".to_owned(),
            reason: e.to_string(),
        },
        AnalyzerError::PuntedOperator { op, reason } => EmissionError::UnsupportedOp { op, reason },
        AnalyzerError::UnsupportedRule { rule, reason } => EmissionError::UnsupportedExpression {
            shape: rule,
            reason,
        },
    }
}

// ── Internal: single-pass bottom-up analyzer ────────────────────────────────

fn analyze_node(ast: CommonAst, base_types: &BaseTypes) -> Result<TypedAst, AnalyzerError> {
    match ast.op {
        // ── Leaves ────────────────────────────────────────────────────────
        CommonOp::SingleRow => Ok(TypedAst {
            op: TypedOp::SingleRow,
            resolved_schema: StructType::empty(),
        }),

        CommonOp::TableScan { table, alias } => {
            // resolve: seed schema from base_types.
            let schema =
                base_types
                    .lookup(&table)
                    .cloned()
                    .ok_or_else(|| AnalyzerError::UnknownTable {
                        name: table.clone(),
                    })?;
            let resolved = apply_alias_to_schema(&schema, alias.as_deref());
            Ok(TypedAst {
                op: TypedOp::TableScan { table, alias },
                resolved_schema: resolved,
            })
        }

        CommonOp::Values { rows, column_names } => {
            let schema = infer_values_schema(&rows, &column_names)?;
            let typed_rows = rows
                .into_iter()
                .map(|row| resolve_expr_list(row, &schema, base_types))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TypedAst {
                op: TypedOp::Values {
                    rows: typed_rows,
                    column_names,
                },
                resolved_schema: schema,
            })
        }

        CommonOp::LocalRelation { schema, rows } => Ok(TypedAst {
            op: TypedOp::LocalRelation {
                schema: schema.clone(),
                rows,
            },
            resolved_schema: schema,
        }),

        CommonOp::FileScan {
            format,
            paths,
            schema,
            options,
        } => match schema {
            Some(s) => Ok(TypedAst {
                op: TypedOp::FileScan {
                    format,
                    paths,
                    schema: s.clone(),
                    options,
                },
                resolved_schema: s,
            }),
            None => Err(AnalyzerError::PuntedOperator {
                op: "FileScan".to_owned(),
                reason: "schema-less FileScan (parquet inference) (not implemented in τ)".to_owned(),
            }),
        },

        CommonOp::TableFunction {
            name,
            args: _,
            with_ordinality: _,
        } => Err(AnalyzerError::PuntedOperator {
            op: format!("TableFunction[{name}]"),
            reason: "table-function analysis (not implemented in τ)".to_owned(),
        }),

        CommonOp::Unnest {
            expr: _,
            with_ordinality: _,
        } => Err(AnalyzerError::PuntedOperator {
            op: "Unnest".to_owned(),
            reason: "unnest analysis (not implemented in τ)".to_owned(),
        }),

        // ── Unary ─────────────────────────────────────────────────────────
        CommonOp::Project { input, projections } => {
            let typed_input = analyze_node(*input, base_types)?;
            // Pass 85 — expand `df.colRegex("`.*_id`")` projections BEFORE
            // resolution. Each `UnresolvedRegex` becomes N `UnresolvedColumn`
            // refs (one per matching input field, schema order preserved).
            let projections = expand_regex_projections(projections, &typed_input.resolved_schema)?;
            // Pass 90 — expand `F.inline(arr)` / `F.inline_outer(arr)`
            // projections into N synthetic per-struct-field projections. Each
            // becomes `Alias(inline_field(arr, "<name>"), "<name>")` (inner)
            // or `Alias(inline_outer_field(arr, "<name>"), "<name>")` (outer).
            // Runs BEFORE `resolve_and_stamp` — the synthesized args are
            // resolved by the outer walk. Corpus: inl-001, inl-002.
            let projections = expand_inline_projections(projections, &typed_input.resolved_schema)?;
            // Pass 91 — expand `F.json_tuple(json, k1, ..., kN)` projections
            // into N synthetic per-key projections. Each becomes
            // `Alias(json_tuple_field(json, "<ki>"), "c<i>")` — positional
            // names per Spark's `Generator.elementSchema`, NOT the key
            // literals. Runs after inline expansion, before
            // `resolve_and_stamp`. Corpus: json-002.
            let projections = expand_json_tuple_projections(projections)?;
            let projections = projections
                .into_iter()
                .map(|e| resolve_and_stamp(e, &typed_input.resolved_schema, base_types))
                .collect::<Result<Vec<_>, _>>()?;
            // Compute output schema — expand Star; take alias name if present.
            let output_schema = project_output_schema(&projections, &typed_input.resolved_schema)?;
            Ok(TypedAst {
                op: TypedOp::Project {
                    input: Box::new(typed_input),
                    projections,
                },
                resolved_schema: output_schema,
            })
        }

        CommonOp::Filter { input, condition } => {
            let typed_input = analyze_node(*input, base_types)?;
            let condition = resolve_and_stamp(condition, &typed_input.resolved_schema, base_types)?;
            let cond_type = condition.data_type(&typed_input.resolved_schema);
            if !matches!(
                cond_type,
                DataType::Boolean | DataType::Unresolved | DataType::Null
            ) {
                return Err(AnalyzerError::TypeMismatch {
                    expected: DataType::Boolean,
                    actual: cond_type,
                    context: "filter-condition".to_owned(),
                });
            }
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::Filter {
                    input: Box::new(typed_input),
                    condition,
                },
                resolved_schema: output_schema,
            })
        }

        CommonOp::Sort {
            input,
            order,
            limit,
            offset,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let order = order
                .into_iter()
                .map(|so| {
                    let expr =
                        resolve_and_stamp(*so.expr, &typed_input.resolved_schema, base_types)?;
                    Ok::<SortOrder, AnalyzerError>(SortOrder {
                        expr: Box::new(expr),
                        direction: so.direction,
                        null_ordering: so.null_ordering,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::Sort {
                    input: Box::new(typed_input),
                    order,
                    limit,
                    offset,
                },
                resolved_schema: output_schema,
            })
        }

        CommonOp::Limit {
            input,
            limit,
            offset,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::Limit {
                    input: Box::new(typed_input),
                    limit,
                    offset,
                },
                resolved_schema: output_schema,
            })
        }

        CommonOp::Aggregate {
            input,
            grouping,
            aggregates,
            grouping_kind,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let grouping = resolve_expr_list(grouping, &typed_input.resolved_schema, base_types)?;
            let aggregates =
                resolve_expr_list(aggregates, &typed_input.resolved_schema, base_types)?;
            // Output schema construction:
            // SparkSQL path folds grouping cols into `aggregates` already
            // (per CommonOp::Aggregate invariant), so output = aggregates as-is.
            // DataFrame path keeps them separate — detect by seeing whether
            // the aggregates list already begins with the grouping's output
            // names; if not, prepend grouping. Empty grouping = global agg
            // (no unfolding needed).
            let agg_names: Vec<String> = aggregates.iter().map(expression_output_name).collect();
            let group_names: Vec<String> = grouping.iter().map(expression_output_name).collect();
            let already_folded = grouping.is_empty()
                || group_names
                    .iter()
                    .all(|gn| agg_names.iter().any(|an| an.eq_ignore_ascii_case(gn)));
            let mut output_fields: Vec<StructField> = Vec::new();
            if !already_folded {
                for g in &grouping {
                    let name = expression_output_name(g);
                    let dt = g.data_type(&typed_input.resolved_schema);
                    let nullable = g.nullable(&typed_input.resolved_schema);
                    output_fields.push(StructField::new(name, dt, nullable));
                }
            }
            for e in &aggregates {
                let name = expression_output_name(e);
                let dt = e.data_type(&typed_input.resolved_schema);
                let nullable = e.nullable(&typed_input.resolved_schema);
                output_fields.push(StructField::new(name, dt, nullable));
            }
            let output_schema = StructType::new(output_fields);
            Ok(TypedAst {
                op: TypedOp::Aggregate {
                    input: Box::new(typed_input),
                    grouping,
                    aggregates,
                    grouping_kind,
                },
                resolved_schema: output_schema,
            })
        }

        // ── WithColumns (add-or-replace by name, Spark semantics) ────────
        CommonOp::WithColumns { input, assignments } => {
            let typed_input = analyze_node(*input, base_types)?;
            let input_schema = &typed_input.resolved_schema;
            // Resolve each assignment expression against the INPUT schema —
            // Spark semantics: later assignments see the input value, not
            // intermediate replacements.
            let mut resolved_assignments: Vec<(String, Expression)> =
                Vec::with_capacity(assignments.len());
            for (name, expr) in assignments {
                let resolved = resolve_and_stamp(expr, input_schema, base_types)?;
                resolved_assignments.push((name, resolved));
            }
            // Output schema: walk input fields; if a field name matches an
            // assignment (case-insensitive), substitute the assignment's
            // resolved (type, nullable). Then append assignments whose name
            // did not match any input field.
            let mut assigned_lower: std::collections::HashMap<String, usize> =
                std::collections::HashMap::with_capacity(resolved_assignments.len());
            for (i, (name, _)) in resolved_assignments.iter().enumerate() {
                assigned_lower.insert(name.to_lowercase(), i);
            }
            let mut consumed = vec![false; resolved_assignments.len()];
            let mut output_fields: Vec<StructField> =
                Vec::with_capacity(input_schema.fields.len() + resolved_assignments.len());
            for f in &input_schema.fields {
                if let Some(&idx) = assigned_lower.get(&f.name.to_lowercase()) {
                    let (_, expr) = &resolved_assignments[idx];
                    let dt = expr.data_type(input_schema);
                    let nullable = expr.nullable(input_schema);
                    // Preserve the input field's original casing for the name.
                    output_fields.push(StructField::new(f.name.clone(), dt, nullable));
                    consumed[idx] = true;
                } else {
                    output_fields.push(f.clone());
                }
            }
            for (i, (name, expr)) in resolved_assignments.iter().enumerate() {
                if !consumed[i] {
                    let dt = expr.data_type(input_schema);
                    let nullable = expr.nullable(input_schema);
                    output_fields.push(StructField::new(name.clone(), dt, nullable));
                }
            }
            let output_schema = StructType::new(output_fields);
            Ok(TypedAst {
                op: TypedOp::WithColumns {
                    input: Box::new(typed_input),
                    assignments: resolved_assignments,
                },
                resolved_schema: output_schema,
            })
        }

        // ── NA family ────────────────────────────────────────────────────
        CommonOp::NaFill {
            input,
            cols,
            values,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            // Columns filled with a non-null value become non-nullable.
            // If the fill value itself is null (unusual), preserve
            // nullability. Empty `cols` = fill all cols compatible with
            // the (single) value's type — we widen to "make all cols with
            // that type non-null" via a simple pass.
            let filled = |col_name: &str| -> Option<&Expression> {
                if cols.is_empty() {
                    Some(&values[0])
                } else if values.len() == 1 {
                    if cols.iter().any(|c| c.eq_ignore_ascii_case(col_name)) {
                        Some(&values[0])
                    } else {
                        None
                    }
                } else {
                    for (c, v) in cols.iter().zip(values.iter()) {
                        if c.eq_ignore_ascii_case(col_name) {
                            return Some(v);
                        }
                    }
                    None
                }
            };
            let mut output_fields: Vec<StructField> =
                Vec::with_capacity(typed_input.resolved_schema.fields.len());
            for f in &typed_input.resolved_schema.fields {
                let fill_expr = filled(&f.name);
                let mut nf = f.clone();
                if let Some(v) = fill_expr {
                    // If fill value is non-null (typical case), the output
                    // column becomes non-nullable.
                    let fill_nullable = v.nullable(&typed_input.resolved_schema);
                    if !fill_nullable {
                        nf.nullable = false;
                    }
                }
                output_fields.push(nf);
            }
            let output_schema = StructType::new(output_fields);
            Ok(TypedAst {
                op: TypedOp::NaFill {
                    input: Box::new(typed_input),
                    cols,
                    values,
                },
                resolved_schema: output_schema,
            })
        }
        CommonOp::NaDrop {
            input,
            cols,
            min_non_nulls,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::NaDrop {
                    input: Box::new(typed_input),
                    cols,
                    min_non_nulls,
                },
                resolved_schema: output_schema,
            })
        }
        CommonOp::NaReplace {
            input,
            cols,
            replacements,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::NaReplace {
                    input: Box::new(typed_input),
                    cols,
                    replacements,
                },
                resolved_schema: output_schema,
            })
        }

        // ── Unpivot (Spark `df.unpivot(...)` / `df.melt(...)`) ──────────
        CommonOp::Unpivot {
            input,
            ids,
            values,
            variable_column_name,
            value_column_name,
        } => analyze_unpivot(
            *input,
            ids,
            values,
            variable_column_name,
            value_column_name,
            base_types,
        ),

        // ── Describe (Spark `df.describe(...)`) ─────────────────────────
        CommonOp::Describe { input, cols } => analyze_describe(*input, cols, base_types),

        // ── Summary (Spark `df.summary(...)`) ───────────────────────────
        CommonOp::Summary { input, statistics } => analyze_summary(*input, statistics, base_types),

        // ── FreqItems (Spark `df.stat.freqItems(...)`) ──────────────────
        CommonOp::FreqItems {
            input,
            cols,
            support,
        } => analyze_freq_items(*input, cols, support, base_types),

        // ── Crosstab — Thunderduck-boundary (ADR-022) ───────────────────
        // Output columns are DISTINCT(col2) — unknowable at plan time.
        // Mirror-image of Pivot[implicit-values]: same session-hook blocker
        //. Reject loudly rather than stamp a partial schema.
        CommonOp::Crosstab { .. } => Err(AnalyzerError::PuntedOperator {
            op: "Crosstab[dynamic-values]".to_owned(),
            reason: "requires session-injected DISTINCT-query hook".to_owned(),
        }),

        // ── Pivot (Spark `df.groupBy(...).pivot(...).agg(...)`) ─────────
        CommonOp::Pivot {
            input,
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
        } => analyze_pivot(
            *input,
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
            base_types,
        ),

        // ── Deduplicate (Spark `df.dropDuplicates` / `df.distinct`) ──────
        CommonOp::Deduplicate { input, on_columns } => {
            let typed_input = analyze_node(*input, base_types)?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::Deduplicate {
                    input: Box::new(typed_input),
                    on_columns,
                },
                resolved_schema: output_schema,
            })
        }

        // ── Sample (Spark `df.sample(...)`) ─────────────────────────────
        CommonOp::Sample {
            input,
            lower_bound,
            upper_bound,
            with_replacement,
            seed,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::Sample {
                    input: Box::new(typed_input),
                    lower_bound,
                    upper_bound,
                    with_replacement,
                    seed,
                },
                resolved_schema: output_schema,
            })
        }

        // ── SampleBy (Spark `df.sampleBy(col, fractions, seed)`) ───────
        CommonOp::SampleBy {
            input,
            col,
            fractions,
            seed,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let col = resolve_and_stamp(col, &typed_input.resolved_schema, base_types)?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::SampleBy {
                    input: Box::new(typed_input),
                    col,
                    fractions,
                    seed,
                },
                resolved_schema: output_schema,
            })
        }

        // ── ToDf (Spark `df.toDF(new1, new2, ...)`) ──────────────────────
        CommonOp::ToDf {
            input,
            column_names,
        } => {
            let typed_input = analyze_node(*input, base_types)?;
            let input_fields = &typed_input.resolved_schema.fields;
            if input_fields.len() != column_names.len() {
                return Err(AnalyzerError::Other {
                    reason: format!(
                        "toDF arity mismatch: input has {} columns, got {} names",
                        input_fields.len(),
                        column_names.len()
                    ),
                });
            }
            let mut output_fields: Vec<StructField> = Vec::with_capacity(input_fields.len());
            for (f, new_name) in input_fields.iter().zip(column_names.iter()) {
                output_fields.push(StructField::new(
                    new_name.clone(),
                    f.data_type.clone(),
                    f.nullable,
                ));
            }
            // Convert to WithColumnsRenamed for emission simplicity.
            let renames: Vec<(String, String)> = input_fields
                .iter()
                .zip(column_names.iter())
                .map(|(f, n)| (f.name.clone(), n.clone()))
                .collect();
            let output_schema = StructType::new(output_fields);
            Ok(TypedAst {
                op: TypedOp::WithColumnsRenamed {
                    input: Box::new(typed_input),
                    renames,
                },
                resolved_schema: output_schema,
            })
        }

        // ── AliasedRelation (Spark `df.alias(name)`) ─────────────────────
        CommonOp::AliasedRelation { input, alias } => {
            let typed_input = analyze_node(*input, base_types)?;
            let output_schema = typed_input.resolved_schema.clone();
            Ok(TypedAst {
                op: TypedOp::AliasedRelation {
                    input: Box::new(typed_input),
                    alias,
                },
                resolved_schema: output_schema,
            })
        }

        // ── WithColumnsRenamed (Spark `df.withColumnsRenamed(...)`) ──────
        CommonOp::WithColumnsRenamed { input, renames } => {
            let typed_input = analyze_node(*input, base_types)?;
            let rename_map: std::collections::HashMap<String, String> = renames
                .iter()
                .map(|(old, new)| (old.to_lowercase(), new.clone()))
                .collect();
            let mut output_fields: Vec<StructField> =
                Vec::with_capacity(typed_input.resolved_schema.fields.len());
            for f in &typed_input.resolved_schema.fields {
                let new_name = rename_map.get(&f.name.to_lowercase()).cloned();
                let mut nf = f.clone();
                if let Some(n) = new_name {
                    nf.name = n;
                }
                output_fields.push(nf);
            }
            let output_schema = StructType::new(output_fields);
            Ok(TypedAst {
                op: TypedOp::WithColumnsRenamed {
                    input: Box::new(typed_input),
                    renames,
                },
                resolved_schema: output_schema,
            })
        }

        // ── DropColumns (Spark `df.drop(...)`) ───────────────────────────
        CommonOp::DropColumns { input, drop_names } => {
            let typed_input = analyze_node(*input, base_types)?;
            let drop_lower: std::collections::HashSet<String> =
                drop_names.iter().map(|s| s.to_lowercase()).collect();
            let mut output_fields: Vec<StructField> =
                Vec::with_capacity(typed_input.resolved_schema.fields.len());
            for f in &typed_input.resolved_schema.fields {
                if !drop_lower.contains(&f.name.to_lowercase()) {
                    output_fields.push(f.clone());
                }
            }
            let output_schema = StructType::new(output_fields);
            Ok(TypedAst {
                op: TypedOp::DropColumns {
                    input: Box::new(typed_input),
                    drop_names,
                },
                resolved_schema: output_schema,
            })
        }

        // ── Binary: Join ──────────────────────────────────────────────────
        CommonOp::Join {
            left,
            right,
            join_type,
            condition,
            using_columns,
            left_plan_ids,
            right_plan_ids,
        } => {
            let typed_left = analyze_node(*left, base_types)?;
            let typed_right = analyze_node(*right, base_types)?;

            // resolve+assign_types: resolve condition against merged schema.
            let combined_input_schema =
                StructType::merge(&typed_left.resolved_schema, &typed_right.resolved_schema);

            // Ambiguity is now surfaced centrally by `resolve_column` (see
            // its comment). Any unqualified reference — whether in the join
            // condition here, or in projections/filters/sort keys above —
            // that resolves to more than one field raises `AmbiguousColumn`.
            //
            // BUT: proto `Expression.Attribute.plan_id` is Spark's mechanism
            // to disambiguate `emp.dept_id == dept.dept_id` — the two refs
            // share a name but carry different plan_ids. Pre-process the
            // condition to synthesize the emission-facing qualifier
            // (`__td_jl` / `__td_jr`) from plan_id membership; that turns a
            // plan_id-tagged unqualified reference into a "trust the
            // caller" qualified reference that `resolve_column` accepts and
            // emission renders as `__td_jl.dept_id` — matching the aliases
            // `render_join` emits.
            let condition = match condition {
                Some(c) => {
                    let qualified = qualify_plan_id_refs(c, &left_plan_ids, &right_plan_ids);
                    Some(resolve_and_stamp(
                        qualified,
                        &combined_input_schema,
                        base_types,
                    )?)
                }
                None => None,
            };

            // derive_nullability: apply outer-join flipping (§6).
            let (derived_left_schema, derived_right_schema) = apply_join_nullability(
                &typed_left.resolved_schema,
                &typed_right.resolved_schema,
                join_type,
            );
            // Output schema by join kind:
            //   SEMI/ANTI  → left schema only (right's columns are semantically absent).
            //   USING(...) → the USING columns appear ONCE (deduped), then
            //                left's remaining columns, then right's remaining
            //                columns. Matches DuckDB `SELECT * FROM l JOIN r
            //                USING (k1, k2)` output shape and Spark's
            //                `join(other, on=[...])`.
            //   Otherwise  → simple concatenation.
            // Output schema by join kind (Spark-parity — verified against
            // corpus join cases). For USING joins, Spark hoists the USING
            // columns to position 0, then left's non-USING cols, then
            // right's non-USING cols.
            //   SEMI/ANTI + USING     → USING first, then left's non-USING.
            //   SEMI/ANTI (no USING)  → left schema unchanged.
            //   INNER/LEFT/RIGHT/FULL + USING → USING first, left non-USING, right non-USING.
            //   Otherwise             → simple concatenation.
            // USING-column donor rules (Spark-parity):
            //   INNER / LEFT / SEMI / ANTI → left side (unchanged).
            //   RIGHT                       → right side (right is dominant).
            //   FULL                        → left side by name, but with
            //                                 nullable = left.nullable AND
            //                                 right.nullable (COALESCE
            //                                 semantics: non-null iff either
            //                                 side is non-null).
            //   CROSS                       → USING never applies.
            let build_using_prefix = |using: &[String]| -> Vec<StructField> {
                let mut fields = Vec::with_capacity(using.len());
                for n in using {
                    let left_field = derived_left_schema
                        .fields
                        .iter()
                        .find(|f| f.name.eq_ignore_ascii_case(n));
                    let right_field = derived_right_schema
                        .fields
                        .iter()
                        .find(|f| f.name.eq_ignore_ascii_case(n));
                    match (join_type, left_field, right_field) {
                        (JoinType::Right, _, Some(rf)) => fields.push(rf.clone()),
                        (JoinType::Full, Some(lf), Some(rf)) => {
                            // Non-null iff EITHER side is non-null.
                            let mut coalesced = lf.clone();
                            coalesced.nullable = lf.nullable && rf.nullable;
                            fields.push(coalesced);
                        }
                        (_, Some(lf), _) => fields.push(lf.clone()),
                        (_, None, Some(rf)) => fields.push(rf.clone()),
                        _ => {}
                    }
                }
                fields
            };
            let output_schema = if !using_columns.is_empty() {
                let using_lower: std::collections::HashSet<String> =
                    using_columns.iter().map(|s| s.to_lowercase()).collect();
                let mut fields = build_using_prefix(&using_columns);
                for f in &derived_left_schema.fields {
                    if !using_lower.contains(&f.name.to_lowercase()) {
                        fields.push(f.clone());
                    }
                }
                if !matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti) {
                    for f in &derived_right_schema.fields {
                        if !using_lower.contains(&f.name.to_lowercase()) {
                            fields.push(f.clone());
                        }
                    }
                }
                StructType::new(fields)
            } else {
                match join_type {
                    JoinType::LeftSemi | JoinType::LeftAnti => derived_left_schema.clone(),
                    _ => StructType::merge(&derived_left_schema, &derived_right_schema),
                }
            };

            Ok(TypedAst {
                op: TypedOp::Join {
                    left: Box::new(typed_left),
                    right: Box::new(typed_right),
                    join_type,
                    condition,
                    using_columns,
                    left_plan_ids,
                    right_plan_ids,
                    derived_left_schema,
                    derived_right_schema,
                },
                resolved_schema: output_schema,
            })
        }

        // ── N-ary: SetOp with widening sub-sweep ──────────────────────────
        CommonOp::SetOp {
            kind,
            all,
            by_name,
            allow_missing_columns,
            children,
        } => {
            // UNION BY NAME is analyzed by name-matching each column across
            // children; INTERSECT / EXCEPT BY NAME are not supported by
            // DuckDB itself.
            if by_name && !matches!(kind, SetOpKind::Union) {
                return Err(AnalyzerError::PuntedOperator {
                    op: format!("SetOp[{kind:?} BY NAME]"),
                    reason: "by-name INTERSECT/EXCEPT unsupported in DuckDB".to_owned(),
                });
            }
            // Spark's Dataset API forbids `allowMissingColumns` without
            // by-name matching (PySpark's `unionByName` unconditionally sets
            // both). Reject as Spark-emulated.
            if allow_missing_columns && !by_name {
                return Err(AnalyzerError::Other {
                    reason: "allowMissingColumns requires by-name matching".to_owned(),
                });
            }
            if children.is_empty() {
                return Err(AnalyzerError::Other {
                    reason: "set-op requires at least one child".to_owned(),
                });
            }
            let mut typed_children: Vec<TypedAst> = children
                .into_iter()
                .map(|c| analyze_node(c, base_types))
                .collect::<Result<Vec<_>, _>>()?;

            // set-op widening sub-sweep (§5):
            // By-position (default): verify arity + per-column-index type
            // unify. By-name (UNION only): match columns across children by
            // NAME (case-insensitive). When `allow_missing_columns = false`,
            // each child must have the same NAME SET (Spark's strict
            // unionByName). When `allow_missing_columns = true`, the widened
            // schema is the ordered union of names — LEFT's columns first in
            // declared order, followed by RIGHT's extras in declared order
            // (Spark `ResolveUnion` rule); columns missing from a child
            // become unconditionally nullable.
            let widened_schema = if by_name {
                // First child's name order is canonical (Spark semantics).
                let first_schema = &typed_children[0].resolved_schema;
                if allow_missing_columns {
                    // Build the ordered union of names across all children.
                    // Case-insensitive dedup with first-seen casing preserved
                    // (matches `StructType::field_by_name`).
                    let mut ordered_names: Vec<String> =
                        Vec::with_capacity(first_schema.fields.len());
                    let mut seen_lower: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for child in typed_children.iter() {
                        for f in &child.resolved_schema.fields {
                            let lower = f.name.to_lowercase();
                            if seen_lower.insert(lower) {
                                ordered_names.push(f.name.clone());
                            }
                        }
                    }
                    let mut widened_fields: Vec<StructField> =
                        Vec::with_capacity(ordered_names.len());
                    for name in &ordered_names {
                        let mut widened_type: Option<DataType> = None;
                        let mut widened_nullable = false;
                        let mut any_child_missing = false;
                        for child in typed_children.iter() {
                            if let Some(fk) = child.resolved_schema.field_by_name(name) {
                                widened_type = Some(match widened_type {
                                    Some(t) => TypeInferenceEngine::unify_types(&t, &fk.data_type),
                                    None => fk.data_type.clone(),
                                });
                                widened_nullable = widened_nullable || fk.nullable;
                            } else {
                                any_child_missing = true;
                            }
                        }
                        // `widened_type` must be Some — the name came from
                        // some child so at least one child has it.
                        let ty = widened_type.ok_or_else(|| AnalyzerError::Other {
                            reason: format!(
                                "internal: union-of-names produced orphan name {name:?}"
                            ),
                        })?;
                        // Extras present in only one child become
                        // unconditionally nullable — the other child pads
                        // with NULL. Stronger than the OR rule.
                        let nullable = widened_nullable || any_child_missing;
                        widened_fields.push(StructField::new(name.clone(), ty, nullable));
                    }
                    StructType::new(widened_fields)
                } else {
                    let first_names_lower: std::collections::HashSet<String> = first_schema
                        .fields
                        .iter()
                        .map(|f| f.name.to_lowercase())
                        .collect();
                    for (idx, child) in typed_children.iter().enumerate().skip(1) {
                        let child_names_lower: std::collections::HashSet<String> = child
                            .resolved_schema
                            .fields
                            .iter()
                            .map(|f| f.name.to_lowercase())
                            .collect();
                        if child_names_lower != first_names_lower {
                            return Err(AnalyzerError::Other {
                                reason: format!(
                                    "unionByName column-name mismatch: child 0 has {:?}, child {idx} has {:?}",
                                    first_names_lower, child_names_lower
                                ),
                            });
                        }
                    }
                    let mut widened_fields: Vec<StructField> =
                        Vec::with_capacity(first_schema.fields.len());
                    for f0 in &first_schema.fields {
                        let mut widened_type = f0.data_type.clone();
                        let mut widened_nullable = f0.nullable;
                        for child in typed_children.iter().skip(1) {
                            if let Some(fk) = child
                                .resolved_schema
                                .fields
                                .iter()
                                .find(|f| f.name.eq_ignore_ascii_case(&f0.name))
                            {
                                widened_type =
                                    TypeInferenceEngine::unify_types(&widened_type, &fk.data_type);
                                widened_nullable = widened_nullable || fk.nullable;
                            }
                        }
                        widened_fields.push(StructField::new(
                            f0.name.clone(),
                            widened_type,
                            widened_nullable,
                        ));
                    }
                    StructType::new(widened_fields)
                }
            } else {
                let first_len = typed_children[0].resolved_schema.len();
                for (idx, child) in typed_children.iter().enumerate().skip(1) {
                    if child.resolved_schema.len() != first_len {
                        return Err(AnalyzerError::Other {
                            reason: format!(
                                "set-op arity mismatch: child 0 has {} columns, child {idx} has {}",
                                first_len,
                                child.resolved_schema.len()
                            ),
                        });
                    }
                }
                let mut widened_fields: Vec<StructField> = Vec::with_capacity(first_len);
                for col_idx in 0..first_len {
                    let first_field = &typed_children[0].resolved_schema.fields[col_idx];
                    // Type widening (ADR-006) is operator-independent: unify the
                    // i-th column type across every child regardless of `kind`.
                    let mut widened_type = first_field.data_type.clone();
                    for child in typed_children.iter().skip(1) {
                        let f = &child.resolved_schema.fields[col_idx];
                        widened_type =
                            TypeInferenceEngine::unify_types(&widened_type, &f.data_type);
                    }
                    // Nullability is operator-aware (Spark
                    // `basicLogicalOperators.scala`, ADR-015):
                    //   * Union     → nullable if ANY child's i-th col is
                    //                 nullable (OR-fold).
                    //   * Intersect → nullable only if EVERY child's i-th col is
                    //                 nullable (AND-fold — a value present in a
                    //                 non-nullable side cannot be null in the
                    //                 intersection).
                    //   * Except    → the LEFT (first) child's nullability only;
                    //                 output rows come from the left, so the
                    //                 other children are ignored.
                    let widened_nullable = match kind {
                        SetOpKind::Union => typed_children
                            .iter()
                            .any(|child| child.resolved_schema.fields[col_idx].nullable),
                        SetOpKind::Intersect => typed_children
                            .iter()
                            .all(|child| child.resolved_schema.fields[col_idx].nullable),
                        SetOpKind::Except => first_field.nullable,
                    };
                    widened_fields.push(StructField::new(
                        first_field.name.clone(),
                        widened_type,
                        widened_nullable,
                    ));
                }
                StructType::new(widened_fields)
            };

            // Downward push (§5.4): wrap terminal projections with CAST when
            // their column-type differs from the widened type. Only touches
            // direct `Project` children; opaque children (e.g. TableScan)
            // rely on future τ work to emit the CAST at render time.
            //
            // BY NAME: the emission wrapper (see `render_set_op`) already
            // emits per-name `CAST(<child_col> AS <widened_ty>) AS
            // <widened_name>`, matching child columns to the widened schema by
            // name. Positional pushdown is actively wrong here: the child's
            // column-order differs from the widened order by definition, so
            // wrapping `projections[i]` with `widened_schema.fields[i]`'s type
            // mis-casts columns (e.g. `salary DOUBLE → id BIGINT`). Pass 76 /
            // corpus witness: `set-003`. Skip the pushdown for by-name.
            if !by_name {
                for child in typed_children.iter_mut() {
                    push_setop_casts(child, &widened_schema);
                }
            }

            Ok(TypedAst {
                op: TypedOp::SetOp {
                    kind,
                    all,
                    by_name,
                    allow_missing_columns,
                    children: typed_children,
                    widened_schema: widened_schema.clone(),
                },
                resolved_schema: widened_schema,
            })
        }
    }
}

// ── Expression resolution helpers ───────────────────────────────────────────

/// Expand every top-level [`Expression::UnresolvedRegex`] projection into N
/// [`Expression::UnresolvedColumn`] refs — one per `input_schema` field whose
/// name matches the pattern. Non-regex projections pass through unchanged in
/// place. Schema order is preserved.
///
/// Errors:
///
/// * Invalid regex → [`AnalyzerError::Other`] (Spark-emulated — Spark rejects
///   the same input with `PatternSyntaxException`).
/// * Zero matches → [`AnalyzerError::UnknownColumn`] with the pattern as the
///   column name (mirrors Spark's `AnalysisException: cannot resolve regex`).
///
/// Called by `analyze_node`'s `CommonOp::Project` arm BEFORE
/// [`resolve_and_stamp`] so downstream analysis never sees `UnresolvedRegex`.
fn expand_regex_projections(
    projections: Vec<Expression>,
    input_schema: &StructType,
) -> Result<Vec<Expression>, AnalyzerError> {
    let mut out = Vec::with_capacity(projections.len());
    for proj in projections {
        match proj {
            Expression::UnresolvedRegex(r) => {
                let re = regex::Regex::new(&r.pattern).map_err(|e| AnalyzerError::Other {
                    reason: format!("invalid regex `{}`: {e}", r.pattern),
                })?;
                let mut matched = 0usize;
                for f in &input_schema.fields {
                    if re.is_match(&f.name) {
                        matched += 1;
                        out.push(Expression::UnresolvedColumn(UnresolvedColumn {
                            name: f.name.clone(),
                            qualifier: None,
                            plan_id: r.plan_id,
                        }));
                    }
                }
                if matched == 0 {
                    return Err(AnalyzerError::UnknownColumn {
                        name: r.pattern,
                        qualifier: None,
                    });
                }
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// Expand every top-level `F.inline(arr)` / `F.inline_outer(arr)` projection
/// into N synthetic per-struct-field projections. Non-inline projections pass
/// through unchanged in place. Schema order is preserved.
///
/// Each `inline(arr)` where `arr : Array<Struct<f1: T1, ..., fN: TN>>`
/// becomes:
///
/// ```text
/// Alias(inline_field(arr, "f1"), "f1"), ..., Alias(inline_field(arr, "fN"), "fN")
/// ```
///
/// `inline_outer(arr)` uses `inline_outer_field(...)` — same shape, but the
/// emission arm wraps `arr` in a struct-typed-NULL sentinel guard so a NULL
/// or empty array still emits one all-NULL row (matches Spark's `Inline`
/// with `outer=true`).
///
/// Errors (ADR-022 two-category):
///
/// * **Spark-emulated** ([`AnalyzerError::TypeMismatch`]) — argument is
///   proven not `Array<Struct<...>>` (e.g. `Array<Long>` or `String`). Spark
///   rejects the same input at analysis time.
/// * **Thunderduck-boundary** ([`AnalyzerError::UnsupportedRule`], Display
///   prefix `[TDCK-BOUNDARY]`) — argument's type could not be statically
///   resolved (e.g. `F.inline(F.transform(arr, lam))` with an unresolvable
///   lambda body). Honest ADR-022 non-implementation, not a silent DuckDB
///   catalog error.
/// * **Spark-emulated** ([`AnalyzerError::Other`]) — arity ≠ 1.
///
/// Called by `analyze_node`'s `CommonOp::Project` arm AFTER
/// [`expand_regex_projections`] and BEFORE [`resolve_and_stamp`] so
/// downstream analysis never sees a top-level `inline` / `inline_outer`.
fn expand_inline_projections(
    projections: Vec<Expression>,
    input_schema: &StructType,
) -> Result<Vec<Expression>, AnalyzerError> {
    let mut out = Vec::with_capacity(projections.len());
    for proj in projections {
        // Only fire on a bare top-level `FunctionCall("inline"|"inline_outer",...)`.
        // Aliased or nested forms fall through unchanged (multi-alias
        // `.alias("f1","f2",...)` and non-Project contexts are non-goals per
        // the Pass-90 plan §Non-goals — they surface as boundary errors
        // downstream if the corpus ever exercises them).
        let (name_lower, args, is_outer) = match &proj {
            Expression::FunctionCall(f) => {
                let n = f.name.to_ascii_lowercase();
                match n.as_str() {
                    "inline" => (n, f.args.clone(), false),
                    "inline_outer" => (n, f.args.clone(), true),
                    _ => {
                        out.push(proj);
                        continue;
                    }
                }
            }
            _ => {
                out.push(proj);
                continue;
            }
        };
        if args.len() != 1 {
            return Err(AnalyzerError::Other {
                reason: format!(
                    "`{name_lower}` requires exactly 1 argument, got {}",
                    args.len()
                ),
            });
        }
        let arr = args.into_iter().next().expect("checked len == 1 above");
        let arg_ty = arr.data_type(input_schema);
        let (elem_struct, contains_null) = match arg_ty {
            DataType::Array(inner, cn) => match *inner {
                DataType::Struct(st) => (st, cn),
                DataType::Unresolved => {
                    return Err(AnalyzerError::UnsupportedRule {
                        rule: format!("{name_lower}-expansion"),
                        reason: format!(
                            "`{name_lower}` argument's element type could not be statically resolved — τ requires a resolved `Array<Struct<...>>` schema"
                        ),
                    });
                }
                other => {
                    return Err(AnalyzerError::TypeMismatch {
                        expected: DataType::Struct(StructType::new(vec![])),
                        actual: other,
                        context: format!("`{name_lower}` argument element type"),
                    });
                }
            },
            DataType::Unresolved => {
                return Err(AnalyzerError::UnsupportedRule {
                    rule: format!("{name_lower}-expansion"),
                    reason: format!(
                        "`{name_lower}` argument's type could not be statically resolved — τ requires a resolved `Array<Struct<...>>` schema"
                    ),
                });
            }
            other => {
                return Err(AnalyzerError::TypeMismatch {
                    expected: DataType::Array(
                        Box::new(DataType::Struct(StructType::new(vec![]))),
                        true,
                    ),
                    actual: other,
                    context: format!("`{name_lower}` argument"),
                });
            }
        };
        // `contains_null` is carried on the synthesized arr's `DataType`
        // itself via `Expression::data_type` at emission / nullability time;
        // no need to thread it through the synthetic call's args.
        let _ = contains_null;
        let synthetic_name = if is_outer {
            "inline_outer_field"
        } else {
            "inline_field"
        };
        for field in &elem_struct.fields {
            let field_name_lit = Expression::Literal(Literal {
                value: LiteralValue::String(field.name.clone()),
                data_type: DataType::String,
            });
            let call = Expression::FunctionCall(FunctionCall {
                name: synthetic_name.to_owned(),
                args: vec![arr.clone(), field_name_lit],
                distinct: false,
            });
            out.push(Expression::Alias(AliasExpression {
                expr: Box::new(call),
                alias: field.name.clone(),
            }));
        }
    }
    Ok(out)
}

/// Character set rejected inside a `json_tuple` key literal. See
/// [`expand_json_tuple_projections`] for rationale.
fn json_tuple_key_char_is_unsafe(c: char) -> bool {
    // Quoting hazards for a single-quoted SQL literal, plus JSONPath tokens
    // that would change Spark's flat-key lookup semantics if forwarded to
    // DuckDB's `json_extract_string`.
    matches!(c, '\'' | '"' | '\\' | '.' | '[' | ']') || c.is_ascii_control()
}

/// Expand every top-level `F.json_tuple(json, k1, ..., kN)` projection into
/// N synthetic per-key projections. Non-`json_tuple` projections pass through
/// unchanged in place. Schema order is preserved.
///
/// Each `json_tuple(j, k1, ..., kN)` becomes:
///
/// ```text
/// Alias(json_tuple_field(j, "k1"), "c0"), ..., Alias(json_tuple_field(j, "kN"), "cN-1")
/// ```
///
/// Names are POSITIONAL (`c0, c1, ..., c<N-1>`) — matches Spark's
/// `Generator.elementSchema`, NOT the key literals. Verified against
/// PySpark docstring `pyspark/sql/functions/builtin.py:20737`. Corpus witness
/// `json-002` uses bare (no `.alias(...)`) `json_tuple`.
///
/// Errors (ADR-022 two-category):
///
/// * **Spark-emulated** ([`AnalyzerError::Other`]) — arity < 2 (Spark rejects
///   `json_tuple(x)` with zero keys at analysis time).
/// * **Spark-emulated** ([`AnalyzerError::TypeMismatch`]) — a key arg is not
///   a `Literal::String` (Catalyst's `JsonTuple.checkInputDataTypes` rejects
///   non-literal field names).
/// * **Thunderduck-boundary** ([`AnalyzerError::UnsupportedRule`], Display
///   prefix `[TDCK-BOUNDARY]`, `rule = "json_tuple-expansion"`) — a key
///   contains a character in `json_tuple_key_char_is_unsafe`. `'` / `\` / `"`
///   / ASCII control would break the bare single-quoted SQL literal; `.` /
///   `[` / `]` would cause DuckDB's `json_extract_string('$.<key>')` to
///   path-walk whereas Spark treats those characters as flat key literals.
///
/// Called by `analyze_node`'s `CommonOp::Project` arm AFTER
/// [`expand_inline_projections`] and BEFORE [`resolve_and_stamp`], so
/// downstream analysis never sees a top-level `json_tuple`.
fn expand_json_tuple_projections(
    projections: Vec<Expression>,
) -> Result<Vec<Expression>, AnalyzerError> {
    let mut out = Vec::with_capacity(projections.len());
    for proj in projections {
        // Only fire on a bare top-level `FunctionCall("json_tuple", ...)`.
        // Aliased or nested forms fall through unchanged (multi-alias
        // `.alias("k1", ...)` and non-Project contexts are non-goals per
        // the Pass-91 plan §Non-goals — they surface as boundary errors
        // downstream if the corpus ever exercises them).
        let args = match &proj {
            Expression::FunctionCall(f) if f.name.eq_ignore_ascii_case("json_tuple") => {
                f.args.clone()
            }
            _ => {
                out.push(proj);
                continue;
            }
        };
        if args.len() < 2 {
            return Err(AnalyzerError::Other {
                reason: format!(
                    "`json_tuple` requires at least 2 arguments (json_str, key_1, ...), got {}",
                    args.len()
                ),
            });
        }
        let mut args_iter = args.into_iter();
        let json_expr = args_iter.next().expect("checked args.len() >= 2 above");
        let key_args: Vec<Expression> = args_iter.collect();
        for (i, key_arg) in key_args.into_iter().enumerate() {
            let key = match &key_arg {
                Expression::Literal(Literal {
                    value: LiteralValue::String(s),
                    ..
                }) => s.clone(),
                other => {
                    return Err(AnalyzerError::TypeMismatch {
                        expected: DataType::String,
                        actual: other.data_type(&StructType::new(vec![])),
                        context: format!(
                            "`json_tuple` field-name at position {} must be a string literal",
                            i + 1
                        ),
                    });
                }
            };
            if key.chars().any(json_tuple_key_char_is_unsafe) {
                return Err(AnalyzerError::UnsupportedRule {
                    rule: "json_tuple-expansion".to_owned(),
                    reason: format!(
                        "`json_tuple` key `{key}` contains a character τ does not \
                         safely forward to DuckDB's `json_extract_string` — reject \
                         to avoid diverging from Spark's flat-key semantics or \
                         breaking the SQL string literal"
                    ),
                });
            }
            let key_lit = Expression::Literal(Literal {
                value: LiteralValue::String(key),
                data_type: DataType::String,
            });
            let call = Expression::FunctionCall(FunctionCall {
                name: "json_tuple_field".to_owned(),
                args: vec![json_expr.clone(), key_lit],
                distinct: false,
            });
            out.push(Expression::Alias(AliasExpression {
                expr: Box::new(call),
                alias: format!("c{i}"),
            }));
        }
    }
    Ok(out)
}

/// Resolve every `UnresolvedColumn` in `expr` against `schema` and stamp
/// resolved `ColumnReference`s with `data_type` and `nullable`.
fn resolve_and_stamp(
    expr: Expression,
    schema: &StructType,
    base_types: &BaseTypes,
) -> Result<Expression, AnalyzerError> {
    match expr {
        Expression::UnresolvedColumn(u) => resolve_column(u, schema),
        Expression::ColumnReference(c) => {
            let stamped = stamp_column_reference(c, schema);
            Ok(Expression::ColumnReference(stamped))
        }
        Expression::Literal(_) | Expression::Star(_) => Ok(expr),
        Expression::Binary(mut b) => {
            b.left = Box::new(resolve_and_stamp(*b.left, schema, base_types)?);
            b.right = Box::new(resolve_and_stamp(*b.right, schema, base_types)?);
            Ok(Expression::Binary(b))
        }
        Expression::Unary(mut u) => {
            u.operand = Box::new(resolve_and_stamp(*u.operand, schema, base_types)?);
            Ok(Expression::Unary(u))
        }
        Expression::FunctionCall(mut f) => {
            f.args = f
                .args
                .into_iter()
                .map(|a| resolve_and_stamp(a, schema, base_types))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::FunctionCall(f))
        }
        Expression::Cast(mut c) => {
            c.expr = Box::new(resolve_and_stamp(*c.expr, schema, base_types)?);
            Ok(Expression::Cast(c))
        }
        Expression::CaseWhen(mut cw) => {
            cw.branches = cw
                .branches
                .into_iter()
                .map(|(w, t)| {
                    let w = resolve_and_stamp(w, schema, base_types)?;
                    let t = resolve_and_stamp(t, schema, base_types)?;
                    Ok::<_, AnalyzerError>((w, t))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(e) = cw.else_expr {
                cw.else_expr = Some(Box::new(resolve_and_stamp(*e, schema, base_types)?));
            }
            Ok(Expression::CaseWhen(cw))
        }
        Expression::Window(mut w) => {
            w.func = Box::new(resolve_and_stamp(*w.func, schema, base_types)?);
            w.partition_by = w
                .partition_by
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema, base_types))
                .collect::<Result<Vec<_>, _>>()?;
            let mut new_order = Vec::with_capacity(w.order_by.len());
            for so in w.order_by {
                let e = resolve_and_stamp(*so.expr, schema, base_types)?;
                new_order.push(SortOrder {
                    expr: Box::new(e),
                    direction: so.direction,
                    null_ordering: so.null_ordering,
                });
            }
            w.order_by = new_order;
            Ok(Expression::Window(w))
        }
        Expression::Alias(mut a) => {
            a.expr = Box::new(resolve_and_stamp(*a.expr, schema, base_types)?);
            Ok(Expression::Alias(a))
        }
        Expression::Between(mut b) => {
            b.expr = Box::new(resolve_and_stamp(*b.expr, schema, base_types)?);
            b.low = Box::new(resolve_and_stamp(*b.low, schema, base_types)?);
            b.high = Box::new(resolve_and_stamp(*b.high, schema, base_types)?);
            Ok(Expression::Between(b))
        }
        Expression::InList(mut i) => {
            i.expr = Box::new(resolve_and_stamp(*i.expr, schema, base_types)?);
            i.list = i
                .list
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema, base_types))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::InList(i))
        }
        Expression::Like(mut l) => {
            l.value = Box::new(resolve_and_stamp(*l.value, schema, base_types)?);
            l.pattern = Box::new(resolve_and_stamp(*l.pattern, schema, base_types)?);
            Ok(Expression::Like(l))
        }
        Expression::IsDistinctFrom(mut d) => {
            d.left = Box::new(resolve_and_stamp(*d.left, schema, base_types)?);
            d.right = Box::new(resolve_and_stamp(*d.right, schema, base_types)?);
            Ok(Expression::IsDistinctFrom(d))
        }
        Expression::ExtractValue(mut ev) => {
            ev.child = Box::new(resolve_and_stamp(*ev.child, schema, base_types)?);
            ev.extraction = Box::new(resolve_and_stamp(*ev.extraction, schema, base_types)?);
            Ok(Expression::ExtractValue(ev))
        }
        Expression::ArrayLiteral(mut a) => {
            a.elements = a
                .elements
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema, base_types))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::ArrayLiteral(a))
        }
        Expression::MapLiteral(mut m) => {
            m.entries = m
                .entries
                .into_iter()
                .map(|(k, v)| {
                    let k = resolve_and_stamp(k, schema, base_types)?;
                    let v = resolve_and_stamp(v, schema, base_types)?;
                    Ok::<_, AnalyzerError>((k, v))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::MapLiteral(m))
        }
        Expression::StructLiteral(mut s) => {
            s.fields = s
                .fields
                .into_iter()
                .map(|(n, e)| {
                    Ok::<_, AnalyzerError>((n, resolve_and_stamp(e, schema, base_types)?))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::StructLiteral(s))
        }
        Expression::RowConstructor(mut rc) => {
            rc.elements = rc
                .elements
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema, base_types))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::RowConstructor(rc))
        }
        Expression::UpdateFields(mut u) => {
            u.struct_expr = Box::new(resolve_and_stamp(*u.struct_expr, schema, base_types)?);
            u.updates = u
                .updates
                .into_iter()
                .map(|(n, e)| {
                    let resolved = match e {
                        Some(expr) => Some(resolve_and_stamp(expr, schema, base_types)?),
                        None => None,
                    };
                    Ok::<_, AnalyzerError>((n, resolved))
                })
                .collect::<Result<Vec<_>, _>>()?;
            // Spark 4.1 rejects `dropFields("X")` when field `X` does not
            // exist (case-insensitive) with `AnalysisException: Field name
            // X does not exist`. We surface the same as a Spark-emulated
            // error at analysis time so emission never runs on invalid ops.
            // See Catalyst `UpdateFields.scala::checkInputDataTypes`.
            if let DataType::Struct(base_st) = u.struct_expr.data_type(schema) {
                let base_names: Vec<String> =
                    base_st.fields.iter().map(|f| f.name.clone()).collect();
                if let Err(missing) =
                    super::expression::validate_update_fields_ops(&base_names, &u.updates)
                {
                    return Err(AnalyzerError::Other {
                        reason: format!(
                            "cannot resolve field `{missing}` in dropFields — not present in struct"
                        ),
                    });
                }
            }
            Ok(Expression::UpdateFields(u))
        }
        // Uncorrelated subqueries: analyze the inner plan against the SAME
        // `base_types` and carry the typed plan forward so emission renders it
        // node-local (ADR-007 A / INV2). A correlated inner ref to an outer
        // column is an `UnresolvedColumn` this isolated `analyze` cannot
        // resolve → resolution error → honest Thunderduck boundary (ADR-022).
        Expression::ScalarSubquery(mut s) => {
            let inner = analyze_subquery_plan(s.subquery, base_types)?;
            if inner.resolved_schema.fields.len() != 1 {
                return Err(AnalyzerError::Other {
                    reason: "scalar subquery must return exactly one column".to_owned(),
                });
            }
            s.subquery = SubqueryPlan::Analyzed(Box::new(inner));
            Ok(Expression::ScalarSubquery(s))
        }
        Expression::InSubquery(mut i) => {
            i.expr = Box::new(resolve_and_stamp(*i.expr, schema, base_types)?);
            let inner = analyze_subquery_plan(i.subquery, base_types)?;
            if inner.resolved_schema.fields.len() != 1 {
                return Err(AnalyzerError::Other {
                    reason: "IN subquery must return exactly one column".to_owned(),
                });
            }
            i.subquery = SubqueryPlan::Analyzed(Box::new(inner));
            Ok(Expression::InSubquery(i))
        }
        Expression::ExistsSubquery(mut e) => {
            let inner = analyze_subquery_plan(e.subquery, base_types)?;
            e.subquery = SubqueryPlan::Analyzed(Box::new(inner));
            Ok(Expression::ExistsSubquery(e))
        }
        // Lambda / raw-sql / interval — left opaque.
        Expression::Lambda(_)
        | Expression::LambdaVariable(_)
        | Expression::RawSql(_)
        | Expression::Interval(_) => Ok(expr),
        // Defensive — Pass 85's `expand_regex_projections` pre-pass in
        // `CommonOp::Project` rewrites this variant before we walk here.
        // Any residual (e.g. nested inside another expression) is passed
        // through opaquely; emission's defensive arm surfaces the error.
        Expression::UnresolvedRegex(_) => Ok(expr),
    }
}

fn resolve_expr_list(
    exprs: Vec<Expression>,
    schema: &StructType,
    base_types: &BaseTypes,
) -> Result<Vec<Expression>, AnalyzerError> {
    exprs
        .into_iter()
        .map(|e| resolve_and_stamp(e, schema, base_types))
        .collect()
}

/// Analyze an embedded subquery's inner plan against `base_types`. The plan is
/// `Unanalyzed` when produced by the front-end; an already-`Analyzed` plan is
/// returned unchanged (idempotent — the analyzer normally runs each plan once).
fn analyze_subquery_plan(
    plan: SubqueryPlan,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    match plan {
        SubqueryPlan::Unanalyzed(inner) => analyze(*inner, base_types),
        SubqueryPlan::Analyzed(inner) => Ok(*inner),
    }
}

/// Synthetic qualifier attached to plan_id-tagged column refs during Join
/// condition analysis. Emission renders `ColumnReference { qualifier:
/// Some(TD_JOIN_LEFT), .. }` as `__td_jl.<col>`, which matches the
/// left/right subquery aliases `render_join` emits.
pub(crate) const TD_JOIN_LEFT: &str = "__td_jl";
pub(crate) const TD_JOIN_RIGHT: &str = "__td_jr";

/// Walk an expression and replace `UnresolvedColumn { qualifier: None,
/// plan_id: Some(N) }` with `UnresolvedColumn { qualifier: Some("__td_jl"),
/// .. }` or `Some("__td_jr")` based on which side `N` belongs to. Leaves
/// qualifier-set references and plan_id-free references untouched.
fn qualify_plan_id_refs(expr: Expression, left_ids: &[i64], right_ids: &[i64]) -> Expression {
    fn walk(e: Expression, left_ids: &[i64], right_ids: &[i64]) -> Expression {
        match e {
            Expression::UnresolvedColumn(u) if u.qualifier.is_none() => {
                let synth = match u.plan_id {
                    Some(pid) if left_ids.contains(&pid) => Some(TD_JOIN_LEFT.to_owned()),
                    Some(pid) if right_ids.contains(&pid) => Some(TD_JOIN_RIGHT.to_owned()),
                    _ => None,
                };
                if let Some(q) = synth {
                    Expression::UnresolvedColumn(UnresolvedColumn {
                        name: u.name,
                        qualifier: Some(q),
                        plan_id: u.plan_id,
                    })
                } else {
                    Expression::UnresolvedColumn(u)
                }
            }
            Expression::Binary(b) => Expression::Binary(BinaryExpression {
                op: b.op,
                left: Box::new(walk(*b.left, left_ids, right_ids)),
                right: Box::new(walk(*b.right, left_ids, right_ids)),
            }),
            Expression::Unary(u) => Expression::Unary(UnaryExpression {
                op: u.op,
                operand: Box::new(walk(*u.operand, left_ids, right_ids)),
            }),
            Expression::FunctionCall(f) => Expression::FunctionCall(FunctionCall {
                name: f.name,
                args: f
                    .args
                    .into_iter()
                    .map(|a| walk(a, left_ids, right_ids))
                    .collect(),
                distinct: f.distinct,
            }),
            Expression::Cast(c) => Expression::Cast(CastExpression {
                expr: Box::new(walk(*c.expr, left_ids, right_ids)),
                to_type: c.to_type,
                try_cast: c.try_cast,
            }),
            Expression::CaseWhen(cw) => Expression::CaseWhen(CaseWhenExpression {
                branches: cw
                    .branches
                    .into_iter()
                    .map(|(c, v)| (walk(c, left_ids, right_ids), walk(v, left_ids, right_ids)))
                    .collect(),
                else_expr: cw
                    .else_expr
                    .map(|e| Box::new(walk(*e, left_ids, right_ids))),
            }),
            Expression::Alias(a) => Expression::Alias(AliasExpression {
                alias: a.alias,
                expr: Box::new(walk(*a.expr, left_ids, right_ids)),
            }),
            other => other,
        }
    }
    walk(expr, left_ids, right_ids)
}

/// Detect multi-level nested-struct field access like `F.col("address.geo.lat")`
/// and rewrite it as an `ExtractValue` chain rooted at the top-level struct
/// column. Returns `None` when the input is not a nested-struct path or when
/// the tail does not resolve against the schema; callers fall back to the
/// standard column-resolution logic.
///
/// Requirements for a rewrite:
/// * `u.qualifier` is `Some(q)`
/// * `q` is not a synthetic join qualifier (`__td_jl` / `__td_jr`) and has no
///   `plan_id` attached (both signal a table-level qualifier, not struct nav)
/// * `u.name` contains at least one `.` (single-level `qualifier.name` already
///   emits correctly as `"qualifier"."name"` in DuckDB)
/// * `q` names a top-level struct column in `schema` and the dot-separated
///   segments of `u.name` traverse a chain of struct fields
fn try_rewrite_nested_struct_path(u: &UnresolvedColumn, schema: &StructType) -> Option<Expression> {
    if u.plan_id.is_some() {
        return None;
    }
    if !u.name.contains('.') {
        return None;
    }
    let qualifier = u.qualifier.as_deref()?;
    if qualifier == TD_JOIN_LEFT || qualifier == TD_JOIN_RIGHT {
        return None;
    }
    let root_field = schema.field_by_name(qualifier)?;
    let mut current_type = match &root_field.data_type {
        DataType::Struct(st) => st.clone(),
        _ => return None,
    };
    let segments: Vec<&str> = u.name.split('.').collect();
    // Validate every intermediate segment is a struct-typed field before
    // committing to a rewrite. If any segment fails to resolve, return None
    // and let the standard resolver emit a proper `UnknownColumn` error.
    for seg in &segments[..segments.len() - 1] {
        let f = current_type.field_by_name(seg)?;
        match &f.data_type {
            DataType::Struct(st) => current_type = st.clone(),
            _ => return None,
        }
    }
    // Terminal segment must be an existing field on the innermost struct.
    let last = segments.last()?;
    current_type.field_by_name(last)?;

    // Build the chain bottom-up starting from a bare ColumnReference to the
    // top-level struct column. Type/nullable are stamped lazily by the
    // ExtractValue derivations at emission time; leaving them as `None` on
    // the root ColumnReference is fine because we immediately re-run
    // `stamp_column_reference` via the normal walk (the caller path stamps
    // any embedded ColumnReferences on subsequent visits).
    let mut expr = Expression::ColumnReference(ColumnReference {
        name: qualifier.to_owned(),
        qualifier: None,
        data_type: Some(root_field.data_type.clone()),
        nullable: Some(root_field.nullable),
    });
    for seg in &segments {
        expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(expr),
            extraction: Box::new(Expression::Literal(Literal {
                value: LiteralValue::String((*seg).to_owned()),
                data_type: DataType::String,
            })),
        });
    }
    Some(expr)
}

fn resolve_column(u: UnresolvedColumn, schema: &StructType) -> Result<Expression, AnalyzerError> {
    // Multi-level nested-struct navigation: `F.col("address.geo.lat")` arrives
    // here as `UnresolvedColumn { qualifier: Some("address"), name: "geo.lat" }`
    // (the Spark Connect converter does a single `splitn(2, '.')`). Emitting
    // this ColumnReference verbatim produces `"address"."geo.lat"` which DuckDB
    // rejects because it treats `geo.lat` as a single field key. When the
    // qualifier matches a top-level struct column and the tail is a valid
    // nested field path, rewrite as an `ExtractValue` chain so emission goes
    // through the struct-field access path.
    if let Some(chain) = try_rewrite_nested_struct_path(&u, schema) {
        return Ok(chain);
    }
    // Qualified: `qualifier.name` — the analyzer accepts both a top-level
    // qualifier column (a struct field access) and a direct match on the
    // outer name; ambiguity is not surfaced for qualified references at
    // τ's analyzer (the plan_id + qualifier disambiguation lands in future τ work's
    // rendering; here we resolve type/nullability).
    //
    // Unqualified: surface `AmbiguousColumn` whenever more than one field
    // (case-insensitive match, matching `field_by_name`'s Spark-compatible
    // rule) resolves. This is the single, central ambiguity check point —
    // it catches ambiguity everywhere a column reference is resolved
    // (projections, filters, sort keys, join conditions, ...), not just in
    // join conditions.
    // Synthetic __td_jl / __td_jr qualifiers set by `qualify_plan_id_refs`
    // are "trust the caller" markers — resolve type/nullable against the
    // merged schema by name alone, and preserve the qualifier for
    // emission. The merged schema by construction has the field on the
    // corresponding side; picking the first match by name is correct.
    let is_synthetic_join_qualifier = matches!(
        u.qualifier.as_deref(),
        Some(TD_JOIN_LEFT) | Some(TD_JOIN_RIGHT)
    );
    if u.qualifier.is_none() {
        let matches: Vec<&StructField> = schema
            .fields
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case(&u.name))
            .collect();
        if matches.len() > 1 {
            let candidates = matches.iter().map(|f| f.name.clone()).collect();
            return Err(AnalyzerError::AmbiguousColumn {
                name: u.name,
                candidates,
            });
        }
    }
    let dt = if is_synthetic_join_qualifier {
        // Resolve by NAME against the merged schema (qualifier isn't a
        // real schema field, it's an emission-side alias).
        schema
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(&u.name))
            .map(|f| f.data_type.clone())
            .unwrap_or(DataType::Unresolved)
    } else {
        TypeInferenceEngine::qualified_column_type(&u.name, u.qualifier.as_deref(), schema)
    };
    let nullable = if is_synthetic_join_qualifier {
        schema
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(&u.name))
            .map(|f| f.nullable)
            .unwrap_or(true)
    } else {
        TypeInferenceEngine::qualified_column_nullable(&u.name, u.qualifier.as_deref(), schema)
    };
    if matches!(dt, DataType::Unresolved) {
        return Err(AnalyzerError::UnknownColumn {
            name: u.name,
            qualifier: u.qualifier,
        });
    }
    Ok(Expression::ColumnReference(ColumnReference {
        name: u.name,
        qualifier: u.qualifier,
        data_type: Some(dt),
        nullable: Some(nullable),
    }))
}

fn stamp_column_reference(c: ColumnReference, schema: &StructType) -> ColumnReference {
    let dt = c.data_type.clone().unwrap_or_else(|| {
        TypeInferenceEngine::qualified_column_type(&c.name, c.qualifier.as_deref(), schema)
    });
    let nullable = c.nullable.unwrap_or_else(|| {
        TypeInferenceEngine::qualified_column_nullable(&c.name, c.qualifier.as_deref(), schema)
    });
    ColumnReference {
        name: c.name,
        qualifier: c.qualifier,
        data_type: Some(dt),
        nullable: Some(nullable),
    }
}

/// Return `true` iff any `Expression::UnresolvedColumn` remains, or any
/// `ColumnReference` has `data_type = None` / `nullable = None`.
fn expression_is_fully_resolved(expr: &Expression) -> bool {
    match expr {
        Expression::UnresolvedColumn(_) => false,
        Expression::ColumnReference(c) => c.data_type.is_some() && c.nullable.is_some(),
        Expression::Literal(_) | Expression::Star(_) | Expression::LambdaVariable(_) => true,
        Expression::Binary(b) => {
            expression_is_fully_resolved(&b.left) && expression_is_fully_resolved(&b.right)
        }
        Expression::Unary(u) => expression_is_fully_resolved(&u.operand),
        Expression::FunctionCall(f) => f.args.iter().all(expression_is_fully_resolved),
        Expression::Cast(c) => expression_is_fully_resolved(&c.expr),
        Expression::CaseWhen(cw) => {
            cw.branches
                .iter()
                .all(|(w, t)| expression_is_fully_resolved(w) && expression_is_fully_resolved(t))
                && cw
                    .else_expr
                    .as_ref()
                    .is_none_or(|e| expression_is_fully_resolved(e))
        }
        Expression::Window(w) => {
            expression_is_fully_resolved(&w.func)
                && w.partition_by.iter().all(expression_is_fully_resolved)
                && w.order_by
                    .iter()
                    .all(|so| expression_is_fully_resolved(&so.expr))
        }
        Expression::Alias(a) => expression_is_fully_resolved(&a.expr),
        Expression::Between(b) => {
            expression_is_fully_resolved(&b.expr)
                && expression_is_fully_resolved(&b.low)
                && expression_is_fully_resolved(&b.high)
        }
        Expression::InList(i) => {
            expression_is_fully_resolved(&i.expr) && i.list.iter().all(expression_is_fully_resolved)
        }
        Expression::Like(l) => {
            expression_is_fully_resolved(&l.value) && expression_is_fully_resolved(&l.pattern)
        }
        Expression::IsDistinctFrom(d) => {
            expression_is_fully_resolved(&d.left) && expression_is_fully_resolved(&d.right)
        }
        Expression::ExtractValue(ev) => {
            expression_is_fully_resolved(&ev.child) && expression_is_fully_resolved(&ev.extraction)
        }
        Expression::ArrayLiteral(a) => a.elements.iter().all(expression_is_fully_resolved),
        Expression::MapLiteral(m) => m
            .entries
            .iter()
            .all(|(k, v)| expression_is_fully_resolved(k) && expression_is_fully_resolved(v)),
        Expression::StructLiteral(s) => s
            .fields
            .iter()
            .all(|(_, e)| expression_is_fully_resolved(e)),
        Expression::RowConstructor(rc) => rc.elements.iter().all(expression_is_fully_resolved),
        Expression::UpdateFields(u) => {
            expression_is_fully_resolved(&u.struct_expr)
                && u.updates.iter().all(|(_, e)| match e {
                    Some(expr) => expression_is_fully_resolved(expr),
                    None => true,
                })
        }
        // Subquery bodies must be analyzed: the inner plan is fully resolved
        // only once the analyzer has rewritten `Unanalyzed` → `Analyzed`.
        Expression::ScalarSubquery(s) => subquery_plan_is_resolved(&s.subquery),
        Expression::InSubquery(i) => {
            expression_is_fully_resolved(&i.expr) && subquery_plan_is_resolved(&i.subquery)
        }
        Expression::ExistsSubquery(e) => subquery_plan_is_resolved(&e.subquery),
        // Lambda / raw-sql / interval — opaque.
        Expression::Lambda(_) | Expression::RawSql(_) | Expression::Interval(_) => true,
        // Pass 85 — pattern-driven column expander; expanded away by
        // `expand_regex_projections` in the `CommonOp::Project` pre-pass.
        // If it survives to this check, treat it as unresolved.
        Expression::UnresolvedRegex(_) => false,
    }
}

/// A subquery's inner plan is resolved only once the analyzer has stamped it
/// (`Analyzed`) and every node under it carries a resolved schema.
fn subquery_plan_is_resolved(plan: &SubqueryPlan) -> bool {
    match plan {
        SubqueryPlan::Analyzed(inner) => has_resolved_schema(inner),
        SubqueryPlan::Unanalyzed(_) => false,
    }
}

// ── Schema / expression naming ──────────────────────────────────────────────

fn schema_has_unresolved(schema: &StructType) -> bool {
    schema
        .fields
        .iter()
        .any(|f| f.data_type.contains_unresolved())
}

fn apply_alias_to_schema(schema: &StructType, alias: Option<&str>) -> StructType {
    // At τ's analyzer, we don't rewrite field qualifiers into names — the alias
    // is preserved on the operator itself. future τ work's renderer handles the
    // alias projection.
    let _ = alias;
    schema.clone()
}

fn project_output_schema(
    projections: &[Expression],
    input_schema: &StructType,
) -> Result<StructType, AnalyzerError> {
    let mut fields: Vec<StructField> = Vec::with_capacity(projections.len());
    for expr in projections {
        match expr {
            Expression::Star(s) => {
                // Star: expand at schema level. Qualified star: filter by
                // struct field / qualifier (τ's analyzer keeps it simple —
                // qualifier match against field name).
                match &s.qualifier {
                    None => {
                        fields.extend(input_schema.fields.iter().cloned());
                    }
                    Some(q) => {
                        // If the qualifier matches a struct field, expand
                        // that struct's inner fields.
                        if let Some(f) = input_schema.field_by_name(q) {
                            if let DataType::Struct(st) = &f.data_type {
                                let base_nullable = f.nullable;
                                for inner in &st.fields {
                                    fields.push(StructField::new(
                                        inner.name.clone(),
                                        inner.data_type.clone(),
                                        base_nullable || inner.nullable,
                                    ));
                                }
                                continue;
                            }
                        }
                        // Unknown qualifier — do NOT silently expand as `*`.
                        // Surface as an UnknownColumn error so `SELECT
                        // nonexistent.*` produces the same Spark-emulated
                        // diagnostic as an unqualified `nonexistent`.
                        return Err(AnalyzerError::UnknownColumn {
                            name: format!("{q}.*"),
                            qualifier: Some(q.clone()),
                        });
                    }
                }
            }
            other => {
                let name = expression_output_name(other);
                let dt = other.data_type(input_schema);
                let nullable = other.nullable(input_schema);
                fields.push(StructField::new(name, dt, nullable));
            }
        }
    }
    Ok(StructType::new(fields))
}

// ── Unpivot analysis ────────────────────────────────────────────────────────

/// Analyze `CommonOp::Unpivot`: resolve the input, materialise the `values`
/// list (empty ⇒ all non-id input columns per Spark), then stamp the output
/// schema as `<ids> + (variable_column_name: STRING NOT NULL,
/// value_column_name: T)` where `T` is Spark's numeric widening (via
/// [`TypeInferenceEngine::unify_types`]) across the resolved input types of
/// the `values` columns; the value column is nullable iff any source value
/// column is nullable.
fn analyze_unpivot(
    input: CommonAst,
    ids: UnpivotIds,
    values: Vec<String>,
    variable_column_name: String,
    value_column_name: String,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types)?;
    let input_schema = &typed_input.resolved_schema;

    // OPT-1: build a lowercase-keyed lookup once (case-insensitive per Spark
    // identifier semantics). Turns O(V·F) name resolution across the id and
    // value lists — plus the empty-values fallback's O(F·I) filter — into
    // O(F + I + V) total.
    let field_index: HashMap<String, &StructField> = input_schema
        .fields
        .iter()
        .map(|f| (f.name.to_ascii_lowercase(), f))
        .collect();
    let find_field = |name: &str| -> Option<&StructField> {
        field_index.get(&name.to_ascii_lowercase()).copied()
    };

    // Resolve the id list. The DataFrame path supplies ids explicitly; SQL
    // `UNPIVOT` supplies only value columns, so the analyzer derives the ids as
    // `input schema − value columns` (input order) per Spark parity (ADR-015).
    let ids: Vec<String> = match ids {
        UnpivotIds::Explicit(v) => v,
        UnpivotIds::Implicit => {
            if values.is_empty() {
                return Err(AnalyzerError::Other {
                    reason: "SQL UNPIVOT requires at least one value column".to_owned(),
                });
            }
            // Validate each value column resolves before deriving ids from it.
            for v in &values {
                if find_field(v).is_none() {
                    return Err(AnalyzerError::UnknownColumn {
                        name: v.clone(),
                        qualifier: None,
                    });
                }
            }
            let value_set: std::collections::HashSet<String> =
                values.iter().map(|v| v.to_ascii_lowercase()).collect();
            input_schema
                .fields
                .iter()
                .filter(|f| !value_set.contains(&f.name.to_ascii_lowercase()))
                .map(|f| f.name.clone())
                .collect()
        }
    };

    // Validate every id column resolves.
    for id in &ids {
        if find_field(id).is_none() {
            return Err(AnalyzerError::UnknownColumn {
                name: id.clone(),
                qualifier: None,
            });
        }
    }

    // Materialise `values`: empty ⇒ all non-id input columns (Spark default).
    let materialised_values: Vec<String> = if values.is_empty() {
        let id_set: std::collections::HashSet<String> =
            ids.iter().map(|id| id.to_ascii_lowercase()).collect();
        input_schema
            .fields
            .iter()
            .filter(|f| !id_set.contains(&f.name.to_ascii_lowercase()))
            .map(|f| f.name.clone())
            .collect()
    } else {
        // Validate each named value column resolves.
        for v in &values {
            if find_field(v).is_none() {
                return Err(AnalyzerError::UnknownColumn {
                    name: v.clone(),
                    qualifier: None,
                });
            }
        }
        values
    };

    if materialised_values.is_empty() {
        return Err(AnalyzerError::Other {
            reason:
                "unpivot requires at least one value column (none supplied and no non-id columns)"
                    .to_owned(),
        });
    }

    // M2: reject duplicate/overlapping names across the union of id + value
    // columns (case-insensitive per Spark identifier semantics). Spark itself
    // rejects overlap between ids and values; τ mirrors that Spark-emulated
    // behavior with `AnalyzerError::Other`.
    {
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(ids.len() + materialised_values.len());
        for name in ids.iter().chain(materialised_values.iter()) {
            let key = name.to_ascii_lowercase();
            if !seen.insert(key) {
                return Err(AnalyzerError::Other {
                    reason: format!(
                        "unpivot id and value columns must be disjoint and unique; duplicate name: {name}"
                    ),
                });
            }
        }
    }

    // M3: reject collisions between the synthetic variable/value column names
    // and any id column (case-insensitive). Otherwise the stamped output
    // schema would carry two fields with the same name — Spark rejects this.
    for id in &ids {
        if id.eq_ignore_ascii_case(&variable_column_name) {
            return Err(AnalyzerError::Other {
                reason: format!(
                    "unpivot variable column name '{variable_column_name}' collides with id column '{id}'"
                ),
            });
        }
        if id.eq_ignore_ascii_case(&value_column_name) {
            return Err(AnalyzerError::Other {
                reason: format!(
                    "unpivot value column name '{value_column_name}' collides with id column '{id}'"
                ),
            });
        }
    }
    if variable_column_name.eq_ignore_ascii_case(&value_column_name) {
        return Err(AnalyzerError::Other {
            reason: format!(
                "unpivot variable and value column names must differ; both are '{variable_column_name}'"
            ),
        });
    }

    // Widen value-column types across `materialised_values`.
    let mut widened_type = DataType::Unresolved;
    let mut widened_nullable = false;
    for v in &materialised_values {
        let field = find_field(v).expect("value column resolved above");
        if matches!(widened_type, DataType::Unresolved) {
            widened_type = field.data_type.clone();
        } else {
            widened_type = TypeInferenceEngine::unify_types(&widened_type, &field.data_type);
        }
        if field.nullable {
            widened_nullable = true;
        }
    }

    // Build output schema: <ids> + variable_col (STRING NOT NULL) + value_col.
    let mut output_fields: Vec<StructField> = Vec::with_capacity(ids.len() + 2);
    for id in &ids {
        let f = find_field(id).expect("id column resolved above");
        output_fields.push((*f).clone());
    }
    output_fields.push(StructField::not_null(
        variable_column_name.clone(),
        DataType::String,
    ));
    output_fields.push(StructField::new(
        value_column_name.clone(),
        widened_type,
        widened_nullable,
    ));
    let output_schema = StructType::new(output_fields);

    Ok(TypedAst {
        op: TypedOp::Unpivot {
            input: Box::new(typed_input),
            ids,
            values: materialised_values,
            variable_column_name,
            value_column_name,
        },
        resolved_schema: output_schema,
    })
}

// ── Describe / Summary analysis (Pass 80) ───────────────────────────────────

/// Build the shared output schema for `describe` / `summary`: a `summary`
/// STRING NOT NULL column followed by one STRING NULLABLE column per
/// materialised input col. Per-col stats can produce NULL (`TRY_CAST` on a
/// non-numeric col returns NULL) so every stat column is nullable.
fn build_stats_output_schema(cols: &[String]) -> StructType {
    let mut fields: Vec<StructField> = Vec::with_capacity(cols.len() + 1);
    // Spark stamps `summary` as nullable=true even though every value is a
    // string literal — see `Dataset.summary()` output schema. Spark parity
    // (ADR-015: schema oracle wins) requires we match, not the STRING NOT
    // NULL that the emission's `'count'` literal would justify.
    fields.push(StructField::nullable("summary", DataType::String));
    for c in cols {
        fields.push(StructField::nullable(c.clone(), DataType::String));
    }
    StructType::new(fields)
}

/// Materialise a caller-supplied `cols` list against `input_schema`:
///   - empty ⇒ all input columns in schema order (Spark default);
///   - non-empty ⇒ each name resolves case-insensitively or
///     [`AnalyzerError::UnknownColumn`] is returned. The output preserves the
///     caller's casing (Spark parity).
fn materialise_stats_cols(
    cols: Vec<String>,
    input_schema: &StructType,
) -> Result<Vec<String>, AnalyzerError> {
    if cols.is_empty() {
        Ok(input_schema.fields.iter().map(|f| f.name.clone()).collect())
    } else {
        let lowercase_names: HashSet<String> = input_schema
            .fields
            .iter()
            .map(|f| f.name.to_ascii_lowercase())
            .collect();
        for c in &cols {
            if !lowercase_names.contains(&c.to_ascii_lowercase()) {
                return Err(AnalyzerError::UnknownColumn {
                    name: c.clone(),
                    qualifier: None,
                });
            }
        }
        Ok(cols)
    }
}

/// Analyze `CommonOp::Describe`: resolve the input, materialise `cols`
/// (empty ⇒ all input columns in schema order), stamp the shared stats
/// output schema.
fn analyze_describe(
    input: CommonAst,
    cols: Vec<String>,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types)?;
    let materialised = materialise_stats_cols(cols, &typed_input.resolved_schema)?;
    let output_schema = build_stats_output_schema(&materialised);
    Ok(TypedAst {
        op: TypedOp::Describe {
            input: Box::new(typed_input),
            cols: materialised,
        },
        resolved_schema: output_schema,
    })
}

/// Analyze `CommonOp::Summary`: resolve the input, materialise the full
/// column list from the input schema (proto `StatSummary` has no `cols`
/// field), and materialise the statistics list (empty ⇒
/// [`DEFAULT_SUMMARY_STATS`]).
fn analyze_summary(
    input: CommonAst,
    statistics: Vec<String>,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types)?;
    let materialised_cols: Vec<String> = typed_input
        .resolved_schema
        .fields
        .iter()
        .map(|f| f.name.clone())
        .collect();
    let materialised_stats: Vec<String> = if statistics.is_empty() {
        DEFAULT_SUMMARY_STATS
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    } else {
        statistics
    };
    let output_schema = build_stats_output_schema(&materialised_cols);
    Ok(TypedAst {
        op: TypedOp::Summary {
            input: Box::new(typed_input),
            cols: materialised_cols,
            statistics: materialised_stats,
        },
        resolved_schema: output_schema,
    })
}

/// Analyze `CommonOp::FreqItems`: resolve the input, materialise `cols`
/// (case-insensitive; unresolved names raise `AnalyzerError::UnknownColumn`),
/// and stamp the output schema as one `ARRAY<T>` NON-NULLABLE column per
/// input col — where `T` is the source column's declared [`DataType`].
/// Spark parity per ADR-015: the element type of each `ARRAY<T>` matches the
/// source column's declared `DataType` (never a hardcoded `Array<String>`).
///
/// **Spark parity — outer nullability.** Spark's `StatFunctions.freqItems`
/// stamps every output column non-nullable: the aggregate always returns a
/// value (empty array when no rows meet the support threshold), never NULL.
///
/// **Spark parity — element `contains_null`.** Element type mirrors the source
/// col type; `contains_null=true` per Spark's `ArrayType(t)` default. Spark
/// builds each output column as `ArrayType(v._2)` and the single-arg
/// `ArrayType` apply defaults `containsNull=true` regardless of source
/// nullability — τ matches Spark's schema oracle here (not DuckDB's runtime
/// materialisation).
///
/// Column names use `{col}_freqItems` (preserving the caller's casing).
fn analyze_freq_items(
    input: CommonAst,
    cols: Vec<String>,
    support: f64,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types)?;
    let materialised = materialise_stats_cols(cols, &typed_input.resolved_schema)?;
    let output_fields: Vec<StructField> = materialised
        .iter()
        .map(|c| {
            // `materialise_stats_cols` already validated `c` case-insensitively;
            // `field_by_name` uses the same case-insensitive lookup.
            let src = typed_input
                .resolved_schema
                .field_by_name(c)
                .expect("materialise_stats_cols already validated");
            // Element type mirrors source col type; contains_null=true per
            // Spark's ArrayType(t) default. Spark's `StatFunctions.freqItems`
            // builds each output column as `ArrayType(v._2)` — the single-arg
            // `ArrayType` apply defaults `containsNull=true` regardless of
            // source nullability, so τ matches Spark's schema oracle here
            // (not DuckDB's runtime materialisation). Outer column stays
            // non-nullable: the aggregate always returns a value (empty array
            // when no rows meet the support threshold), never NULL.
            StructField::not_null(
                format!("{c}_freqItems"),
                DataType::Array(Box::new(src.data_type.clone()), true),
            )
        })
        .collect();
    Ok(TypedAst {
        op: TypedOp::FreqItems {
            input: Box::new(typed_input),
            cols: materialised,
            support,
        },
        resolved_schema: StructType::new(output_fields),
    })
}

// ── Pivot analysis (Pass 60) ────────────────────────────────────────────────

/// Analyze `CommonOp::Pivot`: resolve the input, resolve grouping / pivot /
/// aggregate expressions against the input schema, and stamp the output
/// schema.
///
/// **Schema stamping.** When `pivot_values` is non-empty, the output schema
/// is `<grouping> + <pivot_value_i × aggregate_j>`. Column names follow
/// Spark:
///
/// - Single aggregate ⇒ `pivot_value.to_string()` (Spark's "toString" of
///   the literal — Boolean `true` → `"true"`, integers → decimal repr,
///   strings verbatim).
/// - Multi aggregate ⇒ `"{pivot_value}_{agg_alias}"` per Spark.
///
/// Column types follow the aggregate's return type; nullability follows
/// Spark aggregate nullability (COUNT is non-nullable; SUM/AVG/etc. tolerate
/// NULLs in the pivot bucket ⇒ nullable).
///
/// **Empty `pivot_values`.** τ rejects loudly with a Thunderduck-boundary
/// `PuntedOperator("Pivot[implicit-values]")` per ADR-022. Spark's Analyzer
/// resolves the value list via an eager `SELECT DISTINCT pivot_col FROM
/// input`; τ has no session-injected DISTINCT-query hook, so
/// stamping a partial schema here would mismatch DuckDB's runtime output
/// and confuse PySpark's `df.schema` / `df.collect()` contract. Explicit-
/// values pivot is fully supported.
fn analyze_pivot(
    input: CommonAst,
    grouping: PivotGrouping,
    pivot_column: Expression,
    pivot_values: Vec<Expression>,
    aggregates: Vec<Expression>,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    // Thunderduck-boundary (ADR-022): implicit pivot values require an
    // eager DISTINCT query against DuckDB (Spark's Analyzer does this
    // eagerly). τ's analyzer has no session hook — implementing
    // it needs the base_types overlay extended with a value-query closure.
    // Reject loudly with a Thunderduck-boundary error rather than stamping
    // an incorrect schema. See Pass 60 notes for the follow-up work.
    if pivot_values.is_empty() {
        return Err(AnalyzerError::PuntedOperator {
            op: "Pivot[implicit-values]".to_owned(),
            reason:
                "pivot without explicit values requires eager DISTINCT query; τ needs a session-injected value-discovery hook"
                    .to_owned(),
        });
    }
    let typed_input = analyze_node(input, base_types)?;
    let input_schema = &typed_input.resolved_schema;
    // Resolve the pivot column and aggregates first: the implicit-grouping
    // derivation needs to know which columns the aggregates reference.
    let pivot_column = resolve_and_stamp(pivot_column, input_schema, base_types)?;
    let aggregates = resolve_expr_list(aggregates, input_schema, base_types)?;
    // The DataFrame path supplies grouping explicitly (from `groupBy`); SQL
    // `PIVOT` supplies none, so the analyzer derives it as
    // `input schema − pivot column − aggregate-referenced columns`, in input
    // order, per Spark parity (ADR-015).
    let grouping = match grouping {
        PivotGrouping::Explicit(g) => resolve_expr_list(g, input_schema, base_types)?,
        PivotGrouping::Implicit => {
            derive_implicit_grouping(input_schema, &pivot_column, &aggregates)
        }
    };
    // Pivot values are literals; they only need type resolution against the
    // pivot column (Spark coerces them into that type at read). We defer
    // typing to the emission stage — literals carry their own type already.
    let pivot_values = resolve_expr_list(pivot_values, input_schema, base_types)?;

    // Spark-emulated (Pass 60 H2): Catalyst rejects NULL pivot values with
    // "Literal expressions required for pivot values, found 'null'". Mirror
    // that behavior so callers cannot smuggle a NULL bucket in.
    for pv in &pivot_values {
        if let Expression::Literal(lit) = pv {
            if matches!(lit.value, super::expression::LiteralValue::Null) {
                return Err(AnalyzerError::Other {
                    reason: "literal expressions required for pivot values, found 'null'"
                        .to_owned(),
                });
            }
        }
    }

    // Build the output schema. Grouping columns come first, verbatim.
    let mut output_fields: Vec<StructField> = Vec::new();
    for g in &grouping {
        let name = expression_output_name(g);
        let dt = g.data_type(input_schema);
        let nullable = g.nullable(input_schema);
        output_fields.push(StructField::new(name, dt, nullable));
    }

    // When pivot values are explicit, stamp one output column per
    // (pivot_value, aggregate) pair per Spark. Otherwise leave the pivot
    // outputs off the schema (DuckDB will materialise them at execute time).
    // **Nullability:** pivot output columns are always nullable per Spark —
    // a given pivot bucket may be empty for a particular group, in which
    // case the aggregate cell materialises as NULL (verified by the
    // grp-004 differential test). Ignore the aggregate's intrinsic
    // nullability here.
    if !pivot_values.is_empty() {
        let single_agg = aggregates.len() == 1;
        for pv in &pivot_values {
            let pv_name = literal_to_pivot_column_name(pv);
            for a in &aggregates {
                let col_name = if single_agg {
                    pv_name.clone()
                } else {
                    let agg_name = expression_output_name(a);
                    format!("{pv_name}_{agg_name}")
                };
                let dt = a.data_type(input_schema);
                output_fields.push(StructField::nullable(col_name, dt));
            }
        }
    }
    let output_schema = StructType::new(output_fields);

    Ok(TypedAst {
        op: TypedOp::Pivot {
            input: Box::new(typed_input),
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
        },
        resolved_schema: output_schema,
    })
}

/// Spark's implicit PIVOT grouping: the input columns minus the pivot column
/// minus every column referenced by the aggregate argument(s), preserved in
/// input-schema order (Spark parity per ADR-015). Used for SQL `PIVOT`, which
/// supplies no grouping list. `count(*)` references no column (its `Star`
/// argument contributes nothing), so every non-pivot column remains grouped.
fn derive_implicit_grouping(
    input_schema: &StructType,
    pivot_column: &Expression,
    aggregates: &[Expression],
) -> Vec<Expression> {
    let mut excluded: HashSet<String> = HashSet::new();
    // Exclude the columns the pivot expression REFERENCES (not its output
    // name). For a simple `FOR dept_id` this is just `dept_id`; for an
    // expression pivot like `FOR extract(year FROM d)` it is the underlying
    // `d` (not the literal name "extract"); an aliased pivot column strips its
    // alias via the helper's `Alias` arm. Uniform across simple-column,
    // expression, and alias cases via the exhaustive helper below.
    collect_referenced_columns(pivot_column, &mut excluded);
    // Exclude every column referenced by the aggregate argument(s).
    for agg in aggregates {
        collect_referenced_columns(agg, &mut excluded);
    }
    input_schema
        .fields
        .iter()
        .filter(|f| !excluded.contains(&f.name.to_ascii_lowercase()))
        .map(|f| {
            Expression::ColumnReference(ColumnReference {
                name: f.name.clone(),
                qualifier: None,
                data_type: Some(f.data_type.clone()),
                nullable: Some(f.nullable),
            })
        })
        .collect()
}

/// Recursively collect the (lowercased) names of every column referenced by an
/// expression tree into `acc`. A bare `Star` contributes nothing (so
/// `count(*)` references no column). Used by [`derive_implicit_grouping`].
///
/// MAINTENANCE CONTRACT: the match below is exhaustive over `Expression` (a new
/// *variant* is a compile error). It does NOT catch a new sub-expression *field*
/// on an existing variant — if you add one, recurse into it here (and in the
/// sibling walker `base_types::collect_scan_tables_in_expr`).
fn collect_referenced_columns(expr: &Expression, acc: &mut HashSet<String>) {
    match expr {
        Expression::ColumnReference(c) => {
            acc.insert(c.name.to_ascii_lowercase());
        }
        Expression::UnresolvedColumn(u) => {
            acc.insert(u.name.to_ascii_lowercase());
        }
        Expression::Star(_) => {}
        Expression::Alias(a) => collect_referenced_columns(&a.expr, acc),
        Expression::Binary(b) => {
            collect_referenced_columns(&b.left, acc);
            collect_referenced_columns(&b.right, acc);
        }
        Expression::Unary(u) => collect_referenced_columns(&u.operand, acc),
        Expression::Cast(c) => collect_referenced_columns(&c.expr, acc),
        Expression::FunctionCall(f) => {
            for arg in &f.args {
                collect_referenced_columns(arg, acc);
            }
        }
        Expression::CaseWhen(cw) => {
            for (w, t) in &cw.branches {
                collect_referenced_columns(w, acc);
                collect_referenced_columns(t, acc);
            }
            if let Some(e) = &cw.else_expr {
                collect_referenced_columns(e, acc);
            }
        }
        Expression::Between(b) => {
            collect_referenced_columns(&b.expr, acc);
            collect_referenced_columns(&b.low, acc);
            collect_referenced_columns(&b.high, acc);
        }
        Expression::InList(i) => {
            collect_referenced_columns(&i.expr, acc);
            for item in &i.list {
                collect_referenced_columns(item, acc);
            }
        }
        Expression::Like(l) => {
            collect_referenced_columns(&l.value, acc);
            collect_referenced_columns(&l.pattern, acc);
        }
        Expression::IsDistinctFrom(d) => {
            collect_referenced_columns(&d.left, acc);
            collect_referenced_columns(&d.right, acc);
        }
        Expression::Window(w) => {
            collect_referenced_columns(&w.func, acc);
            for p in &w.partition_by {
                collect_referenced_columns(p, acc);
            }
            for o in &w.order_by {
                collect_referenced_columns(&o.expr, acc);
            }
        }
        Expression::ArrayLiteral(a) => {
            for e in &a.elements {
                collect_referenced_columns(e, acc);
            }
        }
        Expression::MapLiteral(m) => {
            for (k, v) in &m.entries {
                collect_referenced_columns(k, acc);
                collect_referenced_columns(v, acc);
            }
        }
        Expression::StructLiteral(s) => {
            for (_name, e) in &s.fields {
                collect_referenced_columns(e, acc);
            }
        }
        Expression::RowConstructor(r) => {
            for e in &r.elements {
                collect_referenced_columns(e, acc);
            }
        }
        Expression::ExtractValue(x) => {
            collect_referenced_columns(&x.child, acc);
            collect_referenced_columns(&x.extraction, acc);
        }
        Expression::UpdateFields(u) => {
            collect_referenced_columns(&u.struct_expr, acc);
            for (_name, update) in &u.updates {
                if let Some(e) = update {
                    collect_referenced_columns(e, acc);
                }
            }
        }
        // `x IN (subquery)`: the outer `expr` is a genuine outer-scope column
        // reference (e.g. `dept_id IN (…)` references `dept_id`), so recurse
        // into it. The subquery's inner plan is a SEPARATE scope — τ does not
        // support correlated pivot aggregates, so we do not recurse into it
        // (a correlated outer ref inside the inner plan contributes nothing).
        Expression::InSubquery(s) => collect_referenced_columns(&s.expr, acc),
        // `EXISTS (subquery)` / `(scalar subquery)`: the only sub-expressions
        // live inside the subquery's inner plan, which is a separate scope
        // (see `InSubquery` above). τ does not support correlated pivot
        // aggregates, so treat these as referencing nothing from the outer.
        Expression::ExistsSubquery(_) | Expression::ScalarSubquery(_) => {}
        // A lambda body can reference an outer column (e.g.
        // `transform(arr, x -> x + outer_col)`). Recurse into it: the body's
        // `LambdaVariable` refs are lambda-local (handled below) and add
        // nothing, so only real outer column refs are collected.
        Expression::Lambda(l) => collect_referenced_columns(&l.body, acc),
        // Lambda-local variable — not a schema column, contributes nothing.
        Expression::LambdaVariable(_) => {}
        // Opaque raw SQL: τ cannot introspect column refs out of an unparsed
        // SQL string, so it contributes nothing to the exclusion set. (This is
        // a `spark.expr(...)` passthrough; not reachable in the pivot cases τ
        // supports.)
        Expression::RawSql(_) => {}
        // Pattern-driven column expander: the analyzer's Project pre-pass
        // expands it into concrete `UnresolvedColumn`s before inference, so it
        // does not survive into a resolved aggregate. No enumerable name here.
        Expression::UnresolvedRegex(_) => {}
        // Leaves — no sub-expression can carry a column reference. (`Star` is
        // handled above so `count(*)` references no column.)
        Expression::Literal(_) | Expression::Interval(_) => {}
    }
}

/// Spark's rendering of a pivot value literal to a column name. Boolean
/// `true`/`false` render as `"true"`/`"false"`; integers as their decimal
/// repr; strings verbatim. Non-literal expressions (should not happen —
/// PySpark only sends literals) fall back to [`expression_output_name`].
fn literal_to_pivot_column_name(expr: &Expression) -> String {
    use super::expression::LiteralValue;
    if let Expression::Literal(lit) = expr {
        return match &lit.value {
            // Pass 60 H2: analyze_pivot rejects NULL pivot values before we
            // ever reach this arm, so the case is unreachable in practice.
            LiteralValue::Null => {
                unreachable!("analyzer rejects Null pivot values (Pass 60 H2)")
            }
            LiteralValue::Boolean(b) => b.to_string(),
            LiteralValue::Byte(v) => v.to_string(),
            LiteralValue::Short(v) => v.to_string(),
            LiteralValue::Int(v) => v.to_string(),
            LiteralValue::Long(v) => v.to_string(),
            // Pass 60 H1: Spark's Catalyst `Literal.sql` renders integral
            // floats/doubles with a `.0` suffix ("1.0", not "1"). Match it
            // so pivot output column names match Spark exactly.
            LiteralValue::Float(v) => format_float_pivot_name(f64::from(*v)),
            LiteralValue::Double(v) => format_float_pivot_name(*v),
            LiteralValue::Decimal { value, .. } => value.clone(),
            LiteralValue::String(s) => s.clone(),
            LiteralValue::Date(d) => d.to_string(),
            LiteralValue::Timestamp(t) => t.to_string(),
            LiteralValue::TimestampNtz(t) => t.to_string(),
            LiteralValue::Binary(_) => "binary".to_owned(),
        };
    }
    expression_output_name(expr)
}

/// Spark-parity formatter for float/double pivot column names.
///
/// Catalyst's `Literal.sql` for a `DoubleType(1.0)` yields the string `"1.0"`
/// (integral doubles get a `.0` suffix; non-integral doubles use their
/// natural decimal repr). NaN/infinity fall through to Rust's default
/// `Display`, which emits `"NaN"` / `"inf"` / `"-inf"` — a lossless-but-not
/// necessarily Spark-precise stringification. This is acceptable for pivot
/// column names (Pass 60 finding M2 was dropped as info-only).
fn format_float_pivot_name(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        v.to_string()
    }
}

fn expression_output_name(expr: &Expression) -> String {
    match expr {
        Expression::Alias(a) => a.alias.clone(),
        Expression::ColumnReference(c) => c.name.clone(),
        Expression::UnresolvedColumn(u) => u.name.clone(),
        Expression::FunctionCall(f) => f.name.clone(),
        Expression::Literal(_) => "col".to_owned(),
        _ => "expr".to_owned(),
    }
}

// ── Set-op widening (§5) ─────────────────────────────────────────────────────

fn push_setop_casts(ast: &mut TypedAst, widened_schema: &StructType) {
    // Only push CASTs into direct `Project` children whose projection list
    // matches the widened schema column-by-column. Non-Project inputs
    // (TableScan, Values, ...) receive their CAST at emission time.
    if let TypedOp::Project { projections, .. } = &mut ast.op {
        if projections.len() != widened_schema.fields.len() {
            return;
        }
        let input_schema = match &ast.op {
            TypedOp::Project { input, .. } => input.resolved_schema.clone(),
            _ => return,
        };
        // Re-borrow to mutate.
        if let TypedOp::Project { projections, .. } = &mut ast.op {
            for (idx, proj) in projections.iter_mut().enumerate() {
                let target = &widened_schema.fields[idx];
                let current_type = proj.data_type(&input_schema);
                if current_type != target.data_type && !matches!(current_type, DataType::Unresolved)
                {
                    // Wrap in CAST; preserve alias if present at the top.
                    wrap_projection_with_cast(proj, target.data_type.clone());
                }
            }
        }
        ast.resolved_schema = widened_schema.clone();
    }
}

fn wrap_projection_with_cast(expr: &mut Expression, to_type: DataType) {
    // If the projection is `Alias(inner)`, wrap inner and reattach alias.
    match expr {
        Expression::Alias(alias) => {
            let inner = std::mem::replace(
                &mut *alias.expr,
                Expression::Literal(super::expression::Literal {
                    value: super::expression::LiteralValue::Null,
                    data_type: DataType::Null,
                }),
            );
            let alias_name = alias.alias.clone();
            let casted = Expression::Cast(CastExpression {
                expr: Box::new(inner),
                to_type,
                try_cast: false,
            });
            *expr = Expression::Alias(AliasExpression {
                expr: Box::new(casted),
                alias: alias_name,
            });
        }
        other => {
            let owned = std::mem::replace(
                other,
                Expression::Literal(super::expression::Literal {
                    value: super::expression::LiteralValue::Null,
                    data_type: DataType::Null,
                }),
            );
            *other = Expression::Cast(CastExpression {
                expr: Box::new(owned),
                to_type,
                try_cast: false,
            });
        }
    }
}

// ── Join helpers (§6) ────────────────────────────────────────────────────────

fn apply_join_nullability(
    left: &StructType,
    right: &StructType,
    join_type: JoinType,
) -> (StructType, StructType) {
    match join_type {
        JoinType::Inner | JoinType::Cross => (left.clone(), right.clone()),
        JoinType::Left => (left.clone(), flip_all_nullable(right)),
        JoinType::Right => (flip_all_nullable(left), right.clone()),
        JoinType::Full => (flip_all_nullable(left), flip_all_nullable(right)),
        JoinType::LeftSemi | JoinType::LeftAnti => (left.clone(), StructType::empty()),
    }
}

fn flip_all_nullable(schema: &StructType) -> StructType {
    let fields = schema
        .fields
        .iter()
        .map(|f| StructField::new(f.name.clone(), f.data_type.clone(), true))
        .collect();
    StructType::new(fields)
}

// ── Values schema inference ─────────────────────────────────────────────────

fn infer_values_schema(
    rows: &[Vec<Expression>],
    column_names: &[String],
) -> Result<StructType, AnalyzerError> {
    if rows.is_empty() {
        return Err(AnalyzerError::Other {
            reason: "VALUES relation must have at least one row".to_owned(),
        });
    }
    let ncols = rows[0].len();
    if ncols != column_names.len() {
        // Arity mismatch, not a per-column type mismatch — see the set-op
        // path for the equivalent decision.
        return Err(AnalyzerError::Other {
            reason: format!(
                "VALUES column count mismatch: {} names vs {} row columns",
                column_names.len(),
                ncols
            ),
        });
    }
    let empty = StructType::empty();
    let mut fields: Vec<StructField> = Vec::with_capacity(ncols);
    for col_idx in 0..ncols {
        let mut widened = rows[0][col_idx].data_type(&empty);
        let mut nullable = rows[0][col_idx].nullable(&empty);
        for row in &rows[1..] {
            widened = TypeInferenceEngine::unify_types(&widened, &row[col_idx].data_type(&empty));
            nullable = nullable || row[col_idx].nullable(&empty);
        }
        fields.push(StructField::new(
            column_names[col_idx].clone(),
            widened,
            nullable,
        ));
    }
    Ok(StructType::new(fields))
}

// Manually mark `StarExpression` as used-by-name for the module doc anchor.
#[allow(dead_code)]
const _STAR: fn(StarExpression) = |_| {};

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::analyzer_fixtures;
    use super::super::ast::CommonAst;
    use super::super::expression::{
        BetweenExpression, BinaryExpression, BinaryOp, ExistsSubquery, FunctionCall, InSubquery,
        Literal, LiteralValue, ScalarSubquery, UnresolvedRegexExpression,
    };
    use super::*;

    fn emp_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ])
    }

    fn dept_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("dept_id", DataType::Integer),
            StructField::nullable("dept_name", DataType::String),
        ])
    }

    fn base_types_with_emp_dept() -> BaseTypes {
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            right: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema()),
            "dept" => Some(dept_schema()),
            _ => None,
        })
    }

    // ── resolve pass ──────────────────────────────────────────────────────

    #[test]
    fn resolve_table_scan_seeds_schema_from_base_types() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn resolve_unknown_table_surfaces_spark_emulated_error() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::TableScan {
            table: "missing".to_owned(),
            alias: None,
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownTable { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn resolve_unknown_column_surfaces_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "not_a_column".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    // ── Pass 106 — uncorrelated subquery analysis ────────────────────────

    fn emp_scan() -> CommonAst {
        CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        })
    }

    /// Inner plan `SELECT <col> FROM emp` — a single-column subquery body.
    fn inner_select_col(col: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: col.to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        })
    }

    #[test]
    fn scalar_subquery_types_to_inner_single_column_and_becomes_analyzed() {
        let bt = base_types_with_emp_dept();
        // SELECT (SELECT id FROM emp) AS s FROM emp
        let scalar = Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(inner_select_col("id"))),
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![scalar],
        });
        let typed = analyze(ast, &bt).unwrap();
        // `id` is Long; a scalar subquery is always nullable (no-row ⇒ NULL).
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
        assert!(typed.resolved_schema.fields[0].nullable);
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ScalarSubquery(s) => {
                    assert!(
                        matches!(s.subquery, SubqueryPlan::Analyzed(_)),
                        "analyzer must rewrite Unanalyzed → Analyzed"
                    );
                }
                other => panic!("expected ScalarSubquery, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn scalar_subquery_two_columns_is_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "salary".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
            ],
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::Other { .. }));
    }

    #[test]
    fn exists_subquery_over_dept_analyzes_and_stays_boolean() {
        let bt = base_types_with_emp_dept();
        // SELECT * FROM emp WHERE EXISTS (SELECT dept_id FROM dept)
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(emp_scan()),
            condition: Expression::ExistsSubquery(ExistsSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Filter { condition, .. } => match condition {
                Expression::ExistsSubquery(e) => {
                    assert!(matches!(e.subquery, SubqueryPlan::Analyzed(_)));
                }
                other => panic!("expected ExistsSubquery, got {other:?}"),
            },
            other => panic!("expected Filter, got {other:?}"),
        }
    }

    #[test]
    fn in_subquery_correlated_outer_ref_is_boundary_error() {
        let bt = base_types_with_emp_dept();
        // Inner references a column absent from `dept` — analyzed in isolation
        // this fails resolution (the correlated boundary, ADR-022).
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "not_in_dept".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(emp_scan()),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                })),
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    // ── assign_types pass ────────────────────────────────────────────────

    #[test]
    fn assign_types_stamps_column_reference_type_and_nullability() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "id".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.data_type.as_ref(), Some(&DataType::Long));
                    assert_eq!(c.nullable, Some(false));
                }
                _ => panic!("expected ColumnReference"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn filter_condition_must_be_boolean() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            condition: Expression::Literal(Literal {
                value: LiteralValue::Int(42),
                data_type: DataType::Integer,
            }),
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::TypeMismatch { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    // ── derive_nullability pass — outer join flipping ────────────────────

    #[test]
    fn left_outer_join_flips_right_side_nullability() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: Some("emp".to_owned()),
                plan_id: None,
            })),
            right: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: Some("dept".to_owned()),
                plan_id: None,
            })),
        });
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            right: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            join_type: JoinType::Left,
            condition: Some(cond),
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        match typed.op {
            TypedOp::Join {
                derived_left_schema,
                derived_right_schema,
                ..
            } => {
                // Left preserved: `id` stays not-null.
                assert!(!derived_left_schema.field_by_name("id").unwrap().nullable);
                // Right flipped: `dept_id` becomes nullable.
                assert!(
                    derived_right_schema
                        .field_by_name("dept_id")
                        .unwrap()
                        .nullable
                );
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn right_outer_join_flips_left_side_nullability() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            right: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            join_type: JoinType::Right,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        match typed.op {
            TypedOp::Join {
                derived_left_schema,
                derived_right_schema,
                ..
            } => {
                assert!(derived_left_schema.field_by_name("id").unwrap().nullable);
                assert!(
                    !derived_right_schema
                        .field_by_name("dept_id")
                        .unwrap()
                        .nullable
                );
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn full_outer_join_flips_both_sides() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            right: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            join_type: JoinType::Full,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        match typed.op {
            TypedOp::Join {
                derived_left_schema,
                derived_right_schema,
                ..
            } => {
                assert!(derived_left_schema.field_by_name("id").unwrap().nullable);
                assert!(
                    derived_right_schema
                        .field_by_name("dept_id")
                        .unwrap()
                        .nullable
                );
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn inner_join_preserves_both_sides_nullability() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            right: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        match typed.op {
            TypedOp::Join {
                derived_left_schema,
                derived_right_schema,
                ..
            } => {
                assert!(!derived_left_schema.field_by_name("id").unwrap().nullable);
                assert!(
                    !derived_right_schema
                        .field_by_name("dept_id")
                        .unwrap()
                        .nullable
                );
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn left_semi_join_drops_right_side() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            right: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            join_type: JoinType::LeftSemi,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        match typed.op {
            TypedOp::Join {
                derived_left_schema,
                derived_right_schema,
                ..
            } => {
                assert_eq!(derived_left_schema, emp_schema());
                assert!(derived_right_schema.is_empty());
            }
            _ => panic!("expected Join"),
        }
        // Output schema is left-only.
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn ambiguous_column_across_joins_surfaces_error() {
        let bt = base_types_with_emp_dept();
        // Both `emp` and `dept` have `dept_id`; unqualified reference is
        // ambiguous.
        let cond = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "dept_id".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            right: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "dept".to_owned(),
                alias: None,
            })),
            join_type: JoinType::Inner,
            condition: Some(cond),
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::AmbiguousColumn { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn resolve_column_projection_ambiguous_across_join_errors() {
        // The central `resolve_column` ambiguity check catches ambiguous
        // references anywhere — including projections above a join — not
        // only inside the join condition itself.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Join {
                left: Box::new(CommonAst::new(CommonOp::TableScan {
                    table: "emp".to_owned(),
                    alias: None,
                })),
                right: Box::new(CommonAst::new(CommonOp::TableScan {
                    table: "dept".to_owned(),
                    alias: None,
                })),
                join_type: JoinType::Inner,
                condition: None,
                using_columns: vec![],
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            })),
            // `dept_id` is present on both sides of the join — unqualified
            // reference must fail.
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::AmbiguousColumn {
                ref name,
                ref candidates,
            } => {
                assert_eq!(name, "dept_id");
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousColumn, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn resolve_column_projection_unambiguous_still_resolves() {
        // Sanity anchor: an unqualified column that resolves uniquely across
        // the joined schema must still resolve cleanly.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Join {
                left: Box::new(CommonAst::new(CommonOp::TableScan {
                    table: "emp".to_owned(),
                    alias: None,
                })),
                right: Box::new(CommonAst::new(CommonOp::TableScan {
                    table: "dept".to_owned(),
                    alias: None,
                })),
                join_type: JoinType::Inner,
                condition: None,
                using_columns: vec![],
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            })),
            // `salary` only exists on `emp`.
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "salary".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.name, "salary");
                    assert_eq!(c.data_type.as_ref(), Some(&DataType::Double));
                }
                _ => panic!("expected resolved ColumnReference"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn qualified_star_with_unknown_qualifier_errors() {
        // `SELECT nonexistent.*` must not silently expand to `*`; it must
        // surface `UnknownColumn` (formatted as `nonexistent.*`).
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some("nonexistent".to_owned()),
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn {
                ref name,
                ref qualifier,
            } => {
                assert_eq!(name, "nonexistent.*");
                assert_eq!(qualifier.as_deref(), Some("nonexistent"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    // ── set-op widening (§5) ────────────────────────────────────────────

    fn tiny_int_plan() -> CommonAst {
        CommonAst::new(CommonOp::Values {
            rows: vec![vec![Expression::Literal(Literal {
                value: LiteralValue::Int(1),
                data_type: DataType::Integer,
            })]],
            column_names: vec!["x".to_owned()],
        })
    }

    fn tiny_double_plan() -> CommonAst {
        CommonAst::new(CommonOp::Values {
            rows: vec![vec![Expression::Literal(Literal {
                value: LiteralValue::Double(1.5),
                data_type: DataType::Double,
            })]],
            column_names: vec!["x".to_owned()],
        })
    }

    fn tiny_long_plan() -> CommonAst {
        CommonAst::new(CommonOp::Values {
            rows: vec![vec![Expression::Literal(Literal {
                value: LiteralValue::Long(1),
                data_type: DataType::Long,
            })]],
            column_names: vec!["x".to_owned()],
        })
    }

    #[test]
    fn setop_union_widens_int_and_double_to_double() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: false,
            allow_missing_columns: false,
            children: vec![tiny_int_plan(), tiny_double_plan()],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::SetOp { widened_schema, .. } => {
                assert_eq!(widened_schema.fields[0].data_type, DataType::Double);
            }
            _ => panic!("expected SetOp"),
        }
    }

    #[test]
    fn setop_intersect_widens_int_and_long_to_long() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Intersect,
            all: false,
            by_name: false,
            allow_missing_columns: false,
            children: vec![tiny_int_plan(), tiny_long_plan()],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::SetOp {
                widened_schema,
                kind,
                ..
            } => {
                assert_eq!(*kind, SetOpKind::Intersect);
                assert_eq!(widened_schema.fields[0].data_type, DataType::Long);
            }
            _ => panic!("expected SetOp"),
        }
    }

    #[test]
    fn setop_except_widens_short_and_long_to_long() {
        let bt = BaseTypes::empty();
        let short_plan = CommonAst::new(CommonOp::Values {
            rows: vec![vec![Expression::Literal(Literal {
                value: LiteralValue::Short(1),
                data_type: DataType::Short,
            })]],
            column_names: vec!["x".to_owned()],
        });
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Except,
            all: false,
            by_name: false,
            allow_missing_columns: false,
            children: vec![short_plan, tiny_long_plan()],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::SetOp {
                widened_schema,
                kind,
                ..
            } => {
                assert_eq!(*kind, SetOpKind::Except);
                assert_eq!(widened_schema.fields[0].data_type, DataType::Long);
            }
            _ => panic!("expected SetOp"),
        }
    }

    /// Project the `dept_id` column (present on both `emp` and `dept`, but
    /// with opposite nullability — `emp.dept_id` nullable, `dept.dept_id`
    /// not-null) from the named table so set-op children carry a single
    /// column of a known nullability.
    fn dept_id_from(table: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: table.to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        })
    }

    /// INTERSECT nullability is an AND-fold (Spark `Intersect.computeOutput`):
    /// nullable(emp.dept_id)=true ∧ non-nullable(dept.dept_id)=false ⇒ the
    /// intersection column is **non-nullable**.
    #[test]
    fn setop_intersect_nullability_is_and_across_children() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Intersect,
            all: false,
            by_name: false,
            allow_missing_columns: false,
            children: vec![dept_id_from("emp"), dept_id_from("dept")],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert!(
            !typed.resolved_schema.fields[0].nullable,
            "INTERSECT of nullable ∩ non-nullable must be non-nullable (AND)"
        );
    }

    /// EXCEPT nullability is the LEFT child's only (Spark `Except.output =
    /// left.output`). Left non-nullable, right nullable ⇒ output non-nullable.
    #[test]
    fn setop_except_nullability_is_left_child_only_nonnull_left() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Except,
            all: false,
            by_name: false,
            allow_missing_columns: false,
            // Left = dept (non-nullable dept_id), Right = emp (nullable).
            children: vec![dept_id_from("dept"), dept_id_from("emp")],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert!(
            !typed.resolved_schema.fields[0].nullable,
            "EXCEPT must take the non-nullable LEFT child's nullability, ignoring the nullable right"
        );
    }

    /// EXCEPT with a nullable LEFT and non-nullable right ⇒ output nullable
    /// (left-only rule — the right child's non-nullability is irrelevant).
    #[test]
    fn setop_except_nullability_is_left_child_only_nullable_left() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Except,
            all: false,
            by_name: false,
            allow_missing_columns: false,
            // Left = emp (nullable dept_id), Right = dept (non-nullable).
            children: vec![dept_id_from("emp"), dept_id_from("dept")],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert!(
            typed.resolved_schema.fields[0].nullable,
            "EXCEPT must take the nullable LEFT child's nullability, ignoring the non-nullable right"
        );
    }

    /// Regression guard for the unchanged Union OR-fold: nullable ∪
    /// non-nullable ⇒ nullable.
    #[test]
    fn setop_union_nullability_is_or_across_children() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: false,
            allow_missing_columns: false,
            children: vec![dept_id_from("emp"), dept_id_from("dept")],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert!(
            typed.resolved_schema.fields[0].nullable,
            "UNION of nullable ∪ non-nullable must remain nullable (OR)"
        );
    }

    #[test]
    fn setop_arity_mismatch_uses_other_variant() {
        let bt = BaseTypes::empty();
        let two_col = CommonAst::new(CommonOp::Values {
            rows: vec![vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Int(2),
                    data_type: DataType::Integer,
                }),
            ]],
            column_names: vec!["x".to_owned(), "y".to_owned()],
        });
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: false,
            allow_missing_columns: false,
            children: vec![tiny_int_plan(), two_col],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::Other { ref reason } => {
                assert!(
                    reason.contains("arity mismatch"),
                    "expected arity-mismatch message, got: {reason}",
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    /// Non-Union set-ops by-name are punted (DuckDB does not support
    /// `INTERSECT BY NAME` / `EXCEPT BY NAME`); Union by-name proceeds
    /// normally, see [`setop_union_by_name_skips_positional_cast_pushdown`].
    #[test]
    fn setop_intersect_by_name_punts_with_boundary_prefix() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Intersect,
            all: true,
            by_name: true,
            allow_missing_columns: false,
            children: vec![tiny_int_plan(), tiny_int_plan()],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::PuntedOperator { .. }));
        assert!(err.to_string().starts_with("[TDCK-BOUNDARY]"));
    }

    /// Pass 76 — `UNION BY NAME` used to trip the positional-cast pushdown
    /// (`push_setop_casts`), which mis-cast child columns whenever the child
    /// column order differed from the widened schema order (e.g. corpus
    /// `set-003`). The analyzer now skips pushdown when `by_name = true`; the
    /// emission wrapper aligns child columns to the widened schema by NAME.
    #[test]
    fn setop_union_by_name_skips_positional_cast_pushdown() {
        let bt = BaseTypes::empty();
        // Build two `Values` plans with the same column-name set but in
        // different orders — pushdown would incorrectly cast `x` to `y`'s
        // widened type if it fired.
        let left = CommonAst::new(CommonOp::Values {
            rows: vec![vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("a".to_owned()),
                    data_type: DataType::String,
                }),
            ]],
            column_names: vec!["x".to_owned(), "y".to_owned()],
        });
        // Right side: reversed column order.
        let right = CommonAst::new(CommonOp::Values {
            rows: vec![vec![
                Expression::Literal(Literal {
                    value: LiteralValue::String("b".to_owned()),
                    data_type: DataType::String,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Int(2),
                    data_type: DataType::Integer,
                }),
            ]],
            column_names: vec!["y".to_owned(), "x".to_owned()],
        });
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: false,
            children: vec![left, right],
        });
        let typed = analyze(ast, &bt).unwrap();
        // Widened schema follows first-child column order.
        let (kind, by_name, child_schemas) = match &typed.op {
            TypedOp::SetOp {
                kind,
                by_name,
                children,
                ..
            } => (
                *kind,
                *by_name,
                children
                    .iter()
                    .map(|c| c.resolved_schema.clone())
                    .collect::<Vec<_>>(),
            ),
            other => panic!("expected SetOp, got {other:?}"),
        };
        assert_eq!(kind, SetOpKind::Union);
        assert!(by_name);
        // Neither child's resolved_schema is the widened schema — pushdown
        // is skipped for by_name. Left keeps `[x:Int, y:String]`; right
        // keeps `[y:String, x:Int]`.
        assert_eq!(child_schemas[0].fields[0].name, "x");
        assert_eq!(child_schemas[0].fields[0].data_type, DataType::Integer);
        assert_eq!(child_schemas[1].fields[0].name, "y");
        assert_eq!(child_schemas[1].fields[0].data_type, DataType::String);
    }

    // ── unionByName(allowMissingColumns=True) — Pass 77 (set-004) ────────

    /// Build a single-row `Values` plan with the given `(name, ty, value)`
    /// triples. Column nullability follows Spark's Literal semantics (all
    /// non-null unless the LiteralValue is `Null`).
    fn values_row(cols: &[(&str, DataType, LiteralValue)]) -> CommonAst {
        let row: Vec<Expression> = cols
            .iter()
            .map(|(_, ty, v)| {
                Expression::Literal(Literal {
                    value: v.clone(),
                    data_type: ty.clone(),
                })
            })
            .collect();
        let names: Vec<String> = cols.iter().map(|(n, _, _)| (*n).to_owned()).collect();
        CommonAst::new(CommonOp::Values {
            rows: vec![row],
            column_names: names,
        })
    }

    #[test]
    fn union_by_name_allow_missing_partial_overlap_produces_ordered_union() {
        // LEFT `{a: Long, b: Long}` × RIGHT `{b: Long, c: Long}`
        // Expected widened schema: `{a nullable, b, c nullable}`.
        let bt = BaseTypes::empty();
        let left = values_row(&[
            ("a", DataType::Long, LiteralValue::Long(1)),
            ("b", DataType::Long, LiteralValue::Long(2)),
        ]);
        let right = values_row(&[
            ("b", DataType::Long, LiteralValue::Long(3)),
            ("c", DataType::Long, LiteralValue::Long(4)),
        ]);
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: true,
            children: vec![left, right],
        });
        let typed = analyze(ast, &bt).unwrap();
        let widened = match &typed.op {
            TypedOp::SetOp { widened_schema, .. } => widened_schema.clone(),
            other => panic!("expected SetOp, got {other:?}"),
        };
        assert_eq!(widened.fields.len(), 3);
        assert_eq!(widened.fields[0].name, "a");
        assert!(widened.fields[0].nullable, "a is padded on RIGHT");
        assert_eq!(widened.fields[1].name, "b");
        assert!(
            !widened.fields[1].nullable,
            "b present in both non-null children"
        );
        assert_eq!(widened.fields[2].name, "c");
        assert!(widened.fields[2].nullable, "c is padded on LEFT");
    }

    #[test]
    fn union_by_name_allow_missing_disjoint_schemas() {
        // LEFT `{a, b, c}` × RIGHT `{d, e, f}` → `{a, b, c, d, e, f}`,
        // every field nullable.
        let bt = BaseTypes::empty();
        let left = values_row(&[
            ("a", DataType::Long, LiteralValue::Long(1)),
            ("b", DataType::Long, LiteralValue::Long(2)),
            ("c", DataType::Long, LiteralValue::Long(3)),
        ]);
        let right = values_row(&[
            ("d", DataType::String, LiteralValue::String("x".to_owned())),
            ("e", DataType::String, LiteralValue::String("y".to_owned())),
            ("f", DataType::String, LiteralValue::String("z".to_owned())),
        ]);
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: true,
            children: vec![left, right],
        });
        let typed = analyze(ast, &bt).unwrap();
        let widened = match &typed.op {
            TypedOp::SetOp { widened_schema, .. } => widened_schema.clone(),
            other => panic!("expected SetOp, got {other:?}"),
        };
        let names: Vec<_> = widened.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d", "e", "f"]);
        assert!(widened.fields.iter().all(|f| f.nullable));
    }

    #[test]
    fn union_by_name_allow_missing_widens_shared_column_type() {
        // LEFT `{x: Integer}` × RIGHT `{x: Double, y: Integer}` →
        // `{x: Double, y: Integer nullable}`.
        let bt = BaseTypes::empty();
        let left = values_row(&[("x", DataType::Integer, LiteralValue::Int(1))]);
        let right = values_row(&[
            ("x", DataType::Double, LiteralValue::Double(1.5)),
            ("y", DataType::Integer, LiteralValue::Int(2)),
        ]);
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: true,
            children: vec![left, right],
        });
        let typed = analyze(ast, &bt).unwrap();
        let widened = match &typed.op {
            TypedOp::SetOp { widened_schema, .. } => widened_schema.clone(),
            other => panic!("expected SetOp, got {other:?}"),
        };
        assert_eq!(widened.fields[0].name, "x");
        assert_eq!(widened.fields[0].data_type, DataType::Double);
        assert_eq!(widened.fields[1].name, "y");
        assert_eq!(widened.fields[1].data_type, DataType::Integer);
        assert!(
            widened.fields[1].nullable,
            "y is padded on LEFT, must be nullable"
        );
    }

    #[test]
    fn union_by_name_allow_missing_rejected_without_by_name() {
        // `allow_missing_columns = true` with `by_name = false` is
        // Spark-emulated (Spark's Dataset API forbids the combination).
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: false,
            allow_missing_columns: true,
            children: vec![tiny_int_plan(), tiny_int_plan()],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::Other { ref reason } => {
                assert!(
                    reason.contains("allowMissingColumns"),
                    "expected reason to mention allowMissingColumns, got: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn union_by_name_allow_missing_identical_name_sets_matches_strict() {
        // Degenerate case: same names in both children — the widened schema
        // must match the strict by-name path (Spark parity + set-003 shape).
        let bt = BaseTypes::empty();
        let left = values_row(&[
            ("a", DataType::Long, LiteralValue::Long(1)),
            ("b", DataType::Long, LiteralValue::Long(2)),
        ]);
        let right = values_row(&[
            ("a", DataType::Long, LiteralValue::Long(3)),
            ("b", DataType::Long, LiteralValue::Long(4)),
        ]);
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: true,
            children: vec![left, right],
        });
        let typed = analyze(ast, &bt).unwrap();
        let widened = match &typed.op {
            TypedOp::SetOp { widened_schema, .. } => widened_schema.clone(),
            other => panic!("expected SetOp, got {other:?}"),
        };
        assert_eq!(widened.fields.len(), 2);
        assert_eq!(widened.fields[0].name, "a");
        assert_eq!(widened.fields[1].name, "b");
        assert!(!widened.fields[0].nullable);
        assert!(!widened.fields[1].nullable);
    }

    // ── Display prefix categorization ────────────────────────────────────

    #[test]
    fn spark_emulated_variants_use_spark_prefix() {
        let ut = AnalyzerError::UnknownTable {
            name: "t".to_owned(),
        };
        let uc = AnalyzerError::UnknownColumn {
            name: "c".to_owned(),
            qualifier: None,
        };
        let ac = AnalyzerError::AmbiguousColumn {
            name: "c".to_owned(),
            candidates: vec!["a.c".to_owned(), "b.c".to_owned()],
        };
        let tm = AnalyzerError::TypeMismatch {
            expected: DataType::Boolean,
            actual: DataType::Integer,
            context: "filter".to_owned(),
        };
        let ot = AnalyzerError::Other {
            reason: "x".to_owned(),
        };
        for e in [
            ut.to_string(),
            uc.to_string(),
            ac.to_string(),
            tm.to_string(),
            ot.to_string(),
        ] {
            assert!(
                e.starts_with("[SPARK-EMULATED]"),
                "expected `[SPARK-EMULATED]` prefix, got: {e}",
            );
        }
    }

    #[test]
    fn thunderduck_boundary_variants_use_tdck_prefix() {
        let po = AnalyzerError::PuntedOperator {
            op: "FileScan".to_owned(),
            reason: "wip".to_owned(),
        };
        let ur = AnalyzerError::UnsupportedRule {
            rule: "some_rule".to_owned(),
            reason: "wip".to_owned(),
        };
        for e in [po.to_string(), ur.to_string()] {
            assert!(
                e.starts_with("[TDCK-BOUNDARY]"),
                "expected `[TDCK-BOUNDARY]` prefix, got: {e}",
            );
        }
    }

    // ── Star expansion — schema expanded, tree preserved ─────────────────

    #[test]
    fn project_star_expands_schema_but_keeps_star_in_tree() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let typed = analyze(ast, &bt).unwrap();
        // Schema fully expanded.
        assert_eq!(typed.resolved_schema, emp_schema());
        // Tree keeps Star node.
        match &typed.op {
            TypedOp::Project { projections, .. } => {
                assert!(matches!(&projections[0], Expression::Star(_)));
            }
            _ => panic!("expected Project"),
        }
    }

    // ── has_resolved_schema — INV5 anchor ────────────────────────────────

    #[test]
    fn has_resolved_schema_true_for_analyzed_fixture() {
        for (name, ast, bt, _expected) in analyzer_fixtures::all_fixtures() {
            let typed = analyze(ast, &bt)
                .unwrap_or_else(|e| panic!("fixture `{name}` failed to analyze: {e}"));
            assert!(
                has_resolved_schema(&typed),
                "fixture `{name}` did not report has_resolved_schema=true",
            );
        }
    }

    #[test]
    fn has_resolved_schema_false_for_unresolved_manually_built_typed_ast() {
        // A TypedAst manually built with an Unresolved schema field must
        // report `has_resolved_schema = false`.
        let unresolved = TypedAst {
            op: TypedOp::SingleRow,
            resolved_schema: StructType::new(vec![StructField::nullable(
                "x",
                DataType::Unresolved,
            )]),
        };
        assert!(!has_resolved_schema(&unresolved));

        // Or with a Project whose projection contains an UnresolvedColumn.
        let with_unresolved_expr = TypedAst {
            op: TypedOp::Project {
                input: Box::new(TypedAst {
                    op: TypedOp::SingleRow,
                    resolved_schema: StructType::empty(),
                }),
                projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "x".to_owned(),
                    qualifier: None,
                    plan_id: None,
                })],
            },
            resolved_schema: StructType::new(vec![StructField::nullable("x", DataType::Long)]),
        };
        assert!(!has_resolved_schema(&with_unresolved_expr));
    }

    // ── analyze composes the three passes ───────────────────────────────

    #[test]
    fn analyze_composes_resolve_assign_types_and_derive_nullability() {
        // A Filter over TableScan exercises all three passes end-to-end.
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Gt,
            left: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "salary".to_owned(),
                qualifier: None,
                plan_id: None,
            })),
            right: Box::new(Expression::Literal(Literal {
                value: LiteralValue::Double(50000.0),
                data_type: DataType::Double,
            })),
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            condition: cond,
        });
        let typed = analyze(ast, &bt).unwrap();
        // Schema propagated from input.
        assert_eq!(typed.resolved_schema, emp_schema());
        // Condition resolved.
        match &typed.op {
            TypedOp::Filter { condition, .. } => match condition {
                Expression::Binary(b) => match b.left.as_ref() {
                    Expression::ColumnReference(c) => {
                        assert_eq!(c.data_type.as_ref(), Some(&DataType::Double));
                        assert_eq!(c.nullable, Some(true));
                    }
                    _ => panic!("expected ColumnReference"),
                },
                _ => panic!("expected Binary"),
            },
            _ => panic!("expected Filter"),
        }
    }

    // ── analyzer_error_to_emission_error bridge ─────────────────────────

    #[test]
    fn analyzer_error_bridge_maps_spark_emulated_to_unsupported_expression() {
        let e = AnalyzerError::UnknownColumn {
            name: "c".to_owned(),
            qualifier: None,
        };
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::UnsupportedExpression { shape, reason } => {
                assert_eq!(shape, "analyzer-spark-emulated");
                assert!(reason.starts_with("[SPARK-EMULATED]"));
            }
            _ => panic!("expected UnsupportedExpression"),
        }
    }

    #[test]
    fn analyzer_error_bridge_maps_punted_operator_to_unsupported_op() {
        let e = AnalyzerError::PuntedOperator {
            op: "FileScan".to_owned(),
            reason: "wip".to_owned(),
        };
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::UnsupportedOp { op, .. } => assert_eq!(op, "FileScan"),
            _ => panic!("expected UnsupportedOp"),
        }
    }

    // ── Aggregate output schema uses function names ─────────────────────

    // ── Unpivot output schema ───────────────────────────────────────────

    #[test]
    fn unpivot_stamps_schema_with_widened_value_column() {
        // Anchor: piv-004 shape — ids=[id], values=[dept_id (INT), salary
        // (DOUBLE)]. Spark widens INT + DOUBLE → DOUBLE; salary is nullable
        // so the value column is nullable. Variable column is STRING NOT NULL.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        let typed = analyze(ast, &bt).unwrap();
        let fields = &typed.resolved_schema.fields;
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "id");
        assert_eq!(fields[0].data_type, DataType::Long);
        assert!(!fields[0].nullable);
        assert_eq!(fields[1].name, "metric");
        assert_eq!(fields[1].data_type, DataType::String);
        assert!(!fields[1].nullable);
        assert_eq!(fields[2].name, "value");
        assert_eq!(fields[2].data_type, DataType::Double);
        assert!(fields[2].nullable);
    }

    #[test]
    fn unpivot_empty_values_materialises_all_non_id_columns() {
        // Anchor: Spark's default when `values` is empty is "all non-id
        // input columns". The analyzer must materialise that expansion so
        // the emission stage can render an explicit ON list.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec![],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        let typed = analyze(ast, &bt).unwrap();
        // τ's coarse approximation via `unify_types`' String fallback — Spark
        // itself would raise `UNPIVOT_VALUE_DATA_TYPE_MISMATCH` here for a
        // mixed numeric+string value set; tracking M1 for follow-up hardening
        // (systemic pattern across Unpivot/SetOp/TableFunction).
        match &typed.op {
            TypedOp::Unpivot { values, .. } => {
                assert_eq!(
                    values,
                    &vec!["name".to_owned(), "dept_id".to_owned(), "salary".to_owned()]
                );
            }
            _ => panic!("expected Unpivot"),
        }
    }

    #[test]
    fn unpivot_unknown_id_column_surfaces_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Explicit(vec!["not_a_col".to_owned()]),
            values: vec!["salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::UnknownColumn { name, .. }) => {
                assert_eq!(name, "not_a_col");
            }
            other => panic!("expected UnknownColumn, got: {other:?}"),
        }
    }

    #[test]
    fn unpivot_duplicate_across_ids_and_values_rejected() {
        // M2: `salary` appears in both ids and values. Spark rejects id/value
        // overlap; τ mirrors that with `AnalyzerError::Other`, case-insensitive.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Explicit(vec!["id".to_owned(), "salary".to_owned()]),
            values: vec!["SALARY".to_owned(), "dept_id".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::Other { reason }) => {
                assert!(
                    reason.contains("disjoint") || reason.contains("duplicate"),
                    "reason should mention duplicate/disjoint: {reason}"
                );
                assert!(
                    reason.to_ascii_lowercase().contains("salary"),
                    "reason should surface the offending name: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got: {other:?}"),
        }
    }

    #[test]
    fn unpivot_variable_column_colliding_with_id_rejected() {
        // M3: `variable_column_name` shares a name with an id column
        // (case-insensitive). The stamped schema would produce two "id" fields;
        // Spark rejects — τ mirrors with `AnalyzerError::Other`.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "ID".to_owned(),
            value_column_name: "value".to_owned(),
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::Other { reason }) => {
                assert!(
                    reason.contains("variable column name") && reason.contains("collides"),
                    "reason should describe the collision: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got: {other:?}"),
        }
    }

    #[test]
    fn unpivot_value_column_colliding_with_id_rejected() {
        // M3: `value_column_name` shares a name with an id column. Symmetric
        // to the variable-column case above.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "Id".to_owned(),
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::Other { reason }) => {
                assert!(
                    reason.contains("value column name") && reason.contains("collides"),
                    "reason should describe the collision: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got: {other:?}"),
        }
    }

    // ── Aggregate output schema uses function names ─────────────────────

    #[test]
    fn aggregate_output_schema_stamps_count_result_as_long() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: vec![],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "count".to_owned(),
                args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                })],
                distinct: false,
            })],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
        assert!(!typed.resolved_schema.fields[0].nullable);
    }

    // ── Pivot output schema (Pass 60) ───────────────────────────────────

    /// Explicit-values Pivot with a single aggregate stamps output columns
    /// named after each pivot value verbatim, all nullable (empty buckets
    /// yield NULL per Spark). Corresponds to grp-004:
    ///   emp.groupBy("dept_id").pivot("active", [True, False])
    ///      .agg(count(lit(1)).alias("n"))
    #[test]
    fn analyze_pivot_explicit_bool_values_stamps_single_agg_output_schema() {
        let bt = base_types_with_emp_dept();
        let emp_scan = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        // Add an `active` column to the emp schema via `withColumn` for the
        // test — grp-004 expects an `active` bool column on emp.
        let with_active = CommonAst::new(CommonOp::WithColumns {
            input: Box::new(emp_scan),
            assignments: vec![(
                "active".to_owned(),
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(true),
                    data_type: DataType::Boolean,
                }),
            )],
        });
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(with_active),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "active".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(true),
                    data_type: DataType::Boolean,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(false),
                    data_type: DataType::Boolean,
                }),
            ],
            aggregates: vec![Expression::Alias(AliasExpression {
                alias: "n".to_owned(),
                expr: Box::new(Expression::FunctionCall(FunctionCall {
                    name: "count".to_owned(),
                    args: vec![Expression::Literal(Literal {
                        value: LiteralValue::Int(1),
                        data_type: DataType::Integer,
                    })],
                    distinct: false,
                })),
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        let fields = &typed.resolved_schema.fields;
        // Expected: dept_id + true + false = 3 output columns.
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "dept_id");
        assert_eq!(fields[1].name, "true");
        assert_eq!(fields[2].name, "false");
        // Pivot outputs are always nullable per Spark (empty-bucket NULL).
        assert!(fields[1].nullable);
        assert!(fields[2].nullable);
    }

    /// Implicit-values Pivot (empty pivot_values) is a Thunderduck-boundary
    /// case per ADR-022. τ has no eager-DISTINCT hook
    #[test]
    fn analyze_pivot_implicit_values_returns_boundary_punted_operator() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "salary".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "avg".to_owned(),
                args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "salary".to_owned(),
                    qualifier: None,
                    plan_id: None,
                })],
                distinct: false,
            })],
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::PuntedOperator { op, .. }) => {
                assert_eq!(op, "Pivot[implicit-values]");
            }
            other => panic!("expected PuntedOperator, got {other:?}"),
        }
    }

    /// Multi-aggregate explicit Pivot names outputs `<value>_<agg_alias>`
    /// per Spark. Guard against name-collision between grouping and pivot
    /// output columns as a bonus assertion.
    #[test]
    fn analyze_pivot_multi_agg_names_outputs_value_underscore_alias() {
        let bt = base_types_with_emp_dept();
        let emp_scan = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Int(10),
                    data_type: DataType::Integer,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Int(20),
                    data_type: DataType::Integer,
                }),
            ],
            aggregates: vec![
                Expression::Alias(AliasExpression {
                    alias: "sum_sal".to_owned(),
                    expr: Box::new(Expression::FunctionCall(FunctionCall {
                        name: "sum".to_owned(),
                        args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "salary".to_owned(),
                            qualifier: None,
                            plan_id: None,
                        })],
                        distinct: false,
                    })),
                }),
                Expression::Alias(AliasExpression {
                    alias: "cnt".to_owned(),
                    expr: Box::new(Expression::FunctionCall(FunctionCall {
                        name: "count".to_owned(),
                        args: vec![Expression::Literal(Literal {
                            value: LiteralValue::Int(1),
                            data_type: DataType::Integer,
                        })],
                        distinct: false,
                    })),
                }),
            ],
        });
        let typed = analyze(ast, &bt).unwrap();
        let names: Vec<&str> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        // Grouping (dept_id) + 2 pivot values × 2 aggregates = 5 output cols.
        assert_eq!(
            names,
            vec!["dept_id", "10_sum_sal", "10_cnt", "20_sum_sal", "20_cnt"]
        );
    }

    /// Pass 60 H1 — Spark's Catalyst `Literal.sql` renders integral doubles
    /// with a `.0` suffix. `lit(1.0d)` becomes pivot column `"1.0"`, not
    /// `"1"`. Non-integral doubles use their natural decimal repr.
    #[test]
    fn analyze_pivot_double_values_render_dot_zero_for_integral_spark_parity() {
        let bt = base_types_with_emp_dept();
        let emp_scan = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "salary".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![
                // Integral double → "1.0".
                Expression::Literal(Literal {
                    value: LiteralValue::Double(1.0),
                    data_type: DataType::Double,
                }),
                // Negative integral double → "-2.0".
                Expression::Literal(Literal {
                    value: LiteralValue::Double(-2.0),
                    data_type: DataType::Double,
                }),
                // Non-integral float → "1.5".
                Expression::Literal(Literal {
                    value: LiteralValue::Float(1.5),
                    data_type: DataType::Float,
                }),
            ],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "count".to_owned(),
                args: vec![Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                })],
                distinct: false,
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        let names: Vec<&str> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["dept_id", "1.0", "-2.0", "1.5"]);
    }

    /// Pass 60 H2 — Spark's Catalyst rejects NULL pivot values with a
    /// `Literal expressions required for pivot values` analysis error. τ
    /// mirrors that as an `AnalyzerError::Other` (Spark-emulated).
    #[test]
    fn analyze_pivot_rejects_null_literal_in_pivot_values() {
        let bt = base_types_with_emp_dept();
        let emp_scan = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "salary".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Int(10),
                    data_type: DataType::Integer,
                }),
                // Null in the middle of an otherwise valid list must fail.
                Expression::Literal(Literal {
                    value: LiteralValue::Null,
                    data_type: DataType::Null,
                }),
            ],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "count".to_owned(),
                args: vec![Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                })],
                distinct: false,
            })],
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::Other { reason }) => {
                assert!(
                    reason.contains("null"),
                    "expected null-rejection reason, got: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
    }

    // ── Implicit PIVOT grouping / UNPIVOT ids (pass 107, SQL front-end) ──

    /// SQL PIVOT supplies no grouping list. The analyzer derives it as
    /// `input − pivot column − aggregate-referenced columns`, in input order.
    /// emp = {id, name, dept_id, salary}; pivot on dept_id, agg avg(salary)
    /// ⇒ grouping {id, name}.
    #[test]
    fn analyze_pivot_implicit_grouping_excludes_pivot_and_agg_refs() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: PivotGrouping::Implicit,
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Int(10),
                    data_type: DataType::Integer,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Int(20),
                    data_type: DataType::Integer,
                }),
            ],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "avg".to_owned(),
                args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "salary".to_owned(),
                    qualifier: None,
                    plan_id: None,
                })],
                distinct: false,
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Pivot { grouping, .. } => {
                let names: Vec<&str> = grouping
                    .iter()
                    .map(|g| match g {
                        Expression::ColumnReference(c) => c.name.as_str(),
                        other => panic!("expected resolved ColumnReference, got {other:?}"),
                    })
                    .collect();
                assert_eq!(names, vec!["id", "name"]);
            }
            other => panic!("expected TypedOp::Pivot, got {other:?}"),
        }
    }

    /// `count(*)` references no column (its `Star` argument contributes
    /// nothing), so every non-pivot column stays in the implicit grouping.
    /// pivot on dept_id, agg count(*) ⇒ grouping {id, name, salary}.
    #[test]
    fn analyze_pivot_implicit_grouping_count_star_keeps_all_non_pivot_cols() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: PivotGrouping::Implicit,
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Int(10),
                    data_type: DataType::Integer,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Int(20),
                    data_type: DataType::Integer,
                }),
            ],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "count".to_owned(),
                args: vec![Expression::Star(StarExpression { qualifier: None })],
                distinct: false,
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Pivot { grouping, .. } => {
                let names: Vec<&str> = grouping
                    .iter()
                    .map(|g| match g {
                        Expression::ColumnReference(c) => c.name.as_str(),
                        other => panic!("expected resolved ColumnReference, got {other:?}"),
                    })
                    .collect();
                assert_eq!(names, vec!["id", "name", "salary"]);
            }
            other => panic!("expected TypedOp::Pivot, got {other:?}"),
        }
    }

    /// M2 regression: a column referenced only through a nested `CASE` /
    /// `BETWEEN` must still be excluded from the implicit grouping. Before the
    /// exhaustive `collect_referenced_columns`, the `Between` node fell into a
    /// `_ => {}` catch-all, so `id` leaked back into the grouping (silent wrong
    /// result). Agg `sum(CASE WHEN id BETWEEN 1 AND 2 THEN salary END)` on a
    /// pivot over dept_id references {id, salary}; excluding those plus the
    /// pivot column leaves grouping {name}.
    #[test]
    fn analyze_pivot_implicit_grouping_excludes_column_referenced_through_case_between() {
        let bt = base_types_with_emp_dept();
        let case_between = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(
                Expression::Between(BetweenExpression {
                    expr: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                        name: "id".to_owned(),
                        qualifier: None,
                        plan_id: None,
                    })),
                    low: Box::new(Expression::Literal(Literal {
                        value: LiteralValue::Int(1),
                        data_type: DataType::Integer,
                    })),
                    high: Box::new(Expression::Literal(Literal {
                        value: LiteralValue::Int(2),
                        data_type: DataType::Integer,
                    })),
                    negated: false,
                }),
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "salary".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
            )],
            else_expr: None,
        });
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: PivotGrouping::Implicit,
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![Expression::Literal(Literal {
                value: LiteralValue::Int(10),
                data_type: DataType::Integer,
            })],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "sum".to_owned(),
                args: vec![case_between],
                distinct: false,
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Pivot { grouping, .. } => {
                let names: Vec<&str> = grouping
                    .iter()
                    .map(|g| match g {
                        Expression::ColumnReference(c) => c.name.as_str(),
                        other => panic!("expected resolved ColumnReference, got {other:?}"),
                    })
                    .collect();
                assert_eq!(names, vec!["name"]);
            }
            other => panic!("expected TypedOp::Pivot, got {other:?}"),
        }
    }

    /// M1 regression: pivoting on an EXPRESSION column must exclude the
    /// underlying REFERENCED column, not the expression's output name. Pivot
    /// over `abs(dept_id)` references `dept_id`; the old code excluded the
    /// literal name "abs" (no such column) and left `dept_id` in the grouping.
    /// With structural exclusion, agg `avg(salary)` ⇒ grouping {id, name}.
    #[test]
    fn analyze_pivot_implicit_grouping_expression_pivot_excludes_referenced_column() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: PivotGrouping::Implicit,
            pivot_column: Expression::FunctionCall(FunctionCall {
                name: "abs".to_owned(),
                args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                })],
                distinct: false,
            }),
            pivot_values: vec![Expression::Literal(Literal {
                value: LiteralValue::Int(10),
                data_type: DataType::Integer,
            })],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "avg".to_owned(),
                args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "salary".to_owned(),
                    qualifier: None,
                    plan_id: None,
                })],
                distinct: false,
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Pivot { grouping, .. } => {
                let names: Vec<&str> = grouping
                    .iter()
                    .map(|g| match g {
                        Expression::ColumnReference(c) => c.name.as_str(),
                        other => panic!("expected resolved ColumnReference, got {other:?}"),
                    })
                    .collect();
                assert_eq!(names, vec!["id", "name"]);
            }
            other => panic!("expected TypedOp::Pivot, got {other:?}"),
        }
    }

    /// SQL UNPIVOT lists only value columns; the analyzer derives ids as
    /// `input − values`, in input order. values = {dept_id, salary}
    /// ⇒ ids {id, name}.
    #[test]
    fn analyze_unpivot_implicit_ids_are_input_minus_values() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Implicit,
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "val".to_owned(),
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Unpivot { ids, .. } => {
                assert_eq!(ids, &vec!["id".to_owned(), "name".to_owned()]);
            }
            other => panic!("expected TypedOp::Unpivot, got {other:?}"),
        }
        // Output schema: <ids> + metric (STRING NN) + val (widened nullable).
        let names: Vec<&str> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["id", "name", "metric", "val"]);
    }

    /// `UnpivotIds::Implicit` with an empty value list is nonsensical (both
    /// axes implicit) — the analyzer rejects it.
    #[test]
    fn analyze_unpivot_implicit_ids_empty_values_rejected() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: UnpivotIds::Implicit,
            values: vec![],
            variable_column_name: "metric".to_owned(),
            value_column_name: "val".to_owned(),
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::Other { reason }) => {
                assert!(
                    reason.contains("value column"),
                    "expected value-column reason, got: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
    }

    // ── Review-fix H1: missing dropField target is Spark-emulated error ──

    fn base_types_with_addr_table() -> BaseTypes {
        // A one-column table `addrs(addr STRUCT<street, city, geo>)` — the
        // shape used by struct-005/006 corpus cases.
        let addr_ty = DataType::Struct(StructType::new(vec![
            StructField::nullable("street", DataType::String),
            StructField::nullable("city", DataType::String),
            StructField::nullable("geo", DataType::String),
        ]));
        let scan = CommonAst::new(CommonOp::TableScan {
            table: "addrs".to_owned(),
            alias: None,
        });
        BaseTypes::build_from_plan(&scan, |name| match name {
            "addrs" => Some(StructType::new(vec![StructField::nullable(
                "addr",
                addr_ty.clone(),
            )])),
            _ => None,
        })
    }

    /// Spark 4.1 (Catalyst `UpdateFields.scala::checkInputDataTypes`) rejects
    /// `dropFields("X")` when `X` is not present in the struct. τ mirrors
    /// this as `AnalyzerError::Other` (Spark-emulated). Locking this here
    /// guards against regressing to Spark 3.5's silent-ignore behaviour.
    #[test]
    fn analyze_update_fields_missing_drop_target_is_spark_emulated_error() {
        let bt = base_types_with_addr_table();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "addrs".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UpdateFields(
                super::super::expression::UpdateFieldsExpression {
                    struct_expr: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                        name: "addr".to_owned(),
                        qualifier: None,
                        plan_id: None,
                    })),
                    // `nope` does not exist in the struct — case-insensitive
                    // lookup must still fail.
                    updates: vec![("nope".to_owned(), None)],
                },
            )],
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::Other { reason }) => {
                assert!(
                    reason.contains("nope"),
                    "expected missing-field reason to name `nope`, got: {reason}"
                );
                assert!(
                    reason.contains("dropFields"),
                    "expected reason to mention `dropFields`, got: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
    }

    // ── Pass 65: multi-level nested-struct dot-path access ──────────────────

    fn base_types_with_nested_struct() -> BaseTypes {
        // `emp(id BIGINT, address STRUCT<city STRING, geo STRUCT<lat DOUBLE, lng DOUBLE>>)`
        // — the shape used by struct-004 corpus case.
        let geo_ty = DataType::Struct(StructType::new(vec![
            StructField::nullable("lat", DataType::Double),
            StructField::nullable("lng", DataType::Double),
        ]));
        let addr_ty = DataType::Struct(StructType::new(vec![
            StructField::nullable("city", DataType::String),
            StructField::nullable("zip", DataType::String),
            StructField::nullable("geo", geo_ty),
        ]));
        let scan = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        BaseTypes::build_from_plan(&scan, |name| match name {
            "emp" => Some(StructType::new(vec![
                StructField::not_null("id", DataType::Long),
                StructField::nullable("address", addr_ty.clone()),
            ])),
            _ => None,
        })
    }

    /// `F.col("address.geo.lat")` — the Spark Connect converter emits
    /// `UnresolvedColumn { qualifier: "address", name: "geo.lat" }`. The
    /// analyzer must rewrite this as an `ExtractValue` chain so emission
    /// produces `("address").geo.lat` rather than `"address"."geo.lat"`.
    #[test]
    fn resolve_multi_level_nested_struct_path_becomes_extract_value_chain() {
        let bt = base_types_with_nested_struct();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "geo.lat".to_owned(),
                qualifier: Some("address".to_owned()),
                plan_id: None,
            })],
        });
        let typed = analyze(ast, &bt).expect("multi-level dot path must resolve");
        let proj = match &typed.op {
            TypedOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        assert_eq!(proj.len(), 1, "single projection");
        // Outer ExtractValue(ExtractValue(ColumnReference("address"), "geo"), "lat")
        let outer = match &proj[0] {
            Expression::ExtractValue(ev) => ev,
            other => panic!("expected ExtractValue, got {other:?}"),
        };
        match outer.extraction.as_ref() {
            Expression::Literal(Literal {
                value: LiteralValue::String(s),
                ..
            }) => assert_eq!(s, "lat"),
            other => panic!("expected String literal 'lat', got {other:?}"),
        }
        let inner = match outer.child.as_ref() {
            Expression::ExtractValue(ev) => ev,
            other => panic!("expected nested ExtractValue, got {other:?}"),
        };
        match inner.extraction.as_ref() {
            Expression::Literal(Literal {
                value: LiteralValue::String(s),
                ..
            }) => assert_eq!(s, "geo"),
            other => panic!("expected String literal 'geo', got {other:?}"),
        }
        match inner.child.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "address");
                assert!(c.qualifier.is_none(), "root ColumnReference is unqualified");
            }
            other => panic!("expected root ColumnReference('address'), got {other:?}"),
        }
        // Output schema records the leaf field type — nullable Double.
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Double);
        assert!(typed.resolved_schema.fields[0].nullable);
    }

    /// Single-level dot access (`F.col("address.city")`, struct-002) must
    /// NOT be rewritten — it already emits correctly as `"address"."city"`
    /// and we want zero regression on the passing case.
    #[test]
    fn resolve_single_level_nested_struct_path_stays_as_column_reference() {
        let bt = base_types_with_nested_struct();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "city".to_owned(),
                qualifier: Some("address".to_owned()),
                plan_id: None,
            })],
        });
        let typed = analyze(ast, &bt).expect("single-level dot path must resolve");
        let proj = match &typed.op {
            TypedOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        match &proj[0] {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "city");
                assert_eq!(c.qualifier.as_deref(), Some("address"));
            }
            other => panic!("expected ColumnReference, got {other:?}"),
        }
    }

    /// Unknown nested field on an otherwise valid struct qualifier must
    /// fall through to the standard resolver so the caller sees a proper
    /// `UnknownColumn` (Spark-emulated) error rather than a silent rewrite.
    #[test]
    fn resolve_unknown_nested_field_falls_through_to_unknown_column() {
        let bt = base_types_with_nested_struct();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "geo.nope".to_owned(),
                qualifier: Some("address".to_owned()),
                plan_id: None,
            })],
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::UnknownColumn { .. }) => {}
            other => panic!("expected UnknownColumn error, got {other:?}"),
        }
    }

    /// Case-insensitive drop matching a real field must succeed (not error).
    /// Anchors the CI match in `validate_update_fields_ops`.
    #[test]
    fn analyze_update_fields_drop_field_case_insensitive_ok() {
        let bt = base_types_with_addr_table();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "addrs".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UpdateFields(
                super::super::expression::UpdateFieldsExpression {
                    struct_expr: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                        name: "addr".to_owned(),
                        qualifier: None,
                        plan_id: None,
                    })),
                    updates: vec![("GEO".to_owned(), None)],
                },
            )],
        });
        // Successful analysis is the assertion.
        analyze(ast, &bt).expect("case-insensitive drop must analyze cleanly");
    }

    // ── Describe / Summary analysis (Pass 80) ────────────────────────────

    fn base_types_with_emp() -> BaseTypes {
        let plan = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema()),
            _ => None,
        })
    }

    fn assert_stats_output_schema(schema: &StructType, expected_col_names: &[&str]) {
        assert_eq!(
            schema.fields.len(),
            expected_col_names.len() + 1,
            "stats output schema has 1 (summary) + N stat cols",
        );
        assert_eq!(schema.fields[0].name, "summary");
        assert_eq!(schema.fields[0].data_type, DataType::String);
        // Spark parity: `summary` is stamped nullable in Spark's schema even
        // though every emitted value is a literal string.
        assert!(
            schema.fields[0].nullable,
            "`summary` column must be nullable per Spark parity"
        );
        for (idx, want) in expected_col_names.iter().enumerate() {
            let f = &schema.fields[idx + 1];
            assert_eq!(f.name, *want, "field #{idx} name");
            assert_eq!(f.data_type, DataType::String, "field #{idx} is STRING");
            assert!(f.nullable, "field #{idx} must be nullable");
        }
    }

    #[test]
    fn analyze_describe_stamps_summary_col_plus_string_nullable_per_input_col() {
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Describe {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            cols: vec!["dept_id".to_owned(), "salary".to_owned()],
        });
        let typed = analyze(ast, &bt).expect("analyze describe");
        assert_stats_output_schema(&typed.resolved_schema, &["dept_id", "salary"]);
        match typed.op {
            TypedOp::Describe { cols, .. } => {
                assert_eq!(cols, vec!["dept_id".to_owned(), "salary".to_owned()]);
            }
            _ => panic!("expected TypedOp::Describe"),
        }
    }

    #[test]
    fn analyze_describe_empty_cols_expands_to_all_input_cols_in_order() {
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Describe {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            cols: vec![],
        });
        let typed = analyze(ast, &bt).expect("analyze describe");
        assert_stats_output_schema(&typed.resolved_schema, &["id", "name", "dept_id", "salary"]);
    }

    #[test]
    fn analyze_describe_unknown_column_surfaces_spark_emulated_error() {
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Describe {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            cols: vec!["missing".to_owned()],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn analyze_summary_empty_statistics_applies_default_eight_stats() {
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Summary {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            statistics: vec![],
        });
        let typed = analyze(ast, &bt).expect("analyze summary");
        // Schema always covers the full input column list.
        assert_stats_output_schema(&typed.resolved_schema, &["id", "name", "dept_id", "salary"]);
        match typed.op {
            TypedOp::Summary { statistics, .. } => {
                assert_eq!(
                    statistics,
                    DEFAULT_SUMMARY_STATS
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect::<Vec<_>>(),
                );
            }
            _ => panic!("expected TypedOp::Summary"),
        }
    }

    // ── FreqItems / Crosstab analysis (Pass 82) ──────────────────────────

    /// Fixture with a stats-shaped schema that exercises all four
    /// element-type variants (Integer, String, Double, Decimal). Pins ADR-015
    /// Spark parity: freqItems must stamp `Array<source_type>` per column
    /// (never a hardcoded `Array<String>`).
    fn base_types_with_stats() -> BaseTypes {
        let stats_schema = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("name", DataType::String),
            StructField::nullable("salary", DataType::Double),
            StructField::nullable(
                "bonus",
                DataType::Decimal {
                    precision: 9,
                    scale: 2,
                },
            ),
        ]);
        let plan = CommonAst::new(CommonOp::TableScan {
            table: "stats".to_owned(),
            alias: None,
        });
        BaseTypes::build_from_plan(&plan, |name| match name {
            "stats" => Some(stats_schema.clone()),
            _ => None,
        })
    }

    #[test]
    fn analyze_freq_items_stamps_array_of_source_type_per_col() {
        let bt = base_types_with_stats();
        let ast = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "stats".to_owned(),
                alias: None,
            })),
            cols: vec![
                "dept_id".to_owned(),
                "name".to_owned(),
                "salary".to_owned(),
                "bonus".to_owned(),
            ],
            support: 0.3,
        });
        let typed = analyze(ast, &bt).expect("analyze freqItems");
        // Schema arity: one column per input col (no `summary` prefix — Spark
        // freqItems doesn't emit a summary label column).
        assert_eq!(typed.resolved_schema.fields.len(), 4);
        // Spark parity: each element type mirrors the source col.
        let expected: &[(&str, DataType)] = &[
            ("dept_id_freqItems", DataType::Integer),
            ("name_freqItems", DataType::String),
            ("salary_freqItems", DataType::Double),
            (
                "bonus_freqItems",
                DataType::Decimal {
                    precision: 9,
                    scale: 2,
                },
            ),
        ];
        for (idx, (want_name, want_elem)) in expected.iter().enumerate() {
            let f = &typed.resolved_schema.fields[idx];
            assert_eq!(f.name, *want_name, "field #{idx} name");
            match &f.data_type {
                DataType::Array(elem, _contains_null) => {
                    assert_eq!(
                        elem.as_ref(),
                        want_elem,
                        "field #{idx} element type must mirror source col (ADR-015)"
                    );
                }
                other => panic!("field #{idx} expected Array<{want_elem:?}>, got {other:?}"),
            }
            assert!(
                !f.nullable,
                "field #{idx} must be non-nullable per Spark parity — LIST(...) never returns NULL"
            );
        }
        match typed.op {
            TypedOp::FreqItems { cols, support, .. } => {
                assert_eq!(cols.len(), 4);
                assert!((support - 0.3).abs() < f64::EPSILON);
            }
            _ => panic!("expected TypedOp::FreqItems"),
        }
    }

    #[test]
    fn analyze_freq_items_case_insensitive_column_lookup() {
        let bt = base_types_with_stats();
        let ast = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "stats".to_owned(),
                alias: None,
            })),
            // `Dept_ID` must resolve to `dept_id`.
            cols: vec!["Dept_ID".to_owned()],
            support: 0.01,
        });
        let typed = analyze(ast, &bt).expect("case-insensitive freqItems must analyze");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        // Output name preserves caller casing (matches Describe/Summary).
        assert_eq!(typed.resolved_schema.fields[0].name, "Dept_ID_freqItems");
        // Element type still resolves to Integer via field_by_name (also
        // case-insensitive).
        match &typed.resolved_schema.fields[0].data_type {
            DataType::Array(elem, _) => assert_eq!(elem.as_ref(), &DataType::Integer),
            other => panic!("expected Array<Integer>, got {other:?}"),
        }
    }

    #[test]
    fn analyze_freq_items_unknown_column_surfaces_spark_emulated_error() {
        let bt = base_types_with_stats();
        let ast = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "stats".to_owned(),
                alias: None,
            })),
            cols: vec!["nope".to_owned()],
            support: 0.01,
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn analyze_crosstab_returns_punted_operator_thunderduck_boundary() {
        let bt = base_types_with_stats();
        let ast = CommonAst::new(CommonOp::Crosstab {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "stats".to_owned(),
                alias: None,
            })),
            col1: "dept_id".to_owned(),
            col2: "salary".to_owned(),
        });
        let err = analyze(ast, &bt).unwrap_err();
        match &err {
            AnalyzerError::PuntedOperator { op, .. } => {
                assert_eq!(op, "Crosstab[dynamic-values]");
            }
            other => panic!("expected PuntedOperator, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[TDCK-BOUNDARY]"));
    }

    // ── Sample / SampleBy analysis (Pass 83) ─────────────────────────────

    #[test]
    fn analyze_sample_schema_passthrough() {
        // Anchor: `df.sample(0.5, seed=11)` produces the same schema as the
        // input relation — Sample is schema-preserving.
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sample {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            lower_bound: 0.0,
            upper_bound: 0.5,
            with_replacement: false,
            seed: Some(11),
        });
        let typed = analyze(ast, &bt).expect("analyze Sample");
        assert_eq!(typed.resolved_schema, emp_schema());
        match &typed.op {
            TypedOp::Sample {
                lower_bound,
                upper_bound,
                with_replacement,
                seed,
                ..
            } => {
                assert!((*lower_bound - 0.0).abs() < f64::EPSILON);
                assert!((*upper_bound - 0.5).abs() < f64::EPSILON);
                assert!(!with_replacement);
                assert_eq!(*seed, Some(11));
            }
            other => panic!("expected TypedOp::Sample, got {other:?}"),
        }
        assert!(has_resolved_schema(&typed));
    }

    #[test]
    fn analyze_sample_with_replacement_flag_is_accepted_by_analyzer() {
        // `with_replacement = true` is a Thunderduck-boundary case rejected by
        // the emission stage, not the analyzer. This test pins the analyzer's
        // schema-only responsibility.
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sample {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            lower_bound: 0.0,
            upper_bound: 0.5,
            with_replacement: true,
            seed: None,
        });
        let typed = analyze(ast, &bt).expect("analyzer does not reject with_replacement=true");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn analyze_sample_by_resolves_col_and_passes_schema() {
        // Anchor — samp-002: `sampleBy("dept_id", {10:0.5,...})` resolves the
        // stratum column against the input schema and preserves the schema.
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::SampleBy {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            col: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            fractions: vec![
                (
                    Literal {
                        value: LiteralValue::Int(10),
                        data_type: DataType::Integer,
                    },
                    0.5,
                ),
                (
                    Literal {
                        value: LiteralValue::Int(20),
                        data_type: DataType::Integer,
                    },
                    1.0,
                ),
            ],
            seed: Some(11),
        });
        let typed = analyze(ast, &bt).expect("analyze SampleBy");
        assert_eq!(typed.resolved_schema, emp_schema());
        match &typed.op {
            TypedOp::SampleBy {
                col,
                fractions,
                seed,
                ..
            } => {
                match col {
                    Expression::ColumnReference(c) => {
                        assert_eq!(c.name, "dept_id");
                        assert_eq!(c.data_type.as_ref(), Some(&DataType::Integer));
                    }
                    other => panic!("expected ColumnReference, got {other:?}"),
                }
                assert_eq!(fractions.len(), 2);
                assert_eq!(*seed, Some(11));
            }
            other => panic!("expected TypedOp::SampleBy, got {other:?}"),
        }
        assert!(has_resolved_schema(&typed));
    }

    #[test]
    fn analyze_summary_explicit_statistics_are_preserved() {
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Summary {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            statistics: vec![
                "count".to_owned(),
                "min".to_owned(),
                "25%".to_owned(),
                "75%".to_owned(),
                "max".to_owned(),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze summary");
        match typed.op {
            TypedOp::Summary { statistics, .. } => {
                assert_eq!(statistics.len(), 5);
                assert_eq!(statistics[2], "25%");
            }
            _ => panic!("expected TypedOp::Summary"),
        }
    }

    // ── Pass 85 — expand_regex_projections + resolution predicate ────────

    fn regex_test_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("customer_id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("order_id", DataType::Long),
        ])
    }

    #[test]
    fn expand_regex_projections_matches_2_of_3_fields_in_schema_order() {
        let schema = regex_test_schema();
        let projections = vec![Expression::UnresolvedRegex(UnresolvedRegexExpression {
            pattern: ".*_id".to_owned(),
            plan_id: Some(9),
        })];
        let expanded = expand_regex_projections(projections, &schema).expect("expand ok");
        assert_eq!(expanded.len(), 2);
        match &expanded[0] {
            Expression::UnresolvedColumn(u) => {
                assert_eq!(u.name, "customer_id");
                assert_eq!(u.plan_id, Some(9));
            }
            _ => panic!("expected UnresolvedColumn"),
        }
        match &expanded[1] {
            Expression::UnresolvedColumn(u) => assert_eq!(u.name, "order_id"),
            _ => panic!("expected UnresolvedColumn"),
        }
    }

    #[test]
    fn expand_regex_projections_invalid_regex_returns_other_error() {
        let schema = regex_test_schema();
        let projections = vec![Expression::UnresolvedRegex(UnresolvedRegexExpression {
            // Unbalanced `[` — invalid on both java.util.regex and Rust regex.
            pattern: "[unclosed".to_owned(),
            plan_id: None,
        })];
        let err = expand_regex_projections(projections, &schema).unwrap_err();
        assert!(matches!(err, AnalyzerError::Other { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn expand_regex_projections_zero_match_returns_unknown_column() {
        let schema = regex_test_schema();
        let projections = vec![Expression::UnresolvedRegex(UnresolvedRegexExpression {
            pattern: "no_such_.*_col".to_owned(),
            plan_id: None,
        })];
        let err = expand_regex_projections(projections, &schema).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "no_such_.*_col");
                assert!(qualifier.is_none());
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn expand_regex_projections_preserves_non_regex_projections_in_place() {
        let schema = regex_test_schema();
        let non_regex_before = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "name".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let non_regex_after = Expression::Literal(Literal {
            value: LiteralValue::Int(1),
            data_type: DataType::Integer,
        });
        let projections = vec![
            non_regex_before.clone(),
            Expression::UnresolvedRegex(UnresolvedRegexExpression {
                pattern: ".*_id".to_owned(),
                plan_id: None,
            }),
            non_regex_after.clone(),
        ];
        let expanded = expand_regex_projections(projections, &schema).expect("expand ok");
        // Layout: [name, customer_id, order_id, literal_1]
        assert_eq!(expanded.len(), 4);
        assert_eq!(expanded[0], non_regex_before);
        match &expanded[1] {
            Expression::UnresolvedColumn(u) => assert_eq!(u.name, "customer_id"),
            _ => panic!("expected UnresolvedColumn at [1]"),
        }
        match &expanded[2] {
            Expression::UnresolvedColumn(u) => assert_eq!(u.name, "order_id"),
            _ => panic!("expected UnresolvedColumn at [2]"),
        }
        assert_eq!(expanded[3], non_regex_after);
    }

    #[test]
    fn expression_is_fully_resolved_returns_false_for_unresolved_regex() {
        let expr = Expression::UnresolvedRegex(UnresolvedRegexExpression {
            pattern: ".*".to_owned(),
            plan_id: None,
        });
        assert!(!expression_is_fully_resolved(&expr));
    }

    // ── Pass 90 — expand_inline_projections ──────────────────────────────

    /// Helper — build `F.array(F.struct(col("name"), col("salary")))` shape
    /// exactly as the ingress converter produces it for the corpus witness.
    /// Uses fields present in this file's `emp_schema` fixture (`name` STRING?,
    /// `salary` DOUBLE?) so `resolve_and_stamp` finds them at analysis time.
    fn array_of_struct_name_salary() -> Expression {
        let struct_call = Expression::FunctionCall(FunctionCall {
            name: "struct".to_owned(),
            args: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "name".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "salary".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
            ],
            distinct: false,
        });
        Expression::FunctionCall(FunctionCall {
            name: "array".to_owned(),
            args: vec![struct_call],
            distinct: false,
        })
    }

    fn inline_call(outer: bool, arg: Expression) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: if outer { "inline_outer" } else { "inline" }.to_owned(),
            args: vec![arg],
            distinct: false,
        })
    }

    /// Canonical inl-001 shape: `select("id", inline(array(struct(name, age))))`
    /// widens into `[id, name, age]` — one projection per struct field, with
    /// synthesized `Alias(inline_field(arr, "<n>"), "<n>")` shape.
    #[test]
    fn expand_inline_projections_widens_into_n_fields() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
                inline_call(false, array_of_struct_name_salary()),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        // Output schema: [id, name, salary].
        assert_eq!(
            typed
                .resolved_schema
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "name", "salary"],
        );
        // name field is String, salary field is Double.
        assert_eq!(typed.resolved_schema.fields[1].data_type, DataType::String);
        assert_eq!(typed.resolved_schema.fields[2].data_type, DataType::Double);
        // Post-expansion tree: three projections, latter two are
        // Alias(inline_field(arr, "<n>"), "<n>").
        match &typed.op {
            TypedOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 3);
                for (i, expected) in ["name", "salary"].iter().enumerate() {
                    match &projections[i + 1] {
                        Expression::Alias(a) => {
                            assert_eq!(a.alias, *expected);
                            match a.expr.as_ref() {
                                Expression::FunctionCall(f) => {
                                    assert_eq!(f.name, "inline_field");
                                    assert_eq!(f.args.len(), 2);
                                    match &f.args[1] {
                                        Expression::Literal(Literal {
                                            value: LiteralValue::String(s),
                                            ..
                                        }) => assert_eq!(s, *expected),
                                        other => {
                                            panic!("expected string literal, got {other:?}")
                                        }
                                    }
                                }
                                other => panic!("expected FunctionCall, got {other:?}"),
                            }
                        }
                        other => panic!("expected Alias, got {other:?}"),
                    }
                }
            }
            _ => panic!("expected Project op"),
        }
    }

    /// `inline_outer` widens the same way, but every output field is nullable
    /// (Spark's `Inline` with `outer=true` — sentinel all-NULL row).
    #[test]
    fn expand_inline_outer_projections_marks_all_nullable() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![inline_call(true, array_of_struct_name_salary())],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        assert_eq!(typed.resolved_schema.fields.len(), 2);
        for f in &typed.resolved_schema.fields {
            assert!(
                f.nullable,
                "inline_outer output field `{}` must be nullable",
                f.name
            );
        }
    }

    /// Prefix + suffix projections around an `inline` are preserved in place
    /// — mirrors `expand_regex_projections_preserves_non_regex_projections_in_place`.
    #[test]
    fn expand_inline_preserves_prefix_and_suffix_projections() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
                inline_call(false, array_of_struct_name_salary()),
                Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        // Layout: [id, name, salary, <literal_output_name>].
        let names: Vec<&str> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names.len(), 4);
        assert_eq!(names[0], "id");
        assert_eq!(names[1], "name");
        assert_eq!(names[2], "salary");
        // 4th field is the literal; the specific name is
        // `expression_output_name`'s convention (not the focus of this test).
    }

    /// Non-`Array<Struct<...>>` argument → Spark-emulated TypeMismatch. The
    /// element is INT, not STRUCT — Spark's `Inline` rejects at analysis time.
    #[test]
    fn expand_inline_rejects_non_array_of_struct() {
        // `arr : Array<Integer>` — element is not a struct.
        let bad_arg = Expression::FunctionCall(FunctionCall {
            name: "array".to_owned(),
            args: vec![Expression::Literal(Literal {
                value: LiteralValue::Int(1),
                data_type: DataType::Integer,
            })],
            distinct: false,
        });
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let err = expand_inline_projections(vec![inline_call(false, bad_arg)], &schema)
            .expect_err("must reject non-Array<Struct<...>>");
        assert!(matches!(err, AnalyzerError::TypeMismatch { .. }));
        assert!(
            err.to_string().starts_with("[SPARK-EMULATED]"),
            "err: {err}"
        );
    }

    /// Unresolvable arg → Thunderduck-boundary [`AnalyzerError::UnsupportedRule`]
    /// (Display prefix `[TDCK-BOUNDARY]`). The message must be honest — no
    /// silent fallthrough to a DuckDB catalog error. ADR-022 category-2.
    #[test]
    fn expand_inline_boundary_rejects_unresolved_element_type() {
        // Reference a column that doesn't exist in the schema — data_type
        // returns `Unresolved`, which we treat as a boundary case.
        let unresolved_arg = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "no_such_col".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let err = expand_inline_projections(vec![inline_call(false, unresolved_arg)], &schema)
            .expect_err("must reject Unresolved arg type");
        match &err {
            AnalyzerError::UnsupportedRule { rule, reason } => {
                assert_eq!(rule, "inline-expansion");
                assert!(
                    reason.contains("could not be statically resolved"),
                    "reason must diagnose the unresolved type; got: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::UnsupportedRule, got {other:?}"),
        }
        assert!(
            err.to_string().starts_with("[TDCK-BOUNDARY]"),
            "boundary error must carry `[TDCK-BOUNDARY]` Display prefix per ADR-022; got: {err}"
        );
    }

    /// Sibling boundary — same rule name for `inline_outer` (used to distinguish
    /// the `[TDCK-BOUNDARY]` origin in reviewer / operator diagnostics).
    #[test]
    fn expand_inline_outer_boundary_rejects_unresolved_element_type_with_tdck_prefix() {
        let unresolved_arg = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "no_such_col".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let err = expand_inline_projections(vec![inline_call(true, unresolved_arg)], &schema)
            .expect_err("must reject Unresolved arg type for inline_outer");
        match &err {
            AnalyzerError::UnsupportedRule { rule, .. } => {
                assert_eq!(rule, "inline_outer-expansion");
            }
            other => panic!("expected AnalyzerError::UnsupportedRule, got {other:?}"),
        }
        assert!(
            err.to_string().starts_with("[TDCK-BOUNDARY]"),
            "boundary error must carry `[TDCK-BOUNDARY]` Display prefix per ADR-022; got: {err}"
        );
    }

    // ── Pass 91 — expand_json_tuple_projections ──────────────────────────

    fn json_tuple_call(json_col: &str, keys: &[&str]) -> Expression {
        let mut args: Vec<Expression> = Vec::with_capacity(keys.len() + 1);
        args.push(Expression::UnresolvedColumn(UnresolvedColumn {
            name: json_col.to_owned(),
            qualifier: None,
            plan_id: None,
        }));
        for k in keys {
            args.push(Expression::Literal(Literal {
                value: LiteralValue::String((*k).to_owned()),
                data_type: DataType::String,
            }));
        }
        Expression::FunctionCall(FunctionCall {
            name: "json_tuple".to_owned(),
            args,
            distinct: false,
        })
    }

    fn raw_schema_with_json_str() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("json_str", DataType::String),
        ])
    }

    fn base_types_with_raw() -> BaseTypes {
        let plan = CommonAst::new(CommonOp::TableScan {
            table: "raw".to_owned(),
            alias: None,
        });
        BaseTypes::build_from_plan(&plan, |name| match name {
            "raw" => Some(raw_schema_with_json_str()),
            _ => None,
        })
    }

    /// Canonical json-002 shape: `select("id", json_tuple("json_str", "a", "e"))`
    /// widens into `[id, c0, c1]` — positional names, both nullable STRING.
    #[test]
    fn expand_json_tuple_widens_into_n_fields_with_positional_names() {
        let bt = base_types_with_raw();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "raw".to_owned(),
                alias: None,
            })),
            projections: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
                json_tuple_call("json_str", &["a", "e"]),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        // Output schema: [id, c0, c1].
        let names: Vec<&str> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["id", "c0", "c1"]);
        // Both fanout fields are String, nullable.
        assert_eq!(typed.resolved_schema.fields[1].data_type, DataType::String);
        assert!(typed.resolved_schema.fields[1].nullable);
        assert_eq!(typed.resolved_schema.fields[2].data_type, DataType::String);
        assert!(typed.resolved_schema.fields[2].nullable);
        // Post-expansion tree: three projections, latter two are
        // Alias(json_tuple_field(json_str, "<k>"), "c<i>").
        match &typed.op {
            TypedOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 3);
                for (i, expected_key) in ["a", "e"].iter().enumerate() {
                    match &projections[i + 1] {
                        Expression::Alias(a) => {
                            assert_eq!(a.alias, format!("c{i}"));
                            match a.expr.as_ref() {
                                Expression::FunctionCall(f) => {
                                    assert_eq!(f.name, "json_tuple_field");
                                    assert_eq!(f.args.len(), 2);
                                    match &f.args[1] {
                                        Expression::Literal(Literal {
                                            value: LiteralValue::String(s),
                                            ..
                                        }) => assert_eq!(s, *expected_key),
                                        other => {
                                            panic!("expected string literal, got {other:?}")
                                        }
                                    }
                                }
                                other => panic!("expected FunctionCall, got {other:?}"),
                            }
                        }
                        other => panic!("expected Alias, got {other:?}"),
                    }
                }
            }
            _ => panic!("expected Project op"),
        }
    }

    /// Prefix + suffix projections around a `json_tuple` are preserved in
    /// place — mirrors `expand_inline_preserves_prefix_and_suffix_projections`.
    #[test]
    fn expand_json_tuple_preserves_prefix_and_suffix_projections() {
        let bt = base_types_with_raw();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "raw".to_owned(),
                alias: None,
            })),
            projections: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
                json_tuple_call("json_str", &["a", "e"]),
                Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        let names: Vec<&str> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names.len(), 4);
        assert_eq!(names[0], "id");
        assert_eq!(names[1], "c0");
        assert_eq!(names[2], "c1");
        // The 4th field is the literal; its exact name follows
        // `expression_output_name`'s convention (not the focus of this test).
    }

    /// Zero keys (`json_tuple(json)`) → Spark-emulated `Other` error.
    #[test]
    fn expand_json_tuple_rejects_zero_keys() {
        let err = expand_json_tuple_projections(vec![json_tuple_call("json_str", &[])])
            .expect_err("must reject arity < 2");
        assert!(matches!(err, AnalyzerError::Other { .. }));
        assert!(
            err.to_string().starts_with("[SPARK-EMULATED]"),
            "err: {err}"
        );
    }

    /// Non-literal key arg → Spark-emulated `TypeMismatch`.
    #[test]
    fn expand_json_tuple_rejects_non_literal_key() {
        let bad_call = Expression::FunctionCall(FunctionCall {
            name: "json_tuple".to_owned(),
            args: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "json_str".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "k".to_owned(),
                    qualifier: None,
                    plan_id: None,
                }),
            ],
            distinct: false,
        });
        let err =
            expand_json_tuple_projections(vec![bad_call]).expect_err("must reject non-literal key");
        assert!(matches!(err, AnalyzerError::TypeMismatch { .. }));
        assert!(
            err.to_string().starts_with("[SPARK-EMULATED]"),
            "err: {err}"
        );
    }

    /// Boundary-reject unsafe key chars → `[TDCK-BOUNDARY]` prefix,
    /// `rule = "json_tuple-expansion"`.
    #[test]
    fn expand_json_tuple_boundary_rejects_unsafe_key_chars() {
        // Single-quote in key would break the emitted SQL string literal.
        let err = expand_json_tuple_projections(vec![json_tuple_call("json_str", &["a'b"])])
            .expect_err("must reject key containing '");
        match &err {
            AnalyzerError::UnsupportedRule { rule, .. } => {
                assert_eq!(rule, "json_tuple-expansion");
            }
            other => panic!("expected AnalyzerError::UnsupportedRule, got {other:?}"),
        }
        assert!(
            err.to_string().starts_with("[TDCK-BOUNDARY]"),
            "boundary error must carry `[TDCK-BOUNDARY]` Display prefix per ADR-022; got: {err}"
        );
        // Dot / bracket in key would path-walk in DuckDB's json_extract_string
        // but Spark treats them as flat key chars → boundary reject.
        for bad_key in ["a.b", "a[0]"] {
            let err = expand_json_tuple_projections(vec![json_tuple_call("json_str", &[bad_key])])
                .expect_err("must reject JSONPath metachars in key");
            match &err {
                AnalyzerError::UnsupportedRule { rule, .. } => {
                    assert_eq!(rule, "json_tuple-expansion");
                }
                other => panic!("expected AnalyzerError::UnsupportedRule, got {other:?}"),
            }
        }
    }
}
