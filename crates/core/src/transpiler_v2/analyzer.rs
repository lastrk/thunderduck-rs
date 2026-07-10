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
use super::error::{EmissionError, UnsupportedKind};
use super::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression,
    ColumnReference, Expression, ExtractValueExpression, FunctionCall, Literal, LiteralValue,
    SortOrder, SubqueryPlan, UnaryExpression, UnaryOp, UnresolvedColumn,
};
use super::type_inference::TypeInferenceEngine;
use crate::bail_boundary_rule;
use crate::types::{DataType, StructField, StructType};

// Re-export SetOpKind so downstream callers can use `analyzer::SetOpKind`.
pub use super::ast::SetOpKind;

/// The eight Spark defaults for `df.summary()` when no statistics list is
/// supplied — matches `Dataset.summary()` in Apache Spark 4.x.
pub(super) const DEFAULT_SUMMARY_STATS: &[&str] =
    &["count", "mean", "stddev", "min", "25%", "50%", "75%", "max"];

/// τ's schema type alias — points at the shared `StructType`.
pub type Schema = StructType;

// ── TypedAst / TypedOp ──────────────────────────────────────────────────────

/// A typed plan node: an operator plus its resolved output schema and the
/// alias scope that output exposes.
#[derive(Debug, Clone)]
pub struct TypedAst {
    /// The typed operator this node represents.
    pub op: TypedOp,
    /// The schema of the relation produced by this node — every field has
    /// a resolved (non-`Unresolved`) [`DataType`] and a known `nullable` flag.
    pub resolved_schema: StructType,
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
    pub fn new(op: TypedOp, resolved_schema: StructType) -> Self {
        let mut scope = RelScope::of(&op, &resolved_schema);
        scope.source_quals = source_quals_of(&op, &resolved_schema);
        Self {
            op,
            resolved_schema,
            scope,
        }
    }
}

/// The schema/scope-PASSTHROUGH operator class: position/count-preserving
/// unary operators through which alias bindings (bottom-up, [`RelScope::of`])
/// and synthetic-alias demands (top-down, [`mark_node`]) flow unchanged.
/// The single authority for that classification — both walks match on this
/// pattern, so adding a variant here updates them in lockstep, and both
/// matches are exhaustive so a NEW `TypedOp` variant is a compile error in
/// each until classified.
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
/// names, user aliases, lateral-view table aliases) bind to which contiguous
/// field ranges of the node's `resolved_schema`, plus the plan_id →
/// join-side bindings used for DataFrame `plan_id` disambiguation.
///
/// Ranges are relative to THIS node's schema (base 0); consumers offset when
/// composing (a join's right side shifts by the left side's field count).
#[derive(Debug, Clone, Default)]
pub struct RelScope {
    /// `(qualifier, field-range)` bindings, in tree order.
    pub aliases: Vec<(String, std::ops::Range<usize>)>,
    /// `(plan_id, field-range, TD_JOIN_LEFT | TD_JOIN_RIGHT)` bindings,
    /// OUTERMOST join first — [`RelScope::lookup_plan_id`] uses first
    /// match, so the nearest enclosing join's side qualifier wins. The
    /// third element is the synthetic join-side qualifier emission renders
    /// so DuckDB resolves the reference against the correct side of the
    /// enclosing join's `(left) AS __td_jl … (right) AS __td_jr` FROM.
    pub plan_ids: Vec<(i64, std::ops::Range<usize>, &'static str)>,
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
    /// ADR-023 tier 3: per-OUTPUT-column source-qualifier lineage (Spark attribute
    /// lineage). `source_quals[i]` = the set of relation qualifiers output column `i`
    /// inherits. A passthrough ColumnReference inherits its source column's set; an
    /// Alias/computed column inherits ∅. Populated by `source_quals_of` in
    /// `TypedAst::new`; derived, so EXCLUDED from PartialEq. Invariant:
    /// `source_quals.len() == resolved_schema.len()` for every node.
    pub source_quals: Vec<std::collections::BTreeSet<String>>,
}

/// Equality deliberately ignores `source_quals`: it is derived data, fully
/// determined by `(op, resolved_schema)` (via `source_quals_of`), and
/// existing scope-equality tests assert equality over the binding facts
/// (`aliases` / `plan_ids` / `ambiguous_plan_ids`) only. Mirrors
/// `ColumnReference`'s hand-written `PartialEq` excluding `ordinal`.
impl PartialEq for RelScope {
    fn eq(&self, other: &Self) -> bool {
        self.aliases == other.aliases
            && self.plan_ids == other.plan_ids
            && self.ambiguous_plan_ids == other.ambiguous_plan_ids
    }
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
            .filter(|(name, _)| name.eq_ignore_ascii_case(q))
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

    /// The field range and join-side qualifier a plan_id maps to. When a
    /// plan_id appears in multiple ancestor joins (nested join trees), the
    /// OUTERMOST entry wins — [`RelScope::of`] pushes parent entries before
    /// child entries, so the first match carries the qualifier in scope at
    /// the parent operator's resolution point.
    fn lookup_plan_id(&self, pid: i64) -> Option<(std::ops::Range<usize>, &'static str)> {
        self.plan_ids
            .iter()
            .find(|(id, _, _)| *id == pid)
            .map(|(_, range, qualifier)| (range.clone(), *qualifier))
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
    /// Binding rules (formerly `collect_qualifier_bindings`):
    /// - `TableScan{table, alias}`: bind `table` (and `alias`, if present) to
    ///   the full range.
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
    /// - `LateralView`: the input's scope, plus `table_alias` bound to the
    ///   generated columns' range appended after the input fields.
    /// - Everything else (`Project` / `Aggregate` / `SetOp` / `WithColumns` /
    ///   `Values` / `LocalRelation` / `TableFunction` / `Pivot` / ...):
    ///   EMPTY — these operators retype or reshuffle columns, so no alias
    ///   binding from further down is valid against the CURRENT schema.
    fn of(op: &TypedOp, resolved_schema: &StructType) -> Self {
        match op {
            TypedOp::TableScan { table, alias } => {
                let range = 0..resolved_schema.len();
                let mut aliases = vec![(table.clone(), range.clone())];
                if let Some(a) = alias {
                    aliases.push((a.clone(), range));
                }
                Self {
                    aliases,
                    plan_ids: Vec::new(),
                    ambiguous_plan_ids: Vec::new(),
                    // Populated by `source_quals_of` in `TypedAst::new`.
                    source_quals: Vec::new(),
                }
            }
            TypedOp::AliasedRelation { alias, .. } => Self {
                aliases: vec![(alias.clone(), 0..resolved_schema.len())],
                plan_ids: Vec::new(),
                ambiguous_plan_ids: Vec::new(),
                // Populated by `source_quals_of` in `TypedAst::new`.
                source_quals: Vec::new(),
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
                if !using_columns.is_empty() {
                    return Self::default();
                }
                let left_len = left.resolved_schema.len();
                let left_range = 0..left_len;
                let right_range = left_len..left_len + right.resolved_schema.len();
                let keep_right = !matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti);

                // ADR-023 3b-i: a plan_id present in BOTH this join's OWN
                // left and right sides is the un-realiased self-join
                // `df.join(df, ...)` — genuinely ambiguous (Spark cannot
                // tell which side is meant). When the right side is dropped
                // (LeftSemi/LeftAnti) it contributes no output columns, so
                // there is no ambiguity to raise.
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
                    plan_ids.push((pid, left_range.clone(), TD_JOIN_LEFT));
                }
                if keep_right {
                    for &pid in right_plan_ids {
                        plan_ids.push((pid, right_range.clone(), TD_JOIN_RIGHT));
                    }
                }
                plan_ids.extend(left.scope.plan_ids.iter().cloned());
                let mut ambiguous_plan_ids = own_ambiguous;
                ambiguous_plan_ids.extend(left.scope.ambiguous_plan_ids.iter().copied());
                let mut aliases = left.scope.aliases.clone();
                if keep_right {
                    let offset = |r: &std::ops::Range<usize>| r.start + left_len..r.end + left_len;
                    plan_ids.extend(
                        right
                            .scope
                            .plan_ids
                            .iter()
                            .map(|(pid, r, side)| (*pid, offset(r), *side)),
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
                Self {
                    aliases,
                    plan_ids,
                    ambiguous_plan_ids,
                    // Populated by `source_quals_of` in `TypedAst::new`.
                    source_quals: Vec::new(),
                }
            }
            scope_passthrough!(input) => input.scope.clone(),
            // LateralView appends generated columns after the input. Keep the
            // input's bindings, then bind the table_alias to the generated
            // columns' contiguous range. This makes `t.tag` resolve via the
            // qualifier-scoped path while `e.tag` correctly does NOT resolve.
            TypedOp::LateralView {
                input,
                table_alias,
                columns,
            } => {
                let mut scope = input.scope.clone();
                let start = input.resolved_schema.len();
                scope
                    .aliases
                    .push((table_alias.clone(), start..start + columns.len()));
                scope
            }
            // Everything below retypes or reshuffles columns: no alias
            // binding from further down is valid against the CURRENT schema.
            // Deliberately exhaustive (no `_`) so a new TypedOp variant
            // forces an explicit scope classification here AND in
            // `mark_node` (the compiler enforces the parallel update).
            TypedOp::Project { .. }
            | TypedOp::Aggregate { .. }
            | TypedOp::SetOp { .. }
            | TypedOp::SingleRow
            | TypedOp::Values { .. }
            | TypedOp::LocalRelation { .. }
            | TypedOp::FileScan { .. }
            | TypedOp::TableFunction { .. }
            | TypedOp::Unnest { .. }
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

/// ADR-023 tier 3c — derive the per-OUTPUT-column source-qualifier lineage a
/// just-built operator's schema carries. Shallow, like [`RelScope::of`]:
/// children are already stamped, so this reads their `scope.source_quals`
/// and never re-walks a subtree. ADDITIVE-only (this chunk): not yet
/// consulted by `resolve_column` or emission.
///
/// Exhaustively matches `TypedOp` (no `_`) so a new variant is a compile
/// error here until classified.
fn source_quals_of(
    op: &TypedOp,
    resolved_schema: &StructType,
) -> Vec<std::collections::BTreeSet<String>> {
    use std::collections::BTreeSet;

    let empty_set = || vec![BTreeSet::new(); resolved_schema.len()];

    let out = match op {
        TypedOp::TableScan { table, alias } => {
            let mut quals = BTreeSet::new();
            quals.insert(table.clone());
            if let Some(a) = alias {
                quals.insert(a.clone());
            }
            vec![quals; resolved_schema.len()]
        }
        TypedOp::AliasedRelation { alias, .. } => {
            let mut quals = BTreeSet::new();
            quals.insert(alias.clone());
            vec![quals; resolved_schema.len()]
        }
        scope_passthrough!(input) => input.scope.source_quals.clone(),
        TypedOp::Project { input, projections } => {
            if projections
                .iter()
                .any(|p| matches!(p, Expression::Star(_)))
            {
                // Star note (ADR-023 3c plan): precisely expanding `Star`/
                // `q.*` to a column count is deferred to 3d/3e. The
                // size-correct empty fallback keeps the invariant AND stays
                // corpus-neutral (3d reads ∅ → conservative unknown path).
                empty_set()
            } else {
                let mapped: Vec<BTreeSet<String>> = projections
                    .iter()
                    .map(|p| match p {
                        Expression::ColumnReference(cr) => match cr.ordinal {
                            Some(k) if k < input.scope.source_quals.len() => {
                                let mut quals = input.scope.source_quals[k].clone();
                                if let Some(q) = &cr.qualifier {
                                    quals.insert(q.clone());
                                }
                                quals
                            }
                            _ => BTreeSet::new(),
                        },
                        // Alias/computed columns are created — no inherited
                        // lineage. filt-019/F8 hinge.
                        _ => BTreeSet::new(),
                    })
                    .collect();
                if mapped.len() == resolved_schema.len() {
                    mapped
                } else {
                    // Defensive: projection-list length should always match
                    // the (non-Star) output schema length; fall back rather
                    // than mis-align if that invariant is ever violated.
                    empty_set()
                }
            }
        }
        TypedOp::Join {
            using_columns,
            left,
            right,
            join_type,
            ..
        } => {
            if !using_columns.is_empty() {
                // TODO(ADR-023 3d/3e): USING output reorders/dedups columns,
                // so no positional lineage holds yet — fill this in before
                // the USING legacy name-only resolution path is retired.
                empty_set()
            } else if matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti) {
                left.scope.source_quals.clone()
            } else {
                let mut quals = left.scope.source_quals.clone();
                quals.extend(right.scope.source_quals.iter().cloned());
                quals
            }
        }
        TypedOp::Aggregate {
            input,
            grouping,
            aggregates,
            ..
        } => {
            // The resolved_schema is grouping cols then aggregate cols
            // (CommonOp::Aggregate ordering) ONLY when the SparkSQL path has
            // not already folded grouping into `aggregates` — that fold
            // isn't visible here, so guard on length instead of guessing.
            if grouping.len() + aggregates.len() == resolved_schema.len() {
                let mut quals: Vec<BTreeSet<String>> = grouping
                    .iter()
                    .map(|g| match g {
                        Expression::ColumnReference(cr) => match cr.ordinal {
                            Some(k) if k < input.scope.source_quals.len() => {
                                input.scope.source_quals[k].clone()
                            }
                            _ => BTreeSet::new(),
                        },
                        _ => BTreeSet::new(),
                    })
                    .collect();
                // Aggregate output columns are created — no inherited lineage.
                quals.extend(aggregates.iter().map(|_| BTreeSet::new()));
                quals
            } else {
                empty_set()
            }
        }
        // TODO(ADR-023 3d/3e): a set operation's output positionally aligns
        // with its first child (by construction of the widened schema);
        // real per-column lineage is deferred — stay neutral for now.
        TypedOp::SetOp { .. }
        // TODO(ADR-023 3d/3e): `LateralView`'s input columns are positionally
        // unchanged (generated columns appended) — real lineage deferred.
        | TypedOp::LateralView { .. }
        // TODO(ADR-023 3d/3e): `WithColumns` is positional passthrough for
        // untouched input columns — real lineage deferred.
        | TypedOp::WithColumns { .. }
        // TODO(ADR-023 3d/3e): `WithColumnsRenamed` is a pure positional
        // passthrough (renames only) — real lineage deferred.
        | TypedOp::WithColumnsRenamed { .. }
        // TODO(ADR-023 3d/3e): `DropColumns` is positional passthrough minus
        // the dropped columns — real lineage deferred.
        | TypedOp::DropColumns { .. }
        | TypedOp::SingleRow
        | TypedOp::Values { .. }
        | TypedOp::LocalRelation { .. }
        | TypedOp::FileScan { .. }
        | TypedOp::TableFunction { .. }
        | TypedOp::Unnest { .. }
        | TypedOp::Describe { .. }
        | TypedOp::Summary { .. }
        | TypedOp::FreqItems { .. }
        | TypedOp::Unpivot { .. }
        | TypedOp::Pivot { .. }
        | TypedOp::RecursiveCte { .. } => empty_set(),
    };
    debug_assert_eq!(out.len(), resolved_schema.len());
    out
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
        /// True iff an analyzer-stamped `__td_jl` reference — in this join's
        /// own condition, or in an ancestor expression that resolves through
        /// schema-passthrough operators down to this join — requires emission
        /// to alias the LEFT side with the synthetic [`TD_JOIN_LEFT`] name.
        /// Stamped by the [`mark_join_alias_requirements`] post-pass; the
        /// single authority for emission's synthetic-vs-user join aliasing.
        left_requires_synthetic: bool,
        /// Right-side counterpart of `left_requires_synthetic`
        /// ([`TD_JOIN_RIGHT`]).
        right_requires_synthetic: bool,
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
    /// `LATERAL VIEW [OUTER] generator(arg) table_alias AS col1[, col2]`.
    /// Analyzer resolves each column expression against the input schema,
    /// then appends the generated fields to produce the output schema.
    LateralView {
        /// The input relation.
        input: Box<TypedAst>,
        /// The table alias.
        table_alias: String,
        /// Per-output-column `(alias, generator FunctionCall)` pairs.
        columns: Vec<(String, Expression)>,
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

impl AnalyzerError {
    /// The exact Spark error-class token this variant emulates, if any
    /// (ADR-023 chunk 3b). `None` for `Other` (no specific class to surface)
    /// and for the Thunderduck-boundary variants (no Spark class applies —
    /// these are τ's own gaps, not a Spark-emulated error).
    ///
    /// Best-effort mappings (subclass not reproduced): `UnknownColumn` →
    /// `UNRESOLVED_COLUMN` (Spark's `.WITH_SUGGESTION` subclass is not
    /// reachable here); `AmbiguousLateralColumnAlias` and `TypeMismatch` are
    /// likewise base-class only.
    pub fn spark_class(&self) -> Option<&'static str> {
        match self {
            Self::UnknownTable { .. } => Some("TABLE_OR_VIEW_NOT_FOUND"),
            Self::UnknownColumn { .. } => Some("UNRESOLVED_COLUMN"),
            Self::AmbiguousColumn { .. } => Some("AMBIGUOUS_REFERENCE"),
            Self::AmbiguousLateralColumnAlias { .. } => Some("AMBIGUOUS_LATERAL_COLUMN_ALIAS"),
            Self::TypeMismatch { .. } => Some("DATATYPE_MISMATCH"),
            Self::Other { .. } | Self::PuntedOperator { .. } | Self::UnsupportedRule { .. } => None,
        }
    }
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
    let mut typed = analyze_node(ast, base_types, None)?;
    // Post-pass: stamp each Join's per-side synthetic-alias requirement from
    // the `__td_jl`/`__td_jr` qualifiers resolution left behind (top-down —
    // demands flow from ancestor expressions DOWN to the join they bind).
    mark_join_alias_requirements(&mut typed);
    Ok(typed)
}

// ── Join synthetic-alias requirement post-pass ──────────────────────────────

/// Stamp every `TypedOp::Join`'s `left_requires_synthetic` /
/// `right_requires_synthetic` flags: true iff any analyzer-stamped
/// [`TD_JOIN_LEFT`]/[`TD_JOIN_RIGHT`] qualifier demands that side be
/// addressable under its synthetic alias in the emitted FROM scope.
///
/// Demands arise in the join's own condition (`qualify_plan_id_refs`) or in
/// an ancestor operator's expressions (`resolve_column`'s plan_id arm stamps
/// `__td_jl`/`__td_jr` on ambiguous refs above a join). An ancestor demand
/// reaches the join only through the same schema-passthrough operator class
/// [`RelScope`] recognizes (Filter / Sort / Limit / Sample / SampleBy /
/// Deduplicate / NaFill / NaDrop / NaReplace / LateralView) — every other
/// operator re-scopes, so resolution can never stamp a synthetic qualifier
/// across it (enforced with a `debug_assert`).
///
/// Subquery inner plans are separate resolution universes: they are marked
/// recursively with fresh demand state. (A correlated inner reference to an
/// OUTER join's synthetic alias is not propagated — matching today's
/// resolution scoping, which never stamps one.)
fn mark_join_alias_requirements(node: &mut TypedAst) {
    mark_node(node, false, false);
}

/// Accumulate `__td_jl`/`__td_jr` qualifier uses in `expr`'s immediate tree
/// (subquery bodies excluded per the τ walker convention — they resolve
/// against their own scopes).
fn synthetic_uses(expr: &Expression, jl: &mut bool, jr: &mut bool) {
    let mut check = |q: Option<&str>| {
        if let Some(q) = q {
            if q.eq_ignore_ascii_case(TD_JOIN_LEFT) {
                *jl = true;
            } else if q.eq_ignore_ascii_case(TD_JOIN_RIGHT) {
                *jr = true;
            }
        }
    };
    match expr {
        Expression::ColumnReference(c) => check(c.qualifier.as_deref()),
        Expression::UnresolvedColumn(u) => check(u.qualifier.as_deref()),
        Expression::Star(s) => check(s.qualifier.as_deref()),
        other => {
            for child in other.children() {
                synthetic_uses(child, jl, jr);
            }
        }
    }
}

/// Recurse [`mark_node`] into every analyzed subquery plan carried by
/// `expr` (fresh demand state — inner plans resolve against their own
/// scopes).
fn mark_expr_subplans(expr: &mut Expression) {
    let mark_plan = |plan: &mut SubqueryPlan| {
        if let SubqueryPlan::Analyzed(inner) = plan {
            mark_node(inner, false, false);
        }
    };
    match expr {
        Expression::InSubquery(i) => {
            mark_plan(&mut i.subquery);
            mark_expr_subplans(&mut i.expr);
        }
        Expression::ExistsSubquery(e) => mark_plan(&mut e.subquery),
        Expression::ScalarSubquery(s) => mark_plan(&mut s.subquery),
        other => {
            for child in other.children_mut() {
                mark_expr_subplans(child);
            }
        }
    }
}

/// Collect synthetic-qualifier uses across `exprs` and mark their subquery
/// plans, returning the accumulated `(jl, jr)` demands.
fn scan_exprs<'e>(exprs: impl IntoIterator<Item = &'e mut Expression>) -> (bool, bool) {
    let (mut jl, mut jr) = (false, false);
    for e in exprs {
        synthetic_uses(e, &mut jl, &mut jr);
        mark_expr_subplans(e);
    }
    (jl, jr)
}

fn mark_node(node: &mut TypedAst, pending_jl: bool, pending_jr: bool) {
    let (own_jl, own_jr) = own_expr_demands(&mut node.op);
    match &mut node.op {
        TypedOp::Join {
            left,
            right,
            left_requires_synthetic,
            right_requires_synthetic,
            ..
        } => {
            *left_requires_synthetic = pending_jl || own_jl;
            *right_requires_synthetic = pending_jr || own_jr;
            // Synthetic names are per-join-level: demands do not cross into
            // the sides (inner joins own their own `__td_jl`/`__td_jr`).
            mark_node(left, false, false);
            mark_node(right, false, false);
        }
        // Schema-passthrough operators (single authority: the
        // `scope_passthrough!` class shared with `RelScope::of`): own
        // expression demands compose with the pending ones and continue
        // toward the join below.
        scope_passthrough!(input) => {
            mark_node(input, pending_jl || own_jl, pending_jr || own_jr);
        }
        // LateralView is scope-passthrough-plus-append; its generator
        // expressions resolve against the input scope.
        TypedOp::LateralView { input, .. } => {
            mark_node(input, pending_jl || own_jl, pending_jr || own_jr);
        }
        // AliasedRelation re-scopes its subtree under the user alias.
        TypedOp::AliasedRelation { input, .. } => {
            mark_node(input, false, false);
        }
        // Re-scoping unary operators: resolution can never stamp a synthetic
        // qualifier across them, so no pending demand can arrive here. Their
        // own expressions resolve against the input scope and start the
        // demand chain.
        TypedOp::Project { input, .. }
        | TypedOp::Aggregate { input, .. }
        | TypedOp::WithColumns { input, .. }
        | TypedOp::Pivot { input, .. }
        | TypedOp::Unpivot { input, .. }
        | TypedOp::DropColumns { input, .. }
        | TypedOp::WithColumnsRenamed { input, .. }
        | TypedOp::Describe { input, .. }
        | TypedOp::Summary { input, .. }
        | TypedOp::FreqItems { input, .. } => {
            // No pending demand SHOULD arrive here (these operators
            // re-scope, so resolution can never stamp a synthetic qualifier
            // across them) — but this walk runs on untrusted, analyzed user
            // input, and a session thread must never panic on it. A
            // user-typed reserved qualifier is rejected during resolution
            // (`resolve_column`, F13), but that is not the only path a
            // `pending_jl`/`pending_jr` demand could theoretically reach
            // here (e.g. a qualified star — F12 territory), so tolerate and
            // drop the demand rather than assert.
            mark_node(input, own_jl, own_jr);
        }
        TypedOp::SetOp { children, .. } => {
            // See the comment above: tolerate a stray pending demand rather
            // than panic on untrusted input.
            for child in children {
                mark_node(child, false, false);
            }
        }
        TypedOp::RecursiveCte {
            anchor,
            recursive_term,
            ..
        } => {
            mark_node(anchor, false, false);
            mark_node(recursive_term, false, false);
        }
        // Leaves: `Values`/`LocalRelation` rows and `TableFunction` args are
        // literal-bearing and resolve against bare scopes - no joins below,
        // no demands to carry.
        TypedOp::SingleRow
        | TypedOp::TableScan { .. }
        | TypedOp::Values { .. }
        | TypedOp::LocalRelation { .. }
        | TypedOp::FileScan { .. }
        | TypedOp::TableFunction { .. }
        | TypedOp::Unnest { .. } => {}
    }
}

/// Scan the operator's OWN expressions for `__td_jl`/`__td_jr` demands and
/// recurse [`mark_node`] into any analyzed subquery plans they carry.
/// Exhaustive over expression-carrying variants: a new variant with
/// expressions must decide here whether its expressions resolve against the
/// input scope (scan them) or a bare scope (skip).
fn own_expr_demands(op: &mut TypedOp) -> (bool, bool) {
    match op {
        TypedOp::Join { condition, .. } => scan_exprs(condition.iter_mut()),
        TypedOp::Filter { condition, .. } => scan_exprs(std::iter::once(condition)),
        TypedOp::Sort { order, .. } => scan_exprs(order.iter_mut().map(|so| so.expr.as_mut())),
        TypedOp::SampleBy { col, .. } => scan_exprs(std::iter::once(col)),
        TypedOp::NaFill { values, .. } => scan_exprs(values.iter_mut()),
        TypedOp::NaReplace { replacements, .. } => {
            scan_exprs(replacements.iter_mut().flat_map(|(old, new)| [old, new]))
        }
        TypedOp::LateralView { columns, .. } => scan_exprs(columns.iter_mut().map(|(_, e)| e)),
        TypedOp::Project { projections, .. } => scan_exprs(projections.iter_mut()),
        TypedOp::Aggregate {
            grouping,
            aggregates,
            having,
            ..
        } => scan_exprs(
            grouping
                .iter_mut()
                .chain(aggregates.iter_mut())
                .chain(having.iter_mut()),
        ),
        TypedOp::WithColumns { assignments, .. } => {
            scan_exprs(assignments.iter_mut().map(|(_, e)| e))
        }
        TypedOp::Pivot {
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
            ..
        } => scan_exprs(
            grouping
                .iter_mut()
                .chain(std::iter::once(pivot_column))
                .chain(pivot_values.iter_mut())
                .chain(aggregates.iter_mut()),
        ),
        // Literal-bearing rows / args resolve against bare scopes; the
        // remaining variants carry no expressions.
        TypedOp::Limit { .. }
        | TypedOp::Sample { .. }
        | TypedOp::Deduplicate { .. }
        | TypedOp::NaDrop { .. }
        | TypedOp::AliasedRelation { .. }
        | TypedOp::DropColumns { .. }
        | TypedOp::WithColumnsRenamed { .. }
        | TypedOp::Describe { .. }
        | TypedOp::Summary { .. }
        | TypedOp::FreqItems { .. }
        | TypedOp::Unpivot { .. }
        | TypedOp::SetOp { .. }
        | TypedOp::RecursiveCte { .. }
        | TypedOp::SingleRow
        | TypedOp::TableScan { .. }
        | TypedOp::Values { .. }
        | TypedOp::LocalRelation { .. }
        | TypedOp::FileScan { .. }
        | TypedOp::TableFunction { .. }
        | TypedOp::Unnest { .. } => (false, false),
    }
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
        TypedOp::LateralView { input, columns, .. } => {
            has_resolved_schema(input)
                && columns.iter().all(|(_, e)| expression_is_fully_resolved(e))
        }
        TypedOp::SingleRow | TypedOp::TableScan { .. } | TypedOp::FileScan { .. } => true,
    }
}

/// Bridge an [`AnalyzerError`] into an [`EmissionError`] preserving the
/// two-category classification. Spark-emulated variants with a known
/// [`AnalyzerError::spark_class`] surface as [`EmissionError::SparkEmulated`]
/// so the class token leads the wire message (ADR-023 chunk 3b); `Other`
/// (no class) keeps the legacy `Unsupported{name: "analyzer-spark-emulated"}`
/// path. Thunderduck-boundary variants (`[TDCK-BOUNDARY]`) are unaffected.
/// Called by `transpiler_v2::generate()`.
pub(super) fn analyzer_error_to_emission_error(e: AnalyzerError) -> EmissionError {
    match e.spark_class() {
        Some(class) => {
            let full = e.to_string();
            let message = full
                .strip_prefix("[SPARK-EMULATED] ")
                .unwrap_or(&full)
                .to_owned();
            EmissionError::SparkEmulated { class, message }
        }
        None => {
            let full = e.to_string();
            match e {
                AnalyzerError::Other { .. } => EmissionError::Unsupported {
                    kind: UnsupportedKind::Expression,
                    name: "analyzer-spark-emulated".to_owned(),
                    reason: full,
                },
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
                AnalyzerError::UnknownTable { .. }
                | AnalyzerError::UnknownColumn { .. }
                | AnalyzerError::AmbiguousColumn { .. }
                | AnalyzerError::AmbiguousLateralColumnAlias { .. }
                | AnalyzerError::TypeMismatch { .. } => {
                    unreachable!("spark_class() returns Some for these variants")
                }
            }
        }
    }
}

// ── Internal: single-pass bottom-up analyzer ────────────────────────────────

/// Analyze `input`, clone its resolved schema, and wrap it in a caller-built
/// [`TypedOp`] variant. The output schema is a straight passthrough — used by
/// operators that neither add, drop, nor retype columns (Filter, Sort, Limit,
/// Deduplicate, Sample, SampleBy, NaDrop, NaReplace, AliasedRelation).
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

fn analyze_node(
    ast: CommonAst,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    match ast.op {
        // ── Leaves ────────────────────────────────────────────────────────
        CommonOp::SingleRow => Ok(TypedAst::new(TypedOp::SingleRow, StructType::empty())),

        CommonOp::TableScan { table, alias } => {
            // resolve: seed schema from base_types.
            let schema =
                base_types
                    .lookup(&table)
                    .cloned()
                    .ok_or_else(|| AnalyzerError::UnknownTable {
                        name: table.clone(),
                    })?;
            // At τ's analyzer, we don't rewrite field qualifiers into names —
            // the alias is preserved on the operator itself. future τ work's
            // renderer handles the alias projection.
            Ok(TypedAst::new(TypedOp::TableScan { table, alias }, schema))
        }

        CommonOp::Values { rows, column_names } => {
            let schema = infer_values_schema(&rows, &column_names)?;
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
            schema,
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
                s,
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

        // ── Unary ─────────────────────────────────────────────────────────
        CommonOp::Project { input, projections } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
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
            // piv-006 — expand `stack(N, v11, ..., vNK) AS (a1, ..., aK)`
            // (wrapped by `parser_v2::SparkSqlParserV2::parse_expression` as
            // `stack_multi_alias(<stack call>, "a1", ..., "aK")`) into K
            // per-column projections `Alias(stack_col(v1i, v2i, ..., vNi),
            // "ai")`. Emission maps `stack_col(...)` to `UNNEST([...])`.
            let projections = expand_stack_projections(projections)?;
            // Pass 9 (pr-007) — Spark's Lateral Column Alias (LCA): a later
            // SELECT-list item may reference an earlier item's alias, e.g.
            // `SELECT salary * 1.1 AS raised, raised - salary AS delta`.
            // Left-to-right fold; substitutes fully-inlined earlier aliases
            // into later items BEFORE `resolve_and_stamp` ever sees them, so
            // the rest of resolution/typing is completely unaware of LCA.
            let projections =
                expand_lateral_column_aliases(projections, &typed_input.resolved_schema)?;
            let ctx = ResolveContext::of_input(&typed_input, base_types, outer);
            let projections = projections
                .into_iter()
                .map(|e| resolve_and_stamp(e, &ctx))
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
        } => passthrough_schema_arm(*input, base_types, outer, |ti| {
            let ctx = ResolveContext::of_input(&ti, base_types, outer);
            let order = order
                .into_iter()
                .map(|so| {
                    let expr = resolve_and_stamp(*so.expr, &ctx)?;
                    Ok::<SortOrder, AnalyzerError>(SortOrder {
                        expr: Box::new(expr),
                        direction: so.direction,
                        null_ordering: so.null_ordering,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TypedOp::Sort {
                input: Box::new(ti),
                order,
                limit,
                offset,
            })
        }),

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
            let aggregates = resolve_expr_list(aggregates, &ctx)?;
            // HAVING resolves against the aggregate INPUT schema (aggregate
            // exprs + grouping keys bind to input columns), with the same
            // boolean-type guard as Filter.
            let having = having
                .map(|h| resolve_boolean_predicate(h, &ctx, "having-condition"))
                .transpose()?;
            // Output schema construction:
            // SparkSQL path folds grouping cols into `aggregates` already
            // (per CommonOp::Aggregate invariant), so output = aggregates as-is.
            // DataFrame path keeps them separate — detect by seeing whether
            // the aggregates list already begins with the grouping's output
            // names; if not, prepend grouping. Empty grouping = global agg
            // (no unfolding needed).
            let already_folded = grouping_already_folded(&grouping, &aggregates);
            let mut output_fields: Vec<StructField> = Vec::new();
            if !already_folded {
                output_fields.extend(
                    grouping
                        .iter()
                        .map(|g| expr_field(g, &typed_input.resolved_schema)),
                );
            }
            output_fields.extend(
                aggregates
                    .iter()
                    .map(|e| expr_field(e, &typed_input.resolved_schema)),
            );
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
                    if grouping_names
                        .iter()
                        .any(|gn| gn.eq_ignore_ascii_case(&field.name))
                    {
                        field.nullable = true;
                    }
                }
            }
            let output_schema = StructType::new(output_fields);
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

        // ── WithColumns (add-or-replace by name, Spark semantics) ────────
        CommonOp::WithColumns { input, assignments } => {
            analyze_with_columns(*input, assignments, base_types, outer)
        }

        // ── NA family ────────────────────────────────────────────────────
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
            outer,
        ),

        // ── Describe (Spark `df.describe(...)`) ─────────────────────────
        CommonOp::Describe { input, cols } => analyze_describe(*input, cols, base_types, outer),

        // ── Summary (Spark `df.summary(...)`) ───────────────────────────
        CommonOp::Summary { input, statistics } => {
            analyze_summary(*input, statistics, base_types, outer)
        }

        // ── FreqItems (Spark `df.stat.freqItems(...)`) ──────────────────
        CommonOp::FreqItems {
            input,
            cols,
            support,
        } => analyze_freq_items(*input, cols, support, base_types, outer),

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
            outer,
        ),

        // ── Deduplicate (Spark `df.dropDuplicates` / `df.distinct`) ──────
        CommonOp::Deduplicate { input, on_columns } => {
            passthrough_schema_arm(*input, base_types, outer, |ti| {
                Ok(TypedOp::Deduplicate {
                    input: Box::new(ti),
                    on_columns,
                })
            })
        }

        // ── Sample (Spark `df.sample(...)`) ─────────────────────────────
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

        // ── SampleBy (Spark `df.sampleBy(col, fractions, seed)`) ───────
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

        // ── ToDf (Spark `df.toDF(new1, new2, ...)`) ──────────────────────
        CommonOp::ToDf {
            input,
            column_names,
        } => analyze_to_df(*input, column_names, base_types, outer),

        // ── AliasedRelation (Spark `df.alias(name)`) ─────────────────────
        CommonOp::AliasedRelation { input, alias } => {
            passthrough_schema_arm(*input, base_types, outer, |ti| {
                Ok(TypedOp::AliasedRelation {
                    input: Box::new(ti),
                    alias,
                })
            })
        }

        // ── WithColumnsRenamed (Spark `df.withColumnsRenamed(...)`) ──────
        CommonOp::WithColumnsRenamed { input, renames } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
            let rename_map: HashMap<String, String> = renames
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
            Ok(TypedAst::new(
                TypedOp::WithColumnsRenamed {
                    input: Box::new(typed_input),
                    renames,
                },
                output_schema,
            ))
        }

        // ── DropColumns (Spark `df.drop(...)`) ───────────────────────────
        CommonOp::DropColumns { input, drop_names } => {
            let typed_input = analyze_node(*input, base_types, outer)?;
            let drop_lower: HashSet<String> = drop_names.iter().map(|s| s.to_lowercase()).collect();
            let mut output_fields: Vec<StructField> =
                Vec::with_capacity(typed_input.resolved_schema.fields.len());
            for f in &typed_input.resolved_schema.fields {
                if !drop_lower.contains(&f.name.to_lowercase()) {
                    output_fields.push(f.clone());
                }
            }
            let output_schema = StructType::new(output_fields);
            Ok(TypedAst::new(
                TypedOp::DropColumns {
                    input: Box::new(typed_input),
                    drop_names,
                },
                output_schema,
            ))
        }

        // ── LateralView (Hive LATERAL VIEW explode/posexplode) ──────────
        CommonOp::LateralView {
            input,
            table_alias,
            columns,
        } => analyze_lateral_view(*input, table_alias, columns, base_types, outer),

        // ── RecursiveCte (two-phase anchor-first) ──────────────────────
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

        // ── Binary: Join ──────────────────────────────────────────────────
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

        // ── N-ary: SetOp with widening sub-sweep ──────────────────────────
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

// ── Extracted arm bodies (Pass 13 — OPP-V uniform arm shape) ────────────────

/// Analyze a `LATERAL VIEW` node: resolve each generator column expression
/// against the input schema, compute generated-field types/nullability, and
/// produce a merged output schema `input fields ++ generated fields`.
fn analyze_lateral_view(
    input: CommonAst,
    table_alias: String,
    columns: Vec<(String, Expression)>,
    base_types: &BaseTypes,
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
    let ctx = ResolveContext::of_input(&typed_input, base_types, outer);
    let input_schema = &typed_input.resolved_schema;
    let resolved_columns: Vec<(String, Expression)> = columns
        .into_iter()
        .map(|(alias, expr)| {
            let resolved = resolve_and_stamp(expr, &ctx)?;
            // Loud-fail if the generated type resolves Unresolved.
            let dt = resolved.data_type(input_schema);
            if dt == DataType::Unresolved {
                return Err(AnalyzerError::PuntedOperator {
                    op: "LateralView".to_owned(),
                    reason: format!("generated column `{alias}` resolved to Unresolved type"),
                });
            }
            Ok((alias, resolved))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let generated_schema = StructType::new(
        resolved_columns
            .iter()
            .map(|(alias, expr)| {
                StructField::new(
                    alias.clone(),
                    expr.data_type(input_schema),
                    expr.nullable(input_schema),
                )
            })
            .collect(),
    );
    let resolved_schema = StructType::merge(&typed_input.resolved_schema, &generated_schema);
    Ok(TypedAst::new(
        TypedOp::LateralView {
            input: Box::new(typed_input),
            table_alias,
            columns: resolved_columns,
        },
        resolved_schema,
    ))
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
    let mut output_fields: Vec<StructField> =
        Vec::with_capacity(input_schema.fields.len() + resolved_assignments.len());
    for (f, replaced_by) in input_schema.fields.iter().zip(&plan.replaced) {
        if let Some(idx) = replaced_by {
            let (_, expr) = &resolved_assignments[*idx];
            let dt = expr.data_type(input_schema);
            let nullable = expr.nullable(input_schema);
            // Preserve the input field's original casing for the name.
            output_fields.push(StructField::new(f.name.clone(), dt, nullable));
        } else {
            output_fields.push(f.clone());
        }
    }
    for &i in &plan.appended {
        let (name, expr) = &resolved_assignments[i];
        let dt = expr.data_type(input_schema);
        let nullable = expr.nullable(input_schema);
        output_fields.push(StructField::new(name.clone(), dt, nullable));
    }
    let output_schema = StructType::new(output_fields);
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
    input_schema: &StructType,
    assignments: &[(String, Expression)],
) -> WithColumnsPlan {
    let mut assigned_lower: HashMap<String, usize> = HashMap::with_capacity(assignments.len());
    for (i, (name, _)) in assignments.iter().enumerate() {
        assigned_lower.insert(name.to_lowercase(), i);
    }
    let mut consumed = vec![false; assignments.len()];
    let mut replaced: Vec<Option<usize>> = Vec::with_capacity(input_schema.fields.len());
    for f in &input_schema.fields {
        let idx = assigned_lower.get(&f.name.to_lowercase()).copied();
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
    schema: &StructType,
    col_name: &str,
    col_type: &DataType,
) -> Option<&'a Expression> {
    if cols.is_empty() || values.len() == 1 {
        // Single fill value: the column is selected either because
        // `cols` is empty ("fill all") or because it names the column;
        // Spark's type-compatibility filter then gates the fill.
        let selected = cols.is_empty() || cols.iter().any(|c| c.eq_ignore_ascii_case(col_name));
        if selected && na_fill_compatible(col_type, &values[0].data_type(schema)) {
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
    let mut output_fields: Vec<StructField> =
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
    let output_schema = StructType::new(output_fields);
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
    Ok(TypedAst::new(
        TypedOp::WithColumnsRenamed {
            input: Box::new(typed_input),
            renames,
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

    // ── LATERAL guards (analyzer-enforced invariants) ──────────────────
    if lateral && natural {
        return Err(AnalyzerError::Other {
            reason: "UNSUPPORTED_FEATURE: LATERAL join with NATURAL join".to_owned(),
        });
    }
    if lateral && !using_columns.is_empty() {
        return Err(AnalyzerError::Other {
            reason: "UNSUPPORTED_FEATURE: LATERAL join with USING join".to_owned(),
        });
    }
    if lateral && !matches!(join_type, JoinType::Inner | JoinType::Cross) {
        return Err(AnalyzerError::PuntedOperator {
            op: format!("Join[lateral-{join_type:?}]"),
            reason: "lateral join type not implemented in τ".to_owned(),
        });
    }

    // ── Right-child analysis ───────────────────────────────────────────
    // When `lateral`, the right child sees the left sibling's schema as
    // its OuterScope (correlated refs like `e.dept_id` resolve there).
    // This REPLACES whatever `outer` was passed in — preserving pass-16's
    // one-level-only invariant (the lateral's inner sees only its
    // immediate left sibling, never the grandparent).
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
        Some(mut c) => {
            qualify_plan_id_refs(&mut c, &left_plan_ids, &right_plan_ids);
            let ctx = ResolveContext::for_join_condition(
                &combined_input_schema,
                &typed_left,
                &typed_right,
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
            let left_field = derived_left_schema.field_by_name(n);
            let right_field = derived_right_schema.field_by_name(n);
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
        let using_lower: HashSet<String> = using_columns.iter().map(|s| s.to_lowercase()).collect();
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
            // Stamped by the `mark_join_alias_requirements` post-pass once
            // the whole tree is analyzed (demands flow top-down).
            left_requires_synthetic: false,
            right_requires_synthetic: false,
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

    // (b) Rename anchor schema by column_names (positional).
    let cte_schema = if column_names.is_empty() {
        typed_anchor.resolved_schema.clone()
    } else {
        // (d) Arity check — column list length must match anchor output width.
        if column_names.len() != typed_anchor.resolved_schema.fields.len() {
            return Err(AnalyzerError::Other {
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
                StructField::new(col_name.clone(), f.data_type.clone(), f.nullable)
            })
            .collect();
        StructType::new(fields)
    };

    // (c) Reject UNION (without ALL) — Spark's UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE.
    if !union_all {
        return Err(AnalyzerError::Other {
            reason: "UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE: recursive CTE body must use UNION ALL"
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
        return Err(AnalyzerError::Other {
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

    let resolved_schema = StructType::new(
        cte_schema
            .fields
            .iter()
            .zip(output_nullable)
            .map(|(f, nullable)| StructField::new(f.name.clone(), f.data_type.clone(), nullable))
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
        if table.to_lowercase() == cte_name_lower && !out.contains(table) {
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
                .map(|f| f.name.to_lowercase())
                .collect();
            for (idx, child) in typed_children.iter().enumerate().skip(1) {
                let child_names_lower: HashSet<String> = child
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
    // mis-casts columns (e.g. `salary DOUBLE → id BIGINT`). Pass 76 /
    // corpus witness: `set-003`. Skip the pushdown for by-name.
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
fn widen_by_name(children: &[TypedAst]) -> Result<StructType, AnalyzerError> {
    // Build the ordered union of names across all children.
    // Case-insensitive dedup with first-seen casing preserved
    // (matches `StructType::field_by_name`).
    let mut ordered_names: Vec<String> = Vec::new();
    let mut seen_lower: HashSet<String> = HashSet::new();
    for child in children {
        for f in &child.resolved_schema.fields {
            let lower = f.name.to_lowercase();
            if seen_lower.insert(lower) {
                ordered_names.push(f.name.clone());
            }
        }
    }
    let mut widened_fields: Vec<StructField> = Vec::with_capacity(ordered_names.len());
    for name in &ordered_names {
        let mut widened_type: Option<DataType> = None;
        let mut widened_nullable = false;
        let mut any_child_missing = false;
        for child in children {
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
            reason: format!("internal: union-of-names produced orphan name {name:?}"),
        })?;
        // Extras present in only one child become
        // unconditionally nullable — the other child pads
        // with NULL. Stronger than the OR rule.
        let nullable = widened_nullable || any_child_missing;
        widened_fields.push(StructField::new(name.clone(), ty, nullable));
    }
    Ok(StructType::new(widened_fields))
}

/// Widen a positional (by-index) set-op schema across `children`: verify
/// arity, unify each column index's type across every child, and fold
/// nullability per Spark's operator-aware rule. Names and order come from the
/// first child.
fn widen_by_position(kind: SetOpKind, children: &[TypedAst]) -> Result<StructType, AnalyzerError> {
    let first_len = children[0].resolved_schema.len();
    for (idx, child) in children.iter().enumerate().skip(1) {
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
        widened_fields.push(StructField::new(
            first_field.name.clone(),
            widened_type,
            widened_nullable,
        ));
    }
    Ok(StructType::new(widened_fields))
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
    expand_projections(projections, |proj| {
        let r = match proj {
            Expression::UnresolvedRegex(r) => r,
            _ => return Ok(None),
        };
        let re = regex::Regex::new(&r.pattern).map_err(|e| AnalyzerError::Other {
            reason: format!("invalid regex `{}`: {e}", r.pattern),
        })?;
        let expanded: Vec<Expression> = input_schema
            .fields
            .iter()
            .filter(|f| re.is_match(&f.name))
            .map(|f| {
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: f.name.clone(),
                    qualifier: None,
                    plan_id: r.plan_id,
                })
            })
            .collect();
        if expanded.is_empty() {
            return Err(AnalyzerError::UnknownColumn {
                name: r.pattern.clone(),
                qualifier: None,
            });
        }
        Ok(Some(expanded))
    })
}

/// Shared driver for the Project pre-pass expanders
/// ([`expand_regex_projections`], [`expand_inline_projections`],
/// [`expand_json_tuple_projections`], [`expand_stack_projections`]): walk the
/// projection list in order, splicing in `try_expand`'s replacement list when
/// it returns `Some(...)` and passing the projection through unchanged in
/// place when it returns `None`.
fn expand_projections(
    projections: Vec<Expression>,
    mut try_expand: impl FnMut(&Expression) -> Result<Option<Vec<Expression>>, AnalyzerError>,
) -> Result<Vec<Expression>, AnalyzerError> {
    let mut out = Vec::with_capacity(projections.len());
    for proj in projections {
        match try_expand(&proj)? {
            Some(expanded) => out.extend(expanded),
            None => out.push(proj),
        }
    }
    Ok(out)
}

/// Build `Alias(FunctionCall(name, args), alias)` — the synthetic-projection
/// shape shared by the Project pre-pass expanders.
fn aliased_call(name: &str, args: Vec<Expression>, alias: String) -> Expression {
    Expression::Alias(AliasExpression {
        expr: Box::new(Expression::FunctionCall(FunctionCall {
            name: name.to_owned(),
            args,
            distinct: false,
        })),
        alias,
    })
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
    expand_projections(projections, |proj| {
        // Only fire on a bare top-level `FunctionCall("inline"|"inline_outer",...)`.
        // Aliased or nested forms fall through unchanged (multi-alias
        // `.alias("f1","f2",...)` and non-Project contexts are non-goals per
        // the Pass-90 plan §Non-goals — they surface as boundary errors
        // downstream if the corpus ever exercises them).
        let (name_lower, args, is_outer) = match proj {
            Expression::FunctionCall(f) => {
                let n = f.name.to_ascii_lowercase();
                match n.as_str() {
                    "inline" => (n, f.args.clone(), false),
                    "inline_outer" => (n, f.args.clone(), true),
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
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
                    bail_boundary_rule!(
                        format!("{name_lower}-expansion"),
                        format!(
                            "`{name_lower}` argument's element type could not be statically resolved — τ requires a resolved `Array<Struct<...>>` schema"
                        ),
                    );
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
                bail_boundary_rule!(
                    format!("{name_lower}-expansion"),
                    format!(
                        "`{name_lower}` argument's type could not be statically resolved — τ requires a resolved `Array<Struct<...>>` schema"
                    ),
                );
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
        let mut expanded = Vec::with_capacity(elem_struct.fields.len());
        for field in &elem_struct.fields {
            let field_name_lit = Expression::Literal(Literal {
                value: LiteralValue::String(field.name.clone()),
                data_type: DataType::String,
            });
            expanded.push(aliased_call(
                synthetic_name,
                vec![arr.clone(), field_name_lit],
                field.name.clone(),
            ));
        }
        Ok(Some(expanded))
    })
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
    expand_projections(projections, |proj| {
        // Only fire on a bare top-level `FunctionCall("json_tuple", ...)`.
        // Aliased or nested forms fall through unchanged (multi-alias
        // `.alias("k1", ...)` and non-Project contexts are non-goals per
        // the Pass-91 plan §Non-goals — they surface as boundary errors
        // downstream if the corpus ever exercises them).
        let args = match proj {
            Expression::FunctionCall(f) if f.name.eq_ignore_ascii_case("json_tuple") => {
                f.args.clone()
            }
            _ => return Ok(None),
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
        let mut expanded = Vec::with_capacity(key_args.len());
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
                bail_boundary_rule!(
                    "json_tuple-expansion",
                    format!(
                        "`json_tuple` key `{key}` contains a character τ does not \
                         safely forward to DuckDB's `json_extract_string` — reject \
                         to avoid diverging from Spark's flat-key semantics or \
                         breaking the SQL string literal"
                    ),
                );
            }
            let key_lit = Expression::Literal(Literal {
                value: LiteralValue::String(key),
                data_type: DataType::String,
            });
            expanded.push(aliased_call(
                "json_tuple_field",
                vec![json_expr.clone(), key_lit],
                format!("c{i}"),
            ));
        }
        Ok(Some(expanded))
    })
}

/// Expand every top-level `stack_multi_alias(<stack call>, "a1", ..., "aK")`
/// projection into K per-column projections
/// `Alias(stack_col(v1i, v2i, ..., vNi), "ai")`.
///
/// The wrapper is synthesized by
/// [`crate::parser_v2::SparkSqlParserV2::parse_expression`] when it detects a
/// trailing multi-column alias `AS (a1, ..., aK)` on a `stack(...)` call — a
/// shape sqlparser-rs 0.61's `SelectItem::ExprWithAlias { alias: Ident }`
/// cannot represent. Non-`stack_multi_alias` projections pass through
/// unchanged; schema order is preserved.
///
/// Errors (ADR-022 two-category):
///
/// * **Spark-emulated** ([`AnalyzerError::Other`]) — `stack`'s first argument
///   is not a positive-integer literal `N`, or `stack`'s value-argument
///   count is not `1 + N*K` (Spark's `Stack.checkInputDataTypes` matches
///   this shape).
/// * **Thunderduck-boundary** ([`AnalyzerError::UnsupportedRule`], Display
///   prefix `[TDCK-BOUNDARY]`, `rule = "stack-multi-alias-expansion"`) — the
///   wrapped inner expression is not a `stack(...)` FunctionCall (the parser
///   wrap-site guards this, but the analyzer double-checks).
///
/// Called by `analyze_node`'s `CommonOp::Project` arm AFTER
/// [`expand_json_tuple_projections`] and BEFORE [`expand_lateral_column_aliases`]
/// / [`resolve_and_stamp`], so downstream analysis only ever sees the
/// fanned-out `stack_col` calls.
///
/// Corpus witness: `piv-006`.
fn expand_stack_projections(
    projections: Vec<Expression>,
) -> Result<Vec<Expression>, AnalyzerError> {
    expand_projections(projections, |proj| {
        // Only fire on a bare top-level `FunctionCall("stack_multi_alias", ...)`.
        // Aliased or nested forms fall through unchanged — non-Project
        // contexts are Spark-invalid for generator functions and surface
        // as boundary errors downstream.
        let args = match proj {
            Expression::FunctionCall(f) if f.name.eq_ignore_ascii_case("stack_multi_alias") => {
                f.args.clone()
            }
            _ => return Ok(None),
        };
        // args[0] = inner `stack` FunctionCall, args[1..] = K string-literal
        // aliases.
        if args.len() < 2 {
            bail_boundary_rule!(
                "stack-multi-alias-expansion",
                format!(
                    "`stack_multi_alias` wrapper must carry the inner stack call \
                     plus at least one alias, got {} arg(s)",
                    args.len()
                ),
            );
        }
        let mut args_iter = args.into_iter();
        let inner = args_iter.next().expect("checked args.len() >= 2 above");
        let aliases: Vec<String> = args_iter
            .map(|a| match a {
                Expression::Literal(Literal {
                    value: LiteralValue::String(s),
                    ..
                }) => Ok(s),
                other => Err(AnalyzerError::Other {
                    reason: format!(
                        "`stack_multi_alias` alias slots must be string literals, got {other:?}"
                    ),
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let k = aliases.len();

        let stack_args = match inner {
            Expression::FunctionCall(fc) if fc.name.eq_ignore_ascii_case("stack") => fc.args,
            other => {
                bail_boundary_rule!(
                    "stack-multi-alias-expansion",
                    format!(
                        "multi-alias `AS ( ... )` on a non-`stack` generator is not \
                         implemented in τ's SparkSQL path: got {other:?}"
                    ),
                );
            }
        };
        if stack_args.is_empty() {
            return Err(AnalyzerError::Other {
                reason: "`stack` requires at least one argument (row count N)".to_owned(),
            });
        }
        // stack_args[0] is the row-count literal N; validate it's a positive
        // integer and that the remaining k*N value slots line up with the
        // alias count.
        let n = match &stack_args[0] {
            Expression::Literal(Literal {
                value: LiteralValue::Int(i),
                ..
            }) if *i >= 1 => *i as usize,
            Expression::Literal(Literal {
                value: LiteralValue::Long(i),
                ..
            }) if *i >= 1 => *i as usize,
            other => {
                return Err(AnalyzerError::Other {
                    reason: format!(
                        "`stack`'s first argument must be a positive integer literal, got {other:?}"
                    ),
                });
            }
        };
        let values = &stack_args[1..];
        if values.len() != n * k {
            return Err(AnalyzerError::Other {
                reason: format!(
                    "`stack({n}, ...)` with a {k}-alias tail requires {} value arguments, got {}",
                    n * k,
                    values.len()
                ),
            });
        }
        // For column i in [0, k), gather values at positions [i, k+i, 2k+i,
        // ..., (n-1)k+i] — Spark's `stack` is row-major, so the i-th column
        // spans one slot per row.
        let mut expanded = Vec::with_capacity(k);
        for (i, alias) in aliases.into_iter().enumerate() {
            let mut col_vals: Vec<Expression> = Vec::with_capacity(n);
            for row in 0..n {
                col_vals.push(values[row * k + i].clone());
            }
            expanded.push(aliased_call("stack_col", col_vals, alias));
        }
        Ok(Some(expanded))
    })
}

// ── Lateral Column Alias (LCA) — pr-007 ─────────────────────────────────────
//
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
            if entry_name.eq_ignore_ascii_case(name) {
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
/// Called by `analyze_node`'s `CommonOp::Project` arm AFTER
/// [`expand_stack_projections`] (last among the pre-passes — Spark's own LCA
/// rule runs over the fully-expanded project list too) and BEFORE
/// `ResolveContext::of_input` / `resolve_and_stamp`, so downstream resolution
/// never has to know LCA exists — every item it sees is already a plain,
/// fully-inlined tree.
fn expand_lateral_column_aliases(
    projections: Vec<Expression>,
    input_schema: &StructType,
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
/// Mirrors `resolve_and_stamp`'s own opaque-arm list: `Lambda`,
/// `LambdaVariable`, `RawSql`, `Interval`, and `UnresolvedRegex` all pass
/// through unchanged. A lambda's own bound parameter must never be mistaken
/// for an outer lateral alias reference (e.g. `SELECT 1 AS x, transform(arr,
/// x -> x + 1) FROM t` — the lambda's `x` is its own bound variable, not the
/// outer alias). `InSubquery`/`ExistsSubquery`/`ScalarSubquery` are not
/// custom-cased here; `expression_children!` already yields no children for
/// the two Exists/Scalar forms and only the outer probe expression for
/// `InSubquery`, so the default `map_children` arm below reproduces the same
/// opacity `resolve_and_stamp` gets from its explicit arm.
fn substitute_lateral_aliases(
    expr: Expression,
    table: &LateralAliasTable,
) -> Result<Expression, AnalyzerError> {
    match expr {
        Expression::UnresolvedColumn(ref u) if u.qualifier.is_none() => {
            let name = u.name.clone();
            match table.lookup(&name)? {
                Some(replacement) => Ok(replacement.clone()),
                None => Ok(expr),
            }
        }
        Expression::Lambda(_)
        | Expression::LambdaVariable(_)
        | Expression::RawSql(_)
        | Expression::Interval(_)
        | Expression::UnresolvedRegex(_) => Ok(expr),
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
/// (ADR-005; Spark 4.1.1 `range`). `explode`/`explode_outer` over a resolved
/// `Array` argument (exactly 1 arg) resolves to a single `col` column typed/
/// nulled via the shared `Expression::data_type`/`nullable` arms (same logic
/// SELECT-position explode already uses — not duplicated here). Any other TVF,
/// `range` with the wrong arity, or `explode` over a non-`Array` argument is an
/// honest Thunderduck boundary (`PuntedOperator`, ADR-022).
fn analyze_table_function(
    name: String,
    args: Vec<Expression>,
    with_ordinality: bool,
    base_types: &BaseTypes,
) -> Result<TypedAst, AnalyzerError> {
    let empty_schema = StructType::empty();
    let resolved_args = resolve_expr_list(args, &ResolveContext::bare(&empty_schema, base_types))?;
    let name_lower = name.to_ascii_lowercase();
    match name_lower.as_str() {
        "range" if (1..=4).contains(&resolved_args.len()) => Ok(TypedAst::new(
            TypedOp::TableFunction {
                name,
                args: resolved_args,
                with_ordinality,
            },
            StructType::new(vec![StructField::new("id", DataType::Long, false)]),
        )),
        // Bare `FROM explode(array(1,2,3))` — uncorrelated generator as a TVF.
        // Derive the output schema from the resolved arg's element type via the
        // existing single-homed `.data_type()` / `.nullable()` arms (type_inference
        // + expression.rs). Spark's default output column is named `"col"`.
        "explode" | "explode_outer" if resolved_args.len() == 1 => {
            // Gate: the argument must resolve to Array — Map args and non-collection
            // types fall through to PuntedOperator (no witness, ADR-022).
            let arg = &resolved_args[0];
            let arg_type = arg.data_type(&empty_schema);
            if !matches!(arg_type, DataType::Array(..)) {
                return Err(AnalyzerError::PuntedOperator {
                    op: format!("TableFunction[{name}]"),
                    reason: format!(
                        "bare {name_lower} with non-Array argument type ({arg_type:?}) \
                         not implemented in τ"
                    ),
                });
            }
            // Build the canonical FunctionCall shape to derive element type and
            // nullability via the existing single-homed arms in type_inference.rs
            // and expression.rs (no duplication).
            let fc_expr = Expression::FunctionCall(FunctionCall {
                name: name_lower.clone(),
                args: resolved_args.clone(),
                distinct: false,
            });
            let elem_type = fc_expr.data_type(&empty_schema);
            let nullable = fc_expr.nullable(&empty_schema);
            Ok(TypedAst::new(
                TypedOp::TableFunction {
                    name: name_lower,
                    args: resolved_args,
                    with_ordinality,
                },
                StructType::new(vec![StructField::new("col", elem_type, nullable)]),
            ))
        }
        _ => Err(AnalyzerError::PuntedOperator {
            op: format!("TableFunction[{name}]"),
            reason: "table-function analysis (not implemented in τ)".to_owned(),
        }),
    }
}

fn resolve_and_stamp(expr: Expression, ctx: &ResolveContext) -> Result<Expression, AnalyzerError> {
    match expr {
        Expression::UnresolvedColumn(u) => resolve_column(u, ctx),
        Expression::ColumnReference(c) => {
            let stamped = stamp_column_reference(c, ctx.schema);
            Ok(Expression::ColumnReference(stamped))
        }
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
        // τ's analyzer leaves these opaque. Lambda bodies close over lambda
        // variables and are analyzed lazily by their consumer function; RawSql
        // and Interval carry their own types. Pass 85's
        // `expand_regex_projections` rewrites any UnresolvedRegex before it
        // reaches this walker, so residuals are passed through opaquely
        // (emission's defensive arm surfaces the error).
        Expression::Lambda(_)
        | Expression::LambdaVariable(_)
        | Expression::RawSql(_)
        | Expression::Interval(_)
        | Expression::UnresolvedRegex(_) => Ok(expr),
        // UpdateFields: recurse via the walker, then run Spark 4.1's
        // `dropFields("X")` existence validation. See Catalyst
        // `UpdateFields.scala::checkInputDataTypes`.
        Expression::UpdateFields(_) => {
            let recursed = expr.map_children(|e| resolve_and_stamp(e, ctx))?;
            if let Expression::UpdateFields(ref u) = recursed {
                if let DataType::Struct(base_st) = u.struct_expr.data_type(ctx.schema) {
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
            }
            Ok(recursed)
        }
        // Default recursion: walk every immediate child via the shared
        // walker. Covers Binary / Unary / FunctionCall / Cast / CaseWhen /
        // Window / Alias / Between / InList / Like / IsDistinctFrom /
        // ExtractValue / ArrayLiteral / MapLiteral / StructLiteral /
        // RowConstructor plus the leaf variants (Literal, Star) which return
        // themselves unchanged inside `map_children`.
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
        return Err(AnalyzerError::Other {
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

/// Walk an expression in place and rewrite `UnresolvedColumn { qualifier:
/// None, plan_id: Some(N) }` to carry `qualifier: Some("__td_jl")` or
/// `Some("__td_jr")` based on which side `N` belongs to. Leaves
/// qualifier-set references and plan_id-free references untouched.
fn qualify_plan_id_refs(expr: &mut Expression, left_ids: &[i64], right_ids: &[i64]) {
    match expr {
        Expression::UnresolvedColumn(u) if u.qualifier.is_none() => {
            let synth = match u.plan_id {
                Some(pid) if left_ids.contains(&pid) => Some(TD_JOIN_LEFT.to_owned()),
                Some(pid) if right_ids.contains(&pid) => Some(TD_JOIN_RIGHT.to_owned()),
                _ => None,
            };
            if let Some(q) = synth {
                u.qualifier = Some(q);
            }
        }
        other => {
            for c in other.children_mut() {
                qualify_plan_id_refs(c, left_ids, right_ids);
            }
        }
    }
}

// ── Alias-aware qualified-column resolution over joins ─────────────────────
//
// A relation alias (`d` in `... CROSS JOIN dept d`) is not a schema field —
// the merged join schema (built by `StructType::merge`, a positional
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
    schema: &'a StructType,
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
    /// [`StructType::merge`]).
    schema: &'a StructType,
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
    /// Binds both sides' qualifiers at their positional offsets, plus the
    /// synthetic `__td_jl` / `__td_jr` plan_id-derived qualifiers (whole-side
    /// ranges) that [`qualify_plan_id_refs`] may have attached.
    fn for_join_condition(
        schema: &'a StructType,
        left: &'a TypedAst,
        right: &'a TypedAst,
        base_types: &'a BaseTypes,
        outer: Option<OuterScope<'a>>,
    ) -> Self {
        let left_len = left.resolved_schema.len();
        let right_len = right.resolved_schema.len();
        let offset = |r: &std::ops::Range<usize>| r.start + left_len..r.end + left_len;
        let mut aliases = left.scope.aliases.clone();
        aliases.extend(
            right
                .scope
                .aliases
                .iter()
                .map(|(name, r)| (name.clone(), offset(r))),
        );
        let mut plan_ids = left.scope.plan_ids.clone();
        plan_ids.extend(
            right
                .scope
                .plan_ids
                .iter()
                .map(|(pid, r, side)| (*pid, offset(r), *side)),
        );
        let mut ambiguous_plan_ids = left.scope.ambiguous_plan_ids.clone();
        ambiguous_plan_ids.extend(right.scope.ambiguous_plan_ids.iter().copied());
        aliases.push((TD_JOIN_LEFT.to_owned(), 0..left_len));
        aliases.push((TD_JOIN_RIGHT.to_owned(), left_len..left_len + right_len));
        Self {
            schema,
            scopes: std::borrow::Cow::Owned(RelScope {
                aliases,
                plan_ids,
                ambiguous_plan_ids,
                // Compile fix only (ADR-023 3c): this synthetic composite
                // scope is resolution-time-only and never flows through
                // `TypedAst::new`'s `source_quals_of` stamping.
                source_quals: Vec::new(),
            }),
            base_types,
            outer,
        }
    }

    /// Resolve against a bare schema with no alias-scope structure (e.g.
    /// `Values` rows, table-valued-function args against an empty schema).
    fn bare(schema: &'a StructType, base_types: &'a BaseTypes) -> Self {
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

    /// Look up a plan_id's field range and join-side qualifier, with the
    /// same out-of-bounds guard as [`scoped_range`](Self::scoped_range).
    fn plan_id_lookup(&self, pid: i64) -> Option<(std::ops::Range<usize>, &'static str)> {
        self.scopes.lookup_plan_id(pid).filter(|(range, _)| {
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
        ordinal: None,
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
fn resolve_in_outer(u: &UnresolvedColumn, outer: OuterScope<'_>) -> Option<(DataType, bool)> {
    if let Some(q) = u.qualifier.as_deref() {
        // Struct-column precedence in the outer schema (matches resolve_column's
        // existing tier ordering).
        if let Some(info) = TypeInferenceEngine::struct_qualifier_info(&u.name, q, outer.schema) {
            return Some(info);
        }
        // Relation-alias scope in the outer context.
        let range = outer.scopes.lookup(q)?;
        // Guard against out-of-bounds (same pattern as ResolveContext::scoped_range).
        if range.end > outer.schema.fields.len() {
            return None;
        }
        TypeInferenceEngine::column_info_in(&u.name, &outer.schema.fields[range])
    } else {
        // Unqualified: exactly-one case-insensitive match in the outer schema.
        let mut found: Option<(DataType, bool)> = None;
        for f in &outer.schema.fields {
            if f.name.eq_ignore_ascii_case(&u.name) {
                if found.is_some() {
                    // 2+ matches — ambiguous; do not silently pick one.
                    return None;
                }
                found = Some((f.data_type.clone(), f.nullable));
            }
        }
        found
    }
}

/// ADR-023 tier 3: 0-based position of the field that resolves `name` within
/// `fields` (first case-insensitive match), mirroring
/// [`TypeInferenceEngine::column_info_in`]'s own lookup order — exact name
/// first, then the struct-qualified first segment for a dotted name — so the
/// stamped ordinal always agrees with the value actually resolved.
fn field_index(fields: &[StructField], name: &str) -> Option<usize> {
    if let Some(i) = fields
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name))
    {
        return Some(i);
    }
    let dot_pos = name.find('.')?;
    let struct_name = &name[..dot_pos];
    fields
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(struct_name))
}

fn resolve_column(u: UnresolvedColumn, ctx: &ResolveContext) -> Result<Expression, AnalyzerError> {
    // Multi-level nested-struct navigation: `F.col("address.geo.lat")` arrives
    // here as `UnresolvedColumn { qualifier: Some("address"), name: "geo.lat" }`
    // (the Spark Connect converter does a single `splitn(2, '.')`). Emitting
    // this ColumnReference verbatim produces `"address"."geo.lat"` which DuckDB
    // rejects because it treats `geo.lat` as a single field key. When the
    // qualifier matches a top-level struct column and the tail is a valid
    // nested field path, rewrite as an `ExtractValue` chain so emission goes
    // through the struct-field access path.
    if let Some(chain) = try_rewrite_nested_struct_path(&u, ctx.schema) {
        return Ok(chain);
    }
    // Unqualified: surface `AmbiguousColumn` whenever more than one field
    // (case-insensitive match, matching `field_by_name`'s Spark-compatible
    // rule) resolves. This is the single, central ambiguity check point —
    // it catches ambiguity everywhere a column reference is resolved
    // (projections, filters, sort keys, join conditions, ...), not just in
    // join conditions.
    // Synthetic __td_jl / __td_jr qualifiers set by `qualify_plan_id_refs`
    // are "trust the caller" markers for a join condition. Emission renders
    // them verbatim, so nullability/type must be resolved against the
    // correct SIDE via `ctx.scopes` (populated by
    // `ResolveContext::for_join_condition` with whole-side ranges). A miss
    // (`scoped_range` None, or name absent from the side) is now a clean
    // `UnknownColumn` — a user-typed reserved qualifier `__td_jl`/`__td_jr`
    // outside a join condition is rejected (Spark parity:
    // UNRESOLVED_COLUMN), not silently resolved by name (F13).
    let is_synthetic_join_qualifier = matches!(
        u.qualifier.as_deref(),
        Some(TD_JOIN_LEFT) | Some(TD_JOIN_RIGHT)
    );
    // ── plan_id-scoped resolution (above-join disambiguation) ────────
    // When the ref is unqualified but carries a plan_id that maps to a
    // known join side, restrict resolution to that side's field range.
    // This is the above-join analogue of `qualify_plan_id_refs` (which
    // only handles join CONDITIONS). Fall through to legacy behavior
    // when the plan_id is unknown (e.g. ref to a non-join child —
    // Spark tolerates refs resolvable unambiguously without it).
    if u.qualifier.is_none() {
        if let Some(pid) = u.plan_id {
            // ADR-023 3b-i: a plan_id bound on BOTH sides of the SAME join
            // (the un-realiased self-join `df.join(df, ...)`) is genuinely
            // ambiguous — Spark itself cannot tell which side is meant.
            // Checked BEFORE `plan_id_lookup`'s first-match so we raise
            // `AmbiguousColumn` instead of silently binding the left side.
            if ctx.scopes.plan_id_is_ambiguous(pid) {
                return Err(AnalyzerError::AmbiguousColumn {
                    name: u.name.clone(),
                    candidates: vec![
                        format!("{TD_JOIN_LEFT}.{}", u.name),
                        format!("{TD_JOIN_RIGHT}.{}", u.name),
                    ],
                });
            }
            if let Some((range, side_qualifier)) = ctx.plan_id_lookup(pid) {
                let info =
                    TypeInferenceEngine::column_info_in(&u.name, &ctx.schema.fields[range.clone()]);
                if let Some((dt, nullable)) = info {
                    let ordinal = field_index(&ctx.schema.fields[range.clone()], &u.name)
                        .map(|i| range.start + i);
                    // Only stamp the join-side qualifier when the column
                    // name is AMBIGUOUS in the full schema (appears on
                    // both sides of the join). Unambiguous names must NOT
                    // be qualified — they resolve by name in any FROM
                    // scope, and every stamped synthetic qualifier obliges
                    // emission (via the `mark_join_alias_requirements`
                    // flags) to pin that join side under its `__td_jl` /
                    // `__td_jr` alias instead of hoisting the user's.
                    let is_ambiguous = ctx
                        .schema
                        .fields
                        .iter()
                        .filter(|f| f.name.eq_ignore_ascii_case(&u.name))
                        .count()
                        > 1;
                    return Ok(Expression::ColumnReference(ColumnReference {
                        name: u.name,
                        qualifier: if is_ambiguous {
                            Some(side_qualifier.to_owned())
                        } else {
                            None
                        },
                        data_type: Some(dt),
                        nullable: Some(nullable),
                        ordinal,
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
        let matches: Vec<&StructField> = ctx
            .schema
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
    let (dt, nullable, ordinal) = if is_synthetic_join_qualifier {
        let q = u.qualifier.as_deref().unwrap_or_default();
        // `scoped_range` mirrors tier (e) below: `q` spells a reserved
        // synthetic qualifier (`__td_jl`/`__td_jr`), but the analyzer only
        // ever binds a scope for it inside a join condition, via
        // `ResolveContext::for_join_condition`. Both misses below therefore
        // mean the same thing — an untrusted, user-typed reference to a
        // reserved qualifier the analyzer never stamped a scope for — so
        // both reject with `UnknownColumn` (Spark itself raises
        // `UNRESOLVED_COLUMN` for e.g. `col("__td_jl.x")`) rather than
        // falling back to a permissive name-only lookup that would later
        // trip `mark_node`'s join-side bookkeeping on a column the join
        // never introduced (F13).
        match ctx.scoped_range(q) {
            Some(range) => {
                match TypeInferenceEngine::column_info_in(
                    &u.name,
                    &ctx.schema.fields[range.clone()],
                ) {
                    Some((dt, nullable)) => {
                        let ordinal = field_index(&ctx.schema.fields[range.clone()], &u.name)
                            .map(|i| range.start + i);
                        (dt, nullable, ordinal)
                    }
                    // `q` is a real synthetic side scope but `name` is not on it.
                    None => {
                        return Err(AnalyzerError::UnknownColumn {
                            name: u.name.clone(),
                            qualifier: u.qualifier.clone(),
                        });
                    }
                }
            }
            // `q` is `__td_jl`/`__td_jr` by spelling but binds no synthetic
            // side scope — a user-typed reserved qualifier outside a join
            // condition.
            None => {
                return Err(AnalyzerError::UnknownColumn {
                    name: u.name.clone(),
                    qualifier: u.qualifier.clone(),
                });
            }
        }
    } else if let Some(q) = u.qualifier.as_deref() {
        // Qualified, non-synthetic: (d) a qualifier naming a top-level STRUCT
        // column wins over a relation-alias scope — struct-field access takes
        // precedence, matching the pre-existing behavior this pass preserves.
        // The resolved field lives inside the struct, not at a top-level
        // position in `ctx.schema` — no ordinal to stamp.
        if let Some((dt, nullable)) =
            TypeInferenceEngine::struct_qualifier_info(&u.name, q, ctx.schema)
        {
            (dt, nullable, None)
        } else {
            match ctx.scoped_range(q) {
                // (e) qualifier binds exactly one in-bounds relation scope —
                // restrict the name-only lookup to that relation's own
                // fields. A miss here means `q` is a real, unambiguous
                // relation but `name` does not exist on it: a Spark-emulated
                // `UnknownColumn` (ADR-022 cat-1), not an opaque DuckDB bind
                // error.
                Some(range) => {
                    match TypeInferenceEngine::column_info_in(
                        &u.name,
                        &ctx.schema.fields[range.clone()],
                    ) {
                        Some((dt, nullable)) => {
                            let ordinal = field_index(&ctx.schema.fields[range.clone()], &u.name)
                                .map(|i| range.start + i);
                            (dt, nullable, ordinal)
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
                // scope at all (e.g. USING joins stay on the legacy path —
                // `collect_qualifier_bindings` STOPs there). ADR-023 3b-i:
                // distinguish them via `lookup_all` — 2+ is genuinely
                // ambiguous (Spark: AMBIGUOUS_REFERENCE) and must not fall
                // through to the permissive legacy name-only lookup; 0
                // keeps the existing legacy fallback unchanged.
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
                    let (dt, nullable) =
                        TypeInferenceEngine::qualified_column_info(&u.name, Some(q), ctx.schema);
                    let ordinal = field_index(&ctx.schema.fields, &u.name);
                    (dt, nullable, ordinal)
                }
            }
        }
    } else {
        let (dt, nullable) = TypeInferenceEngine::qualified_column_info(&u.name, None, ctx.schema);
        let ordinal = field_index(&ctx.schema.fields, &u.name);
        (dt, nullable, ordinal)
    };
    if matches!(dt, DataType::Unresolved) {
        // (g) Outer-scope fallback: when ALL inner tiers have failed and an
        // enclosing plan's scope is available (inside a subquery), attempt to
        // resolve the reference against the outer plan's schema. On hit, stamp
        // the ColumnReference with the outer type/nullability and preserve the
        // qualifier verbatim — emission renders it as-is, and DuckDB's own
        // correlated-subquery binder resolves it at runtime (same mechanism
        // the 13 existing green correlated subquery cases ride).
        if let Some(outer) = ctx.outer {
            if let Some((outer_dt, outer_nullable)) = resolve_in_outer(&u, outer) {
                return Ok(Expression::ColumnReference(ColumnReference {
                    name: u.name,
                    qualifier: u.qualifier,
                    data_type: Some(outer_dt),
                    nullable: Some(outer_nullable),
                    // (g) The column is not in `ctx.schema` — its position
                    // lives in an enclosing scope; correlated ordinal
                    // handling is a later concern.
                    ordinal: None,
                }));
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
        data_type: Some(dt),
        nullable: Some(nullable),
        ordinal,
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
        data_type: Some(dt),
        nullable: Some(nullable),
        ..c
    }
}

/// Return `true` iff any `Expression::UnresolvedColumn` remains, or any
/// `ColumnReference` has `data_type = None` / `nullable = None`.
fn expression_is_fully_resolved(expr: &Expression) -> bool {
    match expr {
        Expression::UnresolvedColumn(_) => false,
        // Pass 85 — pattern-driven column expander; expanded away by
        // `expand_regex_projections` in the `CommonOp::Project` pre-pass.
        // If it survives to this check, treat it as unresolved.
        Expression::UnresolvedRegex(_) => false,
        Expression::ColumnReference(c) => c.data_type.is_some() && c.nullable.is_some(),
        // Subquery bodies must be analyzed: the inner plan is fully resolved
        // only once the analyzer has rewritten `Unanalyzed` → `Analyzed`.
        Expression::ScalarSubquery(s) => subquery_plan_is_resolved(&s.subquery),
        Expression::InSubquery(i) => {
            expression_is_fully_resolved(&i.expr) && subquery_plan_is_resolved(&i.subquery)
        }
        Expression::ExistsSubquery(e) => subquery_plan_is_resolved(&e.subquery),
        // Lambda / raw-sql / interval bodies: opaque by τ's walker convention.
        Expression::Lambda(_) | Expression::RawSql(_) | Expression::Interval(_) => true,
        // Default recursion: all-children-resolved implies self-resolved. See
        // [`Expression::children`] for the walked-child convention. Covers the
        // leaf variants (Literal, Star, LambdaVariable → no children → true)
        // and every structural node (Binary, Window, CaseWhen, …).
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

// ── Schema / expression naming ──────────────────────────────────────────────

fn schema_has_unresolved(schema: &StructType) -> bool {
    schema
        .fields
        .iter()
        .any(|f| f.data_type.contains_unresolved())
}

/// Build the output [`StructField`] contributed by a projection-position
/// expression: name via [`expression_output_name`], type and nullability
/// resolved against `schema`.
fn expr_field(e: &Expression, schema: &StructType) -> StructField {
    StructField::new(
        expression_output_name(e),
        e.data_type(schema),
        e.nullable(schema),
    )
}

fn project_output_schema(
    projections: &[Expression],
    input: &TypedAst,
) -> Result<StructType, AnalyzerError> {
    let input_schema = &input.resolved_schema;
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
                        // matches).
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
            other => fields.push(expr_field(other, input_schema)),
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
    outer: Option<OuterScope<'_>>,
) -> Result<TypedAst, AnalyzerError> {
    let typed_input = analyze_node(input, base_types, outer)?;
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

    // Reject any unresolvable value column with a Spark-emulated
    // `UnknownColumn`. Runs ONCE per path: the Implicit path validates
    // BEFORE deriving ids from the value set; the Explicit path validates
    // after id validation (error ordering preserved — an unresolvable id
    // must win over an unresolvable value there).
    let validate_values_resolve = |vals: &[String]| -> Result<(), AnalyzerError> {
        for v in vals {
            if find_field(v).is_none() {
                return Err(AnalyzerError::UnknownColumn {
                    name: v.clone(),
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
            .filter(|f| !excluded.contains(&f.name.to_ascii_lowercase()))
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
                return Err(AnalyzerError::Other {
                    reason: "SQL UNPIVOT requires at least one value column".to_owned(),
                });
            }
            // Validate each value column resolves before deriving ids from it.
            validate_values_resolve(&values)?;
            let value_set: HashSet<String> =
                values.iter().map(|v| v.to_ascii_lowercase()).collect();
            fields_minus(&value_set)
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
        let id_set: HashSet<String> = ids.iter().map(|id| id.to_ascii_lowercase()).collect();
        fields_minus(&id_set)
    } else {
        // Validate each named value column resolves — the Implicit path
        // already did (see above), so only the Explicit path checks here.
        if !ids_are_implicit {
            validate_values_resolve(&values)?;
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
        let mut seen: HashSet<String> =
            HashSet::with_capacity(ids.len() + materialised_values.len());
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
    Ok(TypedAst::new(
        TypedOp::FreqItems {
            input: Box::new(typed_input),
            cols: materialised,
            support,
        },
        StructType::new(output_fields),
    ))
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
    outer: Option<OuterScope<'_>>,
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
    let mut output_fields: Vec<StructField> = Vec::new();
    output_fields.extend(grouping.iter().map(|g| expr_field(g, input_schema)));

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
            output_fields.push(StructField::nullable(col_name, dt));
        }
    }
    let output_schema = StructType::new(output_fields);

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
/// The grouping key is intentionally *not* folded into the aggregates, so
/// `analyze_aggregate` prepends col0 (string, nullability from col1) ahead of
/// the count columns.
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

    let aggregates = counts
        .into_iter()
        .map(|(name, count)| {
            Expression::Alias(AliasExpression {
                expr: Box::new(count),
                alias: name,
            })
        })
        .collect();

    CommonOp::Aggregate {
        input: Box::new(input),
        grouping,
        grouping_sets: Vec::new(),
        aggregates,
        grouping_kind: GroupingKind::GroupBy,
        having: None,
    }
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
                ordinal: None,
            })
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
            acc.insert(c.name.to_ascii_lowercase());
        }
        Expression::UnresolvedColumn(u) => {
            acc.insert(u.name.to_ascii_lowercase());
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
        // Pass 60 H1: Spark's Catalyst `Literal.sql` renders integral
        // floats/doubles with a `.0` suffix ("1.0", not "1"). Match it
        // so pivot output column names match Spark exactly.
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
        _ => pretty_name(expr),
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

/// Spark `toPrettySQL`-parity default column name for an unaliased projection
/// expression (Spark `UnresolvedAlias` → `Column.named` → `toPrettySQL`).
///
/// This is deliberately distinct from both [`expression_output_name`] (which
/// names top-level `Literal`s `"col"` and `FunctionCall`s by function name) and
/// the emission `render_*` family (which uses DuckDB symbols, ANSI guards, and
/// casts). It is value-aware — a literal renders its value, not its type — and
/// recursive over the structural variants Spark inlines into a pretty name.
///
/// Variants Spark renders in a shape τ does not yet match exactly (`Cast`,
/// `CaseWhen`, windows, subqueries, complex-type literals, …) keep the
/// Thunderduck-boundary fallback name `"expr"`.
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
            format!("{}({})", f.name, args.join(", "))
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
        _ => "expr".to_owned(),
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

/// Whether the grouping keys are "already folded" into `aggregates` and so
/// must NOT be re-prepended to the output schema / SELECT list.
///
/// True iff the grouping is empty (global aggregate), OR every grouping key
/// either name-matches (case-insensitive, via [`expression_output_name`]) or
/// structurally equals (alias-stripped) some entry in `aggregates`. This is
/// the single source of truth shared by the analyzer's resolved-schema
/// construction and emission's SELECT-list rendering — they MUST agree, or the
/// wire schema desyncs from the emitted column count.
pub(super) fn grouping_already_folded(grouping: &[Expression], aggregates: &[Expression]) -> bool {
    grouping.is_empty() || {
        let agg_names: Vec<String> = aggregates.iter().map(expression_output_name).collect();
        grouping.iter().all(|gk| {
            let name = expression_output_name(gk);
            let name_match = agg_names.iter().any(|an| an.eq_ignore_ascii_case(&name));
            let bare = gk.unaliased();
            let struct_match = aggregates.iter().any(|agg| agg.unaliased() == bare);
            name_match || struct_match
        })
    }
}

// ── Set-op widening (§5) ─────────────────────────────────────────────────────

fn push_setop_casts(ast: &mut TypedAst, widened_schema: &StructType) {
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
        ast.resolved_schema = widened_schema.clone();
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
        }),
        None => inner,
    };
    *expr = Expression::Alias(AliasExpression {
        expr: Box::new(valued),
        alias: name.to_owned(),
    });
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

/// Copy `schema` with every field forced nullable — outer-join padding
/// semantics. `pub(super)` so `analyzer_fixtures` builds its expected
/// join schemas from the same helper.
pub(super) fn flip_all_nullable(schema: &StructType) -> StructType {
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
    if let Some(bad) = rows.iter().find(|r| r.len() != ncols) {
        // Ragged VALUES rows (e.g. `VALUES (1,2),(3)`) would otherwise index
        // out of bounds below on the shorter rows — a session-killing panic.
        // Spark rejects inconsistent-length VALUES with an AnalysisException.
        return Err(AnalyzerError::Other {
            reason: format!(
                "VALUES rows have inconsistent lengths: expected {ncols}, got {}",
                bad.len()
            ),
        });
    }
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::analyzer_fixtures;
    use super::super::ast::CommonAst;
    use super::super::expression::{
        AliasExpression, BetweenExpression, BinaryExpression, BinaryOp, ExistsSubquery,
        FunctionCall, InSubquery, LambdaExpression, LambdaVariableExpression, Literal,
        LiteralValue, ScalarSubquery, StarExpression, UnaryExpression, UnaryOp,
        UnresolvedRegexExpression,
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

    // ── shared expression / plan constructors ────────────────────────────

    /// Bare `TableScan` over `name` (no alias).
    fn scan(name: &str) -> CommonAst {
        CommonAst::new(CommonOp::TableScan {
            table: name.to_owned(),
            alias: None,
        })
    }

    fn emp_scan() -> CommonAst {
        scan("emp")
    }

    /// Unqualified unresolved column (no plan id).
    fn unresolved_col(name: &str) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: None,
        })
    }

    /// Qualifier-scoped unresolved column (`qualifier.name`, no plan id).
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

    /// Non-distinct function call.
    fn func(name: &str, args: Vec<Expression>) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: name.to_owned(),
            args,
            distinct: false,
        })
    }

    /// `expr AS alias`.
    fn alias_expr(expr: Expression, alias: &str) -> Expression {
        Expression::Alias(AliasExpression {
            expr: Box::new(expr),
            alias: alias.to_owned(),
        })
    }

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

    // ── shared op builders (invariant default fields filled in) ──────────

    /// `Join` with `using_columns` / `left_plan_ids` / `right_plan_ids` empty.
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

    /// Positional `SetOp` (`by_name = false`, `allow_missing_columns = false`).
    fn set_op(kind: SetOpKind, all: bool, children: Vec<CommonAst>) -> CommonAst {
        CommonAst::new(CommonOp::SetOp {
            kind,
            all,
            by_name: false,
            allow_missing_columns: false,
            children,
        })
    }

    /// By-name `SetOp` (`allow_missing_columns = false`).
    fn set_op_by_name(kind: SetOpKind, all: bool, children: Vec<CommonAst>) -> CommonAst {
        CommonAst::new(CommonOp::SetOp {
            kind,
            all,
            by_name: true,
            allow_missing_columns: false,
            children,
        })
    }

    /// `unionByName(allowMissingColumns=True)` shape.
    fn union_by_name_allow_missing(children: Vec<CommonAst>) -> CommonAst {
        CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: true,
            children,
        })
    }

    /// `Aggregate` with plain `GROUP BY` (no grouping sets) and the given
    /// HAVING clause.
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

    /// `Aggregate` with plain `GROUP BY`, no grouping sets, no HAVING.
    fn aggregate(
        input: CommonAst,
        grouping: Vec<Expression>,
        aggregates: Vec<Expression>,
    ) -> CommonAst {
        aggregate_having(input, grouping, aggregates, None)
    }

    // ── shared BaseTypes builders ─────────────────────────────────────────

    /// Overlay with the given `table → schema` entries pre-resolved.
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

    // ── RelScope stamping ─────────────────────────────────────────────────
    // Direct parity tests for the stamped scope (formerly the recursive
    // `collect_qualifier_bindings` walk): binding rules per operator class.

    /// `TableScan` over `table` with an explicit alias.
    fn aliased_scan(table: &str, alias: &str) -> CommonAst {
        CommonAst::new(CommonOp::TableScan {
            table: table.to_owned(),
            alias: Some(alias.to_owned()),
        })
    }

    #[test]
    fn rel_scope_table_scan_binds_table_and_alias() {
        let bt = base_types_with_emp_dept();
        let typed = analyze(aliased_scan("emp", "e"), &bt).unwrap();
        assert_eq!(
            typed.scope.aliases,
            vec![("emp".to_owned(), 0..4), ("e".to_owned(), 0..4)]
        );
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
        // Child's `emp` binding is dropped; only the alias is exposed.
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
            vec![
                ("emp".to_owned(), 0..4),
                ("e".to_owned(), 0..4),
                ("dept".to_owned(), 4..6),
                ("d".to_owned(), 4..6),
            ]
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
        assert_eq!(
            typed.scope.aliases,
            vec![("emp".to_owned(), 0..4), ("e".to_owned(), 0..4)]
        );
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
        // Filter passes the join's bindings through unchanged.
        let inner_scope = match &typed.op {
            TypedOp::Filter { input, .. } => input.scope.clone(),
            other => panic!("expected Filter, got {other:?}"),
        };
        assert_eq!(typed.scope, inner_scope);
        assert_eq!(typed.scope.aliases.len(), 4);
    }

    #[test]
    fn rel_scope_join_plan_ids_outermost_first() {
        let bt = base_types_for(&[
            ("emp", emp_schema()),
            ("dept", dept_schema()),
            ("bonus", dept_schema()),
        ]);
        // inner join (plan_ids 1|2), then outer join (plan_ids 1|3): the
        // OUTER join's entry for pid 1 must precede the inner join's, so
        // first-match resolution picks the nearest enclosing join's side.
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
            .filter(|(pid, _, _)| *pid == 1)
            .collect();
        assert_eq!(pid1_entries.len(), 2);
        // Outermost entry first: whole left side of the OUTER join (0..6).
        assert_eq!(pid1_entries[0].1, 0..6);
        assert_eq!(pid1_entries[0].2, TD_JOIN_LEFT);
        // Inner join's entry (0..4) follows.
        assert_eq!(pid1_entries[1].1, 0..4);
    }

    #[test]
    fn rel_scope_lateral_view_appends_generated_range() {
        let bt = base_types_with_emp_dept();
        let input = analyze(aliased_scan("emp", "e"), &bt).unwrap();
        let columns = vec![("tag".to_owned(), lit_str("x"))];
        let merged = StructType::merge(
            &input.resolved_schema,
            &StructType::new(vec![StructField::nullable("tag", DataType::String)]),
        );
        let typed = TypedAst::new(
            TypedOp::LateralView {
                input: Box::new(input),
                table_alias: "t".to_owned(),
                columns,
            },
            merged,
        );
        assert_eq!(
            typed.scope.aliases,
            vec![
                ("emp".to_owned(), 0..4),
                ("e".to_owned(), 0..4),
                ("t".to_owned(), 4..5),
            ]
        );
    }

    // ── mark_join_alias_requirements ──────────────────────────────────────

    /// Extract a Join's stamped synthetic-alias flags.
    fn join_flags(typed: &TypedAst) -> (bool, bool) {
        match &typed.op {
            TypedOp::Join {
                left_requires_synthetic,
                right_requires_synthetic,
                ..
            } => (*left_requires_synthetic, *right_requires_synthetic),
            other => panic!("expected Join, got {other:?}"),
        }
    }

    /// Plan-id-tagged unresolved column (DataFrame disambiguation shape).
    fn pcol(name: &str, plan_id: i64) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: Some(plan_id),
        })
    }

    /// `emp` (plan_id 1) ⋈ `dept` (plan_id 2) with the given condition.
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

    #[test]
    fn join_flags_set_when_condition_carries_plan_id_ambiguity() {
        let bt = base_types_with_emp_dept();
        // `dept_id` exists on both sides — the plan_id-stamped refs become
        // `__td_jl.dept_id` / `__td_jr.dept_id`, demanding synthetic aliases.
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(pcol("dept_id", 1)),
            right: Box::new(pcol("dept_id", 2)),
        });
        let typed = analyze(plan_id_join(Some(cond)), &bt).unwrap();
        assert_eq!(join_flags(&typed), (true, true));
    }

    #[test]
    fn join_flags_clear_for_user_qualified_condition() {
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
        assert_eq!(join_flags(&typed), (false, false));
    }

    #[test]
    fn join_flags_propagate_from_ancestor_through_passthrough() {
        let bt = base_types_with_emp_dept();
        // Table-name-qualified condition — resolves via the qualifier scope,
        // so the condition itself stamps no synthetic names…
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("emp", "dept_id")),
            right: Box::new(qcol("dept", "dept_id")),
        });
        // …but the Project above (through a Filter passthrough) references
        // the ambiguous `dept_id` with a plan_id → `__td_jl.dept_id` demand.
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
        // Walk Project → Filter → Join and check the stamped flags.
        let TypedOp::Project { input, .. } = &typed.op else {
            panic!("expected Project");
        };
        let TypedOp::Filter { input, .. } = &input.op else {
            panic!("expected Filter");
        };
        let (left_flag, right_flag) = join_flags(input);
        assert!(
            left_flag,
            "ancestor __td_jl demand must reach the join through the Filter"
        );
        // The condition resolved via table-name qualifiers (no synthetic
        // stamps), and no ancestor demands the right side.
        assert!(!right_flag);
    }

    #[test]
    fn join_flags_do_not_leak_into_nested_joins() {
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
        let typed = analyze(outer, &bt).unwrap();
        // Outer: pid-7 `dept_id` is ambiguous across the merged schema
        // (emp.dept_id, dept.dept_id, bonus.dept_id) → __td_jl stamped.
        let (outer_left, _) = join_flags(&typed);
        assert!(outer_left);
        // Inner join must stay unmarked — synthetic names are per-level.
        let TypedOp::Join { left, .. } = &typed.op else {
            panic!("expected Join");
        };
        assert_eq!(join_flags(left), (false, false));
    }

    // ── shared TypedAst extractors ────────────────────────────────────────

    /// Output-schema field names, in order.
    fn field_names(typed: &TypedAst) -> Vec<&str> {
        typed
            .resolved_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect()
    }

    /// The widened schema of a `SetOp` node.
    fn widened_of(typed: &TypedAst) -> &StructType {
        match &typed.op {
            TypedOp::SetOp { widened_schema, .. } => widened_schema,
            other => panic!("expected SetOp, got {other:?}"),
        }
    }

    /// The resolved grouping column names of a `Pivot` node.
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

    // ── resolve pass ──────────────────────────────────────────────────────

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

    // ── Pass 126 — table-qualified star (`t.*` / `alias.*`) ──────────────

    #[test]
    fn qualified_star_over_table_scan_expands_to_full_schema() {
        let bt = base_types_with_emp_dept();
        // SELECT emp.* FROM emp
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
        // SELECT e.* FROM emp e LEFT SEMI JOIN dept d ON ...
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
        // Semi join yields only the left relation's columns; `e.*` binds them.
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn qualified_star_with_unknown_qualifier_still_rejects() {
        let bt = base_types_with_emp_dept();
        // SELECT bogus.* FROM emp — qualifier binds no in-scope relation.
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some("bogus".to_owned()),
            })],
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    /// `q.*` projection under the given alias, over `input`.
    fn qstar_project(input: CommonAst, q: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(input),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some(q.to_owned()),
            })],
        })
    }

    /// `emp e INNER JOIN dept d ON e.dept_id = d.dept_id`.
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
        // SELECT e.* FROM emp e JOIN dept d ON … — left side's columns only.
        let typed = analyze(qstar_project(emp_dept_aliased_join(), "e"), &bt)
            .expect("analyze e.* over join");
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    #[test]
    fn qualified_star_over_plain_join_expands_right_range() {
        let bt = base_types_with_emp_dept();
        // SELECT d.* — right side's columns, at the offset range.
        let typed = analyze(qstar_project(emp_dept_aliased_join(), "d"), &bt)
            .expect("analyze d.* over join");
        assert_eq!(typed.resolved_schema, dept_schema());
    }

    #[test]
    fn qualified_star_resolves_through_scope_passthrough() {
        let bt = base_types_with_emp_dept();
        // SELECT e.* FROM (emp e JOIN dept d ON …) WHERE d.dept_id > 0 —
        // Filter is scope-passthrough, so the join's bindings survive.
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
        // SELECT d.* FROM emp e LEFT OUTER JOIN dept d ON … — the right
        // side's fields are null-extended; `d.*` must carry the flip.
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
        // USING joins have an empty RelScope (reorder/dedup breaks the
        // contiguous-range invariant) — q.* stays a clean UnknownColumn.
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
        // Both sides aliased `x` — `x.*` is ambiguous by construction.
        let dup_join = join(
            aliased_scan("emp", "x"),
            aliased_scan("dept", "x"),
            JoinType::Cross,
            None,
        );
        let err = analyze(qstar_project(dup_join, "x"), &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::UnknownColumn { .. }));
    }

    // ── ADR-023 tier 3a — resolved `ordinal` on ColumnReference ──────────
    // Additive, dormant field: `resolve_column` stamps the 0-based position
    // of the resolved column within the producing node's `ctx.schema`.
    // Emission ignores it (dormant until a later ADR-023 sub-chunk); these
    // tests pin the stamped value against the field's actual index in the
    // resolved schema.

    #[test]
    fn resolve_column_ordinal_unqualified_matches_schema_index() {
        let bt = base_types_with_emp_dept();
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![unresolved_col("salary")],
        });
        let typed = analyze(project, &bt).expect("unqualified salary must resolve");
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.name, "salary");
                    // `salary` is the 4th (0-based index 3) field of `emp_schema`.
                    assert_eq!(c.ordinal, Some(3));
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn resolve_column_ordinal_qualified_tier_e_matches_schema_index() {
        let bt = base_types_with_emp_dept();
        // SELECT d.dept_name FROM emp e JOIN dept d ON e.dept_id = d.dept_id
        // — tier (e): qualifier `d` binds the join's right-side scope range
        // (dept fields start at absolute index 4 in the merged schema).
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_dept_aliased_join()),
            projections: vec![qcol("d", "dept_name")],
        });
        let typed = analyze(project, &bt).expect("d.dept_name must resolve");
        match &typed.op {
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.name, "dept_name");
                    // `dept_name` is dept's 2nd field (offset 1), and dept
                    // starts at absolute index 4 in the merged join schema.
                    assert_eq!(c.ordinal, Some(5));
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn resolve_column_ordinal_plan_id_over_join_matches_correct_side() {
        // Self-join `emp(plan_id=1) JOIN emp(plan_id=2)`; a Project above
        // selecting `id` tagged plan_id=2 must resolve to the RIGHT side's
        // `id` — absolute index 4 (0-based) in the merged 8-field schema.
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(qcol("__td_jl", "id")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("__td_jr", "id")),
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
            TypedOp::Project { projections, .. } => match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(c.qualifier.as_deref(), Some(TD_JOIN_RIGHT));
                    assert_eq!(c.ordinal, Some(4));
                }
                other => panic!("expected ColumnReference, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    // ── ADR-023 tier 3c — per-output-column source-qualifier lineage ─────
    // Additive, dormant field: `source_quals_of` stamps `scope.source_quals`
    // in `TypedAst::new`. Not yet consulted by `resolve_column` or emission
    // (that is 3d); these tests pin the derivation itself.

    #[test]
    fn source_quals_aliased_relation_binds_every_col_to_alias() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        let typed = analyze(ast, &bt).unwrap();
        let e: BTreeSet<String> = ["e".to_owned()].into_iter().collect();
        assert_eq!(typed.scope.source_quals, vec![e; 4]);
    }

    #[test]
    fn source_quals_project_passthrough_column_inherits_source() {
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        // SELECT e.dept_id, e.name FROM (emp AS e)
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased),
            projections: vec![qcol("e", "dept_id"), qcol("e", "name")],
        });
        let typed = analyze(project, &bt).unwrap();
        let e: BTreeSet<String> = ["e".to_owned()].into_iter().collect();
        assert_eq!(typed.scope.source_quals, vec![e.clone(), e]);
    }

    #[test]
    fn source_quals_project_alias_creates_empty_lineage() {
        // filt-019/F8 hinge: an Alias'd projection is a CREATED column — it
        // does NOT inherit its source's qualifier set, even though the
        // aliased expression is itself a plain passthrough column.
        let bt = base_types_with_emp_dept();
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(emp_scan()),
            alias: "e".to_owned(),
        });
        // SELECT dept_id AS k FROM (emp AS e)
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased),
            projections: vec![alias_expr(unresolved_col("dept_id"), "k")],
        });
        let typed = analyze(project, &bt).unwrap();
        assert_eq!(typed.scope.source_quals, vec![BTreeSet::new()]);
    }

    #[test]
    fn source_quals_plain_join_composes_left_and_right() {
        // emp e JOIN dept d ON e.dept_id = d.dept_id — TableScan{alias}'s
        // rule binds BOTH the table name and the alias to every column.
        let bt = base_types_with_emp_dept();
        let typed = analyze(emp_dept_aliased_join(), &bt).unwrap();
        let e: BTreeSet<String> = ["emp".to_owned(), "e".to_owned()].into_iter().collect();
        let d: BTreeSet<String> = ["dept".to_owned(), "d".to_owned()].into_iter().collect();
        let mut expected = vec![e; 4];
        expected.extend(vec![d; 2]);
        assert_eq!(typed.scope.source_quals, expected);
    }

    #[test]
    fn source_quals_aggregate_grouping_col_inherits_source_aggregate_col_empty() {
        // df.groupBy("dept_id").count() shape — grouping col inherits its
        // source's qualifier set, the created `count` output col does not.
        let bt = base_types_with_emp_dept();
        let ast = aggregate(
            scan("emp"),
            vec![unresolved_col("dept_id")],
            vec![func("count", vec![int_lit(1)])],
        );
        let typed = analyze(ast, &bt).unwrap();
        let emp: BTreeSet<String> = ["emp".to_owned()].into_iter().collect();
        assert_eq!(typed.scope.source_quals, vec![emp, BTreeSet::new()]);
    }

    #[test]
    fn source_quals_length_invariant_holds_for_star_projection() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(typed.scope.source_quals.len(), typed.resolved_schema.len());
        assert!(typed.scope.source_quals.iter().all(BTreeSet::is_empty));
    }

    #[test]
    fn source_quals_length_invariant_holds_for_using_join() {
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
        assert_eq!(typed.scope.source_quals.len(), typed.resolved_schema.len());
        assert!(typed.scope.source_quals.iter().all(BTreeSet::is_empty));
    }

    // ── Pass 106 — uncorrelated subquery analysis ────────────────────────

    /// Inner plan `SELECT <col> FROM emp` — a single-column subquery body.
    fn inner_select_col(col: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(emp_scan()),
            projections: vec![unresolved_col(col)],
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
            projections: vec![unresolved_col("id"), unresolved_col("salary")],
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
        // Inner references a column absent from `dept` — analyzed in isolation
        // this fails resolution (the correlated boundary, ADR-022).
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

    // ── Pass 16 — correlated subquery outer-scope fallback ─────────────

    /// Dept schema with a `budget` column (not present in emp) for sq-010.
    fn dept_schema_with_budget() -> StructType {
        StructType::new(vec![
            StructField::not_null("dept_id", DataType::Integer),
            StructField::nullable("dept_name", DataType::String),
            StructField::nullable("budget", DataType::Double),
        ])
    }

    /// sq-010 shape: correlated IN subquery where the inner WHERE references
    /// an outer column (`e.salary`) that exists ONLY in the outer table
    /// (`emp`), not in the inner table (`dept`). The analyzer must resolve
    /// `e.salary` against the outer scope and stamp it with `Double` /
    /// `nullable=true` (matching emp's `salary` field).
    #[test]
    fn correlated_in_subquery_outer_ref_absent_from_inner_resolves() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        // SELECT * FROM emp e WHERE e.dept_id IN (
        //     SELECT d.dept_id FROM dept d WHERE d.budget > e.salary
        // )
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
        // Verify the outer `e.dept_id` resolved (outer type stamped).
        assert_eq!(typed.resolved_schema, emp_schema());
    }

    /// Same absent-column correlation shape as sq-010, but using
    /// ExistsSubquery. Verifies the `analyze_subquery_plan` path threads
    /// the outer scope correctly.
    #[test]
    fn correlated_exists_subquery_outer_ref_absent_from_inner_resolves() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        // SELECT * FROM emp e WHERE EXISTS (
        //     SELECT 1 FROM dept d WHERE d.budget > e.salary
        // )
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

    /// Same absent-column correlation shape via ScalarSubquery.
    #[test]
    fn correlated_scalar_subquery_outer_ref_absent_from_inner_resolves() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        // SELECT (SELECT max(d.budget) FROM dept d WHERE d.dept_id = e.dept_id
        //         AND d.budget > e.salary) FROM emp e
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
        // Scalar subquery resolves; output schema carries the scalar column.
        assert_eq!(typed.resolved_schema.fields.len(), 1);
    }

    /// E2E emission test: sq-010 shape parsed → analyzed → dispatched.
    /// The emitted SQL must contain the qualifier `e` on `salary` verbatim
    /// inside the subquery (DuckDB's correlated-subquery binder uses it).
    #[test]
    fn correlated_in_subquery_emission_preserves_outer_qualifier() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        // Build the same sq-010 shape as above but wrapped in a Project *
        // so dispatch_op has a Project root.
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
        // The emitted SQL must contain the outer qualifier verbatim.
        assert!(
            sql.contains("e.salary"),
            "emission must preserve outer qualifier `e` on `salary`; got:\n{sql}"
        );
    }

    /// One-level-only lock: a two-level-nested correlation where the
    /// innermost subquery references the OUTERMOST alias via a column name
    /// absent from BOTH inner schemas must still fail as UnknownColumn.
    /// `OuterScope` deliberately has no `outer` field, so the grandparent's
    /// scope is unrepresentable — the innermost plan only sees its
    /// immediate parent.
    #[test]
    fn two_level_nested_correlation_to_grandparent_still_fails() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        // Innermost subquery: SELECT 1 FROM dept d2 WHERE d2.dept_id = e.salary
        // `e.salary` is from the grandparent (emp e), absent from both
        // the immediate parent (dept d1) and the innermost (dept d2).
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
        // Middle subquery: SELECT d1.dept_id FROM dept d1
        //     WHERE EXISTS (innermost)
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
        // Outer: SELECT * FROM emp e WHERE e.dept_id IN (middle)
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

    /// Inner-precedence regression: when a column name exists in BOTH
    /// the inner and outer schemas with DIFFERENT types, the inner type
    /// must win (tier (f) resolves before tier (g)). This protects the 13
    /// existing green correlated subquery cases.
    #[test]
    fn correlated_subquery_inner_type_takes_precedence_over_outer() {
        // emp.dept_id = Integer(nullable), dept.dept_id = Integer(not-null).
        // Create a variation where emp has dept_id as Long to differentiate.
        let emp_long_dept_id = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Long), // Long, not Integer
            StructField::nullable("salary", DataType::Double),
        ]);
        let bt = base_types_for(&[("emp", emp_long_dept_id), ("dept", dept_schema())]);
        // SELECT * FROM emp e WHERE e.dept_id IN (
        //     SELECT dept_id FROM dept d WHERE d.dept_id = e.dept_id
        // )
        // The inner `dept_id` unqualified reference matches dept's Integer column.
        // The inner `e.dept_id` should resolve via tier (f) (name-only fallback
        // finds `dept_id` in the inner schema) and be stamped as Integer (the
        // inner type), NOT Long (the outer type).
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(scan("dept")),
                    alias: "d".to_owned(),
                })),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(qcol("d", "dept_id")),
                    // e.dept_id: qualifier `e` not bound in inner scope → tier (f)
                    // name-only lookup finds `dept_id` in dept's schema → Integer.
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
        // Dig into the inner subquery's filter condition to check the
        // `e.dept_id` column reference's resolved type.
        // Dig: outer Filter -> InSubquery -> Analyzed(Project(Filter(...)))
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
                    // Must be Integer (inner type), NOT Long (outer).
                    assert_eq!(
                        c.data_type,
                        Some(DataType::Integer),
                        "inner type must take precedence over outer"
                    );
                }
                other => panic!("expected ColumnReference for e.dept_id, got {other:?}"),
            },
            other => panic!("expected Binary condition, got {other:?}"),
        }
    }

    /// Qualifier-strictness: a qualified outer reference whose qualifier
    /// binds NO scope in the outer context, but whose bare name exists in
    /// the outer schema, must still fail as UnknownColumn. This locks the
    /// "no name-only scan in the outer tier" invariant.
    #[test]
    fn correlated_subquery_qualified_outer_ref_with_unbound_qualifier_fails() {
        let bt = base_types_for(&[("emp", emp_schema()), ("dept", dept_schema_with_budget())]);
        // SELECT * FROM emp WHERE dept_id IN (
        //     SELECT dept_id FROM dept d WHERE d.budget > bogus.salary
        // )
        // `bogus` is not the alias of any table in the outer scope.
        // Even though `salary` exists in the outer emp schema, the
        // qualifier-strict outer lookup must not find it.
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
        // NOTE: outer emp has NO alias — so its name is "emp" in the scope,
        // but the inner reference uses "bogus" as qualifier.
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

    // ── assign_types pass ────────────────────────────────────────────────

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
            input: Box::new(scan("emp")),
            condition: int_lit(42),
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::TypeMismatch { .. }));
        assert!(err.to_string().starts_with("[SPARK-EMULATED]"));
    }

    // ── derive_nullability pass — outer join flipping ────────────────────

    /// Analyze `emp JOIN dept` with the given join type and condition; return
    /// the flipped per-side schemas plus the resolved schema. The join output
    /// IS the positional concatenation of the flipped sides (left-only for
    /// semi/anti), so the sides are recovered by slicing at the left child's
    /// field count.
    fn analyze_emp_dept_join(
        jt: JoinType,
        cond: Option<Expression>,
    ) -> (StructType, StructType, StructType) {
        let bt = base_types_with_emp_dept();
        let ast = join(scan("emp"), scan("dept"), jt, cond);
        let typed = analyze(ast, &bt).unwrap();
        let resolved = typed.resolved_schema;
        match &typed.op {
            TypedOp::Join { left, .. } => {
                let left_len = left.resolved_schema.len();
                let flipped_left = StructType::new(resolved.fields[..left_len].to_vec());
                let flipped_right = StructType::new(resolved.fields[left_len..].to_vec());
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
        // Left preserved: `id` stays not-null.
        assert!(!left.field_by_name("id").unwrap().nullable);
        // Right flipped: `dept_id` becomes nullable.
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
        // Output schema is left-only.
        assert_eq!(resolved, emp_schema());
    }

    // ── NATURAL JOIN analyzer desugar (jn-008) ───────────────────────────

    /// `Join` with `natural: true` and no explicit condition/using —
    /// the shape both front-ends produce for a SQL `NATURAL JOIN`.
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

    /// `dept`-shaped table with NO column names in common with `emp_schema`
    /// — exercises the empty-intersection NATURAL rewrite.
    fn dept_no_overlap_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("division", DataType::Integer),
            StructField::nullable("location", DataType::String),
        ])
    }

    /// `emp`-shaped table whose join key is spelled `DEPT_ID` (uppercase) —
    /// the E3 case-sensitivity witness: NATURAL's name intersection is exact
    /// `==`, so this does NOT match `dept_schema`'s lowercase `dept_id`.
    fn emp_uppercase_dept_id_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("DEPT_ID", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ])
    }

    /// LOAD-BEARING: NATURAL inner join over `emp`/`dept` (common column
    /// `dept_id`) must analyze to a TypedAst byte-identical (full
    /// `PartialEq`) to the same join expressed explicitly as
    /// `USING (dept_id)` — proving the desugar rides the existing,
    /// proven-green USING machinery unchanged.
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

        // Output schema: dept_id (INT, nullable — left/emp donor) once and
        // first, then emp's remaining columns, then dept's remaining columns.
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
        // FULL flips both sides fully nullable; every output field is
        // nullable, including the USING-hoisted `dept_id` (coalesced
        // `left.nullable && right.nullable`, both true post-flip).
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
        // Plain concatenation: emp's 4 fields + dept2's 2 fields.
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
        // LEFT flips the right side fully nullable — `division` was not-null.
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

    /// E3 case-sensitivity witness: `DEPT_ID` (left) vs `dept_id` (right)
    /// do NOT intersect under NATURAL's exact `==` match (Spark's
    /// `Seq.intersect`, not τ's usual case-insensitive `field_by_name`) —
    /// the empty intersection rewrites to a cartesian product carrying BOTH
    /// columns.
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
        // Exact (case-sensitive) presence check — `field_by_name` is
        // case-insensitive and would conflate the two distinctly-cased
        // fields, defeating the point of this witness.
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
        // Both `emp` and `dept` have `dept_id`; unqualified reference is
        // ambiguous.
        let cond = unresolved_col("dept_id");
        let ast = join(scan("emp"), scan("dept"), JoinType::Inner, Some(cond));
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
            input: Box::new(join(scan("emp"), scan("dept"), JoinType::Inner, None)),
            // `dept_id` is present on both sides of the join — unqualified
            // reference must fail.
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
        // Sanity anchor: an unqualified column that resolves uniquely across
        // the joined schema must still resolve cleanly.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(join(scan("emp"), scan("dept"), JoinType::Inner, None)),
            // `salary` only exists on `emp`.
            projections: vec![unresolved_col("salary")],
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

    // ── set-op widening (§5) ────────────────────────────────────────────

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
        // `VALUES (1,2),(3)` — a shorter later row must not index out of
        // bounds (session-killing panic); Spark rejects with AnalysisException.
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::Values {
            rows: vec![vec![int_lit(1), int_lit(2)], vec![int_lit(3)]],
            column_names: vec!["a".to_owned(), "b".to_owned()],
        });
        let err = analyze(ast, &bt).expect_err("ragged VALUES must be rejected");
        assert!(
            matches!(err, AnalyzerError::Other { .. }),
            "expected AnalyzerError::Other, got {err:?}"
        );
    }

    /// Project the `dept_id` column (present on both `emp` and `dept`, but
    /// with opposite nullability — `emp.dept_id` nullable, `dept.dept_id`
    /// not-null) from the named table so set-op children carry a single
    /// column of a known nullability.
    fn dept_id_from(table: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(scan(table)),
            projections: vec![unresolved_col("dept_id")],
        })
    }

    /// INTERSECT nullability is an AND-fold (Spark `Intersect.computeOutput`):
    /// nullable(emp.dept_id)=true ∧ non-nullable(dept.dept_id)=false ⇒ the
    /// intersection column is **non-nullable**.
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

    /// EXCEPT nullability is the LEFT child's only (Spark `Except.output =
    /// left.output`). Left non-nullable, right nullable ⇒ output non-nullable.
    #[test]
    fn setop_except_nullability_is_left_child_only_nonnull_left() {
        let bt = base_types_with_emp_dept();
        // Left = dept (non-nullable dept_id), Right = emp (nullable).
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

    /// EXCEPT with a nullable LEFT and non-nullable right ⇒ output nullable
    /// (left-only rule — the right child's non-nullability is irrelevant).
    #[test]
    fn setop_except_nullability_is_left_child_only_nullable_left() {
        let bt = base_types_with_emp_dept();
        // Left = emp (nullable dept_id), Right = dept (non-nullable).
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

    /// Regression guard for the unchanged Union OR-fold: nullable ∪
    /// non-nullable ⇒ nullable.
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

    /// Project a single named column from `table` — a `Project` child so the
    /// set-op positional-cast pushdown (`push_setop_casts`) applies.
    fn project_col(table: &str, col: &str) -> CommonAst {
        CommonAst::new(CommonOp::Project {
            input: Box::new(scan(table)),
            projections: vec![unresolved_col(col)],
        })
    }

    /// Pass 133 — corpus `set-009`: a 3-way `UNION ALL` where the last branch
    /// widens (INT→BIGINT). Each non-`Star` branch projection must be
    /// re-aliased to the widened first-branch name (`id`), casting only the
    /// mismatched branch. The dept child subquery therefore emits
    /// `CAST(dept_id AS BIGINT) AS id`, so `render_set_op`'s outer positional
    /// `... AS id` binds instead of failing "column id cannot be referenced".
    #[test]
    fn setop_union_three_way_widening_aliases_and_casts_mismatched_branch() {
        // SELECT id FROM emp UNION ALL SELECT id FROM emp2
        //   UNION ALL SELECT dept_id FROM dept
        // emp.id / emp2.id = LONG, dept.dept_id = INT → widened LONG, name `id`.
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

    /// Pass 133 latent sibling: a same-type / different-name union. Both
    /// branches are LONG, so NO cast is introduced, but the second branch's
    /// `manager_id` must still be re-aliased to the widened first-branch name
    /// `id` — otherwise its subquery output column is `manager_id` and the
    /// outer positional `... AS id` cannot bind.
    #[test]
    fn setop_union_same_type_different_name_aliases_without_cast() {
        // SELECT id FROM staff UNION ALL SELECT manager_id FROM staff
        // both LONG → alias-only repair, no cast.
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

    /// Non-Union set-ops by-name are punted (DuckDB does not support
    /// `INTERSECT BY NAME` / `EXCEPT BY NAME`); Union by-name proceeds
    /// normally, see [`setop_union_by_name_skips_positional_cast_pushdown`].
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

    /// `range(5)` resolves to a single non-nullable `id: Long` column
    /// (Spark 4.1.1 `range`; ADR-005). Pass-141.
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

    /// An unknown table-valued function is an honest Thunderduck boundary —
    /// `PuntedOperator`, not a silent wrong answer (ADR-022). Pass-141.
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

    /// Pass 13 — `explode(array(1,2,3))` as a bare TVF resolves to a
    /// single-column schema `[col: Integer, non-nullable]` (Spark 4.1.1 default
    /// output column name is `"col"`). Corpus: tbl-007.
    #[test]
    fn table_function_explode_array_resolves_single_col_column() {
        let bt = BaseTypes::empty();
        let arr = func("array", vec![int_lit(1), int_lit(2), int_lit(3)]);
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "explode".to_owned(),
            args: vec![arr],
            with_ordinality: false,
        });
        let typed = analyze(ast, &bt).expect("explode(array(1,2,3)) should analyze");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        let f = &typed.resolved_schema.fields[0];
        assert_eq!(f.name, "col");
        assert_eq!(f.data_type, DataType::Integer);
        assert!(!f.nullable, "explode of non-null array is non-nullable");
    }

    /// Pass 13 — `explode` with a non-Array argument (bare integer) still
    /// punts as PuntedOperator — the gate rejects non-Array args.
    #[test]
    fn table_function_explode_non_array_arg_punts() {
        let bt = BaseTypes::empty();
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "explode".to_owned(),
            args: vec![int_lit(1)],
            with_ordinality: false,
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::PuntedOperator { .. }));
        assert!(
            err.to_string().contains("non-Array"),
            "error should mention non-Array: {err}"
        );
    }

    /// Pass 13 — `explode(unresolved_col)` with no relation in scope yields
    /// `AnalyzerError::UnknownColumn` (args resolve against empty schema).
    #[test]
    fn table_function_explode_unresolvable_arg_is_unknown_column() {
        let bt = BaseTypes::empty();
        let col = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "some_col".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "explode".to_owned(),
            args: vec![col],
            with_ordinality: false,
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(
            matches!(err, AnalyzerError::UnknownColumn { .. }),
            "expected UnknownColumn, got: {err}"
        );
    }

    /// Pass 13 — `posexplode` as bare TVF is not implemented (no witness) —
    /// still punts. The `posexplode` name hits the default `_` arm since it
    /// is not in the `"explode"|"explode_outer"` match set.
    #[test]
    fn table_function_posexplode_still_punts() {
        let bt = BaseTypes::empty();
        let arr = func("array", vec![int_lit(1), int_lit(2)]);
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "posexplode".to_owned(),
            args: vec![arr],
            with_ordinality: false,
        });
        let err = analyze(ast, &bt).unwrap_err();
        assert!(matches!(err, AnalyzerError::PuntedOperator { .. }));
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
            rows: vec![vec![int_lit(1), lit_str("a")]],
            column_names: vec!["x".to_owned(), "y".to_owned()],
        });
        // Right side: reversed column order.
        let right = CommonAst::new(CommonOp::Values {
            rows: vec![vec![lit_str("b"), int_lit(2)]],
            column_names: vec!["y".to_owned(), "x".to_owned()],
        });
        let ast = set_op_by_name(SetOpKind::Union, true, vec![left, right]);
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
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
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
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
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
        let ast = union_by_name_allow_missing(vec![left, right]);
        let typed = analyze(ast, &bt).unwrap();
        let widened = widened_of(&typed);
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
            input: Box::new(scan("emp")),
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
        let unresolved = TypedAst::new(
            TypedOp::SingleRow,
            StructType::new(vec![StructField::nullable("x", DataType::Unresolved)]),
        );
        assert!(!has_resolved_schema(&unresolved));

        // Or with a Project whose projection contains an UnresolvedColumn.
        let with_unresolved_expr = TypedAst::new(
            TypedOp::Project {
                input: Box::new(TypedAst::new(TypedOp::SingleRow, StructType::empty())),
                projections: vec![unresolved_col("x")],
            },
            StructType::new(vec![StructField::nullable("x", DataType::Long)]),
        );
        assert!(!has_resolved_schema(&with_unresolved_expr));
    }

    // ── analyze composes the three passes ───────────────────────────────

    #[test]
    fn analyze_composes_resolve_assign_types_and_derive_nullability() {
        // A Filter over TableScan exercises all three passes end-to-end.
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
    fn analyzer_error_bridge_maps_spark_emulated_with_class_to_spark_emulated() {
        // ADR-023 chunk 3b: `UnknownColumn` has a known `spark_class()`
        // (`UNRESOLVED_COLUMN`), so the bridge routes it to
        // `EmissionError::SparkEmulated` — NOT the legacy
        // `Unsupported{name: "analyzer-spark-emulated"}` path.
        let e = AnalyzerError::UnknownColumn {
            name: "c".to_owned(),
            qualifier: None,
        };
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::SparkEmulated { class, message } => {
                assert_eq!(class, "UNRESOLVED_COLUMN");
                assert!(
                    !message.starts_with("[SPARK-EMULATED]"),
                    "message must not double the internal prefix, got: {message}"
                );
            }
            other => panic!("expected EmissionError::SparkEmulated, got: {other:?}"),
        }
    }

    #[test]
    fn analyzer_error_bridge_maps_other_to_unsupported_expression() {
        // `Other` has no specific Spark class (`spark_class() == None`), so
        // the bridge keeps the legacy `Unsupported{name:
        // "analyzer-spark-emulated"}` path — the ADR-023 chunk 3b carve-out
        // only applies to variants with a known class.
        let e = AnalyzerError::Other {
            reason: "catch-all".to_owned(),
        };
        let bridged = analyzer_error_to_emission_error(e);
        match bridged {
            EmissionError::Unsupported {
                kind: UnsupportedKind::Expression,
                name,
                reason,
            } => {
                assert_eq!(name, "analyzer-spark-emulated");
                assert!(reason.starts_with("[SPARK-EMULATED]"));
            }
            other => panic!("expected EmissionError::Unsupported, got: {other:?}"),
        }
    }

    #[test]
    fn spark_class_mapping_matches_adr023_chunk_3b_table() {
        // Direct pin of the `AnalyzerError::spark_class()` mapping table.
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
            Some("UNRESOLVED_COLUMN")
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

    // ── Aggregate output schema uses function names ─────────────────────

    // ── Unpivot output schema ───────────────────────────────────────────

    #[test]
    fn unpivot_stamps_schema_with_widened_value_column() {
        // Anchor: piv-004 shape — ids=[id], values=[dept_id (INT), salary
        // (DOUBLE)]. Spark widens INT + DOUBLE → DOUBLE; salary is nullable
        // so the value column is nullable. Variable column is STRING NOT NULL.
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
        // Anchor: Spark's default when `values` is empty is "all non-id
        // input columns". The analyzer must materialise that expansion so
        // the emission stage can render an explicit ON list.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
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
    fn unpivot_duplicate_across_ids_and_values_rejected() {
        // M2: `salary` appears in both ids and values. Spark rejects id/value
        // overlap; τ mirrors that with `AnalyzerError::Other`, case-insensitive.
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
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
        // agg-007 shape: `GROUP BY <expr>` where the same expression is also
        // projected under an alias. The grouping key is structurally equal to
        // (the alias-stripped) first aggregate, so it is "already folded" and
        // must NOT be re-prepended → resolved_schema has 2 fields, not 3.
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
    fn aggregate_having_resolves_input_column_inside_aggregate_call() {
        // `HAVING avg(salary) > 80000` — `salary` is an INPUT column not present
        // in the aggregate OUTPUT. Resolving against the input schema must
        // succeed (would fail if resolved against the output schema).
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
            // A bare integer literal is not a boolean predicate.
            Some(int_lit(5)),
        );
        match analyze(ast, &bt) {
            Err(AnalyzerError::TypeMismatch { context, .. }) => {
                assert_eq!(context, "having-condition");
            }
            other => panic!("expected TypeMismatch(having-condition), got {other:?}"),
        }
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
        let emp_scan = scan("emp");
        // Add an `active` column to the emp schema via `withColumn` for the
        // test — grp-004 expects an `active` bool column on emp.
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

    /// Multi-aggregate explicit Pivot names outputs `<value>_<agg_alias>`
    /// per Spark. Guard against name-collision between grouping and pivot
    /// output columns as a bonus assertion.
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
        let emp_scan = scan("emp");
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("salary"),
            pivot_values: vec![
                // Integral double → "1.0".
                lit_double(1.0),
                // Negative integral double → "-2.0".
                lit_double(-2.0),
                // Non-integral float → "1.5".
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

    /// A NULL pivot value is a legitimate bucket. Spark's `PivotTransformer`
    /// only rejects *non-foldable* pivot value expressions
    /// (`NON_LITERAL_PIVOT_VALUES`); a `Literal(null)` is foldable and Spark
    /// names its output column `"null"` (its `outputName` casts to string and
    /// falls back to `"null"`). The values-less overload feeds a discovered
    /// NULL straight into this path, so τ must accept it and stamp a `"null"`
    /// column — verified against `RelationalGroupedDataset.pivot` /
    /// `PivotTransformer.outputName` in Spark 4.x.
    #[test]
    fn analyze_pivot_accepts_null_literal_as_null_bucket() {
        let bt = base_types_with_emp_dept();
        let emp_scan = scan("emp");
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(emp_scan),
            grouping: PivotGrouping::Explicit(vec![unresolved_col("dept_id")]),
            pivot_column: unresolved_col("salary"),
            pivot_values: vec![
                // A discovered NULL bucket (sorts first per Spark's nulls-first
                // ordering) followed by a concrete value.
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
        // grouping column, then the NULL bucket named "null", then "10".
        assert_eq!(names, vec!["dept_id", "null", "10"]);
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
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Implicit,
            pivot_column: unresolved_col("dept_id"),
            pivot_values: vec![int_lit(10), int_lit(20)],
            aggregates: vec![func("avg", vec![unresolved_col("salary")])],
        });
        let typed = analyze(ast, &bt).unwrap();
        assert_eq!(pivot_grouping_names(&typed), vec!["id", "name"]);
    }

    /// `count(*)` references no column (its `Star` argument contributes
    /// nothing), so every non-pivot column stays in the implicit grouping.
    /// pivot on dept_id, agg count(*) ⇒ grouping {id, name, salary}.
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

    /// M1 regression: pivoting on an EXPRESSION column must exclude the
    /// underlying REFERENCED column, not the expression's output name. Pivot
    /// over `abs(dept_id)` references `dept_id`; the old code excluded the
    /// literal name "abs" (no such column) and left `dept_id` in the grouping.
    /// With structural exclusion, agg `avg(salary)` ⇒ grouping {id, name}.
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

    /// SQL UNPIVOT lists only value columns; the analyzer derives ids as
    /// `input − values`, in input order. values = {dept_id, salary}
    /// ⇒ ids {id, name}.
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
        // Output schema: <ids> + metric (STRING NN) + val (widened nullable).
        let names = field_names(&typed);
        assert_eq!(names, vec!["id", "name", "metric", "val"]);
    }

    /// `UnpivotIds::Implicit` with an empty value list is nonsensical (both
    /// axes implicit) — the analyzer rejects it.
    #[test]
    fn analyze_unpivot_implicit_ids_empty_values_rejected() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
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
        base_types_for(&[(
            "addrs",
            StructType::new(vec![StructField::nullable("addr", addr_ty)]),
        )])
    }

    /// Spark 4.1 (Catalyst `UpdateFields.scala::checkInputDataTypes`) rejects
    /// `dropFields("X")` when `X` is not present in the struct. τ mirrors
    /// this as `AnalyzerError::Other` (Spark-emulated). Locking this here
    /// guards against regressing to Spark 3.5's silent-ignore behaviour.
    #[test]
    fn analyze_update_fields_missing_drop_target_is_spark_emulated_error() {
        let bt = base_types_with_addr_table();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("addrs")),
            projections: vec![Expression::UpdateFields(
                super::super::expression::UpdateFieldsExpression {
                    struct_expr: Box::new(unresolved_col("addr")),
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
        base_types_for(&[(
            "emp",
            StructType::new(vec![
                StructField::not_null("id", DataType::Long),
                StructField::nullable("address", addr_ty),
            ]),
        )])
    }

    /// `F.col("address.geo.lat")` — the Spark Connect converter emits
    /// `UnresolvedColumn { qualifier: "address", name: "geo.lat" }`. The
    /// analyzer must rewrite this as an `ExtractValue` chain so emission
    /// produces `("address").geo.lat` rather than `"address"."geo.lat"`.
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
            input: Box::new(scan("emp")),
            projections: vec![qcol("address", "city")],
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
            input: Box::new(scan("emp")),
            projections: vec![qcol("address", "geo.nope")],
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
            input: Box::new(scan("addrs")),
            projections: vec![Expression::UpdateFields(
                super::super::expression::UpdateFieldsExpression {
                    struct_expr: Box::new(unresolved_col("addr")),
                    updates: vec![("GEO".to_owned(), None)],
                },
            )],
        });
        // Successful analysis is the assertion.
        analyze(ast, &bt).expect("case-insensitive drop must analyze cleanly");
    }

    // ── Describe / Summary analysis (Pass 80) ────────────────────────────

    fn base_types_with_emp() -> BaseTypes {
        base_types_for(&[("emp", emp_schema())])
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
            input: Box::new(scan("stats")),
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

    /// The `crosstab(col1, col2)` desugar (`crosstab_to_aggregate`) produces
    /// the Spark-parity contingency-table schema: col0 = `CAST(col1 AS STRING)`
    /// named `{col1}_{col2}` (nullability from col1), then one `bigint`
    /// non-null count column per distinct col2 value, named by the value's
    /// string form and sorted lexicographically ascending as strings. Mirrors
    /// misc-006 (`crosstab(dept_id, active)`).
    #[test]
    fn crosstab_desugar_produces_spark_parity_contingency_schema() {
        let ct_schema = StructType::new(vec![
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("active", DataType::Boolean),
        ]);
        let bt = base_types_for(&[("ct", ct_schema)]);

        // Hand the distinct values in "unsorted" (true before false) to prove
        // the desugar re-sorts by the value's string form ('false' < 'true').
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

        // Lexicographic string sort: 'false' before 'true'.
        assert_eq!(fields[1].name, "false");
        assert_eq!(fields[1].data_type, DataType::Long);
        assert!(!fields[1].nullable, "count columns are bigint non-null");

        assert_eq!(fields[2].name, "true");
        assert_eq!(fields[2].data_type, DataType::Long);
        assert!(!fields[2].nullable, "count columns are bigint non-null");
    }

    // ── Sample / SampleBy analysis (Pass 83) ─────────────────────────────

    #[test]
    fn analyze_sample_schema_passthrough() {
        // Anchor: `df.sample(0.5, seed=11)` produces the same schema as the
        // input relation — Sample is schema-preserving.
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
        // `with_replacement = true` is a Thunderduck-boundary case rejected by
        // the emission stage, not the analyzer. This test pins the analyzer's
        // schema-only responsibility.
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
        // Anchor — samp-002: `sampleBy("dept_id", {10:0.5,...})` resolves the
        // stratum column against the input schema and preserves the schema.
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
        let struct_call = func(
            "struct",
            vec![unresolved_col("name"), unresolved_col("salary")],
        );
        func("array", vec![struct_call])
    }

    fn inline_call(outer: bool, arg: Expression) -> Expression {
        func(if outer { "inline_outer" } else { "inline" }, vec![arg])
    }

    /// Canonical inl-001 shape: `select("id", inline(array(struct(name, age))))`
    /// widens into `[id, name, age]` — one projection per struct field, with
    /// synthesized `Alias(inline_field(arr, "<n>"), "<n>")` shape.
    #[test]
    fn expand_inline_projections_widens_into_n_fields() {
        let bt = base_types_with_emp_dept();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![
                unresolved_col("id"),
                inline_call(false, array_of_struct_name_salary()),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        // Output schema: [id, name, salary].
        assert_eq!(field_names(&typed), vec!["id", "name", "salary"],);
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
            projections: vec![
                unresolved_col("id"),
                inline_call(false, array_of_struct_name_salary()),
                int_lit(1),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        // Layout: [id, name, salary, <literal_output_name>].
        let names = field_names(&typed);
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
        let bad_arg = func("array", vec![int_lit(1)]);
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
        let unresolved_arg = unresolved_col("no_such_col");
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
        let unresolved_arg = unresolved_col("no_such_col");
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
        args.push(unresolved_col(json_col));
        for k in keys {
            args.push(lit_str(k));
        }
        func("json_tuple", args)
    }

    fn raw_schema_with_json_str() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("json_str", DataType::String),
        ])
    }

    fn base_types_with_raw() -> BaseTypes {
        base_types_for(&[("raw", raw_schema_with_json_str())])
    }

    /// Canonical json-002 shape: `select("id", json_tuple("json_str", "a", "e"))`
    /// widens into `[id, c0, c1]` — positional names, both nullable STRING.
    #[test]
    fn expand_json_tuple_widens_into_n_fields_with_positional_names() {
        let bt = base_types_with_raw();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("raw")),
            projections: vec![
                unresolved_col("id"),
                json_tuple_call("json_str", &["a", "e"]),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        // Output schema: [id, c0, c1].
        let names = field_names(&typed);
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
            input: Box::new(scan("raw")),
            projections: vec![
                unresolved_col("id"),
                json_tuple_call("json_str", &["a", "e"]),
                int_lit(1),
            ],
        });
        let typed = analyze(ast, &bt).expect("analyze ok");
        let names = field_names(&typed);
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
        let bad_call = func(
            "json_tuple",
            vec![unresolved_col("json_str"), unresolved_col("k")],
        );
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

    // ── na_fill_compatible predicate ─────────────────────────────────────────

    /// Direct unit coverage of the shared predicate used by both
    /// `analyze_na_fill` and `render_na_fill`.
    #[test]
    fn na_fill_compatible_matches_spark_fill_value_rules() {
        // Numeric ↔ numeric (all pairs) → true.
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
        // Same-family scalars.
        assert!(na_fill_compatible(&DataType::String, &DataType::String));
        assert!(na_fill_compatible(&DataType::Boolean, &DataType::Boolean));
        // Cross-family — the mismatches Spark silently skips.
        assert!(!na_fill_compatible(&DataType::String, &DataType::Long));
        assert!(!na_fill_compatible(&DataType::Long, &DataType::String));
        assert!(!na_fill_compatible(&DataType::Boolean, &DataType::Long));
        assert!(!na_fill_compatible(&DataType::String, &DataType::Boolean));
        // Date / Timestamp never fill.
        assert!(!na_fill_compatible(&DataType::Date, &DataType::Long));
        assert!(!na_fill_compatible(&DataType::Timestamp, &DataType::Long));
    }

    /// Regression for `chain-002`: `.na.fill(0)` on a mixed schema must (a)
    /// preserve original nullability on non-numeric columns, (b) flip
    /// nullability only on numeric columns. This exercises the `analyze()`
    /// entry (shared by `generate_with_schema` and `analyze_schema` — verified
    /// in `crates/core/src/transpiler_v2/mod.rs::{generate_with_schema,
    /// analyze_schema}`).
    #[test]
    fn analyze_na_fill_empty_cols_int_value_skips_non_numeric_columns() {
        // Schema: mixed [string, long, double, boolean].
        let mixed_schema = StructType::new(vec![
            StructField::nullable("s", DataType::String),
            StructField::nullable("l", DataType::Long),
            StructField::nullable("d", DataType::Double),
            StructField::nullable("b", DataType::Boolean),
        ]);
        let bt = base_types_for(&[("t", mixed_schema)]);
        // NaFill { cols: [], values: [Int(0)] } — client's `.na.fill(0)` form.
        let ast = CommonAst::new(CommonOp::NaFill {
            input: Box::new(scan("t")),
            cols: vec![],
            values: vec![int_lit(0)],
        });
        let typed = analyze(ast, &bt).expect("analyze NaFill must succeed");
        let fields = &typed.resolved_schema.fields;
        assert_eq!(fields.len(), 4);
        // Non-numeric columns preserve original nullability (nullable=true).
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
        // Numeric columns become non-nullable (compatible fill).
        assert_eq!(fields[1].name, "l");
        assert_eq!(fields[1].data_type, DataType::Long);
        assert!(!fields[1].nullable, "Long column must flip to non-null");
        assert_eq!(fields[2].name, "d");
        assert_eq!(fields[2].data_type, DataType::Double);
        assert!(!fields[2].nullable, "Double column must flip to non-null");
    }

    // ── Pass 127 — Spark toPrettySQL default naming for unaliased exprs ───

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
    fn sel_008_shaped_project_names_unaliased_computed_columns() {
        let bt = base_types_with_emp_dept();
        // SELECT id, dept_id + 1, salary / 1000 FROM emp — mirrors sel-008's
        // shape (an id passthrough plus two unaliased computed projections),
        // using columns present in `emp_schema`.
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

    // ── Alias-aware qualified column resolution over joins (jn-006) ───────
    //
    // The pin-table cases (both sides typed, correct nullability per join
    // kind, passthrough descent, nested-join range-vs-side-schema) live in
    // `analyzer_fixtures.rs` and are exercised via INV4/INV5. These direct
    // tests cover the error paths and precedence rules that the fixture
    // registry deliberately excludes (see its module doc).

    /// `TableScan` with an explicit alias — the shape both front-ends
    /// produce for `FROM table AS alias`.
    fn aliased(table: &str, alias: &str) -> CommonAst {
        CommonAst::new(CommonOp::TableScan {
            table: table.to_owned(),
            alias: Some(alias.to_owned()),
        })
    }

    #[test]
    fn unqualified_duplicate_name_over_join_is_ambiguous() {
        // `dept_id` exists on both sides of `emp e CROSS JOIN dept d` — an
        // UNQUALIFIED reference must still raise `AmbiguousColumn`, exactly
        // as before this pass (tier (c) is unchanged).
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
        // `d` unambiguously binds to `dept`, but `dept` has no `id` column —
        // a Spark-emulated `UnknownColumn{qualifier: Some("d")}`, not an
        // opaque DuckDB bind error and not a silent fallback to emp's `id`.
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
    fn self_join_duplicate_alias_binding_falls_back_to_legacy_no_panic() {
        // `emp AS e1 JOIN emp AS e2`, referenced by the bare TABLE NAME
        // `emp` (not either alias): the `TableScan` scope arm binds the bare
        // table name `emp` on BOTH sides (once per side), so the qualifier
        // `emp` binds 2+ ranges. ADR-023 3b-i: this is exactly the qualifier
        // 2+ case — `resolve_column` now raises `AmbiguousColumn` here
        // instead of silently falling back to the legacy first-match path
        // (this test's name predates that fix; a rename is deliberately
        // deferred to the next chunk that touches this test's other
        // assertions, to keep this diff to the ambiguity-discipline change).
        //
        // Empirically (probed against Spark 4.1.1 via
        // `differential.dataframe_corpus.build_inputs`), Spark's real error
        // class for THIS exact shape is `UNRESOLVED_COLUMN.WITH_SUGGESTION`,
        // not `AMBIGUOUS_REFERENCE`: once a relation is aliased, Spark's
        // analyzer treats the base table name as shadowed/unresolvable
        // entirely (even for a single, non-duplicated alias), so `emp` never
        // reaches ambiguity-checking in Spark at all. τ's `RelScope`
        // TableScan arm does not implement that alias-shadowing (a
        // pre-existing, separate divergence out of scope for this chunk) —
        // it still binds both the table name and the alias unconditionally,
        // which is what surfaces the qualifier-2+ ambiguity fixed here.
        // `AmbiguousColumn` is still the strictly better outcome versus the
        // previous silent first-match fallback, so we pin it rather than
        // leave the divergence undocumented.
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
    fn qualifier_binds_both_sides_of_join_is_ambiguous() {
        // ADR-023 3b-i, jn-024 shape: the SAME user alias `x` on BOTH sides
        // of a join (`emp AS x JOIN dept AS x`) — `x` binds 2+ ranges, so a
        // qualified reference to it must raise `AmbiguousColumn` rather than
        // resolve via the legacy first-match fallback. Matches Spark 4.1.1's
        // `AMBIGUOUS_REFERENCE` for this shape (probed empirically).
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
        // `emp` has a top-level STRUCT column named `address`; alias the
        // `emp` scan itself as `address` too, so the qualifier `address` is
        // BOTH a relation alias (whole-schema range) AND a struct column
        // name. Tier (d) — struct-column qualifier — must still win: the
        // reference resolves as `address.city` (a struct field), not as
        // relation-alias lookup for a top-level `city` column (which does
        // not exist and would raise `UnknownColumn` if alias precedence won).
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
        // `e1.id = e2.manager_id` inside a self-join's `ON` condition:
        // `ResolveContext::for_join_condition` must stamp each side against
        // its OWN range, not the merged schema's first match (both refs
        // share candidate names — `id`/`manager_id` — that also exist,
        // differently typed/nulled, on the other side is not the case here,
        // but the alias-qualified condition path must still resolve each
        // side correctly rather than through the synthetic `__td_j*` path).
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
                    assert_eq!(l.data_type, Some(DataType::Long));
                    assert_eq!(l.nullable, Some(false));
                    assert_eq!(r.data_type, Some(DataType::Long));
                    assert_eq!(r.nullable, Some(true));
                }
                other => panic!("expected two ColumnReferences, got {other:?}"),
            },
            other => panic!("expected Join with a Binary condition, got {other:?}"),
        }
    }

    #[test]
    fn using_join_qualifier_resolution_stays_on_legacy_path_no_panic() {
        // USING joins reorder/dedup the output schema, breaking the
        // contiguous-range invariant `collect_qualifier_bindings` relies on
        // — it deliberately STOPs there (no bindings collected), so
        // qualified refs keep resolving via the legacy, pre-fix path.
        // Exercise the STOP arm end-to-end so it isn't dead code and
        // doesn't regress to a panic.
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

    // ── Pass 9 (pr-007) — Lateral Column Alias (LCA) ───────────────────────

    /// Whether `expr` (or any descendant) contains an `UnresolvedColumn`
    /// case-insensitively named `name`. Shared by the chain/nested tests to
    /// assert an earlier alias name has been fully inlined away.
    fn contains_unresolved_ref(expr: &Expression, name: &str) -> bool {
        match expr {
            Expression::UnresolvedColumn(u) => u.name.eq_ignore_ascii_case(name),
            other => other.children().any(|c| contains_unresolved_ref(c, name)),
        }
    }

    #[test]
    fn lateral_column_alias_single_ref_resolves_and_types() {
        // pr-007: SELECT salary * 1.1 AS raised, raised - salary AS delta FROM emp
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
        // a = salary + 1; b = a + 1 (refs `a`); c = b + 1 (refs `b`) — a
        // three-item chain must fully inline in a single left-to-right pass.
        let schema = emp_schema();
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
        // `emp_schema` has a real `dept_id` input column. An item aliased AS
        // `dept_id` collides with it, so it must NOT be recorded as a
        // lateral source; a later bare `dept_id` ref must stay untouched
        // (falls through to ordinary resolution against the INPUT column,
        // not the alias expression).
        let schema = emp_schema();
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
        // Two earlier items both alias to `x`; a third item references `x`
        // — Spark's AMBIGUOUS_LATERAL_COLUMN_ALIAS (count = 2).
        let schema = emp_schema();
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
        // Two same-named aliases with NO later reference is ordinary, legal
        // SQL (duplicate output column names) — must not error.
        let schema = emp_schema();
        let x1 = alias_expr(unresolved_col("salary"), "x");
        let x2 = alias_expr(unresolved_col("id"), "x");
        let expanded = expand_lateral_column_aliases(vec![x1, x2], &schema)
            .expect("duplicate alias names with no later reference must not error");
        assert_eq!(expanded.len(), 2);
    }

    #[test]
    fn lateral_column_alias_substitutes_inside_function_call_and_case_when() {
        let schema = emp_schema();
        let raised = alias_expr(
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Mul,
                left: Box::new(unresolved_col("salary")),
                right: Box::new(lit_double(1.1)),
            }),
            "raised",
        );
        // Lateral ref inside a FunctionCall arg: `abs(raised)`.
        let via_call = alias_expr(func("abs", vec![unresolved_col("raised")]), "abs_raised");
        // Lateral ref inside a CASE branch: `CASE WHEN raised > 0 THEN raised
        // ELSE salary END`.
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
        // Item 1 references `delta`, which is only DEFINED by item 2 — no
        // look-ahead: item 1's `delta` ref is left untouched (table is empty
        // when item 1 is processed), falls through to ordinary resolution,
        // and surfaces the correct `UnknownColumn` (proven end-to-end via
        // `analyze()`, not merely the pre-pass in isolation).
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
        // `SELECT 1 AS x, transform(arr, x -> x + 1) AS transformed FROM t`
        // — the lambda's OWN bound parameter `x` must NOT be substituted by
        // the outer lateral alias `x`.
        let schema = StructType::new(vec![StructField::nullable(
            "arr",
            DataType::Array(Box::new(DataType::Integer), true),
        )]);
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

    // ── LATERAL VIEW analyzer tests (cx-007/cx-008/cx-009) ──────────────

    /// Build an emp schema with a `tags` column of type `ARRAY<STRING>` for
    /// LATERAL VIEW tests.
    fn emp_tags_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("tags", DataType::Array(Box::new(DataType::String), true)),
        ])
    }

    /// Build a `CommonOp::LateralView` wrapping a TableScan("emp") with the
    /// given generator columns under table alias `t`.
    fn lateral_view_plan(columns: Vec<(String, Expression)>) -> CommonAst {
        CommonAst::new(CommonOp::LateralView {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: Some("e".to_owned()),
            })),
            table_alias: "t".to_owned(),
            columns,
        })
    }

    fn explode_call(arg: Expression) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: "explode".to_owned(),
            args: vec![arg],
            distinct: false,
        })
    }

    fn explode_outer_call(arg: Expression) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: "explode_outer".to_owned(),
            args: vec![arg],
            distinct: false,
        })
    }

    #[test]
    fn lateral_view_schema_union_is_input_then_generated() {
        let plan = lateral_view_plan(vec![("tag".to_owned(), explode_call(qcol("e", "tags")))]);
        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), emp_tags_schema())]
                .into_iter()
                .collect(),
        );
        let typed = analyze(plan, &bt).expect("analyze");
        // Output schema = input fields (id, name, tags) + generated (tag).
        assert_eq!(typed.resolved_schema.len(), 4);
        assert_eq!(typed.resolved_schema.fields[0].name, "id");
        assert_eq!(typed.resolved_schema.fields[1].name, "name");
        assert_eq!(typed.resolved_schema.fields[2].name, "tags");
        assert_eq!(typed.resolved_schema.fields[3].name, "tag");
        assert_eq!(typed.resolved_schema.fields[3].data_type, DataType::String);
    }

    #[test]
    fn lateral_view_qualifier_binding_t_tag_resolves() {
        // Project[t.tag] over LateralView — `t.tag` must resolve.
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral_view_plan(vec![(
                "tag".to_owned(),
                explode_call(qcol("e", "tags")),
            )])),
            projections: vec![qcol("t", "tag")],
        });
        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), emp_tags_schema())]
                .into_iter()
                .collect(),
        );
        let typed = analyze(plan, &bt).expect("analyze must succeed for t.tag");
        assert_eq!(typed.resolved_schema.fields[0].name, "tag");
    }

    #[test]
    fn lateral_view_qualifier_binding_e_id_resolves() {
        // Project[e.id] over LateralView — input-side qualifier must resolve.
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral_view_plan(vec![(
                "tag".to_owned(),
                explode_call(qcol("e", "tags")),
            )])),
            projections: vec![qcol("e", "id")],
        });
        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), emp_tags_schema())]
                .into_iter()
                .collect(),
        );
        let typed = analyze(plan, &bt).expect("analyze must succeed for e.id");
        assert_eq!(typed.resolved_schema.fields[0].name, "id");
    }

    #[test]
    fn lateral_view_qualifier_t_nope_is_unknown_column() {
        // `t.nope` — column not in the generated range → UnknownColumn.
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral_view_plan(vec![(
                "tag".to_owned(),
                explode_call(qcol("e", "tags")),
            )])),
            projections: vec![qcol("t", "nope")],
        });
        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), emp_tags_schema())]
                .into_iter()
                .collect(),
        );
        let err = analyze(plan, &bt).expect_err("t.nope must fail");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "nope");
                assert_eq!(qualifier, Some("t".to_owned()));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn lateral_view_qualifier_e_tag_is_unknown_column() {
        // `e.tag` — `tag` is NOT in the input (emp) range → UnknownColumn.
        // This proves the bespoke qualifier binding arm is active (the legacy
        // name-only fallback would wrongly resolve `tag` by name).
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral_view_plan(vec![(
                "tag".to_owned(),
                explode_call(qcol("e", "tags")),
            )])),
            projections: vec![qcol("e", "tag")],
        });
        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), emp_tags_schema())]
                .into_iter()
                .collect(),
        );
        let err = analyze(plan, &bt).expect_err("e.tag must fail");
        match err {
            AnalyzerError::UnknownColumn { name, qualifier } => {
                assert_eq!(name, "tag");
                assert_eq!(qualifier, Some("e".to_owned()));
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn lateral_view_outer_always_nullable() {
        // explode_outer always produces nullable output regardless of
        // the array's containsNull flag.
        let non_null_array_schema = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::not_null("tags", DataType::Array(Box::new(DataType::String), false)),
        ]);
        let plan = CommonAst::new(CommonOp::LateralView {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: Some("e".to_owned()),
            })),
            table_alias: "t".to_owned(),
            columns: vec![("tag".to_owned(), explode_outer_call(qcol("e", "tags")))],
        });
        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), non_null_array_schema)]
                .into_iter()
                .collect(),
        );
        let typed = analyze(plan, &bt).expect("analyze");
        // explode_outer always produces nullable output.
        assert!(typed.resolved_schema.fields[2].nullable);
    }

    #[test]
    fn lateral_view_posexplode_pos_not_nullable() {
        let plan = CommonAst::new(CommonOp::LateralView {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: Some("e".to_owned()),
            })),
            table_alias: "t".to_owned(),
            columns: vec![
                (
                    "pos".to_owned(),
                    Expression::FunctionCall(FunctionCall {
                        name: "posexplode_pos".to_owned(),
                        args: vec![qcol("e", "tags")],
                        distinct: false,
                    }),
                ),
                (
                    "tag".to_owned(),
                    Expression::FunctionCall(FunctionCall {
                        name: "posexplode_val".to_owned(),
                        args: vec![qcol("e", "tags")],
                        distinct: false,
                    }),
                ),
            ],
        });
        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), emp_tags_schema())]
                .into_iter()
                .collect(),
        );
        let typed = analyze(plan, &bt).expect("analyze");
        // pos is the 4th field (index 3), val is the 5th (index 4).
        assert_eq!(typed.resolved_schema.fields[3].name, "pos");
        assert!(
            !typed.resolved_schema.fields[3].nullable,
            "posexplode_pos is non-nullable"
        );
        assert_eq!(typed.resolved_schema.fields[3].data_type, DataType::Integer);
        assert_eq!(typed.resolved_schema.fields[4].name, "tag");
        // posexplode_val nullable depends on containsNull of the array.
        assert!(typed.resolved_schema.fields[4].nullable);
        assert_eq!(typed.resolved_schema.fields[4].data_type, DataType::String);
    }

    // ── Pass-17: LATERAL derived-table join ─────────────────────────────

    /// Build a `lateral_join` AST node.
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

    /// tbl-005 shape: `emp e JOIN LATERAL (SELECT avg(e2.salary) AS dept_avg
    /// FROM emp e2 WHERE e2.dept_id <=> e.dept_id) t`.
    /// Simplified to: `emp e CROSS JOIN LATERAL (SELECT e.name AS dept_avg) t`
    /// — the right subquery references the left's column via OuterScope.
    #[test]
    fn lateral_join_analyzes_with_outer_scope_from_left_sibling() {
        let bt = base_types_with_emp_dept();
        // Left: `emp` aliased as `e`.
        let left = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan("emp")),
            alias: "e".to_owned(),
        });
        // Right: a subquery that references e.name from the left side.
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
        // Output schema: [name, dept_avg].
        assert_eq!(typed.resolved_schema.fields.len(), 2);
        assert_eq!(typed.resolved_schema.fields[0].name, "name");
        assert_eq!(typed.resolved_schema.fields[1].name, "dept_avg");
        // The join should be rewritten to Cross (Inner + no ON + lateral).
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
        let msg = format!("{err:?}");
        assert!(msg.contains("LATERAL join with NATURAL join"), "got: {msg}");
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
        let msg = format!("{err:?}");
        assert!(msg.contains("LATERAL join with USING join"), "got: {msg}");
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

    /// One-level-only witness: a lateral join nested inside a genuinely
    /// CORRELATED subquery context (EXISTS over `dept d`) that supplies a
    /// non-None inherited `outer`. The lateral's right child references
    /// `d.dept_name` from the inherited outer. If the lateral branch
    /// COMPOSED (rather than replaced) outer scopes, `d.dept_name` would
    /// resolve via the leaked inherited outer. Must fail with UnknownColumn
    /// because the lateral REPLACES the inherited outer with only its
    /// immediate left sibling (`e`).
    #[test]
    fn lateral_join_one_level_only_grandparent_ref_fails() {
        use crate::transpiler_v2::expression::{ExistsSubquery, SubqueryPlan};
        let bt = base_types_with_emp_dept();
        // The lateral join that will live inside the EXISTS subquery:
        // `emp e JOIN LATERAL (SELECT d.dept_name AS x) t`
        // The right subquery references `d.dept_name` from the outer scope.
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
        // Wrap the lateral join in an EXISTS subquery expression.
        // The EXISTS is the filter condition of `SELECT * FROM dept d WHERE EXISTS(...)`.
        // `analyze_subquery_plan` will analyze the inner plan with
        // `outer = Some(OuterScope { schema: &dept_d_schema, scopes: ... })`,
        // so a non-None outer IS available. If the lateral composed instead of
        // replaced, `d.dept_name` would resolve via the leaked outer.
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
        // The lateral's right child sees only `e` (its left sibling), not `d`
        // from the inherited outer. `d.dept_name` must be UnknownColumn.
        assert!(
            msg.contains("UnknownColumn") || msg.contains("unknown column"),
            "expected UnknownColumn for d.dept_name leaked from inherited outer, got: {msg}"
        );
    }

    /// Regression: a non-lateral Inner join with no ON/USING still triggers the
    /// existing boundary error (it was not converted to Cross).
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
        // dispatch_op should error (boundary: non-cross join without ON/USING).
        let result = crate::transpiler_v2::emission::dispatch_op(&typed.op, &typed.resolved_schema);
        assert!(result.is_err(), "non-lateral clauseless Inner must error");
    }

    // ── Pass 18: RecursiveCte analyzer tests ──────────────────────────────

    /// emp schema with `manager_id` for recursive CTE tests (cte-010).
    fn emp_schema_with_manager() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("manager_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ])
    }

    /// Build a `RecursiveCte` AST node directly (bypasses the parser).
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
        // cte-009: `WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1
        // FROM seq WHERE n < 5) SELECT * FROM seq`.
        // Anchor: `SELECT 1` → Project over SingleRow with INT literal.
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        // Recursive term: `SELECT n + 1 FROM seq WHERE n < 5`.
        // In SQL, WHERE filters before SELECT projects:
        // Project { input: Filter { input: scan("seq"), cond }, projections }
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
        // Wrap in the CTE reference: AliasedRelation { RecursiveCte, "seq" }
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
        // resolved_schema should be the anchor's renamed schema: n:Integer.
        assert_eq!(field_names(&typed), vec!["n"]);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Integer);
        // Nullability is always true for recursive-CTE output, regardless of
        // the anchor's own (non-nullable) literal — matches Spark's reference
        // behavior verified against the 4.1.1 pin.
        assert!(typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn analyze_recursive_cte_010_join_form() {
        // cte-010: `WITH RECURSIVE chain(id, name, manager_id, lvl) AS (
        //   SELECT id, name, manager_id, 0 FROM emp WHERE manager_id IS NULL
        //   UNION ALL
        //   SELECT e.id, e.name, e.manager_id, c.lvl + 1
        //   FROM emp e JOIN chain c ON e.manager_id = c.id
        // ) SELECT * FROM chain`.
        // In SQL, WHERE filters before SELECT projects.
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
        // Recursive term: join emp e with chain c.
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

        // emp_schema_with_manager has id:Long, name:String, manager_id:Integer
        // (nullable), salary:Double. We need emp in base_types for the anchor's
        // scan AND the recursive term's `FROM emp e`. chain resolves via the
        // injected entry.
        let bt = base_types_for(&[("emp", emp_schema_with_manager())]);
        let typed = analyze(outer, &bt).expect("analyze cte-010");

        assert_eq!(field_names(&typed), vec!["id", "name", "manager_id", "lvl"]);
        // id comes from emp.id (Long NOT NULL).
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
        // lvl: anchor is int_lit(0) → Integer.
        assert_eq!(typed.resolved_schema.fields[3].data_type, DataType::Integer);
        // id: both legs source from the real `emp` table's NOT NULL id — no
        // self-reference touches this column, so nullability stays false
        // (verified against the 4.1.1 reference: Reference nullable=false).
        assert!(!typed.resolved_schema.fields[0].nullable);
        // lvl: the recursive leg's `c.lvl + 1` reads the self-reference
        // (`chain c`), which is always typed nullable — OR-folded with the
        // anchor's non-nullable `0` literal, the output is nullable.
        assert!(typed.resolved_schema.fields[3].nullable);
    }

    #[test]
    fn analyze_recursive_cte_union_without_all_rejected() {
        // UNION (without ALL) → Spark-emulated UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE.
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
        let msg = format!("{err}");
        assert!(
            msg.contains("UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE"),
            "error should mention UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE, got: {msg}"
        );
    }

    #[test]
    fn analyze_recursive_cte_column_list_arity_mismatch() {
        // column_names has 2 entries but anchor produces 1 column.
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
        // Anchor produces 1 column but recursive term produces 2.
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        // Recursive term produces 2 columns.
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
        // BaseTypes already has an entry for "seq" (a catalog table with a
        // STRING column). The CTE injection must shadow it — the recursive
        // term must resolve `n` via the INJECTED schema (Integer from anchor),
        // not the pre-existing catalog entry. If injection-wins is broken, the
        // recursive term's `n + 1` would attempt Integer + Integer on a String
        // column (from catalog) and produce a wrong type or fail.
        let catalog_seq_schema = StructType::new(vec![
            StructField::nullable("n", DataType::String), // different type!
        ]);
        let bt = base_types_for(&[("seq", catalog_seq_schema)]);

        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        // The recursive term's `n + 1` can only succeed as Integer + Integer.
        // If the catalog's String schema leaks through (injection-wins broken),
        // `n` resolves as String and the Add expression types differently.
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
        // n should be Integer (from anchor), NOT String (from catalog).
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Integer);
        // Verify the recursive term resolved `n` as Integer (injection-wins
        // proof): drill into RecursiveCte → recursive_term → its input
        // TableScan resolves to the Integer schema, not the catalog String.
        let cte_op = match &typed.op {
            TypedOp::Project { input, .. } => match &input.op {
                TypedOp::AliasedRelation { input, .. } => &input.op,
                other => panic!("expected AliasedRelation, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        };
        match cte_op {
            TypedOp::RecursiveCte { recursive_term, .. } => {
                // The recursive term's input (under Project) is a TableScan
                // whose resolved_schema must be the INJECTED Integer schema.
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
        // Regression: `WITH RECURSIVE Seq(n) AS (... FROM Seq ...)` — the CTE
        // name is lowercased to "seq" for BaseTypes injection, but the
        // self-reference TableScan preserves source case "Seq". Without the
        // case-insensitive injection fix, BaseTypes::lookup("Seq") misses and
        // produces a spurious UnknownTable error.
        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        // Self-reference uses source-case "Seq" (not "seq").
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
        // CTE name is lowercase "seq" (from lowering).
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
        // Regression (silent-wrong-results variant): a catalog table "Seq"
        // exists with a STRING column. The CTE injection under lowercase "seq"
        // misses the case-sensitive lookup for "Seq", which would fall through
        // to the catalog entry and bind with the wrong schema.
        let catalog_schema = StructType::new(vec![StructField::nullable("n", DataType::String)]);
        let bt = base_types_for(&[("Seq", catalog_schema)]);

        let anchor = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![int_lit(1)],
        });
        // Self-reference uses "Seq" (matching the catalog entry's case).
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
        // Must be Integer (CTE anchor), not String (catalog).
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Integer);
    }

    #[test]
    fn analyze_recursive_cte_010_c_lvl_resolves_integer() {
        // Specifically verify that `c.lvl` inside the recursive term's JOIN
        // condition resolves with the correct type (Integer), proving the
        // self-reference binds through the injected BaseTypes entry.
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

        // lvl field type must be Integer (anchor's int_lit(0)).
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

    // ── plan_id-scoped resolution above joins ──────────────────────────

    /// Unqualified unresolved column WITH a plan_id tag.
    fn plan_id_col(name: &str, pid: i64) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: Some(pid),
        })
    }

    /// `Join` with plan_id sets but no USING columns.
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
        // Self-join `emp(plan_id=1) JOIN emp(plan_id=2)` with a Project
        // above selecting `id` tagged with plan_id=2 — must resolve to the
        // RIGHT side's `id`, not raise AmbiguousColumn. The resolved
        // ColumnReference must carry qualifier `__td_jr` so emission renders
        // `__td_jr.id` (unambiguous in the alias-transparent FROM clause).
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(qcol("__td_jl", "id")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("__td_jr", "id")),
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
        // Verify the resolved projection carries the join-side qualifier.
        if let TypedOp::Project { projections, .. } = &typed.op {
            match &projections[0] {
                Expression::ColumnReference(c) => {
                    assert_eq!(
                        c.qualifier.as_deref(),
                        Some(TD_JOIN_RIGHT),
                        "plan_id=2 must resolve with __td_jr qualifier"
                    );
                }
                other => panic!("expected ColumnReference, got: {other:?}"),
            }
        } else {
            panic!("expected Project op");
        }
    }

    #[test]
    fn plan_id_disambiguates_filter_above_join() {
        // Filter above a self-join: `WHERE salary > 5` with plan_id=1 must
        // resolve to the LEFT side without ambiguity error, and carry the
        // `__td_jl` qualifier.
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(qcol("__td_jl", "salary")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("__td_jr", "salary")),
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
        // Output schema is the full join (8 fields: 4 left + 4 right).
        assert_eq!(typed.resolved_schema.fields.len(), 8);
        // Verify the resolved condition's left operand carries __td_jl.
        if let TypedOp::Filter { condition, .. } = &typed.op {
            if let Expression::Binary(b) = condition {
                match b.left.as_ref() {
                    Expression::ColumnReference(c) => {
                        assert_eq!(
                            c.qualifier.as_deref(),
                            Some(TD_JOIN_LEFT),
                            "plan_id=1 must resolve with __td_jl qualifier"
                        );
                    }
                    other => panic!("expected ColumnReference, got: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn plan_id_binds_both_sides_of_same_join_is_ambiguous() {
        // ADR-023 3b-i, join-023 shape: the un-realiased self-join
        // `df.join(df, ...)` — the SAME plan_id (1) tagged on BOTH the left
        // AND right side of the SAME join. A reference carrying that plan_id
        // must raise `AmbiguousColumn`, not silently bind the left side.
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
    fn plan_id_three_way_nested_join() {
        // Three-way join: (emp(pid=1) JOIN emp(pid=2)) JOIN dept(pid=3)
        // with a Project above selecting `dept_id` tagged with plan_id=3
        // (the right side of the OUTER join) — must resolve to dept's
        // `dept_id`, not emp's `dept_id`.
        let bt = base_types_with_emp_dept();
        let inner_cond = Expression::Binary(BinaryExpression {
            left: Box::new(qcol("__td_jl", "id")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("__td_jr", "id")),
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
            left: Box::new(qcol("__td_jl", "dept_id")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("__td_jr", "dept_id")),
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
        // dept's dept_id is Integer (not_null); emp's dept_id is also
        // Integer but nullable. The dept-side field is NOT NULL.
        assert!(!typed.resolved_schema.fields[0].nullable);
    }

    #[test]
    fn plan_id_unique_column_omits_qualifier() {
        // When the column name is UNIQUE across both sides of a join,
        // the qualifier must NOT be stamped — __td_jl/__td_jr are only
        // in scope under alias-transparent rendering (Project-over-Join).
        // Other shapes (Filter-over-Join → __td_filter, etc.) would
        // break if the qualifier were present. Verify that `value`
        // (unique to the left side emp) and `dept_name` (unique to the
        // right side dept) resolve without qualifiers.
        let bt = base_types_with_emp_dept();
        let join_cond = Expression::Binary(BinaryExpression {
            left: Box::new(qcol("__td_jl", "dept_id")),
            op: BinaryOp::Eq,
            right: Box::new(qcol("__td_jr", "dept_id")),
        });
        let joined = join_with_plan_ids(
            scan("emp"),
            scan("dept"),
            JoinType::Inner,
            Some(join_cond),
            vec![1],
            vec![2],
        );
        // Filter above the join: references unique columns by plan_id.
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(joined),
            condition: Expression::Binary(BinaryExpression {
                left: Box::new(plan_id_col("salary", 1)),
                op: BinaryOp::Gt,
                right: Box::new(int_lit(0)),
            }),
        });
        let typed = analyze(filter, &bt).expect("unique column should resolve without qualifier");
        if let TypedOp::Filter { condition, .. } = &typed.op {
            if let Expression::Binary(b) = condition {
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
    }

    #[test]
    fn plan_id_unknown_falls_back_to_legacy() {
        // A plan_id that does not appear in any join's plan_id sets
        // should fall through to the legacy name-only resolution.
        // With a simple (non-join) scan, `id` with plan_id=99 resolves
        // normally because there is exactly one `id` field.
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
        // A plan_id-tagged ref under a non-join input (no join in tree)
        // must resolve via the legacy path as long as the column is
        // unambiguous. This verifies we do not error on plan_id when
        // no join is present.
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

    // ── F13 — user-typed reserved qualifier must not panic ─────────────────
    // `__td_jl`/`__td_jr` are analyzer-internal join-condition markers
    // (`qualify_plan_id_refs`), only ever scoped inside a join condition via
    // `ResolveContext::for_join_condition`. A user typing them directly
    // (outside a join condition, so `ctx.scoped_range` misses) must get a
    // clean `UnknownColumn` — Spark itself raises `UNRESOLVED_COLUMN` for
    // `col("__td_jl.x")` — never a `mark_node` debug_assert panic.

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
        // A returned Err (rather than a panic) is itself the regression
        // proof for F13.
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
}
