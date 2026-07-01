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
            projection_types,
            ..
        } => {
            walk_resolved(input)
                && projection_types
                    .iter()
                    .all(|t| !t.data_type.contains_unresolved())
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
    let input = resolve(*p.input, base_types)?;
    let child_schema = input.schema().clone();

    // Star-expand for schema derivation.  The projection list itself is
    // preserved verbatim so the emitter can decide whether to emit `*` or
    // the expanded refs.
    let mut fields: Vec<StructField> = Vec::new();
    for expr in &p.projections {
        match expr {
            Expression::Star(_) => {
                fields.extend(child_schema.fields.iter().cloned());
            }
            _ => {
                let name = projection_output_name(expr);
                // Pass 1 leaves types unresolved; the actual DataType and
                // nullability are filled in during Pass 2 against the
                // real child schema. We seed with `Unresolved` and let
                // Pass 2's assign_types_project resolve them.
                fields.push(StructField::new(name, DataType::Unresolved, true));
            }
        }
    }
    let projection_types = p
        .projections
        .iter()
        .map(|_| TypedAttr {
            data_type: DataType::Unresolved,
            nullable: true,
        })
        .collect();
    Ok(TypedOp::Project {
        input: Box::new(input),
        projections: p.projections,
        projection_types,
        schema: StructType::new(fields),
    })
}

/// Compute the output field name for a projection expression, matching the
/// legacy `projection_to_field` helper's naming conventions.
fn projection_output_name(expr: &Expression) -> String {
    match expr {
        Expression::Alias(a) => a.alias.clone(),
        Expression::ColumnReference(c) => c.name.clone(),
        Expression::UnresolvedColumn(u) => u.name.clone(),
        other => spark_column_name(other),
    }
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
    let input = resolve(*a.input, base_types)?;
    // Grouping fields come first, then aggregate output fields — types get
    // filled in by Pass 2 (they need the child schema for delegation).
    let mut fields: Vec<StructField> = Vec::new();
    for g in &a.grouping {
        let name = projection_output_name(g);
        fields.push(StructField::new(name, DataType::Unresolved, true));
    }
    for agg in &a.aggregates {
        let name = aggregate_output_name(&agg.func);
        fields.push(StructField::new(name, DataType::Unresolved, true));
    }
    let grouping_types = a
        .grouping
        .iter()
        .map(|_| TypedAttr {
            data_type: DataType::Unresolved,
            nullable: true,
        })
        .collect();
    let aggregate_types = a
        .aggregates
        .iter()
        .map(|_| TypedAttr {
            data_type: DataType::Unresolved,
            nullable: true,
        })
        .collect();
    Ok(TypedOp::Aggregate {
        input: Box::new(input),
        grouping: a.grouping,
        grouping_types,
        aggregates: a.aggregates,
        aggregate_types,
        having: a.having,
        grouping_sets: a.grouping_sets,
        schema: StructType::new(fields),
    })
}

/// Output field name for an aggregate call — mirrors `agg_expr_to_field`.
fn aggregate_output_name(func: &Expression) -> String {
    match func {
        Expression::Alias(a) => a.alias.clone(),
        other => spark_column_name(other),
    }
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
    let left = resolve(*u.left, base_types)?;
    let right = resolve(*u.right, base_types)?;
    let left_schema = left.schema().clone();
    let right_schema = right.schema().clone();
    if left_schema.fields.len() != right_schema.fields.len() {
        return Err(AnalyzerError::SetOpArityMismatch {
            op: "UNION",
            left_cols: left_schema.fields.len(),
            right_cols: right_schema.fields.len(),
        });
    }
    // Field NAMES from left; types stay Unresolved until Pass 2 widens.
    let fields = left_schema
        .fields
        .iter()
        .map(|f| StructField::new(f.name.clone(), DataType::Unresolved, true))
        .collect();
    Ok(TypedOp::Union {
        left: Box::new(left),
        right: Box::new(right),
        all: u.all,
        schema: StructType::new(fields),
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
    let input = resolve(*w.input, base_types)?;
    let input_schema = input.schema().clone();

    // Apply each column in order, so later entries can reference earlier ones.
    let mut schema = input_schema;
    let mut columns: Vec<(String, Expression, TypedAttr)> = Vec::with_capacity(w.columns.len());
    for (new_name, expr) in w.columns {
        // Compute data_type + nullability against the current schema.
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
        columns.push((new_name, expr, attr));
    }
    Ok(TypedOp::WithColumns {
        input: Box::new(input),
        columns,
        schema,
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
            schema: _,
        } => {
            let left = assign_types(*left)?;
            let right = assign_types(*right)?;

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
    let mut fields = Vec::new();
    for name in using {
        if let Some(f) = left_schema
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
        {
            fields.push(f.clone());
        }
    }
    for f in &left_schema.fields {
        if !using.iter().any(|n| f.name.eq_ignore_ascii_case(n)) {
            fields.push(f.clone());
        }
    }
    for f in &right_schema.fields {
        if !using.iter().any(|n| f.name.eq_ignore_ascii_case(n)) {
            fields.push(f.clone());
        }
    }
    StructType::new(fields)
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

            let left_len = left.schema().fields.len();
            let (left_nullable, right_nullable) = match join_type {
                JoinKind::Left => (false, true),
                JoinKind::Right => (true, false),
                JoinKind::Full => (true, true),
                _ => (false, false),
            };
            let schema = if left_nullable || right_nullable {
                let fields = schema
                    .fields
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut f)| {
                        // For semi/anti joins we bailed above with (false, false).
                        // For regular joins with USING dedup, the merged
                        // schema puts USING keys first, then non-USING left,
                        // then non-USING right; we widen the segments that
                        // correspond to the "outer" side.
                        if (i < left_len && left_nullable) || (i >= left_len && right_nullable) {
                            f.nullable = true;
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
            schema,
        } => Ok(TypedOp::Union {
            left: Box::new(derive_nullability(*left)?),
            right: Box::new(derive_nullability(*right)?),
            all,
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
}
