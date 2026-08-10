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

use std::collections::{BTreeSet, HashMap, HashSet};

use super::ast::{
    grouped_aggregate, CommonAst, CommonOp, FileFormat, JoinType, PivotGrouping, UnpivotIds,
};
use super::base_types::BaseTypes;
use super::error::{EmissionError, UnsupportedKind};
use super::expression::{
    materialize_binary_coercions, AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression,
    CastExpression, ColumnReference, Expression, ExtractValueExpression, FunctionCall, Literal,
    LiteralValue, SortOrder, SubqueryPlan, UnaryExpression, UnaryOp, UnresolvedColumn,
};
use super::generator::{Generator, GeneratorKind};
use super::name_fold::{eq_fold, fold_key};
use super::schema::{Attribute, ResolvedSchema};
use super::type_inference::{
    is_aggregate_classifier_name, is_nondeterministic_fn_name, TypeInferenceEngine,
};
use crate::types::{DataType, StructField, StructType};

// Re-export SetOpKind so downstream callers can use `analyzer::SetOpKind`.
pub use super::ast::SetOpKind;

/// The eight Spark defaults for `df.summary()` when no statistics list is
/// supplied — matches `Dataset.summary()` in Apache Spark 4.x.
pub(super) const DEFAULT_SUMMARY_STATS: &[&str] =
    &["count", "mean", "stddev", "min", "25%", "50%", "75%", "max"];

/// τ's resolved schema type alias.
pub type Schema = ResolvedSchema;

/// A typed plan node: an operator plus its resolved output schema and the
/// alias scope that output exposes.
#[derive(Debug, Clone)]
pub struct TypedAst {
    /// The typed operator this node represents.
    pub op: TypedOp,
    /// The schema of the relation produced by this node — every field has
    /// a resolved (non-`Unresolved`) [`DataType`] and a known `nullable` flag.
    pub resolved_schema: ResolvedSchema,
    /// The alias set this node's output exposes to enclosing clauses —
    /// stamped once at construction by [`TypedAst::new`] and consumed by
    /// both the analyzer's [`ResolveContext`] and emission's block builder.
    /// The single scope authority (INV2: facts are pushed into the node).
    pub scope: RelScope,
}

/// Equality deliberately ignores `scope`: it is derived data, fully
/// determined by `(op, resolved_schema)`, and existing analyzer tests
/// assert equality over the semantic pair only.
impl PartialEq for TypedAst {
    fn eq(&self, other: &Self) -> bool {
        self.op == other.op && self.resolved_schema == other.resolved_schema
    }
}

impl TypedAst {
    /// Build a typed node, stamping the [`RelScope`] its output exposes.
    ///
    /// Analysis is strictly bottom-up, so every child inside `op` is already
    /// stamped; the scope derivation is therefore shallow (reads children's
    /// `scope` fields, never re-walks subtrees).
    pub fn new(op: TypedOp, resolved_schema: ResolvedSchema) -> Self {
        let scope = RelScope::of(&op, &resolved_schema);
        Self {
            op,
            resolved_schema,
            scope,
        }
    }
}

/// The schema/scope-PASSTHROUGH operator class: position/count-preserving
/// unary operators through which alias bindings (bottom-up, [`RelScope::of`])
/// flow unchanged.
/// The single authority for that classification — the scope walk matches on
/// this pattern, so adding a variant here updates it in lockstep, and the
/// match is exhaustive so a NEW `TypedOp` variant is a compile error until
/// classified.
macro_rules! scope_passthrough {
    ($input:ident) => {
        TypedOp::Filter { input: $input, .. }
            | TypedOp::Sort { input: $input, .. }
            | TypedOp::Limit { input: $input, .. }
            | TypedOp::Sample { input: $input, .. }
            | TypedOp::SampleBy { input: $input, .. }
            | TypedOp::Deduplicate { input: $input, .. }
            | TypedOp::NaFill { input: $input, .. }
            | TypedOp::NaDrop { input: $input, .. }
            | TypedOp::NaReplace { input: $input, .. }
    };
}

/// The alias scope a relation's OUTPUT exposes: which qualifiers (table
/// names, user aliases, generator qualifiers) bind to which contiguous
/// field ranges of the node's `resolved_schema`, plus the plan_id →
/// join-side bindings used for DataFrame `plan_id` disambiguation.
///
/// Ranges are relative to THIS node's schema (base 0); consumers offset when
/// composing (a join's right side shifts by the left side's field count).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RelScope {
    /// `(qualifier, field-range)` bindings, in tree order.
    pub aliases: Vec<(String, std::ops::Range<usize>)>,
    /// `(plan_id, field-range)` bindings, outermost join first —
    /// [`RelScope::lookup_plan_id`] uses first match, so the nearest
    /// enclosing join's range wins. Plan-id references are bound bare and
    /// carry the resolved attribute identity for emission-side binding.
    pub(crate) plan_ids: Vec<(i64, std::ops::Range<usize>)>,
    /// plan_ids bound on BOTH the left AND right side of the SAME join —
    /// the un-realiased self-join `df.join(df, ...)` reusing the identical
    /// underlying relation on both sides without a fresh alias. Any
    /// reference to one of these plan_ids is genuinely ambiguous (Spark
    /// itself cannot tell which side is meant), so
    /// [`RelScope::plan_id_is_ambiguous`] is checked BEFORE
    /// [`RelScope::lookup_plan_id`]'s first-match. Distinct from a plan_id
    /// repeated at multiple NESTING levels on the SAME side (outermost-wins,
    /// see `plan_ids` above) — that case never lands here.
    ambiguous_plan_ids: Vec<i64>,
}

impl RelScope {
    /// ALL field ranges bound to `q`, case-insensitively, in tree order.
    /// Distinguishes 0 / 1 / 2+ matches — [`RelScope::lookup`] collapses 2+
    /// into `None` for the legacy name-only fallback; callers that must
    /// raise `AmbiguousColumn` on 2+ (tier e/f in `resolve_column`) use this
    /// instead.
    fn lookup_all(&self, q: &str) -> Vec<std::ops::Range<usize>> {
        self.aliases
            .iter()
            .filter(|(name, _)| eq_fold(name, q))
            .map(|(_, range)| range.clone())
            .collect()
    }

    /// The field range `q` binds to, iff EXACTLY ONE binding matches `q`
    /// case-insensitively. A duplicate name — e.g. a self-join `emp e1 JOIN
    /// emp e2` referenced by the bare table name `emp` — is ambiguous by
    /// construction; return `None` so the caller falls back to the legacy
    /// name-only resolution instead of picking an arbitrary side.
    fn lookup(&self, q: &str) -> Option<std::ops::Range<usize>> {
        let matches = self.lookup_all(q);
        match matches.len() {
            1 => Some(matches.into_iter().next().expect("len checked")),
            _ => None,
        }
    }

    /// The field range a plan_id maps to. When a plan_id appears in multiple
    /// ancestor joins (nested join trees), the OUTERMOST entry wins —
    /// [`RelScope::of`] pushes parent entries before child entries, so the
    /// first match wins at the parent operator's resolution point.
    fn lookup_plan_id(&self, pid: i64) -> Option<std::ops::Range<usize>> {
        self.plan_ids
            .iter()
            .find(|(id, _)| *id == pid)
            .map(|(_, range)| range.clone())
    }

    /// `true` iff `pid` is bound on both sides of the SAME join (see
    /// `ambiguous_plan_ids` doc) — checked BEFORE [`RelScope::lookup_plan_id`]
    /// so callers raise `AmbiguousColumn` instead of silently binding the
    /// left side.
    fn plan_id_is_ambiguous(&self, pid: i64) -> bool {
        self.ambiguous_plan_ids.contains(&pid)
    }

    /// Derive the scope a just-built operator exposes. Shallow: children are
    /// already stamped, so this reads their `scope` fields and offsets
    /// ranges — it never re-walks a subtree.
    ///
    /// Binding rules:
    /// - `TableScan{table}`: bind `table` to the full range.
    /// - `AliasedRelation{alias}`: bind `alias` to the full range; the child's
    ///   scope is dropped — emission re-scopes everything under it to `alias`.
    /// - `Join` with non-empty `using_columns`: EMPTY. USING output reorders
    ///   and dedups columns, so no contiguous-range invariant holds — USING
    ///   joins keep resolving via the legacy name-only path.
    /// - `Join{LeftSemi | LeftAnti}`: left side only — the right side
    ///   contributes no columns to the output schema.
    /// - Any other `Join`: own plan_id entries FIRST (outermost-wins), then
    ///   the left child's scope at base 0 and the right child's offset by
    ///   `left.resolved_schema.len()`.
    /// - Schema-verbatim passthroughs (`Filter` / `Sort` / `Limit` / `Sample`
    ///   / `SampleBy` / `Deduplicate` / `NaFill` / `NaDrop` / `NaReplace`):
    ///   the child's scope verbatim — these clone the input schema
    ///   field-for-field (position/count preserved), so the contiguous-range
    ///   invariant holds through them.
    /// - `Generate`: preserve the input scope and bind its optional qualifier
    ///   to the appended generated columns.
    /// - Everything else (`Project` / `Aggregate` / `SetOp` / `WithColumns` /
    ///   `Values` / `LocalRelation` / `TableFunction` / `Pivot` / ...):
    ///   EMPTY — these operators retype or reshuffle columns, so no alias
    ///   binding from further down is valid against the CURRENT schema.
    fn of(op: &TypedOp, resolved_schema: &ResolvedSchema) -> Self {
        match op {
            TypedOp::TableScan { table } => Self {
                aliases: vec![(table.clone(), 0..resolved_schema.len())],
                plan_ids: Vec::new(),
                ambiguous_plan_ids: Vec::new(),
            },
            TypedOp::AliasedRelation { alias, .. } => Self {
                aliases: vec![(alias.clone(), 0..resolved_schema.len())],
                plan_ids: Vec::new(),
                ambiguous_plan_ids: Vec::new(),
            },
            TypedOp::Join {
                using_columns,
                left,
                right,
                join_type,
                left_plan_ids,
                right_plan_ids,
                ..
            } => {
                // The USING gate stays HERE, not inside `merge_join_scopes`:
                // it is a property of the join's OUTPUT schema (reordered and
                // deduped, so no contiguous-range invariant holds), whereas a
                // USING join's *condition* still resolves against the plain
                // merged schema via `for_join_condition`.
                if !using_columns.is_empty() {
                    return Self::default();
                }
                let right_side = match join_type {
                    JoinType::LeftSemi | JoinType::LeftAnti => RightSide::Drop,
                    _ => RightSide::Keep,
                };
                merge_join_scopes(left, right, left_plan_ids, right_plan_ids, right_side)
            }
            scope_passthrough!(input) => input.scope.clone(),
            TypedOp::Generate {
                input,
                qualifier: Some(qualifier),
                generator,
            } => {
                let mut scope = input.scope.clone();
                let start = input.resolved_schema.len();
                scope
                    .aliases
                    .push((qualifier.clone(), start..start + generator.aliases.len()));
                scope
            }
            TypedOp::Generate {
                input,
                qualifier: None,
                ..
            } => input.scope.clone(),
            // Everything below retypes or reshuffles columns: no alias
            // binding from further down is valid against the CURRENT schema.
            // Deliberately exhaustive (no `_`) so a new TypedOp variant
            // forces an explicit scope classification here (the compiler
            // enforces the update).
            TypedOp::Project { .. }
            | TypedOp::Aggregate { .. }
            | TypedOp::SetOp { .. }
            | TypedOp::SingleRow
            | TypedOp::Values { .. }
            | TypedOp::LocalRelation { .. }
            | TypedOp::FileScan { .. }
            | TypedOp::TableFunction { .. }
            | TypedOp::WithColumns { .. }
            | TypedOp::DropColumns { .. }
            | TypedOp::WithColumnsRenamed { .. }
            | TypedOp::Describe { .. }
            | TypedOp::Summary { .. }
            | TypedOp::FreqItems { .. }
            | TypedOp::Unpivot { .. }
            | TypedOp::Pivot { .. }
            | TypedOp::RecursiveCte { .. } => Self::default(),
        }
    }
}

/// Whether a join's RIGHT side contributes columns to the merged range space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RightSide {
    /// Bind the right side's own plan_ids and offset-merge its child scope.
    Keep,
    /// `LeftSemi` / `LeftAnti` output: the right side contributes no columns,
    /// so nothing from it binds — and a plan_id on both sides is NOT
    /// ambiguous, since only the left occurrence is reachable.
    Drop,
}

/// Offset-merge two already-stamped child scopes into the single range space a
/// `left ⋈ right` schema exposes — the one home of the join
/// scope-composition rules, shared by [`RelScope::of`]'s Join arm and
/// [`ResolveContext::for_join_condition`].
///
/// Push ORDER is contractual, not incidental: `lookup` / `lookup_plan_id` take
/// the first match, so this join's OWN plan_id entries must precede the
/// children's (nearest enclosing join wins) and left must precede right.
fn merge_join_scopes(
    left: &TypedAst,
    right: &TypedAst,
    left_plan_ids: &[i64],
    right_plan_ids: &[i64],
    right_side: RightSide,
) -> RelScope {
    let keep_right = right_side == RightSide::Keep;
    let left_len = left.resolved_schema.len();
    let left_range = 0..left_len;
    let right_range = left_len..left_len + right.resolved_schema.len();
    let offset = |r: &std::ops::Range<usize>| r.start + left_len..r.end + left_len;

    // A plan_id present on both sides of an unaliased self-join is ambiguous.
    let own_ambiguous: Vec<i64> = if keep_right {
        left_plan_ids
            .iter()
            .filter(|pid| right_plan_ids.contains(pid))
            .copied()
            .collect()
    } else {
        Vec::new()
    };

    let mut plan_ids = Vec::new();
    for &pid in left_plan_ids {
        plan_ids.push((pid, left_range.clone()));
    }
    if keep_right {
        for &pid in right_plan_ids {
            plan_ids.push((pid, right_range.clone()));
        }
    }
    plan_ids.extend(left.scope.plan_ids.iter().cloned());
    let mut ambiguous_plan_ids = own_ambiguous;
    ambiguous_plan_ids.extend(left.scope.ambiguous_plan_ids.iter().copied());
    let mut aliases = left.scope.aliases.clone();
    if keep_right {
        plan_ids.extend(
            right
                .scope
                .plan_ids
                .iter()
                .map(|(pid, r)| (*pid, offset(r))),
        );
        ambiguous_plan_ids.extend(right.scope.ambiguous_plan_ids.iter().copied());
        aliases.extend(
            right
                .scope
                .aliases
                .iter()
                .map(|(name, r)| (name.clone(), offset(r))),
        );
    }
    RelScope {
        aliases,
        plan_ids,
        ambiguous_plan_ids,
    }
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
        /// The complete output list; grouping columns may be restated here.
        aggregates: Vec<Expression>,
        /// GROUP BY variant.
        grouping_kind: crate::transpiler_v2::ast::GroupingKind,
        /// Per-set column membership for [`GroupingKind::GroupingSets`] —
        /// indices into `grouping` (first-appearance order); empty inner vec =
        /// empty set `()`. EMPTY for all other kinds and on the DataFrame
        /// `groupingSets` path (which stays a boundary error, ADR-022).
        grouping_sets: Vec<Vec<usize>>,
        /// Resolved SparkSQL `HAVING <pred>` — see [`CommonOp::Aggregate`].
        /// `None` for the DataFrame path. Resolved against the aggregate
        /// INPUT schema.
        having: Option<Expression>,
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
        /// Whether this is a LATERAL join (correlated derived-table join).
        lateral: bool,
        /// Plan-ids appearing anywhere under the left side.
        left_plan_ids: Vec<i64>,
        /// Plan-ids appearing anywhere under the right side.
        right_plan_ids: Vec<i64>,
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
        widened_schema: ResolvedSchema,
    },
    /// A relation with exactly one row and zero columns.
    SingleRow,
    /// A named table scan. Aliases live on an enclosing `AliasedRelation`
    /// (see [`CommonOp::TableScan`]).
    TableScan {
        /// The table name.
        table: String,
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
    /// `df.withColumnsRenamed({old: new, ...})` and `df.toDF(...)`. The
    /// analyzer computes the renamed output schema at construction (walking
    /// input fields; misses silently ignored per Spark semantics), so the
    /// node carries NO rename list — `resolved_schema` IS the rename, and
    /// emission mirrors it positionally.
    WithColumnsRenamed {
        /// The input relation.
        input: Box<TypedAst>,
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
    /// Append a resolved generator's outputs to its input.
    Generate {
        input: Box<TypedAst>,
        generator: Generator,
        qualifier: Option<String>,
    },
    /// `WITH RECURSIVE name(cols) AS (anchor UNION ALL recursive_term)
    /// SELECT * FROM name` — post-analysis recursive CTE. The `union_all`
    /// field from the parser is dropped (the analyzer rejects `UNION`-without-
    /// ALL as a Spark-emulated error before constructing this variant).
    RecursiveCte {
        /// The CTE name.
        name: String,
        /// The typed anchor leg.
        anchor: Box<TypedAst>,
        /// The typed recursive leg.
        recursive_term: Box<TypedAst>,
    },
    /// `df.groupBy(...).pivot(col, [values]).agg(...)`. See
    /// [`CommonOp::Pivot`] for the semantic contract. The analyzer resolves
    /// grouping / pivot column / aggregates against the input schema and
    /// stamps the output schema per Spark: `<grouping>` + one output column
    /// per pivot value × aggregate. `pivot_values` is non-empty by
    /// construction: the connect-server resolves implicit pivots (eager
    /// DISTINCT value discovery against the live session) BEFORE `analyze`
    /// runs, and `analyze_pivot` rejects an empty list with a
    /// Thunderduck-boundary `PuntedOperator("Pivot[implicit-values]")`.
    Pivot {
        /// The input relation.
        input: Box<TypedAst>,
        /// Grouping columns (resolved).
        grouping: Vec<Expression>,
        /// The pivot column (resolved).
        pivot_column: Expression,
        /// Explicit pivot value literals (resolved). Non-empty by
        /// construction — see the variant doc above.
        pivot_values: Vec<Expression>,
        /// Aggregate expressions (resolved).
        aggregates: Vec<Expression>,
    },
}

/// Errors surfaced by the τ analyzer. They are classified per ADR-022:
///
/// - **Spark-emulated** — errors reference Spark would also raise. The client
///   sees the same error under Thunderduck as under Spark.
/// - **Thunderduck-boundary** — errors that signal Thunderduck's incomplete
///   implementation (a plan / rule not yet lowered).
///
/// Display prefixes identify the category and are stripped before the error
/// crosses the wire.
pub const SPARK_EMULATED_PREFIX: &str = "[SPARK-EMULATED] ";
/// Display prefix marking an [`AnalyzerError`] as a Thunderduck-boundary gap.
pub const TDCK_BOUNDARY_PREFIX: &str = "[TDCK-BOUNDARY] ";
/// Display prefix marking an [`AnalyzerError`] as a τ-internal invariant
/// violation — neither ADR-022 category.
pub const TDCK_INTERNAL_PREFIX: &str = "[TDCK-INTERNAL] ";

#[derive(thiserror::Error, Debug, Clone, PartialEq)]
pub enum AnalyzerError {
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

    /// A plan_id-tagged reference binds the SAME join-side plan_id on BOTH sides
    /// of one join (the un-realiased self-join `df.join(df, …)`). Spark cannot
    /// tell which side is meant. Distinct Spark class from `AmbiguousColumn`
    /// (bare-name ambiguity): AMBIGUOUS_COLUMN_REFERENCE (42702) vs
    /// AMBIGUOUS_REFERENCE (42704).
    #[error("[SPARK-EMULATED] column `{name}` is ambiguous — the same DataFrame is joined on both sides")]
    AmbiguousColumnReference {
        /// The ambiguous column name.
        name: String,
    },

    /// A later SELECT-list item references an earlier item's alias (Spark's
    /// Lateral Column Alias feature, pr-007) but 2+ earlier items in the same
    /// projection list share that alias name — Spark's own
    /// `AMBIGUOUS_LATERAL_COLUMN_ALIAS` error class.
    #[error("[SPARK-EMULATED] lateral column alias `{name}` is ambiguous and has {count} matches")]
    AmbiguousLateralColumnAlias {
        /// The ambiguous lateral column alias name.
        name: String,
        /// The number of earlier same-SELECT aliases sharing this name.
        count: usize,
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

    /// A Spark-emulated error whose exact Spark error-class token has been
    /// established from Spark 4.1.1 behavior.
    ///
    /// Prefer this over [`Self::Other`] whenever the class is known. `Other`
    /// means "Spark rejects this input too, but we have not established which
    /// class it raises" — it is not a licence to invent a token. Three sites
    /// previously carried a *prose* pseudo-class inside `Other`'s `reason`
    /// (e.g. `"UNSUPPORTED_FEATURE: LATERAL join with NATURAL join"`); the
    /// oracle's `^\s*\[([A-Z][A-Z0-9_.]*)\]` token regex cannot recover those,
    /// and two of the three prose tokens turned out to be wrong (the real
    /// classes are `INCOMPATIBLE_JOIN_TYPES` and the
    /// `UNSUPPORTED_FEATURE.LATERAL_JOIN_USING` *subclass*). Put the token
    /// here, never in the message.
    #[error("[SPARK-EMULATED] {reason}")]
    SparkEmulated {
        /// The exact Spark error-class token, e.g.
        /// `"UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE"`.
        class: &'static str,
        /// A description of the error, WITHOUT a leading class token.
        reason: String,
    },

    /// A catch-all Spark-emulated error not captured by the more specific
    /// variants above, and whose Spark error class is not established.
    #[error("[SPARK-EMULATED] {reason}")]
    Other {
        /// A description of the error.
        reason: String,
    },

    /// A τ-internal invariant violation raised from the analyzer. Bridges to
    /// [`EmissionError::Internal`] → `Status::internal`.
    ///
    /// This is neither category in ADR-022's pair: the client did nothing
    /// wrong (so it is not Spark-emulated) and τ is not missing a feature (so
    /// it is not a boundary gap) — τ broke its own promise. Reporting these as
    /// Spark-emulated would blame the user for a τ bug.
    ///
    /// [`EmissionError::Internal`]: super::error::EmissionError::Internal
    #[error("[TDCK-INTERNAL] {reason}")]
    Internal {
        /// Description of the violated invariant and where it fired.
        reason: String,
    },

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

impl AnalyzerError {
    /// The exact Spark error-class token this variant emulates, if any.
    /// `None` for `Other` (no specific class to surface)
    /// and for the Thunderduck-boundary variants (no Spark class applies —
    /// these are τ's own gaps, not a Spark-emulated error).
    ///
    /// Best-effort mappings (subclass not reproduced): `AmbiguousLateralColumnAlias`
    /// and `TypeMismatch` are base-class only. `UnknownColumn` → the
    /// `.WITH_SUGGESTION` subclass; Spark emits it whenever
    /// candidate columns exist in scope, which holds for every shape that
    /// reaches `UnknownColumn` here.
    pub fn spark_class(&self) -> Option<&'static str> {
        match self {
            Self::UnknownTable { .. } => Some("TABLE_OR_VIEW_NOT_FOUND"),
            Self::UnknownColumn { .. } => Some("UNRESOLVED_COLUMN.WITH_SUGGESTION"),
            Self::AmbiguousColumn { .. } => Some("AMBIGUOUS_REFERENCE"),
            Self::AmbiguousColumnReference { .. } => Some("AMBIGUOUS_COLUMN_REFERENCE"),
            Self::AmbiguousLateralColumnAlias { .. } => Some("AMBIGUOUS_LATERAL_COLUMN_ALIAS"),
            Self::TypeMismatch { .. } => Some("DATATYPE_MISMATCH"),
            Self::SparkEmulated { class, .. } => Some(class),
            Self::Other { .. }
            | Self::Internal { .. }
            | Self::PuntedOperator { .. }
            | Self::UnsupportedRule { .. } => None,
        }
    }

    /// Which ADR-022 category (plus τ-internal) this variant belongs to.
    ///
    /// This — **not** `spark_class().is_some()` — is the bridge's branch
    /// condition. A Spark-emulated error with no established class
    /// is still Spark-emulated, and keying on the class made every such error
    /// fall to the Thunderduck-boundary path and exit as gRPC UNIMPLEMENTED,
    /// telling clients "τ doesn't support this" about inputs Spark itself
    /// rejects. One exhaustive match, so a new variant cannot silently pick
    /// the wrong category.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::UnknownTable { .. }
            | Self::UnknownColumn { .. }
            | Self::AmbiguousColumn { .. }
            | Self::AmbiguousColumnReference { .. }
            | Self::AmbiguousLateralColumnAlias { .. }
            | Self::TypeMismatch { .. }
            | Self::SparkEmulated { .. }
            | Self::Other { .. } => ErrorCategory::SparkEmulated,
            Self::Internal { .. } => ErrorCategory::Internal,
            Self::PuntedOperator { .. } | Self::UnsupportedRule { .. } => {
                ErrorCategory::ThunderduckBoundary
            }
        }
    }
}

/// Which wire shape — and therefore which gRPC status — an [`AnalyzerError`]
/// takes. Used by [`analyzer_error_to_emission_error`].
///
/// Maps to ADR-022's categories 1 and 2, plus a τ-internal variant that is
/// **neither** ADR-022 category (the client did nothing wrong and τ is not
/// missing a feature — τ broke its own promise).
///
/// Note the numbering does not line up with the ADR: ADR-022's Amendment 1
/// added a *category 3* — "strict rejections", inputs Spark accepts that τ
/// deliberately rejects as malformed. That is a distinct concept from
/// [`Self::Internal`] and has **no variant here yet**, because no analyzer
/// error currently implements it (the sole register entry is rejected at the
/// parse stage, before this enum is reachable). Add a variant when the first
/// analyzer-stage strict rejection lands — do not overload `Internal` for it,
/// which would report a deliberate policy decision as a τ bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// ADR-022 category 1 — Spark itself would reject this input →
    /// `Status::invalid_argument`.
    SparkEmulated,
    /// ADR-022 category 2 — τ has not implemented this shape →
    /// `Status::unimplemented`.
    ThunderduckBoundary,
    /// Not an ADR-022 category — τ broke its own invariant →
    /// `Status::internal`.
    Internal,
}

/// Analyze a plan: resolve columns, assign types, derive nullability.
///
/// Returns a [`TypedAst`] whose every plan node carries a resolved
/// [`StructType`] and whose every `ColumnReference` carries populated
/// `data_type` and `nullable` fields.
pub fn analyze(ast: CommonAst, base_types: &BaseTypes) -> Result<TypedAst, AnalyzerError> {
    // The three logical passes (resolve → assign_types → derive_nullability)
    // are fused into a single bottom-up traversal for efficiency. Section
    // comments below mark where each conceptual pass runs.
    analyze_node(ast, base_types, None)
}

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
        TypedOp::WithColumns { input, assignments } => {
            has_resolved_schema(input)
                && assignments
                    .iter()
                    .all(|(_, e)| expression_is_fully_resolved(e))
        }
        TypedOp::Limit { input, .. }
        | TypedOp::DropColumns { input, .. }
        | TypedOp::AliasedRelation { input, .. }
        | TypedOp::WithColumnsRenamed { input, .. }
        | TypedOp::Deduplicate { input, .. }
        | TypedOp::NaFill { input, .. }
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
        TypedOp::RecursiveCte {
            anchor,
            recursive_term,
            ..
        } => has_resolved_schema(anchor) && has_resolved_schema(recursive_term),
        TypedOp::Generate {
            input, generator, ..
        } => has_resolved_schema(input) && generator.args.iter().all(expression_is_fully_resolved),
        TypedOp::SingleRow | TypedOp::TableScan { .. } | TypedOp::FileScan { .. } => true,
    }
}

/// Bridge an [`AnalyzerError`] into an [`EmissionError`] preserving the
/// ADR-022 classification.
///
/// Branches on [`AnalyzerError::category`], **not** on whether a Spark class
/// is known. Spark-emulated errors become
/// [`EmissionError::SparkEmulated`] — carrying the class token when one has
/// been established, and `None` otherwise, which renders a clean prefix-free
/// message rather than a fabricated token. Either way they exit as
/// `Status::invalid_argument`, because Spark rejects these inputs too.
/// Thunderduck-boundary variants stay `Unsupported` → `UNIMPLEMENTED`, and
/// τ-internal ones become [`EmissionError::Internal`] → `INTERNAL`.
///
/// Called by `transpiler_v2::generate()`.
pub(super) fn analyzer_error_to_emission_error(e: AnalyzerError) -> EmissionError {
    match e.category() {
        ErrorCategory::SparkEmulated => {
            let class = e.spark_class();
            let full = e.to_string();
            let message = full
                .strip_prefix(SPARK_EMULATED_PREFIX)
                .unwrap_or(&full)
                .to_owned();
            EmissionError::SparkEmulated { class, message }
        }
        ErrorCategory::Internal => {
            let full = e.to_string();
            let message = full
                .strip_prefix(TDCK_INTERNAL_PREFIX)
                .unwrap_or(&full)
                .to_owned();
            EmissionError::Internal { message }
        }
        ErrorCategory::ThunderduckBoundary => match e {
            AnalyzerError::PuntedOperator { op, reason } => EmissionError::Unsupported {
                kind: UnsupportedKind::Op,
                name: op,
                reason,
            },
            AnalyzerError::UnsupportedRule { rule, reason } => EmissionError::Unsupported {
                kind: UnsupportedKind::Expression,
                name: rule,
                reason,
            },
            other => unreachable!(
                "category() returns ThunderduckBoundary only for PuntedOperator / \
                 UnsupportedRule, got {other:?}"
            ),
        },
    }
}

/// Analyze `input`, clone its resolved schema, and wrap it in a caller-built
/// [`TypedOp`] variant. The output schema is a straight passthrough — used by
/// operators that neither add, drop, nor retype columns (Filter, Sort, Limit,
/// Deduplicate, Sample, SampleBy, NaDrop, NaReplace). `AliasedRelation`
/// resets lineage to its own alias and is built separately.
///
/// The `build_op` closure receives the typed input by value so it can move it
/// into a `Box`; it returns a `Result` so callers may perform failable
/// resolution (`resolve_and_stamp`, etc.) inside the closure.
fn passthrough_schema_arm(
    input: CommonAst,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
    build_op: impl FnOnce(TypedAst) -> Result<TypedOp, AnalyzerError>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    let resolved_schema = typed_input.resolved_schema.clone();
    let op = build_op(typed_input)?;
    Ok(TypedAst::new(op, resolved_schema))
}

/// Seed every output attribute with the same source-qualifier set.
fn seed_source_quals(schema: ResolvedSchema, quals: BTreeSet<String>) -> ResolvedSchema {
    ResolvedSchema::new(
        schema
            .fields
            .into_iter()
            .map(|f| f.with_quals(quals.clone()))
            .collect(),
    )
}

fn analyze_node(
    ast: CommonAst,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    match ast.op {
        CommonOp::SingleRow => Ok(TypedAst::new(TypedOp::SingleRow, ResolvedSchema::empty())),

        CommonOp::TableScan { table } => {
            // resolve: seed schema from base_types.
            let schema =
                base_types
                    .lookup(&table)
                    .cloned()
                    .ok_or_else(|| AnalyzerError::UnknownTable {
                        name: table.clone(),
                    })?;
            // A table scan mints fresh ids and seeds every column with the
            // table qualifier. An enclosing alias replaces that lineage.
            let mut quals = BTreeSet::new();
            quals.insert(table.clone());
            Ok(TypedAst::new(
                TypedOp::TableScan { table },
                seed_source_quals(ResolvedSchema::minted(schema), quals),
            ))
        }

        CommonOp::Values { rows, column_names } => {
            // `infer_values_schema` stays `StructType`-returning (no prior
            // identity to carry); mint fresh ids for this origination point.
            let schema = ResolvedSchema::minted(infer_values_schema(&rows, &column_names)?);
            let ctx = ResolveContext::bare(&schema, base_types);
            let typed_rows = rows
                .into_iter()
                .map(|row| resolve_expr_list(row, &ctx))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TypedAst::new(
                TypedOp::Values {
                    rows: typed_rows,
                    column_names,
                },
                schema,
            ))
        }

        CommonOp::LocalRelation { schema, rows } => Ok(TypedAst::new(
            TypedOp::LocalRelation {
                schema: schema.clone(),
                rows,
            },
            // The op's own `schema` field stays a plain `StructType`
            // (declared-shape value, not an identity carrier); the node's
            // resolved schema mints fresh ids at this origination point.
            ResolvedSchema::minted(schema),
        )),

        CommonOp::FileScan {
            format,
            paths,
            schema,
            options,
        } => match schema {
            Some(s) => Ok(TypedAst::new(
                TypedOp::FileScan {
                    format,
                    paths,
                    schema: s.clone(),
                    options,
                },
                // Same shape as `LocalRelation` above: op keeps its
                // `StructType`, node schema mints fresh ids.
                ResolvedSchema::minted(s),
            )),
            None => Err(AnalyzerError::PuntedOperator {
                op: "FileScan".to_owned(),
                reason: "schema-less FileScan (parquet inference) (not implemented in τ)"
                    .to_owned(),
            }),
        },

        CommonOp::TableFunction {
            name,
            args,
            with_ordinality,
        } => analyze_table_function(name, args, with_ordinality, base_types),

        CommonOp::Unnest {
            expr: _,
            with_ordinality: _,
        } => Err(AnalyzerError::PuntedOperator {
            op: "Unnest".to_owned(),
            reason: "unnest analysis (not implemented in τ)".to_owned(),
        }),

        CommonOp::Project { input, projections } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
            // Expand regex projections before resolution.
            let projections = expand_regex_projections(projections, &typed_input.resolved_schema)?;
            if projections.iter().any(contains_generator) {
                return analyze_generator_project(typed_input, projections, base_types, outer);
            }
            // Inline earlier SELECT-list aliases for Spark's lateral-column
            // alias behavior before ordinary resolution.
            let projections =
                expand_lateral_column_aliases(projections, &typed_input.resolved_schema)?;
            let ctx = ResolveContext::of_input(&typed_input, base_types, outer);
            // Give each computed output a name, matching Spark's
            // `UnresolvedAlias` → `Alias` resolution.
            let projections = projections
                .into_iter()
                .map(|e| resolve_and_stamp(e, &ctx).map(ensure_named))
                .collect::<Result<Vec<_>, _>>()?;
            // Compute output schema — expand Star; take alias name if present.
            let output_schema = project_output_schema(&projections, &typed_input)?;
            Ok(TypedAst::new(
                TypedOp::Project {
                    input: Box::new(typed_input),
                    projections,
                },
                output_schema,
            ))
        }

        CommonOp::Filter { input, condition } => {
            passthrough_schema_arm(*input, base_types, outer, |ti| {
                let ctx = ResolveContext::of_input(&ti, base_types, outer);
                let condition = resolve_boolean_predicate(condition, &ctx, "filter-condition")?;
                Ok(TypedOp::Filter {
                    input: Box::new(ti),
                    condition,
                })
            })
        }

        CommonOp::Sort {
            input,
            order,
            limit,
            offset,
        } => {
            let mut typed_input = analyze_node(*input, base_types, outer)?;
            let original_schema = typed_input.resolved_schema.clone();
            let order = analyze_sort(order, &mut typed_input, base_types, outer)?;
            let extended_schema = typed_input.resolved_schema.clone();
            let sort = TypedAst::new(
                TypedOp::Sort {
                    input: Box::new(typed_input),
                    order,
                    limit,
                    offset,
                },
                extended_schema.clone(),
            );
            if extended_schema.len() == original_schema.len() {
                Ok(sort)
            } else {
                // Hidden keys are promoted into this derived Sort and trimmed
                // here. ORDER BY and LIMIT/OFFSET must stay inside: hoisting
                // would rebind duplicate names (ord-014). Preserving row order
                // through the trim is a DuckDB execution property, and the
                // differential harness sorts rows; the q078/ord-014 emission
                // shape tests are therefore the regression guard.
                let trim_projections: Vec<Expression> = original_schema
                    .fields
                    .iter()
                    .map(|f| Expression::ColumnReference(ColumnReference::from_attr(f)))
                    .collect();
                Ok(TypedAst::new(
                    TypedOp::Project {
                        input: Box::new(sort),
                        projections: trim_projections,
                    },
                    original_schema,
                ))
            }
        }

        CommonOp::Limit {
            input,
            limit,
            offset,
        } => passthrough_schema_arm(*input, base_types, outer, |ti| {
            Ok(TypedOp::Limit {
                input: Box::new(ti),
                limit,
                offset,
            })
        }),

        CommonOp::Aggregate {
            input,
            grouping,
            aggregates,
            grouping_kind,
            grouping_sets,
            having,
        } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
            let ctx = ResolveContext::of_input(&typed_input, base_types, outer);
            let grouping = resolve_expr_list(grouping, &ctx)?;
            // Wrap computed output entries as named aliases.
            // `grouping` is NOT wrapped — it is an internal GROUP BY key
            // list, not an output list (its restated copy inside
            // `aggregates`, if any, gets wrapped there instead).
            let aggregates: Vec<Expression> = resolve_expr_list(aggregates, &ctx)?
                .into_iter()
                .map(ensure_named)
                .collect();
            // HAVING resolves against the aggregate INPUT schema (aggregate
            // exprs + grouping keys bind to input columns), with the same
            // boolean-type guard as Filter.
            let having = having
                .map(|h| resolve_boolean_predicate(h, &ctx, "having-condition"))
                .transpose()?;
            // The aggregate list is the complete output list.
            let mut output_fields: Vec<Attribute> = aggregates
                .iter()
                .map(|e| output_attribute(e, &typed_input.resolved_schema))
                .collect();
            // Spark's Expand node (ROLLUP / CUBE / GROUPING SETS) inserts
            // NULL into every grouping-column position for super-aggregate
            // rows, so those columns are unconditionally nullable in the
            // output schema — regardless of source nullability.  Plain
            // GROUP BY preserves source nullability.  Precedent:
            // `flip_all_nullable` for outer-join padding.
            let forces_nullable = matches!(
                grouping_kind,
                crate::transpiler_v2::ast::GroupingKind::Rollup
                    | crate::transpiler_v2::ast::GroupingKind::Cube
                    | crate::transpiler_v2::ast::GroupingKind::GroupingSets
            );
            if forces_nullable && !grouping.is_empty() {
                let grouping_names: Vec<String> =
                    grouping.iter().map(expression_output_name).collect();
                for field in &mut output_fields {
                    if grouping_names.iter().any(|gn| eq_fold(gn, &field.name)) {
                        field.nullable = true;
                    }
                }
            }
            let output_schema = ResolvedSchema::new(output_fields);
            Ok(TypedAst::new(
                TypedOp::Aggregate {
                    input: Box::new(typed_input),
                    grouping,
                    aggregates,
                    grouping_kind,
                    grouping_sets,
                    having,
                },
                output_schema,
            ))
        }

        CommonOp::WithColumns { input, assignments } => {
            analyze_with_columns(*input, assignments, base_types, outer)
        }

        CommonOp::NaFill {
            input,
            cols,
            values,
        } => analyze_na_fill(*input, cols, values, base_types, outer),
        CommonOp::NaDrop {
            input,
            cols,
            min_non_nulls,
        } => passthrough_schema_arm(*input, base_types, outer, |ti| {
            Ok(TypedOp::NaDrop {
                input: Box::new(ti),
                cols,
                min_non_nulls,
            })
        }),
        CommonOp::NaReplace {
            input,
            cols,
            replacements,
        } => passthrough_schema_arm(*input, base_types, outer, |ti| {
            Ok(TypedOp::NaReplace {
                input: Box::new(ti),
                cols,
                replacements,
            })
        }),

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
            outer,
        ),

        CommonOp::Describe { input, cols } => analyze_describe(*input, cols, base_types, outer),

        CommonOp::Summary { input, statistics } => {
            analyze_summary(*input, statistics, base_types, outer)
        }

        CommonOp::FreqItems {
            input,
            cols,
            support,
        } => analyze_freq_items(*input, cols, support, base_types, outer),

        // Output columns are DISTINCT(col2) — unknowable at plan time.
        // Mirror-image of Pivot[implicit-values]: same session-hook blocker
        //. Reject loudly rather than stamp a partial schema.
        CommonOp::Crosstab { .. } => Err(AnalyzerError::PuntedOperator {
            op: "Crosstab[dynamic-values]".to_owned(),
            reason: "requires session-injected DISTINCT-query hook".to_owned(),
        }),

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
            outer,
        ),

        CommonOp::Deduplicate { input, on_columns } => {
            passthrough_schema_arm(*input, base_types, outer, |ti| {
                Ok(TypedOp::Deduplicate {
                    input: Box::new(ti),
                    on_columns,
                })
            })
        }

        CommonOp::Sample {
            input,
            lower_bound,
            upper_bound,
            with_replacement,
            seed,
        } => passthrough_schema_arm(*input, base_types, outer, |ti| {
            Ok(TypedOp::Sample {
                input: Box::new(ti),
                lower_bound,
                upper_bound,
                with_replacement,
                seed,
            })
        }),

        CommonOp::SampleBy {
            input,
            col,
            fractions,
            seed,
        } => passthrough_schema_arm(*input, base_types, outer, |ti| {
            let col = resolve_and_stamp(col, &ResolveContext::of_input(&ti, base_types, outer))?;
            Ok(TypedOp::SampleBy {
                input: Box::new(ti),
                col,
                fractions,
                seed,
            })
        }),

        CommonOp::ToDf {
            input,
            column_names,
        } => analyze_to_df(*input, column_names, base_types, outer),

        CommonOp::AliasedRelation { input, alias } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
            // An alias is a fresh lineage origin: every output column is
            // attributable to the alias alone.
            let mut quals = BTreeSet::new();
            quals.insert(alias.clone());
            let resolved_schema = seed_source_quals(typed_input.resolved_schema.clone(), quals);
            Ok(TypedAst::new(
                TypedOp::AliasedRelation {
                    input: Box::new(typed_input),
                    alias,
                },
                resolved_schema,
            ))
        }

        CommonOp::WithColumnsRenamed { input, renames } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
            let rename_map: HashMap<String, String> = renames
                .iter()
                .map(|(old, new)| (fold_key(old), new.clone()))
                .collect();
            let mut output_fields: Vec<Attribute> =
                Vec::with_capacity(typed_input.resolved_schema.fields.len());
            for f in &typed_input.resolved_schema.fields {
                let new_name = rename_map.get(&fold_key(&f.name)).cloned();
                // Rename is a pure name mutation on the SAME logical column —
                // clone-with-new-name keeps the id.
                let mut nf = f.clone();
                if let Some(n) = new_name {
                    nf.name = n;
                    // A rename clears only the renamed slot's inherited
                    // lineage; its identity remains unchanged.
                    nf.source_quals.clear();
                }
                output_fields.push(nf);
            }
            let output_schema = ResolvedSchema::new(output_fields);
            Ok(TypedAst::new(
                TypedOp::WithColumnsRenamed {
                    input: Box::new(typed_input),
                },
                output_schema,
            ))
        }

        CommonOp::DropColumns { input, drop_names } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
            let drop_lower: HashSet<String> = drop_names.iter().map(|s| fold_key(s)).collect();
            let mut output_fields: Vec<Attribute> =
                Vec::with_capacity(typed_input.resolved_schema.fields.len());
            for f in &typed_input.resolved_schema.fields {
                if !drop_lower.contains(&fold_key(&f.name)) {
                    // Filter keeps the surviving columns' ids — clone-filter.
                    output_fields.push(f.clone());
                }
            }
            let output_schema = ResolvedSchema::new(output_fields);
            Ok(TypedAst::new(
                TypedOp::DropColumns {
                    input: Box::new(typed_input),
                    drop_names,
                },
                output_schema,
            ))
        }

        CommonOp::Generate {
            input,
            generator,
            qualifier,
        } => analyze_generate(*input, generator, qualifier, base_types, outer),

        CommonOp::RecursiveCte {
            name,
            column_names,
            union_all,
            anchor,
            recursive_term,
        } => analyze_recursive_cte(
            name,
            column_names,
            union_all,
            *anchor,
            *recursive_term,
            base_types,
            outer,
        ),

        CommonOp::Join {
            left,
            right,
            join_type,
            condition,
            using_columns,
            natural,
            lateral,
            left_plan_ids,
            right_plan_ids,
        } => analyze_join(
            *left,
            *right,
            join_type,
            condition,
            using_columns,
            natural,
            lateral,
            left_plan_ids,
            right_plan_ids,
            base_types,
            outer,
        ),

        CommonOp::SetOp {
            kind,
            all,
            by_name,
            allow_missing_columns,
            children,
        } => analyze_set_op(
            kind,
            all,
            by_name,
            allow_missing_columns,
            children,
            base_types,
            outer,
        ),
    }
}

fn analyze_generate(
    input: CommonAst,
    generator: Generator,
    qualifier: Option<String>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    finish_generate(typed_input, generator, qualifier, base_types, outer)
}

enum ClassifiedGeneratorProjection {
    Expression(Expression),
    Generator(Generator),
}

fn analyze_generator_project(
    typed_input: TypedAst,
    projections: Vec<Expression>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let mut generator = None;
    let mut items = Vec::with_capacity(projections.len());
    for projection in projections {
        match classify_generator_projection(projection)? {
            ClassifiedGeneratorProjection::Generator(found) => {
                if generator.replace(found).is_some() {
                    return Err(AnalyzerError::SparkEmulated {
                        class: "UNSUPPORTED_GENERATOR.MULTI_GENERATOR",
                        reason: "only one generator is allowed per SELECT clause".to_owned(),
                    });
                }
                items.push(None);
            }
            ClassifiedGeneratorProjection::Expression(expression) => {
                if contains_generator(&expression) {
                    return Err(AnalyzerError::SparkEmulated {
                        class: "UNSUPPORTED_GENERATOR.NESTED_IN_EXPRESSIONS",
                        reason: "generator functions may only appear at the top level of SELECT"
                            .to_owned(),
                    });
                }
                items.push(Some(expression));
            }
        }
    }
    let generator = generator.ok_or_else(|| AnalyzerError::Internal {
        reason: "generator projection was detected but not extracted".to_owned(),
    })?;

    let input_len = typed_input.resolved_schema.len();
    let typed_generate = finish_generate(typed_input, generator, None, base_types, outer)?;
    let generated = &typed_generate.resolved_schema.fields[input_len..];
    let mut expanded = Vec::with_capacity(items.len() + generated.len().saturating_sub(1));
    for item in items {
        match item {
            Some(expression) => expanded.push(expression),
            None => expanded.extend(
                generated
                    .iter()
                    .map(|attr| Expression::ColumnReference(ColumnReference::from_attr(attr))),
            ),
        }
    }
    let expanded = expand_lateral_column_aliases(expanded, &typed_generate.resolved_schema)?;
    let ctx = ResolveContext::of_input(&typed_generate, base_types, outer);
    let projections = expanded
        .into_iter()
        .map(|expression| resolve_and_stamp(expression, &ctx).map(ensure_named))
        .collect::<Result<Vec<_>, _>>()?;
    let output_schema = project_output_schema(&projections, &typed_generate)?;
    Ok(TypedAst::new(
        TypedOp::Project {
            input: Box::new(typed_generate),
            projections,
        },
        output_schema,
    ))
}

fn classify_generator_projection(
    expression: Expression,
) -> Result<ClassifiedGeneratorProjection, AnalyzerError> {
    match expression {
        Expression::Generator(generator) => Ok(ClassifiedGeneratorProjection::Generator(generator)),
        Expression::Alias(alias) => match *alias.expr {
            Expression::Generator(mut generator) => {
                if generator.aliases.is_empty() {
                    generator.aliases.push(alias.alias);
                    Ok(ClassifiedGeneratorProjection::Generator(generator))
                } else {
                    Err(AnalyzerError::Internal {
                        reason: "generator carried both an Alias wrapper and explicit aliases"
                            .to_owned(),
                    })
                }
            }
            inner => Ok(ClassifiedGeneratorProjection::Expression(
                Expression::Alias(AliasExpression {
                    expr: Box::new(inner),
                    alias: alias.alias,
                }),
            )),
        },
        other => Ok(ClassifiedGeneratorProjection::Expression(other)),
    }
}

fn contains_generator(expression: &Expression) -> bool {
    matches!(expression, Expression::Generator(_)) || expression.children().any(contains_generator)
}

fn finish_generate(
    typed_input: TypedAst,
    mut generator: Generator,
    qualifier: Option<String>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let ctx = ResolveContext::of_input(&typed_input, base_types, outer);
    generator.args = generator
        .args
        .into_iter()
        .map(|arg| resolve_and_stamp(arg, &ctx))
        .collect::<Result<Vec<_>, _>>()?;
    let generated_schema = generator_output(&mut generator, &typed_input.resolved_schema)?;
    let resolved_schema = ResolvedSchema::merge(&typed_input.resolved_schema, &generated_schema);
    Ok(TypedAst::new(
        TypedOp::Generate {
            input: Box::new(typed_input),
            generator,
            qualifier,
        },
        resolved_schema,
    ))
}

fn generator_output(
    generator: &mut Generator,
    input: &ResolvedSchema,
) -> Result<ResolvedSchema, AnalyzerError> {
    let mut fields = match generator.kind {
        GeneratorKind::Explode | GeneratorKind::PosExplode => {
            expect_generator_arity(generator, 1)?;
            let positioned = generator.kind == GeneratorKind::PosExplode;
            match generator.args[0].data_type(input) {
                DataType::Array(element, contains_null) => {
                    let mut fields = Vec::with_capacity(if positioned { 2 } else { 1 });
                    if positioned {
                        fields.push(StructField::not_null("pos", DataType::Integer));
                    }
                    fields.push(StructField::new("col", *element, contains_null));
                    fields
                }
                DataType::Map {
                    key,
                    value,
                    value_nullable,
                } => {
                    let mut fields = Vec::with_capacity(if positioned { 3 } else { 2 });
                    if positioned {
                        fields.push(StructField::not_null("pos", DataType::Integer));
                    }
                    fields.push(StructField::not_null("key", *key));
                    fields.push(StructField::new("value", *value, value_nullable));
                    fields
                }
                actual => return Err(generator_type_error(generator, actual, "array or map")),
            }
        }
        GeneratorKind::Inline => {
            expect_generator_arity(generator, 1)?;
            match generator.args[0].data_type(input) {
                DataType::Array(element, contains_null) => match *element {
                    DataType::Struct(st) => st
                        .fields
                        .into_iter()
                        .map(|mut field| {
                            field.nullable |= contains_null;
                            field
                        })
                        .collect(),
                    actual => {
                        return Err(generator_type_error(
                            generator,
                            DataType::Array(Box::new(actual), contains_null),
                            "array<struct>",
                        ));
                    }
                },
                actual => return Err(generator_type_error(generator, actual, "array<struct>")),
            }
        }
        GeneratorKind::JsonTuple => {
            if generator.args.len() < 2 {
                return Err(AnalyzerError::SparkEmulated {
                    class: "WRONG_NUM_ARGS.WITHOUT_SUGGESTION",
                    reason: format!(
                        "`json_tuple` requires at least 2 arguments, got {}",
                        generator.args.len()
                    ),
                });
            }
            if let Some(actual) = generator
                .args
                .iter()
                .map(|arg| arg.data_type(input))
                .find(|ty| !matches!(ty, DataType::String | DataType::Null))
            {
                return Err(generator_type_error(generator, actual, "string"));
            }
            (0..generator.args.len() - 1)
                .map(|i| StructField::nullable(format!("c{i}"), DataType::String))
                .collect()
        }
        GeneratorKind::Stack => stack_output(generator, input)?,
    };

    if generator.aliases.is_empty() {
        generator.aliases = fields.iter().map(|field| field.name.clone()).collect();
    } else if generator.aliases.len() != fields.len() {
        return Err(AnalyzerError::SparkEmulated {
            class: "UDTF_ALIAS_NUMBER_MISMATCH",
            reason: format!(
                "`{}` produces {} columns but received {} aliases",
                generator.name(),
                fields.len(),
                generator.aliases.len()
            ),
        });
    }
    for (field, alias) in fields.iter_mut().zip(&generator.aliases) {
        field.name = alias.clone();
        field.nullable |= generator.outer;
    }
    Ok(ResolvedSchema::minted(StructType::new(fields)))
}

fn stack_output(
    generator: &Generator,
    input: &ResolvedSchema,
) -> Result<Vec<StructField>, AnalyzerError> {
    if generator.args.len() < 2 {
        return Err(AnalyzerError::SparkEmulated {
            class: "WRONG_NUM_ARGS.WITHOUT_SUGGESTION",
            reason: "`stack` requires a row count and at least one value".to_owned(),
        });
    }
    let rows = match &generator.args[0] {
        Expression::Literal(Literal {
            value: LiteralValue::Int(n),
            ..
        }) if *n > 0 => *n as usize,
        Expression::Literal(Literal {
            value: LiteralValue::Long(n),
            ..
        }) if *n > 0 => *n as usize,
        other => {
            return Err(AnalyzerError::SparkEmulated {
                class: "DATATYPE_MISMATCH.VALUE_OUT_OF_RANGE",
                reason: format!("`stack` row count must be a positive integer literal: {other:?}"),
            });
        }
    };
    let values = &generator.args[1..];
    let columns = values.len().div_ceil(rows);
    let mut output = Vec::with_capacity(columns);
    for column in 0..columns {
        let column_types: Vec<DataType> = values
            .iter()
            .skip(column)
            .step_by(columns)
            .map(|expr| expr.data_type(input))
            .collect();
        let data_type = column_types
            .iter()
            .find(|ty| **ty != DataType::Null)
            .cloned()
            .unwrap_or(DataType::Null);
        if let Some(actual) = column_types
            .into_iter()
            .find(|ty| *ty != DataType::Null && *ty != data_type)
        {
            return Err(AnalyzerError::SparkEmulated {
                class: "DATATYPE_MISMATCH.STACK_COLUMN_DIFF_TYPES",
                reason: format!("`stack` column {column} mixes `{data_type}` and `{actual}`"),
            });
        }
        output.push(StructField::nullable(format!("col{column}"), data_type));
    }
    Ok(output)
}

fn expect_generator_arity(generator: &Generator, expected: usize) -> Result<(), AnalyzerError> {
    if generator.args.len() == expected {
        Ok(())
    } else {
        Err(AnalyzerError::SparkEmulated {
            class: "WRONG_NUM_ARGS.WITHOUT_SUGGESTION",
            reason: format!(
                "`{}` requires exactly {expected} argument(s), got {}",
                generator.name(),
                generator.args.len()
            ),
        })
    }
}

fn generator_type_error(generator: &Generator, actual: DataType, expected: &str) -> AnalyzerError {
    AnalyzerError::SparkEmulated {
        class: "DATATYPE_MISMATCH.UNEXPECTED_INPUT_TYPE",
        reason: format!(
            "`{}` requires {expected} input, got `{actual}`",
            generator.name()
        ),
    }
}

fn analyze_with_columns(
    input: CommonAst,
    assignments: Vec<(String, Expression)>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    let input_schema = &typed_input.resolved_schema;
    let ctx = ResolveContext::of_input(&typed_input, base_types, outer);
    // Resolve each assignment expression against the INPUT schema —
    // Spark semantics: later assignments see the input value, not
    // intermediate replacements.
    let mut resolved_assignments: Vec<(String, Expression)> = Vec::with_capacity(assignments.len());
    for (name, expr) in assignments {
        let resolved = resolve_and_stamp(expr, &ctx)?;
        resolved_assignments.push((name, resolved));
    }
    // Output schema: the slot alignment is single-homed in
    // [`with_columns_plan`] (shared with emission's `render_with_columns`) —
    // matched input fields take the assignment's resolved (type, nullable)
    // in place; net-new assignments append in assignment order.
    let plan = with_columns_plan(input_schema, &resolved_assignments);
    let mut output_fields: Vec<Attribute> =
        Vec::with_capacity(input_schema.fields.len() + resolved_assignments.len());
    for (f, replaced_by) in input_schema.fields.iter().zip(&plan.replaced) {
        if let Some(idx) = replaced_by {
            // Replaced: this slot's VALUE is a new expression — mint. (Same
            // name/position, but not the same logical column any more.)
            let (_, expr) = &resolved_assignments[*idx];
            let dt = expr.data_type(input_schema);
            let nullable = expr.nullable(input_schema);
            // Preserve the input field's original casing for the name.
            output_fields.push(Attribute::minted(f.name.clone(), dt, nullable));
        } else {
            // Untouched: same logical column — clone keeps the id.
            output_fields.push(f.clone());
        }
    }
    for &i in &plan.appended {
        // Net-new trailing column — mint.
        let (name, expr) = &resolved_assignments[i];
        let dt = expr.data_type(input_schema);
        let nullable = expr.nullable(input_schema);
        output_fields.push(Attribute::minted(name.clone(), dt, nullable));
    }
    let output_schema = ResolvedSchema::new(output_fields);
    Ok(TypedAst::new(
        TypedOp::WithColumns {
            input: Box::new(typed_input),
            assignments: resolved_assignments,
        },
        output_schema,
    ))
}

/// Slot alignment for `withColumns`: which assignment (if any) replaces each
/// input field, and which assignments append as net-new trailing columns.
///
/// Single source of truth for the column-order contract between
/// [`analyze_with_columns`] (resolved-schema construction) and emission's
/// `render_with_columns` (SELECT-slot construction): input fields keep their
/// original positions (replaced in place when named by an assignment,
/// case-insensitively; callers preserve the input field's casing), and
/// assignments that match no input field append at the end in assignment
/// order. Any drift between the two walks would misalign Arrow columns with
/// the schema Spark Connect advertises via `analyze_plan`, corrupting
/// downstream decoding.
pub(super) struct WithColumnsPlan {
    /// Per input field (by position): `Some(i)` when assignment `i` replaces
    /// the field, `None` when the field passes through unchanged.
    pub(super) replaced: Vec<Option<usize>>,
    /// Indices of assignments that matched no input field, in assignment
    /// order.
    pub(super) appended: Vec<usize>,
}

/// Compute the [`WithColumnsPlan`] for `assignments` over `input_schema`.
/// Duplicate assignment names: the last assignment with a given
/// (lowercased) name wins the in-place replacement; earlier duplicates
/// append as trailing columns (map insertion overwrites — long-standing
/// behavior, preserved verbatim).
pub(super) fn with_columns_plan(
    input_schema: &ResolvedSchema,
    assignments: &[(String, Expression)],
) -> WithColumnsPlan {
    let mut assigned_lower: HashMap<String, usize> = HashMap::with_capacity(assignments.len());
    for (i, (name, _)) in assignments.iter().enumerate() {
        assigned_lower.insert(fold_key(name), i);
    }
    let mut consumed = vec![false; assignments.len()];
    let mut replaced: Vec<Option<usize>> = Vec::with_capacity(input_schema.fields.len());
    for f in &input_schema.fields {
        let idx = assigned_lower.get(&fold_key(&f.name)).copied();
        if let Some(i) = idx {
            consumed[i] = true;
        }
        replaced.push(idx);
    }
    let appended = (0..assignments.len()).filter(|&i| !consumed[i]).collect();
    WithColumnsPlan { replaced, appended }
}

/// Spark's `DataFrameNaFunctions.fillValue` type-compatibility rule.
///
/// When the client omits `subset` on `.na.fill(v)`, Spark silently applies the
/// fill only to columns whose static type is compatible with the fill value's
/// type — numeric-with-numeric, string-with-string, or boolean-with-boolean.
/// All other columns pass through untouched (no COALESCE, nullability
/// preserved). Both `analyze_na_fill` (nullability inference) and
/// `render_na_fill` (SQL emission) MUST honour this predicate to keep the
/// stamped schema and emitted SQL consistent with Spark parity.
pub(super) fn na_fill_compatible(col_type: &DataType, value_type: &DataType) -> bool {
    if col_type.is_numeric() && value_type.is_numeric() {
        return true;
    }
    matches!(
        (col_type, value_type),
        (DataType::String, DataType::String) | (DataType::Boolean, DataType::Boolean)
    )
}

/// Select the fill value (if any) that `.na.fill` applies to the column
/// `col_name` of static type `col_type`, given the request's `cols` / `values`
/// and the input relation's `schema`.
///
/// Encodes Spark's `fillValue` selection: the empty-`cols` "fill all" branch
/// and the single-value subset branch gate on [`na_fill_compatible`]; the
/// multi-value form pairs by position without filtering (matches Spark's dict
/// form). Shared by `analyze_na_fill` (nullability inference) and emission's
/// `render_na_fill` (SQL) so the stamped schema and emitted SQL stay
/// consistent.
pub(super) fn na_fill_value_for<'a>(
    cols: &[String],
    values: &'a [Expression],
    schema: &ResolvedSchema,
    col_name: &str,
    col_type: &DataType,
) -> Option<&'a Expression> {
    if cols.is_empty() || values.len() == 1 {
        // Single fill value: the column is selected either because
        // `cols` is empty ("fill all") or because it names the column;
        // Spark's type-compatibility filter then gates the fill.
        let selected = cols.is_empty() || cols.iter().any(|c| eq_fold(c, col_name));
        if selected && na_fill_compatible(col_type, &values[0].data_type(schema)) {
            Some(&values[0])
        } else {
            None
        }
    } else {
        for (c, v) in cols.iter().zip(values.iter()) {
            if eq_fold(c, col_name) {
                return Some(v);
            }
        }
        None
    }
}

fn analyze_na_fill(
    input: CommonAst,
    cols: Vec<String>,
    values: Vec<Expression>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    // Columns filled with a non-null value become non-nullable — but only
    // for columns whose static type is compatible with the fill value's type
    // (Spark's `fillValue` silently skips type-incompatible columns). See
    // `na_fill_value_for` for the selection rule.
    let filled = |col_name: &str, col_type: &DataType| -> Option<&Expression> {
        na_fill_value_for(
            &cols,
            &values,
            &typed_input.resolved_schema,
            col_name,
            col_type,
        )
    };
    // NaFill mutates nullability only — same logical columns throughout,
    // so clone (COPY) every field and adjust `nullable` in place.
    let mut output_fields: Vec<Attribute> =
        Vec::with_capacity(typed_input.resolved_schema.fields.len());
    for f in &typed_input.resolved_schema.fields {
        let fill_expr = filled(&f.name, &f.data_type);
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
    let output_schema = ResolvedSchema::new(output_fields);
    Ok(TypedAst::new(
        TypedOp::NaFill {
            input: Box::new(typed_input),
            cols,
            values,
        },
        output_schema,
    ))
}

fn analyze_to_df(
    input: CommonAst,
    column_names: Vec<String>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    let input_fields = &typed_input.resolved_schema.fields;
    if input_fields.len() != column_names.len() {
        return Err(AnalyzerError::SparkEmulated {
            class: "ASSIGNMENT_ARITY_MISMATCH",
            reason: format!(
                "toDF arity mismatch: input has {} columns, got {} names",
                input_fields.len(),
                column_names.len()
            ),
        });
    }
    // Rename is a pure name mutation on the SAME logical column —
    // clone-with-new-name keeps the id (same as WithColumnsRenamed).
    let mut output_fields: Vec<Attribute> = Vec::with_capacity(input_fields.len());
    for (f, new_name) in input_fields.iter().zip(column_names.iter()) {
        let mut nf = f.clone();
        nf.name = new_name.clone();
        // Renaming clears inherited lineage for every slot. The ids remain
        // attached to their logical columns; an outer alias can seed new
        // lineage afterward.
        nf.source_quals.clear();
        output_fields.push(nf);
    }
    // Convert to WithColumnsRenamed for emission simplicity — the renamed
    // resolved_schema built above IS the rename; no pair list is carried.
    let output_schema = ResolvedSchema::new(output_fields);
    Ok(TypedAst::new(
        TypedOp::WithColumnsRenamed {
            input: Box::new(typed_input),
        },
        output_schema,
    ))
}

#[allow(clippy::too_many_arguments)]
fn analyze_join(
    left: CommonAst,
    right: CommonAst,
    join_type: JoinType,
    condition: Option<Expression>,
    using_columns: Vec<String>,
    natural: bool,
    lateral: bool,
    left_plan_ids: Vec<i64>,
    right_plan_ids: Vec<i64>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_left = analyze_node(left, base_types, outer)?;

    if lateral && natural {
        return Err(AnalyzerError::SparkEmulated {
            class: "INCOMPATIBLE_JOIN_TYPES",
            reason: "The join types LATERAL and NATURAL are incompatible.".to_owned(),
        });
    }
    if lateral && !using_columns.is_empty() {
        return Err(AnalyzerError::SparkEmulated {
            class: "UNSUPPORTED_FEATURE.LATERAL_JOIN_USING",
            reason: "The feature is not supported: JOIN USING with LATERAL correlation.".to_owned(),
        });
    }
    if lateral && !matches!(join_type, JoinType::Inner | JoinType::Cross) {
        return Err(AnalyzerError::PuntedOperator {
            op: format!("Join[lateral-{join_type:?}]"),
            reason: "lateral join type not implemented in τ".to_owned(),
        });
    }

    // When `lateral`, the right child sees the left sibling's schema as
    // its OuterScope (correlated refs like `e.dept_id` resolve there).
    // This replaces whatever `outer` was passed in, enforcing one-level
    // correlation: the lateral's inner sees only its immediate left sibling,
    // never the grandparent.
    let typed_right = if lateral {
        let left_outer = OuterScope {
            schema: &typed_left.resolved_schema,
            scopes: &typed_left.scope,
        };
        analyze_node(right, base_types, Some(left_outer))?
    } else {
        analyze_node(right, base_types, outer)?
    };

    // NATURAL-join desugar (Spark's `ResolveNaturalAndUsingJoin`): rewrite
    // NATURAL into the equivalent USING(...) shape now that both sides'
    // resolved schemas are known, so every downstream step (condition
    // resolution, outer-join nullability flip, USING donor rules, output
    // schema) rides the existing, proven USING/Cross machinery unchanged.
    let mut join_type = join_type;
    let mut condition = condition;
    let mut using_columns = using_columns;
    if natural {
        // Spark rejects NATURAL combined with SEMI/ANTI outright.
        if matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti) {
            return Err(AnalyzerError::Other {
                reason: format!("requirement failed: Unsupported natural join type {join_type:?}"),
            });
        }
        debug_assert!(
            condition.is_none() && using_columns.is_empty(),
            "CommonOp::Join invariant: natural implies no condition/using_columns"
        );
        // Case-SENSITIVE exact-name intersection (Spark's `Seq.intersect`,
        // not τ's usual case-insensitive `field_by_name`), in LEFT schema
        // order, keep-first dedup.
        let right_names: HashSet<&str> = typed_right
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        let mut common: Vec<String> = Vec::new();
        for f in &typed_left.resolved_schema.fields {
            if right_names.contains(f.name.as_str()) && !common.contains(&f.name) {
                common.push(f.name.clone());
            }
        }
        if common.is_empty() {
            // No shared column names: Spark yields a condition-less cartesian
            // product (INNER → CROSS; other join kinds have no "outer
            // cross" so a TRUE condition stands in). NEVER both — Cross +
            // condition would emit an invalid `CROSS JOIN ... ON TRUE`.
            match join_type {
                JoinType::Inner => join_type = JoinType::Cross,
                _ => {
                    condition = Some(Expression::Literal(Literal {
                        value: LiteralValue::Boolean(true),
                        data_type: DataType::Boolean,
                    }));
                }
            }
        } else {
            using_columns = common;
        }
    }

    // LATERAL clause-less Inner → Cross rewrite: `JOIN LATERAL (subq) t`
    // with no ON clause is a cross-lateral join (Spark's `LateralJoin(Inner, None)`).
    // Mirrors the NATURAL empty-intersection rewrite above.
    if lateral && condition.is_none() && matches!(join_type, JoinType::Inner) {
        join_type = JoinType::Cross;
    }

    // resolve+assign_types: resolve condition against merged schema.
    let combined_input_schema =
        ResolvedSchema::merge(&typed_left.resolved_schema, &typed_right.resolved_schema);

    // Ambiguity is now surfaced centrally by `resolve_column` (see
    // its comment). Any unqualified reference — whether in the join
    // condition here, or in projections/filters/sort keys above —
    // that resolves to more than one field raises `AmbiguousColumn`.
    //
    // `Expression.Attribute.plan_id` is Spark's mechanism to disambiguate
    // `emp.dept_id == dept.dept_id` — the two refs share a name but carry
    // different plan_ids. `ResolveContext::for_join_condition` binds the
    // join's OWN `left_plan_ids`/`right_plan_ids` into its scope (the same
    // plan_id arm in `resolve_column` that resolves above-join refs), so a
    // plan_id-tagged unqualified reference reaching `resolve_and_stamp`
    // resolves to the correct side directly — no pre-processing needed.
    let condition = match condition {
        Some(c) => {
            let ctx = ResolveContext::for_join_condition(
                &combined_input_schema,
                &typed_left,
                &typed_right,
                &left_plan_ids,
                &right_plan_ids,
                base_types,
                outer,
            );
            Some(resolve_and_stamp(c, &ctx)?)
        }
        None => None,
    };

    // derive_nullability: apply outer-join flipping (§6).
    let (derived_left_schema, derived_right_schema) = apply_join_nullability(
        &typed_left.resolved_schema,
        &typed_right.resolved_schema,
        join_type,
    );
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
    // A USING key remains referenceable through either side's qualifier;
    // merge both donors' lineage onto the selected value/id.
    let build_using_prefix = |using: &[String]| -> Vec<Attribute> {
        let mut fields = Vec::with_capacity(using.len());
        for n in using {
            let left_field = derived_left_schema.field_by_name(n);
            let right_field = derived_right_schema.field_by_name(n);
            let donor = match (join_type, left_field, right_field) {
                (JoinType::Right, _, Some(rf)) => rf.clone(),
                (JoinType::Full, Some(lf), Some(rf)) => {
                    // Non-null iff EITHER side is non-null. Keep the LEFT
                    // side's id — it is the donor of record for FULL/USING.
                    let mut coalesced = lf.clone();
                    coalesced.nullable = lf.nullable && rf.nullable;
                    coalesced
                }
                (_, Some(lf), _) => lf.clone(),
                (_, None, Some(rf)) => rf.clone(),
                _ => continue,
            };
            let mut quals = BTreeSet::new();
            if let Some(lf) = left_field {
                quals.extend(lf.source_quals.iter().cloned());
            }
            if let Some(rf) = right_field {
                quals.extend(rf.source_quals.iter().cloned());
            }
            fields.push(donor.with_quals(quals));
        }
        fields
    };
    let output_schema = if !using_columns.is_empty() {
        let using_lower: HashSet<String> = using_columns.iter().map(|s| fold_key(s)).collect();
        let mut fields = build_using_prefix(&using_columns);
        for f in &derived_left_schema.fields {
            if !using_lower.contains(&fold_key(&f.name)) {
                fields.push(f.clone());
            }
        }
        if !matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti) {
            for f in &derived_right_schema.fields {
                if !using_lower.contains(&fold_key(&f.name)) {
                    fields.push(f.clone());
                }
            }
        }
        ResolvedSchema::new(fields)
    } else {
        match join_type {
            JoinType::LeftSemi | JoinType::LeftAnti => derived_left_schema.clone(),
            _ => ResolvedSchema::merge(&derived_left_schema, &derived_right_schema),
        }
    };

    Ok(TypedAst::new(
        TypedOp::Join {
            left: Box::new(typed_left),
            right: Box::new(typed_right),
            join_type,
            condition,
            using_columns,
            lateral,
            left_plan_ids,
            right_plan_ids,
        },
        output_schema,
    ))
}

/// Analyze a recursive CTE: two-phase anchor-first.
///
/// (a) analyze anchor against ordinary base_types;
/// (b) rename the anchor's resolved schema positionally by column_names;
/// (c) reject `!union_all` (Spark: `UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE`);
/// (d) validate column-name arity;
/// (e) build augmented base_types with the CTE schema injected;
/// (f) analyze recursive_term against augmented;
/// (g) validate recursive_term schema arity matches anchor;
/// (h) push setop casts to pin recursive term's types to anchor's;
/// (i) return `TypedOp::RecursiveCte`.
fn analyze_recursive_cte(
    name: String,
    column_names: Vec<String>,
    union_all: bool,
    anchor: CommonAst,
    recursive_term: CommonAst,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    // (a) Analyze anchor.
    let typed_anchor = analyze_node(anchor, base_types, outer)?;

    // (b) Rename anchor schema by column_names (positional). Renaming is a
    // pure name mutation on the SAME logical column — clone-with-new-name
    // keeps the anchor's ids (same COPY pattern as WithColumnsRenamed).
    let cte_schema = if column_names.is_empty() {
        typed_anchor.resolved_schema.clone()
    } else {
        // (d) Arity check — column list length must match anchor output width.
        if column_names.len() != typed_anchor.resolved_schema.fields.len() {
            return Err(AnalyzerError::SparkEmulated {
                class: "ASSIGNMENT_ARITY_MISMATCH",
                reason: format!(
                    "recursive CTE `{name}` column list has {} names but the anchor produces {} columns",
                    column_names.len(),
                    typed_anchor.resolved_schema.fields.len()
                ),
            });
        }
        let fields = typed_anchor
            .resolved_schema
            .fields
            .iter()
            .zip(column_names.iter())
            .map(|(f, col_name)| {
                let mut attr = f.clone();
                attr.name = col_name.clone();
                attr
            })
            .collect();
        ResolvedSchema::new(fields)
    };

    // (c) Reject UNION (without ALL) — Spark's UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE.
    if !union_all {
        return Err(AnalyzerError::SparkEmulated {
            class: "UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE",
            reason: "The UNION operator is not yet supported within recursive common table \
                     expressions (WITH clauses that refer to themselves, directly or \
                     indirectly). Please use UNION ALL."
                .to_owned(),
        });
    }

    // (e) Inject CTE schema into base_types so self-references resolve. The
    // self-reference sees ALL fields as nullable regardless of the anchor's
    // own nullability — verified against the 4.1.1 reference: a self-ref
    // column (e.g. cte-010's `c.lvl`) always types nullable, while a column
    // untouched by self-reference (cte-010's `e.id`, sourced from the real
    // `emp` table) keeps its own non-nullable type. This models the
    // recursive relation's schema being fixed-point-uncertain across
    // iterations, distinct from the final OR-folded output schema below.
    // Insert under the lowercase key (which `name` already is from lowering).
    // Also collect any self-referencing TableScan names from the recursive term
    // that match case-insensitively but differ in case (e.g. the user wrote
    // `WITH RECURSIVE Chain(...) ... FROM Chain c`), and insert the schema
    // under those exact-case keys too — BaseTypes::lookup is case-sensitive.
    // `BaseTypes` stores plain `StructType` bookkeeping; extract field values
    // per column rather than calling
    // the banned bulk `to_struct_type()` door.
    let self_ref_schema = StructType::new(
        cte_schema
            .fields
            .iter()
            .map(|f| StructField::new(f.name.clone(), f.data_type.clone(), true))
            .collect(),
    );
    let mut augmented = base_types.with_entry(&name, self_ref_schema.clone());
    for source_case_name in self_ref_table_names(&recursive_term, &name) {
        if source_case_name != name {
            augmented = augmented.with_entry(&source_case_name, self_ref_schema.clone());
        }
    }

    // (f) Analyze recursive term with augmented base_types.
    let mut typed_recursive = analyze_node(recursive_term, &augmented, outer)?;

    // (g) Arity match — recursive term must produce the same number of columns.
    if typed_recursive.resolved_schema.fields.len() != cte_schema.fields.len() {
        return Err(AnalyzerError::SparkEmulated {
            class: "ASSIGNMENT_ARITY_MISMATCH",
            reason: format!(
                "recursive CTE `{name}` anchor has {} columns but the recursive term produces {}",
                cte_schema.fields.len(),
                typed_recursive.resolved_schema.fields.len()
            ),
        });
    }

    // Final output nullability = OR-fold across anchor and recursive-term legs
    // (standard UNION ALL nullability rule, same as `widen_by_name`'s
    // pairwise fold for ordinary set operations) — captured BEFORE
    // `push_setop_casts` below overwrites `typed_recursive.resolved_schema`
    // with the anchor-typed schema.
    let output_nullable: Vec<bool> = cte_schema
        .fields
        .iter()
        .zip(typed_recursive.resolved_schema.fields.iter())
        .map(|(anchor_f, rec_f)| anchor_f.nullable || rec_f.nullable)
        .collect();

    // (h) Pin recursive term types to anchor (anchor-directional).
    push_setop_casts(&mut typed_recursive, &cte_schema);

    // The RecursiveCte's own output is a NEW relation (the fixed-point union,
    // not simply "the anchor" or "the recursive term") — mint fresh ids.
    let resolved_schema = ResolvedSchema::new(
        cte_schema
            .fields
            .iter()
            .zip(output_nullable)
            .map(|(f, nullable)| Attribute::minted(f.name.clone(), f.data_type.clone(), nullable))
            .collect(),
    );

    Ok(TypedAst::new(
        TypedOp::RecursiveCte {
            name,
            anchor: Box::new(typed_anchor),
            recursive_term: Box::new(typed_recursive),
        },
        resolved_schema,
    ))
}

/// Collect source-case `TableScan.table` names in `ast` that case-insensitively
/// match `cte_name_lower`. Used by `analyze_recursive_cte` to ensure the
/// injected `BaseTypes` entry covers every casing of the self-reference.
fn self_ref_table_names(ast: &CommonAst, cte_name_lower: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_self_ref_names(ast, cte_name_lower, &mut out);
    out
}

fn collect_self_ref_names(ast: &CommonAst, cte_name_lower: &str, out: &mut Vec<String>) {
    if let CommonOp::TableScan { table, .. } = &ast.op {
        if fold_key(table) == cte_name_lower && !out.contains(table) {
            out.push(table.clone());
        }
    }
    for child in ast.op.children() {
        collect_self_ref_names(child, cte_name_lower, out);
    }
}

fn analyze_set_op(
    kind: SetOpKind,
    all: bool,
    by_name: bool,
    allow_missing_columns: bool,
    children: Vec<CommonAst>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    // UNION BY NAME is analyzed by name-matching each column across
    // children; INTERSECT / EXCEPT BY NAME are not supported by
    // DuckDB itself so we still punt those to a future slice.
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
        .map(|c| analyze_node(c, base_types, outer))
        .collect::<Result<Vec<_>, _>>()?;

    // set-op widening sub-sweep (§5):
    // By-position (default): verify arity + per-column-index type
    // unify. By-name (UNION only): match columns across children by
    // NAME (case-insensitive). When `allow_missing_columns = false`,
    // each child must have the same NAME SET (Spark's strict
    // unionByName) — after that guard, the widening fold is the same
    // one the allow-missing form uses (its missing-name rule never
    // fires when the name sets are equal).
    let widened_schema = if by_name {
        if !allow_missing_columns {
            let first_names_lower: HashSet<String> = typed_children[0]
                .resolved_schema
                .fields
                .iter()
                .map(|f| fold_key(&f.name))
                .collect();
            for (idx, child) in typed_children.iter().enumerate().skip(1) {
                let child_names_lower: HashSet<String> = child
                    .resolved_schema
                    .fields
                    .iter()
                    .map(|f| fold_key(&f.name))
                    .collect();
                if child_names_lower != first_names_lower {
                    return Err(AnalyzerError::SparkEmulated {
                        class: "_LEGACY_ERROR_TEMP_1201",
                        reason: format!(
                            "unionByName column-name mismatch: child 0 has {:?}, child {idx} has {:?}",
                            first_names_lower, child_names_lower
                        ),
                    });
                }
            }
        }
        widen_by_name(&typed_children)?
    } else {
        widen_by_position(kind, &typed_children)?
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
    // mis-casts columns (for example, `salary DOUBLE → id BIGINT`). Skip the
    // pushdown for by-name.
    if !by_name {
        for child in typed_children.iter_mut() {
            push_setop_casts(child, &widened_schema);
        }
    }

    Ok(TypedAst::new(
        TypedOp::SetOp {
            kind,
            all,
            by_name,
            allow_missing_columns,
            children: typed_children,
            widened_schema: widened_schema.clone(),
        },
        widened_schema,
    ))
}

/// Widen a by-name set-op schema across `children`: the ordered union of
/// names — first child's columns first in declared order (its name order is
/// canonical per Spark), followed by each later child's extras in declared
/// order (Spark `ResolveUnion` rule). Per-name types unify via
/// [`TypeInferenceEngine::unify_types`]; nullability OR-folds, and a name
/// missing from any child becomes unconditionally nullable (that child pads
/// with NULL — stronger than the OR rule). In the strict `unionByName` case
/// (equal name sets, guarded by the caller) the missing-name rule never
/// fires, so this same fold serves both by-name forms.
fn widen_by_name(children: &[TypedAst]) -> Result<ResolvedSchema, AnalyzerError> {
    // Build the ordered union of names across all children.
    // Case-insensitive dedup with first-seen casing preserved
    // (matches `StructType::field_by_name`).
    let mut ordered_names: Vec<String> = Vec::new();
    let mut seen_lower: HashSet<String> = HashSet::new();
    for child in children {
        for f in &child.resolved_schema.fields {
            let lower = fold_key(&f.name);
            if seen_lower.insert(lower) {
                ordered_names.push(f.name.clone());
            }
        }
    }
    let mut widened_fields: Vec<Attribute> = Vec::with_capacity(ordered_names.len());
    for name in &ordered_names {
        let mut widened_type: Option<DataType> = None;
        let mut widened_nullable = false;
        let mut any_child_missing = false;
        // Identity: the FIRST child to carry this name donates its id —
        // this is the "same logical column" the widened schema is
        // standing in for at this name.
        let mut base_attr: Option<Attribute> = None;
        // Item 3 (F3/O1 NIT): whether the FIRST child (not just the
        // donating one) carries this name — see the qualifier-clearing
        // note below.
        let first_child_has_name = children[0].resolved_schema.field_by_name(name).is_some();
        for child in children {
            if let Some(fk) = child.resolved_schema.field_by_name(name) {
                widened_type = Some(match widened_type {
                    Some(t) => TypeInferenceEngine::unify_types(&t, &fk.data_type),
                    None => fk.data_type.clone(),
                });
                widened_nullable = widened_nullable || fk.nullable;
                if base_attr.is_none() {
                    base_attr = Some(fk.clone());
                }
            } else {
                any_child_missing = true;
            }
        }
        // `widened_type` must be Some — the name came from
        // some child so at least one child has it.
        let ty = widened_type.ok_or_else(|| AnalyzerError::Internal {
            reason: format!("union-of-names produced orphan name {name:?}"),
        })?;
        // Extras present in only one child become
        // unconditionally nullable — the other child pads
        // with NULL. Stronger than the OR rule.
        let nullable = widened_nullable || any_child_missing;
        let mut attr = base_attr.expect("name came from some child, so base_attr is Some");
        attr.name = name.clone();
        attr.data_type = ty;
        attr.nullable = nullable;
        if !first_child_has_name {
            // `allowMissingColumns` unionByName (Spark's `ResolveUnion`):
            // when a name is missing from the FIRST child, Spark pads the
            // FIRST child with a null-literal-aliased column for it — the
            // union output column has no donor-qualifier addressability,
            // so e.g. `a.unionByName(b.alias("b"), allowMissingColumns=True)`
            // REJECTS `b.z` even though bare `z` resolves (live-Spark-
            // probed, 4.1.1, 2026-07-14). Without this, `base_attr` above
            // is a `.clone()` of the donating (non-first) child's
            // `Attribute`, inheriting ITS qualifiers — making `b.z`
            // resolvable in τ, strictly more permissive than Spark. Clear
            // the qualifiers so this widened attribute is addressable only
            // by its bare name, matching Spark's first-child-null-alias
            // pad slot. The retained expr_id is the DONOR (non-first)
            // child's — not the first child's, and not a fresh mint;
            // Spark's pad path mints a fresh null-alias exprId, so the
            // donor-id retention is a recorded benign divergence (ADR-024
            // tier-3e amendment). Only qualifier addressability is
            // dropped. Names present in the first child are untouched
            // (F3 pinned first-child qualifiers over unions as faithful).
            attr = attr.with_quals(BTreeSet::new());
        }
        widened_fields.push(attr);
    }
    Ok(ResolvedSchema::new(widened_fields))
}

/// Widen a positional (by-index) set-op schema across `children`: verify
/// arity, unify each column index's type across every child, and fold
/// nullability per Spark's operator-aware rule. Names and order come from the
/// first child.
fn widen_by_position(
    kind: SetOpKind,
    children: &[TypedAst],
) -> Result<ResolvedSchema, AnalyzerError> {
    let first_len = children[0].resolved_schema.len();
    for (idx, child) in children.iter().enumerate().skip(1) {
        if child.resolved_schema.len() != first_len {
            return Err(AnalyzerError::SparkEmulated {
                class: "NUM_COLUMNS_MISMATCH",
                reason: format!(
                    "set-op arity mismatch: child 0 has {} columns, child {idx} has {}",
                    first_len,
                    child.resolved_schema.len()
                ),
            });
        }
    }
    let mut widened_fields: Vec<Attribute> = Vec::with_capacity(first_len);
    for col_idx in 0..first_len {
        let first_field = &children[0].resolved_schema.fields[col_idx];
        // Type widening (ADR-006) is operator-independent: unify the
        // i-th column type across every child regardless of `kind`.
        let mut widened_type = first_field.data_type.clone();
        for child in children.iter().skip(1) {
            let f = &child.resolved_schema.fields[col_idx];
            widened_type = TypeInferenceEngine::unify_types(&widened_type, &f.data_type);
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
            SetOpKind::Union => children
                .iter()
                .any(|child| child.resolved_schema.fields[col_idx].nullable),
            SetOpKind::Intersect => children
                .iter()
                .all(|child| child.resolved_schema.fields[col_idx].nullable),
            SetOpKind::Except => first_field.nullable,
        };
        // Identity: child 0's id at this position — the widened schema's
        // column at index `col_idx` stands in for child 0's own column.
        let mut attr = first_field.clone();
        attr.data_type = widened_type;
        attr.nullable = widened_nullable;
        widened_fields.push(attr);
    }
    Ok(ResolvedSchema::new(widened_fields))
}

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
    input_schema: &ResolvedSchema,
) -> Result<Vec<Expression>, AnalyzerError> {
    let mut out = Vec::with_capacity(projections.len());
    for projection in projections {
        let Expression::UnresolvedRegex(regex) = projection else {
            out.push(projection);
            continue;
        };
        let compiled = regex::Regex::new(&regex.pattern).map_err(|error| AnalyzerError::Other {
            reason: format!("invalid regex `{}`: {error}", regex.pattern),
        })?;
        let start = out.len();
        out.extend(
            input_schema
                .fields
                .iter()
                .filter(|field| compiled.is_match(&field.name))
                .map(|field| {
                    Expression::UnresolvedColumn(UnresolvedColumn {
                        name: field.name.clone(),
                        qualifier: None,
                        plan_id: regex.plan_id,
                    })
                }),
        );
        if out.len() == start {
            return Err(AnalyzerError::UnknownColumn {
                name: regex.pattern,
                qualifier: None,
            });
        }
    }
    Ok(out)
}

// `SELECT salary * 1.1 AS raised, raised - salary AS delta FROM emp`: Spark
// lets a later SELECT-list item reference an earlier item's alias. τ's
// resolver (`resolve_and_stamp` / `ResolveContext`) is schema/scope-shaped —
// it has no notion of "aliases introduced earlier in this same projection
// list" — so this is a dedicated pre-pass, not a `ResolveContext` extension.
//
// Semantics (confirmed against Spark 4.1.1's `ResolveLateralColumnAliasReference`
// catalyst rule, whose `AliasEntry(alias, index)` map is itself left-to-right,
// map-then-substitute):
// * Substitution, not scoped lookup — a later reference is replaced with the
//   earlier item's (already-inlined) expression, so a 3-item chain resolves
//   in one left-to-right pass with no fixed-point iteration.
// * Input-column-wins — a name that is BOTH a real input column and a
//   would-be lateral alias is never recorded as a lateral source; the real
//   column always wins at the reference site (falls through to ordinary
//   resolution).
// * Ambiguity is lazy — two same-named aliases with no later reference is
//   ordinary, legal SQL (duplicate output names). It only errors
//   (`AnalyzerError::AmbiguousLateralColumnAlias`) if a LATER item actually
//   references the shared name.

/// Left-to-right accumulator of LCA definitions built while folding one
/// `Project`'s projection list. Append-only MULTIMAP — mirrors
/// `RelScope::lookup`'s "exactly one match or bail" shape, but (unlike
/// `RelScope::lookup`, which collapses both to `None`) distinguishes
/// 0 matches (fall through to ordinary resolution) from 2+ (a hard
/// ambiguity error) — see [`Self::lookup`].
#[derive(Debug, Default)]
struct LateralAliasTable {
    entries: Vec<(String, Expression)>,
}

impl LateralAliasTable {
    /// Record a newly-eligible lateral alias definition. Callers push in
    /// list order; a reused name appends a second entry rather than
    /// overwriting, mirroring Spark's own `AliasEntry` map.
    fn record(&mut self, name: String, expr: Expression) {
        self.entries.push((name, expr));
    }

    /// Case-insensitive lookup against every entry recorded so far.
    ///
    /// * 0 matches -> `Ok(None)` — the caller leaves the reference as a
    ///   plain `UnresolvedColumn`, which falls through to ordinary
    ///   `resolve_and_stamp`/`resolve_column` resolution, so a genuinely
    ///   nonexistent name still gets the correct `UnknownColumn`.
    /// * Exactly 1 match -> `Ok(Some(&expr))` — substitute.
    /// * 2+ matches -> `Err(AmbiguousLateralColumnAlias)` — Spark's own
    ///   `AMBIGUOUS_LATERAL_COLUMN_ALIAS` error class.
    fn lookup(&self, name: &str) -> Result<Option<&Expression>, AnalyzerError> {
        let mut found: Option<&Expression> = None;
        let mut count = 0usize;
        for (entry_name, expr) in &self.entries {
            if eq_fold(entry_name, name) {
                count += 1;
                found = Some(expr);
            }
        }
        match count {
            0 => Ok(None),
            1 => Ok(found),
            _ => Err(AnalyzerError::AmbiguousLateralColumnAlias {
                name: name.to_owned(),
                count,
            }),
        }
    }
}

/// Left-to-right fold over `projections`: for each item, substitute any
/// eligible earlier-sibling lateral alias reference (via
/// [`substitute_lateral_aliases`]), then — if the (already-substituted) item
/// is itself an `Alias` whose name does not collide with a real input
/// column — record it as a new lateral alias definition for subsequent
/// items. Recording the POST-substitution expression (not the raw one) is
/// what makes a multi-hop chain (`a` -> `b` referencing `a` -> `c`
/// referencing `b`) resolve fully-inlined in a single pass.
///
/// Called by the `CommonOp::Project` arm after generator normalization and
/// before ordinary expression resolution.
fn expand_lateral_column_aliases(
    projections: Vec<Expression>,
    input_schema: &ResolvedSchema,
) -> Result<Vec<Expression>, AnalyzerError> {
    let mut table = LateralAliasTable::default();
    let mut out = Vec::with_capacity(projections.len());
    for proj in projections {
        let proj = substitute_lateral_aliases(proj, &table)?;
        if let Expression::Alias(ref a) = proj {
            if input_schema.field_by_name(&a.alias).is_none() {
                table.record(a.alias.clone(), (*a.expr).clone());
            }
        }
        out.push(proj);
    }
    Ok(out)
}

/// Recursively rewrite every unqualified `UnresolvedColumn` in `expr` that
/// case-insensitively matches exactly one entry in `table` to that entry's
/// (already-inlined) expression, cloned. Zero matches leave the column
/// reference unchanged (ordinary resolution handles it — typo or genuine
/// forward-reference alike). Recurses via `Expression::map_children`, the
/// SAME fallible-fold primitive `resolve_and_stamp` itself uses, walking
/// into `FunctionCall` args, `CaseWhen` branches, etc.
///
/// Opaque per [`Expression::is_opaque_unit`], plus `UnresolvedRegex` — a
/// documented pass-through matching
/// `resolve_and_stamp`'s own extra arm — all pass through unchanged. A
/// lambda's own bound parameter must never be mistaken for an outer lateral
/// alias reference (e.g. `SELECT 1 AS x, transform(arr, x -> x + 1) FROM t` —
/// the lambda's `x` is its own bound variable, not the outer alias).
/// `InSubquery`/`ExistsSubquery`/`ScalarSubquery` are not custom-cased here;
/// `expression_children!` already yields no children for the two
/// Exists/Scalar forms and only the outer probe expression for `InSubquery`,
/// so the default `map_children` arm below reproduces the same opacity
/// `resolve_and_stamp` gets from its explicit arm.
fn substitute_lateral_aliases(
    expr: Expression,
    table: &LateralAliasTable,
) -> Result<Expression, AnalyzerError> {
    if expr.is_resolve_opaque() {
        return Ok(expr);
    }
    match expr {
        Expression::UnresolvedColumn(ref u) if u.qualifier.is_none() => {
            let name = u.name.clone();
            match table.lookup(&name)? {
                Some(replacement) => Ok(replacement.clone()),
                None => Ok(expr),
            }
        }
        _ => expr.map_children(|e| substitute_lateral_aliases(e, table)),
    }
}

/// Resolve `expr` against `ctx` and enforce Spark's boolean-predicate guard
/// shared by `Filter` conditions and `HAVING` clauses: the resolved type must be
/// `Boolean` (or still `Unresolved`/`Null`), otherwise a `TypeMismatch` carrying
/// `context` is returned. Preserves the caller-provided context string verbatim.
fn resolve_boolean_predicate(
    expr: Expression,
    ctx: &ResolveContext,
    context: &str,
) -> Result<Expression, AnalyzerError> {
    let resolved = resolve_and_stamp(expr, ctx)?;
    let cond_type = resolved.data_type(ctx.schema);
    if !matches!(
        cond_type,
        DataType::Boolean | DataType::Unresolved | DataType::Null
    ) {
        return Err(AnalyzerError::TypeMismatch {
            expected: DataType::Boolean,
            actual: cond_type,
            context: context.to_owned(),
        });
    }
    Ok(resolved)
}

/// Analyze a table-valued function call. Arguments resolve against an empty
/// schema — Spark's TVF arguments must be foldable constants, so a bare column
/// ref correctly fails `UnknownColumn`. `range(end)` / `range(start, end)` /
/// `range(start, end, step)` / `range(start, end, step, numPartitions)` (arity
/// 1..=4) resolves to a single non-nullable `id: Long` column, end-exclusive
/// (ADR-005; Spark 4.1.1 `range`). Generators use `CommonOp::Generate`, so any
/// other table function is an honest Thunderduck boundary.
fn analyze_table_function(
    name: String,
    args: Vec<Expression>,
    with_ordinality: bool,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    let empty_schema = ResolvedSchema::empty();
    let resolved_args = resolve_expr_list(args, &ResolveContext::bare(&empty_schema, base_types))?;
    match name.as_str() {
        "range" if (1..=4).contains(&resolved_args.len()) => Ok(TypedAst::new(
            TypedOp::TableFunction {
                name,
                args: resolved_args,
                with_ordinality,
            },
            ResolvedSchema::minted(StructType::new(vec![StructField::new(
                "id",
                DataType::Long,
                false,
            )])),
        )),
        _ => Err(AnalyzerError::PuntedOperator {
            op: format!("TableFunction[{name}]"),
            reason: "table-function analysis (not implemented in τ)".to_owned(),
        }),
    }
}

fn resolve_and_stamp(expr: Expression, ctx: &ResolveContext) -> Result<Expression, AnalyzerError> {
    // Opaque expressions are not traversed by resolution.
    if expr.is_resolve_opaque() {
        return Ok(expr);
    }
    match expr {
        Expression::UnresolvedColumn(u) => resolve_column(u, ctx),
        // Front-ends emit `UnresolvedColumn`; a `ColumnReference` reaching
        // resolution is therefore always already stamped by one of the
        // analyzer's own construction sites (`resolve_column` and friends) —
        // there is no bare/"already resolved" `ColumnReference` constructor
        // left to fill in, and `data_type`/`nullable` are non-`Option` by
        // construction (E4/D3), so there is nothing left to assert here —
        // pure passthrough. `expr_id` legitimately stays `None` for a
        // tier-(d) struct-qualifier reference (and its outer twin) — see
        // `ColumnReference::expr_id`'s doc.
        Expression::ColumnReference(c) => Ok(Expression::ColumnReference(c)),
        // Subqueries: analyze the inner plan with the enclosing context
        // threaded as the outer scope so correlated outer references (e.g.
        // `e.salary` referencing the outer `emp e`) resolve against the
        // parent plan's schema when the inner plan has no match (ADR-008).
        Expression::ScalarSubquery(mut s) => {
            s.subquery = analyze_single_column_subquery(
                s.subquery,
                ctx,
                "scalar subquery must return exactly one column",
            )?;
            Ok(Expression::ScalarSubquery(s))
        }
        Expression::InSubquery(mut i) => {
            i.expr = Box::new(resolve_and_stamp(*i.expr, ctx)?);
            i.subquery = analyze_single_column_subquery(
                i.subquery,
                ctx,
                "IN subquery must return exactly one column",
            )?;
            Ok(Expression::InSubquery(i))
        }
        Expression::ExistsSubquery(mut e) => {
            let inner = analyze_subquery_plan(e.subquery, ctx)?;
            e.subquery = SubqueryPlan::Analyzed(Box::new(inner));
            Ok(Expression::ExistsSubquery(e))
        }
        // Dropping a missing struct field is a Spark no-op.
        Expression::UpdateFields(_) => expr.map_children(|e| resolve_and_stamp(e, ctx)),
        // Recurse first, then materialize
        // the binary-arithmetic coercions (decimal-Div widening, Date ±
        // Interval correction) `binary_data_type`'s own inference implies but
        // does not insert into the tree. See
        // [`materialize_binary_coercions`]'s doc for the full contract.
        Expression::Binary(_) => {
            let recursed = expr.map_children(|e| resolve_and_stamp(e, ctx))?;
            Ok(materialize_binary_coercions(recursed, ctx.schema))
        }
        // An `implicit` Cast can arrive here already
        // materialized (e.g. re-running `resolve_and_stamp` over a
        // `semantic_eq` rebind side). Recurse into the child first, then
        // collapse a redundant nested implicit Cast to the SAME target type
        // — `Cast_impl(T, Cast_impl(T, x))` → `Cast_impl(T, x)` — so
        // repeated materialization reaches a fixpoint rather than stacking
        // CASTs.
        Expression::Cast(c) if c.implicit => {
            let CastExpression {
                expr: inner,
                to_type,
                try_cast,
                implicit,
            } = c;
            let resolved_inner = resolve_and_stamp(*inner, ctx)?;
            Ok(match resolved_inner {
                Expression::Cast(inner_c) if inner_c.implicit && inner_c.to_type == to_type => {
                    Expression::Cast(inner_c)
                }
                other => Expression::Cast(CastExpression {
                    expr: Box::new(other),
                    to_type,
                    try_cast,
                    implicit,
                }),
            })
        }
        // Function names are canonicalized at the front-end boundary.
        Expression::FunctionCall(f) => {
            debug_assert!(
                !f.name.bytes().any(|b| b.is_ascii_uppercase()),
                "N5: FunctionCall.name must be canonical lowercase: {}",
                f.name
            );
            Expression::FunctionCall(f).map_children(|e| resolve_and_stamp(e, ctx))
        }
        Expression::Generator(generator) => Err(AnalyzerError::SparkEmulated {
            class: "UNSUPPORTED_GENERATOR.NESTED_IN_EXPRESSIONS",
            reason: format!(
                "generator `{}` may only appear at the top level of SELECT",
                generator.name()
            ),
        }),
        // Default recursion: walk every immediate child via the shared
        // walker. Covers Unary / Cast (non-implicit) / CaseWhen / Window /
        // Alias / Between / InList / Like / IsDistinctFrom / ExtractValue /
        // ArrayLiteral / MapLiteral / StructLiteral plus the
        // leaf variants (Literal, Star) which return themselves unchanged
        // inside `map_children`.
        _ => expr.map_children(|e| resolve_and_stamp(e, ctx)),
    }
}

fn resolve_expr_list(
    exprs: Vec<Expression>,
    ctx: &ResolveContext,
) -> Result<Vec<Expression>, AnalyzerError> {
    exprs
        .into_iter()
        .map(|e| resolve_and_stamp(e, ctx))
        .collect()
}

// Resolve ORDER BY keys against the child output first, then against an
// Aggregate/Project child's input when Spark's aggregate-restatement rules
// require it. Unmatched promotable keys become hidden outputs and are trimmed
// from the final schema; unresolvable keys retain Spark's UnknownColumn error.

/// Resolve every ORDER BY key of a `Sort` against its child `ti`. Per key:
/// today's direct resolution (unchanged) when it succeeds and the key does
/// not restate an aggregate; otherwise the fallback in
/// [`rebind_sort_key`].
fn analyze_sort(
    order: Vec<SortOrder>,
    ti: &mut TypedAst,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<Vec<SortOrder>, AnalyzerError> {
    let resolved = order
        .into_iter()
        .map(|so| {
            let expr = analyze_sort_key(*so.expr, ti, base_types, outer)?;
            Ok(SortOrder {
                expr: Box::new(expr),
                direction: so.direction,
                null_ordering: so.null_ordering,
            })
        })
        .collect::<Result<Vec<_>, AnalyzerError>>()?;
    // Promotion may grow the child schema; verify its scope remains derived
    // from the same operator/schema pair.
    debug_assert_eq!(
        RelScope::of(&ti.op, &ti.resolved_schema),
        ti.scope,
        "growth-invariant violated: re-derived scope must equal the carried scope"
    );
    Ok(resolved)
}

/// Resolve one ORDER BY key against the Sort's child `ti`.
///
/// Step 1 (today's behavior, unchanged): resolve against `ti`'s OUTPUT
/// schema. Success with no aggregate `FunctionCall` in the resolved key →
/// done, byte-identical to before this pass.
///
/// The fallback (steps 3-4, [`rebind_sort_key`]) triggers ONLY when step 1
/// (i) fails with `UnknownColumn`, or (ii) succeeds but `ti.op` is
/// `TypedOp::Aggregate` and the resolved key contains an aggregate
/// `FunctionCall` (that combination always dies at DuckDB's binder today —
/// e.g. tpcds-q096's `ORDER BY count(1)` over a global aggregate). Any OTHER
/// step-1 error propagates unchanged. On trigger (i), a fallback failure
/// re-raises step 1's EXACT `UnknownColumn` (unchanged error text) rather
/// than whatever [`rebind_sort_key`] itself constructs.
fn analyze_sort_key(
    key: Expression,
    ti: &mut TypedAst,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<Expression, AnalyzerError> {
    let original = key.clone();
    let step1 = resolve_and_stamp(key, &ResolveContext::of_input(ti, base_types, outer));
    match step1 {
        Ok(resolved) => {
            let restates_aggregate = matches!(ti.op, TypedOp::Aggregate { .. })
                && contains_matching_call(&resolved, is_aggregate_classifier_name);
            if !restates_aggregate {
                return Ok(resolved);
            }
            rebind_sort_key(original, ti, base_types, outer)
        }
        Err(AnalyzerError::UnknownColumn { name, qualifier }) => {
            rebind_sort_key(original, ti, base_types, outer)
                .map_err(|_| AnalyzerError::UnknownColumn { name, qualifier })
        }
        Err(other) => Err(other),
    }
}

/// Which of the two children [`rebind_over_child`] is rebinding against —
/// carries the one piece of shape-specific data (`Aggregate`'s `grouping`
/// list) the shared fallback needs on top of the output list every child
/// variant already provides.
#[derive(Clone, Copy)]
enum SortChild<'a> {
    Aggregate { grouping: &'a [Expression] },
    Project,
}

/// Re-resolve against the child's own input, then bind to or promote a
/// semantically matching output entry.
///
/// Only `TypedOp::Aggregate` / `TypedOp::Project` children define an "own
/// input" / "own SELECT list" here — any other child (e.g. `Deduplicate`)
/// never engages this fallback, matching Spark's own refusal to push
/// resolution through a non-Aggregate/Project operator.
fn rebind_sort_key(
    original: Expression,
    ti: &mut TypedAst,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<Expression, AnalyzerError> {
    match &mut ti.op {
        TypedOp::Aggregate {
            input,
            grouping,
            aggregates,
            ..
        } => rebind_over_child(
            original,
            input,
            aggregates,
            SortChild::Aggregate { grouping },
            &mut ti.resolved_schema,
            base_types,
            outer,
        ),
        TypedOp::Project {
            input, projections, ..
        } => rebind_over_child(
            original,
            input,
            projections,
            SortChild::Project,
            &mut ti.resolved_schema,
            base_types,
            outer,
        ),
        _ => Err(unresolvable_sort_key_error(&original)),
    }
}

/// The aggregate and project output lists align directly with schema fields;
/// Star expansion is rejected when it breaks that positional invariant.
fn rebind_over_child(
    original: Expression,
    child_input: &TypedAst,
    output_list: &mut Vec<Expression>,
    kind: SortChild<'_>,
    schema: &mut ResolvedSchema,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<Expression, AnalyzerError> {
    // Star projections (and any other schema-expanding rewrite that runs
    // BEFORE `resolve_and_stamp`) break the 1:1 alignment between
    // `output_list`'s positions and `schema`'s fields that the rewrite below
    // depends on — bail rather than mis-index.
    if output_list.len() != schema.len() {
        return Err(unresolvable_sort_key_error(&original));
    }
    let ctx = ResolveContext::of_input(child_input, base_types, outer);
    let input_resolved = resolve_and_stamp(original.clone(), &ctx)
        .map_err(|_| unresolvable_sort_key_error(&original))?;
    // `promote_subtree` checks whole-key matches before descending.
    let input_schema = &child_input.resolved_schema;
    promote_subtree(input_resolved, output_list, schema, input_schema, kind)
        .ok_or_else(|| unresolvable_sort_key_error(&original))
}

/// Bind a sort key to an existing output entry by returning a bare reference
/// to its schema field. Output entries are already named and read-only.
fn bind_slot(entries: &[Expression], schema: &ResolvedSchema, k: usize) -> Expression {
    debug_assert!(
        matches!(
            &entries[k],
            Expression::ColumnReference(_) | Expression::Star(_) | Expression::Alias(_)
        ),
        "N8: output-list entry must be a NamedExpression"
    );
    Expression::ColumnReference(ColumnReference::from_attr(&schema.fields[k]))
}

/// Whether sort-key promotion must treat an expression as opaque. Window and
/// subquery bodies cannot be rewritten without changing their scope or shape.
fn opaque_to_subtree_promotion(expr: &Expression) -> bool {
    expr.is_opaque_unit()
        || matches!(
            expr,
            Expression::Window(_)
                | Expression::ScalarSubquery(_)
                | Expression::InSubquery(_)
                | Expression::ExistsSubquery(_)
        )
}

/// Recursively bind or promote subtrees of `expr` already resolved against
/// the child's own input schema. Structurally matching output entries bind
/// directly; otherwise eligible aggregate, grouping, or bare-column
/// subtrees are promoted to hidden outputs.
/// - otherwise, per `kind`: for [`SortChild::Aggregate`], a subtree that is
///   itself an aggregate-classifier `FunctionCall`, or that matches
///   (`semantic_eq`) a `grouping` entry, is promoted via
///   [`promote_hidden_alias`] (a fresh `Alias(subtree, name)` MINTED onto
///   `output_list`); for [`SortChild::Project`], a bare `ColumnReference`
///   subtree is PROMOTED via [`promote_bare_column`] (pushed onto
///   `output_list` verbatim, copying its identity — Spark
///   `ResolveReferencesInSort`'s "add the missing ATTRIBUTE, not a computed
///   subexpression"). Deduplication (a subtree already promoted earlier
///   in the same walk or by an earlier key in the same `ORDER BY`) is caught
///   by the whole-entry match above since it is now literally present in
///   `output_list`) — Spark `ResolveAggregateFunctions#buildAggExprList`'s
///   by the whole-entry match above;
/// - otherwise, recursion continues into its children (unless
///   [`opaque_to_subtree_promotion`] says the node is an opaque unit);
/// - a `ColumnReference` LEAF that survives all of the above (neither bound
///   nor promotable) is Spark's "cannot be resolved" case: `None`, so the
///   caller re-raises the ORIGINAL `UnknownColumn` (confirmed against a live
///   Spark 4.1.1 session to be `UNRESOLVED_COLUMN.WITH_SUGGESTION`, the SAME
///   class an ordinary unresolvable reference gets — there is no distinct
///   `MISSING_AGGREGATION`-style error for this shape from `ORDER BY`).
fn promote_subtree(
    expr: Expression,
    output_list: &mut Vec<Expression>,
    schema: &mut ResolvedSchema,
    input_schema: &ResolvedSchema,
    kind: SortChild<'_>,
) -> Option<Expression> {
    // Canonicalize the FIXED `expr` side once for both the whole-entry match
    // below and (Aggregate only) the `grouping` match; `None`
    // (nondeterministic) short-circuits both.
    let ca_expr = (!contains_matching_call(&expr, is_nondeterministic_fn_name))
        .then(|| canonicalize_for_semantic_eq(&expr));
    if let Some(k) = ca_expr.as_ref().and_then(|ca| {
        output_list
            .iter()
            .position(|entry| semantic_eq_against(ca, entry))
    }) {
        return Some(bind_slot(output_list, schema, k));
    }
    match kind {
        SortChild::Aggregate { grouping } => {
            let is_new_aggregate = matches!(&expr, Expression::FunctionCall(f) if is_aggregate_classifier_name(&f.name));
            let matches_grouping = ca_expr
                .as_ref()
                .is_some_and(|ca| grouping.iter().any(|g| semantic_eq_against(ca, g)));
            if is_new_aggregate || matches_grouping {
                return Some(promote_hidden_alias(
                    expr,
                    output_list,
                    schema,
                    input_schema,
                ));
            }
        }
        SortChild::Project => {
            if matches!(&expr, Expression::ColumnReference(_)) {
                return Some(promote_bare_column(expr, output_list, schema, input_schema));
            }
        }
    }
    if matches!(expr, Expression::ColumnReference(_)) || opaque_to_subtree_promotion(&expr) {
        return None;
    }
    expr.map_children(|c| promote_subtree(c, output_list, schema, input_schema, kind).ok_or(()))
        .ok()
}

/// [`promote_subtree`]'s `SortChild::Aggregate` promotion: `expr` is either a
/// fresh aggregate-classifier `FunctionCall` or a computed subexpression
/// matching a `grouping` entry — either way a brand-new logical column that
/// did not exist in the output list before this point: MINT (never
/// clone-derive from `input_schema` here).
fn promote_hidden_alias(
    expr: Expression,
    aggregates: &mut Vec<Expression>,
    schema: &mut ResolvedSchema,
    input_schema: &ResolvedSchema,
) -> Expression {
    let name = unique_hidden_output_name(&expr, aggregates);
    let field = Attribute::minted(
        name.clone(),
        expr.data_type(input_schema),
        expr.nullable(input_schema),
    );
    aggregates.push(Expression::Alias(AliasExpression {
        expr: Box::new(expr),
        alias: name,
    }));
    schema.fields.push(field.clone());
    Expression::ColumnReference(ColumnReference::from(field))
}

/// Promote a bare input reference by copying its existing attribute identity.
fn promote_bare_column(
    expr: Expression,
    projections: &mut Vec<Expression>,
    schema: &mut ResolvedSchema,
    input_schema: &ResolvedSchema,
) -> Expression {
    let field = output_attribute(&expr, input_schema);
    projections.push(expr);
    schema.fields.push(field.clone());
    Expression::ColumnReference(ColumnReference::from(field))
}

/// A hidden-promotion output name, uniquified against existing
/// names (case-insensitive) so two structurally-different promoted subtrees
/// never collide on the same schema field name.
fn unique_hidden_output_name(expr: &Expression, aggregates: &[Expression]) -> String {
    let base = expression_output_name(expr);
    let taken: HashSet<String> = aggregates
        .iter()
        .map(|e| fold_key(&expression_output_name(e)))
        .collect();
    if !taken.contains(&fold_key(&base)) {
        return base;
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("{base}_{n}");
        if !taken.contains(&fold_key(&candidate)) {
            return candidate;
        }
        n += 1;
    }
}

/// The catch-all `UnknownColumn` for a sort key the fallback could not bind
/// to any child SELECT-list entry. Named from the original
/// key when it is itself a bare column reference; otherwise from its
/// output-name rendering, so the error text still names the offending key.
fn unresolvable_sort_key_error(original: &Expression) -> AnalyzerError {
    match original {
        Expression::UnresolvedColumn(u) => AnalyzerError::UnknownColumn {
            name: u.name.clone(),
            qualifier: u.qualifier.clone(),
        },
        Expression::ColumnReference(c) => AnalyzerError::UnknownColumn {
            name: c.name.clone(),
            qualifier: c.qualifier.clone(),
        },
        other => AnalyzerError::UnknownColumn {
            name: expression_output_name(other),
            qualifier: None,
        },
    }
}

/// `true` iff `expr` contains (anywhere in its tree) a `FunctionCall` whose
/// name satisfies `pred` — the shared walk behind both the aggregate-
/// classifier check ([`is_aggregate_classifier_name`], used to detect a Sort
/// key that restates a SELECT-list aggregate) and the nondeterministic-
/// function check
/// ([`is_nondeterministic_fn_name`], used to exclude a Sort key that calls a
/// nondeterministic function from the [`semantic_eq`] rebind fallback). Both
/// rosters are name-keyed and non-exhaustive; per-instance nondeterminism
///
/// `Expression::children()` also descends into `WindowFunction.func`, so a
/// key restating `sum(x) OVER (...)` also trips the aggregate check — that
/// A window aggregate is not a plain SELECT-list aggregate and therefore is
/// not rebound.
fn contains_matching_call(expr: &Expression, pred: fn(&str) -> bool) -> bool {
    if let Expression::FunctionCall(f) = expr {
        if pred(&f.name) {
            return true;
        }
    }
    expr.children().any(|c| contains_matching_call(c, pred))
}

/// Spark-`semanticEquals`-inspired structural comparison used to bind a
/// resolved Sort key back onto a matching child SELECT-list entry (Spark
/// `ResolveAggregateFunctions#buildAggExprList`'s first-match fold over
/// `agg.aggregateExpressions`). Alias-stripped (mirrors Spark's
/// `trimAliases`), qualifier-stripped and case-insensitive on column/function
/// names (the two sides may carry different or absent table qualifiers, and
/// independently-resolved `data_type`/`nullable` stamps, for the identical
/// logical column post-resolution). Always `false` when either side contains
/// a nondeterministic function call.
///
/// `a` and `b` are typically (though not necessarily — see below) resolved
/// against the SAME child-input schema (`rebind_sort_key` re-resolves the
/// sort key against `child_input`, and `child_list`'s entries were themselves
/// resolved against that same `child_input` when the Aggregate/Project node
/// was originally analyzed — see the `CommonOp::Aggregate`/`CommonOp::Project`
/// arms). Either way, `ColumnReference::expr_id` is [`ExprId`](super::schema::ExprId)
/// — the SAME process-global identity Spark's own `exprId` provides — so the
/// [`ids_compatible`] check below is sound EVEN ACROSS DIFFERENT SCHEMAS: two
/// refs with equal ids are the same logical column no matter which schema
/// each was resolved against, and two refs that happen to share a
/// (qualifier-stripped) name but different ids (e.g. `t1.x` and `t2.x` after a
/// join) are correctly told apart. This is sound across schemas because
/// single shared input schema. NOTE: [`ColumnReference`]'s hand-written
/// `PartialEq` deliberately EXCLUDES `expr_id` (it is derived data, not part
/// of a reference's logical identity for every OTHER analyzer equality
/// check), so the structural `==` below can never see it —
/// [`ids_compatible`] re-walks both (already `==`-confirmed, hence
/// same-shape) canonicalized trees afterwards to add that check back in
/// specifically for this comparison.
///
/// Production call sites (`rebind_over_child`/[`promote_subtree`]) all
/// compare many candidates against one fixed operand and so call
/// [`semantic_eq_against`] directly (hoisting the fixed side's
/// canonicalization out of the loop); this pairwise form is kept for the
/// single-pair comparisons.
#[cfg(test)]
fn semantic_eq(a: &Expression, b: &Expression) -> bool {
    if contains_matching_call(a, is_nondeterministic_fn_name) {
        return false;
    }
    let ca = canonicalize_for_semantic_eq(a);
    semantic_eq_against(&ca, b)
}

/// [`semantic_eq`], factored so a loop comparing many `entry` candidates
/// against one FIXED expression (`rebind_over_child`/[`promote_subtree`]'s
/// `.position`/`.any` loops) pays for [`canonicalize_for_semantic_eq`] and
/// the nondeterminism check on the FIXED side exactly ONCE, not on every
/// iteration — those
/// callers precompute `ca_fixed` themselves (skipping the call entirely, and
/// hence the whole loop, when the fixed side is itself nondeterministic; see
/// each call site) and pass it in here. The `entry` side is still
/// canonicalized and nondeterminism-checked per call, since it legitimately
/// differs every iteration.
fn semantic_eq_against(ca_fixed: &Expression, entry: &Expression) -> bool {
    if contains_matching_call(entry, is_nondeterministic_fn_name) {
        return false;
    }
    let ce = canonicalize_for_semantic_eq(entry);
    ca_fixed == &ce && ids_compatible(ca_fixed, &ce)
}

/// Walk two expressions already confirmed structurally `==` (hence the same
/// shape/variant at every level — `==` recurses through `map_children`'s
/// child slots the same way [`Expression::children`] does) and additionally
/// require that wherever BOTH sides are a `ColumnReference` carrying a
/// resolved `expr_id`, those ids agree. Closes the gap
/// `ColumnReference::eq`'s `expr_id` exclusion leaves open: two different
/// input columns that happen to share a (qualifier-stripped) name — e.g.
/// `t1.x` and `t2.x` after a join — canonicalize and `==`-compare IDENTICAL,
/// so without the id check `semantic_eq` would bind to whichever SELECT-list
/// entry happens to come first, silently picking the wrong one.
fn ids_compatible(a: &Expression, b: &Expression) -> bool {
    match (a, b) {
        (Expression::ColumnReference(ca), Expression::ColumnReference(cb)) => {
            match (ca.expr_id, cb.expr_id) {
                (Some(ia), Some(ib)) => ia == ib,
                _ => true,
            }
        }
        _ => a
            .children()
            .zip(b.children())
            .all(|(x, y)| ids_compatible(x, y)),
    }
}

/// Recursively strip every `Alias` wrapper and implicit Cast (analyzer
/// materialization that Spark's `semanticEquals` does not see)
/// and case-fold/qualifier-strip `ColumnReference` / `UnresolvedColumn`
/// identity, producing the canonical form [`semantic_eq`] compares with `==`
/// (and then re-walks via [`ids_compatible`]). Uses
/// [`Expression::map_children`] for the
/// default recursion so every current and future `Expression` variant is
/// covered without a hand-enumerated match; infallible (`Result<_,
/// Infallible>`), so the `unwrap_or_else` below can never panic.
///
/// `ColumnReference::expr_id` is preserved (only
/// qualifier/name-case are normalized away here — `data_type`/`nullable` are
/// carried through unchanged too, since [`ColumnReference`]'s hand-written
/// `PartialEq` already excludes them from `==`) — [`semantic_eq`]'s
/// `==` check cannot see `expr_id` either (`ColumnReference::eq` excludes
/// it), but [`ids_compatible`] reads `expr_id` directly off these
/// canonicalized trees. Dropping this preservation (e.g. re-normalizing
/// `expr_id` to `None` here) would silently revert the [`ids_compatible`]
/// check to always-`true` — `canonicalize_for_semantic_eq_preserves_expr_id`
/// guards against exactly that regression.
fn canonicalize_for_semantic_eq(expr: &Expression) -> Expression {
    if let Expression::Alias(a) = expr {
        return canonicalize_for_semantic_eq(&a.expr);
    }
    // Implicit casts are analyzer materialization and are transparent to
    // Spark's semantic equality.
    if let Expression::Cast(c) = expr {
        if c.implicit {
            return canonicalize_for_semantic_eq(&c.expr);
        }
    }
    let normalized = match expr {
        // Names are case-folded and qualifiers are stripped; type, nullability,
        // and identity remain attached to the reference.
        Expression::ColumnReference(c) => Expression::ColumnReference(ColumnReference {
            name: fold_key(&c.name),
            qualifier: None,
            data_type: c.data_type.clone(),
            nullable: c.nullable,
            expr_id: c.expr_id,
        }),
        Expression::UnresolvedColumn(u) => Expression::UnresolvedColumn(UnresolvedColumn {
            name: fold_key(&u.name),
            qualifier: None,
            plan_id: None,
        }),
        other => other.clone(),
    };
    normalized
        .map_children(|c| Ok::<_, std::convert::Infallible>(canonicalize_for_semantic_eq(&c)))
        .unwrap_or_else(|e: std::convert::Infallible| match e {})
}

/// Analyze an embedded subquery's inner plan. Builds an [`OuterScope`] from
/// the enclosing [`ResolveContext`] so that correlated references to columns
/// present ONLY in the outer plan's schema resolve correctly (tier (g) in
/// [`resolve_column`]). The outer scope REPLACES (does not chain onto)
/// whatever outer the enclosing context had — a doubly-nested subquery's
/// inner plan only ever sees its immediate parent's scope, never a
/// grandparent's.
///
/// An already-`Analyzed` plan is returned unchanged (idempotent).
fn analyze_subquery_plan(
    plan: SubqueryPlan,
    ctx: &ResolveContext,
) -> Result<TypedAst, AnalyzerError> {
    match plan {
        SubqueryPlan::Unanalyzed(inner) => {
            let enclosing_outer = OuterScope {
                schema: ctx.schema,
                scopes: &ctx.scopes,
            };
            analyze_node(*inner, ctx.base_types, Some(enclosing_outer))
        }
        SubqueryPlan::Analyzed(inner) => Ok(*inner),
    }
}

/// Analyze a subquery's inner plan and enforce the shared single-column
/// contract of `ScalarSubquery` / `InSubquery`. `error_reason` carries the
/// caller's Spark-emulated message verbatim (the two subquery kinds phrase it
/// differently).
fn analyze_single_column_subquery(
    plan: SubqueryPlan,
    ctx: &ResolveContext,
    error_reason: &str,
) -> Result<SubqueryPlan, AnalyzerError> {
    let inner = analyze_subquery_plan(plan, ctx)?;
    if inner.resolved_schema.fields.len() != 1 {
        return Err(AnalyzerError::SparkEmulated {
            class: "INVALID_SUBQUERY_EXPRESSION.SCALAR_SUBQUERY_RETURN_MORE_THAN_ONE_OUTPUT_COLUMN",
            reason: error_reason.to_owned(),
        });
    }
    Ok(SubqueryPlan::Analyzed(Box::new(inner)))
}

/// Synthetic qualifier attached to plan_id-tagged column refs during Join
/// condition analysis. Emission renders `ColumnReference { qualifier:
/// Some(TD_JOIN_LEFT), .. }` as `__td_jl.<col>`, which matches the
/// left/right subquery aliases `render_join` emits.
pub(crate) const TD_JOIN_LEFT: &str = "__td_jl";
pub(crate) const TD_JOIN_RIGHT: &str = "__td_jr";

//
// A relation alias (`d` in `... CROSS JOIN dept d`) is not a schema field —
// the merged join schema (built by `ResolvedSchema::merge`, a positional
// concatenation) carries no per-field qualifier metadata. But `merge` and
// `apply_join_nullability` both preserve field count and order, so each
// source relation occupies a CONTIGUOUS range of the CURRENT resolution
// schema at every nesting level (already outer-join-flip-correct).
// The stamped [`RelScope`] maps each alias / table name to that range;
// `resolve_column`'s qualifier arm restricts a name-only lookup to
// `schema.fields[range]` when the qualifier binds exactly one scope — so
// `d.dept_id` resolves against dept's own fields instead of a first-match-by-
// name scan that can silently pick the wrong side's (wrongly typed / wrongly
// nullable) column.

/// The enclosing (parent) plan's resolution schema, threaded into subquery
/// analysis so a correlated outer reference (e.g. `e.salary` inside
/// `SELECT d.dept_id FROM dept d WHERE d.budget > e.salary`) resolves
/// against the parent plan's schema when the inner plan has no match.
///
/// Deliberately has NO `outer` field: a doubly-nested subquery's inner plan
/// only ever sees its immediate parent's scope, never a grandparent's.
/// This makes multi-level correlation (which Spark itself rejects)
/// unrepresentable by construction.
#[derive(Debug, Clone, Copy)]
struct OuterScope<'a> {
    schema: &'a ResolvedSchema,
    scopes: &'a RelScope,
}

/// The schema + alias-scope bindings a column reference resolves against.
/// Threaded through `resolve_and_stamp` / `resolve_column` / `resolve_expr_list`
/// / `resolve_boolean_predicate` in place of the bare `&StructType` they
/// previously took.
#[derive(Debug)]
struct ResolveContext<'a> {
    /// The current operator's resolution schema (already outer-join-flipped
    /// and positionally merged, per [`apply_join_nullability`] /
    /// [`ResolvedSchema::merge`]).
    schema: &'a ResolvedSchema,
    /// Alias/table-name → field-range bindings the current node resolves
    /// against — the input's stamped [`RelScope`], borrowed in the common
    /// case; owned only when composed (join conditions bind both sides plus
    /// the synthetic whole-side qualifiers). Empty when there is no
    /// join/table-alias structure to bind (e.g. `Values` rows,
    /// table-valued-function args).
    scopes: std::borrow::Cow<'a, RelScope>,
    base_types: &'a BaseTypes,
    /// The enclosing plan's scope for correlated subquery resolution.
    /// `Some` when this context is analyzing a subquery's inner plan;
    /// `None` at the top level.
    outer: Option<OuterScope<'a>>,
}

impl<'a> ResolveContext<'a> {
    /// Resolve against a unary operator's already-typed `input` — the common
    /// case (Project / Filter / Sort / Aggregate / WithColumns / SampleBy /
    /// ... all resolve against their child's resolved schema).
    fn of_input(
        input: &'a TypedAst,
        base_types: &'a BaseTypes,
        outer: Option<OuterScope<'a>>,
    ) -> Self {
        Self {
            schema: &input.resolved_schema,
            scopes: std::borrow::Cow::Borrowed(&input.scope),
            base_types,
            outer,
        }
    }

    /// Resolve a join condition against the merged `left ⋈ right` schema.
    /// Composition is [`merge_join_scopes`], with no synthetic whole-side
    /// aliases:
    /// this composite scope exists only to offset-merge the two sides' own
    /// aliases/plan_ids into one range space; a plan_id ref resolves
    /// bare+ordinal via [`resolve_column`]'s plan_id arm exactly as it would
    /// above the join.
    ///
    /// Always [`RightSide::Keep`], unlike [`RelScope::of`]: the condition
    /// resolves against the full merged schema regardless of join type
    /// (SEMI/ANTI included), since Spark's own resolution runs the fold over
    /// both children irrespective of join type.
    fn for_join_condition(
        schema: &'a ResolvedSchema,
        left: &'a TypedAst,
        right: &'a TypedAst,
        left_plan_ids: &[i64],
        right_plan_ids: &[i64],
        base_types: &'a BaseTypes,
        outer: Option<OuterScope<'a>>,
    ) -> Self {
        Self {
            schema,
            scopes: std::borrow::Cow::Owned(merge_join_scopes(
                left,
                right,
                left_plan_ids,
                right_plan_ids,
                RightSide::Keep,
            )),
            base_types,
            outer,
        }
    }

    /// Resolve against a bare schema with no alias-scope structure (e.g.
    /// `Values` rows, table-valued-function args against an empty schema).
    fn bare(schema: &'a ResolvedSchema, base_types: &'a BaseTypes) -> Self {
        Self {
            schema,
            scopes: std::borrow::Cow::Owned(RelScope::default()),
            base_types,
            outer: None,
        }
    }

    /// Look up `q`'s alias-scope range, guarding it against the current
    /// schema's length. [`RelScope::lookup`] only ever binds ranges
    /// within the schema they were built from, so an out-of-bounds range is
    /// an analyzer invariant violation — surface it loudly in debug builds,
    /// but degrade to `None` (the caller's legacy fallback) in release
    /// rather than panicking on the index. Shared by the synthetic
    /// `__td_jl`/`__td_jr` join-qualifier arm and tier (e) in
    /// `resolve_column`, so both get the identical guard.
    fn scoped_range(&self, q: &str) -> Option<std::ops::Range<usize>> {
        self.scopes.lookup(q).filter(|range| {
            let in_bounds = range.end <= self.schema.len();
            debug_assert!(
                in_bounds,
                "qualifier `{q}` scope range {range:?} exceeds schema of {} fields",
                self.schema.len()
            );
            in_bounds
        })
    }

    /// Look up a plan_id's field range, with the same out-of-bounds guard as
    /// [`scoped_range`](Self::scoped_range).
    fn plan_id_lookup(&self, pid: i64) -> Option<std::ops::Range<usize>> {
        self.scopes.lookup_plan_id(pid).filter(|range| {
            let in_bounds = range.end <= self.schema.len();
            debug_assert!(
                in_bounds,
                "plan_id `{pid}` scope range {range:?} exceeds schema of {} fields",
                self.schema.len()
            );
            in_bounds
        })
    }
}

/// Build an `ExtractValue` chain rooted at the top-level struct attribute,
/// preserving that attribute's identity, type, and nullability. Shared by
/// local and correlated struct-field resolution.
///
/// `expr_id`: `root` IS the real top-level attribute this reference names,
/// so its id is stamped directly on the chain's root ref — matching Spark's
/// `GetStructField`-child-`exprId` model (the child ref names the whole
/// column and keeps its identity; only the synthesized `ExtractValue` nodes
/// above it are id-less, same as `GetStructField` itself).
fn build_struct_extract_chain(root: &Attribute, field_path: &str) -> Expression {
    let mut expr = Expression::ColumnReference(ColumnReference::from_attr(root));
    for seg in field_path.split('.') {
        expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(expr),
            extraction: Box::new(Expression::Literal(Literal {
                value: LiteralValue::String(seg.to_owned()),
                data_type: DataType::String,
            })),
        });
    }
    expr
}

/// Attempt to resolve an [`UnresolvedColumn`] against the outer (enclosing)
/// plan's scope. Used as a final fallback (tier (g)) in [`resolve_column`]
/// when ALL inner tiers have failed — i.e. the column name+qualifier does not
/// match anything in the inner plan's schema.
///
/// **Qualifier-strict**: a qualified reference (`e.salary`) only resolves if
/// the qualifier binds a scope in the outer context (struct-column precedence
/// first, then relation-alias scope). An unqualified reference resolves only
/// when exactly one case-insensitive match exists across the outer schema's
/// fields (zero or 2+ matches yield `None`).
///
/// Returns a fully-built expression on a hit. Correlated references retain
/// their qualifier and the matched outer attribute's identity; process-wide
/// ids keep outer and inner attributes distinct.
fn resolve_in_outer(u: &UnresolvedColumn, outer: OuterScope<'_>) -> Option<Expression> {
    if let Some(q) = u.qualifier.as_deref() {
        // Struct-column precedence in the outer schema (matches resolve_column's
        // existing tier ordering) — same `ExtractValue`-chain construction as
        // the local tier-(d) twin, rooted at the OUTER struct column's own
        // attribute.
        if TypeInferenceEngine::struct_qualifier_info(&u.name, q, outer.schema).is_some() {
            let root = outer.schema.field_by_name(q)?;
            return Some(build_struct_extract_chain(root, &u.name));
        }
        // Relation-alias scope in the outer context.
        let range = outer.scopes.lookup(q)?;
        // Guard against out-of-bounds (same pattern as ResolveContext::scoped_range).
        if range.end > outer.schema.fields.len() {
            return None;
        }
        let slice = &outer.schema.fields[range.clone()];
        let (dt, nullable, attr) = TypeInferenceEngine::resolve_in(&u.name, slice)?;
        // `attr` borrows directly from `outer.schema.fields` (via `slice`, a
        // sub-slice of it), so its `expr_id` needs no `range.start +`
        // re-basing — it already names the right field wherever it lives.
        Some(Expression::ColumnReference(ColumnReference {
            name: u.name.clone(),
            qualifier: u.qualifier.clone(),
            data_type: dt,
            nullable,
            expr_id: Some(attr.expr_id),
        }))
    } else {
        // Unqualified: exactly-one case-insensitive match in the outer schema.
        let mut found: Option<&Attribute> = None;
        for f in &outer.schema.fields {
            if eq_fold(&f.name, &u.name) {
                if found.is_some() {
                    // 2+ matches — ambiguous; do not silently pick one.
                    return None;
                }
                found = Some(f);
            }
        }
        found.map(|f| {
            // `name`/`qualifier` come from the REFERENCE, not the attribute:
            // the match was case-insensitive, so the user's spelling is what
            // must reach emission.
            Expression::ColumnReference(ColumnReference {
                name: u.name.clone(),
                qualifier: u.qualifier.clone(),
                ..ColumnReference::from_attr(f)
            })
        })
    }
}

fn resolve_column(u: UnresolvedColumn, ctx: &ResolveContext) -> Result<Expression, AnalyzerError> {
    // Dotted struct paths become ExtractValue chains; emitting the dotted tail
    // as one identifier would make DuckDB reject the reference.
    // Unqualified: surface `AmbiguousColumn` whenever more than one field
    // (case-insensitive match, matching `field_by_name`'s Spark-compatible
    // rule) resolves. This is the single, central ambiguity check point —
    // it catches ambiguity everywhere a column reference is resolved
    // (projections, filters, sort keys, join conditions, ...), not just in
    // join conditions.
    // Synthetic join qualifiers are reserved for emission and never resolve
    // as user qualifiers.
    let is_synthetic_join_qualifier = matches!(
        u.qualifier.as_deref(),
        Some(TD_JOIN_LEFT) | Some(TD_JOIN_RIGHT)
    );
    // When the ref is unqualified but carries a plan_id that maps to a
    // known join side, restrict resolution to that side's field range.
    // The condition and above-join paths now share this arm: `resolve_column`
    // is the single unification point, reached whether the plan_id-tagged
    // ref lives in the join's own condition (via
    // `ResolveContext::for_join_condition` binding the join's own
    // `left_plan_ids`/`right_plan_ids`) or above the join. Fall through to
    // legacy behavior when the plan_id is unknown (e.g. ref to a non-join
    // child — Spark tolerates refs resolvable unambiguously without it).
    if u.qualifier.is_none() {
        if let Some(pid) = u.plan_id {
            // An unaliased self-join reusing a plan_id is ambiguous.
            // Checked BEFORE `plan_id_lookup`'s first-match so we raise
            // `AmbiguousColumnReference` instead of silently binding the left
            if ctx.scopes.plan_id_is_ambiguous(pid) {
                return Err(AnalyzerError::AmbiguousColumnReference {
                    name: u.name.clone(),
                });
            }
            if let Some(range) = ctx.plan_id_lookup(pid) {
                let info = TypeInferenceEngine::resolve_in(&u.name, &ctx.schema.fields[range]);
                if let Some((dt, nullable, attr)) = info {
                    // The attribute is borrowed from the schema slice, so its
                    // identity needs no re-basing.
                    let expr_id = Some(attr.expr_id);
                    // Plan-id references are bare and bind in emission by
                    // attribute identity, even when names are duplicated.
                    return Ok(Expression::ColumnReference(ColumnReference {
                        name: u.name,
                        qualifier: None,
                        data_type: dt,
                        nullable,
                        expr_id,
                    }));
                }
                // plan_id scope found but column name not in that range —
                // UnknownColumn, not ambiguity.
                return Err(AnalyzerError::UnknownColumn {
                    name: u.name,
                    qualifier: u.qualifier,
                });
            }
            // plan_id unknown in scope map — fall through to legacy
            // resolution (the ref may be unambiguous without it).
        }
        let matches: Vec<&Attribute> = ctx
            .schema
            .fields
            .iter()
            .filter(|f| eq_fold(&f.name, &u.name))
            .collect();
        if matches.len() > 1 {
            let candidates = matches.iter().map(|f| f.name.clone()).collect();
            return Err(AnalyzerError::AmbiguousColumn {
                name: u.name,
                candidates,
            });
        }
    }
    if is_synthetic_join_qualifier {
        // Reserved emission-namespace qualifiers are not user-bindable.
        return Err(AnalyzerError::UnknownColumn {
            name: u.name,
            qualifier: u.qualifier,
        });
    }
    let (dt, nullable, expr_id) = if let Some(q) = u.qualifier.as_deref() {
        // A qualifier naming a top-level struct takes precedence over a
        // relation alias.
        // Both single- and multi-level paths become chains rooted at the
        // struct attribute, preserving identity for emission.
        if TypeInferenceEngine::struct_qualifier_info(&u.name, q, ctx.schema).is_some() {
            let root = ctx.schema.field_by_name(q).expect(
                "struct_qualifier_info matched q as a top-level struct column on this same schema",
            );
            return Ok(build_struct_extract_chain(root, &u.name));
        } else {
            match ctx.scoped_range(q) {
                // (e) qualifier binds exactly one in-bounds relation scope —
                // restrict the name-only lookup to that relation's own
                // fields. A miss here means `q` is a real, unambiguous
                // relation but `name` does not exist on it: a Spark-emulated
                // `UnknownColumn` (ADR-022 cat-1), not an opaque DuckDB bind
                // error.
                Some(range) => {
                    match TypeInferenceEngine::resolve_in(&u.name, &ctx.schema.fields[range]) {
                        Some((dt, nullable, attr)) => {
                            // The attribute is borrowed from the schema slice;
                            // its identity needs no rebase.
                            let expr_id = Some(attr.expr_id);
                            // A uniquely named projected column can bind bare
                            // by identity; duplicated names retain the
                            // qualifier for disambiguation.
                            let name_count = ctx
                                .schema
                                .fields
                                .iter()
                                .filter(|f| eq_fold(&f.name, &u.name))
                                .count();
                            if name_count == 1 {
                                return Ok(Expression::ColumnReference(ColumnReference {
                                    name: u.name,
                                    qualifier: None,
                                    expr_id,
                                    data_type: dt,
                                    nullable,
                                }));
                            }
                            (dt, nullable, expr_id)
                        }
                        None => {
                            return Err(AnalyzerError::UnknownColumn {
                                name: u.name.clone(),
                                qualifier: u.qualifier.clone(),
                            });
                        }
                    }
                }
                // (f) qualifier binds no in-bounds scope. Two cases collapse
                // to this arm: `q` binds 2+ ranges (a duplicate alias / bare
                // table name on both sides of a join — `RelScope::lookup`
                // collapses 2+ matches to `None`, same as 0), or `q` binds no
                // scope at all (for example, USING joins). Distinguish them
                // via `lookup_all` — 2+ is genuinely
                // ambiguous (Spark: AMBIGUOUS_REFERENCE) and must not fall
                // through to the permissive legacy name-only lookup; 0 is
                // resolved below.
                None => {
                    let matches = ctx.scopes.lookup_all(q);
                    if matches.len() > 1 {
                        let candidates = (0..matches.len())
                            .map(|i| format!("{q}#{i}.{}", u.name))
                            .collect();
                        return Err(AnalyzerError::AmbiguousColumn {
                            name: u.name.clone(),
                            candidates,
                        });
                    }
                    // With no scope hit, consult each field's own lineage.
                    let hits: Vec<usize> = ctx
                        .schema
                        .fields
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| {
                            eq_fold(&f.name, &u.name)
                                && f.source_quals.iter().any(|qq| eq_fold(qq, q))
                        })
                        .map(|(i, _)| i)
                        .collect();
                    match hits.len() {
                        1 => {
                            // Projected-through (F10): resolve by
                            // attribute identity and DROP the qualifier
                            // so emission renders the bare column, which
                            // binds by that identity over any wrapper —
                            // no strand.
                            let k = hits[0];
                            // `name` from the REFERENCE (the `source_quals`
                            // match was case-insensitive); everything else
                            // copied from the resolved attribute.
                            return Ok(Expression::ColumnReference(ColumnReference {
                                name: u.name,
                                ..ColumnReference::from_attr(&ctx.schema.fields[k])
                            }));
                        }
                        n if n >= 2 => {
                            return Err(AnalyzerError::AmbiguousColumn {
                                name: u.name.clone(),
                                candidates: hits
                                    .iter()
                                    .map(|&i| ctx.schema.fields[i].name.clone())
                                    .collect(),
                            });
                        }
                        // 0 hits: NOT projected-through. Degrade to
                        // Unresolved so the shared tier-(g) tail below tries
                        // the OUTER scope (correlation, tbl-005/sq-*) and
                        // otherwise raises UnknownColumn (F8) — NO
                        // permissive name-only fallback here.
                        _ => (DataType::Unresolved, false, None),
                    }
                }
            }
        }
    } else {
        let (dt, nullable, attr) =
            TypeInferenceEngine::qualified_resolve_in(&u.name, None, ctx.schema);
        // ADR-024: same attribute the lookup resolved.
        let expr_id = attr.map(|a| a.expr_id);
        (dt, nullable, expr_id)
    };
    if matches!(dt, DataType::Unresolved) {
        // After local resolution fails, try the immediate outer scope for a
        // correlated reference. Preserve the outer attribute's identity.
        if let Some(outer) = ctx.outer {
            if let Some(expr) = resolve_in_outer(&u, outer) {
                return Ok(expr);
            }
        }
        return Err(AnalyzerError::UnknownColumn {
            name: u.name,
            qualifier: u.qualifier,
        });
    }
    Ok(Expression::ColumnReference(ColumnReference {
        name: u.name,
        qualifier: u.qualifier,
        data_type: dt,
        nullable,
        expr_id,
    }))
}

/// Return `true` iff any `Expression::UnresolvedColumn` (or
/// `Expression::UnresolvedRegex`) remains, or an embedded subquery's inner
/// plan is not yet `Analyzed`. A `ColumnReference` is always fully resolved
/// by construction (E4/D3: `data_type`/`nullable` are non-`Option`) — it
/// falls into the default recursion below.
fn expression_is_fully_resolved(expr: &Expression) -> bool {
    // Opaque expression bodies are already resolved by their enclosing
    // expression and are not re-derived here.
    if expr.is_opaque_unit() {
        return true;
    }
    match expr {
        Expression::UnresolvedColumn(_) => false,
        // Regex projections should have expanded before this check.
        Expression::UnresolvedRegex(_) => false,
        // Subquery bodies must be analyzed: the inner plan is fully resolved
        // only once the analyzer has rewritten `Unanalyzed` → `Analyzed`.
        Expression::ScalarSubquery(s) => subquery_plan_is_resolved(&s.subquery),
        Expression::InSubquery(i) => {
            expression_is_fully_resolved(&i.expr) && subquery_plan_is_resolved(&i.subquery)
        }
        Expression::ExistsSubquery(e) => subquery_plan_is_resolved(&e.subquery),
        // Default recursion: all-children-resolved implies self-resolved. See
        // [`Expression::children`] for the walked-child convention. Covers
        // the leaf variants — Literal, Star, and ColumnReference (whose
        // `data_type`/`nullable` are non-`Option` by construction since
        // E4/D3, so it is vacuously resolved via its empty child set) — and
        // every structural node (Binary, Window, CaseWhen, …).
        _ => expr.children().all(expression_is_fully_resolved),
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

fn schema_has_unresolved(schema: &ResolvedSchema) -> bool {
    schema
        .fields
        .iter()
        .any(|f| f.data_type.contains_unresolved())
}

/// Build a projection output attribute. A bare reference matching an input
/// identity copies that attribute and merges any stamped qualifier; aliases
/// and computed expressions mint a fresh identity with empty lineage.
fn output_attribute(e: &Expression, input: &ResolvedSchema) -> Attribute {
    if let Expression::ColumnReference(cr) = e {
        if let Some(id) = cr.expr_id {
            if let Some((_, src)) = input.field_by_id(id, &cr.name) {
                let mut attr = src.clone();
                attr.name = expression_output_name(e);
                attr.data_type = e.data_type(input);
                attr.nullable = e.nullable(input);
                if let Some(q) = &cr.qualifier {
                    attr.source_quals.insert(q.clone());
                }
                return attr;
            }
        }
    }
    Attribute::minted(
        expression_output_name(e),
        e.data_type(input),
        e.nullable(input),
    )
}

fn project_output_schema(
    projections: &[Expression],
    input: &TypedAst,
) -> Result<ResolvedSchema, AnalyzerError> {
    let input_schema = &input.resolved_schema;
    let mut fields: Vec<Attribute> = Vec::with_capacity(projections.len());
    for expr in projections {
        match expr {
            Expression::Star(s) => {
                // Star: expand at schema level. Qualified star: filter by
                // struct field / qualifier (τ's analyzer keeps it simple —
                // qualifier match against field name).
                match &s.qualifier {
                    None => {
                        // Unqualified `*`: the SAME columns, just re-listed —
                        // clone carries each attribute's id forward.
                        fields.extend(input_schema.fields.iter().cloned());
                    }
                    Some(q) => {
                        // If the qualifier matches a struct field, expand
                        // that struct's inner fields. These inner fields
                        // never existed as top-level output columns before —
                        // mint fresh ids for them.
                        if let Some(f) = input_schema.field_by_name(q) {
                            if let DataType::Struct(st) = &f.data_type {
                                let base_nullable = f.nullable;
                                for inner in &st.fields {
                                    fields.push(Attribute::minted(
                                        inner.name.clone(),
                                        inner.data_type.clone(),
                                        base_nullable || inner.nullable,
                                    ));
                                }
                                continue;
                            }
                        }
                        // Table-qualified star (`emp.*` / `e.*`): expand to
                        // the qualifier's bound field RANGE from the input's
                        // stamped RelScope — the same contiguous-range
                        // authority `resolve_column`'s qualifier arm trusts.
                        // This covers every shape whose scope exposes `q`
                        // (aliased relations, bare scans, either side of a
                        // plain multi-relation join, through the
                        // scope-passthrough operator class); emission keeps
                        // the alias visible in exactly those shapes, and its
                        // `q.*` slot returns that relation's columns in the
                        // same range order. USING joins stay excluded
                        // automatically (their RelScope is empty), as are
                        // ambiguous duplicates (`lookup` bails on 2+
                        // matches). These are the SAME columns re-listed —
                        // clone carries each attribute's id forward.
                        if let Some(range) = input.scope.lookup(q) {
                            debug_assert!(range.end <= input_schema.len());
                            if range.end <= input_schema.len() {
                                fields.extend(input_schema.fields[range].iter().cloned());
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
            other => fields.push(output_attribute(other, input_schema)),
        }
    }
    Ok(ResolvedSchema::new(fields))
}

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
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    let input_schema = &typed_input.resolved_schema;

    // OPT-1: build a lowercase-keyed lookup once (case-insensitive per Spark
    // identifier semantics). Turns O(V·F) name resolution across the id and
    // value lists — plus the empty-values fallback's O(F·I) filter — into
    // O(F + I + V) total.
    let field_index: HashMap<String, &Attribute> = input_schema
        .fields
        .iter()
        .map(|f| (fold_key(&f.name), f))
        .collect();
    let find_field =
        |name: &str| -> Option<&Attribute> { field_index.get(&fold_key(name)).copied() };

    // Reject any unresolvable column name with a Spark-emulated
    // `UnknownColumn`. Call ORDER carries the Spark error precedence: the
    // Implicit path validates the values BEFORE deriving ids from them; the
    // Explicit path validates the ids first, so an unresolvable id wins over
    // an unresolvable value there.
    let validate_names_resolve = |names: &[String]| -> Result<(), AnalyzerError> {
        for n in names {
            if find_field(n).is_none() {
                return Err(AnalyzerError::UnknownColumn {
                    name: n.clone(),
                    qualifier: None,
                });
            }
        }
        Ok(())
    };
    // Input-schema fields (input order) minus a lowercased exclusion set —
    // shared by the Implicit-ids derivation and the empty-values fallback.
    let fields_minus = |excluded: &HashSet<String>| -> Vec<String> {
        input_schema
            .fields
            .iter()
            .filter(|f| !excluded.contains(&fold_key(&f.name)))
            .map(|f| f.name.clone())
            .collect()
    };

    // Resolve the id list. The DataFrame path supplies ids explicitly; SQL
    // `UNPIVOT` supplies only value columns, so the analyzer derives the ids as
    // `input schema − value columns` (input order) per Spark parity (ADR-015).
    let ids_are_implicit = matches!(ids, UnpivotIds::Implicit);
    let ids: Vec<String> = match ids {
        UnpivotIds::Explicit(v) => v,
        UnpivotIds::Implicit => {
            if values.is_empty() {
                // Defensive only — unreachable through either front end.
                // `UnpivotIds::Implicit` is produced solely by the SQL path
                // (`v2_lowering::lower_table_factor`); the DataFrame converter
                // always emits `Explicit`. And sqlparser's
                // `parse_unpivot_table_factor` parses the IN list with
                // `allow_empty: false`, so `UNPIVOT (v FOR k IN ())` is
                // rejected at parse time ("Expected: an expression, found: )")
                // and `values` can never arrive empty. Reaching here means a
                // τ invariant broke, not that the user sent bad SQL — hence
                // Internal, not Spark-emulated.
                return Err(AnalyzerError::Internal {
                    reason: "unpivot with implicit ids reached the analyzer with an empty \
                             value list; the SQL parser should have rejected it"
                        .to_owned(),
                });
            }
            // Validate each value column resolves before deriving ids from it.
            validate_names_resolve(&values)?;
            let value_set: HashSet<String> = values.iter().map(|v| fold_key(v)).collect();
            fields_minus(&value_set)
        }
    };

    // Validate every id column resolves.
    validate_names_resolve(&ids)?;

    // Materialise `values`: empty ⇒ all non-id input columns (Spark default).
    let materialised_values: Vec<String> = if values.is_empty() {
        let id_set: HashSet<String> = ids.iter().map(|id| fold_key(id)).collect();
        fields_minus(&id_set)
    } else {
        // Validate each named value column resolves — the Implicit path
        // already did (see above), so only the Explicit path checks here.
        if !ids_are_implicit {
            validate_names_resolve(&values)?;
        }
        values
    };

    if materialised_values.is_empty() {
        return Err(AnalyzerError::SparkEmulated {
            class: "UNPIVOT_REQUIRES_VALUE_COLUMNS",
            reason:
                "unpivot requires at least one value column (none supplied and no non-id columns)"
                    .to_owned(),
        });
    }

    // Spark permits overlapping id/value names and duplicate output names;
    // only ambiguous references are rejected.

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
    // The id columns pass through unchanged — COPY their existing identity.
    // `variable_column_name` / `value_column_name` are brand-new synthetic
    // columns — MINT fresh ids.
    let mut output_fields: Vec<Attribute> = Vec::with_capacity(ids.len() + 2);
    for id in &ids {
        let f = find_field(id).expect("id column resolved above");
        output_fields.push(f.clone());
    }
    output_fields.push(Attribute::minted(
        variable_column_name.clone(),
        DataType::String,
        false,
    ));
    output_fields.push(Attribute::minted(
        value_column_name.clone(),
        widened_type,
        widened_nullable,
    ));
    let output_schema = ResolvedSchema::new(output_fields);

    Ok(TypedAst::new(
        TypedOp::Unpivot {
            input: Box::new(typed_input),
            ids,
            values: materialised_values,
            variable_column_name,
            value_column_name,
        },
        output_schema,
    ))
}

/// Build the shared output schema for `describe` / `summary`: a `summary`
/// STRING NOT NULL column followed by one STRING NULLABLE column per
/// materialised input col. Per-col stats can produce NULL (`TRY_CAST` on a
/// non-numeric col returns NULL) so every stat column is nullable.
fn build_stats_output_schema(cols: &[String]) -> ResolvedSchema {
    let mut fields: Vec<Attribute> = Vec::with_capacity(cols.len() + 1);
    // Spark stamps `summary` as nullable=true even though every value is a
    // string literal — see `Dataset.summary()` output schema. Spark parity
    // (ADR-015: schema oracle wins) requires we match, not the STRING NOT
    // NULL that the emission's `'count'` literal would justify.
    // Every column here is a brand-new synthesized output column — MINT.
    fields.push(Attribute::minted("summary", DataType::String, true));
    for c in cols {
        fields.push(Attribute::minted(c.clone(), DataType::String, true));
    }
    ResolvedSchema::new(fields)
}

/// Materialise a caller-supplied `cols` list against `input_schema`:
///   - empty ⇒ all input columns in schema order (Spark default);
///   - non-empty ⇒ each name resolves case-insensitively or
///     [`AnalyzerError::UnknownColumn`] is returned. The output preserves the
///     caller's casing (Spark parity).
fn materialise_stats_cols(
    cols: Vec<String>,
    input_schema: &ResolvedSchema,
) -> Result<Vec<String>, AnalyzerError> {
    if cols.is_empty() {
        Ok(input_schema.fields.iter().map(|f| f.name.clone()).collect())
    } else {
        let lowercase_names: HashSet<String> = input_schema
            .fields
            .iter()
            .map(|f| fold_key(&f.name))
            .collect();
        for c in &cols {
            if !lowercase_names.contains(&fold_key(c)) {
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
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    let materialised = materialise_stats_cols(cols, &typed_input.resolved_schema)?;
    let output_schema = build_stats_output_schema(&materialised);
    Ok(TypedAst::new(
        TypedOp::Describe {
            input: Box::new(typed_input),
            cols: materialised,
        },
        output_schema,
    ))
}

/// Analyze `CommonOp::Summary`: resolve the input, materialise the full
/// column list from the input schema (proto `StatSummary` has no `cols`
/// field), and materialise the statistics list (empty ⇒
/// [`DEFAULT_SUMMARY_STATS`]).
fn analyze_summary(
    input: CommonAst,
    statistics: Vec<String>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
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
    Ok(TypedAst::new(
        TypedOp::Summary {
            input: Box::new(typed_input),
            cols: materialised_cols,
            statistics: materialised_stats,
        },
        output_schema,
    ))
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
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    let materialised = materialise_stats_cols(cols, &typed_input.resolved_schema)?;
    // Every output column here is a brand-new synthesized `ARRAY<T>` column
    // (not the source column itself) — MINT.
    let output_fields: Vec<Attribute> = materialised
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
            Attribute::minted(
                format!("{c}_freqItems"),
                DataType::Array(Box::new(src.data_type.clone()), true),
                false,
            )
        })
        .collect();
    Ok(TypedAst::new(
        TypedOp::FreqItems {
            input: Box::new(typed_input),
            cols: materialised,
            support,
        },
        ResolvedSchema::new(output_fields),
    ))
}

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
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    // Thunderduck-boundary (ADR-022): implicit pivot values require an
    // eager DISTINCT query against DuckDB (Spark's Analyzer does this
    // eagerly). τ's analyzer has no session hook — implementing
    // it needs the base_types overlay extended with a value-query closure.
    // Reject loudly rather than stamping an incorrect schema.
    if pivot_values.is_empty() {
        return Err(AnalyzerError::PuntedOperator {
            op: "Pivot[implicit-values]".to_owned(),
            reason:
                "pivot without explicit values requires eager DISTINCT query; τ needs a session-injected value-discovery hook"
                    .to_owned(),
        });
    }
    let typed_input = analyze_node(input, base_types, outer)?;
    let input_schema = &typed_input.resolved_schema;
    let ctx = ResolveContext::of_input(&typed_input, base_types, outer);
    // Resolve the pivot column and aggregates first: the implicit-grouping
    // derivation needs to know which columns the aggregates reference.
    let pivot_column = resolve_and_stamp(pivot_column, &ctx)?;
    let aggregates = resolve_expr_list(aggregates, &ctx)?;
    // The DataFrame path supplies grouping explicitly (from `groupBy`); SQL
    // `PIVOT` supplies none, so the analyzer derives it as
    // `input schema − pivot column − aggregate-referenced columns`, in input
    // order, per Spark parity (ADR-015).
    let grouping = match grouping {
        PivotGrouping::Explicit(g) => resolve_expr_list(g, &ctx)?,
        PivotGrouping::Implicit => {
            derive_implicit_grouping(input_schema, &pivot_column, &aggregates)
        }
    };
    // Wrap every computed grouping entry as a named `Alias`; `grouping` is
    // Pivot's output-list prefix. `pivot_column` /
    // `pivot_values` / `aggregates` are NOT wrapped: they own their naming
    // (the pivot-value × aggregate loop below derives output names directly
    // from them; emission consumes `aggregates` via `.unaliased()`).
    // `derive_implicit_grouping` produces bare references, so this is a no-op
    // for implicit grouping.
    let grouping: Vec<Expression> = grouping.into_iter().map(ensure_named).collect();
    // Pivot values are literals; they only need type resolution against the
    // pivot column (Spark coerces them into that type at read). We defer
    // typing to the emission stage — literals carry their own type already.
    let pivot_values = resolve_expr_list(pivot_values, &ctx)?;

    // A NULL pivot value is a legitimate bucket, not an error. Spark's
    // `PivotTransformer` only rejects *non-foldable* pivot value expressions
    // (`NON_LITERAL_PIVOT_VALUES`); a `Literal(null)` is foldable and yields a
    // column named `"null"` (its `outputName` casts the value to string and
    // falls back to `"null"`). Both Spark pivot overloads accept it — the
    // values-less overload discovers it via `select(col).distinct()` (which
    // does not null-filter), and the explicit overload takes it verbatim. See
    // `literal_to_pivot_column_name` for the `"null"` naming.

    // Build the output schema. Grouping columns come first, verbatim.
    let mut output_fields: Vec<Attribute> = Vec::new();
    output_fields.extend(grouping.iter().map(|g| output_attribute(g, input_schema)));

    // `pivot_values` is non-empty by construction — the punt at the top of
    // this fn rejects an empty list, `resolve_expr_list` preserves length,
    // and the connect-server resolves implicit pivots to explicit values
    // before `analyze` runs. Stamp one output column per (pivot_value,
    // aggregate) pair per Spark.
    // **Nullability:** pivot output columns are always nullable per Spark —
    // a given pivot bucket may be empty for a particular group, in which
    // case the aggregate cell materialises as NULL (verified by the
    // grp-004 differential test). Ignore the aggregate's intrinsic
    // nullability here.
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
            output_fields.push(Attribute::minted(col_name, dt, true));
        }
    }
    let output_schema = ResolvedSchema::new(output_fields);

    Ok(TypedAst::new(
        TypedOp::Pivot {
            input: Box::new(typed_input),
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
        },
        output_schema,
    ))
}

/// Desugar a `crosstab(col1, col2)` into a conditional-count
/// [`CommonOp::Aggregate`], the Spark-parity contingency table.
///
/// Spark's `StatFunctions.crossTabulate` emits one output row per distinct
/// `col1` value and one output column per distinct `col2` value, each cell a
/// COUNT of the (col1, col2) co-occurrences. The `col2` column set is
/// data-dependent, so τ's pure/synchronous analyzer (INV10 bars it from
/// `crate::runtime`) cannot discover it — the connect-server discovery pass
/// runs `SELECT DISTINCT col2` against the live session and hands the values
/// in via `distinct_col2_values`.
///
/// The resulting `Aggregate`:
/// - `grouping = [ Alias(CASE WHEN col1 IS NULL THEN 'null' ELSE CAST(col1 AS
///   STRING) END, "{col1}_{col2}") ]` — col0 is named by joining the two column
///   names with `_`; its value is the string form of col1, with a NULL col1
///   relabeled to the literal string `"null"` (Spark parity). Nullability
///   follows col1 (the else branch governs).
/// - `aggregates = [ Alias(count(CASE WHEN predᵢ THEN 1 END), nameᵢ), … ]`,
///   one per distinct col2 value `vᵢ`, where `nameᵢ =
///   literal_to_pivot_column_name(vᵢ)` and `predᵢ = (col2 = vᵢ)` — or `col2 IS
///   NULL` for the NULL bucket (a NULL is a real bucket, never dropped). The
///   aggregates are sorted ascending by `nameᵢ` lexicographically as strings,
///   matching Spark's crosstab column order (e.g. `'10','2','null'`). `count`
///   is intrinsically non-null and 0-fills empty buckets, so each count column
///   resolves to `bigint nullable=False`.
///
/// The grouping key (col0, string, nullability from col1) is folded ahead of
/// the count columns at construction time via [`grouped_aggregate`] — the
/// output list is `[col0] ++ counts`, with the count columns' lexicographic
/// sort applied BEFORE the prepend so col0 stays first.
pub fn crosstab_to_aggregate(
    input: CommonAst,
    col1: &str,
    col2: &str,
    distinct_col2_values: Vec<Expression>,
) -> CommonOp {
    use super::ast::GroupingKind;
    use super::expression::{BinaryOp, UnaryOp};

    let col_ref = |name: &str| {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: None,
        })
    };

    // col0: WHEN col1 IS NULL THEN 'null' ELSE CAST(col1 AS STRING) END
    // AS "{col1}_{col2}". Spark's `StatFunctions.crossTabulate` relabels a NULL
    // col1 row to the literal string "null" (not SQL NULL) in the contingency
    // table. The else branch (`CAST(col1 AS STRING)`) governs nullability, so
    // col0's nullability still follows col1.
    let grouping = vec![Expression::Alias(AliasExpression {
        expr: Box::new(Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(
                Expression::Unary(UnaryExpression {
                    op: UnaryOp::IsNull,
                    operand: Box::new(col_ref(col1)),
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("null".to_owned()),
                    data_type: DataType::String,
                }),
            )],
            else_expr: Some(Box::new(Expression::Cast(CastExpression {
                expr: Box::new(col_ref(col1)),
                to_type: DataType::String,
                try_cast: false,
                implicit: false,
            }))),
        })),
        alias: format!("{col1}_{col2}"),
    })];

    // One conditional-count column per distinct col2 value.
    let mut counts: Vec<(String, Expression)> = distinct_col2_values
        .into_iter()
        .map(|v| {
            let name = literal_to_pivot_column_name(&v);
            let is_null = matches!(
                &v,
                Expression::Literal(Literal {
                    value: LiteralValue::Null,
                    ..
                })
            );
            let pred = if is_null {
                Expression::Unary(UnaryExpression {
                    op: UnaryOp::IsNull,
                    operand: Box::new(col_ref(col2)),
                })
            } else {
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(col_ref(col2)),
                    right: Box::new(v),
                })
            };
            let count = Expression::FunctionCall(FunctionCall {
                name: "count".to_owned(),
                args: vec![Expression::CaseWhen(CaseWhenExpression {
                    branches: vec![(
                        pred,
                        Expression::Literal(Literal {
                            value: LiteralValue::Int(1),
                            data_type: DataType::Integer,
                        }),
                    )],
                    else_expr: None,
                })],
                distinct: false,
            });
            (name, count)
        })
        .collect();
    // Spark sorts crosstab count columns by the string form of the value
    // (lexicographic, not numeric — e.g. '10','2','null').
    counts.sort_by(|(a, _), (b, _)| a.cmp(b));

    let agg_exprs = counts
        .into_iter()
        .map(|(name, count)| {
            Expression::Alias(AliasExpression {
                expr: Box::new(count),
                alias: name,
            })
        })
        .collect();

    grouped_aggregate(input, grouping, agg_exprs, GroupingKind::GroupBy)
}

/// Spark's implicit PIVOT grouping: the input columns minus the pivot column
/// minus every column referenced by the aggregate argument(s), preserved in
/// input-schema order (Spark parity per ADR-015). Used for SQL `PIVOT`, which
/// supplies no grouping list. `count(*)` references no column (its `Star`
/// argument contributes nothing), so every non-pivot column remains grouped.
fn derive_implicit_grouping(
    input_schema: &ResolvedSchema,
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
        .filter(|f| !excluded.contains(&fold_key(&f.name)))
        .map(|f| {
            // `expr_id`: `f` IS the real input attribute this grouping
            // column names, so its id is stamped directly — carries the
            // implicit-grouping output's lineage through to
            // `output_attribute`, which now COPIES (id + lineage) instead of
            // MINTING a fresh identity for these PIVOT grouping outputs.
            Expression::ColumnReference(ColumnReference::from_attr(f))
        })
        .collect()
}

/// Recursively collect the (lowercased) names of every column referenced by an
/// expression tree into `acc`. A bare `Star` contributes nothing (so
/// `count(*)` references no column). Used by [`derive_implicit_grouping`].
///
/// Recursion follows the canonical [`Expression::children`] walker, whose
/// per-variant child set matches this collector's scope rules exactly:
///
/// * **Subqueries are a SEPARATE scope** — τ does not support correlated
///   pivot aggregates. `InSubquery` contributes only its outer `expr` (e.g.
///   `dept_id IN (…)` references `dept_id`); `ExistsSubquery` /
///   `ScalarSubquery` contribute nothing (their only sub-expressions live in
///   the inner plan, which `children()` treats as opaque).
/// * **Lambda bodies ARE recursed** — `transform(arr, x -> x + outer_col)`
///   references `outer_col`. The body's `LambdaVariable` refs are
///   lambda-local leaves (no children, not a `ColumnReference`) and add
///   nothing.
/// * **Opaque leaves contribute nothing** — `RawSql` (unparsed SQL string),
///   `UnresolvedRegex` (expanded away by the Project pre-pass before
///   inference), `Literal`, `Interval`, `Star`.
fn collect_referenced_columns(expr: &Expression, acc: &mut HashSet<String>) {
    match expr {
        Expression::ColumnReference(c) => {
            acc.insert(fold_key(&c.name));
        }
        Expression::UnresolvedColumn(u) => {
            acc.insert(fold_key(&u.name));
        }
        _ => {
            for child in expr.children() {
                collect_referenced_columns(child, acc);
            }
        }
    }
}

/// Spark's rendering of a pivot value literal to a column name. Boolean
/// `true`/`false` render as `"true"`/`"false"`; integers as their decimal
/// repr; strings verbatim. Non-literal expressions (should not happen —
/// PySpark only sends literals) fall back to [`expression_output_name`].
fn literal_to_pivot_column_name(expr: &Expression) -> String {
    if let Expression::Literal(lit) = expr {
        return literal_value_string(&lit.value);
    }
    expression_output_name(expr)
}

/// Shared literal stringifier for [`literal_to_pivot_column_name`] and
/// [`pretty_literal`] — Spark's string form of a literal value. The two
/// callers diverge only on `Null` and `Binary`: this fn carries the
/// pivot-name arms (`Null` → `"null"`, `Binary` → `"binary"`), and
/// `pretty_literal` overrides both before delegating.
fn literal_value_string(value: &LiteralValue) -> String {
    match value {
        // Spark's `PivotTransformer.outputName` casts the pivot value to
        // StringType and falls back to the literal string `"null"` when the
        // cast evaluates to null. A discovered NULL bucket (e.g. from an
        // `explode_outer` pivot column) is therefore named `"null"`.
        LiteralValue::Null => "null".to_owned(),
        LiteralValue::Boolean(b) => b.to_string(),
        LiteralValue::Byte(v) => v.to_string(),
        LiteralValue::Short(v) => v.to_string(),
        LiteralValue::Int(v) => v.to_string(),
        LiteralValue::Long(v) => v.to_string(),
        // Spark renders integral float/double literals with a `.0` suffix.
        LiteralValue::Float(v) => format_float_pivot_name(f64::from(*v)),
        LiteralValue::Double(v) => format_float_pivot_name(*v),
        LiteralValue::Decimal { value, .. } => value.clone(),
        LiteralValue::String(s) => s.clone(),
        LiteralValue::Date(d) => d.to_string(),
        LiteralValue::Timestamp(t) | LiteralValue::TimestampNtz(t) => t.to_string(),
        LiteralValue::Binary(_) => "binary".to_owned(),
    }
}

/// Spark-parity formatter for float/double pivot column names.
///
/// Catalyst's `Literal.sql` for a `DoubleType(1.0)` yields the string `"1.0"`
/// (integral doubles get a `.0` suffix; non-integral doubles use their
/// natural decimal repr). NaN/infinity fall through to Rust's default
/// `Display`, which emits `"NaN"` / `"inf"` / `"-inf"` — a lossless-but-not
/// necessarily Spark-precise stringification; these values are used only as
/// pivot column names.
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
        // An unaliased function call is named by Spark's `toPrettySQL`
        // rendering (`fn(args)`, e.g. `sum(ss_net_profit)`), not the bare
        // function name — verified against Spark 4.1.1's own column naming
        // (incl. multi-aggregate PIVOT, which names unaliased buckets
        // `<value>_<fn(args)>`, e.g. `a_sum(val)`, not `<value>_<fn>`). See
        // the pivot naming call site (`analyze_pivot`, ~L4843), which routes
        // through this function and stays correct under the rename.
        // Spark toPrettySQL special-case: the time-`window` / `session_window`
        // functions produce a named STRUCT column whose output name is the bare
        // function name (`window` / `session_window`), NOT `fn(args)`. Every
        // other unaliased function call is named `fn(args)`.
        Expression::FunctionCall(f) if matches!(f.name.as_str(), "window" | "session_window") => {
            f.name.clone()
        }
        Expression::FunctionCall(_) => pretty_name(expr),
        Expression::Literal(_) => "col".to_owned(),
        _ => pretty_name(expr),
    }
}

/// Carry the Spark `UnresolvedAlias` → `Alias` output name on
/// the tree itself, so any consumer that walks a Project/Aggregate/Pivot
/// output list can read the entry's declared name directly instead of
/// re-deriving it via [`expression_output_name`]. Bare `ColumnReference`s and
/// `Star` stay bare (Spark parity — a passthrough column has no
/// `UnresolvedAlias` to resolve); `Alias` is idempotent (already named).
/// Every other shape is wrapped once, using the same name
/// [`expression_output_name`] would have produced for it.
fn ensure_named(expr: Expression) -> Expression {
    match expr {
        e @ (Expression::Alias(_) | Expression::ColumnReference(_) | Expression::Star(_)) => e,
        e => {
            let alias = expression_output_name(&e);
            Expression::Alias(AliasExpression {
                expr: Box::new(e),
                alias,
            })
        }
    }
}

/// The Spark `.sql` symbol for a binary operator, matching Spark's
/// `Expression.sql` / `toPrettySQL` rendering (NOT DuckDB's emission symbols —
/// see `render_binary`). Exhaustive on purpose: a new [`BinaryOp`] must make a
/// naming decision here rather than silently fall through.
fn pretty_binary_symbol(op: &BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::IntDiv => "div",
        BinaryOp::Eq => "=",
        BinaryOp::NotEq => "<>",
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::And => "AND",
        BinaryOp::Or => "OR",
        BinaryOp::Concat => "||",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
    }
}

/// Function names whose Spark `toPrettySQL` auto-name is UPPERCASE
/// regardless of how the call was written (`ceil(x)` and `CEIL(x)` both
/// auto-name `"CEIL(x)"`) — the `UnaryMathExpression`/`BinaryMathExpression`
/// family in Catalyst. Verified empirically against a vendored Spark 4.1.1
/// session (2026-07-12, `/tmp/n5probe/`); every other function auto-names
/// from its *lowercased* registry key (`SUM(x)` → `"sum(x)"`), so this is an
/// explicit exception roster, not a general case-preservation rule. PRIMARY
/// names only — alias spellings (`ceiling`, `pow`, `sign`, `ucase`) keep
/// their own lowercase lookup spelling (probe-verified: `ceiling(x)` names
/// `"ceiling(x)"`, `pow`/`sign`/`ln`/`rint` stay lowercase).
const SPARK_UPPER_PRETTY: &[&str] = &[
    "acos", "acosh", "asin", "asinh", "atan", "atan2", "atanh", "cbrt", "ceil", "cos", "cosh",
    "cot", "csc", "degrees", "e", "exp", "expm1", "floor", "hypot", "log", "log10", "log1p",
    "log2", "pi", "power", "radians", "sec", "signum", "sin", "sinh", "sqrt", "tan", "tanh",
];

/// Spark `toPrettySQL`-parity default column name for an unaliased projection
/// expression (Spark `UnresolvedAlias` → `Column.named` → `toPrettySQL`).
///
/// This is deliberately distinct from both [`expression_output_name`] (which
/// names top-level `Literal`s `"col"`, and now defers `FunctionCall` straight
/// to this function) and the emission `render_*` family (which uses DuckDB
/// symbols, ANSI guards, and DuckDB-substrate CAST target spellings). It is
/// value-aware — a literal renders its value, not its type — and recursive
/// over the structural variants Spark inlines into a pretty name.
///
/// Variants Spark renders in a shape τ does not yet match exactly (windows,
/// subqueries, complex-type literals, …) keep the Thunderduck-boundary
/// fallback name `"expr"`.
fn pretty_name(expr: &Expression) -> String {
    match expr {
        Expression::ColumnReference(c) => c.name.clone(),
        Expression::UnresolvedColumn(u) => u.name.clone(),
        Expression::Alias(a) => a.alias.clone(),
        Expression::Literal(l) => pretty_literal(&l.value),
        Expression::Binary(b) => format!(
            "({} {} {})",
            pretty_name(&b.left),
            pretty_binary_symbol(&b.op),
            pretty_name(&b.right)
        ),
        Expression::Unary(u) => pretty_unary(u),
        Expression::FunctionCall(f) => {
            let args: Vec<String> = f.args.iter().map(pretty_name).collect();
            // Spark's uppercase auto-name exceptions are listed separately.
            if SPARK_UPPER_PRETTY.contains(&f.name.as_str()) {
                format!("{}({})", f.name.to_ascii_uppercase(), args.join(", "))
            } else {
                format!("{}({})", f.name, args.join(", "))
            }
        }
        // Analyzer-inserted casts are transparent to Spark's output naming.
        Expression::Cast(c) if c.implicit => pretty_name(&c.expr),
        // Spark's `Cast.sql` / `TryCast.sql` renders `CAST(<child> AS <TYPE>)`
        // / `TRY_CAST(<child> AS <TYPE>)` with the UPPERCASE Catalyst type
        // spelling (`DECIMAL(15,4)`, not DuckDB's `DECIMAL(15, 4)` substrate
        // spelling with the internal space — see `spark_type_sql`).
        Expression::Cast(c) => {
            let kw = if c.try_cast { "TRY_CAST" } else { "CAST" };
            format!(
                "{kw}({} AS {})",
                pretty_name(&c.expr),
                spark_type_sql(&c.to_type)
            )
        }
        Expression::Star(s) => match &s.qualifier {
            Some(q) => format!("{q}.*"),
            None => "*".to_owned(),
        },
        // Spark names a struct-field access by its leaf field name (the string
        // extraction key), e.g. `address.geo.lat` → `lat`.
        Expression::ExtractValue(e) => match e.extraction.as_ref() {
            Expression::Literal(Literal {
                value: LiteralValue::String(field),
                ..
            }) => field.clone(),
            _ => "expr".to_owned(),
        },
        // Spark `CaseWhen.sql`: `CASE WHEN <cond> THEN <val> [WHEN …] [ELSE <e>] END`,
        // each child rendered through `pretty_name` (binary conditions get their
        // parens from the `Binary` arm, bare boolean columns stay unparenthesized).
        Expression::CaseWhen(cw) => {
            let mut s = "CASE".to_owned();
            for (cond, val) in &cw.branches {
                s.push_str(&format!(
                    " WHEN {} THEN {}",
                    pretty_name(cond),
                    pretty_name(val)
                ));
            }
            if let Some(e) = &cw.else_expr {
                s.push_str(&format!(" ELSE {}", pretty_name(e)));
            }
            s.push_str(" END");
            s
        }
        _ => "expr".to_owned(),
    }
}

/// Spark `DataType.sql` UPPERCASE type-name spelling, used by [`pretty_name`]'s
/// `Cast` arm to reproduce Spark's `toPrettySQL` rendering (e.g.
/// `CAST(x AS DECIMAL(15,4))`). Deliberately distinct from
/// `emission::render_data_type`, which spells DuckDB-substrate CAST targets
/// (`VARCHAR`, `BLOB`, `TIMESTAMP WITH TIME ZONE`, `DECIMAL(15, 4)` with an
/// internal space, …) for emitted SQL — a different consumer with different
/// spelling rules.
fn spark_type_sql(dt: &DataType) -> String {
    match dt {
        DataType::Boolean => "BOOLEAN".to_owned(),
        DataType::Byte => "TINYINT".to_owned(),
        DataType::Short => "SMALLINT".to_owned(),
        DataType::Integer => "INT".to_owned(),
        DataType::Long => "BIGINT".to_owned(),
        DataType::Float => "FLOAT".to_owned(),
        DataType::Double => "DOUBLE".to_owned(),
        DataType::Decimal { precision, scale } => format!("DECIMAL({precision},{scale})"),
        DataType::String => "STRING".to_owned(),
        DataType::Binary => "BINARY".to_owned(),
        DataType::Date => "DATE".to_owned(),
        DataType::Timestamp => "TIMESTAMP".to_owned(),
        DataType::TimestampNtz => "TIMESTAMP_NTZ".to_owned(),
        DataType::YearMonthInterval => "INTERVAL YEAR TO MONTH".to_owned(),
        DataType::DayTimeInterval => "INTERVAL DAY TO SECOND".to_owned(),
        DataType::Interval => "INTERVAL".to_owned(),
        DataType::Null => "VOID".to_owned(),
        DataType::Unresolved => "STRING".to_owned(),
        DataType::Array(elem, _) => format!("ARRAY<{}>", spark_type_sql(elem)),
        DataType::Map { key, value, .. } => {
            format!("MAP<{}, {}>", spark_type_sql(key), spark_type_sql(value))
        }
        DataType::Struct(st) => {
            let fields: Vec<String> = st
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, spark_type_sql(&f.data_type)))
                .collect();
            format!("STRUCT<{}>", fields.join(", "))
        }
    }
}

/// Spark `.sql` rendering of a literal value for [`pretty_name`]. Strings are
/// rendered UNQUOTED (Spark's pretty name drops the quotes), floats reuse
/// [`format_float_pivot_name`] so an integral value keeps its `.0`. Diverges
/// from [`literal_value_string`] only on `Null` (uppercase `NULL`) and
/// `Binary` (the boundary fallback name `expr`).
fn pretty_literal(value: &LiteralValue) -> String {
    match value {
        LiteralValue::Null => "NULL".to_owned(),
        LiteralValue::Binary(_) => "expr".to_owned(),
        other => literal_value_string(other),
    }
}

/// Spark `.sql` rendering of a unary expression for [`pretty_name`].
fn pretty_unary(u: &UnaryExpression) -> String {
    let x = pretty_name(&u.operand);
    match u.op {
        UnaryOp::Not => format!("(NOT {x})"),
        UnaryOp::IsNull => format!("({x} IS NULL)"),
        UnaryOp::IsNotNull => format!("({x} IS NOT NULL)"),
        UnaryOp::Negate => format!("(- {x})"),
        UnaryOp::IsNaN => format!("isnan({x})"),
        UnaryOp::IsNotNaN => format!("(NOT isnan({x}))"),
    }
}

fn push_setop_casts(ast: &mut TypedAst, widened_schema: &ResolvedSchema) {
    // Only push CASTs into direct `Project` children whose projection list
    // matches the widened schema column-by-column. Non-Project inputs
    // (TableScan, Values, ...) receive their CAST at emission time.
    if let TypedOp::Project {
        projections, input, ..
    } = &mut ast.op
    {
        if projections.len() != widened_schema.fields.len() {
            return;
        }
        // `projections` and `input` are disjoint fields of the same `Project`,
        // and `ast.resolved_schema` is a disjoint field of `ast` — one
        // `if let` binds them all without the re-borrow dance.
        let input_schema = input.resolved_schema.clone();
        for (idx, proj) in projections.iter_mut().enumerate() {
            // Star survives verbatim: the widened schema takes its names
            // from `*`'s expansion and `* AS id` is invalid SQL.
            if matches!(proj, Expression::Star(_)) {
                continue;
            }
            let target = &widened_schema.fields[idx];
            // Type BEFORE stripping any alias (Alias delegates to inner).
            let current_type = proj.data_type(&input_schema);
            let cast_to = (current_type != target.data_type
                && !matches!(current_type, DataType::Unresolved))
            .then(|| target.data_type.clone());
            align_setop_projection(proj, &target.name, cast_to);
        }
        // Overwrite name/type/nullability positionally, but keep this child's
        // own ids — copying the
        // widened schema's (child-0-derived) ids into every child would
        // silently reassign this child's columns' identity to child 0's.
        for (attr, target) in ast
            .resolved_schema
            .fields
            .iter_mut()
            .zip(&widened_schema.fields)
        {
            attr.name = target.name.clone();
            attr.data_type = target.data_type.clone();
            attr.nullable = target.nullable;
        }
    }
}

/// Re-alias a set-op branch projection to `name` (Spark: union column names
/// come from the first branch → the widened name always wins, so strip any
/// existing top-level alias). Cast the value first when `cast_to` is `Some`;
/// otherwise the aliased column keeps its type. Aligning every non-`Star`
/// branch guarantees the child subquery's output column is named `name`, so
/// `render_set_op`'s outer positional `CAST(<name> AS <ty>) AS <name>` binds.
fn align_setop_projection(expr: &mut Expression, name: &str, cast_to: Option<DataType>) {
    let placeholder = Expression::Literal(super::expression::Literal {
        value: super::expression::LiteralValue::Null,
        data_type: DataType::Null,
    });
    let inner = match std::mem::replace(expr, placeholder) {
        Expression::Alias(a) => *a.expr,
        other => other,
    };
    let valued = match cast_to {
        Some(to_type) => Expression::Cast(CastExpression {
            expr: Box::new(inner),
            to_type,
            try_cast: false,
            implicit: false,
        }),
        None => inner,
    };
    *expr = Expression::Alias(AliasExpression {
        expr: Box::new(valued),
        alias: name.to_owned(),
    });
}

fn apply_join_nullability(
    left: &ResolvedSchema,
    right: &ResolvedSchema,
    join_type: JoinType,
) -> (ResolvedSchema, ResolvedSchema) {
    match join_type {
        JoinType::Inner | JoinType::Cross => (left.clone(), right.clone()),
        JoinType::Left => (left.clone(), flip_all_nullable(right)),
        JoinType::Right => (flip_all_nullable(left), right.clone()),
        JoinType::Full => (flip_all_nullable(left), flip_all_nullable(right)),
        JoinType::LeftSemi | JoinType::LeftAnti => (left.clone(), ResolvedSchema::empty()),
    }
}

/// Copy `schema` with every field forced nullable — outer-join padding
/// semantics. `pub(super)` so `analyzer_fixtures` builds its expected
/// join schemas from the same helper. Clone-mutate: every field is the SAME
/// logical column, only its nullability changes — ids ride through.
pub(super) fn flip_all_nullable(schema: &ResolvedSchema) -> ResolvedSchema {
    let fields = schema
        .fields
        .iter()
        .map(|f| {
            let mut attr = f.clone();
            attr.nullable = true;
            attr
        })
        .collect();
    ResolvedSchema::new(fields)
}

fn infer_values_schema(
    rows: &[Vec<Expression>],
    column_names: &[String],
) -> Result<StructType, AnalyzerError> {
    if rows.is_empty() {
        return Err(AnalyzerError::Internal {
            reason: "VALUES relation must have at least one row".to_owned(),
        });
    }
    let ncols = rows[0].len();
    if let Some(bad) = rows.iter().find(|r| r.len() != ncols) {
        // Ragged VALUES rows (e.g. `VALUES (1,2),(3)`) would otherwise index
        // out of bounds below on the shorter rows — a session-killing panic.
        // Spark rejects inconsistent-length VALUES with an AnalysisException.
        return Err(AnalyzerError::SparkEmulated {
            class: "INVALID_INLINE_TABLE.NUM_COLUMNS_MISMATCH",
            reason: format!(
                "VALUES rows have inconsistent lengths: expected {ncols}, got {}",
                bad.len()
            ),
        });
    }
    if ncols != column_names.len() {
        // Arity mismatch, not a per-column type mismatch — see the set-op
        // path for the equivalent decision.
        return Err(AnalyzerError::SparkEmulated {
            class: "INVALID_INLINE_TABLE.NUM_COLUMNS_MISMATCH",
            reason: format!(
                "VALUES column count mismatch: {} names vs {} row columns",
                column_names.len(),
                ncols
            ),
        });
    }
    let empty = ResolvedSchema::empty();
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::analyzer_fixtures;
    use super::super::ast::{CommonAst, GroupingKind};
    use super::super::expression::{
        AliasExpression, BetweenExpression, BinaryExpression, BinaryOp, ExistsSubquery,
        FunctionCall, InSubquery, IntervalExpression, IntervalKind, LambdaExpression,
        LambdaVariableExpression, Literal, LiteralValue, NullOrdering, ScalarSubquery,
        SortDirection, StarExpression, UnaryExpression, UnaryOp, UnresolvedRegexExpression,
    };
    use super::super::schema::ExprId;
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

    fn scan(name: &str) -> CommonAst {
        CommonAst::new(CommonOp::TableScan {
            table: name.to_owned(),
        })
    }

    fn emp_scan() -> CommonAst {
        scan("emp")
    }

    fn unresolved_col(name: &str) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: None,
        })
    }

    fn qcol(qualifier: &str, name: &str) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: Some(qualifier.to_owned()),
            plan_id: None,
        })
    }

    fn int_lit(v: i32) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Int(v),
            data_type: DataType::Integer,
        })
    }

    fn lit_double(v: f64) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Double(v),
            data_type: DataType::Double,
        })
    }

    fn lit_str(s: &str) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::String(s.to_owned()),
            data_type: DataType::String,
        })
    }

    fn lit_bool(b: bool) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Boolean(b),
            data_type: DataType::Boolean,
        })
    }

    fn func(name: &str, args: Vec<Expression>) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: name.to_owned(),
            args,
            distinct: false,
        })
    }

    fn alias_expr(expr: Expression, alias: &str) -> Expression {
        Expression::Alias(AliasExpression {
            expr: Box::new(expr),
            alias: alias.to_owned(),
        })
    }

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

    fn join(
        left: CommonAst,
        right: CommonAst,
        join_type: JoinType,
        condition: Option<Expression>,
    ) -> CommonAst {
        CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type,
            condition,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        })
    }

    fn set_op(kind: SetOpKind, all: bool, children: Vec<CommonAst>) -> CommonAst {
        CommonAst::new(CommonOp::SetOp {
            kind,
            all,
            by_name: false,
            allow_missing_columns: false,
            children,
        })
    }

    fn set_op_by_name(kind: SetOpKind, all: bool, children: Vec<CommonAst>) -> CommonAst {
        CommonAst::new(CommonOp::SetOp {
            kind,
            all,
            by_name: true,
            allow_missing_columns: false,
            children,
        })
    }

    fn union_by_name_allow_missing(children: Vec<CommonAst>) -> CommonAst {
        CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: true,
            children,
        })
    }

    fn aggregate_having(
        input: CommonAst,
        grouping: Vec<Expression>,
        aggregates: Vec<Expression>,
        having: Option<Expression>,
    ) -> CommonAst {
        CommonAst::new(CommonOp::Aggregate {
            input: Box::new(input),
            grouping,
            aggregates,
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having,
        })
    }

    fn aggregate(
        input: CommonAst,
        grouping: Vec<Expression>,
        aggregates: Vec<Expression>,
    ) -> CommonAst {
        aggregate_having(input, grouping, aggregates, None)
    }

    fn base_types_for(tables: &[(&str, StructType)]) -> BaseTypes {
        BaseTypes::from_entries(
            tables
                .iter()
                .map(|(name, schema)| ((*name).to_owned(), schema.clone()))
                .collect(),
        )
    }

    fn base_types_with_emp_dept() -> BaseTypes {
        base_types_for(&[("emp", emp_schema()), ("dept", dept_schema())])
    }

    fn aliased_scan(table: &str, alias: &str) -> CommonAst {
        CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: table.to_owned(),
            })),
            alias: alias.to_owned(),
        })
    }

    #[test]
    fn rel_scope_aliased_scan_binds_alias_only_shadowing_table_name() {
        let bt = base_types_with_emp_dept();
        let typed = analyze(aliased_scan("emp", "e"), &bt).unwrap();
        assert_eq!(typed.scope.aliases, vec![("e".to_owned(), 0..4)]);
        assert!(typed.scope.plan_ids.is_empty());
    }

    #[test]
    fn rel_scope_aliased_relation_rebinds_and_drops_child_scope() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.scope.aliases, vec![("e".to_owned(), 0..4)]);
    }

    #[test]
    fn rel_scope_join_composes_children_with_right_offset() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("e", "dept_id")),
            right: Box::new(qcol("d", "dept_id")),
        });
        let ast = join(
            aliased_scan("emp", "e"),
            aliased_scan("dept", "d"),
            JoinType::Inner,
            Some(cond),
        );
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(
            typed.scope.aliases,
            vec![("e".to_owned(), 0..4), ("d".to_owned(), 4..6)]
        );
    }

    #[test]
    fn rel_scope_semi_anti_join_binds_left_only() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("e", "dept_id")),
            right: Box::new(qcol("d", "dept_id")),
        });
        let ast = join(
            aliased_scan("emp", "e"),
            aliased_scan("dept", "d"),
            JoinType::LeftSemi,
            Some(cond),
        );
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.scope.aliases, vec![("e".to_owned(), 0..4)]);
    }

    #[test]
    fn rel_scope_using_join_is_empty() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(emp_scan()),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert!(typed.scope.aliases.is_empty());
        assert!(typed.scope.plan_ids.is_empty());
    }

    #[test]
    fn rel_scope_passthrough_preserves_child_scope() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Gt,
            left: Box::new(qcol("e", "salary")),
            right: Box::new(lit_double(100.0)),
        });
        let joined = join(
            aliased_scan("emp", "e"),
            aliased_scan("dept", "d"),
            JoinType::Inner,
            Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
        );
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(joined),
            condition: cond,
        });
        let typed = analyze(ast, &bt).unwrap();
        let inner_scope = match &typed.op {
            TypedOp::Filter { input, .. } => input.scope.clone(),
            other => panic!("expected Filter, got {other:?}"),
        };
        assert_eq!(typed.scope, inner_scope);
        assert_eq!(typed.scope.aliases.len(), 2);
    }

    #[test]
    fn rel_scope_join_plan_ids_outermost_first() {
        let bt = base_types_for(&[
            ("emp", emp_schema()),
            ("dept", dept_schema()),
            ("bonus", dept_schema()),
        ]);
        let inner = CommonAst::new(CommonOp::Join {
            left: Box::new(emp_scan()),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: Some(1),
                })),
                right: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: Some(2),
                })),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let outer = CommonAst::new(CommonOp::Join {
            left: Box::new(inner),
            right: Box::new(scan("bonus")),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![3],
        });
        let typed = analyze(outer, &bt).unwrap();
        let pid1_entries: Vec<_> = typed
            .scope
            .plan_ids
            .iter()
            .filter(|(pid, _)| *pid == 1)
            .collect();
        assert_eq!(pid1_entries.len(), 2);
        assert_eq!(pid1_entries[0].1, 0..6);
        assert_eq!(pid1_entries[1].1, 0..4);
    }

    #[test]
    fn rel_scope_generate_appends_generated_range() {
        let bt = base_types_with_emp_dept();
        let input = analyze(aliased_scan("emp", "e"), &bt).unwrap();
        let mut generator = Generator::from_function("explode", vec![lit_str("x")]).unwrap();
        generator.aliases.push("tag".to_owned());
        let merged = ResolvedSchema::merge(
            &input.resolved_schema,
            &ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
                "tag",
                DataType::String,
            )])),
        );
        let typed = TypedAst::new(
            TypedOp::Generate {
                input: Box::new(input),
                generator,
                qualifier: Some("t".to_owned()),
            },
            merged,
        );
        assert_eq!(
            typed.scope.aliases,
            vec![("e".to_owned(), 0..4), ("t".to_owned(), 4..5)]
        );
    }

    fn pcol(name: &str, plan_id: i64) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: Some(plan_id),
        })
    }

    fn plan_id_join(condition: Option<Expression>) -> CommonAst {
        CommonAst::new(CommonOp::Join {
            left: Box::new(emp_scan()),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        })
    }

    fn merged_join_expr_id_at(typed: &TypedAst, k: usize) -> ExprId {
        let TypedOp::Join { left, right, .. } = &typed.op else {
            panic!("expected Join, got {:?}", typed.op);
        };
        let left_len = left.resolved_schema.len();
        if k < left_len {
            left.resolved_schema.fields[k].expr_id
        } else {
            right.resolved_schema.fields[k - left_len].expr_id
        }
    }

    fn join_condition_refs(typed: &TypedAst) -> (ColumnReference, ColumnReference) {
        let TypedOp::Join { condition, .. } = &typed.op else {
            panic!("expected Join, got {:?}", typed.op);
        };
        let Expression::Binary(BinaryExpression { left, right, .. }) =
            condition.as_ref().expect("condition")
        else {
            panic!("expected Binary condition, got {condition:?}");
        };
        let Expression::ColumnReference(l) = left.as_ref() else {
            panic!("expected ColumnReference (left), got {left:?}");
        };
        let Expression::ColumnReference(r) = right.as_ref() else {
            panic!("expected ColumnReference (right), got {right:?}");
        };
        (l.clone(), r.clone())
    }

    #[test]
    fn adr023_phase1_unique_name_condition_drops_qualifier() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(pcol("id", 1)),
            right: Box::new(pcol("dept_name", 2)),
        });
        let typed = analyze(plan_id_join(Some(cond)), &bt).unwrap();
        let (l, r) = join_condition_refs(&typed);
        assert_eq!(l.qualifier, None);
        assert_eq!(l.expr_id, Some(merged_join_expr_id_at(&typed, 0)));
        assert_eq!(r.qualifier, None);
        assert_eq!(r.expr_id, Some(merged_join_expr_id_at(&typed, 5)));
    }

    #[test]
    fn adr023_phase1_dup_name_condition_resolves_bare_ordinal() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(pcol("dept_id", 1)),
            right: Box::new(pcol("dept_id", 2)),
        });
        let typed = analyze(plan_id_join(Some(cond)), &bt).unwrap();
        let (l, r) = join_condition_refs(&typed);
        assert_eq!(l.qualifier, None);
        assert_eq!(l.expr_id, Some(merged_join_expr_id_at(&typed, 2)));
        assert_eq!(r.qualifier, None);
        assert_eq!(r.expr_id, Some(merged_join_expr_id_at(&typed, 4)));
    }

    #[test]
    fn adr023_phase1_self_join_condition_still_wraps() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(pcol("id", 1)),
            right: Box::new(pcol("id", 2)),
        });
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("emp"),
            JoinType::Inner,
            Some(cond),
            vec![1],
            vec![2],
        );
        let typed = analyze(joined, &bt).unwrap();
        let (l, r) = join_condition_refs(&typed);
        assert_eq!(l.qualifier, None);
        assert_eq!(l.expr_id, Some(merged_join_expr_id_at(&typed, 0)));
        assert_eq!(r.qualifier, None);
        assert_eq!(r.expr_id, Some(merged_join_expr_id_at(&typed, 4)));
    }

    #[test]
    fn ancestor_ref_resolves_bare_ordinal_through_passthrough() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("emp", "dept_id")),
            right: Box::new(qcol("dept", "dept_id")),
        });
        let filtered = CommonAst::new(CommonOp::Filter {
            input: Box::new(plan_id_join(Some(cond))),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(unresolved_col("salary")),
                right: Box::new(lit_double(0.0)),
            }),
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(filtered),
            projections: vec![pcol("dept_id", 1)],
        });
        let typed = analyze(ast, &bt).unwrap();
        let TypedOp::Project { input, projections } = &typed.op else {
            panic!("expected Project");
        };
        let TypedOp::Filter { input: join, .. } = &input.op else {
            panic!("expected Filter");
        };
        let Expression::ColumnReference(proj_ref) = &projections[0] else {
            panic!(
                "expected resolved ColumnReference, got {:?}",
                projections[0]
            );
        };
        assert_eq!(proj_ref.qualifier, None);
        assert_eq!(proj_ref.expr_id, Some(merged_join_expr_id_at(join, 2)));
    }

    #[test]
    fn ancestor_ref_does_not_leak_into_nested_joins() {
        let bt = base_types_for(&[
            ("emp", emp_schema()),
            ("dept", dept_schema()),
            ("bonus", dept_schema()),
        ]);
        let inner_cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("emp", "dept_id")),
            right: Box::new(qcol("dept", "dept_id")),
        });
        let inner = join(emp_scan(), scan("dept"), JoinType::Inner, Some(inner_cond));
        let outer_cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(pcol("dept_id", 7)),
            right: Box::new(pcol("dept_name", 8)),
        });
        let outer = CommonAst::new(CommonOp::Join {
            left: Box::new(inner),
            right: Box::new(scan("bonus")),
            join_type: JoinType::Inner,
            condition: Some(outer_cond),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![7],
            right_plan_ids: vec![8],
        });
        let filtered = CommonAst::new(CommonOp::Filter {
            input: Box::new(outer),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(pcol("dept_id", 7)),
                right: Box::new(lit_double(0.0)),
            }),
        });
        let typed = analyze(filtered, &bt).unwrap();
        let TypedOp::Filter {
            input: outer,
            condition,
        } = &typed.op
        else {
            panic!("expected Filter");
        };
        let Expression::Binary(BinaryExpression { left, .. }) = condition else {
            panic!("expected Binary condition");
        };
        let Expression::ColumnReference(ancestor_ref) = left.as_ref() else {
            panic!("expected ColumnReference, got {left:?}");
        };
        assert_eq!(ancestor_ref.qualifier, None);
        assert_eq!(ancestor_ref.expr_id, Some(merged_join_expr_id_at(outer, 2)));
    }

    fn field_names(typed: &TypedAst) -> Vec<&str> {
        typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect()
    }

    fn widened_of(typed: &TypedAst) -> &ResolvedSchema {
        match &typed.op {
            TypedOp::SetOp { widened_schema, .. } => widened_schema,
            other => panic!("expected SetOp, got {other:?}"),
        }
    }

    fn pivot_grouping_names(typed: &TypedAst) -> Vec<&str> {
        match &typed.op {
            TypedOp::Pivot { grouping, .. } => grouping
                .iter()
                .map(|g| match g {
                    Expression::ColumnReference(c) => c.name.as_str(),
                    other => panic!("expected resolved ColumnReference, got {other:?}"),
                })
                .collect(),
            other => panic!("expected TypedOp::Pivot, got {other:?}"),
        }
    }

    #[test]
    fn resolve_table_scan_seeds_schema_from_base_types() {
        let bt = base_types_with_emp_dept();
        let ast = scan("emp");
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn resolve_unknown_table_surfaces_spark_emulated_error() {
        let bt = BaseTypes::empty();
        let ast = scan("missing");
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownTable { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn resolve_unknown_column_surfaces_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("not_a_column")],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    #[test]
    fn qualified_star_over_table_scan_expands_to_full_schema() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some("emp".to_owned()),
            })],
        });
        let typed = analyze(ast, &bt).expect("analyze emp.*");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn qualified_star_over_semi_join_binds_left_relation_schema() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(
                CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("emp")),
                    alias: "e".to_owned(),
                }),
                CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                }),
                JoinType::LeftSemi,
                None,
            )),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some("e".to_owned()),
            })],
        });
        let typed = analyze(ast, &bt).expect("analyze e.* over semi join");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn qualified_star_with_unknown_qualifier_still_rejects() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some("bogus".to_owned()),
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    fn qstar_project(input: CommonAst, q: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(input),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some(q.to_owned()),
            })],
        })
    }

    fn emp_dept_aliased_join() -> CommonAst {
        join(
            aliased_scan("emp", "e"),
            aliased_scan("dept", "d"),
            JoinType::Inner,
            Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
        )
    }

    #[test]
    fn qualified_star_over_plain_join_expands_left_range() {
        let bt = base_types_with_emp_dept();
        let typed = analyze(qstar_project(emp_dept_aliased_join(), "e"), &bt)
            .expect("analyze e.* over join");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn qualified_star_over_plain_join_expands_right_range() {
        let bt = base_types_with_emp_dept();
        let typed = analyze(qstar_project(emp_dept_aliased_join(), "d"), &bt)
            .expect("analyze d.* over join");
        assert_eq!(typed.resolved_schema, dept_schema());
    }

    #[test]
    fn qualified_star_resolves_through_scope_passthrough() {
        let bt = base_types_with_emp_dept();
        let filtered = CommonAst::new(CommonOp::Filter {
            input: Box::new(emp_dept_aliased_join()),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(qcol("d", "dept_id")),
                right: Box::new(int_lit(0)),
            }),
        });
        let typed = analyze(qstar_project(filtered, "e"), &bt).expect("analyze e.* through filter");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn qualified_star_nullability_reflects_outer_join_flip() {
        let bt = base_types_with_emp_dept();
        let outer_join = join(
            aliased_scan("emp", "e"),
            aliased_scan("dept", "d"),
            JoinType::Left,
            Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
        );
        let typed =
            analyze(qstar_project(outer_join, "d"), &bt).expect("analyze d.* over left join");
        assert!(
            typed
                .resolved_schema
                .field_by_name("dept_id")
                .unwrap()
                .nullable,
            "left-join null extension must survive d.* expansion"
        );
    }

    #[test]
    fn qualified_star_over_using_join_still_rejects() {
        let bt = base_types_with_emp_dept();
        let using_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let err = analyze(qstar_project(using_join, "e"), &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    #[test]
    fn qualified_star_ambiguous_duplicate_alias_rejects() {
        let bt = base_types_with_emp_dept();
        let dup_join = join(
            aliased_scan("emp", "x"),
            aliased_scan("dept", "x"),
            JoinType::Cross,
            None,
        );
        let err = analyze(qstar_project(dup_join, "x"), &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    #[test]
    fn resolve_column_unqualified_matches_schema_index() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("salary")],
        });
        let typed = analyze(project, &bt).expect("unqualified salary must resolve");
        match &typed.op {
            TypedOp::Project { input, projections } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.name, "salary");
                    assert_eq!(c.expr_id, Some(input.resolved_schema.fields[3].expr_id));
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn resolve_column_qualified_tier_e_matches_schema_index() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_dept_aliased_join()),
            projections: vec![qcol("d", "dept_name")],
        });
        let typed = analyze(project, &bt).expect("d.dept_name must resolve");
        match &typed.op {
            TypedOp::Project { input, projections } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.name, "dept_name");
                    assert_eq!(c.expr_id, Some(input.resolved_schema.fields[5].expr_id));
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn resolve_column_plan_id_over_join_matches_correct_side() {
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(pcol("id", 1)),
            op: BinaryOp::Eq,
            right: Box::new(pcol("id", 2)),
        });
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("emp"),
            JoinType::Inner,
            Some(join_cond),
            vec![1],
            vec![2],
        );
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(joined),
            projections: vec![plan_id_col("id", 2)],
        });
        let typed = analyze(project, &bt).expect("plan_id should disambiguate above join");
        match &typed.op {
            TypedOp::Project { input, projections } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier, None);
                    assert_eq!(c.expr_id, Some(input.resolved_schema.fields[4].expr_id));
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    fn quals_of(schema: &ResolvedSchema) -> Vec<BTreeSet<String>> {
        schema
            .fields
            .iter()
            .map(|f| f.source_quals.clone())
            .collect()
    }

    #[test]
    fn source_quals_aliased_relation_binds_every_col_to_alias() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let typed = analyze(ast, &bt).unwrap();
        let e: BTreeSet<String> = ["e".to_owned()].into_iter().collect();
        assert_eq!(quals_of(&typed.resolved_schema), vec![e; 4]);
    }

    #[test]
    fn source_quals_project_passthrough_column_inherits_source() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased),
            projections: vec![qcol("e", "dept_id"), qcol("e", "name")],
        });
        let typed = analyze(project, &bt).unwrap();
        let e: BTreeSet<String> = ["e".to_owned()].into_iter().collect();
        assert_eq!(quals_of(&typed.resolved_schema), vec![e.clone(), e]);
    }

    #[test]
    fn source_quals_project_alias_creates_empty_lineage() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased),
            projections: vec![alias_expr(unresolved_col("dept_id"), "k")],
        });
        let typed = analyze(project, &bt).unwrap();
        assert_eq!(quals_of(&typed.resolved_schema), vec![BTreeSet::new()]);
    }

    #[test]
    fn source_quals_plain_join_composes_left_and_right() {
        let bt = base_types_with_emp_dept();
        let typed = analyze(emp_dept_aliased_join(), &bt).unwrap();
        let e: BTreeSet<String> = ["e".to_owned()].into_iter().collect();
        let d: BTreeSet<String> = ["d".to_owned()].into_iter().collect();
        let mut expected = vec![e; 4];
        expected.extend(vec![d; 2]);
        assert_eq!(quals_of(&typed.resolved_schema), expected);
    }

    #[test]
    fn source_quals_using_join_key_column_unions_both_sides() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        let e: BTreeSet<String> = ["e".to_owned()].into_iter().collect();
        let d: BTreeSet<String> = ["d".to_owned()].into_iter().collect();
        let key: BTreeSet<String> = e.union(&d).cloned().collect();
        assert_eq!(
            quals_of(&typed.resolved_schema),
            vec![key, e.clone(), e.clone(), e, d]
        );
    }

    #[test]
    fn source_quals_aggregate_grouping_col_inherits_source_aggregate_col_empty() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(grouped_aggregate(
            scan("emp"),
            vec![unresolved_col("dept_id")],
            vec![func("count", vec![int_lit(1)])],
            crate::transpiler_v2::ast::GroupingKind::GroupBy,
        ));
        let typed = analyze(ast, &bt).unwrap();
        let emp: BTreeSet<String> = ["emp".to_owned()].into_iter().collect();
        assert_eq!(quals_of(&typed.resolved_schema), vec![emp, BTreeSet::new()]);
    }

    #[test]
    fn source_quals_aggregate_folded_grouping_col_inherits_source_aggregate_col_empty() {
        let bt = base_types_with_emp_dept();
        let ast = aggregate(
            scan("emp"),
            vec![unresolved_col("dept_id")],
            vec![
                unresolved_col("dept_id"),
                func("sum", vec![unresolved_col("salary")]),
            ],
        );
        let typed = analyze(ast, &bt).unwrap();
        let emp: BTreeSet<String> = ["emp".to_owned()].into_iter().collect();
        assert_eq!(quals_of(&typed.resolved_schema), vec![emp, BTreeSet::new()]);
    }

    #[test]
    fn source_quals_star_projection_content_carries_through_and_is_tracked() {
        let bt = base_types_with_emp_dept();
        let star_project = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let typed = analyze(star_project.clone(), &bt).unwrap();
        let emp: BTreeSet<String> = ["emp".to_owned()].into_iter().collect();
        assert_eq!(quals_of(&typed.resolved_schema), vec![emp; 4]);

        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(star_project),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("emp", "id")),
                right: Box::new(int_lit(1)),
            }),
        });
        analyze(filter, &bt).expect("qualified reference over the star-projected scope resolves");
    }

    #[test]
    fn source_quals_with_columns_renamed_clears_lineage_on_renamed_slot_only() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "t".to_owned(),
        });
        let renamed = CommonAst::new(CommonOp::WithColumnsRenamed {
            input: Box::new(aliased),
            renames: vec![("id".to_owned(), "y".to_owned())],
        });
        let typed = analyze(renamed, &bt).unwrap();
        let t: BTreeSet<String> = ["t".to_owned()].into_iter().collect();
        let renamed_field = typed
            .resolved_schema
            .field_by_name("y")
            .expect("renamed slot present under its new name");
        assert!(
            renamed_field.source_quals.is_empty(),
            "renamed slot must lose its inherited qualifier lineage"
        );
        let unrenamed_field = typed
            .resolved_schema
            .field_by_name("name")
            .expect("unrenamed slot present under its original name");
        assert_eq!(unrenamed_field.source_quals, t);
    }

    #[test]
    fn resolve_column_with_columns_renamed_pre_rename_qualifier_rejects_renamed_slot() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "t".to_owned(),
        });
        let renamed = CommonAst::new(CommonOp::WithColumnsRenamed {
            input: Box::new(aliased),
            renames: vec![("id".to_owned(), "y".to_owned())],
        });

        let filter_unrenamed = CommonAst::new(CommonOp::Filter {
            input: Box::new(renamed.clone()),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("t", "name")),
                right: Box::new(lit_str("acme")),
            }),
        });
        analyze(filter_unrenamed, &bt).expect("t.other (unrenamed slot) must resolve");

        let filter_renamed = CommonAst::new(CommonOp::Filter {
            input: Box::new(renamed),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("t", "y")),
                right: Box::new(int_lit(1)),
            }),
        });
        let err = analyze(filter_renamed, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "y");
                assert_eq!(qualifier.as_deref(), Some("t"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn source_quals_to_df_clears_lineage_on_every_slot() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "t".to_owned(),
        });
        let to_df = CommonAst::new(CommonOp::ToDf {
            input: Box::new(aliased),
            column_names: vec![
                "c0".to_owned(),
                "c1".to_owned(),
                "c2".to_owned(),
                "c3".to_owned(),
            ],
        });
        let typed = analyze(to_df, &bt).unwrap();
        for f in &typed.resolved_schema.fields {
            assert!(
                f.source_quals.is_empty(),
                "toDF renames every slot, so every slot must lose its inherited \
                 qualifier lineage — `{}` kept {:?}",
                f.name,
                f.source_quals
            );
        }
    }

    #[test]
    fn resolve_column_to_df_pre_rename_qualifier_rejects_renamed_slot() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "t".to_owned(),
        });
        let to_df = CommonAst::new(CommonOp::ToDf {
            input: Box::new(aliased),
            column_names: vec![
                "c0".to_owned(),
                "c1".to_owned(),
                "c2".to_owned(),
                "c3".to_owned(),
            ],
        });
        let filter_renamed = CommonAst::new(CommonOp::Filter {
            input: Box::new(to_df),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("t", "c0")),
                right: Box::new(int_lit(1)),
            }),
        });
        let err = analyze(filter_renamed, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "c0");
                assert_eq!(qualifier.as_deref(), Some("t"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_sql_aliased_column_list_still_resolves_after_to_df_clear() {
        let bt = base_types_with_emp_dept();
        let to_df = CommonAst::new(CommonOp::ToDf {
            input: Box::new(emp_scan()),
            column_names: vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned(),
            ],
        });
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(to_df),
            alias: "x".to_owned(),
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(aliased),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("x", "a")),
                right: Box::new(int_lit(1)),
            }),
        });
        analyze(filter, &bt).expect("`x.a` must resolve — AliasedRelation re-seeds after ToDf");
    }

    #[test]
    fn resolve_column_set_op_first_child_qualifier_resolves_second_child_rejects() {
        let bt = base_types_with_emp_dept();
        let a = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "a".to_owned(),
        });
        let b = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "b".to_owned(),
        });
        let union = set_op(SetOpKind::Union, true, vec![a, b]);

        let filter_a = CommonAst::new(CommonOp::Filter {
            input: Box::new(union.clone()),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("a", "id")),
                right: Box::new(int_lit(1)),
            }),
        });
        analyze(filter_a, &bt).expect("a.id (first child's qualifier) must resolve");

        let filter_b = CommonAst::new(CommonOp::Filter {
            input: Box::new(union),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("b", "id")),
                right: Box::new(int_lit(1)),
            }),
        });
        let err = analyze(filter_b, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "id");
                assert_eq!(qualifier.as_deref(), Some("b"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn authoritative_lineage_values_rejects_bogus_qualifier() {
        let bt = base_types_with_emp_dept();
        let values = values_row(&[("x", DataType::Integer, LiteralValue::Int(1))]);
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(values),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("v", "x")),
                right: Box::new(int_lit(1)),
            }),
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "x");
                assert_eq!(qualifier.as_deref(), Some("v"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn authoritative_lineage_file_scan_rejects_bogus_qualifier() {
        let bt = base_types_with_emp_dept();
        let file_scan = CommonAst::new(CommonOp::FileScan {
            format: FileFormat::Parquet,
            paths: vec!["/tmp/x.parquet".to_owned()],
            schema: Some(StructType::new(vec![StructField::nullable(
                "x",
                DataType::Integer,
            )])),
            options: vec![],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(file_scan),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("v", "x")),
                right: Box::new(int_lit(1)),
            }),
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "x");
                assert_eq!(qualifier.as_deref(), Some("v"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_using_join_semi_and_inner_alike_left_qualifier_resolves() {
        let bt = base_types_with_emp_dept();
        for join_type in [JoinType::Inner, JoinType::LeftSemi] {
            let join = CommonAst::new(CommonOp::Join {
                left: Box::new(aliased_scan("emp", "e")),
                right: Box::new(aliased_scan("dept", "d")),
                join_type,
                condition: None,
                using_columns: vec!["dept_id".to_owned()],
                natural: false,
                lateral: false,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            });
            let filter = CommonAst::new(CommonOp::Filter {
                input: Box::new(join),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(qcol("e", "name")),
                    right: Box::new(lit_str("acme")),
                }),
            });
            analyze(filter, &bt).unwrap_or_else(|e| {
                panic!("expected {join_type:?} left qualifier to resolve, got {e:?}")
            });
        }
    }

    #[test]
    fn resolve_column_projected_through_qualifier_drops_qualifier() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased),
            projections: vec![qcol("e", "dept_id"), qcol("e", "name")],
        });
        let filter1 = CommonAst::new(CommonOp::Filter {
            input: Box::new(project),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(101)),
            }),
        });
        let filter2 = CommonAst::new(CommonOp::Filter {
            input: Box::new(filter1),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "name")),
                right: Box::new(lit_str("x")),
            }),
        });
        let typed = analyze(filter2, &bt).expect("filt-018 shape must resolve");
        match &typed.op {
            TypedOp::Filter {
                input,
                condition: Expression::Binary(b),
            } => match b.left.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier, None);
                    assert_eq!(c.expr_id, Some(input.resolved_schema.fields[1].expr_id));
                    assert_eq!(c.name, "name");
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            other => panic!("expected Filter/Binary, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_created_alias_authoritative_empty_rejects() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased),
            projections: vec![alias_expr(unresolved_col("dept_id"), "k")],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(project),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "k")),
                right: Box::new(int_lit(101)),
            }),
        });
        let err = analyze(filter, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "k");
                assert_eq!(qualifier.as_deref(), Some("e"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
        assert_eq!(
            AnalyzerError::UnknownColumn {
                name: "k".to_owned(),
                qualifier: Some("e".to_owned()),
            }
            .spark_class(),
            Some("UNRESOLVED_COLUMN.WITH_SUGGESTION")
        );
    }

    #[test]
    fn resolve_column_drop_columns_surviving_qualifier_drops_qualifier() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let dropped = CommonAst::new(CommonOp::DropColumns {
            input: Box::new(aliased),
            drop_names: vec!["salary".to_owned()],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(dropped),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(101)),
            }),
        });
        let typed = analyze(filter, &bt).expect("surviving qualified column must resolve");
        match &typed.op {
            TypedOp::Filter {
                input,
                condition: Expression::Binary(b),
            } => match b.left.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier, None);
                    assert_eq!(
                        c.expr_id,
                        Some(
                            input
                                .resolved_schema
                                .field_by_name("dept_id")
                                .expect("dept_id survives drop")
                                .expr_id
                        )
                    );
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            other => panic!("expected Filter/Binary, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_with_columns_untouched_qualifier_drops_qualifier() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let with_active = CommonAst::new(CommonOp::WithColumns {
            input: Box::new(aliased),
            assignments: vec![("active".to_owned(), lit_bool(true))],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(with_active),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(101)),
            }),
        });
        let typed = analyze(filter, &bt).expect("untouched qualified column must resolve");
        match &typed.op {
            TypedOp::Filter {
                input,
                condition: Expression::Binary(b),
            } => match b.left.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier, None);
                    assert_eq!(
                        c.expr_id,
                        Some(
                            input
                                .resolved_schema
                                .field_by_name("dept_id")
                                .expect("dept_id untouched by withColumns")
                                .expr_id
                        )
                    );
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            other => panic!("expected Filter/Binary, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_with_columns_created_qualified_rejects() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let with_active = CommonAst::new(CommonOp::WithColumns {
            input: Box::new(aliased),
            assignments: vec![("active".to_owned(), lit_bool(true))],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(with_active),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "active")),
                right: Box::new(lit_bool(true)),
            }),
        });
        let err = analyze(filter, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "active");
                assert_eq!(qualifier.as_deref(), Some("e"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_unpivot_id_column_qualifier_drops_qualifier() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let unpivoted = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(aliased),
            ids: UnpivotIds::Explicit(vec!["id".to_owned(), "name".to_owned()]),
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "val".to_owned(),
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(unpivoted),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "id")),
                right: Box::new(int_lit(1)),
            }),
        });
        let typed = analyze(filter, &bt).expect("id column must resolve via qualifier");
        match &typed.op {
            TypedOp::Filter {
                input,
                condition: Expression::Binary(b),
            } => match b.left.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier, None);
                    assert_eq!(
                        c.expr_id,
                        Some(
                            input
                                .resolved_schema
                                .field_by_name("id")
                                .expect("id preserved by unpivot")
                                .expr_id
                        )
                    );
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            other => panic!("expected Filter/Binary, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_unpivot_variable_column_qualified_rejects() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let unpivoted = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(aliased),
            ids: UnpivotIds::Explicit(vec!["id".to_owned(), "name".to_owned()]),
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "val".to_owned(),
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(unpivoted),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "metric")),
                right: Box::new(lit_str("dept_id")),
            }),
        });
        let err = analyze(filter, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "metric");
                assert_eq!(qualifier.as_deref(), Some("e"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_pivot_grouping_column_qualifier_drops_qualifier() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let pivoted = CommonAst::new(CommonOp::Pivot {
            input: Box::new(aliased),
            grouping: PivotGrouping::Explicit(vec![qcol("e", "dept_id")]),
            pivot_column: qcol("e", "name"),
            pivot_values: vec![lit_str("Alice"), lit_str("Bob")],
            aggregates: vec![alias_expr(func("count", vec![int_lit(1)]), "n")],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(pivoted),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(101)),
            }),
        });
        let typed = analyze(filter, &bt).expect("grouping column must resolve via qualifier");
        match &typed.op {
            TypedOp::Filter {
                input,
                condition: Expression::Binary(b),
            } => match b.left.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier, None);
                    assert_eq!(
                        c.expr_id,
                        Some(
                            input
                                .resolved_schema
                                .field_by_name("dept_id")
                                .expect("dept_id is the grouping column")
                                .expr_id
                        )
                    );
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            other => panic!("expected Filter/Binary, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_pivot_value_column_qualified_rejects() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let pivoted = CommonAst::new(CommonOp::Pivot {
            input: Box::new(aliased),
            grouping: PivotGrouping::Explicit(vec![qcol("e", "dept_id")]),
            pivot_column: qcol("e", "name"),
            pivot_values: vec![lit_str("Alice"), lit_str("Bob")],
            aggregates: vec![alias_expr(func("count", vec![int_lit(1)]), "n")],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(pivoted),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "Alice")),
                right: Box::new(int_lit(1)),
            }),
        });
        let err = analyze(filter, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "Alice");
                assert_eq!(qualifier.as_deref(), Some("e"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_correlated_outer_ref_preserves_qualifier() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(qcol("d", "budget")),
                    right: Box::new(qcol("e", "salary")),
                }),
            })),
            projections: vec![qcol("d", "dept_id")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(qcol("e", "dept_id")),
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let typed = analyze(ast, &bt).expect("correlated outer ref must resolve");
        match &typed.op {
            TypedOp::Filter {
                condition: Expression::InSubquery(in_sq),
                ..
            } => match &in_sq.subquery {
                SubqueryPlan::Analyzed(inner_typed) => match &inner_typed.op {
                    TypedOp::Project {
                        input: inner_filter,
                        ..
                    } => match &inner_filter.op {
                        TypedOp::Filter {
                            condition: Expression::Binary(b),
                            ..
                        } => match b.right.as_ref() {
                            Expression::ColumnReference(c) => {
                                assert_eq!(c.qualifier.as_deref(), Some("e"));
                                assert_eq!(c.name, "salary");
                                assert_eq!(c.data_type, DataType::Double);
                            }
                            other => panic!("expected ColumnReference, got {other:?}"),
                        },
                        other => panic!("expected inner Filter/Binary, got {other:?}"),
                    },
                    other => panic!("expected inner Project, got {other:?}"),
                },
                other => panic!("expected Analyzed subquery, got {other:?}"),
            },
            other => panic!("expected Filter/InSubquery, got {other:?}"),
        }
    }

    #[test]
    fn resolve_in_outer_struct_arm_becomes_extract_value_chain_rooted_at_outer_id() {
        let addr_ty = DataType::Struct(StructType::new(vec![StructField::nullable(
            "city",
            DataType::String,
        )]));
        let bt = base_types_for(&[
            (
                "emp",
                StructType::new(vec![
                    StructField::not_null("id", DataType::Long),
                    StructField::nullable("address", addr_ty),
                ]),
            ),
            (
                "dept",
                StructType::new(vec![
                    StructField::not_null("dept_id", DataType::Integer),
                    StructField::nullable("location", DataType::String),
                ]),
            ),
        ]);
        let inner = CommonAst::new(CommonOp::Filter {
            input: Box::new(scan("dept")),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("dept", "location")),
                right: Box::new(qcol("address", "city")),
            }),
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            condition: Expression::ExistsSubquery(ExistsSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let typed = analyze(ast, &bt).expect("correlated outer struct-field ref must resolve");
        let outer_address_id = typed
            .resolved_schema
            .field_by_name("address")
            .expect("emp has an address field")
            .expr_id;
        match &typed.op {
            TypedOp::Filter {
                condition: Expression::ExistsSubquery(e),
                ..
            } => match &e.subquery {
                SubqueryPlan::Analyzed(inner_typed) => match &inner_typed.op {
                    TypedOp::Filter {
                        condition: Expression::Binary(b),
                        ..
                    } => match b.right.as_ref() {
                        Expression::ExtractValue(ev) => {
                            match ev.child.as_ref() {
                                Expression::ColumnReference(c) => {
                                    assert_eq!(c.name, "address");
                                    assert!(c.qualifier.is_none(), "chain root is unqualified");
                                    assert_eq!(
                                        c.expr_id,
                                        Some(outer_address_id),
                                        "chain root must carry the OUTER address attribute's own id"
                                    );
                                }
                                other => {
                                    panic!(
                                        "expected root ColumnReference('address'), got {other:?}"
                                    )
                                }
                            }
                            match ev.extraction.as_ref() {
                                Expression::Literal(Literal {
                                    value: LiteralValue::String(s),
                                    ..
                                }) => assert_eq!(s, "city"),
                                other => panic!("expected String literal 'city', got {other:?}"),
                            }
                        }
                        other => panic!("expected ExtractValue, got {other:?}"),
                    },
                    other => panic!("expected inner Filter, got {other:?}"),
                },
                other => panic!("expected Analyzed subquery, got {other:?}"),
            },
            other => panic!("expected Filter/ExistsSubquery, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_using_join_qualified_ref_resolves() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Join {
                left: Box::new(aliased_scan("emp", "e")),
                right: Box::new(aliased_scan("dept", "d")),
                join_type: JoinType::Inner,
                condition: None,
                using_columns: vec!["dept_id".to_owned()],
                natural: false,
                lateral: false,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            })),
            projections: vec![qcol("e", "name")],
        });
        let typed = analyze(ast, &bt).expect("USING join qualified ref must still resolve");
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.name, "name");
                    assert_eq!(c.data_type, DataType::String);
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn resolve_column_project_of_columns_qualifier_resolves() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased),
            projections: vec![qcol("e", "dept_id"), qcol("e", "name")],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(project),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(101)),
            }),
        });
        analyze(filter, &bt).expect("projected qualified column must resolve");
    }

    #[test]
    fn resolve_column_using_join_with_tracked_children_left_qualifier_resolves() {
        let bt = base_types_with_emp_dept();
        let using_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(using_join),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "name")),
                right: Box::new(lit_str("acme")),
            }),
        });
        analyze(filter, &bt).expect("USING join left qualifier must resolve");
    }

    fn inner_select_col(col: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![unresolved_col(col)],
        })
    }

    #[test]
    fn scalar_subquery_types_to_inner_single_column_and_becomes_analyzed() {
        let bt = base_types_with_emp_dept();
        let scalar = Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(inner_select_col("id"))),
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![scalar],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
        assert!(typed.resolved_schema.fields[0].nullable);
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::Alias(a) => {
                    assert_eq!(a.alias, "expr");
                    match a.expr.as_ref() {
                        Expression::ScalarSubquery(s) => {
                            assert!(
                                matches!(s.subquery, SubqueryPlan::Analyzed(_)),
                                "analyzer must rewrite Unanalyzed → Analyzed"
                            );
                        }
                        other => panic!("expected ScalarSubquery, got {other:?}"),
                    }
                }
                other => panic!("expected Alias (N8), got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn scalar_subquery_grouped_single_column_not_restated_analyzes_ok() {
        let bt = base_types_with_emp_dept();
        let inner = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(emp_scan()),
            grouping: vec![unresolved_col("dept_id")],
            aggregates: vec![alias_expr(
                func("avg", vec![unresolved_col("salary")]),
                "rank_col",
            )],
            grouping_kind: GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::Alias(a) => {
                    assert_eq!(a.alias, "expr");
                    match a.expr.as_ref() {
                        Expression::ScalarSubquery(s) => match &s.subquery {
                            SubqueryPlan::Analyzed(inner) => {
                                assert_eq!(inner.resolved_schema.fields.len(), 1);
                                assert_eq!(inner.resolved_schema.fields[0].name, "rank_col");
                                assert!(
                                    matches!(inner.op, TypedOp::Aggregate { .. }),
                                    "Folded aggregate needs no wrapping Project, got {:?}",
                                    inner.op
                                );
                            }
                            other => panic!("expected Analyzed, got {other:?}"),
                        },
                        other => panic!("expected ScalarSubquery, got {other:?}"),
                    }
                }
                other => panic!("expected Alias (N8), got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn scalar_subquery_grouped_two_columns_is_still_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let inner = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(emp_scan()),
            grouping: vec![unresolved_col("dept_id")],
            aggregates: vec![
                unresolved_col("dept_id"),
                alias_expr(func("avg", vec![unresolved_col("salary")]), "rank_col"),
            ],
            grouping_kind: GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(
            err,
            AnalyzerError::SparkEmulated {
                class:
                    "INVALID_SUBQUERY_EXPRESSION.SCALAR_SUBQUERY_RETURN_MORE_THAN_ONE_OUTPUT_COLUMN",
                ..
            }
        ));
    }

    #[test]
    fn scalar_subquery_two_columns_is_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![unresolved_col("id"), unresolved_col("salary")],
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(
            err,
            AnalyzerError::SparkEmulated {
                class:
                    "INVALID_SUBQUERY_EXPRESSION.SCALAR_SUBQUERY_RETURN_MORE_THAN_ONE_OUTPUT_COLUMN",
                ..
            }
        ));
    }

    #[test]
    fn exists_subquery_over_dept_analyzes_and_stays_boolean() {
        let bt = base_types_with_emp_dept();
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![unresolved_col("dept_id")],
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
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![unresolved_col("not_in_dept")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(emp_scan()),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(unresolved_col("dept_id")),
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    fn dept_schema_with_budget() -> StructType {
        StructType::new(vec![
            StructField::not_null("dept_id", DataType::Integer),
            StructField::nullable("dept_name", DataType::String),
            StructField::nullable("budget", DataType::Double),
        ])
    }

    #[test]
    fn correlated_in_subquery_outer_ref_absent_from_inner_resolves() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(qcol("d", "budget")),
                    right: Box::new(qcol("e", "salary")),
                }),
            })),
            projections: vec![qcol("d", "dept_id")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(qcol("e", "dept_id")),
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let typed = analyze(ast, &bt).expect("sq-010 must resolve");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn correlated_exists_subquery_outer_ref_absent_from_inner_resolves() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let inner = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("dept")),
                alias: "d".to_owned(),
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(qcol("d", "budget")),
                right: Box::new(qcol("e", "salary")),
            }),
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            condition: Expression::ExistsSubquery(ExistsSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let typed = analyze(ast, &bt).expect("EXISTS with outer ref must resolve");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn correlated_scalar_subquery_outer_ref_absent_from_inner_resolves() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::And,
                    left: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Eq,
                        left: Box::new(qcol("d", "dept_id")),
                        right: Box::new(qcol("e", "dept_id")),
                    })),
                    right: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Gt,
                        left: Box::new(qcol("d", "budget")),
                        right: Box::new(qcol("e", "salary")),
                    })),
                }),
            })),
            projections: vec![func("max", vec![qcol("d", "budget")])],
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        let typed = analyze(ast, &bt).expect("scalar subquery with outer ref must resolve");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
    }

    #[test]
    fn correlated_in_subquery_emission_preserves_outer_qualifier() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let inner_filter = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(qcol("d", "budget")),
                    right: Box::new(qcol("e", "salary")),
                }),
            })),
            projections: vec![qcol("d", "dept_id")],
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("emp")),
                    alias: "e".to_owned(),
                })),
                condition: Expression::InSubquery(InSubquery {
                    expr: Box::new(qcol("e", "dept_id")),
                    subquery: SubqueryPlan::Unanalyzed(Box::new(inner_filter)),
                    negated: false,
                }),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let typed = analyze(ast, &bt).expect("sq-010 must resolve");
        let sql = super::super::emission::dispatch_op(&typed.op, &typed.resolved_schema).unwrap();
        assert!(
            sql.contains("e.salary"),
            "emission must preserve outer qualifier `e` on `salary`; got:\n{sql}"
        );
    }

    #[test]
    fn two_level_nested_correlation_to_grandparent_still_fails() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let innermost = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("dept")),
                alias: "d2".to_owned(),
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("d2", "dept_id")),
                right: Box::new(qcol("e", "salary")),
            }),
        });
        let middle = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d1".to_owned(),
                })),
                condition: Expression::ExistsSubquery(ExistsSubquery {
                    subquery: SubqueryPlan::Unanalyzed(Box::new(innermost)),
                    negated: false,
                }),
            })),
            projections: vec![qcol("d1", "dept_id")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(qcol("e", "dept_id")),
                subquery: SubqueryPlan::Unanalyzed(Box::new(middle)),
                negated: false,
            }),
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(
            matches!(err, AnalyzerError::UnknownColumn { .. }),
            "two-level nested correlation to grandparent must fail; got: {err:?}"
        );
    }

    #[test]
    fn correlated_subquery_mismatched_qualifier_resolves_as_outer_reference() {
        let emp_long_dept_id = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Long), // Long, not Integer
            StructField::nullable("salary", DataType::Double),
        ]);
        let bt = base_types_for(&[("emp", emp_long_dept_id), ("dept", dept_schema())]);
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(qcol("d", "dept_id")),
                    right: Box::new(qcol("e", "dept_id")),
                }),
            })),
            projections: vec![unresolved_col("dept_id")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(qcol("e", "dept_id")),
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let typed = analyze(ast, &bt).expect("inner-precedence case must resolve");
        let inner_cond = match &typed.op {
            TypedOp::Filter { condition, .. } => match condition {
                Expression::InSubquery(i) => match &i.subquery {
                    SubqueryPlan::Analyzed(inner_typed) => match &inner_typed.op {
                        TypedOp::Project {
                            input: inner_filter,
                            ..
                        } => match &inner_filter.op {
                            TypedOp::Filter { condition: c, .. } => c,
                            other => panic!("expected inner Filter, got {other:?}"),
                        },
                        other => panic!("expected inner Project, got {other:?}"),
                    },
                    SubqueryPlan::Unanalyzed(_) => panic!("subquery must be analyzed"),
                },
                other => panic!("expected InSubquery, got {other:?}"),
            },
            other => panic!("expected outer Filter, got {other:?}"),
        };
        match inner_cond {
            Expression::Binary(b) => match &*b.right {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier.as_deref(), Some("e"));
                    assert_eq!(c.name, "dept_id");
                    assert_eq!(
                        c.data_type,
                        DataType::Long,
                        "mismatched qualifier must resolve as a correlated outer reference"
                    );
                }
                other => panic!("expected ColumnReference for e.dept_id, got {other:?}"),
            },
            other => panic!("expected Binary condition, got {other:?}"),
        }
    }

    #[test]
    fn correlated_subquery_qualified_outer_ref_with_unbound_qualifier_fails() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(qcol("d", "budget")),
                    right: Box::new(qcol("bogus", "salary")),
                }),
            })),
            projections: vec![qcol("d", "dept_id")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(scan("emp")),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(unresolved_col("dept_id")),
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(
            matches!(err, AnalyzerError::UnknownColumn { .. }),
            "qualified outer ref with unbound qualifier must fail; got: {err:?}"
        );
    }

    #[test]
    fn assign_types_stamps_column_reference_type_and_nullability() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("id")],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.data_type, DataType::Long);
                    assert!(!c.nullable);
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
            input: Box::new(scan("emp")),
            condition: int_lit(42),
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::TypeMismatch { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    fn analyze_emp_dept_join(
        jt: JoinType,
        cond: Option<Expression>,
    ) -> (ResolvedSchema, ResolvedSchema, ResolvedSchema) {
        let bt = base_types_with_emp_dept();
        let ast = join(scan("emp"), scan("dept"), jt, cond);
        let typed = analyze(ast, &bt).unwrap();
        let resolved = typed.resolved_schema;
        match &typed.op {
            TypedOp::Join { left, .. } => {
                let left_len = left.resolved_schema.len();
                let flipped_left = ResolvedSchema::new(resolved.fields[..left_len].to_vec());
                let flipped_right = ResolvedSchema::new(resolved.fields[left_len..].to_vec());
                (flipped_left, flipped_right, resolved)
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn left_outer_join_flips_right_side_nullability() {
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("emp", "dept_id")),
            right: Box::new(qcol("dept", "dept_id")),
        });
        let (left, right, _) = analyze_emp_dept_join(JoinType::Left, Some(cond));
        assert!(!left.field_by_name("id").unwrap().nullable);
        assert!(right.field_by_name("dept_id").unwrap().nullable);
    }

    #[test]
    fn right_outer_join_flips_left_side_nullability() {
        let (left, right, _) = analyze_emp_dept_join(JoinType::Right, None);
        assert!(left.field_by_name("id").unwrap().nullable);
        assert!(!right.field_by_name("dept_id").unwrap().nullable);
    }

    #[test]
    fn full_outer_join_flips_both_sides() {
        let (left, right, _) = analyze_emp_dept_join(JoinType::Full, None);
        assert!(left.field_by_name("id").unwrap().nullable);
        assert!(right.field_by_name("dept_id").unwrap().nullable);
    }

    #[test]
    fn inner_join_preserves_both_sides_nullability() {
        let (left, right, _) = analyze_emp_dept_join(JoinType::Inner, None);
        assert!(!left.field_by_name("id").unwrap().nullable);
        assert!(!right.field_by_name("dept_id").unwrap().nullable);
    }

    #[test]
    fn left_semi_join_drops_right_side() {
        let (left, right, resolved) = analyze_emp_dept_join(JoinType::LeftSemi, None);
        assert_eq!(left, emp_schema());
        assert!(right.is_empty());
        assert_eq!(resolved, emp_schema());
    }

    fn natural_join(left: CommonAst, right: CommonAst, join_type: JoinType) -> CommonAst {
        CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type,
            condition: None,
            using_columns: vec![],
            natural: true,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        })
    }

    fn dept_no_overlap_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("division", DataType::Integer),
            StructField::nullable("location", DataType::String),
        ])
    }

    fn emp_uppercase_dept_id_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("DEPT_ID", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ])
    }

    #[test]
    fn natural_inner_join_converges_with_equivalent_using_join() {
        let bt = base_types_with_emp_dept();
        let natural_ast = natural_join(scan("emp"), scan("dept"), JoinType::Inner);
        let using_ast = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let natural_typed = analyze(natural_ast, &bt).expect("natural analyze");
        let using_typed = analyze(using_ast, &bt).expect("using analyze");
        assert_eq!(natural_typed, using_typed);

        assert_eq!(
            field_names(&natural_typed),
            vec!["dept_id", "id", "name", "salary", "dept_name"]
        );
        let dept_id_field = natural_typed
            .resolved_schema
            .field_by_name("dept_id")
            .expect("dept_id present");
        assert_eq!(dept_id_field.data_type, DataType::Integer);
        assert!(
            dept_id_field.nullable,
            "dept_id is nullable in emp (left donor)"
        );
    }

    #[test]
    fn natural_left_join_left_donor_nullability() {
        let bt = base_types_with_emp_dept();
        let ast = natural_join(scan("emp"), scan("dept"), JoinType::Left);
        let typed = analyze(ast, &bt).expect("natural left analyze");
        assert_eq!(
            field_names(&typed),
            vec!["dept_id", "id", "name", "salary", "dept_name"]
        );
        let dept_id_field = typed
            .resolved_schema
            .field_by_name("dept_id")
            .expect("dept_id present");
        assert!(dept_id_field.nullable, "LEFT donor nullability preserved");
    }

    #[test]
    fn natural_full_join_nullability_is_post_flip_and_of_both_sides() {
        let bt = base_types_with_emp_dept();
        let ast = natural_join(scan("emp"), scan("dept"), JoinType::Full);
        let typed = analyze(ast, &bt).expect("natural full analyze");
        for f in &typed.resolved_schema.fields {
            assert!(f.nullable, "field {} must be nullable under FULL", f.name);
        }
    }

    #[test]
    fn natural_inner_join_no_common_columns_rewrites_to_cross() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept2", dept_no_overlap_schema())]);
        let ast = natural_join(scan("emp"), scan("dept2"), JoinType::Inner);
        let typed = analyze(ast, &bt).expect("natural inner no-common analyze");
        match &typed.op {
            TypedOp::Join {
                join_type,
                condition,
                using_columns,
                ..
            } => {
                assert_eq!(*join_type, JoinType::Cross);
                assert!(condition.is_none());
                assert!(using_columns.is_empty());
            }
            other => panic!("expected Join, got {other:?}"),
        }
        assert_eq!(typed.resolved_schema.fields.len(), 6);
    }

    #[test]
    fn natural_left_join_no_common_columns_rewrites_to_true_condition() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept2", dept_no_overlap_schema())]);
        let ast = natural_join(scan("emp"), scan("dept2"), JoinType::Left);
        let typed = analyze(ast, &bt).expect("natural left no-common analyze");
        match &typed.op {
            TypedOp::Join {
                join_type,
                condition,
                using_columns,
                ..
            } => {
                assert_eq!(*join_type, JoinType::Left);
                assert!(using_columns.is_empty());
                match condition {
                    Some(Expression::Literal(Literal {
                        value: LiteralValue::Boolean(true),
                        data_type: DataType::Boolean,
                    })) => {}
                    other => panic!("expected TRUE literal condition, got {other:?}"),
                }
            }
            other => panic!("expected Join, got {other:?}"),
        }
        let division_field = typed
            .resolved_schema
            .field_by_name("division")
            .expect("division present");
        assert!(
            division_field.nullable,
            "right side flipped nullable under LEFT"
        );
    }

    #[test]
    fn natural_semi_join_is_rejected_as_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let ast = natural_join(scan("emp"), scan("dept"), JoinType::LeftSemi);
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::Other { ref reason } => {
                assert!(
                    reason.contains("Unsupported natural join type LeftSemi"),
                    "got: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn natural_anti_join_is_rejected_as_spark_emulated_error() {
        let bt = base_types_with_emp_dept();
        let ast = natural_join(scan("emp"), scan("dept"), JoinType::LeftAnti);
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::Other { ref reason } => {
                assert!(
                    reason.contains("Unsupported natural join type LeftAnti"),
                    "got: {reason}"
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn natural_join_case_sensitive_witness_both_columns_survive() {
        let bt = base_types_for(&[
            ("emp_uc", emp_uppercase_dept_id_schema()),
            ("dept", dept_schema()),
        ]);
        let ast = natural_join(scan("emp_uc"), scan("dept"), JoinType::Inner);
        let typed = analyze(ast, &bt).expect("natural case-witness analyze");
        match &typed.op {
            TypedOp::Join { join_type, .. } => assert_eq!(*join_type, JoinType::Cross),
            other => panic!("expected Join, got {other:?}"),
        }
        assert_eq!(typed.resolved_schema.fields.len(), 6);
        assert!(typed
            .resolved_schema
            .fields
            .iter()
            .any(|f| f.name == "DEPT_ID"));
        assert!(typed
            .resolved_schema
            .fields
            .iter()
            .any(|f| f.name == "dept_id"));
    }

    #[test]
    fn ambiguous_column_across_joins_surfaces_error() {
        let bt = base_types_with_emp_dept();
        let cond = unresolved_col("dept_id");
        let ast = join(scan("emp"), scan("dept"), JoinType::Inner, Some(cond));
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::AmbiguousColumn { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    #[test]
    fn resolve_column_projection_ambiguous_across_join_errors() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(scan("emp"), scan("dept"), JoinType::Inner, None)),
            projections: vec![unresolved_col("dept_id")],
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
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(scan("emp"), scan("dept"), JoinType::Inner, None)),
            projections: vec![unresolved_col("salary")],
        });
        let typed = analyze(ast, &bt).unwrap();
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.name, "salary");
                    assert_eq!(c.data_type, DataType::Double);
                }
                _ => panic!("expected resolved ColumnReference"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn qualified_star_with_unknown_qualifier_errors() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
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

    fn tiny_int_plan() -> CommonAst {
        values_row(&[("x", DataType::Integer, LiteralValue::Int(1))])
    }

    fn tiny_double_plan() -> CommonAst {
        values_row(&[("x", DataType::Double, LiteralValue::Double(1.5))])
    }

    fn tiny_long_plan() -> CommonAst {
        values_row(&[("x", DataType::Long, LiteralValue::Long(1))])
    }

    #[test]
    fn setop_union_widens_int_and_double_to_double() {
        let bt = BaseTypes::empty();
        let ast = set_op(
            SetOpKind::Union,
            true,
            vec![tiny_int_plan(), tiny_double_plan()],
        );
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
        let ast = set_op(
            SetOpKind::Intersect,
            false,
            vec![tiny_int_plan(), tiny_long_plan()],
        );
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
        let ast = set_op(SetOpKind::Except, false, vec![short_plan, tiny_long_plan()]);
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
    fn values_ragged_rows_returns_error_not_panic() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::Values {
            rows: vec![vec![int_lit(1), int_lit(2)], vec![int_lit(3)]],
            column_names: vec!["a".to_owned(), "b".to_owned()],
        });
        let err = analyze(ast, &bt).expect_err("ragged VALUES must be rejected");
        assert!(
            matches!(
                err,
                AnalyzerError::SparkEmulated {
                    class: "INVALID_INLINE_TABLE.NUM_COLUMNS_MISMATCH",
                    ..
                }
            ),
            "expected SparkEmulated INVALID_INLINE_TABLE.NUM_COLUMNS_MISMATCH, got {err:?}"
        );
    }

    fn dept_id_from(table: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(scan(table)),
            projections: vec![unresolved_col("dept_id")],
        })
    }

    #[test]
    fn setop_intersect_nullability_is_and_across_children() {
        let bt = base_types_with_emp_dept();
        let ast = set_op(
            SetOpKind::Intersect,
            false,
            vec![dept_id_from("emp"), dept_id_from("dept")],
        );
        let typed = analyze(ast, &bt).unwrap();
        assert!(
            !typed.resolved_schema.fields[0].nullable,
            "INTERSECT of nullable ∩ non-nullable must be non-nullable (AND)"
        );
    }

    #[test]
    fn setop_except_nullability_is_left_child_only_nonnull_left() {
        let bt = base_types_with_emp_dept();
        let ast = set_op(
            SetOpKind::Except,
            false,
            vec![dept_id_from("dept"), dept_id_from("emp")],
        );
        let typed = analyze(ast, &bt).unwrap();
        assert!(
            !typed.resolved_schema.fields[0].nullable,
            "EXCEPT must take the non-nullable LEFT child's nullability, ignoring the nullable right"
        );
    }

    #[test]
    fn setop_except_nullability_is_left_child_only_nullable_left() {
        let bt = base_types_with_emp_dept();
        let ast = set_op(
            SetOpKind::Except,
            false,
            vec![dept_id_from("emp"), dept_id_from("dept")],
        );
        let typed = analyze(ast, &bt).unwrap();
        assert!(
            typed.resolved_schema.fields[0].nullable,
            "EXCEPT must take the nullable LEFT child's nullability, ignoring the non-nullable right"
        );
    }

    #[test]
    fn setop_union_nullability_is_or_across_children() {
        let bt = base_types_with_emp_dept();
        let ast = set_op(
            SetOpKind::Union,
            true,
            vec![dept_id_from("emp"), dept_id_from("dept")],
        );
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
            rows: vec![vec![int_lit(1), int_lit(2)]],
            column_names: vec!["x".to_owned(), "y".to_owned()],
        });
        let ast = set_op(SetOpKind::Union, true, vec![tiny_int_plan(), two_col]);
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::SparkEmulated {
                class: "NUM_COLUMNS_MISMATCH",
                ref reason,
            } => {
                assert!(
                    reason.contains("arity mismatch"),
                    "expected arity-mismatch message, got: {reason}",
                );
            }
            other => panic!("expected AnalyzerError::Other, got {other:?}"),
        }
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    fn project_col(table: &str, col: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(scan(table)),
            projections: vec![unresolved_col(col)],
        })
    }

    #[test]
    fn setop_union_three_way_widening_aliases_and_casts_mismatched_branch() {
        let ast = set_op(
            SetOpKind::Union,
            true,
            vec![
                project_col("emp", "id"),
                project_col("emp2", "id"),
                project_col("dept", "dept_id"),
            ],
        );
        let bt = base_types_for(&[
            ("emp", emp_schema()),
            (
                "emp2",
                StructType::new(vec![StructField::not_null("id", DataType::Long)]),
            ),
            ("dept", dept_schema()),
        ]);
        let typed = analyze(ast, &bt).unwrap();
        let sql = super::super::emission::dispatch_op(&typed.op, &typed.resolved_schema).unwrap();
        assert!(
            sql.contains("CAST(dept_id AS BIGINT) AS id"),
            "dept branch must cast INT→BIGINT and re-alias to widened name `id`; got:\n{sql}"
        );
    }

    #[test]
    fn setop_union_same_type_different_name_aliases_without_cast() {
        let staff = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("manager_id", DataType::Long),
        ]);
        let ast = set_op(
            SetOpKind::Union,
            true,
            vec![
                project_col("staff", "id"),
                project_col("staff", "manager_id"),
            ],
        );
        let bt = base_types_for(&[("staff", staff.clone())]);
        let typed = analyze(ast, &bt).unwrap();
        let sql = super::super::emission::dispatch_op(&typed.op, &typed.resolved_schema).unwrap();
        assert!(
            sql.contains("manager_id AS id"),
            "second branch must alias manager_id→id (widened name) without a cast; got:\n{sql}"
        );
        assert!(
            !sql.contains("CAST(manager_id"),
            "same-type branch must NOT introduce a cast; got:\n{sql}"
        );
    }

    #[test]
    fn setop_intersect_by_name_punts_with_boundary_prefix() {
        let bt = BaseTypes::empty();
        let ast = set_op_by_name(
            SetOpKind::Intersect,
            true,
            vec![tiny_int_plan(), tiny_int_plan()],
        );
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::PuntedOperator { .. }));
        assert!(err.to_string().starts_with("[TDCK-BOUNDARY]"));
    }

    #[test]
    fn table_function_range_resolves_single_long_id_column() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "range".to_owned(),
            args: vec![int_lit(5)],
            with_ordinality: false,
        });
        let typed = analyze(ast, &bt).expect("range should analyze");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        let f = &typed.resolved_schema.fields[0];
        assert_eq!(f.name, "id");
        assert_eq!(f.data_type, DataType::Long);
        assert!(!f.nullable, "range id column is non-nullable");
    }

    #[test]
    fn table_function_unknown_tvf_punts() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "some_unknown_tvf".to_owned(),
            args: vec![int_lit(1)],
            with_ordinality: false,
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::PuntedOperator { .. }));
        assert!(err.to_string().starts_with("[TDCK-BOUNDARY]"));
    }

    #[test]
    fn setop_union_by_name_skips_positional_cast_pushdown() {
        let bt = BaseTypes::empty();
        let left = CommonAst::new(CommonOp::Values {
            rows: vec![vec![int_lit(1), lit_str("a")]],
            column_names: vec!["x".to_owned(), "y".to_owned()],
        });
        let right = CommonAst::new(CommonOp::Values {
            rows: vec![vec![lit_str("b"), int_lit(2)]],
            column_names: vec!["y".to_owned(), "x".to_owned()],
        });
        let ast = set_op_by_name(SetOpKind::Union, true, vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
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
                    .map(|child| child.resolved_schema.clone())
                    .collect::<Vec<_>>(),
            ),
            other => panic!("expected SetOp, got {other:?}"),
        };
        assert_eq!(kind, SetOpKind::Union);
        assert!(by_name);
        assert_eq!(child_schemas[0].fields[0].name, "x");
        assert_eq!(child_schemas[0].fields[0].data_type, DataType::Integer);
        assert_eq!(child_schemas[1].fields[0].name, "y");
        assert_eq!(child_schemas[1].fields[0].data_type, DataType::String);
    }

    #[test]
    fn union_by_name_allow_missing_partial_overlap_produces_ordered_union() {
        let bt = BaseTypes::empty();
        let left = values_row(&[
            ("a", DataType::Long, LiteralValue::Long(1)),
            ("b", DataType::Long, LiteralValue::Long(2)),
        ]);
        let right = values_row(&[
            ("b", DataType::Long, LiteralValue::Long(3)),
            ("c", DataType::Long, LiteralValue::Long(4)),
        ]);
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
        assert_eq!(widened.fields.len(), 3);
        assert_eq!(widened.fields[0].name, "a");
        assert!(widened.fields[0].nullable, "a is padded on RIGHT");
        assert_eq!(widened.fields[1].name, "b");
        assert!(!widened.fields[1].nullable);
        assert_eq!(widened.fields[2].name, "c");
        assert!(widened.fields[2].nullable, "c is padded on LEFT");
    }

    #[test]
    fn union_by_name_allow_missing_disjoint_schemas() {
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
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
        let names: Vec<_> = widened.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d", "e", "f"]);
        assert!(widened.fields.iter().all(|f| f.nullable));
    }

    #[test]
    fn union_by_name_allow_missing_widens_shared_column_type() {
        let bt = BaseTypes::empty();
        let left = values_row(&[("x", DataType::Integer, LiteralValue::Int(1))]);
        let right = values_row(&[
            ("x", DataType::Double, LiteralValue::Double(1.5)),
            ("y", DataType::Integer, LiteralValue::Int(2)),
        ]);
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
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
    fn union_by_name_allow_missing_clears_quals_for_name_missing_from_first_child() {
        let bt = BaseTypes::empty();
        let left = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(values_row(&[
                ("x", DataType::Long, LiteralValue::Long(1)),
                ("y", DataType::Long, LiteralValue::Long(2)),
            ])),
            alias: "a".to_owned(),
        });
        let right = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(values_row(&[
                ("x", DataType::Long, LiteralValue::Long(10)),
                ("y", DataType::Long, LiteralValue::Long(20)),
                ("z", DataType::Long, LiteralValue::Long(30)),
            ])),
            alias: "b".to_owned(),
        });
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
        assert_eq!(widened.field_names(), vec!["x", "y", "z"]);
        let quals = quals_of(widened);
        let a: BTreeSet<String> = ["a".to_owned()].into_iter().collect();
        assert_eq!(
            quals[0], a,
            "x is present in the FIRST child; its donor qualifier (`a`) stays addressable"
        );
        assert_eq!(
            quals[1], a,
            "y is present in the FIRST child; its donor qualifier (`a`) stays addressable"
        );
        assert!(
            quals[2].is_empty(),
            "z is missing from the FIRST child (padded by Spark's null-alias \
             rule) — its donor's (`b`'s) qualifier must NOT survive onto the \
             widened attribute, matching Spark's `b.z` rejection; got: {:?}",
            quals[2]
        );
    }

    #[test]
    fn union_by_name_allow_missing_bare_padded_col_resolves_qualified_rejected() {
        let bt = BaseTypes::empty();
        let left = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(values_row(&[
                ("x", DataType::Long, LiteralValue::Long(1)),
                ("y", DataType::Long, LiteralValue::Long(2)),
            ])),
            alias: "a".to_owned(),
        });
        let right = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(values_row(&[
                ("x", DataType::Long, LiteralValue::Long(10)),
                ("y", DataType::Long, LiteralValue::Long(20)),
                ("z", DataType::Long, LiteralValue::Long(30)),
            ])),
            alias: "b".to_owned(),
        });
        let union = union_by_name_allow_missing(vec![left, right]);

        let bare_project = CommonAst::new(CommonOp::Project {
            input: Box::new(union.clone()),
            projections: vec![unresolved_col("z")],
        });
        analyze(bare_project, &bt).expect("bare padded column `z` must resolve");

        let qualified_project = CommonAst::new(CommonOp::Project {
            input: Box::new(union),
            projections: vec![qcol("b", "z")],
        });
        let err = analyze(qualified_project, &bt).unwrap_err();
        assert!(
            matches!(err, AnalyzerError::UnknownColumn { .. }),
            "expected UnknownColumn, got {err:?}"
        );
        assert_eq!(err.spark_class(), Some("UNRESOLVED_COLUMN.WITH_SUGGESTION"));
    }

    #[test]
    fn union_by_name_allow_missing_identical_name_sets_matches_strict() {
        let bt = BaseTypes::empty();
        let left = values_row(&[
            ("a", DataType::Long, LiteralValue::Long(1)),
            ("b", DataType::Long, LiteralValue::Long(2)),
        ]);
        let right = values_row(&[
            ("a", DataType::Long, LiteralValue::Long(3)),
            ("b", DataType::Long, LiteralValue::Long(4)),
        ]);
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
        assert_eq!(widened.fields.len(), 2);
        assert_eq!(widened.fields[0].name, "a");
        assert_eq!(widened.fields[1].name, "b");
        assert!(!widened.fields[0].nullable);
        assert!(!widened.fields[1].nullable);
    }

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

    #[test]
    fn project_star_expands_schema_but_keeps_star_in_tree() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema, emp_schema());
        match &typed.op {
            TypedOp::Project { projections, .. } => {
                assert!(matches!(&projections[0], Expression::Star(_)));
            }
            _ => panic!("expected Project"),
        }
    }

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
        let unresolved = TypedAst::new(
            TypedOp::SingleRow,
            ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
                "x",
                DataType::Unresolved,
            )])),
        );
        assert!(!has_resolved_schema(&unresolved));

        let with_unresolved_expr = TypedAst::new(
            TypedOp::Project {
                input: Box::new(TypedAst::new(TypedOp::SingleRow, ResolvedSchema::empty())),
                projections: vec![unresolved_col("x")],
            },
            ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
                "x",
                DataType::Long,
            )])),
        );
        assert!(!has_resolved_schema(&with_unresolved_expr));
    }

    #[test]
    fn analyze_composes_resolve_assign_types_and_derive_nullability() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Gt,
            left: Box::new(unresolved_col("salary")),
            right: Box::new(lit_double(50000.0)),
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(scan("emp")),
            condition: cond,
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema, emp_schema());
        match &typed.op {
            TypedOp::Filter { condition, .. } => match condition {
                Expression::Binary(b) => match b.left.as_ref() {
                    Expression::ColumnReference(c) => {
                        assert_eq!(c.data_type, DataType::Double);
                        assert!(c.nullable);
                    }
                    _ => panic!("expected ColumnReference"),
                },
                _ => panic!("expected Binary"),
            },
            _ => panic!("expected Filter"),
        }
    }

    #[test]
    fn analyzer_error_bridge_maps_spark_emulated_with_class_to_spark_emulated() {
        let e = AnalyzerError::UnknownColumn {
            name: "c".to_owned(),
            qualifier: None,
        };
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::SparkEmulated { class, message } => {
                assert_eq!(class, Some("UNRESOLVED_COLUMN.WITH_SUGGESTION"));
                assert!(
                    !message.starts_with("[SPARK-EMULATED]"),
                    "message must not double the internal prefix, got: {message}"
                );
            }
            other => panic!("expected EmissionError::SparkEmulated, got: {other:?}"),
        }
    }

    #[test]
    fn analyzer_error_bridge_maps_ambiguous_column_reference_to_spark_emulated() {
        let e = AnalyzerError::AmbiguousColumnReference {
            name: "id".to_owned(),
        };
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::SparkEmulated { class, message } => {
                assert_eq!(class, Some("AMBIGUOUS_COLUMN_REFERENCE"));
                assert!(
                    !message.starts_with("[SPARK-EMULATED]"),
                    "message must not double the internal prefix, got: {message}"
                );
                let display = EmissionError::SparkEmulated {
                    class,
                    message: message.clone(),
                }
                .to_string();
                assert!(
                    display.starts_with("[AMBIGUOUS_COLUMN_REFERENCE]"),
                    "expected leading Spark class token, got: {display}",
                );
            }
            other => panic!("expected EmissionError::SparkEmulated, got: {other:?}"),
        }
    }

    #[test]
    fn analyzer_error_bridge_maps_classless_other_to_spark_emulated() {
        let e = AnalyzerError::Other {
            reason: "catch-all".to_owned(),
        };
        assert_eq!(e.category(), ErrorCategory::SparkEmulated);
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::SparkEmulated { class, message } => {
                assert_eq!(class, None);
                assert_eq!(message, "catch-all");
                assert!(
                    !message.contains("[SPARK-EMULATED]"),
                    "the τ-internal prefix must be stripped, got: {message}"
                );
            }
            other => panic!("expected EmissionError::SparkEmulated, got: {other:?}"),
        }
    }

    #[test]
    fn analyzer_error_bridge_maps_internal_to_emission_internal() {
        let e = AnalyzerError::Internal {
            reason: "union-of-names produced orphan name \"x\"".to_owned(),
        };
        assert_eq!(e.category(), ErrorCategory::Internal);
        assert_eq!(e.spark_class(), None);
        match analyzer_error_to_emission_error(e) {
            EmissionError::Internal { message } => {
                assert_eq!(message, "union-of-names produced orphan name \"x\"");
            }
            other => panic!("expected EmissionError::Internal, got: {other:?}"),
        }
    }

    #[test]
    fn analyzer_error_bridge_keeps_boundary_variants_unsupported() {
        let e = AnalyzerError::PuntedOperator {
            op: "FileScan".to_owned(),
            reason: "no arm".to_owned(),
        };
        assert_eq!(e.category(), ErrorCategory::ThunderduckBoundary);
        match analyzer_error_to_emission_error(e) {
            EmissionError::Unsupported {
                kind: UnsupportedKind::Op,
                name,
                ..
            } => assert_eq!(name, "FileScan"),
            other => panic!("expected EmissionError::Unsupported, got: {other:?}"),
        }
    }

    #[test]
    fn spark_class_mapping_matches_adr023_chunk_3b_table() {
        assert_eq!(
            AnalyzerError::UnknownTable {
                name: "t".to_owned()
            }
            .spark_class(),
            Some("TABLE_OR_VIEW_NOT_FOUND")
        );
        assert_eq!(
            AnalyzerError::UnknownColumn {
                name: "c".to_owned(),
                qualifier: None
            }
            .spark_class(),
            Some("UNRESOLVED_COLUMN.WITH_SUGGESTION")
        );
        assert_eq!(
            AnalyzerError::AmbiguousColumn {
                name: "c".to_owned(),
                candidates: vec!["l.c".to_owned(), "r.c".to_owned()],
            }
            .spark_class(),
            Some("AMBIGUOUS_REFERENCE")
        );
        assert_eq!(
            AnalyzerError::AmbiguousColumnReference {
                name: "c".to_owned(),
            }
            .spark_class(),
            Some("AMBIGUOUS_COLUMN_REFERENCE")
        );
        assert_eq!(
            AnalyzerError::AmbiguousLateralColumnAlias {
                name: "a".to_owned(),
                count: 2,
            }
            .spark_class(),
            Some("AMBIGUOUS_LATERAL_COLUMN_ALIAS")
        );
        assert_eq!(
            AnalyzerError::TypeMismatch {
                expected: DataType::Boolean,
                actual: DataType::Integer,
                context: "filter-condition".to_owned(),
            }
            .spark_class(),
            Some("DATATYPE_MISMATCH")
        );
        assert_eq!(
            AnalyzerError::Other {
                reason: "catch-all".to_owned()
            }
            .spark_class(),
            None
        );
        assert_eq!(
            AnalyzerError::PuntedOperator {
                op: "FileScan".to_owned(),
                reason: "wip".to_owned(),
            }
            .spark_class(),
            None
        );
        assert_eq!(
            AnalyzerError::UnsupportedRule {
                rule: "r".to_owned(),
                reason: "wip".to_owned(),
            }
            .spark_class(),
            None
        );
    }

    #[test]
    fn analyzer_error_bridge_maps_punted_operator_to_unsupported_op() {
        let e = AnalyzerError::PuntedOperator {
            op: "FileScan".to_owned(),
            reason: "wip".to_owned(),
        };
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::Unsupported {
                kind: UnsupportedKind::Op,
                name,
                ..
            } => assert_eq!(name, "FileScan"),
            _ => panic!("expected UnsupportedOp"),
        }
    }

    #[test]
    fn unpivot_stamps_schema_with_widened_value_column() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
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
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec![],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        let typed = analyze(ast, &bt).unwrap();
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
            input: Box::new(scan("emp")),
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
    fn unpivot_duplicate_across_ids_and_values_is_accepted_like_spark() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
            ids: UnpivotIds::Explicit(vec!["id".to_owned(), "salary".to_owned()]),
            values: vec!["SALARY".to_owned(), "dept_id".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        let typed = analyze(ast, &bt).expect("Spark accepts id/value overlap, so τ must too");
        let names: Vec<&str> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(
            names.contains(&"metric") && names.contains(&"value"),
            "unpivot must still stamp its variable/value columns, got {names:?}"
        );
    }

    #[test]
    fn unpivot_variable_column_colliding_with_id_is_accepted_like_spark() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "ID".to_owned(),
            value_column_name: "value".to_owned(),
        });
        let typed = analyze(ast, &bt).expect("Spark accepts the collision, so τ must too");
        let dup = typed
            .resolved_schema
            .fields
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case("id"))
            .count();
        assert_eq!(
            dup,
            2,
            "expected the id column AND the variable column both named `id` \
             (Spark permits duplicate output names), got {:?}",
            typed
                .resolved_schema
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn unpivot_value_column_colliding_with_id_is_accepted_like_spark() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "Id".to_owned(),
        });
        let typed = analyze(ast, &bt).expect("Spark accepts the collision, so τ must too");
        let dup = typed
            .resolved_schema
            .fields
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case("id"))
            .count();
        assert_eq!(dup, 2, "expected two `id`-named output fields");
    }

    #[test]
    fn aggregate_output_schema_stamps_count_result_as_long() {
        let bt = base_types_with_emp_dept();
        let ast = aggregate(
            scan("emp"),
            vec![],
            vec![func("count", vec![unresolved_col("id")])],
        );
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
        assert!(!typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn aggregate_grouping_expr_also_projected_is_not_prepended() {
        let bt = base_types_with_emp_dept();
        let senior = || {
            Expression::Binary(BinaryExpression {
                op: BinaryOp::GtEq,
                left: Box::new(unresolved_col("dept_id")),
                right: Box::new(int_lit(40)),
            })
        };
        let avg_salary = func("avg", vec![unresolved_col("salary")]);
        let ast = aggregate(
            scan("emp"),
            vec![senior()],
            vec![alias_expr(senior(), "senior"), alias_expr(avg_salary, "s")],
        );
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(
            typed.resolved_schema.fields.len(),
            2,
            "grouping expr already projected must not be prepended"
        );
        assert_eq!(typed.resolved_schema.fields[0].name, "senior");
        assert_eq!(typed.resolved_schema.fields[1].name, "s");
    }

    #[test]
    fn partial_grouping_fold_does_not_prepend_or_duplicate() {
        let bt = base_types_with_emp_dept();
        let avg_salary = func("avg", vec![unresolved_col("salary")]);
        let ast = aggregate(
            scan("emp"),
            vec![unresolved_col("dept_id"), unresolved_col("salary")],
            vec![unresolved_col("dept_id"), alias_expr(avg_salary, "a")],
        );
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(
            typed.resolved_schema.fields.len(),
            2,
            "partially-selected grouping keys must not be prepended or duplicated"
        );
        assert_eq!(typed.resolved_schema.fields[0].name, "dept_id");
        assert_eq!(typed.resolved_schema.fields[1].name, "a");
    }

    #[test]
    fn dataframe_path_grouping_absent_from_aggregates_still_prepends() {
        let bt = base_types_with_emp_dept();
        let avg_salary = func("avg", vec![unresolved_col("salary")]);
        let ast = CommonAst::new(grouped_aggregate(
            scan("emp"),
            vec![unresolved_col("dept_id"), unresolved_col("salary")],
            vec![alias_expr(avg_salary, "a")],
            crate::transpiler_v2::ast::GroupingKind::GroupBy,
        ));
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(
            typed.resolved_schema.fields.len(),
            3,
            "grouping keys absent from aggregates must still be prepended"
        );
        assert_eq!(typed.resolved_schema.fields[0].name, "dept_id");
        assert_eq!(typed.resolved_schema.fields[1].name, "salary");
        assert_eq!(typed.resolved_schema.fields[2].name, "a");
    }

    #[test]
    fn aggregate_having_resolves_input_column_inside_aggregate_call() {
        let bt = base_types_with_emp_dept();
        let avg_salary = || func("avg", vec![unresolved_col("salary")]);
        let having = Expression::Binary(BinaryExpression {
            op: BinaryOp::Gt,
            left: Box::new(avg_salary()),
            right: Box::new(lit_double(80000.0)),
        });
        let ast = aggregate_having(
            scan("emp"),
            vec![unresolved_col("dept_id")],
            vec![unresolved_col("dept_id"), avg_salary()],
            Some(having),
        );
        let typed = analyze(ast, &bt).expect("HAVING over input column should resolve");
        match typed.op {
            TypedOp::Aggregate { having, .. } => {
                assert!(
                    having.is_some(),
                    "resolved having should be threaded through"
                );
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_non_boolean_having_rejected() {
        let bt = base_types_with_emp_dept();
        let ast = aggregate_having(
            scan("emp"),
            vec![],
            vec![func(
                "count",
                vec![Expression::Star(StarExpression { qualifier: None })],
            )],
            Some(int_lit(5)),
        );
        match analyze(ast, &bt) {
            Err(AnalyzerError::TypeMismatch { context, .. }) => {
                assert_eq!(context, "having-condition");
            }
            other => panic!("expected TypeMismatch(having-condition), got {other:?}"),
        }
    }

    #[test]
    fn analyze_pivot_explicit_bool_values_stamps_single_agg_output_schema() {
        let bt = base_types_with_emp_dept();
        let emp_scan = scan("emp");
        let with_active = CommonAst::new(CommonOp::WithColumns {
            input: Box::new(emp_scan),
            assignments: vec![("active".to_owned(), lit_bool(true))],
        });
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(with_active),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("active"),
            pivot_values: vec![lit_bool(true), lit_bool(false)],
            aggregates: vec![alias_expr(func("count", vec![int_lit(1)]), "n")],
        });
        let typed = analyze(ast, &bt).unwrap();
        let fields = &typed.resolved_schema.fields;
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "dept_id");
        assert_eq!(fields[1].name, "true");
        assert_eq!(fields[2].name, "false");
        assert!(fields[1].nullable);
        assert!(fields[2].nullable);
    }

    #[test]
    fn analyze_pivot_implicit_values_returns_boundary_punted_operator() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("salary"),
            pivot_values: vec![],
            aggregates: vec![func("avg", vec![unresolved_col("salary")])],
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::PuntedOperator { op, .. }) => {
                assert_eq!(op, "Pivot[implicit-values]");
            }
            other => panic!("expected PuntedOperator, got {other:?}"),
        }
    }

    #[test]
    fn analyze_pivot_multi_agg_names_outputs_value_underscore_alias() {
        let bt = base_types_with_emp_dept();
        let emp_scan = scan("emp");
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10), int_lit(20)],
            aggregates: vec![
                alias_expr(func("sum", vec![unresolved_col("salary")]), "sum_sal"),
                alias_expr(func("count", vec![int_lit(1)]), "cnt"),
            ],
        });
        let typed = analyze(ast, &bt).unwrap();
        let names = field_names(&typed);
        assert_eq!(
            names,
            vec!["dept_id", "10_sum_sal", "10_cnt", "20_sum_sal", "20_cnt"]
        );
    }

    #[test]
    fn analyze_pivot_multi_agg_unaliased_names_outputs_value_underscore_pretty_name() {
        let bt = base_types_with_emp_dept();
        let emp_scan = scan("emp");
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10)],
            aggregates: vec![
                func("sum", vec![unresolved_col("salary")]),
                func("avg", vec![unresolved_col("salary")]),
            ],
        });
        let typed = analyze(ast, &bt).unwrap();
        let names = field_names(&typed);
        assert_eq!(names, vec!["dept_id", "10_sum(salary)", "10_avg(salary)"]);
    }

    #[test]
    fn analyze_pivot_double_values_render_dot_zero_for_integral_spark_parity() {
        let bt = base_types_with_emp_dept();
        let emp_scan = scan("emp");
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("salary"),
            pivot_values: vec![
                lit_double(1.0),
                lit_double(-2.0),
                Expression::Literal(Literal {
                    value: LiteralValue::Float(1.5),
                    data_type: DataType::Float,
                }),
            ],
            aggregates: vec![func("count", vec![int_lit(1)])],
        });
        let typed = analyze(ast, &bt).unwrap();
        let names = field_names(&typed);
        assert_eq!(names, vec!["dept_id", "1.0", "-2.0", "1.5"]);
    }

    #[test]
    fn analyze_pivot_accepts_null_literal_as_null_bucket() {
        let bt = base_types_with_emp_dept();
        let emp_scan = scan("emp");
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("salary"),
            pivot_values: vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Null,
                    data_type: DataType::Null,
                }),
                int_lit(10),
            ],
            aggregates: vec![func("count", vec![int_lit(1)])],
        });
        let typed = analyze(ast, &bt).expect("NULL pivot value must be accepted");
        let names = field_names(&typed);
        assert_eq!(names, vec!["dept_id", "null", "10"]);
    }

    #[test]
    fn analyze_pivot_implicit_grouping_excludes_pivot_and_agg_refs() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Implicit,
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10), int_lit(20)],
            aggregates: vec![func("avg", vec![unresolved_col("salary")])],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(pivot_grouping_names(&typed), vec!["id", "name"]);
    }

    #[test]
    fn analyze_pivot_implicit_grouping_count_star_keeps_all_non_pivot_cols() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Implicit,
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10), int_lit(20)],
            aggregates: vec![func(
                "count",
                vec![Expression::Star(StarExpression { qualifier: None })],
            )],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(pivot_grouping_names(&typed), vec!["id", "name", "salary"]);
    }

    #[test]
    fn analyze_pivot_implicit_grouping_excludes_column_referenced_through_case_between() {
        let bt = base_types_with_emp_dept();
        let case_between = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(
                Expression::Between(BetweenExpression {
                    expr: Box::new(unresolved_col("id")),
                    low: Box::new(int_lit(1)),
                    high: Box::new(int_lit(2)),
                    negated: false,
                }),
                unresolved_col("salary"),
            )],
            else_expr: None,
        });
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Implicit,
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10)],
            aggregates: vec![func("sum", vec![case_between])],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(pivot_grouping_names(&typed), vec!["name"]);
    }

    #[test]
    fn analyze_pivot_implicit_grouping_expression_pivot_excludes_referenced_column() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Implicit,
            pivot_column: func("abs", vec![unresolved_col("dept_id")]),
            pivot_values: vec![int_lit(10)],
            aggregates: vec![func("avg", vec![unresolved_col("salary")])],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(pivot_grouping_names(&typed), vec!["id", "name"]);
    }

    #[test]
    fn analyze_pivot_implicit_grouping_output_attribute_carries_input_attribute_id() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Implicit,
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10), int_lit(20)],
            aggregates: vec![func("avg", vec![unresolved_col("salary")])],
        });
        let typed = analyze(ast, &bt).unwrap();
        let TypedOp::Pivot { input, .. } = &typed.op else {
            panic!("expected TypedOp::Pivot");
        };
        let input_id_field = input
            .resolved_schema
            .field_by_name("id")
            .expect("emp has id");
        let input_name_field = input
            .resolved_schema
            .field_by_name("name")
            .expect("emp has name");
        let output_id_field = typed
            .resolved_schema
            .field_by_name("id")
            .expect("id in pivot output");
        let output_name_field = typed
            .resolved_schema
            .field_by_name("name")
            .expect("name in pivot output");
        assert_eq!(
            output_id_field.expr_id, input_id_field.expr_id,
            "PIVOT implicit-grouping output must COPY the input attribute's id, not mint a fresh one"
        );
        assert_eq!(output_name_field.expr_id, input_name_field.expr_id);
        assert!(
            !output_id_field.source_quals.is_empty(),
            "COPY branch must carry lineage forward too"
        );
    }

    #[test]
    fn analyze_unpivot_implicit_ids_are_input_minus_values() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
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
        let names = field_names(&typed);
        assert_eq!(names, vec!["id", "name", "metric", "val"]);
    }

    #[test]
    fn analyze_unpivot_implicit_ids_empty_values_is_internal_error() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
            ids: UnpivotIds::Implicit,
            values: vec![],
            variable_column_name: "metric".to_owned(),
            value_column_name: "val".to_owned(),
        });
        match analyze(ast, &bt) {
            Err(e @ AnalyzerError::Internal { .. }) => {
                assert_eq!(e.category(), ErrorCategory::Internal);
                assert_eq!(
                    e.spark_class(),
                    None,
                    "a τ-internal break must not claim a Spark class"
                );
            }
            other => panic!("expected AnalyzerError::Internal, got {other:?}"),
        }
    }

    fn base_types_with_addr_table() -> BaseTypes {
        let addr_ty = DataType::Struct(StructType::new(vec![
            StructField::nullable("street", DataType::String),
            StructField::nullable("city", DataType::String),
            StructField::nullable("geo", DataType::String),
        ]));
        base_types_for(&[(
            "addrs",
            StructType::new(vec![StructField::nullable("addr", addr_ty)]),
        )])
    }

    #[test]
    fn analyze_update_fields_missing_drop_target_is_accepted_like_spark() {
        let bt = base_types_with_addr_table();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("addrs")),
            projections: vec![Expression::UpdateFields(
                super::super::expression::UpdateFieldsExpression {
                    struct_expr: Box::new(unresolved_col("addr")),
                    updates: vec![("nope".to_owned(), None)],
                },
            )],
        });
        let typed = analyze(ast, &bt).expect("Spark accepts a no-op dropFields, so τ must too");
        let DataType::Struct(st) = &typed.resolved_schema.fields[0].data_type else {
            panic!(
                "expected a struct output type, got {:?}",
                typed.resolved_schema.fields[0].data_type
            );
        };
        assert!(
            !st.fields.is_empty(),
            "the base struct's fields must survive a no-op drop, got {st:?}"
        );
    }

    fn base_types_with_nested_struct() -> BaseTypes {
        let geo_ty = DataType::Struct(StructType::new(vec![
            StructField::nullable("lat", DataType::Double),
            StructField::nullable("lng", DataType::Double),
        ]));
        let addr_ty = DataType::Struct(StructType::new(vec![
            StructField::nullable("city", DataType::String),
            StructField::nullable("zip", DataType::String),
            StructField::nullable("geo", geo_ty),
        ]));
        base_types_for(&[(
            "emp",
            StructType::new(vec![
                StructField::not_null("id", DataType::Long),
                StructField::nullable("address", addr_ty),
            ]),
        )])
    }

    #[test]
    fn resolve_multi_level_nested_struct_path_becomes_extract_value_chain() {
        let bt = base_types_with_nested_struct();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![qcol("address", "geo.lat")],
        });
        let typed = analyze(ast, &bt).expect("multi-level dot path must resolve");
        let proj = match &typed.op {
            TypedOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        assert_eq!(proj.len(), 1, "single projection");
        let wrapped = match &proj[0] {
            Expression::Alias(a) => a,
            other => panic!("expected Alias (N8), got {other:?}"),
        };
        assert_eq!(wrapped.alias, "lat");
        let outer = match wrapped.expr.as_ref() {
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
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Double);
        assert!(typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn resolve_single_level_nested_struct_path_becomes_extract_value_chain() {
        let bt = base_types_with_nested_struct();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![qcol("address", "city")],
        });
        let typed = analyze(ast, &bt).expect("single-level dot path must resolve");
        let proj = match &typed.op {
            TypedOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        assert_eq!(proj.len(), 1, "single projection");
        let wrapped = match &proj[0] {
            Expression::Alias(a) => a,
            other => panic!("expected Alias (N8), got {other:?}"),
        };
        assert_eq!(wrapped.alias, "city");
        let ev = match wrapped.expr.as_ref() {
            Expression::ExtractValue(ev) => ev,
            other => panic!("expected ExtractValue, got {other:?}"),
        };
        match ev.extraction.as_ref() {
            Expression::Literal(Literal {
                value: LiteralValue::String(s),
                ..
            }) => assert_eq!(s, "city"),
            other => panic!("expected String literal 'city', got {other:?}"),
        }
        match ev.child.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "address");
                assert!(c.qualifier.is_none(), "root ColumnReference is unqualified");
            }
            other => panic!("expected root ColumnReference('address'), got {other:?}"),
        }
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::String);
        assert!(typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn resolve_single_level_nested_struct_path_root_carries_column_expr_id() {
        let bt = base_types_with_nested_struct();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("address"), qcol("address", "city")],
        });
        let typed = analyze(ast, &bt).expect("projections must resolve");
        let proj = match &typed.op {
            TypedOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        assert_eq!(proj.len(), 2);
        let address_id = match &proj[0] {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "address");
                c.expr_id
                    .expect("raw address column reference is id-carrying")
            }
            other => panic!("expected ColumnReference('address'), got {other:?}"),
        };
        let wrapped = match &proj[1] {
            Expression::Alias(a) => a,
            other => panic!("expected Alias (N8), got {other:?}"),
        };
        let ev = match wrapped.expr.as_ref() {
            Expression::ExtractValue(ev) => ev,
            other => panic!("expected ExtractValue, got {other:?}"),
        };
        match ev.child.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(
                    c.expr_id,
                    Some(address_id),
                    "chain root must carry the SAME id as the raw address column reference"
                );
            }
            other => panic!("expected root ColumnReference('address'), got {other:?}"),
        }
    }

    #[test]
    fn semantic_eq_rejects_same_named_struct_field_different_root_ids() {
        let id_a = ExprId::fresh();
        let id_b = ExprId::fresh();
        assert_ne!(id_a, id_b);
        let extract = |root_name: &str, root_id: ExprId, field_ty: DataType| {
            Expression::ExtractValue(ExtractValueExpression {
                child: Box::new(Expression::ColumnReference(ColumnReference {
                    name: root_name.to_owned(),
                    qualifier: None,
                    data_type: DataType::Struct(StructType::new(vec![StructField::nullable(
                        "x",
                        field_ty.clone(),
                    )])),
                    nullable: true,
                    expr_id: Some(root_id),
                })),
                extraction: Box::new(Expression::Literal(Literal {
                    value: LiteralValue::String("x".to_owned()),
                    data_type: DataType::String,
                })),
            })
        };
        let a = extract("s", id_a, DataType::Integer);
        let b = extract("s", id_b, DataType::String);
        assert!(
            !semantic_eq(&a, &b),
            "same-named struct field through different root columns must not \
             semantic_eq-collide just because the canonicalized shapes agree"
        );
    }

    #[test]
    fn resolve_unknown_nested_field_falls_through_to_unknown_column() {
        let bt = base_types_with_nested_struct();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![qcol("address", "geo.nope")],
        });
        match analyze(ast, &bt) {
            Err(AnalyzerError::UnknownColumn { .. }) => {}
            other => panic!("expected UnknownColumn error, got {other:?}"),
        }
    }

    #[test]
    fn analyze_update_fields_drop_field_case_insensitive_ok() {
        let bt = base_types_with_addr_table();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("addrs")),
            projections: vec![Expression::UpdateFields(
                super::super::expression::UpdateFieldsExpression {
                    struct_expr: Box::new(unresolved_col("addr")),
                    updates: vec![("GEO".to_owned(), None)],
                },
            )],
        });
        analyze(ast, &bt).expect("case-insensitive drop must analyze cleanly");
    }

    fn base_types_with_emp() -> BaseTypes {
        base_types_for(&[("emp", emp_schema())])
    }

    fn assert_stats_output_schema(schema: &ResolvedSchema, expected_col_names: &[&str]) {
        assert_eq!(
            schema.fields.len(),
            expected_col_names.len() + 1,
            "stats output schema has 1 (summary) + N stat cols",
        );
        assert_eq!(schema.fields[0].name, "summary");
        assert_eq!(schema.fields[0].data_type, DataType::String);
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
            cols: vec![],
        });
        let typed = analyze(ast, &bt).expect("analyze describe");
        assert_stats_output_schema(&typed.resolved_schema, &["id", "name", "dept_id", "salary"]);
    }

    #[test]
    fn analyze_describe_unknown_column_surfaces_spark_emulated_error() {
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Describe {
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
            statistics: vec![],
        });
        let typed = analyze(ast, &bt).expect("analyze summary");
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
        base_types_for(&[("stats", stats_schema)])
    }

    #[test]
    fn analyze_freq_items_stamps_array_of_source_type_per_col() {
        let bt = base_types_with_stats();
        let ast = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(scan("stats")),
            cols: vec![
                "dept_id".to_owned(),
                "name".to_owned(),
                "salary".to_owned(),
                "bonus".to_owned(),
            ],
            support: 0.3,
        });
        let typed = analyze(ast, &bt).expect("analyze freqItems");
        assert_eq!(typed.resolved_schema.fields.len(), 4);
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
            input: Box::new(scan("stats")),
            cols: vec!["Dept_ID".to_owned()],
            support: 0.01,
        });
        let typed = analyze(ast, &bt).expect("case-insensitive freqItems must analyze");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].name, "Dept_ID_freqItems");
        match &typed.resolved_schema.fields[0].data_type {
            DataType::Array(elem, _) => assert_eq!(elem.as_ref(), &DataType::Integer),
            other => panic!("expected Array<Integer>, got {other:?}"),
        }
    }

    #[test]
    fn analyze_freq_items_unknown_column_surfaces_spark_emulated_error() {
        let bt = base_types_with_stats();
        let ast = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(scan("stats")),
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
            input: Box::new(scan("stats")),
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

    #[test]
    fn crosstab_desugar_produces_spark_parity_contingency_schema() {
        let ct_schema = StructType::new(vec![
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("active", DataType::Boolean),
        ]);
        let bt = base_types_for(&[("ct", ct_schema)]);

        let op = crosstab_to_aggregate(
            scan("ct"),
            "dept_id",
            "active",
            vec![lit_bool(true), lit_bool(false)],
        );
        let typed = analyze(CommonAst::new(op), &bt).unwrap();

        let fields = &typed.resolved_schema.fields;
        assert_eq!(fields.len(), 3, "col0 + one count col per distinct value");

        assert_eq!(fields[0].name, "dept_id_active");
        assert_eq!(fields[0].data_type, DataType::String);
        assert!(
            fields[0].nullable,
            "col0 nullability follows col1 (dept_id)"
        );

        assert_eq!(fields[1].name, "false");
        assert_eq!(fields[1].data_type, DataType::Long);
        assert!(!fields[1].nullable, "count columns are bigint non-null");

        assert_eq!(fields[2].name, "true");
        assert_eq!(fields[2].data_type, DataType::Long);
        assert!(!fields[2].nullable, "count columns are bigint non-null");
    }

    #[test]
    fn analyze_sample_schema_passthrough() {
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sample {
            input: Box::new(scan("emp")),
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
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sample {
            input: Box::new(scan("emp")),
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
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::SampleBy {
            input: Box::new(scan("emp")),
            col: unresolved_col("dept_id"),
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
                        assert_eq!(c.data_type, DataType::Integer);
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
            input: Box::new(scan("emp")),
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

    fn regex_test_schema() -> ResolvedSchema {
        ResolvedSchema::minted(StructType::new(vec![
            StructField::not_null("customer_id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("order_id", DataType::Long),
        ]))
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
        let non_regex_before = unresolved_col("name");
        let non_regex_after = int_lit(1);
        let projections = vec![
            non_regex_before.clone(),
            Expression::UnresolvedRegex(UnresolvedRegexExpression {
                pattern: ".*_id".to_owned(),
                plan_id: None,
            }),
            non_regex_after.clone(),
        ];
        let expanded = expand_regex_projections(projections, &schema).expect("expand ok");
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
    fn generator_expr(name: &str, args: Vec<Expression>, aliases: &[&str]) -> Expression {
        let mut generator = Generator::from_function(name, args).expect("generator name");
        generator.aliases = aliases.iter().map(|alias| (*alias).to_owned()).collect();
        Expression::Generator(generator)
    }

    #[test]
    fn project_generator_normalizes_to_generate() {
        let structs = func(
            "array",
            vec![func(
                "struct",
                vec![unresolved_col("name"), unresolved_col("salary")],
            )],
        );
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![
                unresolved_col("id"),
                generator_expr("inline", vec![structs], &[]),
            ],
        });
        let typed = analyze(ast, &base_types_with_emp_dept()).expect("analyze");
        assert_eq!(field_names(&typed), ["id", "name", "salary"]);
        let TypedOp::Project { input, .. } = typed.op else {
            panic!("expected Project");
        };
        assert!(matches!(
            input.op,
            TypedOp::Generate {
                generator: Generator {
                    kind: GeneratorKind::Inline,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn json_tuple_and_stack_derive_output_once() {
        let raw = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("json", DataType::String),
        ]);
        let json = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("raw")),
            projections: vec![generator_expr(
                "json_tuple",
                vec![unresolved_col("json"), lit_str("a"), lit_str("b")],
                &[],
            )],
        });
        let typed = analyze(json, &base_types_for(&[("raw", raw)])).expect("json_tuple");
        assert_eq!(field_names(&typed), ["c0", "c1"]);
        assert!(typed
            .resolved_schema
            .fields
            .iter()
            .all(|field| field.data_type == DataType::String && field.nullable));

        let stack = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![generator_expr(
                "stack",
                vec![
                    int_lit(2),
                    lit_str("a"),
                    int_lit(1),
                    lit_str("b"),
                    int_lit(2),
                ],
                &["key", "value"],
            )],
        });
        let typed = analyze(stack, &base_types_for(&[])).expect("stack");
        assert_eq!(field_names(&typed), ["key", "value"]);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::String);
        assert_eq!(typed.resolved_schema.fields[1].data_type, DataType::Integer);
    }

    #[test]
    fn generator_alias_arity_is_checked_after_input_resolution() {
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![generator_expr(
                "inline",
                vec![func(
                    "array",
                    vec![func(
                        "struct",
                        vec![unresolved_col("name"), unresolved_col("salary")],
                    )],
                )],
                &["only_one"],
            )],
        });
        let err = analyze(ast, &base_types_with_emp_dept()).expect_err("alias mismatch");
        assert!(matches!(
            err,
            AnalyzerError::SparkEmulated {
                class: "UDTF_ALIAS_NUMBER_MISMATCH",
                ..
            }
        ));
    }

    #[test]
    fn nested_and_multiple_generators_are_rejected() {
        let array = func("array", vec![int_lit(1)]);
        let nested = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![func(
                "coalesce",
                vec![
                    generator_expr("explode", vec![array.clone()], &[]),
                    int_lit(0),
                ],
            )],
        });
        assert!(matches!(
            analyze(nested, &base_types_for(&[])),
            Err(AnalyzerError::SparkEmulated {
                class: "UNSUPPORTED_GENERATOR.NESTED_IN_EXPRESSIONS",
                ..
            })
        ));

        let multiple = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![
                generator_expr("explode", vec![array.clone()], &[]),
                generator_expr("explode", vec![array], &[]),
            ],
        });
        assert!(matches!(
            analyze(multiple, &base_types_for(&[])),
            Err(AnalyzerError::SparkEmulated {
                class: "UNSUPPORTED_GENERATOR.MULTI_GENERATOR",
                ..
            })
        ));
    }
    #[test]
    fn na_fill_compatible_matches_spark_fill_value_rules() {
        for c in [
            DataType::Byte,
            DataType::Short,
            DataType::Integer,
            DataType::Long,
            DataType::Float,
            DataType::Double,
            DataType::Decimal {
                precision: 10,
                scale: 2,
            },
        ] {
            assert!(
                na_fill_compatible(&c, &DataType::Long),
                "{c:?} × Long should be compatible"
            );
        }
        assert!(na_fill_compatible(&DataType::String, &DataType::String));
        assert!(na_fill_compatible(&DataType::Boolean, &DataType::Boolean));
        assert!(!na_fill_compatible(&DataType::String, &DataType::Long));
        assert!(!na_fill_compatible(&DataType::Long, &DataType::String));
        assert!(!na_fill_compatible(&DataType::Boolean, &DataType::Long));
        assert!(!na_fill_compatible(&DataType::String, &DataType::Boolean));
        assert!(!na_fill_compatible(&DataType::Date, &DataType::Long));
        assert!(!na_fill_compatible(&DataType::Timestamp, &DataType::Long));
    }

    #[test]
    fn analyze_na_fill_empty_cols_int_value_skips_non_numeric_columns() {
        let mixed_schema = StructType::new(vec![
            StructField::nullable("s", DataType::String),
            StructField::nullable("l", DataType::Long),
            StructField::nullable("d", DataType::Double),
            StructField::nullable("b", DataType::Boolean),
        ]);
        let bt = base_types_for(&[("t", mixed_schema)]);
        let ast = CommonAst::new(CommonOp::NaFill {
            input: Box::new(scan("t")),
            cols: vec![],
            values: vec![int_lit(0)],
        });
        let typed = analyze(ast, &bt).expect("analyze NaFill must succeed");
        let fields = &typed.resolved_schema.fields;
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].name, "s");
        assert_eq!(fields[0].data_type, DataType::String);
        assert!(
            fields[0].nullable,
            "String column must stay nullable (type-incompatible with Int fill)"
        );
        assert_eq!(fields[3].name, "b");
        assert_eq!(fields[3].data_type, DataType::Boolean);
        assert!(
            fields[3].nullable,
            "Boolean column must stay nullable (type-incompatible with Int fill)"
        );
        assert_eq!(fields[1].name, "l");
        assert_eq!(fields[1].data_type, DataType::Long);
        assert!(!fields[1].nullable, "Long column must flip to non-null");
        assert_eq!(fields[2].name, "d");
        assert_eq!(fields[2].data_type, DataType::Double);
        assert!(!fields[2].nullable, "Double column must flip to non-null");
    }

    #[test]
    fn pretty_name_binary_arithmetic() {
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(unresolved_col("age")),
            right: Box::new(int_lit(1)),
        });
        assert_eq!(pretty_name(&expr), "(age + 1)");
    }

    #[test]
    fn pretty_name_binary_division() {
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(unresolved_col("salary")),
            right: Box::new(int_lit(1000)),
        });
        assert_eq!(pretty_name(&expr), "(salary / 1000)");
    }

    #[test]
    fn pretty_name_nested_binary() {
        let inner = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(unresolved_col("age")),
            right: Box::new(int_lit(1)),
        });
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Mul,
            left: Box::new(inner),
            right: Box::new(int_lit(2)),
        });
        assert_eq!(pretty_name(&expr), "((age + 1) * 2)");
    }

    #[test]
    fn pretty_name_function_call() {
        let expr = func("upper", vec![unresolved_col("name")]);
        assert_eq!(pretty_name(&expr), "upper(name)");
    }

    #[test]
    fn pretty_name_unary_is_null() {
        let expr = Expression::Unary(UnaryExpression {
            op: UnaryOp::IsNull,
            operand: Box::new(unresolved_col("x")),
        });
        assert_eq!(pretty_name(&expr), "(x IS NULL)");
    }

    #[test]
    fn pretty_name_star_unqualified() {
        let expr = Expression::Star(StarExpression { qualifier: None });
        assert_eq!(pretty_name(&expr), "*");
    }

    #[test]
    fn pretty_name_extract_value_leaf_field() {
        let inner = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(unresolved_col("address")),
            extraction: Box::new(lit_str("geo")),
        });
        let expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(inner),
            extraction: Box::new(lit_str("lat")),
        });
        assert_eq!(pretty_name(&expr), "lat");
    }

    #[test]
    fn pretty_name_literals() {
        assert_eq!(pretty_name(&int_lit(1)), "1");
        assert_eq!(pretty_name(&lit_str("hello")), "hello");
        assert_eq!(pretty_name(&lit_bool(true)), "true");
        assert_eq!(
            pretty_name(&Expression::Literal(Literal {
                value: LiteralValue::Null,
                data_type: DataType::Null,
            })),
            "NULL"
        );
    }

    #[test]
    fn pretty_name_cast_decimal_uppercase_spelling() {
        let expr = Expression::Cast(CastExpression {
            expr: Box::new(unresolved_col("x")),
            to_type: DataType::Decimal {
                precision: 15,
                scale: 4,
            },
            try_cast: false,
            implicit: false,
        });
        assert_eq!(pretty_name(&expr), "CAST(x AS DECIMAL(15,4))");
    }

    #[test]
    fn pretty_name_try_cast_uses_try_cast_keyword() {
        let expr = Expression::Cast(CastExpression {
            expr: Box::new(unresolved_col("x")),
            to_type: DataType::Long,
            try_cast: true,
            implicit: false,
        });
        assert_eq!(pretty_name(&expr), "TRY_CAST(x AS BIGINT)");
    }

    #[test]
    fn pretty_name_q061_nested_cast_division_times_literal() {
        let dec = DataType::Decimal {
            precision: 15,
            scale: 4,
        };
        let cast = |col: &str| {
            Expression::Cast(CastExpression {
                expr: Box::new(unresolved_col(col)),
                to_type: dec.clone(),
                try_cast: false,
                implicit: false,
            })
        };
        let div = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(cast("promotions")),
            right: Box::new(cast("total")),
        });
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Mul,
            left: Box::new(div),
            right: Box::new(int_lit(100)),
        });
        assert_eq!(
            pretty_name(&expr),
            "((CAST(promotions AS DECIMAL(15,4)) / CAST(total AS DECIMAL(15,4))) * 100)"
        );
    }

    #[test]
    fn pretty_name_case_when_matches_spark_pretty_sql() {
        let single = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::GtEq,
                    left: Box::new(unresolved_col("age")),
                    right: Box::new(int_lit(40)),
                }),
                int_lit(1),
            )],
            else_expr: Some(Box::new(int_lit(0))),
        });
        assert_eq!(
            pretty_name(&single),
            "CASE WHEN (age >= 40) THEN 1 ELSE 0 END"
        );

        let multi = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![
                (
                    Expression::Binary(BinaryExpression {
                        op: BinaryOp::Lt,
                        left: Box::new(unresolved_col("age")),
                        right: Box::new(int_lit(30)),
                    }),
                    int_lit(0),
                ),
                (
                    Expression::Binary(BinaryExpression {
                        op: BinaryOp::Lt,
                        left: Box::new(unresolved_col("age")),
                        right: Box::new(int_lit(45)),
                    }),
                    int_lit(1),
                ),
            ],
            else_expr: Some(Box::new(int_lit(2))),
        });
        assert_eq!(
            pretty_name(&multi),
            "CASE WHEN (age < 30) THEN 0 WHEN (age < 45) THEN 1 ELSE 2 END"
        );

        let no_else = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(unresolved_col("active"), unresolved_col("salary"))],
            else_expr: None,
        });
        assert_eq!(pretty_name(&no_else), "CASE WHEN active THEN salary END");
    }

    #[test]
    fn expression_output_name_unaliased_sum_uses_pretty_name() {
        let expr = func("sum", vec![unresolved_col("ss_net_profit")]);
        assert_eq!(expression_output_name(&expr), "sum(ss_net_profit)");
    }

    #[test]
    fn expression_output_name_unaliased_avg_uses_pretty_name() {
        let expr = func("avg", vec![unresolved_col("x")]);
        assert_eq!(expression_output_name(&expr), "avg(x)");
    }

    #[test]
    fn expression_output_name_time_window_stays_bare() {
        let w = func(
            "window",
            vec![unresolved_col("last_login"), lit_str("1 day")],
        );
        assert_eq!(expression_output_name(&w), "window");
        let sw = func(
            "session_window",
            vec![unresolved_col("ts"), lit_str("5 minutes")],
        );
        assert_eq!(expression_output_name(&sw), "session_window");
    }

    #[test]
    fn expression_output_name_unaliased_nested_round_uses_pretty_name() {
        let div = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(unresolved_col("s1")),
            right: Box::new(unresolved_col("s2")),
        });
        let expr = func("round", vec![div, int_lit(2)]);
        assert_eq!(expression_output_name(&expr), "round((s1 / s2), 2)");
    }

    #[test]
    fn expression_output_name_aliased_function_call_keeps_alias() {
        let expr = alias_expr(func("sum", vec![unresolved_col("salary")]), "total");
        assert_eq!(expression_output_name(&expr), "total");
    }

    #[test]
    fn expression_output_name_passthrough_column_unchanged() {
        assert_eq!(
            expression_output_name(&unresolved_col("dept_id")),
            "dept_id"
        );
    }

    #[test]
    fn expression_output_name_literal_unchanged() {
        assert_eq!(expression_output_name(&int_lit(1)), "col");
    }

    #[test]
    fn sel_008_shaped_project_names_unaliased_computed_columns() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![
                unresolved_col("id"),
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::Add,
                    left: Box::new(unresolved_col("dept_id")),
                    right: Box::new(int_lit(1)),
                }),
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::Div,
                    left: Box::new(unresolved_col("salary")),
                    right: Box::new(int_lit(1000)),
                }),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze sel-008-shaped project");
        let names = field_names(&typed);
        assert_eq!(names, vec!["id", "(dept_id + 1)", "(salary / 1000)"]);
    }

    fn aliased(table: &str, alias: &str) -> CommonAst {
        CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: table.to_owned(),
            })),
            alias: alias.to_owned(),
        })
    }

    #[test]
    fn unqualified_duplicate_name_over_join_is_ambiguous() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(
                aliased("emp", "e"),
                aliased("dept", "d"),
                JoinType::Cross,
                None,
            )),
            projections: vec![unresolved_col("dept_id")],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::AmbiguousColumn { .. }));
    }

    #[test]
    fn qualifier_binds_relation_but_field_absent_is_unknown_column() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(
                aliased("emp", "e"),
                aliased("dept", "d"),
                JoinType::Cross,
                None,
            )),
            projections: vec![qcol("d", "id")],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "id");
                assert_eq!(qualifier.as_deref(), Some("d"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn self_join_referenced_by_shadowed_table_name_is_unresolved() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(
                aliased("emp", "e1"),
                aliased("emp", "e2"),
                JoinType::Inner,
                None,
            )),
            projections: vec![qcol("emp", "id")],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn {
                ref name,
                ref qualifier,
            } => {
                assert_eq!(name, "id");
                assert_eq!(qualifier.as_deref(), Some("emp"));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn qualifier_binds_both_sides_of_join_is_ambiguous() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(
                aliased("emp", "x"),
                aliased("dept", "x"),
                JoinType::Inner,
                None,
            )),
            projections: vec![qcol("x", "id")],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::AmbiguousColumn {
                ref name,
                ref candidates,
            } => {
                assert_eq!(name, "id");
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousColumn, got {other:?}"),
        }
    }

    #[test]
    fn struct_qualifier_wins_over_colliding_relation_alias() {
        let bt = base_types_for(&[("emp", analyzer_fixtures::emp_schema())]);
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "address".to_owned(),
            })),
            projections: vec![qcol("address", "city")],
        });
        let typed =
            analyze(ast, &bt).expect("struct-field access must win over the colliding alias");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::String);
        assert!(typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn join_condition_qualifier_stamps_correct_side_field() {
        let bt = base_types_for(&[("emp", analyzer_fixtures::emp_schema())]);
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("e1", "id")),
            right: Box::new(qcol("e2", "manager_id")),
        });
        let ast = join(
            aliased("emp", "e1"),
            aliased("emp", "e2"),
            JoinType::Inner,
            Some(cond),
        );
        let typed = analyze(ast, &bt).expect("join condition resolves");
        match &typed.op {
            TypedOp::Join {
                condition: Some(Expression::Binary(b)),
                ..
            } => match (&*b.left, &*b.right) {
                (Expression::ColumnReference(l), Expression::ColumnReference(r)) => {
                    assert_eq!(l.data_type, DataType::Long);
                    assert!(!l.nullable);
                    assert_eq!(r.data_type, DataType::Long);
                    assert!(r.nullable);
                }
                other => panic!("expected two ColumnReferences, got {other:?}"),
            },
            other => panic!("expected Join with a Binary condition, got {other:?}"),
        }
    }

    #[test]
    fn using_join_qualifier_resolution_stays_on_legacy_path_no_panic() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Join {
                left: Box::new(aliased("emp", "e")),
                right: Box::new(aliased("dept", "d")),
                join_type: JoinType::Inner,
                condition: None,
                using_columns: vec!["dept_id".to_owned()],
                natural: false,
                lateral: false,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            })),
            projections: vec![qcol("d", "dept_id")],
        });
        let typed = analyze(ast, &bt).expect("USING join qualifier resolution must not panic");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
    }

    fn contains_unresolved_ref(expr: &Expression, name: &str) -> bool {
        match expr {
            Expression::UnresolvedColumn(u) => eq_fold(&u.name, name),
            other => other.children().any(|c| contains_unresolved_ref(c, name)),
        }
    }

    #[test]
    fn lateral_column_alias_single_ref_resolves_and_types() {
        let bt = base_types_for(&[("emp", emp_schema())]);
        let raised = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Mul,
                left: Box::new(unresolved_col("salary")),
                right: Box::new(lit_double(1.1)),
            }),
            "raised",
        );
        let delta = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Sub,
                left: Box::new(unresolved_col("raised")),
                right: Box::new(unresolved_col("salary")),
            }),
            "delta",
        );
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![raised, delta],
        });
        let typed = analyze(ast, &bt).expect("lateral column alias must resolve");
        assert_eq!(field_names(&typed), vec!["raised", "delta"]);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Double);
        assert_eq!(typed.resolved_schema.fields[1].data_type, DataType::Double);
    }

    #[test]
    fn lateral_column_alias_chain_inlines_in_one_pass() {
        let schema = ResolvedSchema::minted(emp_schema());
        let a = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(unresolved_col("salary")),
                right: Box::new(lit_double(1.0)),
            }),
            "a",
        );
        let b = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(unresolved_col("a")),
                right: Box::new(lit_double(1.0)),
            }),
            "b",
        );
        let c = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(unresolved_col("b")),
                right: Box::new(lit_double(1.0)),
            }),
            "c",
        );
        let expanded = expand_lateral_column_aliases(vec![a, b, c], &schema)
            .expect("chained lateral aliases must inline in one pass");
        let c_expr = match &expanded[2] {
            Expression::Alias(alias) => &*alias.expr,
            other => panic!("expected Alias, got {other:?}"),
        };
        assert!(
            !contains_unresolved_ref(c_expr, "a"),
            "chain must fully inline away the `a` reference"
        );
        assert!(
            !contains_unresolved_ref(c_expr, "b"),
            "chain must fully inline away the `b` reference"
        );
        assert!(contains_unresolved_ref(c_expr, "salary"));
    }

    #[test]
    fn lateral_column_alias_input_column_wins_over_alias() {
        let schema = ResolvedSchema::minted(emp_schema());
        let shadow = alias_expr(unresolved_col("id"), "dept_id");
        let later = unresolved_col("dept_id");
        let expanded = expand_lateral_column_aliases(vec![shadow, later], &schema)
            .expect("input-column collision must not error");
        match &expanded[1] {
            Expression::UnresolvedColumn(u) => assert_eq!(u.name, "dept_id"),
            other => panic!("expected untouched UnresolvedColumn(\"dept_id\"), got {other:?}"),
        }
    }

    #[test]
    fn lateral_column_alias_ambiguous_when_referenced() {
        let schema = ResolvedSchema::minted(emp_schema());
        let x1 = alias_expr(unresolved_col("salary"), "x");
        let x2 = alias_expr(unresolved_col("id"), "x");
        let referencer = alias_expr(unresolved_col("x"), "y");
        let err = expand_lateral_column_aliases(vec![x1, x2, referencer], &schema).unwrap_err();
        match err {
            AnalyzerError::AmbiguousLateralColumnAlias { name, count } => {
                assert_eq!(name, "x");
                assert_eq!(count, 2);
            }
            other => panic!("expected AmbiguousLateralColumnAlias, got {other:?}"),
        }
    }

    #[test]
    fn lateral_column_alias_duplicate_name_without_reference_is_not_an_error() {
        let schema = ResolvedSchema::minted(emp_schema());
        let x1 = alias_expr(unresolved_col("salary"), "x");
        let x2 = alias_expr(unresolved_col("id"), "x");
        let expanded = expand_lateral_column_aliases(vec![x1, x2], &schema)
            .expect("duplicate alias names with no later reference must not error");
        assert_eq!(expanded.len(), 2);
    }

    #[test]
    fn lateral_column_alias_substitutes_inside_function_call_and_case_when() {
        let schema = ResolvedSchema::minted(emp_schema());
        let raised = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Mul,
                left: Box::new(unresolved_col("salary")),
                right: Box::new(lit_double(1.1)),
            }),
            "raised",
        );
        let via_call = alias_expr(func("abs", vec![unresolved_col("raised")]), "abs_raised");
        let case_expr = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(unresolved_col("raised")),
                    right: Box::new(int_lit(0)),
                }),
                unresolved_col("raised"),
            )],
            else_expr: Some(Box::new(unresolved_col("salary"))),
        });
        let via_case = alias_expr(case_expr, "case_raised");

        let expanded = expand_lateral_column_aliases(vec![raised, via_call, via_case], &schema)
            .expect("nested lateral refs must substitute");

        let call_expr = match &expanded[1] {
            Expression::Alias(alias) => &*alias.expr,
            other => panic!("expected Alias, got {other:?}"),
        };
        assert!(!contains_unresolved_ref(call_expr, "raised"));
        assert!(contains_unresolved_ref(call_expr, "salary"));

        let case_expr = match &expanded[2] {
            Expression::Alias(alias) => &*alias.expr,
            other => panic!("expected Alias, got {other:?}"),
        };
        assert!(!contains_unresolved_ref(case_expr, "raised"));
    }

    #[test]
    fn lateral_column_alias_forward_reference_still_unknown_column() {
        let bt = base_types_for(&[("emp", emp_schema())]);
        let first = alias_expr(unresolved_col("delta"), "early");
        let second = alias_expr(unresolved_col("salary"), "delta");
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![first, second],
        });
        let err = analyze(ast, &bt).unwrap_err();
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "delta");
                assert_eq!(qualifier, None);
            }
            other => panic!("expected UnknownColumn(\"delta\"), got {other:?}"),
        }
    }

    #[test]
    fn lateral_column_alias_does_not_reach_into_lambda_body() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "arr",
            DataType::Array(Box::new(DataType::Integer), true),
        )]));
        let outer_x = alias_expr(int_lit(1), "x");
        let lambda_body = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                name: "x".to_owned(),
            })),
            right: Box::new(int_lit(1)),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x".to_owned()],
            body: Box::new(lambda_body),
        });
        let transform_call = alias_expr(
            func("transform", vec![unresolved_col("arr"), lambda]),
            "transformed",
        );
        let expanded = expand_lateral_column_aliases(vec![outer_x, transform_call], &schema)
            .expect("lambda-shadowed name must not error");
        let lambda_arg = match &expanded[1] {
            Expression::Alias(alias) => match &*alias.expr {
                Expression::FunctionCall(f) => &f.args[1],
                other => panic!("expected FunctionCall, got {other:?}"),
            },
            other => panic!("expected Alias, got {other:?}"),
        };
        match lambda_arg {
            Expression::Lambda(l) => match &*l.body {
                Expression::Binary(b) => match &*b.left {
                    Expression::LambdaVariable(lv) => assert_eq!(lv.name, "x"),
                    other => panic!("expected LambdaVariable untouched, got {other:?}"),
                },
                other => panic!("expected Binary, got {other:?}"),
            },
            other => panic!("expected Lambda untouched, got {other:?}"),
        }
    }

    fn emp_tags_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("tags", DataType::Array(Box::new(DataType::String), true)),
        ])
    }

    fn generate_plan(name: &str, arg: Expression, aliases: &[&str]) -> CommonAst {
        let mut generator = Generator::from_function(name, vec![arg]).expect("generator name");
        generator.aliases = aliases.iter().map(|alias| (*alias).to_owned()).collect();
        CommonAst::new(CommonOp::Generate {
            input: Box::new(aliased("emp", "e")),
            generator,
            qualifier: Some("t".to_owned()),
        })
    }

    #[test]
    fn generate_appends_schema_and_binds_qualifier() {
        let generated = generate_plan("explode", qcol("e", "tags"), &["tag"]);
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(generated),
            projections: vec![qcol("e", "id"), qcol("t", "tag")],
        });
        let typed = analyze(plan, &base_types_for(&[("emp", emp_tags_schema())]))
            .expect("qualified generator columns");
        assert_eq!(field_names(&typed), ["id", "tag"]);
        assert_eq!(typed.resolved_schema.fields[1].data_type, DataType::String);
    }

    #[test]
    fn generate_qualifier_does_not_capture_input_columns() {
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(generate_plan("explode", qcol("e", "tags"), &["tag"])),
            projections: vec![qcol("e", "tag")],
        });
        let err = analyze(plan, &base_types_for(&[("emp", emp_tags_schema())]))
            .expect_err("input qualifier must not bind generated columns");
        assert!(matches!(
            err,
            AnalyzerError::UnknownColumn {
                name,
                qualifier: Some(qualifier)
            } if name == "tag" && qualifier == "e"
        ));
    }

    #[test]
    fn posexplode_outer_makes_position_and_value_nullable() {
        let schema = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::not_null("tags", DataType::Array(Box::new(DataType::String), false)),
        ]);
        let typed = analyze(
            generate_plan("posexplode_outer", qcol("e", "tags"), &["pos", "tag"]),
            &base_types_for(&[("emp", schema)]),
        )
        .expect("posexplode_outer");
        let generated = &typed.resolved_schema.fields[2..];
        assert_eq!(generated[0].data_type, DataType::Integer);
        assert_eq!(generated[1].data_type, DataType::String);
        assert!(generated.iter().all(|field| field.nullable));
    }

    #[test]
    fn map_posexplode_outer_derives_three_nullable_columns() {
        let schema = StructType::new(vec![StructField::not_null(
            "attrs",
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::Long),
                value_nullable: false,
            },
        )]);
        let typed = analyze(
            generate_plan(
                "posexplode_outer",
                qcol("e", "attrs"),
                &["pos", "key", "value"],
            ),
            &base_types_for(&[("emp", schema)]),
        )
        .expect("map posexplode_outer");
        let generated = &typed.resolved_schema.fields[1..];
        assert_eq!(
            generated
                .iter()
                .map(|field| (&field.name, &field.data_type))
                .collect::<Vec<_>>(),
            [
                (&"pos".to_owned(), &DataType::Integer),
                (&"key".to_owned(), &DataType::String),
                (&"value".to_owned(), &DataType::Long),
            ]
        );
        assert!(generated.iter().all(|field| field.nullable));
    }

    #[test]
    fn inner_position_is_non_nullable() {
        let typed = analyze(
            generate_plan("posexplode", qcol("e", "tags"), &["pos", "tag"]),
            &base_types_for(&[("emp", emp_tags_schema())]),
        )
        .expect("posexplode");
        assert!(!typed.resolved_schema.fields[3].nullable);
        assert!(typed.resolved_schema.fields[4].nullable);
    }

    fn lateral_join(
        left: CommonAst,
        right: CommonAst,
        join_type: JoinType,
        condition: Option<Expression>,
    ) -> CommonAst {
        CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type,
            condition,
            using_columns: vec![],
            natural: false,
            lateral: true,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        })
    }

    #[test]
    fn lateral_join_analyzes_with_outer_scope_from_left_sibling() {
        let bt = base_types_with_emp_dept();
        let left = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan("emp")),
            alias: "e".to_owned(),
        });
        let right_inner = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![Expression::Alias(
                crate::transpiler_v2::expression::AliasExpression {
                    expr: Box::new(qcol("e", "name")),
                    alias: "dept_avg".to_owned(),
                },
            )],
        });
        let right = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(right_inner),
            alias: "t".to_owned(),
        });
        let lateral = lateral_join(left, right, JoinType::Inner, None);
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral),
            projections: vec![qcol("e", "name"), qcol("t", "dept_avg")],
        });
        let typed = analyze(plan, &bt).expect("lateral join must analyze");
        assert_eq!(typed.resolved_schema.fields.len(), 2);
        assert_eq!(typed.resolved_schema.fields[0].name, "name");
        assert_eq!(typed.resolved_schema.fields[1].name, "dept_avg");
        match &typed.op {
            TypedOp::Project { input, .. } => match &input.op {
                TypedOp::Join {
                    join_type, lateral, ..
                } => {
                    assert_eq!(*join_type, JoinType::Cross, "lateral Inner no ON → Cross");
                    assert!(*lateral, "lateral must be stamped on TypedOp::Join");
                }
                other => panic!("expected Join, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn lateral_join_with_natural_errors() {
        let bt = base_types_with_emp_dept();
        let left = scan("emp");
        let right = scan("dept");
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            natural: true,
            lateral: true,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let err = analyze(ast, &bt).expect_err("lateral + natural must error");
        assert_eq!(
            err.spark_class(),
            Some("INCOMPATIBLE_JOIN_TYPES"),
            "got: {err:?}"
        );
    }

    #[test]
    fn lateral_join_with_using_errors() {
        let bt = base_types_with_emp_dept();
        let left = scan("emp");
        let right = scan("dept");
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: true,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let err = analyze(ast, &bt).expect_err("lateral + USING must error");
        assert_eq!(
            err.spark_class(),
            Some("UNSUPPORTED_FEATURE.LATERAL_JOIN_USING"),
            "got: {err:?}"
        );
    }

    #[test]
    fn lateral_join_with_left_semi_punts() {
        let bt = base_types_with_emp_dept();
        let left = scan("emp");
        let right = scan("dept");
        let ast = lateral_join(left, right, JoinType::LeftSemi, None);
        let err = analyze(ast, &bt).expect_err("lateral LeftSemi must punt");
        assert!(
            matches!(err, AnalyzerError::PuntedOperator { .. }),
            "expected PuntedOperator, got {err:?}"
        );
    }

    #[test]
    fn lateral_join_with_right_join_type_punts() {
        let bt = base_types_with_emp_dept();
        let left = scan("emp");
        let right = scan("dept");
        let ast = lateral_join(left, right, JoinType::Right, None);
        let err = analyze(ast, &bt).expect_err("lateral Right must punt");
        assert!(
            matches!(err, AnalyzerError::PuntedOperator { .. }),
            "expected PuntedOperator, got {err:?}"
        );
    }

    #[test]
    fn lateral_join_one_level_only_grandparent_ref_fails() {
        use crate::transpiler_v2::expression::{ExistsSubquery, SubqueryPlan};
        let bt = base_types_with_emp_dept();
        let lateral_left = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan("emp")),
            alias: "e".to_owned(),
        });
        let lateral_right_subq = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![Expression::Alias(
                crate::transpiler_v2::expression::AliasExpression {
                    expr: Box::new(qcol("d", "dept_name")),
                    alias: "x".to_owned(),
                },
            )],
        });
        let lateral_right = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(lateral_right_subq),
            alias: "t".to_owned(),
        });
        let lateral_node = lateral_join(lateral_left, lateral_right, JoinType::Inner, None);
        let exists_subq = Expression::ExistsSubquery(ExistsSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(CommonAst::new(CommonOp::Project {
                input: Box::new(lateral_node),
                projections: vec![unresolved_col("x")],
            }))),
            negated: false,
        });
        let outer_plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: exists_subq,
            })),
            projections: vec![qcol("d", "dept_name")],
        });
        let err = analyze(outer_plan, &bt).expect_err("grandparent ref must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("UnknownColumn") || msg.contains("unknown column"),
            "expected UnknownColumn for d.dept_name leaked from inherited outer, got: {msg}"
        );
    }

    #[test]
    fn non_lateral_inner_join_no_on_still_boundary_errors() {
        let bt = base_types_with_emp_dept();
        let left = scan("emp");
        let right = scan("dept");
        let ast = join(left, right, JoinType::Inner, None);
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(ast),
            projections: vec![unresolved_col("name")],
        });
        let typed = analyze(plan, &bt).expect("non-lateral analyze OK");
        let result = crate::transpiler_v2::emission::dispatch_op(&typed.op, &typed.resolved_schema);
        assert!(result.is_err(), "non-lateral clauseless Inner must error");
    }

    fn emp_schema_with_manager() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("manager_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ])
    }

    fn recursive_cte(
        name: &str,
        column_names: Vec<&str>,
        union_all: bool,
        anchor: CommonAst,
        recursive_term: CommonAst,
    ) -> CommonAst {
        CommonAst::new(CommonOp::RecursiveCte {
            name: name.to_owned(),
            column_names: column_names.into_iter().map(|s| s.to_owned()).collect(),
            union_all,
            anchor: Box::new(anchor),
            recursive_term: Box::new(recursive_term),
        })
    }

    #[test]
    fn analyze_recursive_cte_009_simple_sequence() {
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(scan("seq")),
                condition: Expression::Binary(BinaryExpression {
                    left: Box::new(unresolved_col("n")),
                    op: BinaryOp::Lt,
                    right: Box::new(int_lit(5)),
                }),
            })),
            projections: vec![Expression::Binary(BinaryExpression {
                left: Box::new(unresolved_col("n")),
                op: BinaryOp::Add,
                right: Box::new(int_lit(1)),
            })],
        });
        let cte_node = recursive_cte("seq", vec!["n"], true, anchor, recursive_term);
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "seq".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });

        let bt = BaseTypes::empty();
        let typed = analyze(outer, &bt).expect("analyze cte-009");
        assert_eq!(field_names(&typed), vec!["n"]);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Integer);
        assert!(typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn analyze_recursive_cte_010_join_form() {
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(scan("emp")),
                condition: Expression::Unary(UnaryExpression {
                    op: UnaryOp::IsNull,
                    operand: Box::new(unresolved_col("manager_id")),
                }),
            })),
            projections: vec![
                unresolved_col("id"),
                unresolved_col("name"),
                unresolved_col("manager_id"),
                int_lit(0),
            ],
        });
        let emp_aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan("emp")),
            alias: "e".to_owned(),
        });
        let chain_aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan("chain")),
            alias: "c".to_owned(),
        });
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(qcol("e", "manager_id")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("c", "id")),
        });
        let joined = join(emp_aliased, chain_aliased, JoinType::Inner, Some(join_cond));
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(joined),
            projections: vec![
                qcol("e", "id"),
                qcol("e", "name"),
                qcol("e", "manager_id"),
                Expression::Binary(BinaryExpression {
                    left: Box::new(qcol("c", "lvl")),
                    op: BinaryOp::Add,
                    right: Box::new(int_lit(1)),
                }),
            ],
        });

        let cte_node = recursive_cte(
            "chain",
            vec!["id", "name", "manager_id", "lvl"],
            true,
            anchor,
            recursive_term,
        );
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "chain".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });

        let bt = base_types_for(&[("emp", emp_schema_with_manager())]);
        let typed = analyze(outer, &bt).expect("analyze cte-010");

        assert_eq!(field_names(&typed), vec!["id", "name", "manager_id", "lvl"]);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
        assert_eq!(typed.resolved_schema.fields[3].data_type, DataType::Integer);
        assert!(!typed.resolved_schema.fields[0].nullable);
        assert!(typed.resolved_schema.fields[3].nullable);
    }

    #[test]
    fn analyze_recursive_cte_union_without_all_rejected() {
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("seq")),
            projections: vec![unresolved_col("n")],
        });
        let cte_node = recursive_cte("seq", vec!["n"], false, anchor, recursive_term);
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "seq".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let bt = BaseTypes::empty();
        let err = analyze(outer, &bt).expect_err("should reject UNION without ALL");
        assert_eq!(
            err.spark_class(),
            Some("UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE"),
            "got: {err:?}"
        );
        assert_eq!(err.category(), ErrorCategory::SparkEmulated);
    }

    #[test]
    fn analyze_recursive_cte_column_list_arity_mismatch() {
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("seq")),
            projections: vec![unresolved_col("n")],
        });
        let cte_node = recursive_cte("seq", vec!["a", "b"], true, anchor, recursive_term);
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "seq".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let bt = BaseTypes::empty();
        let err = analyze(outer, &bt).expect_err("should reject arity mismatch");
        let msg = format!("{err}");
        assert!(
            msg.contains("2 names") && msg.contains("1 columns"),
            "error should cite the arity mismatch, got: {msg}"
        );
    }

    #[test]
    fn analyze_recursive_cte_anchor_recursive_arity_mismatch() {
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("seq")),
            projections: vec![unresolved_col("n"), int_lit(99)],
        });
        let cte_node = recursive_cte("seq", vec!["n"], true, anchor, recursive_term);
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "seq".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let bt = BaseTypes::empty();
        let err = analyze(outer, &bt).expect_err("should reject recursive term arity mismatch");
        let msg = format!("{err}");
        assert!(
            msg.contains("anchor has 1 columns") && msg.contains("produces 2"),
            "error should cite the anchor/recursive arity mismatch, got: {msg}"
        );
    }

    #[test]
    fn analyze_recursive_cte_shadowing_base_types_entry() {
        let catalog_seq_schema = StructType::new(vec![
            StructField::nullable("n", DataType::String), // different type!
        ]);
        let bt = base_types_for(&[("seq", catalog_seq_schema)]);

        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("seq")),
            projections: vec![Expression::Binary(BinaryExpression {
                left: Box::new(unresolved_col("n")),
                op: BinaryOp::Add,
                right: Box::new(int_lit(1)),
            })],
        });
        let cte_node = recursive_cte("seq", vec!["n"], true, anchor, recursive_term);
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "seq".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });

        let typed = analyze(outer, &bt).expect("analyze should succeed (CTE shadows catalog)");
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Integer);
        let cte_op = match &typed.op {
            TypedOp::Project { input, .. } => match &input.op {
                TypedOp::AliasedRelation { input, .. } => &input.op,
                other => panic!("expected AliasedRelation, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        };
        match cte_op {
            TypedOp::RecursiveCte { recursive_term, .. } => {
                let inner_input = match &recursive_term.op {
                    TypedOp::Project { input, .. } => input,
                    other => panic!("expected Project in recursive term, got {other:?}"),
                };
                assert_eq!(
                    inner_input.resolved_schema.fields[0].data_type,
                    DataType::Integer,
                    "recursive term's TableScan must resolve via injected schema (Integer), \
                     not catalog (String)"
                );
            }
            other => panic!("expected RecursiveCte, got {other:?}"),
        }
    }

    #[test]
    fn analyze_recursive_cte_case_mismatch_self_ref_resolves() {
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(scan("Seq")),
                condition: Expression::Binary(BinaryExpression {
                    left: Box::new(unresolved_col("n")),
                    op: BinaryOp::Lt,
                    right: Box::new(int_lit(5)),
                }),
            })),
            projections: vec![Expression::Binary(BinaryExpression {
                left: Box::new(unresolved_col("n")),
                op: BinaryOp::Add,
                right: Box::new(int_lit(1)),
            })],
        });
        let cte_node = recursive_cte("seq", vec!["n"], true, anchor, recursive_term);
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "seq".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });

        let bt = BaseTypes::empty();
        let typed = analyze(outer, &bt)
            .expect("case-mismatched self-reference must resolve, not UnknownTable");
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Integer);
    }

    #[test]
    fn analyze_recursive_cte_uppercase_self_ref_shadows_catalog() {
        let catalog_schema = StructType::new(vec![StructField::nullable("n", DataType::String)]);
        let bt = base_types_for(&[("Seq", catalog_schema)]);

        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("Seq")),
            projections: vec![Expression::Binary(BinaryExpression {
                left: Box::new(unresolved_col("n")),
                op: BinaryOp::Add,
                right: Box::new(int_lit(1)),
            })],
        });
        let cte_node = recursive_cte("seq", vec!["n"], true, anchor, recursive_term);
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "seq".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });

        let typed =
            analyze(outer, &bt).expect("CTE injection must shadow the catalog entry for 'Seq'");
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Integer);
    }

    #[test]
    fn analyze_recursive_cte_010_c_lvl_resolves_integer() {
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(scan("emp")),
                condition: Expression::Unary(UnaryExpression {
                    op: UnaryOp::IsNull,
                    operand: Box::new(unresolved_col("manager_id")),
                }),
            })),
            projections: vec![
                unresolved_col("id"),
                unresolved_col("name"),
                unresolved_col("manager_id"),
                int_lit(0),
            ],
        });
        let emp_aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan("emp")),
            alias: "e".to_owned(),
        });
        let chain_aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan("chain")),
            alias: "c".to_owned(),
        });
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(qcol("e", "manager_id")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("c", "id")),
        });
        let joined = join(emp_aliased, chain_aliased, JoinType::Inner, Some(join_cond));
        let recursive_term = CommonAst::new(CommonOp::Project {
            input: Box::new(joined),
            projections: vec![
                qcol("e", "id"),
                qcol("e", "name"),
                qcol("e", "manager_id"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(Expression::Binary(BinaryExpression {
                        left: Box::new(qcol("c", "lvl")),
                        op: BinaryOp::Add,
                        right: Box::new(int_lit(1)),
                    })),
                    alias: "lvl_plus_1".to_owned(),
                }),
            ],
        });

        let cte_node = recursive_cte(
            "chain",
            vec!["id", "name", "manager_id", "lvl"],
            true,
            anchor,
            recursive_term,
        );
        let outer = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(cte_node),
                alias: "chain".to_owned(),
            })),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });

        let bt = base_types_for(&[("emp", emp_schema_with_manager())]);
        let typed = analyze(outer, &bt).expect("analyze cte-010 join form");

        let lvl_field = typed
            .resolved_schema
            .field_by_name("lvl")
            .expect("lvl field should exist");
        assert_eq!(
            lvl_field.data_type,
            DataType::Integer,
            "c.lvl must resolve as Integer from the injected anchor schema"
        );
    }

    fn plan_id_col(name: &str, pid: i64) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: Some(pid),
        })
    }

    fn join_with_plan_ids(
        left: CommonAst,
        right: CommonAst,
        join_type: JoinType,
        condition: Option<Expression>,
        left_plan_ids: Vec<i64>,
        right_plan_ids: Vec<i64>,
    ) -> CommonAst {
        CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type,
            condition,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids,
            right_plan_ids,
        })
    }

    #[test]
    fn plan_id_disambiguates_self_join_project_above() {
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(pcol("id", 1)),
            op: BinaryOp::Eq,
            right: Box::new(pcol("id", 2)),
        });
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("emp"),
            JoinType::Inner,
            Some(join_cond),
            vec![1],
            vec![2],
        );
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(joined),
            projections: vec![plan_id_col("id", 2)],
        });
        let typed = analyze(project, &bt).expect("plan_id should disambiguate above join");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].name, "id");
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
        if let TypedOp::Project { input, projections } = &typed.op {
            match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(
                        c.qualifier, None,
                        "plan_id=2 must resolve bare (Phase 3b: no synthetic qualifier)"
                    );
                    assert_eq!(c.expr_id, Some(input.resolved_schema.fields[4].expr_id));
                }
                other => panic!("expected ColumnReference, got: {other:?}"),
            }
        } else {
            panic!("expected Project op");
        }
    }

    #[test]
    fn plan_id_disambiguates_filter_above_join() {
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(pcol("salary", 1)),
            op: BinaryOp::Eq,
            right: Box::new(pcol("salary", 2)),
        });
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("emp"),
            JoinType::Inner,
            Some(join_cond),
            vec![1],
            vec![2],
        );
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(joined),
            condition: Expression::Binary(BinaryExpression {
                left: Box::new(plan_id_col("salary", 1)),
                op: BinaryOp::Gt,
                right: Box::new(int_lit(5)),
            }),
        });
        let typed = analyze(filter, &bt).expect("plan_id should disambiguate filter above join");
        assert_eq!(typed.resolved_schema.fields.len(), 8);
        if let TypedOp::Filter {
            input,
            condition: Expression::Binary(b),
        } = &typed.op
        {
            match b.left.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(
                        c.qualifier, None,
                        "plan_id=1 must resolve bare (Phase 3b: no synthetic qualifier)"
                    );
                    assert_eq!(c.expr_id, Some(input.resolved_schema.fields[3].expr_id));
                }
                other => panic!("expected ColumnReference, got: {other:?}"),
            }
        }
    }

    #[test]
    fn plan_id_binds_both_sides_of_same_join_is_ambiguous() {
        let bt = base_types_with_emp_dept();
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("emp"),
            JoinType::Inner,
            None,
            vec![1],
            vec![1],
        );
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(joined),
            projections: vec![plan_id_col("id", 1)],
        });
        let err = analyze(project, &bt).unwrap_err();
        match err {
            AnalyzerError::AmbiguousColumnReference { ref name } => {
                assert_eq!(name, "id");
            }
            other => panic!("expected AmbiguousColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn condition_binds_both_sides_of_same_join_is_ambiguous_column_reference() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(pcol("id", 1)),
            right: Box::new(pcol("id", 1)),
        });
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("emp"),
            JoinType::Inner,
            Some(cond),
            vec![1],
            vec![1],
        );
        let err = analyze(joined, &bt).unwrap_err();
        match err {
            AnalyzerError::AmbiguousColumnReference { ref name } => {
                assert_eq!(name, "id");
            }
            other => panic!("expected AmbiguousColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn for_join_condition_binds_own_plan_ids_not_just_children_scope() {
        let left_schema = StructType::new(vec![
            StructField::not_null("dept_id", DataType::Integer),
            StructField::not_null("f1", DataType::Integer),
            StructField::not_null("f2", DataType::Integer),
            StructField::not_null("f3", DataType::Integer),
            StructField::not_null("f4", DataType::Integer),
            StructField::not_null("f5", DataType::Integer),
        ]);
        let right_schema =
            StructType::new(vec![StructField::not_null("dept_id", DataType::Integer)]);
        let bt = base_types_for(&[("left_t", left_schema), ("right_t", right_schema)]);
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(pcol("dept_id", 1)),
            right: Box::new(pcol("dept_id", 2)),
        });
        let joined = join_with_plan_ids(
            scan("left_t"),
            scan("right_t"),
            JoinType::Inner,
            Some(cond),
            vec![1],
            vec![2],
        );
        let typed = analyze(joined, &bt).expect("distinct-pid dept_id condition should resolve");
        let (l, r) = join_condition_refs(&typed);
        assert_eq!(l.qualifier, None);
        assert_eq!(l.expr_id, Some(merged_join_expr_id_at(&typed, 0)));
        assert_eq!(r.qualifier, None);
        assert_eq!(r.expr_id, Some(merged_join_expr_id_at(&typed, 6)));
    }

    #[test]
    fn plan_id_three_way_nested_join() {
        let bt = base_types_with_emp_dept();
        let inner_cond = Expression::Binary(BinaryExpression {
            left: Box::new(pcol("id", 1)),
            op: BinaryOp::Eq,
            right: Box::new(pcol("id", 2)),
        });
        let inner_join = join_with_plan_ids(
            scan("emp"),
            scan("emp"),
            JoinType::Inner,
            Some(inner_cond),
            vec![1],
            vec![2],
        );
        let outer_cond = Expression::Binary(BinaryExpression {
            left: Box::new(pcol("dept_id", 1)),
            op: BinaryOp::Eq,
            right: Box::new(pcol("dept_id", 3)),
        });
        let outer_join = join_with_plan_ids(
            inner_join,
            scan("dept"),
            JoinType::Inner,
            Some(outer_cond),
            vec![1, 2],
            vec![3],
        );
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![plan_id_col("dept_id", 3)],
        });
        let typed = analyze(project, &bt).expect("plan_id 3 should resolve to dept side");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].name, "dept_id");
        assert!(!typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn plan_id_unique_column_omits_qualifier() {
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(pcol("dept_id", 1)),
            op: BinaryOp::Eq,
            right: Box::new(pcol("dept_id", 2)),
        });
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("dept"),
            JoinType::Inner,
            Some(join_cond),
            vec![1],
            vec![2],
        );
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(joined),
            condition: Expression::Binary(BinaryExpression {
                left: Box::new(plan_id_col("salary", 1)),
                op: BinaryOp::Gt,
                right: Box::new(int_lit(0)),
            }),
        });
        let typed = analyze(filter, &bt).expect("unique column should resolve without qualifier");
        if let TypedOp::Filter {
            condition: Expression::Binary(b),
            ..
        } = &typed.op
        {
            match b.left.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(
                        c.qualifier, None,
                        "unique column 'salary' must NOT carry a synthetic qualifier"
                    );
                }
                other => panic!("expected ColumnReference, got: {other:?}"),
            }
        }
    }

    #[test]
    fn plan_id_unknown_falls_back_to_legacy() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![plan_id_col("id", 99)],
        });
        let typed = analyze(project, &bt).expect("unknown plan_id should fall back");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].name, "id");
    }

    #[test]
    fn plan_id_under_plain_non_join_input_unaffected() {
        let bt = base_types_with_emp_dept();
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(scan("emp")),
            condition: Expression::Binary(BinaryExpression {
                left: Box::new(plan_id_col("salary", 42)),
                op: BinaryOp::Gt,
                right: Box::new(int_lit(100)),
            }),
        });
        let typed = analyze(filter, &bt).expect("plan_id with no join should resolve normally");
        assert_eq!(typed.resolved_schema.fields.len(), 4);
    }

    #[test]
    fn user_typed_td_jl_qualifier_is_unknown_column_not_panic() {
        let bt = base_types_for(&[("emp", emp_schema())]);
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("id")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(project),
            condition: Expression::Binary(BinaryExpression {
                left: Box::new(qcol("__td_jl", "id")),
                op: BinaryOp::Gt,
                right: Box::new(int_lit(0)),
            }),
        });
        let err = analyze(ast, &bt).expect_err("user-typed __td_jl qualifier must be rejected");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "id");
                assert_eq!(qualifier, Some("__td_jl".to_owned()));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn user_typed_td_jr_qualifier_is_unknown_column() {
        let bt = base_types_for(&[("emp", emp_schema())]);
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("id")],
        });
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(project),
            condition: Expression::Binary(BinaryExpression {
                left: Box::new(qcol("__td_jr", "id")),
                op: BinaryOp::Gt,
                right: Box::new(int_lit(0)),
            }),
        });
        let err = analyze(ast, &bt).expect_err("user-typed __td_jr qualifier must be rejected");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "id");
                assert_eq!(qualifier, Some("__td_jr".to_owned()));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    fn asc_key(expr: Expression) -> SortOrder {
        SortOrder {
            expr: Box::new(expr),
            direction: SortDirection::Ascending,
            null_ordering: NullOrdering::NullsLast,
        }
    }

    #[test]
    fn sort_aggregate_restatement_binds_onto_n8_wrapped_entry() {
        let bt = base_types_with_emp_dept();
        let sum_salary = || func("sum", vec![unresolved_col("salary")]);
        let agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id")],
            vec![unresolved_col("dept_id"), sum_salary()],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(sum_salary())],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("aggregate restatement must resolve");
        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "sum(salary)");
                assert!(c.qualifier.is_none());
                assert_eq!(c.expr_id, Some(input.resolved_schema.fields[1].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &input.op {
            TypedOp::Aggregate { aggregates, .. } => match &aggregates[1] {
                Expression::Alias(a) => {
                    assert_eq!(a.alias, "sum(salary)");
                    assert!(matches!(
                        a.expr.as_ref(),
                        Expression::FunctionCall(f) if f.name.eq_ignore_ascii_case("sum")
                    ));
                }
                other => panic!("expected N8-wrapped sum(salary), got {other:?}"),
            },
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_project_binds_renamed_column_alias_stripped() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![alias_expr(unresolved_col("id"), "customer_id")],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(unresolved_col("id"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("renamed base column must resolve");
        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => assert_eq!(c.name, "customer_id"),
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &input.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::Alias(a) => assert_eq!(a.alias, "customer_id"),
                other => panic!("expected unchanged existing alias, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_project_binds_whole_expr_onto_n8_wrapped_entry() {
        let bt = base_types_with_emp_dept();
        let substr_name = || {
            func(
                "substr",
                vec![unresolved_col("name"), int_lit(1), int_lit(3)],
            )
        };
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![substr_name()],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(substr_name())],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("whole-expression match must resolve");
        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        let expected_name = "substr(name, 1, 3)";
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => assert_eq!(c.name, expected_name),
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &input.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::Alias(a) => assert_eq!(a.alias, expected_name),
                other => panic!("expected N8-wrapped substr entry, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn sort_count_star_over_global_aggregate_resolves_q096_shape() {
        let bt = base_types_with_emp_dept();
        let count_star = || {
            func(
                "count",
                vec![Expression::Star(StarExpression { qualifier: None })],
            )
        };
        let agg = aggregate(emp_scan(), vec![], vec![count_star()]);
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(count_star())],
            limit: None,
            offset: None,
        });
        let typed =
            analyze(ast, &bt).expect("count(*) restatement over a global aggregate must resolve");
        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert!(c.qualifier.is_none());
                assert_eq!(c.expr_id, Some(input.resolved_schema.fields[0].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &input.op {
            TypedOp::Aggregate { aggregates, .. } => {
                assert!(
                    matches!(&aggregates[0], Expression::Alias(a) if matches!(a.expr.as_ref(), Expression::FunctionCall(f) if f.name.eq_ignore_ascii_case("count"))),
                    "expected N8-wrapped count(*) entry, got {:?}",
                    aggregates[0]
                );
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn sort_by_existing_alias_is_unchanged_direct_path() {
        let bt = base_types_with_emp_dept();
        let agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id")],
            vec![
                unresolved_col("dept_id"),
                alias_expr(func("sum", vec![unresolved_col("salary")]), "total"),
            ],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(unresolved_col("total"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("ordering by an existing alias must resolve");
        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => assert_eq!(c.name, "total"),
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &input.op {
            TypedOp::Aggregate { aggregates, .. } => match &aggregates[1] {
                Expression::Alias(a) => assert_eq!(a.alias, "total"),
                other => panic!("expected unchanged existing alias, got {other:?}"),
            },
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn sort_by_plain_output_column_is_unchanged_direct_path() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![unresolved_col("id"), unresolved_col("name")],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(unresolved_col("id"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("ordering by a plain output column must resolve");
        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "id");
                assert_eq!(c.expr_id, Some(input.resolved_schema.fields[0].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_deduplicate_does_not_engage_fallback() {
        let bt = base_types_with_emp_dept();
        let dedup = CommonAst::new(CommonOp::Deduplicate {
            input: Box::new(emp_scan()),
            on_columns: vec![],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(dedup),
            order: vec![asc_key(unresolved_col("salary_typo"))],
            limit: None,
            offset: None,
        });
        let err = analyze(ast, &bt).expect_err("unresolvable key over Deduplicate must error");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "salary_typo");
                assert!(qualifier.is_none());
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn sort_key_genuinely_unresolvable_still_errors_unknown_column() {
        let bt = base_types_with_emp_dept();
        let agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id")],
            vec![
                unresolved_col("dept_id"),
                func("sum", vec![unresolved_col("salary")]),
            ],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(unresolved_col("does_not_exist"))],
            limit: None,
            offset: None,
        });
        let err = analyze(ast, &bt).expect_err("a genuinely unknown column must still error");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "does_not_exist");
                assert!(qualifier.is_none());
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_join_same_name_collision_binds_correct_output_column() {
        let bt = base_types_with_emp_dept();
        let joined = join(
            aliased_scan("emp", "e"),
            aliased_scan("dept", "d"),
            JoinType::Inner,
            Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
        );
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(joined),
            projections: vec![
                alias_expr(qcol("e", "dept_id"), "a"),
                alias_expr(qcol("d", "dept_id"), "b"),
            ],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(qcol("d", "dept_id"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("d.dept_id must resolve against the join's own input");
        let TypedOp::Sort { order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(
                    c.name, "b",
                    "ORDER BY d.dept_id must bind to output column `b` (d.dept_id), \
                     never `a` (e.dept_id) — a same-identity, not same-name, match"
                );
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn sort_promotes_missing_grouping_key_and_trims_q098_shape() {
        let bt = base_types_with_emp_dept();
        let agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id"), unresolved_col("name")],
            vec![
                unresolved_col("name"),
                func("sum", vec![unresolved_col("salary")]),
            ],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(unresolved_col("dept_id"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("missing grouping key must be promoted, not rejected");

        let TypedOp::Project {
            input: sort_ast,
            projections,
        } = &typed.op
        else {
            panic!(
                "expected a trim Project wrapping the Sort, got {:?}",
                typed.op
            );
        };
        assert_eq!(
            typed.resolved_schema.field_names(),
            vec!["name", "sum(salary)"]
        );
        assert_eq!(projections.len(), 2);
        match &projections[0] {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "name");
                assert_eq!(c.expr_id, Some(sort_ast.resolved_schema.fields[0].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &projections[1] {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "sum(salary)");
                assert_eq!(c.expr_id, Some(sort_ast.resolved_schema.fields[1].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }

        let TypedOp::Sort {
            input: agg_ast,
            order,
            ..
        } = &sort_ast.op
        else {
            panic!(
                "expected Sort under the trim Project, got {:?}",
                sort_ast.op
            );
        };
        assert_eq!(sort_ast.resolved_schema.len(), 3);
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "dept_id");
                assert_eq!(c.expr_id, Some(sort_ast.resolved_schema.fields[2].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &agg_ast.op {
            TypedOp::Aggregate { aggregates, .. } => {
                assert_eq!(aggregates.len(), 3);
                match &aggregates[2] {
                    Expression::Alias(a) => {
                        assert_eq!(a.alias, "dept_id");
                        assert!(matches!(
                            a.expr.as_ref(),
                            Expression::ColumnReference(c) if c.name == "dept_id"
                        ));
                    }
                    other => panic!("expected a new hidden dept_id entry, got {other:?}"),
                }
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_grouped_aggregate_whole_matches_folded_grouping_expression() {
        let bt = base_types_with_emp_dept();
        let senior = || {
            Expression::Binary(BinaryExpression {
                op: BinaryOp::GtEq,
                left: Box::new(unresolved_col("dept_id")),
                right: Box::new(int_lit(40)),
            })
        };
        let avg_salary = func("avg", vec![unresolved_col("salary")]);
        let agg = CommonAst::new(grouped_aggregate(
            emp_scan(),
            vec![senior()],
            vec![avg_salary],
            crate::transpiler_v2::ast::GroupingKind::GroupBy,
        ));
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(senior())],
            limit: None,
            offset: None,
        });
        let typed =
            analyze(ast, &bt).expect("grouping expression restatement must whole-match directly");

        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!(
                "expected Sort with no trim Project wrapper, got {:?}",
                typed.op
            );
        };
        assert!(
            matches!(input.op, TypedOp::Aggregate { .. }),
            "Sort's child must still be the Aggregate directly, got {:?}",
            input.op
        );
        assert_eq!(
            input.resolved_schema.len(),
            2,
            "no hidden output appended — extended schema len == original schema len"
        );
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert!(c.qualifier.is_none());
                assert_eq!(
                    c.expr_id,
                    Some(input.resolved_schema.fields[0].expr_id),
                    "the folded grouping expression is aggregates[0] by construction"
                );
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn sort_promotes_project_hidden_column_and_trims_q078_shape() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![unresolved_col("id"), unresolved_col("name")],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(unresolved_col("salary"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("hidden input column must be promoted, not rejected");

        let TypedOp::Project {
            input: sort_ast,
            projections,
        } = &typed.op
        else {
            panic!(
                "expected a trim Project wrapping the Sort, got {:?}",
                typed.op
            );
        };
        assert_eq!(typed.resolved_schema.field_names(), vec!["id", "name"]);
        assert_eq!(projections.len(), 2);

        let TypedOp::Sort {
            input: proj_ast,
            order,
            ..
        } = &sort_ast.op
        else {
            panic!(
                "expected Sort under the trim Project, got {:?}",
                sort_ast.op
            );
        };
        assert_eq!(sort_ast.resolved_schema.len(), 3);
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "salary");
                assert_eq!(c.expr_id, Some(sort_ast.resolved_schema.fields[2].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &proj_ast.op {
            TypedOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 3);
                match &projections[2] {
                    Expression::ColumnReference(c) => assert_eq!(c.name, "salary"),
                    other => panic!("expected a bare hidden salary push-down, got {other:?}"),
                }
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_aggregate_non_grouping_non_aggregate_leftover_still_errors_unknown_column() {
        let bt = base_types_with_emp_dept();
        let agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id")],
            vec![unresolved_col("dept_id")],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(unresolved_col("salary"))],
            limit: None,
            offset: None,
        });
        let err = analyze(ast, &bt)
            .expect_err("a leftover column neither grouped nor aggregated must still error");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "salary");
                assert!(qualifier.is_none());
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_deduplicate_wrapping_aggregate_does_not_reach_through_to_grouping_key() {
        let bt = base_types_with_emp_dept();
        let agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id")],
            vec![unresolved_col("dept_id")],
        );
        let dedup = CommonAst::new(CommonOp::Deduplicate {
            input: Box::new(agg),
            on_columns: vec![],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(dedup),
            order: vec![asc_key(unresolved_col("salary"))],
            limit: None,
            offset: None,
        });
        let err =
            analyze(ast, &bt).expect_err("Deduplicate must block the increment-2 fallback too");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "salary");
                assert!(qualifier.is_none());
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn resolve_and_stamp_div_widen_materialization_is_idempotent() {
        let bt = base_types_for(&[(
            "t",
            StructType::new(vec![
                StructField::nullable(
                    "d",
                    DataType::Decimal {
                        precision: 15,
                        scale: 2,
                    },
                ),
                StructField::nullable("i", DataType::Integer),
            ]),
        )]);
        let scanned = analyze(scan("t"), &bt).expect("scan analyzes");
        let ctx = ResolveContext::of_input(&scanned, &bt, None);
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(unresolved_col("d")),
            right: Box::new(unresolved_col("i")),
        });
        let once = resolve_and_stamp(expr, &ctx).expect("first resolve");
        let Expression::Binary(b) = &once else {
            panic!("expected Binary, got {once:?}");
        };
        assert!(
            matches!(b.right.as_ref(), Expression::Cast(c) if c.implicit),
            "the integral side must be widened via an implicit Cast: {b:?}"
        );
        let twice = resolve_and_stamp(once.clone(), &ctx).expect("second resolve");
        assert_eq!(
            once, twice,
            "re-resolving an already-materialized Div-widen tree must be a fixpoint"
        );
    }

    #[test]
    fn resolve_and_stamp_date_interval_materialization_is_idempotent() {
        let bt = base_types_for(&[(
            "t",
            StructType::new(vec![StructField::nullable("d", DataType::Date)]),
        )]);
        let scanned = analyze(scan("t"), &bt).expect("scan analyzes");
        let ctx = ResolveContext::of_input(&scanned, &bt, None);
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(unresolved_col("d")),
            right: Box::new(Expression::Interval(IntervalExpression {
                months: 0,
                days: 1,
                microseconds: 0,
                kind: IntervalKind::Calendar,
            })),
        });
        let once = resolve_and_stamp(expr, &ctx).expect("first resolve");
        assert!(
            matches!(&once, Expression::Cast(c) if c.implicit && c.to_type == DataType::Date),
            "the whole Date + Interval node must be wrapped in an implicit Cast to Date: {once:?}"
        );
        let twice = resolve_and_stamp(once.clone(), &ctx).expect("second resolve");
        assert_eq!(
            once, twice,
            "re-resolving an already-materialized Date-preserving cast must be a fixpoint, \
             not stack another wrapper"
        );
    }

    #[test]
    fn pretty_name_transparent_over_materialized_div_widen() {
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable(
                "sum_x",
                DataType::Decimal {
                    precision: 15,
                    scale: 2,
                },
            ),
            StructField::nullable("w_sq_ft", DataType::Integer),
        ]));
        let raw = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(UnresolvedColumn::bare("sum_x")),
            right: Box::new(UnresolvedColumn::bare("w_sq_ft")),
        });
        let pre_name = expression_output_name(&raw);
        let materialized = materialize_binary_coercions(raw.clone(), &schema);
        assert_ne!(
            materialized, raw,
            "sanity: N4 must actually change the tree in this shape"
        );
        assert_eq!(
            expression_output_name(&materialized),
            pre_name,
            "an implicit N4 Cast must not perturb output naming"
        );
    }

    #[test]
    fn pretty_name_transparent_over_materialized_date_plus_interval() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            DataType::Date,
        )]));
        let raw = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(UnresolvedColumn::bare("d")),
            right: Box::new(Expression::Interval(IntervalExpression {
                months: 0,
                days: 1,
                microseconds: 0,
                kind: IntervalKind::Calendar,
            })),
        });
        let pre_name = expression_output_name(&raw);
        let materialized = materialize_binary_coercions(raw.clone(), &schema);
        assert!(matches!(&materialized, Expression::Cast(c) if c.implicit));
        assert_eq!(
            expression_output_name(&materialized),
            pre_name,
            "an implicit N4 Cast must not perturb output naming"
        );
    }

    #[test]
    fn semantic_eq_strips_implicit_cast_on_div_widen_shape() {
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable(
                "d",
                DataType::Decimal {
                    precision: 15,
                    scale: 2,
                },
            ),
            StructField::nullable("i", DataType::Integer),
        ]));
        let raw = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(UnresolvedColumn::bare("d")),
            right: Box::new(UnresolvedColumn::bare("i")),
        });
        let materialized = materialize_binary_coercions(raw.clone(), &schema);
        assert_ne!(
            materialized, raw,
            "sanity: N4 must actually change the tree in this shape"
        );
        assert!(
            semantic_eq(&materialized, &raw),
            "an implicit N4 Cast must be invisible to semantic_eq, exactly like an Alias"
        );
    }

    #[test]
    fn semantic_eq_does_not_strip_user_written_cast() {
        let bare = UnresolvedColumn::bare("i");
        let user_cast = Expression::Cast(CastExpression {
            expr: Box::new(bare.clone()),
            to_type: DataType::Decimal {
                precision: 20,
                scale: 0,
            },
            try_cast: false,
            implicit: false,
        });
        assert!(
            !semantic_eq(&user_cast, &bare),
            "a user-written CAST is a semantic operation and must stay distinct"
        );
    }

    #[test]
    fn semantic_eq_strips_implicit_cast_on_date_plus_interval_shape() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            DataType::Date,
        )]));
        let raw = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(UnresolvedColumn::bare("d")),
            right: Box::new(Expression::Interval(IntervalExpression {
                months: 0,
                days: 1,
                microseconds: 0,
                kind: IntervalKind::Calendar,
            })),
        });
        let materialized = materialize_binary_coercions(raw.clone(), &schema);
        assert_ne!(
            materialized, raw,
            "sanity: N4 must actually change the tree in this shape"
        );
        assert!(
            semantic_eq(&materialized, &raw),
            "an implicit N4 Cast must be invisible to semantic_eq, exactly like an Alias"
        );
    }

    #[test]
    fn project_computed_entry_wrapped_named_schema_unchanged() {
        let bt = base_types_with_emp_dept();
        let computed = || {
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(unresolved_col("dept_id")),
                right: Box::new(int_lit(1)),
            })
        };
        let scanned = analyze(scan("emp"), &bt).expect("scan analyzes");
        let ctx = ResolveContext::of_input(&scanned, &bt, None);
        let baseline_resolved = resolve_and_stamp(computed(), &ctx).expect("baseline resolves");
        let baseline_field = output_attribute(&baseline_resolved, &scanned.resolved_schema);

        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("id"), computed()],
        });
        let typed = analyze(ast, &bt).expect("project analyzes");
        let TypedOp::Project { projections, .. } = &typed.op else {
            panic!("expected Project, got {:?}", typed.op);
        };
        match &projections[1] {
            Expression::Alias(a) => assert_eq!(a.alias, "(dept_id + 1)"),
            other => panic!("expected N8-wrapped computed entry, got {other:?}"),
        }
        assert_eq!(
            typed.resolved_schema.fields[1], baseline_field,
            "N8's wrap must not perturb the computed schema field"
        );
    }

    #[test]
    fn aggregate_folded_grouping_clone_wrapped_grouping_copy_stays_bare() {
        let bt = base_types_with_emp_dept();
        let bump = || {
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(unresolved_col("dept_id")),
                right: Box::new(int_lit(1)),
            })
        };
        let avg_salary = func("avg", vec![unresolved_col("salary")]);
        let ast = CommonAst::new(grouped_aggregate(
            emp_scan(),
            vec![bump()],
            vec![avg_salary],
            crate::transpiler_v2::ast::GroupingKind::GroupBy,
        ));
        let typed = analyze(ast, &bt).expect("folded grouping aggregate analyzes");
        let TypedOp::Aggregate {
            grouping,
            aggregates,
            ..
        } = &typed.op
        else {
            panic!("expected Aggregate, got {:?}", typed.op);
        };
        match &grouping[0] {
            Expression::Binary(_) => {}
            other => panic!("grouping copy must stay bare (unwrapped), got {other:?}"),
        }
        match &aggregates[0] {
            Expression::Alias(a) => {
                assert_eq!(a.alias, "(dept_id + 1)");
                assert!(matches!(a.expr.as_ref(), Expression::Binary(_)));
            }
            other => {
                panic!("expected the N8-wrapped folded grouping entry in aggregates, got {other:?}")
            }
        }
    }

    #[test]
    fn ensure_named_idempotent_over_alias_bare_ref_and_star() {
        let aliased = Expression::Alias(AliasExpression {
            expr: Box::new(unresolved_col("id")),
            alias: "renamed".to_owned(),
        });
        assert_eq!(ensure_named(aliased.clone()), aliased);

        let bare_qualified = Expression::ColumnReference(ColumnReference {
            name: "dept_id".to_owned(),
            qualifier: Some("e".to_owned()),
            data_type: DataType::Integer,
            nullable: true,
            expr_id: None,
        });
        assert_eq!(ensure_named(bare_qualified.clone()), bare_qualified);

        let star = Expression::Star(StarExpression { qualifier: None });
        assert_eq!(ensure_named(star.clone()), star);
    }

    #[test]
    fn ensure_named_implicit_cast_aliased_with_inner_pretty_name() {
        let inner = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(unresolved_col("age")),
            right: Box::new(int_lit(1)),
        });
        let implicit_cast = Expression::Cast(CastExpression {
            expr: Box::new(inner.clone()),
            to_type: DataType::Long,
            try_cast: false,
            implicit: true,
        });
        let wrapped = ensure_named(implicit_cast.clone());
        match wrapped {
            Expression::Alias(a) => {
                assert_eq!(
                    a.alias,
                    pretty_name(&inner),
                    "must use the inner expression's pretty name, not the Cast's"
                );
                assert_eq!(a.alias, "(age + 1)");
                assert_eq!(
                    *a.expr, implicit_cast,
                    "the implicit Cast itself must remain intact under the wrap"
                );
            }
            other => panic!("expected an Alias wrap, got {other:?}"),
        }
    }

    #[test]
    fn sort_rebind_binds_wrapped_entry_and_leaves_bare_entry_unmutated() {
        let bt = base_types_with_emp_dept();
        let avg_salary = || func("avg", vec![unresolved_col("salary")]);
        let agg = aggregate(
            aliased_scan("emp", "e"),
            vec![unresolved_col("dept_id")],
            vec![unresolved_col("dept_id"), avg_salary()],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(qcol("e", "dept_id")), asc_key(avg_salary())],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("both keys must resolve via the fallback");

        let TypedOp::Sort { input, order, .. } = &typed.op else {
            panic!("expected Sort, got {:?}", typed.op);
        };
        match order[0].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "dept_id");
                assert_eq!(c.expr_id, Some(input.resolved_schema.fields[0].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match order[1].expr.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "avg(salary)");
                assert_eq!(c.expr_id, Some(input.resolved_schema.fields[1].expr_id));
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        match &input.op {
            TypedOp::Aggregate { aggregates, .. } => {
                match &aggregates[0] {
                    Expression::ColumnReference(c) => assert_eq!(c.name, "dept_id"),
                    other => panic!(
                        "N8: bind_slot is read-only — the bare restated grouping key \
                         must NOT be mutated into an Alias, got {other:?}"
                    ),
                }
                match &aggregates[1] {
                    Expression::Alias(a) => assert_eq!(a.alias, "avg(salary)"),
                    other => panic!("expected the N8-wrapped avg(salary) entry, got {other:?}"),
                }
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    fn assert_n8_output_list_invariant(ti: &TypedAst) {
        fn assert_entries(entries: &[Expression], schema: &ResolvedSchema, ctx: &str) {
            let has_star = entries.iter().any(|e| matches!(e, Expression::Star(_)));
            for e in entries {
                match e {
                    Expression::ColumnReference(_) | Expression::Star(_) | Expression::Alias(_) => {
                    }
                    other => {
                        panic!("{ctx}: entry is neither a bare ref, Star, nor Alias: {other:?}")
                    }
                }
            }
            if has_star {
                return;
            }
            assert_eq!(
                entries.len(),
                schema.len(),
                "{ctx}: entries/schema length mismatch"
            );
            for (entry, field) in entries.iter().zip(schema.fields.iter()) {
                if let Expression::Alias(a) = entry {
                    assert_eq!(
                        a.alias, field.name,
                        "{ctx}: entry's alias must equal its schema field name"
                    );
                }
            }
        }
        match &ti.op {
            TypedOp::Project {
                input, projections, ..
            } => {
                assert_entries(projections, &ti.resolved_schema, "Project");
                assert_n8_output_list_invariant(input);
            }
            TypedOp::Aggregate {
                input, aggregates, ..
            } => {
                assert_entries(aggregates, &ti.resolved_schema, "Aggregate");
                assert_n8_output_list_invariant(input);
            }
            TypedOp::Pivot {
                input, grouping, ..
            } => {
                let prefix =
                    ResolvedSchema::new(ti.resolved_schema.fields[..grouping.len()].to_vec());
                assert_entries(grouping, &prefix, "Pivot grouping");
                assert_n8_output_list_invariant(input);
            }
            TypedOp::Sort { input, .. } => assert_n8_output_list_invariant(input),
            _ => {}
        }
    }

    #[test]
    fn n8_invariant_holds_over_representative_plans() {
        let bt = base_types_with_emp_dept();

        let raised = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Mul,
                left: Box::new(unresolved_col("salary")),
                right: Box::new(lit_double(1.1)),
            }),
            "raised",
        );
        let delta = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Sub,
                left: Box::new(unresolved_col("raised")),
                right: Box::new(unresolved_col("salary")),
            }),
            "delta",
        );
        let lca_ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![raised, delta],
        });
        assert_n8_output_list_invariant(&analyze(lca_ast, &bt).expect("LCA analyzes"));

        let inline_ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![
                unresolved_col("id"),
                generator_expr(
                    "inline",
                    vec![func(
                        "array",
                        vec![func(
                            "struct",
                            vec![unresolved_col("name"), unresolved_col("salary")],
                        )],
                    )],
                    &[],
                ),
            ],
        });
        assert_n8_output_list_invariant(&analyze(inline_ast, &bt).expect("inline analyzes"));

        let json_bt = base_types_for(&[(
            "raw",
            StructType::new(vec![
                StructField::not_null("id", DataType::Long),
                StructField::nullable("json_str", DataType::String),
            ]),
        )]);
        let json_ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("raw")),
            projections: vec![
                unresolved_col("id"),
                generator_expr(
                    "json_tuple",
                    vec![unresolved_col("json_str"), lit_str("a"), lit_str("e")],
                    &[],
                ),
            ],
        });
        assert_n8_output_list_invariant(&analyze(json_ast, &json_bt).expect("json_tuple analyzes"));

        let stack = generator_expr("stack", vec![int_lit(2), int_lit(10), int_lit(20)], &["x"]);
        let stack_ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![stack],
        });
        assert_n8_output_list_invariant(&analyze(stack_ast, &bt).expect("stack analyzes"));

        let promote_agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id"), unresolved_col("name")],
            vec![
                unresolved_col("name"),
                func("sum", vec![unresolved_col("salary")]),
            ],
        );
        let promote_ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(promote_agg),
            order: vec![asc_key(unresolved_col("dept_id"))],
            limit: None,
            offset: None,
        });
        assert_n8_output_list_invariant(
            &analyze(promote_ast, &bt).expect("hidden grouping key promotes"),
        );

        let pivot_ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan()),
            grouping: PivotGrouping::Explicit(vec![Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(unresolved_col("dept_id")),
                right: Box::new(int_lit(1)),
            })]),
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10)],
            aggregates: vec![func("count", vec![int_lit(1)])],
        });
        assert_n8_output_list_invariant(
            &analyze(pivot_ast, &bt).expect("pivot with explicit computed grouping analyzes"),
        );
    }

    #[test]
    fn passthrough_filter_over_scan_preserves_input_attribute_ids() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(emp_scan()),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(unresolved_col("salary")),
                right: Box::new(lit_double(1000.0)),
            }),
        });
        let typed = analyze(ast, &bt).expect("filter over scan analyzes");
        let TypedOp::Filter { input, .. } = &typed.op else {
            panic!("expected Filter, got {:?}", typed.op);
        };
        assert_eq!(typed.resolved_schema.len(), input.resolved_schema.len());
        for (outer, inner) in typed
            .resolved_schema
            .fields
            .iter()
            .zip(input.resolved_schema.fields.iter())
        {
            assert_eq!(
                outer.expr_id, inner.expr_id,
                "Filter is a pure passthrough — attribute ids must ride through unchanged"
            );
        }
    }

    #[test]
    fn project_bare_ref_copies_id_alias_computed_entry_mints_fresh_id() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![
                unresolved_col("id"), // bare ref -> COPY
                alias_expr(func("upper", vec![unresolved_col("name")]), "name_upper"), // computed -> MINT
            ],
        });
        let typed = analyze(ast, &bt).expect("project over scan analyzes");
        let TypedOp::Project { input, .. } = &typed.op else {
            panic!("expected Project, got {:?}", typed.op);
        };
        let input_id_attr = input
            .resolved_schema
            .field_by_name("id")
            .expect("emp schema has an id column");
        assert_eq!(
            typed.resolved_schema.fields[0].expr_id, input_id_attr.expr_id,
            "bare column reference must COPY the source attribute's id"
        );
        let all_input_ids: Vec<_> = input
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.expr_id)
            .collect();
        assert!(
            !all_input_ids.contains(&typed.resolved_schema.fields[1].expr_id),
            "Alias/computed projection entry must MINT a fresh id, not reuse any input id"
        );
    }

    #[test]
    fn inner_join_output_schema_concatenates_both_sides_ids_in_order() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("e", "dept_id")),
            right: Box::new(qcol("d", "dept_id")),
        });
        let ast = join(
            aliased_scan("emp", "e"),
            aliased_scan("dept", "d"),
            JoinType::Inner,
            Some(cond),
        );
        let typed = analyze(ast, &bt).expect("inner join analyzes");
        let TypedOp::Join { left, right, .. } = &typed.op else {
            panic!("expected Join, got {:?}", typed.op);
        };
        let expected_ids: Vec<_> = left
            .resolved_schema
            .fields
            .iter()
            .chain(right.resolved_schema.fields.iter())
            .map(|f| f.expr_id)
            .collect();
        let actual_ids: Vec<_> = typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.expr_id)
            .collect();
        assert_eq!(
            actual_ids, expected_ids,
            "plain join output is left-then-right concatenation, ids included"
        );
    }

    #[test]
    fn sort_hidden_promotion_q078_shape_preserves_ids_through_restamp_and_trim_project() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![unresolved_col("id"), unresolved_col("name")],
        });
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(unresolved_col("salary"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("hidden input column must be promoted, not rejected");

        let TypedOp::Project {
            input: sort_ast, ..
        } = &typed.op
        else {
            panic!(
                "expected a trim Project wrapping the Sort, got {:?}",
                typed.op
            );
        };
        let TypedOp::Sort {
            input: proj_ast, ..
        } = &sort_ast.op
        else {
            panic!(
                "expected Sort under the trim Project, got {:?}",
                sort_ast.op
            );
        };
        let TypedOp::Project {
            input: scan_ast, ..
        } = &proj_ast.op
        else {
            panic!("expected Project under the Sort, got {:?}", proj_ast.op);
        };
        let scan_id = scan_ast
            .resolved_schema
            .field_by_name("id")
            .expect("emp scan has id")
            .expr_id;
        let scan_name = scan_ast
            .resolved_schema
            .field_by_name("name")
            .expect("emp scan has name")
            .expr_id;

        assert_eq!(proj_ast.resolved_schema.fields[0].expr_id, scan_id);
        assert_eq!(proj_ast.resolved_schema.fields[1].expr_id, scan_name);

        assert_eq!(sort_ast.resolved_schema.fields[0].expr_id, scan_id);
        assert_eq!(sort_ast.resolved_schema.fields[1].expr_id, scan_name);

        assert_eq!(typed.resolved_schema.fields[0].expr_id, scan_id);
        assert_eq!(typed.resolved_schema.fields[1].expr_id, scan_name);

        let emp: BTreeSet<String> = ["emp".to_owned()].into_iter().collect();
        assert_eq!(
            quals_of(&sort_ast.resolved_schema),
            vec![emp.clone(), emp.clone(), emp.clone()],
            "Sort's own extended schema must carry emp/emp/emp through the (deleted) re-stamp"
        );
        assert_eq!(
            quals_of(&typed.resolved_schema),
            vec![emp.clone(), emp],
            "trim Project's COPY branch must carry the scan's qualifier through unchanged"
        );
    }

    #[test]
    fn output_attribute_copy_branch_inserts_reference_qualifier() {
        let bt = base_types_with_emp_dept();
        let scanned = analyze(scan("emp"), &bt).expect("scan analyzes");
        let field = scanned
            .resolved_schema
            .field_by_name("dept_id")
            .expect("emp has dept_id");
        let cr = Expression::ColumnReference(ColumnReference {
            name: "dept_id".to_owned(),
            qualifier: Some("e".to_owned()),
            data_type: field.data_type.clone(),
            nullable: field.nullable,
            expr_id: Some(field.expr_id),
        });
        let attr = output_attribute(&cr, &scanned.resolved_schema);
        let expected: BTreeSet<String> = ["emp".to_owned(), "e".to_owned()].into_iter().collect();
        assert_eq!(attr.source_quals, expected);
        assert_eq!(
            attr.expr_id, field.expr_id,
            "COPY branch must carry the id forward too"
        );
    }

    #[test]
    fn output_attribute_mint_branch_has_empty_quals() {
        let bt = base_types_with_emp_dept();
        let scanned = analyze(scan("emp"), &bt).expect("scan analyzes");
        let computed = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(unresolved_col("dept_id")),
            right: Box::new(int_lit(1)),
        });
        let ctx = ResolveContext::of_input(&scanned, &bt, None);
        let resolved = resolve_and_stamp(computed, &ctx).expect("computed expr resolves");
        let attr = output_attribute(&resolved, &scanned.resolved_schema);
        assert!(attr.source_quals.is_empty());
    }

    #[test]
    fn source_quals_using_join_right_only_donor_still_unions_both_sides_quals() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Right,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let typed = analyze(ast, &bt).unwrap();
        let e: BTreeSet<String> = ["e".to_owned()].into_iter().collect();
        let d: BTreeSet<String> = ["d".to_owned()].into_iter().collect();
        let key: BTreeSet<String> = e.union(&d).cloned().collect();
        let field = typed
            .resolved_schema
            .field_by_name("dept_id")
            .expect("USING key must be in the output schema");
        assert_eq!(
            field.source_quals, key,
            "RIGHT-donor USING key must still union BOTH sides' qualifiers"
        );
    }

    #[test]
    fn sort_promotes_missing_grouping_key_mints_empty_lineage_not_inherited() {
        let bt = base_types_with_emp_dept();
        let agg = aggregate(
            aliased_scan("emp", "e"),
            vec![unresolved_col("dept_id"), unresolved_col("name")],
            vec![
                unresolved_col("name"),
                func("sum", vec![unresolved_col("salary")]),
            ],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(unresolved_col("dept_id"))],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("missing grouping key must be promoted, not rejected");
        let TypedOp::Project {
            input: sort_ast, ..
        } = &typed.op
        else {
            panic!(
                "expected a trim Project wrapping the Sort, got {:?}",
                typed.op
            );
        };
        let TypedOp::Sort { input: agg_ast, .. } = &sort_ast.op else {
            panic!(
                "expected Sort under the trim Project, got {:?}",
                sort_ast.op
            );
        };
        let TypedOp::Aggregate { .. } = &agg_ast.op else {
            panic!("expected Aggregate under the Sort, got {:?}", agg_ast.op);
        };
        assert_eq!(agg_ast.resolved_schema.len(), 3);
        assert_eq!(agg_ast.resolved_schema.fields[2].name, "dept_id");
        assert!(
            agg_ast.resolved_schema.fields[2].source_quals.is_empty(),
            "promotion mints a fresh Attribute — current behavior does NOT inherit \
             the grouping key's source lineage, even though it is a bare passthrough"
        );
    }

    #[test]
    fn push_setop_casts_preserves_childs_own_id_not_the_widened_donor_id() {
        let input = TypedAst::new(
            TypedOp::TableScan {
                table: "dept".to_owned(),
            },
            ResolvedSchema::new(vec![Attribute::minted("dept_id", DataType::Integer, false)]),
        );
        let child_attr = Attribute::minted("id", DataType::Integer, false);
        let child_id = child_attr.expr_id;
        let mut child = TypedAst::new(
            TypedOp::Project {
                input: Box::new(input),
                projections: vec![Expression::ColumnReference(ColumnReference {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    data_type: DataType::Integer,
                    nullable: false,
                    expr_id: None,
                })],
            },
            ResolvedSchema::new(vec![child_attr]),
        );

        let widened_attr = Attribute::minted("id", DataType::Long, false);
        let widened_id = widened_attr.expr_id;
        let widened_schema = ResolvedSchema::new(vec![widened_attr]);

        push_setop_casts(&mut child, &widened_schema);

        assert_eq!(child.resolved_schema.fields[0].data_type, DataType::Long);
        assert_eq!(child.resolved_schema.fields[0].expr_id, child_id);
        assert_ne!(child.resolved_schema.fields[0].expr_id, widened_id);
    }

    #[test]
    fn widen_by_position_output_schema_carries_child_zero_ids() {
        let left = TypedAst::new(
            TypedOp::TableScan {
                table: "emp".to_owned(),
            },
            ResolvedSchema::new(vec![Attribute::minted("id", DataType::Long, false)]),
        );
        let left_id = left.resolved_schema.fields[0].expr_id;
        let right = TypedAst::new(
            TypedOp::TableScan {
                table: "dept".to_owned(),
            },
            ResolvedSchema::new(vec![Attribute::minted("id", DataType::Integer, false)]),
        );
        let widened = widen_by_position(SetOpKind::Union, &[left, right])
            .expect("single-column arity matches");
        assert_eq!(
            widened.fields[0].expr_id, left_id,
            "the widened schema's column identity donor is child 0"
        );
    }

    #[test]
    fn semantic_eq_rejects_same_name_different_join_side_ids() {
        let id_a = ExprId::fresh();
        let id_b = ExprId::fresh();
        assert_ne!(id_a, id_b);
        let a = Expression::ColumnReference(ColumnReference {
            name: "x".to_owned(),
            qualifier: Some("t1".to_owned()),
            data_type: DataType::Integer,
            nullable: true,
            expr_id: Some(id_a),
        });
        let b = Expression::ColumnReference(ColumnReference {
            name: "x".to_owned(),
            qualifier: Some("t2".to_owned()),
            data_type: DataType::Integer,
            nullable: true,
            expr_id: Some(id_b),
        });
        assert!(
            !semantic_eq(&a, &b),
            "same-named columns from different join sides must not semantic_eq-collide \
             just because they canonicalize to the same qualifier-stripped shape"
        );
    }

    #[test]
    fn rebind_over_aggregate_binds_correct_duplicate_name_slot_by_id() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("e", "dept_id")),
            right: Box::new(qcol("d", "dept_id")),
        });
        let joined_ast = join(
            aliased_scan("emp", "e"),
            aliased_scan("emp", "d"),
            JoinType::Inner,
            Some(cond),
        );
        let child_input = analyze(joined_ast, &bt).expect("self-join analyzes");
        let e_field = child_input.resolved_schema.fields[2].clone();
        let d_field = child_input.resolved_schema.fields[6].clone();
        assert_eq!(e_field.name, "dept_id");
        assert_eq!(d_field.name, "dept_id");
        assert_ne!(e_field.expr_id, d_field.expr_id);

        let mut aggregates = vec![
            Expression::ColumnReference(ColumnReference {
                name: e_field.name.clone(),
                qualifier: None,
                data_type: e_field.data_type.clone(),
                nullable: e_field.nullable,
                expr_id: Some(e_field.expr_id),
            }),
            Expression::ColumnReference(ColumnReference {
                name: d_field.name.clone(),
                qualifier: None,
                data_type: d_field.data_type.clone(),
                nullable: d_field.nullable,
                expr_id: Some(d_field.expr_id),
            }),
        ];
        let grouping = aggregates.clone();
        let mut schema = ResolvedSchema::new(vec![e_field.clone(), d_field.clone()]);

        let bound = rebind_over_child(
            qcol("d", "dept_id"),
            &child_input,
            &mut aggregates,
            SortChild::Aggregate {
                grouping: &grouping,
            },
            &mut schema,
            &bt,
            None,
        )
        .expect("d.dept_id must rebind onto the d-side aggregates entry");
        match bound {
            Expression::ColumnReference(c) => {
                assert_eq!(
                    c.expr_id,
                    Some(d_field.expr_id),
                    "must bind aggregates[1] (the d-side entry), not aggregates[0] \
                     which merely shares the name"
                );
            }
            other => panic!("expected bare ColumnReference, got {other:?}"),
        }
        assert_eq!(aggregates.len(), 2);
    }

    #[test]
    fn self_join_left_right_resolved_schema_ids_are_disjoint() {
        let bt = base_types_with_emp_dept();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("e", "dept_id")),
            right: Box::new(qcol("m", "dept_id")),
        });
        let joined_ast = join(
            aliased_scan("emp", "e"),
            aliased_scan("emp", "m"),
            JoinType::Left,
            Some(cond),
        );
        let typed = analyze(joined_ast, &bt).expect("self-join analyzes");
        let TypedOp::Join { left, right, .. } = &typed.op else {
            panic!("expected TypedOp::Join, got {:?}", typed.op);
        };
        let left_ids: HashSet<_> = left
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.expr_id)
            .collect();
        let right_ids: HashSet<_> = right
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.expr_id)
            .collect();
        assert!(
            left_ids.is_disjoint(&right_ids),
            "self-join left/right sides must never share an expr_id, even when \
             scanning the same underlying table through distinct aliases — N10-lite \
             stage 2's id-keyed join-condition binding depends on this"
        );
    }

    #[test]
    fn second_order_by_key_binds_by_id_to_already_promoted_entry_no_duplicate_append() {
        let bt = base_types_with_emp_dept();
        let avg_salary = || func("avg", vec![unresolved_col("salary")]);
        let agg = aggregate(
            emp_scan(),
            vec![unresolved_col("dept_id")],
            vec![unresolved_col("dept_id")],
        );
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(agg),
            order: vec![asc_key(avg_salary()), asc_key(avg_salary())],
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("both avg(salary) keys must resolve");

        let TypedOp::Project {
            input: sort_ast, ..
        } = &typed.op
        else {
            panic!(
                "expected a trim Project wrapping the Sort, got {:?}",
                typed.op
            );
        };
        let TypedOp::Sort {
            input: agg_ast,
            order,
            ..
        } = &sort_ast.op
        else {
            panic!(
                "expected Sort under the trim Project, got {:?}",
                sort_ast.op
            );
        };
        let TypedOp::Aggregate { aggregates, .. } = &agg_ast.op else {
            panic!("expected Aggregate under the Sort, got {:?}", agg_ast.op);
        };
        assert_eq!(
            aggregates.len(),
            2,
            "the second identical key must not append a duplicate hidden column"
        );
        let promoted_id = agg_ast.resolved_schema.fields[1].expr_id;
        for (i, so) in order.iter().enumerate() {
            match so.expr.as_ref() {
                Expression::ColumnReference(c) => {
                    assert_eq!(
                        c.expr_id,
                        Some(promoted_id),
                        "order-by key {i} must bind the SAME promoted entry by id"
                    );
                }
                other => panic!("expected bare ColumnReference for key {i}, got {other:?}"),
            }
        }
    }

    #[test]
    fn tier_g_correlated_outer_ref_stamps_outer_expr_id() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(qcol("d", "budget")),
                    right: Box::new(qcol("e", "salary")),
                }),
            })),
            projections: vec![qcol("d", "dept_id")],
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(scan("emp")),
                alias: "e".to_owned(),
            })),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        let typed = analyze(ast, &bt).expect("correlated scalar subquery resolves");
        let TypedOp::Project {
            input: outer_input,
            projections,
        } = &typed.op
        else {
            panic!("expected Project, got {:?}", typed.op);
        };
        let outer_salary_id = outer_input
            .resolved_schema
            .field_by_name("salary")
            .expect("outer plan (alias `e` over `emp`) must resolve a `salary` attribute")
            .expr_id;
        let unaliased = match &projections[0] {
            Expression::Alias(a) => a.expr.as_ref(),
            other => other,
        };
        let Expression::ScalarSubquery(sub) = unaliased else {
            panic!("expected ScalarSubquery, got {unaliased:?}");
        };
        let SubqueryPlan::Analyzed(inner_typed) = &sub.subquery else {
            panic!("subquery must be analyzed");
        };
        let TypedOp::Project {
            input: filter_ast, ..
        } = &inner_typed.op
        else {
            panic!("expected Project, got {:?}", inner_typed.op);
        };
        let TypedOp::Filter { condition, .. } = &filter_ast.op else {
            panic!("expected Filter, got {:?}", filter_ast.op);
        };
        let Expression::Binary(cmp) = condition else {
            panic!("expected Binary, got {condition:?}");
        };
        match cmp.right.as_ref() {
            Expression::ColumnReference(c) => {
                assert_eq!(c.name, "salary");
                assert_eq!(c.qualifier.as_deref(), Some("e"));
                assert_eq!(
                    c.expr_id,
                    Some(outer_salary_id),
                    "tier-(g) must stamp the matched OUTER attribute's expr_id"
                );
            }
            other => panic!("expected outer ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn canonicalize_for_semantic_eq_preserves_expr_id() {
        let id = ExprId::fresh();
        let c = Expression::ColumnReference(ColumnReference {
            name: "X".to_owned(),
            qualifier: Some("t".to_owned()),
            data_type: DataType::Integer,
            nullable: true,
            expr_id: Some(id),
        });
        match canonicalize_for_semantic_eq(&c) {
            Expression::ColumnReference(cc) => {
                assert_eq!(
                    cc.expr_id,
                    Some(id),
                    "canonicalize_for_semantic_eq must preserve expr_id"
                );
                assert_eq!(cc.name, "x", "name must still be case-folded");
                assert_eq!(cc.qualifier, None, "qualifier must still be stripped");
            }
            other => panic!("expected ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn unicode_column_select_and_drop_agree_via_name_fold() {
        let bt = base_types_for(&[(
            "t",
            StructType::new(vec![StructField::nullable("É", DataType::String)]),
        )]);

        let select_ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("t")),
            projections: vec![unresolved_col("é")],
        });
        let typed = analyze(select_ast, &bt).expect("select(\"é\") should resolve `É`");
        assert_eq!(typed.resolved_schema.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].name, "é");

        let drop_ast = CommonAst::new(CommonOp::DropColumns {
            input: Box::new(scan("t")),
            drop_names: vec!["é".to_owned()],
        });
        let typed = analyze(drop_ast, &bt).expect("drop(\"é\") should analyze");
        assert!(
            typed.resolved_schema.is_empty(),
            "drop(\"é\") must drop the SAME `É` attribute select(\"é\") resolves, got {:?}",
            typed.resolved_schema.field_names()
        );
    }
}
