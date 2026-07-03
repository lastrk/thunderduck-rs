//! τ's `CommonAst` — the substrate-independent plan tree shared by every
//! front-end (Spark Connect proto, SparkSQL, future front-ends).
//!
//! **INV10:** this file imports ONLY value-level types from `crate::types`
//! (`StructType`) plus intra-τ modules. No `crate::logical`, `crate::parser`,
//! `crate::generator`, `crate::functions`, `crate::runtime`,
//! `crate::types::TypeInferenceEngine`.
//!
//! The wrapper `CommonAst { op: CommonOp }` exists so Slice B can attach
//! resolution metadata (resolved schema, plan_id, etc.) without a source-wide
//! refactor. Slice A.2 keeps the wrapper minimal.

use super::expression::{Expression, SortOrder};
use crate::types::StructType;

/// τ's canonical plan tree — a single wrapper around a [`CommonOp`] variant.
///
/// Slice B extends this wrapper (e.g. `pub resolved_schema: Option<StructType>`).
/// Slice A.2 keeps it as a thin wrapper so the extension is additive.
#[derive(Debug, Clone, PartialEq)]
pub struct CommonAst {
    /// The plan operator this node represents.
    pub op: CommonOp,
}

impl CommonAst {
    /// Construct a `CommonAst` wrapping the given [`CommonOp`].
    pub fn new(op: CommonOp) -> Self {
        Self { op }
    }
}

/// The canonical plan operator set shared by every τ front-end.
///
/// Slice A.2 covers the structured shapes needed by the round-trip tests
/// (Project / Filter / Sort / Limit / Aggregate primitive / TableScan /
/// FileScan / Values / LocalRelation / Join / TableFunction / Unnest /
/// SingleRow). Deferred plan shapes (SetOp, SubqueryAlias, WithColumns,
/// Distinct, Sample, ShowString, Tail, DropColumns, ToDataFrame, NA family,
/// Pivot, Stat family, Repartition/Hint passthrough) surface as
/// [`super::EmissionError::UnsupportedProtoShape`] until later slices grow
/// their variants. There is **no** opaque `Sql` variant — parser_v2 owns SQL
/// text (Open Decision 1 Option 1b).
#[derive(Debug, Clone, PartialEq)]
pub enum CommonOp {
    // ── Relational (structured) ──────────────────────────────────────────
    /// `SELECT projections FROM input`.
    Project {
        /// The input relation.
        input: Box<CommonAst>,
        /// The projection list — a sequence of expressions (with optional
        /// aliases) evaluated against the input schema.
        projections: Vec<Expression>,
    },

    /// `SELECT * FROM input WHERE condition`.
    Filter {
        /// The input relation.
        input: Box<CommonAst>,
        /// The predicate — a Boolean-valued expression.
        condition: Expression,
    },

    /// `SELECT * FROM input ORDER BY order [LIMIT limit] [OFFSET offset]`.
    ///
    /// The `Sort` variant carries optional `limit` / `offset` so an
    /// `OFFSET` alone (without a preceding `Sort` in the source plan) still
    /// lowers to a single node.
    Sort {
        /// The input relation.
        input: Box<CommonAst>,
        /// The sort keys (may be empty when representing a bare OFFSET).
        order: Vec<SortOrder>,
        /// Optional LIMIT — evaluated after sorting.
        limit: Option<i64>,
        /// Optional OFFSET — evaluated after sorting (and after LIMIT).
        offset: Option<i64>,
    },

    /// `SELECT * FROM input LIMIT limit [OFFSET offset]`.
    Limit {
        /// The input relation.
        input: Box<CommonAst>,
        /// The maximum number of rows to return.
        limit: i64,
        /// Optional OFFSET.
        offset: Option<i64>,
    },

    /// `SELECT aggregates FROM input GROUP BY grouping`.
    ///
    /// **Primitive Aggregate only.** Rollup / Cube / GroupingSets / Pivot
    /// surface as [`super::EmissionError::UnsupportedProtoShape`] and land
    /// in Slice G.
    ///
    /// # Slice A.2 invariant on `aggregates`
    ///
    /// The SparkSQL front-end at Slice A.2 (`parser_v2::v2_lowering::
    /// lower_aggregate_select`) folds the SELECT projection list — grouping
    /// columns and aggregate calls both — into `aggregates` when the query
    /// has a GROUP BY, mirroring the raw projection. This matches the
    /// protobuf `Aggregate` semantics where `aggregate_expressions` already
    /// carries the composite output list. Slice C.1's emission table will
    /// unfold this back into a strict `{grouping, aggregates}` split at
    /// emission time.
    ///
    // TODO(Slice C.1): unfold parser-folded grouping columns from `aggregates`.
    Aggregate {
        /// The input relation.
        input: Box<CommonAst>,
        /// The grouping expressions (may be empty for global aggregation).
        grouping: Vec<Expression>,
        /// The aggregate expressions. See variant-level doc for the Slice A.2
        /// folding invariant — the SparkSQL front-end may include grouping
        /// columns here alongside the actual aggregate calls until Slice C.1
        /// unfolds them.
        aggregates: Vec<Expression>,
        /// The grouping kind — GroupBy (default), Rollup, Cube, or
        /// GroupingSets (Slice G — Pivot lives elsewhere).
        grouping_kind: GroupingKind,
    },

    // ── Leaves ───────────────────────────────────────────────────────────
    /// A relation with exactly one row and zero columns — the identity for
    /// `SELECT literal` with no `FROM`.
    SingleRow,

    /// A named table scan (`SELECT * FROM table`).
    TableScan {
        /// The table name (as written by the caller).
        table: String,
        /// Optional alias (e.g. `FROM t AS x`).
        alias: Option<String>,
    },

    /// An in-line `VALUES (...) AS t(col_names)` relation.
    Values {
        /// One expression list per row.
        rows: Vec<Vec<Expression>>,
        /// The column names for the resulting relation.
        column_names: Vec<String>,
    },

    /// A `createDataFrame` payload — Arrow-IPC rows parsed into
    /// `Expression::Literal` values. **No synthesized SQL** — the emission
    /// arm is responsible for rendering (per §2.1 anti-SQL anchor).
    LocalRelation {
        /// The declared schema (may include columns not present in `rows`).
        schema: StructType,
        /// One expression list per row (each element must be an
        /// `Expression::Literal`).
        rows: Vec<Vec<Expression>>,
    },

    /// A file-format scan (`spark.read.parquet(...)`, etc.).
    FileScan {
        /// The file format.
        format: FileFormat,
        /// One or more file paths / globs.
        paths: Vec<String>,
        /// Optional declared schema.
        schema: Option<StructType>,
        /// Format-specific options (e.g. `header=true`).
        options: Vec<(String, String)>,
    },

    /// A table-valued function call (`spark.table("...")` alternatives).
    TableFunction {
        /// The function name.
        name: String,
        /// The function arguments.
        args: Vec<Expression>,
        /// Whether to emit an ordinality column (`WITH ORDINALITY`).
        with_ordinality: bool,
    },

    /// `UNNEST(expr) [WITH ORDINALITY]`.
    Unnest {
        /// The array/map expression being unnested.
        expr: Expression,
        /// Whether to emit an ordinality column.
        with_ordinality: bool,
    },

    // ── Set operations (Slice B) ─────────────────────────────────────────
    /// A n-ary set operation: UNION / INTERSECT / EXCEPT.
    ///
    /// **Slice B adds this variant.** Set-op widening (analyzer's downward
    /// sub-sweep, per rearchitect ADR-006) runs across `children` to compute
    /// the widened schema; the resolved schema is stamped by the analyzer.
    /// `UNION BY NAME` (`by_name = true`) is deferred to Slice G and surfaces
    /// as `AnalyzerError::PuntedOperator` today.
    SetOp {
        /// The kind of set operation.
        kind: SetOpKind,
        /// Whether duplicates are preserved (`UNION ALL`) or removed
        /// (`UNION DISTINCT`).
        all: bool,
        /// Whether column matching is by-name (`UNION BY NAME`) or by-position.
        by_name: bool,
        /// The n-ary children of the set operation.
        children: Vec<CommonAst>,
    },

    /// `df.na.fill(values, subset=cols)`. `values` has length 1 (fill all
    /// subset cols with that scalar) or length == cols.len() (per-column).
    NaFill {
        /// The input relation.
        input: Box<CommonAst>,
        /// Subset of column names (empty = all columns of matching type).
        cols: Vec<String>,
        /// Fill values (Literal expressions).
        values: Vec<Expression>,
    },

    /// `df.na.drop(how, subset=cols, thresh)`. Row is dropped if fewer than
    /// `min_non_nulls` of the subset cols are non-null. When `min_non_nulls`
    /// is None, all subset cols must be non-null (how="any").
    NaDrop {
        /// The input relation.
        input: Box<CommonAst>,
        /// Subset of columns to inspect.
        cols: Vec<String>,
        /// Optional minimum non-null count.
        min_non_nulls: Option<i32>,
    },

    /// `df.na.replace([old], [new], subset=cols)`.
    NaReplace {
        /// The input relation.
        input: Box<CommonAst>,
        /// Subset of column names.
        cols: Vec<String>,
        /// (old, new) pairs.
        replacements: Vec<(Expression, Expression)>,
    },

    /// `df.unpivot(ids, values, variable_column_name, value_column_name)` /
    /// `df.melt(...)` (PySpark alias). Wide → long transformation.
    ///
    /// **Semantics:** produces `<ids>` columns unchanged plus two new columns:
    /// `variable_column_name` (STRING NOT NULL, the source column name) and
    /// `value_column_name` (Spark-widened common type across all `values`
    /// columns, nullable if any source value column is nullable).
    ///
    /// `ids` and `values` are column *names* (mirror legacy
    /// `logical::Unpivot`). When `values` is empty, Spark defaults to "all
    /// non-id columns"; the analyzer materialises that expansion so the
    /// emission stage sees a fully-resolved column list.
    Unpivot {
        /// The input relation.
        input: Box<CommonAst>,
        /// Id columns — preserved verbatim in the output.
        ids: Vec<String>,
        /// Value columns to unpivot. Empty ⇒ analyzer expands to all
        /// non-id input columns per Spark semantics.
        values: Vec<String>,
        /// The name of the output variable column (source-column names).
        variable_column_name: String,
        /// The name of the output value column (source-column values).
        value_column_name: String,
    },

    /// `df.groupBy(...).pivot(col, [values]).agg(...)` — rotate rows to
    /// columns. Grouping columns remain as rows; the pivot column's distinct
    /// values (either supplied explicitly or discovered eagerly at runtime by
    /// DuckDB's PIVOT operator when `pivot_values` is empty) become new column
    /// headers, one per aggregate per pivot value.
    ///
    /// **Semantics:** the output schema is `<grouping> + <pivot_value_i × agg_j>`.
    /// When multiple aggregates are supplied, output column names follow
    /// Spark's `<pivot_value>_<agg_alias>` convention; when a single aggregate
    /// is supplied, output columns are named after the pivot values verbatim.
    /// Empty `pivot_values` (implicit / "eager discovery" per Spark) is a
    /// Thunderduck-boundary case per ADR-022 — the τ analyzer rejects with
    /// `PuntedOperator("Pivot[implicit-values]")` because implementing it
    /// needs a session-injected DISTINCT-query hook (Slice G).
    Pivot {
        /// The input relation.
        input: Box<CommonAst>,
        /// Grouping columns (preserved as rows).
        grouping: Vec<Expression>,
        /// The column whose distinct values become new column headers.
        pivot_column: Expression,
        /// Explicit list of pivot values (Literal expressions). Empty ⇒
        /// DuckDB eagerly discovers values at PIVOT execution.
        pivot_values: Vec<Expression>,
        /// One or more aggregate expressions applied per pivot cell.
        aggregates: Vec<Expression>,
    },

    /// `df.dropDuplicates([cols])` / `df.distinct()`. `on_columns` empty ⇒
    /// dedupe on the full row (`SELECT DISTINCT *`). Non-empty ⇒
    /// `SELECT DISTINCT ON (cols) * FROM ...`.
    Deduplicate {
        /// The input relation.
        input: Box<CommonAst>,
        /// Optional subset of columns to dedupe on. Empty = all columns.
        on_columns: Vec<String>,
    },

    /// `df.alias(name)` — wrap the input in a named subquery alias.
    /// Semantically transparent for schemas (analyzer passes input schema
    /// through); the alias name is retained for scope-resolution in
    /// downstream operators.
    AliasedRelation {
        /// The input relation.
        input: Box<CommonAst>,
        /// The alias to apply.
        alias: String,
    },

    /// `df.toDF(new1, new2, ...)` — positional rename all columns.
    /// Analyzer must have input schema to zip positional names.
    ToDf {
        /// The input relation.
        input: Box<CommonAst>,
        /// The new positional names.
        column_names: Vec<String>,
    },

    /// `df.withColumnsRenamed({old: new, ...})` — rename columns.
    /// Column identity + types + nullability are preserved.
    WithColumnsRenamed {
        /// The input relation.
        input: Box<CommonAst>,
        /// Old-name → new-name renames (order preserved).
        renames: Vec<(String, String)>,
    },

    // ── Column-list extensions ───────────────────────────────────────────
    /// `df.drop(col1, col2, ...)` — remove named columns from the input.
    /// Analyzer produces an output schema equal to input minus the named
    /// columns (case-insensitive per Spark). Missing names are silently
    /// ignored per Spark semantics.
    DropColumns {
        /// The input relation.
        input: Box<CommonAst>,
        /// Column names to remove.
        drop_names: Vec<String>,
    },

    /// `df.withColumn(name, expr)` / `df.withColumns({...})` — add or replace
    /// per-name column assignments over `input`. Semantics: for each
    /// `(name, expr)`, if `name` matches an existing input column
    /// (case-insensitive per Spark), the column is *replaced*; otherwise the
    /// column is *appended*. Duplicate names within `assignments` are a
    /// Spark-emulated error surfaced by the analyzer.
    WithColumns {
        /// The input relation.
        input: Box<CommonAst>,
        /// One `(column_name, expression)` per proto `Alias`. Order matters:
        /// later assignments referencing an earlier assignment's name see the
        /// pre-assignment value (analyzer resolves against input schema, not
        /// intermediate state — matches Spark's `withColumn` semantics).
        assignments: Vec<(String, Expression)>,
    },

    // ── Join with first-class plan_ids (§2.3) ────────────────────────────
    /// A binary join.
    ///
    /// `left_plan_ids` / `right_plan_ids` carry the set of proto `plan_id`s
    /// that appear on each side of the join. Slice B's analyzer uses these
    /// to disambiguate column references on either side without string
    /// qualifier encoding.
    Join {
        /// The left relation.
        left: Box<CommonAst>,
        /// The right relation.
        right: Box<CommonAst>,
        /// The join type.
        join_type: JoinType,
        /// Optional `ON` condition.
        condition: Option<Expression>,
        /// USING column names (empty when `ON` is used).
        using_columns: Vec<String>,
        /// Plan-ids appearing anywhere under the left side.
        left_plan_ids: Vec<i64>,
        /// Plan-ids appearing anywhere under the right side.
        right_plan_ids: Vec<i64>,
    },
}

/// The file formats supported by [`CommonOp::FileScan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileFormat {
    /// Apache Parquet.
    Parquet,
    /// CSV / delimited text.
    Csv,
    /// JSON / JSON-lines.
    Json,
    /// Apache ORC.
    Orc,
}

/// The set-operation kind carried by [`CommonOp::SetOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SetOpKind {
    /// `UNION` / `UNION ALL` — vertical concatenation.
    Union,
    /// `INTERSECT` — row intersection.
    Intersect,
    /// `EXCEPT` (`MINUS`) — row set difference.
    Except,
}

/// GROUP BY variants for [`CommonOp::Aggregate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupingKind {
    /// Standard `GROUP BY cols`.
    GroupBy,
    /// `GROUP BY ROLLUP(cols)`.
    Rollup,
    /// `GROUP BY CUBE(cols)`.
    Cube,
    /// `GROUP BY GROUPING SETS((cols), (cols), ...)` — Slice G structured
    /// form. The `grouping` list carries all distinct cols in first-appear
    /// order; the set membership is applied at emission time (not yet
    /// wired; punts as `UnsupportedOp` today).
    GroupingSets,
}

/// The join types supported by [`CommonOp::Join`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JoinType {
    /// `INNER JOIN`.
    Inner,
    /// `LEFT OUTER JOIN`.
    Left,
    /// `RIGHT OUTER JOIN`.
    Right,
    /// `FULL OUTER JOIN`.
    Full,
    /// `CROSS JOIN`.
    Cross,
    /// `LEFT SEMI JOIN`.
    LeftSemi,
    /// `LEFT ANTI JOIN`.
    LeftAnti,
}

#[cfg(test)]
mod tests {
    use super::super::expression::{Literal, LiteralValue};
    use super::*;
    use crate::types::{DataType, StructField};

    #[test]
    fn common_ast_single_row_constructs() {
        let plan = CommonAst::new(CommonOp::SingleRow);
        assert!(matches!(plan.op, CommonOp::SingleRow));
    }

    #[test]
    fn common_op_project_carries_projections() {
        let lit = Expression::Literal(Literal {
            value: LiteralValue::Int(42),
            data_type: DataType::Integer,
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![lit.clone()],
        });
        match plan.op {
            CommonOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 1);
                assert_eq!(projections[0], lit);
            }
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn common_op_join_carries_plan_ids() {
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::SingleRow)),
            right: Box::new(CommonAst::new(CommonOp::SingleRow)),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![1, 2],
            right_plan_ids: vec![3],
        });
        match plan.op {
            CommonOp::Join {
                left_plan_ids,
                right_plan_ids,
                ..
            } => {
                // Anchor: plan_ids are `Vec<i64>`, not `Vec<String>` (§2.3).
                assert_eq!(left_plan_ids, vec![1i64, 2]);
                assert_eq!(right_plan_ids, vec![3i64]);
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn common_op_local_relation_carries_expression_literals() {
        // Anti-SQL anchor (§2.1): LocalRelation rows are Expression::Literal,
        // never raw SQL text.
        let schema = StructType::single("x", DataType::Integer);
        let row = vec![Expression::Literal(Literal {
            value: LiteralValue::Int(1),
            data_type: DataType::Integer,
        })];
        let plan = CommonAst::new(CommonOp::LocalRelation {
            schema,
            rows: vec![row],
        });
        match plan.op {
            CommonOp::LocalRelation { rows, .. } => {
                assert_eq!(rows.len(), 1);
                assert!(matches!(rows[0][0], Expression::Literal(_)));
            }
            _ => panic!("expected LocalRelation"),
        }
    }

    #[test]
    fn common_op_setop_construction() {
        // Anchor: SetOp variant + SetOpKind enum land at Slice B.
        let child_a = CommonAst::new(CommonOp::SingleRow);
        let child_b = CommonAst::new(CommonOp::SingleRow);
        let plan = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: false,
            children: vec![child_a, child_b],
        });
        match plan.op {
            CommonOp::SetOp {
                kind,
                all,
                by_name,
                children,
            } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(all);
                assert!(!by_name);
                assert_eq!(children.len(), 2);
            }
            _ => panic!("expected SetOp"),
        }
    }

    #[test]
    fn common_op_unpivot_carries_ids_values_and_output_names() {
        // Anchor: Unpivot variant lands with `ids` + `values` as column names
        // (mirrors legacy `logical::Unpivot`), plus explicit variable/value
        // column names for the two new output columns.
        let plan = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            ids: vec!["id".to_owned()],
            values: vec!["age".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        match plan.op {
            CommonOp::Unpivot {
                ids,
                values,
                variable_column_name,
                value_column_name,
                ..
            } => {
                assert_eq!(ids, vec!["id".to_owned()]);
                assert_eq!(values, vec!["age".to_owned(), "salary".to_owned()]);
                assert_eq!(variable_column_name, "metric");
                assert_eq!(value_column_name, "value");
            }
            _ => panic!("expected Unpivot"),
        }
    }

    #[test]
    fn common_op_pivot_carries_grouping_pivot_column_values_and_aggregates() {
        // Anchor (Pass 60): Pivot variant lands with `grouping`,
        // `pivot_column`, `pivot_values` (empty ⇒ implicit / DuckDB-eager),
        // and `aggregates` — mirrors the legacy `logical::Pivot` shape.
        use super::super::expression::{Literal, LiteralValue, UnresolvedColumn};
        let group = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "dept_id".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let pivot_col = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "active".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let v_true = Expression::Literal(Literal {
            value: LiteralValue::Boolean(true),
            data_type: DataType::Boolean,
        });
        let v_false = Expression::Literal(Literal {
            value: LiteralValue::Boolean(false),
            data_type: DataType::Boolean,
        });
        let agg = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "n".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let plan = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            grouping: vec![group.clone()],
            pivot_column: pivot_col.clone(),
            pivot_values: vec![v_true.clone(), v_false.clone()],
            aggregates: vec![agg.clone()],
        });
        match plan.op {
            CommonOp::Pivot {
                grouping,
                pivot_column,
                pivot_values,
                aggregates,
                ..
            } => {
                assert_eq!(grouping, vec![group]);
                assert_eq!(pivot_column, pivot_col);
                assert_eq!(pivot_values, vec![v_true, v_false]);
                assert_eq!(aggregates, vec![agg]);
            }
            _ => panic!("expected Pivot"),
        }
    }

    #[test]
    fn common_op_pivot_empty_values_signals_implicit_discovery() {
        // Anchor (Pass 60): empty `pivot_values` ⇒ DuckDB PIVOT eagerly
        // discovers distinct values at runtime (grp-005 semantics).
        use super::super::expression::UnresolvedColumn;
        let plan = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            grouping: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "active".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![],
            aggregates: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "salary".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        match plan.op {
            CommonOp::Pivot { pivot_values, .. } => {
                assert!(pivot_values.is_empty());
            }
            _ => panic!("expected Pivot"),
        }
    }

    #[test]
    fn common_op_file_scan_uses_file_format_enum() {
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let plan = CommonAst::new(CommonOp::FileScan {
            format: FileFormat::Parquet,
            paths: vec!["/tmp/x.parquet".to_owned()],
            schema: Some(schema),
            options: vec![],
        });
        match plan.op {
            CommonOp::FileScan { format, .. } => {
                assert_eq!(format, FileFormat::Parquet);
            }
            _ => panic!("expected FileScan"),
        }
    }
}
