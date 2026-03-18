use crate::expression::{Expression, SortOrder};
use crate::types::{DataType, StructField, StructType};

// ── Supporting types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    Cross,
    LeftSemi,
    LeftAnti,
}

impl JoinType {
    pub fn sql_keyword(&self) -> &'static str {
        match self {
            JoinType::Inner => "INNER JOIN",
            JoinType::Left => "LEFT JOIN",
            JoinType::Right => "RIGHT JOIN",
            JoinType::Full => "FULL OUTER JOIN",
            JoinType::Cross => "CROSS JOIN",
            JoinType::LeftSemi => "SEMI JOIN",
            JoinType::LeftAnti => "ANTI JOIN",
        }
    }

    pub fn is_semi_or_anti(&self) -> bool {
        matches!(self, JoinType::LeftSemi | JoinType::LeftAnti)
    }
}

/// A single aggregate expression (function call + optional DISTINCT and FILTER).
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateExpr {
    /// The aggregate function call expression.
    pub func: Expression,
    pub is_distinct: bool,
    pub filter: Option<Expression>,
}

impl AggregateExpr {
    pub fn new(func: Expression) -> Self {
        Self { func, is_distinct: false, filter: None }
    }
    pub fn distinct(func: Expression) -> Self {
        Self { func, is_distinct: true, filter: None }
    }
}

/// ROLLUP / CUBE / GROUPING SETS specification.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupingSets {
    Rollup(Vec<Vec<Expression>>),
    Cube(Vec<Vec<Expression>>),
    GroupingSets(Vec<Vec<Expression>>),
}

/// Describes a position in an Aggregate's SELECT list — either a grouping
/// column or an aggregate expression (by index into the aggregates vec).
#[derive(Debug, Clone, PartialEq)]
pub enum SelectEntry {
    GroupingExpr(Expression),
    AggregateExpr(usize),
}

// ── LogicalPlan ───────────────────────────────────────────────────────────────

/// The closed set of all logical plan node types.
///
/// Every variant of this enum is handled exhaustively in `SqlGenerator::generate()`.
/// Adding a new variant without updating the generator is a **compile error**.
#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    Project(Project),
    Filter(Filter),
    Aggregate(Aggregate),
    Join(Join),
    Sort(Sort),
    Limit(Limit),
    Tail(Tail),
    Union(Union),
    Except(Except),
    Intersect(Intersect),
    Distinct(Distinct),
    Sample(Sample),
    TableScan(TableScan),
    SqlRelation(SqlRelation),
    LocalRelation(LocalRelation),
    LocalDataRelation(LocalDataRelation),
    RangeRelation(RangeRelation),
    InMemoryRelation(InMemoryRelation),
    WithCte(WithCte),
    WithColumns(WithColumns),
    AliasedRelation(AliasedRelation),
    RawDdlStatement(RawDdlStatement),
    ToDataFrame(ToDataFrame),
}

// ── Plan node structs ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub input: Box<LogicalPlan>,
    pub projections: Vec<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub input: Box<LogicalPlan>,
    pub condition: Expression,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub input: Box<LogicalPlan>,
    pub grouping: Vec<Expression>,
    pub aggregates: Vec<AggregateExpr>,
    pub having: Option<Expression>,
    pub grouping_sets: Option<GroupingSets>,
    /// Ordered select list interleaving grouping columns and aggregate results.
    /// If empty, the generator uses `grouping` then `aggregates` in order.
    pub select_order: Vec<SelectEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub left: Box<LogicalPlan>,
    pub right: Box<LogicalPlan>,
    pub join_type: JoinType,
    pub condition: Option<Expression>,
    /// Column names for USING clause (empty if using ON condition).
    pub using_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sort {
    pub input: Box<LogicalPlan>,
    pub order: Vec<SortOrder>,
    pub limit: Option<Expression>,
    pub offset: Option<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub input: Box<LogicalPlan>,
    pub limit: Expression,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tail {
    pub input: Box<LogicalPlan>,
    pub limit: Expression,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Union {
    pub left: Box<LogicalPlan>,
    pub right: Box<LogicalPlan>,
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Except {
    pub left: Box<LogicalPlan>,
    pub right: Box<LogicalPlan>,
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Intersect {
    pub left: Box<LogicalPlan>,
    pub right: Box<LogicalPlan>,
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Distinct {
    pub input: Box<LogicalPlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub input: Box<LogicalPlan>,
    pub fraction: f64,
    pub seed: Option<i64>,
    pub with_replacement: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableScan {
    pub table: String,
    pub alias: Option<String>,
}

/// Wraps a raw SQL string as a sub-relation (e.g., from `spark.sql(...)`).
#[derive(Debug, Clone, PartialEq)]
pub struct SqlRelation {
    pub sql: String,
}

/// An empty relation with only a schema (no data rows).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalRelation {
    pub schema: StructType,
}

/// An in-memory relation carrying serialised Arrow data.
/// Phase 1: schema only. Arrow bytes added in Phase 2.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalDataRelation {
    pub schema: StructType,
    // Phase 2 will add: pub data: Vec<RecordBatch>
}

/// `range(start, end, step)` — produces a table with one `id` column.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeRelation {
    pub start: i64,
    pub end: i64,
    pub step: i64,
    pub num_partitions: Option<i32>,
}

/// Reference to an already-registered in-memory / temporary table.
#[derive(Debug, Clone, PartialEq)]
pub struct InMemoryRelation {
    pub view_name: String,
    pub schema: StructType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WithCte {
    pub ctes: Vec<(String, Box<LogicalPlan>)>,
    pub input: Box<LogicalPlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WithColumns {
    pub input: Box<LogicalPlan>,
    /// (column_name, expression) pairs — new or replaced columns.
    pub columns: Vec<(String, Expression)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AliasedRelation {
    pub input: Box<LogicalPlan>,
    pub alias: String,
    /// Optional column aliases (for `AS alias(col1, col2, ...)`).
    pub column_aliases: Vec<String>,
}

/// A raw DDL/DML statement passed through directly to DuckDB.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDdlStatement {
    pub sql: String,
}

/// Rename the output columns (from `df.toDF("a", "b", ...)`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToDataFrame {
    pub input: Box<LogicalPlan>,
    pub column_names: Vec<String>,
}

// ── Schema inference ──────────────────────────────────────────────────────────

impl LogicalPlan {
    /// Infer the output schema of this plan.
    ///
    /// For plans whose schema depends on runtime data (e.g. `TableScan`
    /// before the table has been registered) returns `StructType::empty()`.
    /// The generator and session layer fill this in at execution time.
    pub fn infer_schema(&self) -> StructType {
        match self {
            LogicalPlan::Project(p) => infer_project_schema(p),
            LogicalPlan::Filter(f) => f.input.infer_schema(),
            LogicalPlan::Aggregate(a) => infer_aggregate_schema(a),
            LogicalPlan::Join(j) => {
                StructType::merge(&j.left.infer_schema(), &j.right.infer_schema())
            }
            LogicalPlan::Sort(s) => s.input.infer_schema(),
            LogicalPlan::Limit(l) => l.input.infer_schema(),
            LogicalPlan::Tail(t) => t.input.infer_schema(),
            LogicalPlan::Union(u) => u.left.infer_schema(),
            LogicalPlan::Except(e) => e.left.infer_schema(),
            LogicalPlan::Intersect(i) => i.left.infer_schema(),
            LogicalPlan::Distinct(d) => d.input.infer_schema(),
            LogicalPlan::Sample(s) => s.input.infer_schema(),
            LogicalPlan::TableScan(_) => StructType::empty(), // resolved at runtime
            LogicalPlan::SqlRelation(_) => StructType::empty(),
            LogicalPlan::LocalRelation(r) => r.schema.clone(),
            LogicalPlan::LocalDataRelation(r) => r.schema.clone(),
            LogicalPlan::RangeRelation(_) => StructType::new(vec![
                StructField::not_null("id", DataType::Long),
            ]),
            LogicalPlan::InMemoryRelation(r) => r.schema.clone(),
            LogicalPlan::WithCte(c) => c.input.infer_schema(),
            LogicalPlan::WithColumns(w) => infer_with_columns_schema(w),
            LogicalPlan::AliasedRelation(a) => a.input.infer_schema(),
            LogicalPlan::RawDdlStatement(_) => StructType::empty(),
            LogicalPlan::ToDataFrame(t) => infer_to_dataframe_schema(t),
        }
    }
}

fn infer_project_schema(p: &Project) -> StructType {
    let child_schema = p.input.infer_schema();
    let fields = p
        .projections
        .iter()
        .filter_map(|e| projection_to_field(e, &child_schema))
        .collect();
    StructType::new(fields)
}

fn projection_to_field(expr: &Expression, schema: &StructType) -> Option<StructField> {
    match expr {
        Expression::Alias(a) => Some(StructField::nullable(
            a.alias.clone(),
            a.expr.data_type(schema),
        )),
        Expression::ColumnReference(c) => Some(StructField::nullable(
            c.name.clone(),
            c.data_type.clone().pipe_if_unresolved(|| {
                crate::types::TypeInferenceEngine::column_type(&c.name, schema)
            }),
        )),
        Expression::UnresolvedColumn(u) => {
            let dt = crate::types::TypeInferenceEngine::column_type(&u.name, schema);
            Some(StructField::nullable(u.name.clone(), dt))
        }
        Expression::Star(_) => None, // expanded by caller
        other => {
            let dt = other.data_type(schema);
            Some(StructField::nullable("expr".to_string(), dt))
        }
    }
}

fn infer_aggregate_schema(a: &Aggregate) -> StructType {
    let child_schema = a.input.infer_schema();
    let mut fields = Vec::new();

    let use_select_order = !a.select_order.is_empty();

    if use_select_order {
        for entry in &a.select_order {
            match entry {
                SelectEntry::GroupingExpr(e) => {
                    if let Some(f) = projection_to_field(e, &child_schema) {
                        fields.push(f);
                    }
                }
                SelectEntry::AggregateExpr(idx) => {
                    if let Some(agg) = a.aggregates.get(*idx) {
                        if let Some(f) = agg_expr_to_field(&agg.func, &child_schema) {
                            fields.push(f);
                        }
                    }
                }
            }
        }
    } else {
        for e in &a.grouping {
            if let Some(f) = projection_to_field(e, &child_schema) {
                fields.push(f);
            }
        }
        for agg in &a.aggregates {
            if let Some(f) = agg_expr_to_field(&agg.func, &child_schema) {
                fields.push(f);
            }
        }
    }

    StructType::new(fields)
}

fn agg_expr_to_field(expr: &Expression, schema: &StructType) -> Option<StructField> {
    match expr {
        Expression::Alias(a) => {
            let dt = a.expr.data_type(schema);
            Some(StructField::nullable(a.alias.clone(), dt))
        }
        Expression::FunctionCall(f) => {
            let arg_types: Vec<_> = f.args.iter().map(|a| a.data_type(schema)).collect();
            let dt = crate::types::TypeInferenceEngine::aggregate_return_type(
                &f.name,
                arg_types.first().unwrap_or(&DataType::Unresolved),
            );
            Some(StructField::nullable(f.name.clone(), dt))
        }
        other => projection_to_field(other, schema),
    }
}

fn infer_with_columns_schema(w: &WithColumns) -> StructType {
    let mut schema = w.input.infer_schema();
    for (name, expr) in &w.columns {
        let dt = expr.data_type(&schema);
        // Replace existing field or append
        if let Some(idx) = schema.field_index(name) {
            schema.fields[idx] = StructField::nullable(name.clone(), dt);
        } else {
            schema.fields.push(StructField::nullable(name.clone(), dt));
        }
    }
    schema
}

fn infer_to_dataframe_schema(t: &ToDataFrame) -> StructType {
    let child = t.input.infer_schema();
    if child.is_empty() {
        // Can't rename if we don't know the child schema
        return child;
    }
    let fields = child
        .fields
        .into_iter()
        .zip(t.column_names.iter())
        .map(|(mut f, name)| {
            f.name = name.clone();
            f
        })
        .collect();
    StructType::new(fields)
}

// ── Small helpers ─────────────────────────────────────────────────────────────

trait PipeIfUnresolved {
    fn pipe_if_unresolved(self, f: impl FnOnce() -> Self) -> Self;
}

impl PipeIfUnresolved for DataType {
    fn pipe_if_unresolved(self, f: impl FnOnce() -> Self) -> Self {
        if self == DataType::Unresolved { f() } else { self }
    }
}

// ── Constructors ──────────────────────────────────────────────────────────────

impl LogicalPlan {
    pub fn table_scan(table: impl Into<String>) -> Self {
        LogicalPlan::TableScan(TableScan { table: table.into(), alias: None })
    }
    pub fn filter(input: LogicalPlan, condition: Expression) -> Self {
        LogicalPlan::Filter(Filter { input: Box::new(input), condition })
    }
    pub fn project(input: LogicalPlan, projections: Vec<Expression>) -> Self {
        LogicalPlan::Project(Project { input: Box::new(input), projections })
    }
    pub fn limit(input: LogicalPlan, n: i64) -> Self {
        use crate::expression::Literal;
        LogicalPlan::Limit(Limit { input: Box::new(input), limit: Literal::long(n) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::{ColumnReference, Literal};

    #[test]
    fn range_schema() {
        let plan = LogicalPlan::RangeRelation(RangeRelation {
            start: 0,
            end: 100,
            step: 1,
            num_partitions: None,
        });
        let schema = plan.infer_schema();
        assert_eq!(schema.len(), 1);
        assert_eq!(schema.fields[0].name, "id");
        assert_eq!(schema.fields[0].data_type, DataType::Long);
    }

    #[test]
    fn filter_preserves_schema() {
        let input = LogicalPlan::LocalRelation(LocalRelation {
            schema: StructType::new(vec![
                StructField::nullable("x", DataType::Integer),
                StructField::nullable("y", DataType::String),
            ]),
        });
        let plan = LogicalPlan::filter(
            input,
            Expression::Binary(crate::expression::BinaryExpression {
                op: crate::expression::BinaryOp::Gt,
                left: Box::new(ColumnReference::untyped("x")),
                right: Box::new(Literal::int(0)),
            }),
        );
        let schema = plan.infer_schema();
        assert_eq!(schema.len(), 2);
    }

    #[test]
    fn project_schema() {
        let input = LogicalPlan::LocalRelation(LocalRelation {
            schema: StructType::new(vec![StructField::nullable("id", DataType::Long)]),
        });
        let plan = LogicalPlan::project(
            input,
            vec![Expression::Alias(crate::expression::AliasExpression {
                expr: Box::new(ColumnReference::untyped("id")),
                alias: "user_id".into(),
            })],
        );
        let schema = plan.infer_schema();
        assert_eq!(schema.fields[0].name, "user_id");
        assert_eq!(schema.fields[0].data_type, DataType::Long);
    }
}
