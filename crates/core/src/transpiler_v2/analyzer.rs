//! τ's analyzer (Slice B) — resolve, assign types, derive nullability.
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

use super::ast::{CommonAst, CommonOp, FileFormat, JoinType};
use super::base_types::BaseTypes;
use super::error::EmissionError;
use super::expression::{
    AliasExpression, BinaryExpression, CaseWhenExpression, CastExpression, ColumnReference,
    Expression, FunctionCall, SortOrder, StarExpression, UnaryExpression, UnresolvedColumn,
};
use super::type_inference::TypeInferenceEngine;
use crate::types::{DataType, StructField, StructType};

// Re-export SetOpKind so downstream callers can use `analyzer::SetOpKind`.
pub use super::ast::SetOpKind;

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
        /// Aggregate expressions (may fold grouping columns per Slice A.2
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
        /// flipping. Retained for Slice E's join emitter.
        derived_left_schema: StructType,
        /// The right side's per-column schema **after** outer-join
        /// nullability flipping. Retained for Slice E's join emitter.
        derived_right_schema: StructType,
    },
    /// A set operation (UNION / INTERSECT / EXCEPT).
    SetOp {
        /// The kind of set operation.
        kind: SetOpKind,
        /// Whether duplicates are preserved.
        all: bool,
        /// By-name matching (Slice G).
        by_name: bool,
        /// The typed children.
        children: Vec<TypedAst>,
        /// The widened output schema — the analyzer's post-sub-sweep result.
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
    /// A file-format scan (declared-schema only at Slice B; schema-less
    /// forms surface as `PuntedOperator("FileScan", "Slice F")`).
    FileScan {
        /// The file format.
        format: FileFormat,
        /// One or more file paths / globs.
        paths: Vec<String>,
        /// The declared schema (required at Slice B).
        schema: StructType,
        /// Format-specific options.
        options: Vec<(String, String)>,
    },
    /// A table-valued function call — Slice B punts (Slice F).
    TableFunction {
        /// The function name.
        name: String,
        /// The function arguments.
        args: Vec<Expression>,
        /// Whether to emit an ordinality column.
        with_ordinality: bool,
    },
    /// `UNNEST(expr) [WITH ORDINALITY]` — Slice B punts (Slice F).
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
}

/// A typed attribute — the resolved shape of a single output column.
///
/// Currently a projection over [`StructField`] with an optional `qualifier`
/// and `plan_id`. Slice B does not thread `TypedAttr` through the tree — the
/// per-node `resolved_schema: StructType` carries the same information at
/// coarser granularity. `TypedAttr` is retained so Slice E can attach
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
        | TypedOp::NaReplace { input, .. } => has_resolved_schema(input),
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
                .map(|row| resolve_expr_list(row, &schema))
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
                reason: "schema-less FileScan (parquet inference) lands in Slice F".to_owned(),
            }),
        },

        CommonOp::TableFunction {
            name,
            args: _,
            with_ordinality: _,
        } => Err(AnalyzerError::PuntedOperator {
            op: format!("TableFunction[{name}]"),
            reason: "table-function analysis lands in Slice F".to_owned(),
        }),

        CommonOp::Unnest {
            expr: _,
            with_ordinality: _,
        } => Err(AnalyzerError::PuntedOperator {
            op: "Unnest".to_owned(),
            reason: "unnest analysis lands in Slice F".to_owned(),
        }),

        // ── Unary ─────────────────────────────────────────────────────────
        CommonOp::Project { input, projections } => {
            let typed_input = analyze_node(*input, base_types)?;
            let projections = projections
                .into_iter()
                .map(|e| resolve_and_stamp(e, &typed_input.resolved_schema))
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
            let condition = resolve_and_stamp(condition, &typed_input.resolved_schema)?;
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
                    let expr = resolve_and_stamp(*so.expr, &typed_input.resolved_schema)?;
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
            let grouping = resolve_expr_list(grouping, &typed_input.resolved_schema)?;
            let aggregates = resolve_expr_list(aggregates, &typed_input.resolved_schema)?;
            // Output schema construction:
            // SparkSQL path folds grouping cols into `aggregates` already
            // (per CommonOp::Aggregate invariant), so output = aggregates as-is.
            // DataFrame path keeps them separate — detect by seeing whether
            // the aggregates list already begins with the grouping's output
            // names; if not, prepend grouping. Empty grouping = global agg
            // (no unfolding needed).
            let agg_names: Vec<String> =
                aggregates.iter().map(expression_output_name).collect();
            let group_names: Vec<String> =
                grouping.iter().map(expression_output_name).collect();
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
                let resolved = resolve_and_stamp(expr, input_schema)?;
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
        CommonOp::NaFill { input, cols, values } => {
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

        // ── ToDf (Spark `df.toDF(new1, new2, ...)`) ──────────────────────
        CommonOp::ToDf { input, column_names } => {
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
            let mut output_fields: Vec<StructField> =
                Vec::with_capacity(input_fields.len());
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
                    let qualified =
                        qualify_plan_id_refs(c, &left_plan_ids, &right_plan_ids);
                    Some(resolve_and_stamp(
                        qualified,
                        &combined_input_schema,
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
                let using_lower: std::collections::HashSet<String> = using_columns
                    .iter()
                    .map(|s| s.to_lowercase())
                    .collect();
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
            children,
        } => {
            // UNION BY NAME is analyzed by name-matching each column across
            // children; INTERSECT / EXCEPT BY NAME are not supported by
            // DuckDB itself so we still punt those to a future slice.
            if by_name && !matches!(kind, SetOpKind::Union) {
                return Err(AnalyzerError::PuntedOperator {
                    op: format!("SetOp[{kind:?} BY NAME]"),
                    reason: "by-name INTERSECT/EXCEPT unsupported in DuckDB".to_owned(),
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
            // NAME (case-insensitive) — each child must have the same NAME
            // SET; per-name type unify.
            let widened_schema = if by_name {
                // First child's name order is canonical (Spark semantics).
                let first_schema = &typed_children[0].resolved_schema;
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
                    let mut widened_type = first_field.data_type.clone();
                    let mut widened_nullable = first_field.nullable;
                    for child in typed_children.iter().skip(1) {
                        let f = &child.resolved_schema.fields[col_idx];
                        widened_type =
                            TypeInferenceEngine::unify_types(&widened_type, &f.data_type);
                        widened_nullable = widened_nullable || f.nullable;
                    }
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
            // rely on Slice E to emit the CAST at render time.
            for child in typed_children.iter_mut() {
                push_setop_casts(child, &widened_schema);
            }

            Ok(TypedAst {
                op: TypedOp::SetOp {
                    kind,
                    all,
                    by_name,
                    children: typed_children,
                    widened_schema: widened_schema.clone(),
                },
                resolved_schema: widened_schema,
            })
        }
    }
}

// ── Expression resolution helpers ───────────────────────────────────────────

/// Resolve every `UnresolvedColumn` in `expr` against `schema` and stamp
/// resolved `ColumnReference`s with `data_type` and `nullable`.
fn resolve_and_stamp(expr: Expression, schema: &StructType) -> Result<Expression, AnalyzerError> {
    match expr {
        Expression::UnresolvedColumn(u) => resolve_column(u, schema),
        Expression::ColumnReference(c) => {
            let stamped = stamp_column_reference(c, schema);
            Ok(Expression::ColumnReference(stamped))
        }
        Expression::Literal(_) | Expression::Star(_) => Ok(expr),
        Expression::Binary(mut b) => {
            b.left = Box::new(resolve_and_stamp(*b.left, schema)?);
            b.right = Box::new(resolve_and_stamp(*b.right, schema)?);
            Ok(Expression::Binary(b))
        }
        Expression::Unary(mut u) => {
            u.operand = Box::new(resolve_and_stamp(*u.operand, schema)?);
            Ok(Expression::Unary(u))
        }
        Expression::FunctionCall(mut f) => {
            f.args = f
                .args
                .into_iter()
                .map(|a| resolve_and_stamp(a, schema))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::FunctionCall(f))
        }
        Expression::Cast(mut c) => {
            c.expr = Box::new(resolve_and_stamp(*c.expr, schema)?);
            Ok(Expression::Cast(c))
        }
        Expression::CaseWhen(mut cw) => {
            cw.branches = cw
                .branches
                .into_iter()
                .map(|(w, t)| {
                    let w = resolve_and_stamp(w, schema)?;
                    let t = resolve_and_stamp(t, schema)?;
                    Ok::<_, AnalyzerError>((w, t))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(e) = cw.else_expr {
                cw.else_expr = Some(Box::new(resolve_and_stamp(*e, schema)?));
            }
            Ok(Expression::CaseWhen(cw))
        }
        Expression::Window(mut w) => {
            w.func = Box::new(resolve_and_stamp(*w.func, schema)?);
            w.partition_by = w
                .partition_by
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema))
                .collect::<Result<Vec<_>, _>>()?;
            let mut new_order = Vec::with_capacity(w.order_by.len());
            for so in w.order_by {
                let e = resolve_and_stamp(*so.expr, schema)?;
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
            a.expr = Box::new(resolve_and_stamp(*a.expr, schema)?);
            Ok(Expression::Alias(a))
        }
        Expression::Between(mut b) => {
            b.expr = Box::new(resolve_and_stamp(*b.expr, schema)?);
            b.low = Box::new(resolve_and_stamp(*b.low, schema)?);
            b.high = Box::new(resolve_and_stamp(*b.high, schema)?);
            Ok(Expression::Between(b))
        }
        Expression::InList(mut i) => {
            i.expr = Box::new(resolve_and_stamp(*i.expr, schema)?);
            i.list = i
                .list
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::InList(i))
        }
        Expression::Like(mut l) => {
            l.value = Box::new(resolve_and_stamp(*l.value, schema)?);
            l.pattern = Box::new(resolve_and_stamp(*l.pattern, schema)?);
            Ok(Expression::Like(l))
        }
        Expression::IsDistinctFrom(mut d) => {
            d.left = Box::new(resolve_and_stamp(*d.left, schema)?);
            d.right = Box::new(resolve_and_stamp(*d.right, schema)?);
            Ok(Expression::IsDistinctFrom(d))
        }
        Expression::ExtractValue(mut ev) => {
            ev.child = Box::new(resolve_and_stamp(*ev.child, schema)?);
            ev.extraction = Box::new(resolve_and_stamp(*ev.extraction, schema)?);
            Ok(Expression::ExtractValue(ev))
        }
        Expression::ArrayLiteral(mut a) => {
            a.elements = a
                .elements
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::ArrayLiteral(a))
        }
        Expression::MapLiteral(mut m) => {
            m.entries = m
                .entries
                .into_iter()
                .map(|(k, v)| {
                    let k = resolve_and_stamp(k, schema)?;
                    let v = resolve_and_stamp(v, schema)?;
                    Ok::<_, AnalyzerError>((k, v))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::MapLiteral(m))
        }
        Expression::StructLiteral(mut s) => {
            s.fields = s
                .fields
                .into_iter()
                .map(|(n, e)| Ok::<_, AnalyzerError>((n, resolve_and_stamp(e, schema)?)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::StructLiteral(s))
        }
        Expression::RowConstructor(mut rc) => {
            rc.elements = rc
                .elements
                .into_iter()
                .map(|e| resolve_and_stamp(e, schema))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::RowConstructor(rc))
        }
        Expression::UpdateFields(mut u) => {
            u.struct_expr = Box::new(resolve_and_stamp(*u.struct_expr, schema)?);
            u.updates = u
                .updates
                .into_iter()
                .map(|(n, e)| Ok::<_, AnalyzerError>((n, resolve_and_stamp(e, schema)?)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expression::UpdateFields(u))
        }
        // Subquery / lambda / raw-sql / interval — Slice B leaves them
        // opaque. The subquery's inner CommonAst is not re-analyzed here;
        // Slice F owns subquery analysis.
        Expression::InSubquery(_)
        | Expression::ExistsSubquery(_)
        | Expression::ScalarSubquery(_)
        | Expression::Lambda(_)
        | Expression::LambdaVariable(_)
        | Expression::RawSql(_)
        | Expression::Interval(_) => Ok(expr),
    }
}

fn resolve_expr_list(
    exprs: Vec<Expression>,
    schema: &StructType,
) -> Result<Vec<Expression>, AnalyzerError> {
    exprs
        .into_iter()
        .map(|e| resolve_and_stamp(e, schema))
        .collect()
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
fn qualify_plan_id_refs(
    expr: Expression,
    left_ids: &[i64],
    right_ids: &[i64],
) -> Expression {
    fn walk(e: Expression, left_ids: &[i64], right_ids: &[i64]) -> Expression {
        match e {
            Expression::UnresolvedColumn(u) if u.qualifier.is_none() => {
                let synth = match u.plan_id {
                    Some(pid) if left_ids.contains(&pid) => {
                        Some(TD_JOIN_LEFT.to_owned())
                    }
                    Some(pid) if right_ids.contains(&pid) => {
                        Some(TD_JOIN_RIGHT.to_owned())
                    }
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
                    .map(|(c, v)| {
                        (
                            walk(c, left_ids, right_ids),
                            walk(v, left_ids, right_ids),
                        )
                    })
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

fn resolve_column(u: UnresolvedColumn, schema: &StructType) -> Result<Expression, AnalyzerError> {
    // Qualified: `qualifier.name` — the analyzer accepts both a top-level
    // qualifier column (a struct field access) and a direct match on the
    // outer name; ambiguity is not surfaced for qualified references at
    // Slice B (the plan_id + qualifier disambiguation lands in Slice E's
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
                && u.updates
                    .iter()
                    .all(|(_, e)| expression_is_fully_resolved(e))
        }
        // Subquery bodies: opaque at Slice B (Slice F owns).
        Expression::InSubquery(_)
        | Expression::ExistsSubquery(_)
        | Expression::ScalarSubquery(_)
        | Expression::Lambda(_)
        | Expression::RawSql(_)
        | Expression::Interval(_) => true,
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
    // At Slice B, we don't rewrite field qualifiers into names — the alias
    // is preserved on the operator itself. Slice E's renderer handles the
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
                // struct field / qualifier (Slice B keeps it simple —
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
    // (TableScan, Values, ...) receive their CAST at emission time (Slice E).
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
        BinaryExpression, BinaryOp, FunctionCall, Literal, LiteralValue,
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

    #[test]
    fn setop_by_name_punts_with_boundary_prefix() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            children: vec![tiny_int_plan(), tiny_int_plan()],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::PuntedOperator { .. }));
        assert!(err.to_string().starts_with("[TDCK-BOUNDARY]"));
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
}
