//! Type & nullability analyzer (ADR-005, ADR-006).
//!
//! `analyze(CommonAst, &BaseTypes) -> Result<TypedAst, AnalyzerError>` runs
//! a bounded sequence of passes (see §6 of the Slice B plan). After a
//! successful call:
//! - every operator carries a resolved [`Schema`] (its output shape);
//! - every expression slot carries a [`TypedAttr`] (Spark `DataType` +
//!   nullability), derived by delegating to the legacy
//!   [`crate::types::TypeInferenceEngine`] and
//!   [`crate::expression::Expression::data_type`] / `::nullable`.
//!
//! Invariant [INV5]: on the returned `TypedAst`, no `TypedAttr` carries
//! [`crate::types::DataType::Unresolved`]. Enforced by
//! [`has_resolved_schema`].

use crate::expression::{Expression, SortOrder};
use crate::logical::{spark_column_name, GroupingSets};
use crate::transpiler_v2::ast::{
    Aggregate, AggregateCall, AliasedRelation, CommonAst, CommonOp, Distinct, DropColumns, Except,
    Filter, Intersect, Join, JoinKind, Limit, Project, Sort, TableScan, Tail, Union, WithColumns,
};
use crate::types::{DataType, StructField, StructType, TypeInferenceEngine};

// ── Public aliases ────────────────────────────────────────────────────────────

/// Catalog seed — the Spark-typed schemas for base relations the
/// analyzer's `TableScan`s reference. See ADR-012 (overlay) — Slice B
/// consumes this as a `&HashMap`; the real overlay lands in Slice H.
///
/// **Fallback-only overlay semantics (Slice C.2 OPT-M3):** the analyzer's
/// [`resolve_table_scan`] prefers the AST-carried [`TableScan::schema`]
/// when it is non-empty; this overlay is consulted **only** when a scan's
/// AST schema is [`StructType::empty()`]. That means callers that
/// construct a plan whose every scan carries its own schema can skip
/// seeding this overlay entirely — the walk that populates it is dead
/// work for the common case. See `service.rs::build_base_types_from_plan`
/// for the short-circuit that materialises this contract.
pub type BaseTypes = std::collections::HashMap<String, StructType>;

/// A resolved schema — the output shape of an operator. Alias to
/// [`StructType`] so it composes with legacy inference helpers.
pub type Schema = StructType;

/// Spark type + nullability for a single expression slot.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAttr {
    /// Inferred Spark `DataType`.
    pub data_type: DataType,
    /// Nullability.
    pub nullable: bool,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors surfaced by the analyzer. `thiserror`-typed per crate convention.
///
/// Every variant carries enough context to diagnose without a debugger.
/// These errors are **local to the v2 analyzer** and do NOT leak into
/// [`crate::error::ThunderduckError`] in this slice — the coder should
/// wrap at the `transpiler_v2::generate` boundary in a later slice (C).
#[derive(thiserror::Error, Debug)]
pub enum AnalyzerError {
    /// A column name did not resolve against the current operator's schema.
    #[error("column `{name}` not found in schema fields {schema_fields:?}")]
    UnresolvedColumn {
        /// Column name looked up.
        name: String,
        /// Field names of the schema against which resolution failed.
        schema_fields: Vec<String>,
    },

    /// A column name matched more than one field in the current schema.
    #[error("column `{name}` is ambiguous in schema; candidates: {candidates:?}")]
    AmbiguousColumn {
        /// Column name looked up.
        name: String,
        /// Field names that all matched.
        candidates: Vec<String>,
    },

    /// A `TableScan` referenced a name that was not seeded in `BaseTypes`.
    #[error("table `{name}` not found in base-types catalog")]
    UnknownTable {
        /// Table name that was not in the catalog.
        name: String,
    },

    /// A set-operation (`UNION`/`INTERSECT`/`EXCEPT`) had mismatched arity.
    #[error(
        "set-op {op} requires matching column counts: left has {left_cols}, right has {right_cols}"
    )]
    SetOpArityMismatch {
        /// Which set-op raised the error.
        op: &'static str,
        /// Column count of the left input.
        left_cols: usize,
        /// Column count of the right input.
        right_cols: usize,
    },

    /// The AST contains an operator kind Slice B does not cover.
    #[error("operator `{kind}` is out of scope for Slice B: {reason}")]
    PuntedOperator {
        /// Diagnostic name of the punted operator.
        kind: &'static str,
        /// Why it is out of scope for the current slice.
        reason: &'static str,
    },

    /// A type mismatch was detected at analysis time.
    #[error("type mismatch in {context}: expected {expected}, got {actual}")]
    TypeMismatch {
        /// Expected Spark type.
        expected: DataType,
        /// Actual Spark type inferred.
        actual: DataType,
        /// Human-readable context (e.g., "filter predicate").
        context: &'static str,
    },
}

// ── TypedAst ──────────────────────────────────────────────────────────────────

/// The analyzed AST. Structurally parallel to [`CommonAst`] but each op
/// carries its output [`Schema`] and each expression slot its [`TypedAttr`].
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAst {
    /// Root typed operator.
    pub root: TypedOp,
}

/// Operators after analysis. Structurally 1:1 with
/// [`crate::transpiler_v2::ast::CommonOp`] (Slice B does not add or remove
/// operator kinds), but each carries a `schema` and each expression list is
/// paired with a matching `Vec<TypedAttr>`.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedOp {
    /// Typed projection with per-slot types.
    Project {
        /// Input operator.
        input: Box<TypedOp>,
        /// Projection expressions (star-expanded internally by Pass 1 only
        /// for schema purposes; the expression list is left as-is so the
        /// emitter can decide whether to emit `*` or expanded refs).
        projections: Vec<Expression>,
        /// One entry per projection; length matches `projections`.
        projection_types: Vec<TypedAttr>,
        /// Output schema.
        schema: Schema,
    },
    /// Typed filter.
    Filter {
        /// Input operator.
        input: Box<TypedOp>,
        /// Predicate expression.
        predicate: Expression,
        /// Output schema (unchanged from input).
        schema: Schema,
    },
    /// Typed join.
    Join {
        /// Left input.
        left: Box<TypedOp>,
        /// Right input.
        right: Box<TypedOp>,
        /// Join kind.
        join_type: JoinKind,
        /// Optional `ON` predicate.
        on: Option<Expression>,
        /// USING columns.
        using: Vec<String>,
        /// Merged output schema with outer-join nullability applied by Pass 3.
        schema: Schema,
    },
    /// Typed aggregate.
    Aggregate {
        /// Input operator.
        input: Box<TypedOp>,
        /// Grouping expressions.
        grouping: Vec<Expression>,
        /// One entry per grouping expression.
        grouping_types: Vec<TypedAttr>,
        /// Aggregate calls.
        aggregates: Vec<AggregateCall>,
        /// One entry per aggregate call.
        aggregate_types: Vec<TypedAttr>,
        /// Optional `HAVING` clause.
        having: Option<Expression>,
        /// Optional ROLLUP/CUBE/GROUPING SETS spec.
        grouping_sets: Option<GroupingSets>,
        /// Output schema.
        schema: Schema,
    },
    /// Typed sort.
    Sort {
        /// Input operator.
        input: Box<TypedOp>,
        /// Sort expressions.
        order: Vec<SortOrder>,
        /// Optional `LIMIT`.
        limit: Option<Expression>,
        /// Optional `OFFSET`.
        offset: Option<Expression>,
        /// Output schema (unchanged from input).
        schema: Schema,
    },
    /// Typed `LIMIT`.
    Limit {
        /// Input operator.
        input: Box<TypedOp>,
        /// Row count expression.
        n: Expression,
        /// Output schema (unchanged from input).
        schema: Schema,
    },
    /// Typed `TAIL`.
    Tail {
        /// Input operator.
        input: Box<TypedOp>,
        /// Row count expression.
        n: Expression,
        /// Output schema (unchanged from input).
        schema: Schema,
    },
    /// Typed union.
    Union {
        /// Left input.
        left: Box<TypedOp>,
        /// Right input.
        right: Box<TypedOp>,
        /// `UNION ALL` when true.
        all: bool,
        /// `unionByName` when true — Pass 2 reorders the right child by
        /// column name before widening (M4).
        by_name: bool,
        /// Widened output schema (per-field type = `unify_types(l, r)`,
        /// nullable = `l.nullable || r.nullable`).
        schema: Schema,
    },
    /// Typed intersect.
    Intersect {
        /// Left input.
        left: Box<TypedOp>,
        /// Right input.
        right: Box<TypedOp>,
        /// `INTERSECT ALL` when true.
        all: bool,
        /// Output schema (left's schema, per Spark semantics).
        schema: Schema,
    },
    /// Typed except.
    Except {
        /// Left input.
        left: Box<TypedOp>,
        /// Right input.
        right: Box<TypedOp>,
        /// `EXCEPT ALL` when true.
        all: bool,
        /// Output schema (left's schema, per Spark semantics).
        schema: Schema,
    },
    /// Typed distinct.
    Distinct {
        /// Input operator.
        input: Box<TypedOp>,
        /// Empty = full-row distinct; non-empty = subset key.
        on: Vec<Expression>,
        /// Output schema (unchanged from input).
        schema: Schema,
    },
    /// Typed `withColumns`.
    WithColumns {
        /// Input operator.
        input: Box<TypedOp>,
        /// Ordered `(name, expression, typed-attr)` triples.
        columns: Vec<(String, Expression, TypedAttr)>,
        /// Output schema after applying every column in order.
        schema: Schema,
    },
    /// Typed `dropColumns`.
    DropColumns {
        /// Input operator.
        input: Box<TypedOp>,
        /// Column names to drop.
        names: Vec<String>,
        /// Output schema (input minus dropped columns).
        schema: Schema,
    },
    /// Typed aliased relation.
    AliasedRelation {
        /// Input operator.
        input: Box<TypedOp>,
        /// Relation alias.
        alias: String,
        /// Positional column aliases.
        column_aliases: Vec<String>,
        /// Output schema (input with any positional renames applied).
        schema: Schema,
    },
    /// Typed table scan.
    TableScan {
        /// Table name.
        name: String,
        /// Output schema pulled from the catalog seed.
        schema: Schema,
    },
    /// Typed local relation.
    LocalRelation {
        /// Precomputed output schema.
        schema: Schema,
    },
    /// Typed range relation.
    RangeRelation {
        /// Start (inclusive).
        start: i64,
        /// End (exclusive).
        end: i64,
        /// Step.
        step: i64,
        /// Fixed output schema `id: Long NOT NULL`.
        schema: Schema,
    },
}

impl TypedOp {
    /// Output schema of this operator — the type-source for the parent
    /// operator's pass. Callers do NOT recompute; they read this.
    pub fn schema(&self) -> &Schema {
        match self {
            TypedOp::Project { schema, .. }
            | TypedOp::Filter { schema, .. }
            | TypedOp::Join { schema, .. }
            | TypedOp::Aggregate { schema, .. }
            | TypedOp::Sort { schema, .. }
            | TypedOp::Limit { schema, .. }
            | TypedOp::Tail { schema, .. }
            | TypedOp::Union { schema, .. }
            | TypedOp::Intersect { schema, .. }
            | TypedOp::Except { schema, .. }
            | TypedOp::Distinct { schema, .. }
            | TypedOp::WithColumns { schema, .. }
            | TypedOp::DropColumns { schema, .. }
            | TypedOp::AliasedRelation { schema, .. }
            | TypedOp::TableScan { schema, .. }
            | TypedOp::LocalRelation { schema, .. }
            | TypedOp::RangeRelation { schema, .. } => schema,
        }
    }
}

/// Sealed helper trait so `TypedOp` and `TypedAst` share one schema accessor.
pub(crate) trait HasSchema {
    /// Output schema.
    fn schema(&self) -> &Schema;
}

impl HasSchema for TypedOp {
    fn schema(&self) -> &Schema {
        TypedOp::schema(self)
    }
}

impl HasSchema for TypedAst {
    fn schema(&self) -> &Schema {
        self.root.schema()
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Slice B entry point. Runs the three bounded passes (§6) and returns the
/// analyzed AST or the first typed error.
///
/// The passes:
/// 1. `resolve` — bottom-up structural pass producing per-operator schemas.
/// 2. `assign_types` — bottom-up typing pass plus one downward re-sweep at
///    every `Union` node (ADR-006's set-op widening exception).
/// 3. `derive_nullability` — bottom-up nullability rewrite for outer joins
///    and grouping sets (aggregate/CASE-WHEN nullability is already
///    correct because Pass 2 delegates to
///    [`Expression::nullable`](crate::expression::Expression::nullable)).
///
/// Non-`pub` today; promote to `pub(crate)` when Slice C wires emission.
pub(crate) fn analyze(ast: CommonAst, base_types: &BaseTypes) -> Result<TypedAst, AnalyzerError> {
    let root = resolve(ast.root, base_types)?;
    let root = assign_types(root)?;
    let root = derive_nullability(root)?;
    Ok(TypedAst { root })
}

/// [INV5 §CV.5] — returns `true` iff no field in any operator's schema
/// carries [`DataType::Unresolved`] (transitively). Walks the whole tree;
/// the caller does not have to.
pub fn has_resolved_schema(ast: &TypedAst) -> bool {
    walk_resolved(&ast.root)
}

fn walk_resolved(op: &TypedOp) -> bool {
    // Prefer the trait-mediated accessor so `has_resolved_schema` uses the
    // sealed `HasSchema` surface — the same one Slice C's emitter will use.
    if <TypedOp as HasSchema>::schema(op)
        .fields
        .iter()
        .any(|f| f.data_type.contains_unresolved())
    {
        return false;
    }
    match op {
        TypedOp::Project {
            input,
            projections,
            projection_types,
            ..
        } => {
            // M1 — `type_projection_list` writes a placeholder `TypedAttr`
            // with `DataType::Unresolved` for bare `Expression::Star` slots
            // because a star slot is not a single-value expression. The
            // walker must skip those slots when checking for Unresolved —
            // otherwise every `SELECT *` trips INV5 spuriously.
            walk_resolved(input)
                && projections
                    .iter()
                    .zip(projection_types.iter())
                    .all(|(e, t)| {
                        matches!(e, Expression::Star(_)) || !t.data_type.contains_unresolved()
                    })
        }
        TypedOp::Filter { input, .. }
        | TypedOp::Sort { input, .. }
        | TypedOp::Limit { input, .. }
        | TypedOp::Tail { input, .. }
        | TypedOp::Distinct { input, .. }
        | TypedOp::DropColumns { input, .. }
        | TypedOp::AliasedRelation { input, .. } => walk_resolved(input),
        TypedOp::Join { left, right, .. }
        | TypedOp::Union { left, right, .. }
        | TypedOp::Intersect { left, right, .. }
        | TypedOp::Except { left, right, .. } => walk_resolved(left) && walk_resolved(right),
        TypedOp::Aggregate {
            input,
            grouping_types,
            aggregate_types,
            ..
        } => {
            walk_resolved(input)
                && grouping_types
                    .iter()
                    .all(|t| !t.data_type.contains_unresolved())
                && aggregate_types
                    .iter()
                    .all(|t| !t.data_type.contains_unresolved())
        }
        TypedOp::WithColumns { input, columns, .. } => {
            walk_resolved(input)
                && columns
                    .iter()
                    .all(|(_, _, t)| !t.data_type.contains_unresolved())
        }
        TypedOp::TableScan { .. }
        | TypedOp::LocalRelation { .. }
        | TypedOp::RangeRelation { .. } => true,
    }
}

/// [INV4 §CV.5] — inference-only smoke test.
///
/// Runs the analyzer over five mini-fixtures drawn from the DataFrame corpus
/// (`type-001`, `cond-003`, `agg-013`, `type-011` outer join, `type-019`
/// union widening). Panics with a rich diff if any produced schema disagrees
/// with the expected literal Spark schema.
///
/// Called from
/// [`crate::transpiler_v2::invariants::inv4_inference_validated_in_isolation`]
/// and available as an ordinary `#[test] fn` inside the module's tests.
pub fn inference_smoke() {
    self::analyzer_fixtures::run_all();
}

#[path = "analyzer_fixtures.rs"]
pub mod analyzer_fixtures;

// ── Pass 1: resolve ───────────────────────────────────────────────────────────

/// Pass 1 — bottom-up structural resolution: name-resolve columns,
/// star-expand, join-merge, alias-honor. Types on ambiguous slots stay
/// `DataType::Unresolved`; Pass 2 fills them in.
fn resolve(op: CommonOp, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    match op {
        CommonOp::TableScan(t) => resolve_table_scan(t, base_types),
        CommonOp::LocalRelation(l) => Ok(TypedOp::LocalRelation { schema: l.schema }),
        CommonOp::RangeRelation(r) => Ok(TypedOp::RangeRelation {
            start: r.start,
            end: r.end,
            step: r.step,
            schema: StructType::new(vec![StructField::not_null("id", DataType::Long)]),
        }),
        CommonOp::Project(p) => resolve_project(p, base_types),
        CommonOp::Filter(f) => resolve_filter(f, base_types),
        CommonOp::Join(j) => resolve_join(j, base_types),
        CommonOp::Aggregate(a) => resolve_aggregate(a, base_types),
        CommonOp::Sort(s) => resolve_sort(s, base_types),
        CommonOp::Limit(l) => resolve_limit(l, base_types),
        CommonOp::Tail(t) => resolve_tail(t, base_types),
        CommonOp::Union(u) => resolve_union(u, base_types),
        CommonOp::Intersect(i) => resolve_intersect(i, base_types),
        CommonOp::Except(e) => resolve_except(e, base_types),
        CommonOp::Distinct(d) => resolve_distinct(d, base_types),
        CommonOp::WithColumns(w) => resolve_with_columns(w, base_types),
        CommonOp::DropColumns(d) => resolve_drop_columns(d, base_types),
        CommonOp::AliasedRelation(a) => resolve_aliased_relation(a, base_types),
        CommonOp::Punt { kind, reason } => Err(AnalyzerError::PuntedOperator { kind, reason }),
    }
}

fn resolve_table_scan(t: TableScan, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    // Prefer the caller-supplied schema on the AST node; if empty, consult
    // the catalog seed. Failing both, error.
    let schema = if !t.schema.is_empty() {
        t.schema
    } else if let Some(s) = base_types.get(&t.name) {
        s.clone()
    } else {
        return Err(AnalyzerError::UnknownTable { name: t.name });
    };
    Ok(TypedOp::TableScan {
        name: t.name,
        schema,
    })
}

fn resolve_project(p: Project, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    // M6 — Pass 1 no longer seeds `Unresolved` fields that Pass 2 overwrites.
    // Pass 2's `assign_types_project` builds `schema` and `projection_types`
    // from the resolved child schema; we hand it empty vectors so there is a
    // single writer for each. Star expansion for schema purposes happens in
    // Pass 2 as well (`type_projection_list`).
    let input = resolve(*p.input, base_types)?;
    Ok(TypedOp::Project {
        input: Box::new(input),
        projections: p.projections,
        projection_types: Vec::new(),
        schema: StructType::empty(),
    })
}

fn resolve_filter(f: Filter, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let input = resolve(*f.input, base_types)?;
    let schema = input.schema().clone();
    Ok(TypedOp::Filter {
        input: Box::new(input),
        predicate: f.predicate,
        schema,
    })
}

fn resolve_join(j: Join, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let left = resolve(*j.left, base_types)?;
    let right = resolve(*j.right, base_types)?;

    let left_schema = left.schema().clone();
    let right_schema = right.schema().clone();

    // Semi/anti return only the left schema.
    let schema = if matches!(j.join_type, JoinKind::LeftSemi | JoinKind::LeftAnti) {
        left_schema
    } else if j.using.is_empty() {
        StructType::merge(&left_schema, &right_schema)
    } else {
        // USING columns first (from left), then non-USING left, then non-USING right.
        let mut fields = Vec::new();
        for name in &j.using {
            if let Some(f) = left_schema
                .fields
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(name))
            {
                fields.push(f.clone());
            }
        }
        for f in &left_schema.fields {
            if !j.using.iter().any(|n| f.name.eq_ignore_ascii_case(n)) {
                fields.push(f.clone());
            }
        }
        for f in &right_schema.fields {
            if !j.using.iter().any(|n| f.name.eq_ignore_ascii_case(n)) {
                fields.push(f.clone());
            }
        }
        StructType::new(fields)
    };
    Ok(TypedOp::Join {
        left: Box::new(left),
        right: Box::new(right),
        join_type: j.join_type,
        on: j.on,
        using: j.using,
        schema,
    })
}

fn resolve_aggregate(a: Aggregate, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    // M6 — Pass 1 no longer seeds `Unresolved` schema / grouping_types /
    // aggregate_types that Pass 2 will overwrite. Pass 2's
    // `assign_types` (Aggregate arm) calls `type_aggregate` against the
    // resolved child schema and produces all three vectors in one place.
    let input = resolve(*a.input, base_types)?;
    Ok(TypedOp::Aggregate {
        input: Box::new(input),
        grouping: a.grouping,
        grouping_types: Vec::new(),
        aggregates: a.aggregates,
        aggregate_types: Vec::new(),
        having: a.having,
        grouping_sets: a.grouping_sets,
        schema: StructType::empty(),
    })
}

fn resolve_sort(s: Sort, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let input = resolve(*s.input, base_types)?;
    let schema = input.schema().clone();
    Ok(TypedOp::Sort {
        input: Box::new(input),
        order: s.order,
        limit: s.limit,
        offset: s.offset,
        schema,
    })
}

fn resolve_limit(l: Limit, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let input = resolve(*l.input, base_types)?;
    let schema = input.schema().clone();
    Ok(TypedOp::Limit {
        input: Box::new(input),
        n: l.n,
        schema,
    })
}

fn resolve_tail(t: Tail, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let input = resolve(*t.input, base_types)?;
    let schema = input.schema().clone();
    Ok(TypedOp::Tail {
        input: Box::new(input),
        n: t.n,
        schema,
    })
}

fn resolve_union(u: Union, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    // M4 — Propagate `by_name` through to `TypedOp::Union`. The actual
    // name-match reorder happens in Pass 2's Union arm (`assign_types`)
    // because Pass 1's Project schemas are empty (M6 defers filling
    // them to Pass 2); by the time Pass 2 sees this Union, both children
    // have resolved schemas the reorder can key on.
    let left = resolve(*u.left, base_types)?;
    let right = resolve(*u.right, base_types)?;
    Ok(TypedOp::Union {
        left: Box::new(left),
        right: Box::new(right),
        all: u.all,
        by_name: u.by_name,
        schema: StructType::empty(),
    })
}

fn resolve_intersect(i: Intersect, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let left = resolve(*i.left, base_types)?;
    let right = resolve(*i.right, base_types)?;
    let left_schema = left.schema().clone();
    if left_schema.fields.len() != right.schema().fields.len() {
        return Err(AnalyzerError::SetOpArityMismatch {
            op: "INTERSECT",
            left_cols: left_schema.fields.len(),
            right_cols: right.schema().fields.len(),
        });
    }
    Ok(TypedOp::Intersect {
        left: Box::new(left),
        right: Box::new(right),
        all: i.all,
        schema: left_schema,
    })
}

fn resolve_except(e: Except, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let left = resolve(*e.left, base_types)?;
    let right = resolve(*e.right, base_types)?;
    let left_schema = left.schema().clone();
    if left_schema.fields.len() != right.schema().fields.len() {
        return Err(AnalyzerError::SetOpArityMismatch {
            op: "EXCEPT",
            left_cols: left_schema.fields.len(),
            right_cols: right.schema().fields.len(),
        });
    }
    Ok(TypedOp::Except {
        left: Box::new(left),
        right: Box::new(right),
        all: e.all,
        schema: left_schema,
    })
}

fn resolve_distinct(d: Distinct, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let input = resolve(*d.input, base_types)?;
    let schema = input.schema().clone();
    Ok(TypedOp::Distinct {
        input: Box::new(input),
        on: d.on,
        schema,
    })
}

fn resolve_with_columns(w: WithColumns, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    // M6 — Pass 1 no longer seeds per-column TypedAttrs or the output schema
    // that Pass 2's `assign_types` (WithColumns arm) overwrites. Pass 1 only
    // recurses into the input; Pass 2 walks the columns against the resolved
    // child schema and produces the final `columns` triples + `schema`.
    let input = resolve(*w.input, base_types)?;
    // Pass 1 stores placeholder `Unresolved` TypedAttrs so Pass 2 can shred
    // them without losing the raw `(name, expr)` list.  We prefer this over
    // introducing a separate WithColumns type just for the Pass-1 shape.
    let placeholder_columns: Vec<(String, Expression, TypedAttr)> = w
        .columns
        .into_iter()
        .map(|(name, expr)| {
            (
                name,
                expr,
                TypedAttr {
                    data_type: DataType::Unresolved,
                    nullable: true,
                },
            )
        })
        .collect();
    Ok(TypedOp::WithColumns {
        input: Box::new(input),
        columns: placeholder_columns,
        schema: StructType::empty(),
    })
}

/// Resolve one `(new_name, expr)` slot in a `WithColumns`, honoring the legacy
/// rename detection (pure column ref with a different name preserves the old
/// column's type and nullability).
fn resolve_with_columns_slot(
    new_name: &str,
    expr: &Expression,
    schema: &StructType,
) -> (DataType, bool) {
    let old_col_name: Option<&str> = match expr {
        Expression::UnresolvedColumn(uc) if uc.name != new_name => Some(&uc.name),
        Expression::ColumnReference(cr) if cr.name != new_name => Some(&cr.name),
        _ => None,
    };
    if let Some(old_name) = old_col_name {
        if let Some(f) = schema.field_by_name(old_name) {
            return (f.data_type.clone(), f.nullable);
        }
    }
    (expr.data_type(schema), expr.nullable(schema))
}

fn resolve_drop_columns(d: DropColumns, base_types: &BaseTypes) -> Result<TypedOp, AnalyzerError> {
    let input = resolve(*d.input, base_types)?;
    let child = input.schema().clone();
    let schema = StructType::new(
        child
            .fields
            .into_iter()
            .filter(|f| !d.names.iter().any(|n| f.name.eq_ignore_ascii_case(n)))
            .collect(),
    );
    Ok(TypedOp::DropColumns {
        input: Box::new(input),
        names: d.names,
        schema,
    })
}

fn resolve_aliased_relation(
    a: AliasedRelation,
    base_types: &BaseTypes,
) -> Result<TypedOp, AnalyzerError> {
    let input = resolve(*a.input, base_types)?;
    let child = input.schema().clone();
    let schema = if !a.column_aliases.is_empty() && a.column_aliases.len() == child.fields.len() {
        let fields = child
            .fields
            .into_iter()
            .zip(&a.column_aliases)
            .map(|(mut f, name)| {
                f.name = name.clone();
                f
            })
            .collect();
        StructType::new(fields)
    } else {
        child
    };
    Ok(TypedOp::AliasedRelation {
        input: Box::new(input),
        alias: a.alias,
        column_aliases: a.column_aliases,
        schema,
    })
}

// ── Pass 2: assign_types ──────────────────────────────────────────────────────

/// Pass 2 — bottom-up typing pass. For every expression slot, delegate to
/// `Expression::data_type(&resolved_schema)` and `Expression::nullable(...)`.
/// A downward re-typing sub-pass at every `Union` widens both children's
/// projection lists to the union's widened field types (ADR-006 line 168).
fn assign_types(op: TypedOp) -> Result<TypedOp, AnalyzerError> {
    match op {
        TypedOp::TableScan { .. }
        | TypedOp::LocalRelation { .. }
        | TypedOp::RangeRelation { .. } => Ok(op),
        TypedOp::Project {
            input,
            projections,
            projection_types: _,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let child_schema = input.schema().clone();
            // M5 — Ambiguous-column check: every unqualified column
            // reference in the projection list that matches more than one
            // field in the child schema is a hard error.
            for expr in &projections {
                ensure_no_ambiguous_columns(expr, &child_schema)?;
            }
            let (fields, projection_types) = type_projection_list(&projections, &child_schema);
            Ok(TypedOp::Project {
                input: Box::new(input),
                projections,
                projection_types,
                schema: StructType::new(fields),
            })
        }
        TypedOp::Filter {
            input,
            predicate,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let schema = input.schema().clone();
            // M5 — Predicate must yield boolean. Give up loudly if not; this
            // is a real analyzer bug, not a Punt (see `is_fallback_eligible`
            // in the dispatch wrapper).
            let predicate_type = predicate.data_type(&schema);
            if !matches!(
                predicate_type,
                DataType::Boolean | DataType::Unresolved | DataType::Null
            ) {
                return Err(AnalyzerError::TypeMismatch {
                    expected: DataType::Boolean,
                    actual: predicate_type,
                    context: "filter predicate",
                });
            }
            Ok(TypedOp::Filter {
                input: Box::new(input),
                predicate,
                schema,
            })
        }
        TypedOp::Join {
            left,
            right,
            join_type,
            on,
            using,
            schema: _,
        } => {
            let left = assign_types(*left)?;
            let right = assign_types(*right)?;
            let schema = compute_join_output_schema(&left, &right, join_type, &using);
            Ok(TypedOp::Join {
                left: Box::new(left),
                right: Box::new(right),
                join_type,
                on,
                using,
                schema,
            })
        }
        TypedOp::Aggregate {
            input,
            grouping,
            grouping_types: _,
            aggregates,
            aggregate_types: _,
            having,
            grouping_sets,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let child_schema = input.schema().clone();
            let (fields, grouping_types, aggregate_types) =
                type_aggregate(&grouping, &aggregates, &child_schema);
            Ok(TypedOp::Aggregate {
                input: Box::new(input),
                grouping,
                grouping_types,
                aggregates,
                aggregate_types,
                having,
                grouping_sets,
                schema: StructType::new(fields),
            })
        }
        TypedOp::Sort {
            input,
            order,
            limit,
            offset,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let schema = input.schema().clone();
            Ok(TypedOp::Sort {
                input: Box::new(input),
                order,
                limit,
                offset,
                schema,
            })
        }
        TypedOp::Limit {
            input,
            n,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let schema = input.schema().clone();
            Ok(TypedOp::Limit {
                input: Box::new(input),
                n,
                schema,
            })
        }
        TypedOp::Tail {
            input,
            n,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let schema = input.schema().clone();
            Ok(TypedOp::Tail {
                input: Box::new(input),
                n,
                schema,
            })
        }
        TypedOp::Union {
            left,
            right,
            all,
            by_name,
            schema: _,
        } => {
            let left = assign_types(*left)?;
            let mut right = assign_types(*right)?;

            // M4 — `unionByName` reorder: right's fields get remapped to
            // match left's positional order by (case-insensitive) name
            // *before* positional widening runs.
            if by_name {
                right = reorder_union_right_by_name(right, left.schema())?;
            }

            // Downward re-sweep: widen this union's fields, then update
            // both children's field types to the widened types where they
            // exit the projection layer. The children's projection lists
            // themselves are unchanged; only their `projection_types` and
            // per-op schemas are widened at the union boundary.
            let widened = widen_union_fields(left.schema(), right.schema())?;
            let left = repropagate_union_widening(left, &widened);
            let right = repropagate_union_widening(right, &widened);
            Ok(TypedOp::Union {
                left: Box::new(left),
                right: Box::new(right),
                all,
                by_name,
                schema: widened,
            })
        }
        TypedOp::Intersect {
            left,
            right,
            all,
            schema: _,
        } => {
            let left = assign_types(*left)?;
            let right = assign_types(*right)?;
            let schema = left.schema().clone();
            Ok(TypedOp::Intersect {
                left: Box::new(left),
                right: Box::new(right),
                all,
                schema,
            })
        }
        TypedOp::Except {
            left,
            right,
            all,
            schema: _,
        } => {
            let left = assign_types(*left)?;
            let right = assign_types(*right)?;
            let schema = left.schema().clone();
            Ok(TypedOp::Except {
                left: Box::new(left),
                right: Box::new(right),
                all,
                schema,
            })
        }
        TypedOp::Distinct {
            input,
            on,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let schema = input.schema().clone();
            Ok(TypedOp::Distinct {
                input: Box::new(input),
                on,
                schema,
            })
        }
        TypedOp::WithColumns {
            input,
            columns,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let mut schema = input.schema().clone();
            let mut typed_columns: Vec<(String, Expression, TypedAttr)> =
                Vec::with_capacity(columns.len());
            for (new_name, expr, _prior) in columns {
                let (dt, nullable) = resolve_with_columns_slot(&new_name, &expr, &schema);
                let attr = TypedAttr {
                    data_type: dt.clone(),
                    nullable,
                };
                if let Some(idx) = schema.field_index(&new_name) {
                    schema.fields[idx] = StructField::new(new_name.clone(), dt, nullable);
                } else {
                    schema
                        .fields
                        .push(StructField::new(new_name.clone(), dt, nullable));
                }
                typed_columns.push((new_name, expr, attr));
            }
            Ok(TypedOp::WithColumns {
                input: Box::new(input),
                columns: typed_columns,
                schema,
            })
        }
        TypedOp::DropColumns {
            input,
            names,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let child = input.schema().clone();
            let schema = StructType::new(
                child
                    .fields
                    .into_iter()
                    .filter(|f| !names.iter().any(|n| f.name.eq_ignore_ascii_case(n)))
                    .collect(),
            );
            Ok(TypedOp::DropColumns {
                input: Box::new(input),
                names,
                schema,
            })
        }
        TypedOp::AliasedRelation {
            input,
            alias,
            column_aliases,
            schema: _,
        } => {
            let input = assign_types(*input)?;
            let child = input.schema().clone();
            let schema = if !column_aliases.is_empty() && column_aliases.len() == child.fields.len()
            {
                let fields = child
                    .fields
                    .into_iter()
                    .zip(&column_aliases)
                    .map(|(mut f, name)| {
                        f.name = name.clone();
                        f
                    })
                    .collect();
                StructType::new(fields)
            } else {
                child
            };
            Ok(TypedOp::AliasedRelation {
                input: Box::new(input),
                alias,
                column_aliases,
                schema,
            })
        }
    }
}

/// Type a projection list against the child schema, mirroring
/// `projection_to_field` from the legacy `logical` module.
fn type_projection_list(
    projections: &[Expression],
    child_schema: &StructType,
) -> (Vec<StructField>, Vec<TypedAttr>) {
    let mut fields = Vec::new();
    let mut attrs = Vec::with_capacity(projections.len());
    for expr in projections {
        match expr {
            Expression::Star(_) => {
                // Star expands to the child's fields verbatim.
                fields.extend(child_schema.fields.iter().cloned());
                // The projection-slot itself is not a single-value expression;
                // record the child schema's type-of-first-field or Unresolved
                // to keep the vec aligned with the projection list.
                attrs.push(TypedAttr {
                    data_type: DataType::Unresolved,
                    nullable: true,
                });
            }
            Expression::Alias(a) => {
                let dt = a.expr.data_type(child_schema);
                let nullable = a.expr.nullable(child_schema);
                fields.push(StructField::new(a.alias.clone(), dt.clone(), nullable));
                attrs.push(TypedAttr {
                    data_type: dt,
                    nullable,
                });
            }
            Expression::ColumnReference(c) => {
                let dt = if c.data_type != DataType::Unresolved {
                    c.data_type.clone()
                } else {
                    TypeInferenceEngine::column_type(&c.name, child_schema)
                };
                let nullable = TypeInferenceEngine::column_nullable(&c.name, child_schema);
                fields.push(StructField::new(c.name.clone(), dt.clone(), nullable));
                attrs.push(TypedAttr {
                    data_type: dt,
                    nullable,
                });
            }
            Expression::UnresolvedColumn(u) => {
                let dt = TypeInferenceEngine::column_type(&u.name, child_schema);
                let nullable = TypeInferenceEngine::column_nullable(&u.name, child_schema);
                fields.push(StructField::new(u.name.clone(), dt.clone(), nullable));
                attrs.push(TypedAttr {
                    data_type: dt,
                    nullable,
                });
            }
            other => {
                let dt = other.data_type(child_schema);
                let nullable = other.nullable(child_schema);
                fields.push(StructField::new(
                    spark_column_name(other),
                    dt.clone(),
                    nullable,
                ));
                attrs.push(TypedAttr {
                    data_type: dt,
                    nullable,
                });
            }
        }
    }
    (fields, attrs)
}

/// Type the grouping + aggregate expressions of an `Aggregate`.
fn type_aggregate(
    grouping: &[Expression],
    aggregates: &[AggregateCall],
    child_schema: &StructType,
) -> (Vec<StructField>, Vec<TypedAttr>, Vec<TypedAttr>) {
    let mut fields = Vec::new();
    let mut grouping_types = Vec::with_capacity(grouping.len());
    for g in grouping {
        let (dt, nullable, name) = match g {
            Expression::Alias(a) => (
                a.expr.data_type(child_schema),
                a.expr.nullable(child_schema),
                a.alias.clone(),
            ),
            Expression::ColumnReference(c) => (
                if c.data_type != DataType::Unresolved {
                    c.data_type.clone()
                } else {
                    TypeInferenceEngine::column_type(&c.name, child_schema)
                },
                TypeInferenceEngine::column_nullable(&c.name, child_schema),
                c.name.clone(),
            ),
            Expression::UnresolvedColumn(u) => (
                TypeInferenceEngine::column_type(&u.name, child_schema),
                TypeInferenceEngine::column_nullable(&u.name, child_schema),
                u.name.clone(),
            ),
            other => (
                other.data_type(child_schema),
                other.nullable(child_schema),
                spark_column_name(other),
            ),
        };
        fields.push(StructField::new(name, dt.clone(), nullable));
        grouping_types.push(TypedAttr {
            data_type: dt,
            nullable,
        });
    }
    let mut aggregate_types = Vec::with_capacity(aggregates.len());
    for agg in aggregates {
        let (dt, nullable, name) = type_aggregate_call(&agg.func, child_schema);
        fields.push(StructField::new(name, dt.clone(), nullable));
        aggregate_types.push(TypedAttr {
            data_type: dt,
            nullable,
        });
    }
    (fields, grouping_types, aggregate_types)
}

/// Type a single aggregate call — mirrors `agg_expr_to_field` from legacy.
fn type_aggregate_call(func: &Expression, child_schema: &StructType) -> (DataType, bool, String) {
    match func {
        Expression::Alias(a) => match a.expr.as_ref() {
            Expression::FunctionCall(f) => {
                let arg_types: Vec<_> = f.args.iter().map(|e| e.data_type(child_schema)).collect();
                let dt = TypeInferenceEngine::aggregate_return_type(
                    &f.name,
                    arg_types.first().unwrap_or(&DataType::Unresolved),
                );
                (dt, a.expr.nullable(child_schema), a.alias.clone())
            }
            other => (
                other.data_type(child_schema),
                other.nullable(child_schema),
                a.alias.clone(),
            ),
        },
        Expression::FunctionCall(f) => {
            let arg_types: Vec<_> = f.args.iter().map(|e| e.data_type(child_schema)).collect();
            let dt = TypeInferenceEngine::aggregate_return_type(
                &f.name,
                arg_types.first().unwrap_or(&DataType::Unresolved),
            );
            (dt, func.nullable(child_schema), spark_column_name(func))
        }
        other => (
            other.data_type(child_schema),
            other.nullable(child_schema),
            spark_column_name(other),
        ),
    }
}

/// Merge a join's left+right schemas with USING dedup. Outer-join nullability
/// is applied by Pass 3.
fn compute_join_output_schema(
    left: &TypedOp,
    right: &TypedOp,
    join_type: JoinKind,
    using: &[String],
) -> Schema {
    let left_schema = left.schema();
    let right_schema = right.schema();

    if matches!(join_type, JoinKind::LeftSemi | JoinKind::LeftAnti) {
        return left_schema.clone();
    }
    if using.is_empty() {
        return StructType::merge(left_schema, right_schema);
    }

    // M2 — For RIGHT joins the USING key survives on the *right* side of the
    // join (LEFT + USING keeps left's copy; RIGHT + USING keeps right's copy).
    // Pass 3's outer-join nullability rewrite marks the *left* side nullable
    // for a RIGHT join, so dedupping from the left would incorrectly promote
    // the USING key to nullable. Choose the right's copy for RIGHT + USING.
    let (using_source, using_source_nonusing, other_nonusing) =
        if matches!(join_type, JoinKind::Right) {
            (right_schema, right_schema, left_schema)
        } else {
            (left_schema, left_schema, right_schema)
        };

    let mut fields = Vec::new();
    for name in using {
        if let Some(f) = using_source
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
        {
            fields.push(f.clone());
        }
    }
    for f in &using_source_nonusing.fields {
        if !using.iter().any(|n| f.name.eq_ignore_ascii_case(n)) {
            fields.push(f.clone());
        }
    }
    for f in &other_nonusing.fields {
        if !using.iter().any(|n| f.name.eq_ignore_ascii_case(n)) {
            fields.push(f.clone());
        }
    }
    StructType::new(fields)
}

/// [M4] Reorder a Union's right child so its output columns line up
/// positionally with `left_schema` by (case-insensitive) name match.
///
/// Wraps the child in a synthetic `Project` whose projections are the
/// reordered `UnresolvedColumn` references. Pass 2 has already assigned
/// types to the child at this point, so we can build the reordered
/// schema directly from the child's own schema. If a left name has no
/// case-insensitive match on the right, raise `SetOpArityMismatch`
/// with a `UNION_BY_NAME` marker (the closest fit in the current error
/// surface; a dedicated variant is deferred to Slice C.2).
///
/// Identity permutations return the child unchanged.
fn reorder_union_right_by_name(
    child: TypedOp,
    left_schema: &Schema,
) -> Result<TypedOp, AnalyzerError> {
    let child_schema = child.schema().clone();
    if child_schema.fields.len() != left_schema.fields.len() {
        return Err(AnalyzerError::SetOpArityMismatch {
            op: "UNION_BY_NAME",
            left_cols: left_schema.fields.len(),
            right_cols: child_schema.fields.len(),
        });
    }
    let mut permutation: Vec<usize> = Vec::with_capacity(left_schema.fields.len());
    for lf in &left_schema.fields {
        match child_schema
            .fields
            .iter()
            .position(|rf| rf.name.eq_ignore_ascii_case(&lf.name))
        {
            Some(pos) => permutation.push(pos),
            None => {
                return Err(AnalyzerError::SetOpArityMismatch {
                    op: "UNION_BY_NAME",
                    left_cols: left_schema.fields.len(),
                    right_cols: child_schema.fields.len(),
                });
            }
        }
    }
    // Identity permutation → no rewrite needed.
    if permutation.iter().enumerate().all(|(i, &p)| i == p) {
        return Ok(child);
    }
    // Build a Project whose projections reorder the columns by name; its
    // projection_types match the reordered child schema so this Project
    // is fully typed at Pass-2 exit (Pass 3 does not need to revisit it).
    let projections: Vec<Expression> = permutation
        .iter()
        .map(|&i| {
            let name = child_schema.fields[i].name.clone();
            Expression::UnresolvedColumn(crate::expression::UnresolvedColumn {
                name,
                qualifier: None,
            })
        })
        .collect();
    let reordered_fields: Vec<StructField> = permutation
        .iter()
        .map(|&i| child_schema.fields[i].clone())
        .collect();
    let projection_types: Vec<TypedAttr> = reordered_fields
        .iter()
        .map(|f| TypedAttr {
            data_type: f.data_type.clone(),
            nullable: f.nullable,
        })
        .collect();
    Ok(TypedOp::Project {
        input: Box::new(child),
        projections,
        projection_types,
        schema: StructType::new(reordered_fields),
    })
}

/// Widen a union's fields via `TypeInferenceEngine::unify_types`.
fn widen_union_fields(left: &Schema, right: &Schema) -> Result<Schema, AnalyzerError> {
    if left.fields.len() != right.fields.len() {
        return Err(AnalyzerError::SetOpArityMismatch {
            op: "UNION",
            left_cols: left.fields.len(),
            right_cols: right.fields.len(),
        });
    }
    let fields = left
        .fields
        .iter()
        .zip(right.fields.iter())
        .map(|(lf, rf)| {
            let dt = TypeInferenceEngine::unify_types(&lf.data_type, &rf.data_type);
            StructField::new(lf.name.clone(), dt, lf.nullable || rf.nullable)
        })
        .collect();
    Ok(StructType::new(fields))
}

/// Downward re-typing sub-pass for a union: rewrite the child's outermost
/// projection-typing to the widened union schema. This is bounded — one
/// re-write per union child per union node.
fn repropagate_union_widening(mut child: TypedOp, widened: &Schema) -> TypedOp {
    // The union's widened schema tells us what type/nullability each column
    // "exits" as. We push that back onto the child's terminal schema so the
    // child's own schema field types agree with the union output.
    match &mut child {
        TypedOp::Project {
            projection_types,
            schema,
            ..
        } => {
            // For each output field, take the widened type; align by
            // ordinal — projection lists produce one field per non-star
            // slot, but Union arity is already validated by
            // widen_union_fields.
            for (i, f) in schema.fields.iter_mut().enumerate() {
                if let Some(w) = widened.fields.get(i) {
                    f.data_type = w.data_type.clone();
                    f.nullable = w.nullable;
                }
            }
            for (i, t) in projection_types.iter_mut().enumerate() {
                if let Some(w) = widened.fields.get(i) {
                    t.data_type = w.data_type.clone();
                    t.nullable = w.nullable;
                }
            }
        }
        // For non-project children, update the surface schema so parents
        // see the widened types. This is safe: the widened schema is by
        // definition a super-type of what the child would emit.
        TypedOp::Filter { schema, .. }
        | TypedOp::Join { schema, .. }
        | TypedOp::Aggregate { schema, .. }
        | TypedOp::Sort { schema, .. }
        | TypedOp::Limit { schema, .. }
        | TypedOp::Tail { schema, .. }
        | TypedOp::Union { schema, .. }
        | TypedOp::Intersect { schema, .. }
        | TypedOp::Except { schema, .. }
        | TypedOp::Distinct { schema, .. }
        | TypedOp::WithColumns { schema, .. }
        | TypedOp::DropColumns { schema, .. }
        | TypedOp::AliasedRelation { schema, .. }
        | TypedOp::TableScan { schema, .. }
        | TypedOp::LocalRelation { schema, .. }
        | TypedOp::RangeRelation { schema, .. } => {
            for (i, f) in schema.fields.iter_mut().enumerate() {
                if let Some(w) = widened.fields.get(i) {
                    f.data_type = w.data_type.clone();
                    f.nullable = w.nullable;
                }
            }
        }
    }
    child
}

// ── Pass 3: derive_nullability ────────────────────────────────────────────────

/// Pass 3 — outer-join and grouping-sets nullability rewrite. Expression-level
/// nullability (`coalesce`, when-without-else, aggregate returns) is already
/// correct from Pass 2's delegation to
/// [`Expression::nullable`](crate::expression::Expression::nullable).
fn derive_nullability(op: TypedOp) -> Result<TypedOp, AnalyzerError> {
    match op {
        TypedOp::Join {
            left,
            right,
            join_type,
            on,
            using,
            schema,
        } => {
            let left = derive_nullability(*left)?;
            let right = derive_nullability(*right)?;

            let (left_nullable, right_nullable) = match join_type {
                JoinKind::Left => (false, true),
                JoinKind::Right => (true, false),
                JoinKind::Full => (true, true),
                _ => (false, false),
            };
            let schema = if left_nullable || right_nullable {
                let using_count = using.len();
                let left_len = left.schema().fields.len();
                let right_len = right.schema().fields.len();
                // M2 — Match the ordinal layout produced by
                // `compute_join_output_schema`. For USING joins:
                //   - LEFT/FULL/INNER: [USING (from left), non-USING left, non-USING right]
                //     positions [0, using_count) = USING keys
                //     positions [using_count, left_len) = non-USING left
                //     positions [left_len, ...) = non-USING right
                //   - RIGHT: [USING (from right), non-USING right, non-USING left]
                //     positions [0, using_count) = USING keys
                //     positions [using_count, right_len) = non-USING right
                //     positions [right_len, ...) = non-USING left
                let is_right_using = matches!(join_type, JoinKind::Right) && using_count > 0;
                let (source_len, using_widens) = if using_count == 0 {
                    // No USING dedup — merged is plain [left..., right...].
                    (left_len, false)
                } else if is_right_using {
                    (right_len, matches!(join_type, JoinKind::Full))
                } else {
                    (left_len, matches!(join_type, JoinKind::Full))
                };

                let fields = schema
                    .fields
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut f)| {
                        if using_count > 0 && i < using_count {
                            // USING key: widens in FULL joins only. In LEFT/RIGHT
                            // the key comes from the outer side, which stays
                            // non-null; the inner-side pair is dedupped.
                            if using_widens {
                                f.nullable = true;
                            }
                        } else {
                            // Non-USING slot: determine which input it comes
                            // from and widen if that input's outer complement
                            // is the outer side.
                            let from_source_side = i < source_len;
                            let widen = if is_right_using {
                                // source is right → widen the "left" (non-source) side
                                if from_source_side {
                                    right_nullable
                                } else {
                                    left_nullable
                                }
                            } else {
                                // source is left → widen right (non-source) side
                                if from_source_side {
                                    left_nullable
                                } else {
                                    right_nullable
                                }
                            };
                            if widen {
                                f.nullable = true;
                            }
                        }
                        f
                    })
                    .collect();
                StructType::new(fields)
            } else {
                schema
            };
            Ok(TypedOp::Join {
                left: Box::new(left),
                right: Box::new(right),
                join_type,
                on,
                using,
                schema,
            })
        }
        TypedOp::Aggregate {
            input,
            grouping,
            grouping_types,
            aggregates,
            aggregate_types,
            having,
            grouping_sets,
            mut schema,
        } => {
            let input = derive_nullability(*input)?;
            if let Some(gs) = &grouping_sets {
                let mut gs_names: Vec<String> = grouping_column_names(gs).into_iter().collect();
                for g in &grouping {
                    if let Some(name) = grouping_expr_name(g) {
                        gs_names.push(name.to_string());
                    }
                }
                for f in schema.fields.iter_mut() {
                    if gs_names.iter().any(|n| f.name.eq_ignore_ascii_case(n)) {
                        f.nullable = true;
                    }
                }
            }
            Ok(TypedOp::Aggregate {
                input: Box::new(input),
                grouping,
                grouping_types,
                aggregates,
                aggregate_types,
                having,
                grouping_sets,
                schema,
            })
        }
        // For all other operators, recurse; nullability was set correctly by Pass 2.
        TypedOp::Project {
            input,
            projections,
            projection_types,
            schema,
        } => Ok(TypedOp::Project {
            input: Box::new(derive_nullability(*input)?),
            projections,
            projection_types,
            schema,
        }),
        TypedOp::Filter {
            input,
            predicate,
            schema,
        } => Ok(TypedOp::Filter {
            input: Box::new(derive_nullability(*input)?),
            predicate,
            schema,
        }),
        TypedOp::Sort {
            input,
            order,
            limit,
            offset,
            schema,
        } => Ok(TypedOp::Sort {
            input: Box::new(derive_nullability(*input)?),
            order,
            limit,
            offset,
            schema,
        }),
        TypedOp::Limit { input, n, schema } => Ok(TypedOp::Limit {
            input: Box::new(derive_nullability(*input)?),
            n,
            schema,
        }),
        TypedOp::Tail { input, n, schema } => Ok(TypedOp::Tail {
            input: Box::new(derive_nullability(*input)?),
            n,
            schema,
        }),
        TypedOp::Union {
            left,
            right,
            all,
            by_name,
            schema,
        } => Ok(TypedOp::Union {
            left: Box::new(derive_nullability(*left)?),
            right: Box::new(derive_nullability(*right)?),
            all,
            by_name,
            schema,
        }),
        TypedOp::Intersect {
            left,
            right,
            all,
            schema,
        } => Ok(TypedOp::Intersect {
            left: Box::new(derive_nullability(*left)?),
            right: Box::new(derive_nullability(*right)?),
            all,
            schema,
        }),
        TypedOp::Except {
            left,
            right,
            all,
            schema,
        } => Ok(TypedOp::Except {
            left: Box::new(derive_nullability(*left)?),
            right: Box::new(derive_nullability(*right)?),
            all,
            schema,
        }),
        TypedOp::Distinct { input, on, schema } => Ok(TypedOp::Distinct {
            input: Box::new(derive_nullability(*input)?),
            on,
            schema,
        }),
        TypedOp::WithColumns {
            input,
            columns,
            schema,
        } => Ok(TypedOp::WithColumns {
            input: Box::new(derive_nullability(*input)?),
            columns,
            schema,
        }),
        TypedOp::DropColumns {
            input,
            names,
            schema,
        } => Ok(TypedOp::DropColumns {
            input: Box::new(derive_nullability(*input)?),
            names,
            schema,
        }),
        TypedOp::AliasedRelation {
            input,
            alias,
            column_aliases,
            schema,
        } => Ok(TypedOp::AliasedRelation {
            input: Box::new(derive_nullability(*input)?),
            alias,
            column_aliases,
            schema,
        }),
        leaf @ (TypedOp::TableScan { .. }
        | TypedOp::LocalRelation { .. }
        | TypedOp::RangeRelation { .. }) => Ok(leaf),
    }
}

/// Extract the output column name from a grouping expression.
/// Mirrors `crate::logical::grouping_expr_name` (which is private to that
/// module and thus not reachable here).
fn grouping_expr_name(expr: &Expression) -> Option<&str> {
    match expr {
        Expression::UnresolvedColumn(u) => Some(&u.name),
        Expression::ColumnReference(c) => Some(&c.name),
        Expression::Alias(a) => Some(&a.alias),
        _ => None,
    }
}

/// Lowercase names of columns appearing in any grouping set.
/// Mirrors `crate::logical::GroupingSets::column_names` (private in that
/// module).
fn grouping_column_names(gs: &GroupingSets) -> std::collections::HashSet<String> {
    let sets: &[Vec<Expression>] = match gs {
        GroupingSets::Rollup(s) | GroupingSets::Cube(s) | GroupingSets::GroupingSets(s) => s,
    };
    let mut names = std::collections::HashSet::new();
    for set in sets {
        for expr in set {
            if let Some(n) = grouping_expr_name(expr) {
                names.insert(n.to_lowercase());
            }
        }
    }
    names
}

/// [M5 §Layer 3] — reject unqualified column references whose name matches
/// more than one field in the schema. Called from Pass 2 for `Project`
/// slots; extended in future passes to Filter/Aggregate resolution.
///
/// A qualified reference (`c.qualifier.is_some()`) is not ambiguous by
/// definition — the qualifier disambiguates. Only unqualified names with
/// multiple case-insensitive matches raise
/// [`AnalyzerError::AmbiguousColumn`].
fn ensure_no_ambiguous_columns(
    expr: &Expression,
    schema: &StructType,
) -> Result<(), AnalyzerError> {
    match expr {
        Expression::UnresolvedColumn(u) if u.qualifier.is_none() => {
            check_name_unique(&u.name, schema)?;
        }
        Expression::UnresolvedColumn(_) => {
            // Qualified — the qualifier disambiguates by definition.
        }
        Expression::ColumnReference(c) if c.qualifier.is_none() => {
            check_name_unique(&c.name, schema)?;
        }
        Expression::ColumnReference(_) => {
            // Qualified — disambiguated by construction.
        }
        Expression::Alias(a) => ensure_no_ambiguous_columns(&a.expr, schema)?,
        Expression::Binary(b) => {
            ensure_no_ambiguous_columns(&b.left, schema)?;
            ensure_no_ambiguous_columns(&b.right, schema)?;
        }
        Expression::Unary(u) => ensure_no_ambiguous_columns(&u.operand, schema)?,
        Expression::FunctionCall(f) => {
            for arg in &f.args {
                ensure_no_ambiguous_columns(arg, schema)?;
            }
        }
        Expression::Cast(c) => ensure_no_ambiguous_columns(&c.expr, schema)?,
        Expression::CaseWhen(cw) => {
            if let Some(base) = &cw.base {
                ensure_no_ambiguous_columns(base, schema)?;
            }
            for (cond, val) in &cw.branches {
                ensure_no_ambiguous_columns(cond, schema)?;
                ensure_no_ambiguous_columns(val, schema)?;
            }
            if let Some(else_expr) = &cw.else_expr {
                ensure_no_ambiguous_columns(else_expr, schema)?;
            }
        }
        Expression::Window(w) => {
            ensure_no_ambiguous_columns(&w.func, schema)?;
            for p in &w.partition_by {
                ensure_no_ambiguous_columns(p, schema)?;
            }
            for so in &w.order_by {
                ensure_no_ambiguous_columns(&so.expr, schema)?;
            }
        }
        Expression::Lambda(l) => {
            // Lambda parameters are locals to the lambda body and do
            // not shadow schema fields, so walking the body is safe:
            // any schema-name reference in the body still resolves
            // against the outer schema.
            ensure_no_ambiguous_columns(&l.body, schema)?;
        }
        Expression::ArrayLiteral(a) => {
            for elem in &a.elements {
                ensure_no_ambiguous_columns(elem, schema)?;
            }
        }
        Expression::MapLiteral(m) => {
            for k in &m.keys {
                ensure_no_ambiguous_columns(k, schema)?;
            }
            for v in &m.values {
                ensure_no_ambiguous_columns(v, schema)?;
            }
        }
        Expression::StructLiteral(s) => {
            for (_name, val) in &s.fields {
                ensure_no_ambiguous_columns(val, schema)?;
            }
        }
        Expression::Between(b) => {
            ensure_no_ambiguous_columns(&b.expr, schema)?;
            ensure_no_ambiguous_columns(&b.low, schema)?;
            ensure_no_ambiguous_columns(&b.high, schema)?;
        }
        Expression::InList(il) => {
            ensure_no_ambiguous_columns(&il.expr, schema)?;
            for item in &il.list {
                ensure_no_ambiguous_columns(item, schema)?;
            }
        }
        Expression::Like(l) => {
            ensure_no_ambiguous_columns(&l.value, schema)?;
            ensure_no_ambiguous_columns(&l.pattern, schema)?;
        }
        Expression::IsDistinctFrom(idf) => {
            ensure_no_ambiguous_columns(&idf.left, schema)?;
            ensure_no_ambiguous_columns(&idf.right, schema)?;
        }
        Expression::ExtractValue(ev) => {
            ensure_no_ambiguous_columns(&ev.child, schema)?;
            ensure_no_ambiguous_columns(&ev.extraction, schema)?;
        }
        Expression::RowConstructor(rc) => {
            for f in &rc.fields {
                ensure_no_ambiguous_columns(f, schema)?;
            }
        }
        Expression::UpdateFields(uf) => {
            // Walk into the struct expression AND any replacement value.
            // Even though Slice C.2 doesn't *emit* UpdateFields (Slice F
            // territory), the analyzer must still catch ambiguity inside
            // its subexpressions — otherwise a plan the analyzer accepts
            // might carry unrelated ambiguity that only surfaces at emit
            // time (or in the legacy fallback path).
            ensure_no_ambiguous_columns(&uf.struct_expr, schema)?;
            if let Some(value) = &uf.value {
                ensure_no_ambiguous_columns(value, schema)?;
            }
        }
        Expression::InSubquery(_)
        | Expression::ExistsSubquery(_)
        | Expression::ScalarSubquery(_) => {
            // TODO Slice G: subquery bodies are analyzed by a nested
            // `analyze()` call — the outer walker does not descend
            // into them for the ambiguity pass. Attempting to walk
            // here would double-analyze the inner plan.
            // no-op — subquery body walked by nested analyze()
        }
        Expression::Literal(_)
        | Expression::Star(_)
        | Expression::LambdaVariable(_)
        | Expression::RawSql(_)
        | Expression::Interval(_) => {
            // No column references possible.
        }
    }
    Ok(())
}

fn check_name_unique(name: &str, schema: &StructType) -> Result<(), AnalyzerError> {
    // Dot-notation names traverse struct fields — those are unambiguous
    // even if the outer struct name collides, because the outer + inner
    // pair identifies a single field.
    if name.contains('.') {
        return Ok(());
    }
    let candidates: Vec<String> = schema
        .fields
        .iter()
        .filter(|f| f.name.eq_ignore_ascii_case(name))
        .map(|f| f.name.clone())
        .collect();
    if candidates.len() > 1 {
        return Err(AnalyzerError::AmbiguousColumn {
            name: name.to_owned(),
            candidates,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::analyzer_fixtures;
    use super::*;
    use crate::expression::{
        AliasExpression, BinaryExpression, BinaryOp, ColumnReference, UnresolvedColumn,
    };

    fn base_types_all() -> BaseTypes {
        let mut m = BaseTypes::new();
        m.insert("emp".to_string(), analyzer_fixtures::fixture_emp());
        m.insert("dept".to_string(), analyzer_fixtures::fixture_dept());
        m.insert("nums".to_string(), analyzer_fixtures::fixture_nums());
        m.insert("emp2".to_string(), analyzer_fixtures::fixture_emp2());
        m.insert("raw".to_string(), analyzer_fixtures::fixture_raw());
        m
    }

    #[test]
    fn analyze_project_col_plus_col() {
        // nums.select((col('a') + col('lng')).alias('r')) — expect r: Long, nullable
        let a = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "a".to_string(),
            qualifier: None,
        });
        let lng = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "lng".to_string(),
            qualifier: None,
        });
        let sum = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(a),
            right: Box::new(lng),
        });
        let aliased = Expression::Alias(AliasExpression {
            expr: Box::new(sum),
            alias: "r".to_string(),
        });
        let ast = CommonAst {
            root: CommonOp::Project(Project {
                input: Box::new(CommonOp::TableScan(TableScan {
                    name: "nums".to_string(),
                    schema: StructType::empty(),
                })),
                projections: vec![aliased],
            }),
        };
        let typed = analyze(ast, &base_types_all()).expect("analyze must succeed");
        assert!(has_resolved_schema(&typed));
        assert_eq!(typed.root.schema().fields.len(), 1);
        assert_eq!(typed.root.schema().fields[0].name, "r");
        assert_eq!(typed.root.schema().fields[0].data_type, DataType::Long);
        assert!(typed.root.schema().fields[0].nullable);
    }

    #[test]
    fn analyze_rejects_punt() {
        let ast = CommonAst {
            root: CommonOp::Punt {
                kind: "Pivot",
                reason: "runtime-data-dependent schema",
            },
        };
        let err = analyze(ast, &base_types_all()).expect_err("punt must fail");
        assert!(matches!(
            err,
            AnalyzerError::PuntedOperator { kind: "Pivot", .. }
        ));
    }

    #[test]
    fn analyze_rejects_unknown_table() {
        let ast = CommonAst {
            root: CommonOp::TableScan(TableScan {
                name: "ghost".to_string(),
                schema: StructType::empty(),
            }),
        };
        let err = analyze(ast, &base_types_all()).expect_err("unknown table must fail");
        assert!(matches!(err, AnalyzerError::UnknownTable { .. }));
    }

    #[test]
    fn has_resolved_schema_detects_planted_unresolved() {
        // Manually construct a TypedAst with a planted Unresolved slot to
        // prove the walker actually looks at every slot.
        let typed = TypedAst {
            root: TypedOp::Project {
                input: Box::new(TypedOp::LocalRelation {
                    schema: StructType::single("x", DataType::Long),
                }),
                projections: vec![Expression::ColumnReference(ColumnReference {
                    name: "x".to_string(),
                    qualifier: None,
                    data_type: DataType::Long,
                    nullable: false,
                })],
                projection_types: vec![TypedAttr {
                    data_type: DataType::Unresolved,
                    nullable: true,
                }],
                schema: StructType::single("x", DataType::Long),
            },
        };
        assert!(!has_resolved_schema(&typed));
    }

    #[test]
    fn inference_smoke_runs_all_fixtures() {
        // Redundant with the invariants test but pins the entry point
        // to the mini-fixture matrix.
        inference_smoke();
    }

    /// M1 regression — `SELECT *` on a resolved child must not trip INV5.
    #[test]
    fn m1_bare_star_projection_passes_walker() {
        use crate::expression::StarExpression;
        // Project(Star) over the `nums` table scan.
        let ast = CommonAst {
            root: CommonOp::Project(Project {
                input: Box::new(CommonOp::TableScan(super::super::ast::TableScan {
                    name: "nums".to_string(),
                    schema: StructType::empty(),
                })),
                projections: vec![Expression::Star(StarExpression { qualifier: None })],
            }),
        };
        let typed = analyze(ast, &base_types_all()).expect("bare-Star project must analyze");
        assert!(
            has_resolved_schema(&typed),
            "bare Star projection must not trip INV5 walker (M1 regression)"
        );
        // The Star must have expanded to the child schema's fields.
        assert_eq!(typed.root.schema().fields.len(), 7);
    }

    /// M2 regression — a `RIGHT + USING` join must keep the USING key
    /// non-nullable (it comes from the surviving right side, not the
    /// widened left).
    #[test]
    fn m2_right_using_keeps_key_non_nullable() {
        use crate::expression::UnresolvedColumn;
        // emp2 has dept_id (nullable), dept has dept_id (NOT NULL).
        // RIGHT + USING(dept_id) means dept_id comes from the right (dept)
        // and stays NOT NULL.  The columns from emp2 (the left side)
        // become nullable via outer-join widening.
        let ast = CommonAst {
            root: CommonOp::Join(Join {
                left: Box::new(CommonOp::Project(Project {
                    input: Box::new(CommonOp::TableScan(super::super::ast::TableScan {
                        name: "emp2".to_string(),
                        schema: StructType::empty(),
                    })),
                    projections: vec![
                        Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "dept_id".to_string(),
                            qualifier: None,
                        }),
                        Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "name".to_string(),
                            qualifier: None,
                        }),
                    ],
                })),
                right: Box::new(CommonOp::TableScan(super::super::ast::TableScan {
                    name: "dept".to_string(),
                    schema: StructType::empty(),
                })),
                join_type: JoinKind::Right,
                on: None,
                using: vec!["dept_id".to_string()],
            }),
        };
        let typed = analyze(ast, &base_types_all()).expect("RIGHT+USING must analyze");
        let schema = typed.root.schema();
        let dept_id = schema
            .field_by_name("dept_id")
            .expect("dept_id must be present");
        assert!(
            !dept_id.nullable,
            "RIGHT+USING dept_id must stay NOT NULL (M2 regression); got nullable={}",
            dept_id.nullable
        );
    }

    /// M4 regression — `unionByName` reorders the right's fields to match
    /// left's before positional widening.
    #[test]
    fn m4_union_by_name_reorders_right() {
        use crate::expression::UnresolvedColumn;
        use crate::transpiler_v2::ast::Union;
        // left projects (a, lng); right projects (lng, a). unionByName
        // must reorder right to (a, lng) so the widened union has the
        // right per-column types.
        let left = CommonOp::Project(Project {
            input: Box::new(CommonOp::TableScan(super::super::ast::TableScan {
                name: "nums".to_string(),
                schema: StructType::empty(),
            })),
            projections: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "a".to_string(),
                    qualifier: None,
                }),
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "lng".to_string(),
                    qualifier: None,
                }),
            ],
        });
        let right = CommonOp::Project(Project {
            input: Box::new(CommonOp::TableScan(super::super::ast::TableScan {
                name: "nums".to_string(),
                schema: StructType::empty(),
            })),
            projections: vec![
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "lng".to_string(),
                    qualifier: None,
                }),
                Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "a".to_string(),
                    qualifier: None,
                }),
            ],
        });
        let ast = CommonAst {
            root: CommonOp::Union(Union {
                left: Box::new(left),
                right: Box::new(right),
                all: true,
                by_name: true,
            }),
        };
        let typed = analyze(ast, &base_types_all()).expect("unionByName must analyze");
        let schema = typed.root.schema();
        assert_eq!(schema.fields[0].name, "a");
        assert_eq!(schema.fields[1].name, "lng");
        // Widened per-column type: `a` is Int (both sides Int); `lng` is Long
        // (both sides Long) — union widening produces Long on `lng` even
        // after the by-name reorder.
        assert_eq!(schema.fields[0].data_type, DataType::Integer);
        assert_eq!(schema.fields[1].data_type, DataType::Long);
    }

    /// M5 regression — Filter with non-boolean predicate must raise
    /// `TypeMismatch`, not fall through to legacy.
    #[test]
    fn m5_filter_type_mismatch_raises() {
        use crate::expression::UnresolvedColumn;
        // `nums.a` is Integer; using it as a predicate must fail.
        let ast = CommonAst {
            root: CommonOp::Filter(Filter {
                input: Box::new(CommonOp::TableScan(super::super::ast::TableScan {
                    name: "nums".to_string(),
                    schema: StructType::empty(),
                })),
                predicate: Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "a".to_string(),
                    qualifier: None,
                }),
            }),
        };
        let err = analyze(ast, &base_types_all()).expect_err("non-boolean predicate must fail");
        assert!(
            matches!(err, AnalyzerError::TypeMismatch { .. }),
            "expected TypeMismatch, got {err:?}"
        );
    }

    /// M5 regression — an unqualified reference matching two
    /// case-insensitively-equal fields must raise `AmbiguousColumn`.
    #[test]
    fn m5_ambiguous_column_raises() {
        use crate::expression::UnresolvedColumn;
        // Synthesize a schema with two case-insensitively-equal names
        // (`id` and `ID`). Both are legal StructField names on their
        // own; together they force any unqualified `id` reference to
        // be ambiguous by Spark's case-insensitive resolution rules.
        let ambiguous_schema = StructType::new(vec![
            StructField::nullable("id", DataType::Long),
            StructField::nullable("ID", DataType::Long),
        ]);
        let ast = CommonAst {
            root: CommonOp::Project(Project {
                input: Box::new(CommonOp::TableScan(super::super::ast::TableScan {
                    name: "ambig".to_string(),
                    // Passing a populated schema on the TableScan node
                    // short-circuits the BaseTypes lookup in
                    // `resolve_table_scan`, so the analyzer sees this
                    // synthetic ambiguous schema directly.
                    schema: ambiguous_schema.clone(),
                })),
                projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                    name: "id".to_string(),
                    qualifier: None,
                })],
            }),
        };
        let err = analyze(ast, &BaseTypes::new())
            .expect_err("ambiguous unqualified column must raise AmbiguousColumn");
        match err {
            AnalyzerError::AmbiguousColumn { name, candidates } => {
                assert_eq!(name, "id");
                assert_eq!(candidates.len(), 2);
                assert!(
                    candidates.iter().any(|c| c == "id") && candidates.iter().any(|c| c == "ID"),
                    "candidates must include both `id` and `ID`, got {candidates:?}"
                );
            }
            other => panic!("expected AmbiguousColumn, got {other:?}"),
        }
    }
}
