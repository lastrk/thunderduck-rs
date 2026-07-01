//! Common AST for the rearchitected transpiler (ADR-003, ADR-004).
//!
//! The proto-inspired IR that both front-ends (Spark Connect protobuf and
//! SparkSQL parse tree) lower into. Kept short of a full Catalyst
//! LogicalPlan reimplementation per ADR-003: node types are added only when
//! (a) the corpus produces them AND (b) they are not expressible via existing
//! nodes. See `docs/thunderduck-rearchitect-ADRs.md` ADR-003 for the rule.

use crate::expression::{Expression, SortOrder};
use crate::types::StructType;

/// The common AST: a tree of operators over legacy expressions.
///
/// Slice B carries the operator surface the DataFrame corpus needs to type
/// (`type-*`, `cond-*`, `agg-*`); everything else is [`CommonOp::Punt`],
/// which the analyzer rejects with
/// [`crate::transpiler_v2::analyzer::AnalyzerError::PuntedOperator`].
#[derive(Debug, Clone, PartialEq)]
pub struct CommonAst {
    /// Root operator. Owned; the tree is moved into the analyzer.
    pub root: CommonOp,
}

/// Operator surface. See §4.1 of the Slice B plan for the corpus mapping.
#[derive(Debug, Clone, PartialEq)]
pub enum CommonOp {
    /// `df.select(...)` / raw-SQL `SELECT ...`.
    Project(Project),
    /// `df.filter(...)` / raw-SQL `WHERE ...`.
    Filter(Filter),
    /// `df.join(...)` in all seven join kinds.
    Join(Join),
    /// `df.groupBy(...).agg(...)` with optional `HAVING` and grouping sets.
    Aggregate(Aggregate),
    /// `df.orderBy(...)` (may carry limit/offset).
    Sort(Sort),
    /// `df.limit(n)`.
    Limit(Limit),
    /// `df.tail(n)`.
    Tail(Tail),
    /// `df.union` / `df.unionByName` / `df.unionAll`.
    Union(Union),
    /// `df.intersect(...)`.
    Intersect(Intersect),
    /// `df.exceptAll(...)` / `df.except(...)`.
    Except(Except),
    /// `df.distinct()` or `df.dropDuplicates(cols)`.
    Distinct(Distinct),
    /// `df.withColumn(...)` / `df.withColumns(...)`.
    WithColumns(WithColumns),
    /// `df.drop(...)`.
    DropColumns(DropColumns),
    /// `df.alias(...)`.
    AliasedRelation(AliasedRelation),
    /// Named table scan; `schema` is populated by the caller from the
    /// catalog seed (`BaseTypes`) — empty means "unresolved".
    TableScan(TableScan),
    /// A relation with an explicit, precomputed schema
    /// (e.g. `createDataFrame(rows, schema)`).
    LocalRelation(LocalRelation),
    /// `spark.range(...)`.
    RangeRelation(RangeRelation),
    /// A `LogicalPlan` variant that Slice B intentionally does not type.
    /// Rejected by [`crate::transpiler_v2::analyzer::analyze`] with a
    /// typed error so a caller opting into `THUNDERDUCK_TRANSPILER=v2`
    /// gets a loud failure rather than silent wrong types.
    Punt {
        /// Stable diagnostic name for the punted operator kind.
        kind: &'static str,
        /// Why it is out of scope for the current slice.
        reason: &'static str,
    },
}

/// `df.select(...)` / raw-SQL `SELECT ...`.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Projection list; may contain `Expression::Star`, which Pass 1
    /// star-expands *internally* (schema-only; emission continues to
    /// delegate the `*` to DuckDB per ADR-002 / T1).
    pub projections: Vec<Expression>,
}

/// `df.filter(...)` — a single boolean predicate over the child schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Predicate expression (must yield boolean).
    pub predicate: Expression,
}

/// Mirrors [`crate::logical::JoinType`] but is redeclared here so `CommonAst`
/// does not depend on the legacy `logical` module for its operator surface.
/// Conversion between the two is a mechanical `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    /// Inner (default) join.
    Inner,
    /// Left outer join — right-side columns become nullable.
    Left,
    /// Right outer join — left-side columns become nullable.
    Right,
    /// Full outer join — both sides become nullable.
    Full,
    /// Cartesian / cross join.
    Cross,
    /// Semi join — returns left rows that have a match.
    LeftSemi,
    /// Anti join — returns left rows that do not have a match.
    LeftAnti,
}

/// `df.join(...)` with one of seven join kinds.
#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    /// Left input.
    pub left: Box<CommonOp>,
    /// Right input.
    pub right: Box<CommonOp>,
    /// Join kind.
    pub join_type: JoinKind,
    /// Optional `ON` predicate; when absent and `using` is empty, this is a
    /// cross join.
    pub on: Option<Expression>,
    /// USING columns (Spark's shared-key join). Deduplicated on the output
    /// schema; keys appear first, then non-USING left, then non-USING right.
    pub using: Vec<String>,
}

/// A single aggregate call (`sum(x)`, `count(1)`, etc.) with distinct/filter.
///
/// Mirrors [`crate::logical::AggregateExpr`] so a `LogicalPlan → CommonAst`
/// adapter (Slice C) can port it mechanically.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateCall {
    /// A legacy `Expression::FunctionCall` for the aggregate.
    pub func: Expression,
    /// `DISTINCT` modifier.
    pub is_distinct: bool,
    /// Optional `FILTER (WHERE ...)` clause.
    pub filter: Option<Expression>,
}

/// `df.groupBy(...).agg(...)` — grouping keys plus aggregate calls.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Grouping expressions (may be plain columns or computed expressions).
    pub grouping: Vec<Expression>,
    /// Aggregate calls.
    pub aggregates: Vec<AggregateCall>,
    /// Optional `HAVING` clause applied after aggregation.
    pub having: Option<Expression>,
    /// ROLLUP/CUBE/GROUPING SETS marker — nullability of grouping keys
    /// widens when this is `Some(...)`. Content mirrors the legacy
    /// [`crate::logical::GroupingSets`] to keep the future adapter
    /// mechanical (see Open Question §14.4 in the plan).
    pub grouping_sets: Option<crate::logical::GroupingSets>,
}

/// `df.orderBy(...)` — an ordered relation with optional limit/offset.
#[derive(Debug, Clone, PartialEq)]
pub struct Sort {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Sort expressions (each with direction + null ordering).
    pub order: Vec<SortOrder>,
    /// Optional `LIMIT`.
    pub limit: Option<Expression>,
    /// Optional `OFFSET`.
    pub offset: Option<Expression>,
}

/// `df.limit(n)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Row count (a literal or expression).
    pub n: Expression,
}

/// `df.tail(n)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Tail {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Row count (a literal or expression).
    pub n: Expression,
}

/// `df.union` / `df.unionAll` / `df.unionByName`.
#[derive(Debug, Clone, PartialEq)]
pub struct Union {
    /// Left input.
    pub left: Box<CommonOp>,
    /// Right input.
    pub right: Box<CommonOp>,
    /// When true, `UNION ALL` semantics; when false, deduplicated `UNION`.
    pub all: bool,
}

/// `df.intersect(...)` / `df.intersectAll(...)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Intersect {
    /// Left input.
    pub left: Box<CommonOp>,
    /// Right input.
    pub right: Box<CommonOp>,
    /// `ALL` modifier.
    pub all: bool,
}

/// `df.except(...)` / `df.exceptAll(...)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Except {
    /// Left input.
    pub left: Box<CommonOp>,
    /// Right input.
    pub right: Box<CommonOp>,
    /// `ALL` modifier.
    pub all: bool,
}

/// `df.distinct()` (empty `on`) or `df.dropDuplicates(subset)` (non-empty).
#[derive(Debug, Clone, PartialEq)]
pub struct Distinct {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Empty = full-row distinct; non-empty = subset key.
    pub on: Vec<Expression>,
}

/// `df.withColumn(...)` / `df.withColumns(...)` — batch add/replace columns.
///
/// Renames (an existing column re-referenced under a new name) are represented
/// as `(new_name, Expression::Alias { expr: col, alias: new_name })` — the
/// analyzer does not need a rename discriminator; the emitter (Slice C)
/// decides whether to emit `AS` or a plain projection.
#[derive(Debug, Clone, PartialEq)]
pub struct WithColumns {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Ordered (name, expression) pairs. Later entries can reference earlier
    /// ones by name; the analyzer applies them in order.
    pub columns: Vec<(String, Expression)>,
}

/// `df.drop(...)` — remove named columns.
#[derive(Debug, Clone, PartialEq)]
pub struct DropColumns {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Column names to drop (case-insensitive match against the child schema).
    pub names: Vec<String>,
}

/// `df.alias(...)` — carry an alias plus optional column aliases.
#[derive(Debug, Clone, PartialEq)]
pub struct AliasedRelation {
    /// Input relation.
    pub input: Box<CommonOp>,
    /// Relation alias (qualifier for column references).
    pub alias: String,
    /// Positional column aliases; when non-empty, must match the input arity.
    pub column_aliases: Vec<String>,
}

/// Named table scan; `schema` is populated by Pass 1 from the catalog seed.
#[derive(Debug, Clone, PartialEq)]
pub struct TableScan {
    /// Table name.
    pub name: String,
    /// Populated by the caller from `BaseTypes` (the catalog seed);
    /// empty means unresolved — Pass 1 rejects with
    /// [`crate::transpiler_v2::analyzer::AnalyzerError::UnknownTable`].
    pub schema: StructType,
}

/// A relation with an explicit precomputed schema.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalRelation {
    /// The precomputed schema.
    pub schema: StructType,
}

/// `spark.range(...)` — a single `id: Long NOT NULL` column.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeRelation {
    /// Start (inclusive).
    pub start: i64,
    /// End (exclusive).
    pub end: i64,
    /// Step (defaults to 1 in Spark).
    pub step: i64,
}
