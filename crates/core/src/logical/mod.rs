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
    /// SQL path marker: GROUP BY key intentionally not in SELECT list.
    /// Suppresses the auto-prepend of grouping columns in gen_aggregate but renders nothing.
    GroupingNotSelected,
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
    SingleRow(SingleRowRelation),
    DropColumns(DropColumns),
    ShowString(ShowString),
    NADrop(NADrop),
    NAFill(NAFill),
    NAReplace(NAReplace),
    Unpivot(Unpivot),
    Pivot(Pivot),
    StatCov(StatCov),
    StatCorr(StatCorr),
    ApproxQuantile(ApproxQuantile),
    StatCrosstab(StatCrosstab),
    StatFreqItems(StatFreqItems),
    StatSampleBy(StatSampleBy),
    Describe(Describe),
    Summary(Summary),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatCov {
    pub input: Box<LogicalPlan>,
    pub col1: String,
    pub col2: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatCorr {
    pub input: Box<LogicalPlan>,
    pub col1: String,
    pub col2: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApproxQuantile {
    pub input: Box<LogicalPlan>,
    pub cols: Vec<String>,
    pub probabilities: Vec<f64>,
    pub relative_error: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatCrosstab {
    pub input: Box<LogicalPlan>,
    pub col1: String,
    pub col2: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatFreqItems {
    pub input: Box<LogicalPlan>,
    pub cols: Vec<String>,
    pub support: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatSampleBy {
    pub input: Box<LogicalPlan>,
    pub col_expr: Expression,
    pub fractions: Vec<(crate::expression::Literal, f64)>,
    pub seed: Option<i64>,
}

/// `df.describe(cols...)` — summary statistics as VARCHAR strings (5 fixed stats).
#[derive(Debug, Clone, PartialEq)]
pub struct Describe {
    pub input: Box<LogicalPlan>,
    /// Resolved column names to describe (empty = all columns).
    pub cols: Vec<String>,
}

/// `df.summary(statistics...)` — configurable statistics as VARCHAR strings.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub input: Box<LogicalPlan>,
    /// Requested statistics (empty = default set).
    pub statistics: Vec<String>,
    /// Resolved column names (empty = all columns).
    pub cols: Vec<String>,
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
    /// When the ON condition has plan_id-qualified columns, these aliases are assigned to
    /// left/right subqueries so DuckDB can resolve otherwise-ambiguous column names.
    /// Format: "__plan_id_{N}__" where N is the outermost plan_id of that side.
    pub left_alias: Option<String>,
    pub right_alias: Option<String>,
    /// All plan_ids in the left/right subtrees (used to qualify outer expressions).
    pub left_plan_ids: Vec<i64>,
    pub right_plan_ids: Vec<i64>,
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
    /// When non-empty, deduplicate on these columns only (dropDuplicates subset).
    /// When empty, deduplicate on all columns (SELECT DISTINCT *).
    pub columns: Vec<Expression>,
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
    /// Optional known schema — populated when the SQL comes from a LocalRelation
    /// with Arrow IPC data so that type inference (e.g. SUM→BIGINT cast) can
    /// look up column types without issuing a DESCRIBE query.
    pub schema: StructType,
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

/// A relation that produces a single row with no columns.
///
/// Used for expressions that don't require an input table,
/// e.g. `SELECT 1` or `SELECT ABS(-1)` — no FROM clause needed.
#[derive(Debug, Clone, PartialEq)]
pub struct SingleRowRelation;

/// Drop named columns from the input relation.
/// Generates: `SELECT * EXCLUDE ("col1", "col2") FROM input`
#[derive(Debug, Clone, PartialEq)]
pub struct DropColumns {
    pub input: Box<LogicalPlan>,
    pub column_names: Vec<String>,
}

/// Phase 3 stub for `df.show()` — delegates to input plan; PySpark formats client-side.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowString {
    pub input: Box<LogicalPlan>,
    pub num_rows: i32,
    pub truncate: i32,
    pub vertical: bool,
}

/// `df.dropna()` — drop rows containing null values.
#[derive(Debug, Clone, PartialEq)]
pub struct NADrop {
    pub input: Box<LogicalPlan>,
    pub how: NADropHow,
    pub threshold: Option<i32>,
    /// Resolved column names to check (empty = all columns from schema).
    pub cols: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NADropHow {
    Any,
    All,
}

/// `df.fillna()` — replace null values with constants.
#[derive(Debug, Clone, PartialEq)]
pub struct NAFill {
    pub input: Box<LogicalPlan>,
    /// (column_name, fill_value) pairs for columns that should be filled.
    pub values: Vec<(String, crate::expression::Literal)>,
    /// All column names in the schema (used to construct SELECT list).
    pub all_columns: Vec<String>,
}

/// `df.replace()` — replace specific values with other values.
#[derive(Debug, Clone, PartialEq)]
pub struct NAReplace {
    pub input: Box<LogicalPlan>,
    /// (column_name, from_value, to_value) triples.
    pub replacements: Vec<(String, crate::expression::Literal, crate::expression::Literal)>,
    /// All column names in the schema.
    pub all_columns: Vec<String>,
}

/// `df.groupBy().pivot().agg()` — rotate rows to columns.
#[derive(Debug, Clone, PartialEq)]
pub struct Pivot {
    pub input: Box<LogicalPlan>,
    /// Columns to group by (remain as rows).
    pub grouping: Vec<Expression>,
    /// The column whose distinct values become new column headers.
    pub pivot_col: Expression,
    /// Explicit list of pivot values (empty = auto-discover at query time).
    pub pivot_values: Vec<Expression>,
    /// Aggregation function(s) to apply per pivot cell.
    pub aggregates: Vec<AggregateExpr>,
}

/// `df.unpivot()` / `df.melt()` — reshape wide to long format.
#[derive(Debug, Clone, PartialEq)]
pub struct Unpivot {
    pub input: Box<LogicalPlan>,
    /// Column names to keep as-is (id columns).
    pub ids: Vec<String>,
    /// Column names to unpivot (value columns).
    pub values: Vec<String>,
    /// Name for the new "variable" column.
    pub variable_column_name: String,
    /// Name for the new "value" column.
    pub value_column_name: String,
    /// Whether to include rows where the value is null.
    pub include_nulls: bool,
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
                // LeftSemi / LeftAnti return only left-side columns.
                if j.join_type.is_semi_or_anti() {
                    return j.left.infer_schema();
                }
                // Compute schemas once; reused for USING ordering below.
                let left_schema = j.left.infer_schema();
                let right_schema = j.right.infer_schema();
                // If either side's schema is unknown, we cannot statically determine the join
                // output schema. Return empty so service.rs triggers the DuckDB fallback.
                if left_schema.is_empty() || right_schema.is_empty() {
                    return StructType::empty();
                }
                let left_len = left_schema.fields.len();
                let merged = StructType::merge(&left_schema, &right_schema);

                // Apply outer-join nullability: LEFT makes right-side nullable,
                // RIGHT makes left-side nullable, FULL makes both nullable.
                let right_nullable = matches!(j.join_type, JoinType::Left | JoinType::Full);
                let left_nullable  = matches!(j.join_type, JoinType::Right | JoinType::Full);
                let merged = if left_nullable || right_nullable {
                    let fields = merged.fields.into_iter().enumerate().map(|(i, mut f)| {
                        if (i < left_len && left_nullable) || (i >= left_len && right_nullable) {
                            f.nullable = true;
                        }
                        f
                    }).collect();
                    StructType::new(fields)
                } else {
                    merged
                };

                // Deduplicate columns referenced in USING clause (SQL USING keeps one copy).
                // Also deduplicate for equijoin ON conditions that get converted to USING.
                // When plan_id aliases are active, the join uses ON (not USING), so no dedup.
                let using_cols: Vec<String> = if j.left_alias.is_some() {
                    vec![]
                } else if !j.using_columns.is_empty() {
                    j.using_columns.clone()
                } else if let Some(cond) = &j.condition {
                    // Mirror equijoin_to_using logic from generator
                    fn col_name(e: &Expression) -> Option<String> {
                        match e {
                            Expression::UnresolvedColumn(u) => Some(u.name.clone()),
                            Expression::ColumnReference(c) => Some(c.name.clone()),
                            _ => None,
                        }
                    }
                    fn equijoin_cols(expr: &Expression) -> Option<Vec<String>> {
                        use crate::expression::BinaryOp;
                        match expr {
                            Expression::Binary(b) if b.op == BinaryOp::Eq => {
                                let l = col_name(&b.left)?;
                                let r = col_name(&b.right)?;
                                if l == r { Some(vec![l]) } else { None }
                            }
                            Expression::Binary(b) if b.op == BinaryOp::And => {
                                let mut l = equijoin_cols(&b.left)?;
                                let r = equijoin_cols(&b.right)?;
                                l.extend(r);
                                Some(l)
                            }
                            _ => None,
                        }
                    }
                    equijoin_cols(cond).unwrap_or_default()
                } else {
                    vec![]
                };
                if using_cols.is_empty() {
                    merged
                } else {
                    // Rebuild field list with USING keys first, then left non-USING, then right
                    // non-USING — matching Spark's column ordering convention for USING joins.
                    let using_set: std::collections::HashSet<&str> =
                        using_cols.iter().map(|s| s.as_str()).collect();
                    let mut fields = Vec::new();
                    // 1. USING columns first (from left schema, in USING order)
                    for name in &using_cols {
                        if let Some(f) = left_schema.fields.iter().find(|f| &f.name == name) {
                            fields.push(f.clone());
                        }
                    }
                    // 2. Left non-USING columns
                    for f in &left_schema.fields {
                        if !using_set.contains(f.name.as_str()) { fields.push(f.clone()); }
                    }
                    // 3. Right non-USING columns
                    for f in &right_schema.fields {
                        if !using_set.contains(f.name.as_str()) { fields.push(f.clone()); }
                    }
                    StructType::new(fields)
                }
            }
            LogicalPlan::Sort(s) => s.input.infer_schema(),
            LogicalPlan::Limit(l) => l.input.infer_schema(),
            LogicalPlan::Tail(t) => t.input.infer_schema(),
            LogicalPlan::Union(u) => {
                let left = u.left.infer_schema();
                let right = u.right.infer_schema();
                if left.is_empty() || right.is_empty() || left.fields.len() != right.fields.len() {
                    return left;
                }
                let fields = left.fields.into_iter().zip(right.fields)
                    .map(|(mut lf, rf)| {
                        let promoted = crate::types::TypeInferenceEngine::promote_numeric(
                            &lf.data_type, &rf.data_type,
                        );
                        // promote_numeric returns Double for non-numeric pairs — keep left type
                        lf.data_type = if promoted == DataType::Double
                            && !lf.data_type.is_numeric() && !rf.data_type.is_numeric()
                        { lf.data_type } else { promoted };
                        lf.nullable = lf.nullable || rf.nullable;
                        lf
                    })
                    .collect();
                StructType::new(fields)
            }
            LogicalPlan::Except(e) => e.left.infer_schema(),
            LogicalPlan::Intersect(i) => i.left.infer_schema(),
            LogicalPlan::Distinct(d) => d.input.infer_schema(),
            LogicalPlan::Sample(s) => s.input.infer_schema(),
            LogicalPlan::TableScan(_) => StructType::empty(), // resolved at runtime
            LogicalPlan::SqlRelation(r) => r.schema.clone(),
            LogicalPlan::LocalRelation(r) => r.schema.clone(),
            LogicalPlan::LocalDataRelation(r) => r.schema.clone(),
            LogicalPlan::RangeRelation(_) => StructType::new(vec![
                StructField::not_null("id", DataType::Long),
            ]),
            LogicalPlan::InMemoryRelation(r) => r.schema.clone(),
            LogicalPlan::WithCte(c) => c.input.infer_schema(),
            LogicalPlan::WithColumns(w) => infer_with_columns_schema(w),
            LogicalPlan::AliasedRelation(a) => {
                let child = a.input.infer_schema();
                if !a.column_aliases.is_empty() && a.column_aliases.len() == child.fields.len() {
                    let fields = child.fields.into_iter()
                        .zip(&a.column_aliases)
                        .map(|(mut f, name)| { f.name = name.clone(); f })
                        .collect();
                    StructType::new(fields)
                } else {
                    child
                }
            }
            LogicalPlan::RawDdlStatement(_) => StructType::empty(),
            LogicalPlan::ToDataFrame(t) => infer_to_dataframe_schema(t),
            LogicalPlan::SingleRow(_) => StructType::empty(),
            LogicalPlan::DropColumns(d) => {
                let child = d.input.infer_schema();
                let excluded: std::collections::HashSet<&str> =
                    d.column_names.iter().map(String::as_str).collect();
                StructType::new(
                    child.fields.into_iter().filter(|f| !excluded.contains(f.name.as_str())).collect(),
                )
            }
            LogicalPlan::ShowString(s) => s.input.infer_schema(),
            LogicalPlan::NADrop(n) => n.input.infer_schema(),
            LogicalPlan::NAFill(n) => n.input.infer_schema(),
            LogicalPlan::NAReplace(n) => n.input.infer_schema(),
            LogicalPlan::Unpivot(u) => {
                let mut fields: Vec<StructField> = u
                    .ids
                    .iter()
                    .map(|name| StructField::nullable(name.clone(), DataType::Unresolved))
                    .collect();
                fields.push(StructField::nullable(
                    u.variable_column_name.clone(),
                    DataType::String,
                ));
                fields.push(StructField::nullable(
                    u.value_column_name.clone(),
                    DataType::Unresolved,
                ));
                StructType::new(fields)
            }
            // Pivot schema depends on runtime data (distinct pivot values); return empty.
            LogicalPlan::Pivot(_) => StructType::empty(),
            // StatCov/StatCorr return a single DOUBLE column.
            LogicalPlan::StatCov(s) => StructType::new(vec![
                StructField::nullable(format!("cov({}, {})", s.col1, s.col2), DataType::Double),
            ]),
            LogicalPlan::StatCorr(s) => StructType::new(vec![
                StructField::nullable(format!("corr({}, {})", s.col1, s.col2), DataType::Double),
            ]),
            // ApproxQuantile returns one column of ARRAY<DOUBLE>, one row per input column.
            LogicalPlan::ApproxQuantile(_) => StructType::new(vec![
                StructField::nullable("quantiles".to_string(), DataType::Array(Box::new(DataType::Double))),
            ]),
            // Crosstab: pivot columns unknown at plan time → DuckDB fallback.
            LogicalPlan::StatCrosstab(_) => StructType::empty(),
            // FreqItems: one Array<String> column per input col, named "{col}_freqItems".
            LogicalPlan::StatFreqItems(s) => StructType::new(
                s.cols.iter()
                    .map(|c| StructField::nullable(
                        format!("{}_freqItems", c),
                        DataType::Array(Box::new(DataType::String)),
                    ))
                    .collect()
            ),
            // SampleBy: same schema as input.
            LogicalPlan::StatSampleBy(s) => s.input.infer_schema(),
            // Describe/Summary: "summary" VARCHAR + one VARCHAR column per input column.
            LogicalPlan::Describe(d) => {
                let mut fields = vec![StructField::not_null("summary", DataType::String)];
                for col in &d.cols {
                    fields.push(StructField::nullable(col.clone(), DataType::String));
                }
                StructType::new(fields)
            }
            LogicalPlan::Summary(s) => {
                let mut fields = vec![StructField::not_null("summary", DataType::String)];
                for col in &s.cols {
                    fields.push(StructField::nullable(col.clone(), DataType::String));
                }
                StructType::new(fields)
            }
        }
    }
}

fn infer_project_schema(p: &Project) -> StructType {
    let child_schema = p.input.infer_schema();
    let has_star = p.projections.iter().any(|e| matches!(e, Expression::Star(_)));
    // If we have a wildcard but can't statically resolve the child schema (e.g. TableScan),
    // return empty so the caller falls back to DuckDB schema inference.
    if has_star && child_schema.is_empty() {
        return StructType::empty();
    }
    let mut fields = Vec::new();
    for expr in &p.projections {
        if matches!(expr, Expression::Star(_)) {
            // Expand * to all child schema fields
            fields.extend(child_schema.fields.iter().cloned());
        } else if let Some(f) = projection_to_field(expr, &child_schema) {
            fields.push(f);
        }
    }
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
            Some(StructField::nullable(spark_column_name(other), dt))
        }
    }
}

/// Build a Spark-convention column name for an unaliased expression.
///
/// Mirrors Java `buildSparkColumnName()` for the common cases. Used by
/// `projection_to_field`, `agg_expr_to_field`, and `render_agg_expr` when there is no
/// explicit alias.
pub fn spark_column_name(expr: &Expression) -> String {
    match expr {
        Expression::FunctionCall(f) => {
            // Spark names unaliased count(*) as "count(1)".
            // The sql_converter converts count(*) → count(1) (Literal::int(1)), so match both.
            let is_count_star = f.name.eq_ignore_ascii_case("count")
                && f.args.iter().any(|a| matches!(a,
                    Expression::Star(_)
                    | Expression::Literal(crate::expression::Literal { value: crate::expression::LiteralValue::Int(1), .. })
                ));
            if is_count_star { return "count(1)".to_string(); }
            let args = f.args.iter()
                .map(spark_column_name)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({})", f.name, args)
        }
        Expression::Star(_) => "*".to_string(),
        Expression::UnresolvedColumn(u) => u.name.clone(),
        Expression::ColumnReference(c) => c.name.clone(),
        Expression::Alias(a) => spark_column_name(&a.expr),
        Expression::Cast(c) => {
            let inner = spark_column_name(&c.expr);
            let ty = spark_type_name(&c.to_type);
            format!("CAST({inner} AS {ty})")
        }
        Expression::Binary(b) => {
            let left = spark_column_name(&b.left);
            let right = spark_column_name(&b.right);
            let op = b.op.symbol();
            format!("({left} {op} {right})")
        }
        Expression::Unary(u) => {
            use crate::expression::UnaryOp;
            let inner = spark_column_name(&u.operand);
            match u.op {
                UnaryOp::Not => format!("(NOT {inner})"),
                UnaryOp::Negate => format!("(-{inner})"),
                UnaryOp::IsNull => format!("({inner} IS NULL)"),
                UnaryOp::IsNotNull => format!("({inner} IS NOT NULL)"),
                UnaryOp::IsNaN => format!("isnan({inner})"),
                UnaryOp::IsNotNaN => format!("(NOT isnan({inner}))"),
            }
        }
        Expression::Literal(l) => {
            use crate::expression::LiteralValue;
            match &l.value {
                LiteralValue::Int(i) => i.to_string(),
                LiteralValue::Long(i) => i.to_string(),
                LiteralValue::Float(f) => f.to_string(),
                LiteralValue::Double(d) => d.to_string(),
                LiteralValue::Boolean(b) => b.to_string(),
                LiteralValue::Null => "null".to_string(),
                LiteralValue::String(s) => format!("'{s}'"),
                _ => "?".to_string(),
            }
        }
        // RawSql from selectExpr/ExpressionString: extract column name.
        // If the SQL has an "AS alias" suffix (e.g. "CAST(x AS INT) as foo"), use the alias.
        // For simple column references (e.g. "name", "salary"), use the text as-is.
        Expression::RawSql(r) => {
            let sql = r.sql.trim();
            // Find last " AS " (case-insensitive) to extract the column alias.
            let sql_upper = sql.to_uppercase();
            if let Some(as_pos) = sql_upper.rfind(" AS ") {
                let candidate = sql[as_pos + 4..].trim();
                // Strip double-quote wrapping for quoted identifiers (e.g. `"key"` → `key`).
                let unquoted = if candidate.starts_with('"') && candidate.ends_with('"') && candidate.len() > 2 {
                    &candidate[1..candidate.len() - 1]
                } else {
                    candidate
                };
                // Only use if it's a valid simple identifier (no spaces, parens, etc.)
                let is_simple_ident = !unquoted.is_empty()
                    && unquoted.chars().all(|c| c.is_alphanumeric() || c == '_');
                if is_simple_ident {
                    return unquoted.to_string();
                }
            }
            // Simple column reference or unknown: use the raw SQL text
            sql.to_string()
        }
        _ => "expr".to_string(),
    }
}

/// Spark-style SQL type name — no space after comma in DECIMAL(p,s).
/// Used when building Spark-compatible column names that include type names.
fn spark_type_name(dt: &crate::types::DataType) -> String {
    use crate::types::DataType;
    match dt {
        DataType::Decimal { precision, scale } => format!("DECIMAL({precision},{scale})"),
        other => crate::types::TypeMapper::to_duckdb(other),
    }
}

fn infer_aggregate_schema(a: &Aggregate) -> StructType {
    let child_schema = a.input.infer_schema();
    let mut fields = Vec::new();

    let use_select_order = !a.select_order.is_empty();

    if use_select_order {
        // If no grouping column appears in select_order (e.g. groupBy().count() shorthand),
        // prepend all grouping columns so the schema matches the actual SELECT output.
        // GroupingNotSelected suppresses the prepend for SQL path (GROUP BY not in SELECT).
        let has_grouping_in_order = a.select_order.iter()
            .any(|e| matches!(e, SelectEntry::GroupingExpr(_) | SelectEntry::GroupingNotSelected));
        if !has_grouping_in_order {
            for e in &a.grouping {
                if let Some(f) = projection_to_field(e, &child_schema) {
                    fields.push(f);
                }
            }
        }
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
                SelectEntry::GroupingNotSelected => {}
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

    // ROLLUP/CUBE produce NULL in grouping columns for subtotal/grand-total rows.
    if a.grouping_sets.is_some() {
        let n_grouping = a.grouping.len();
        for f in fields.iter_mut().take(n_grouping) {
            f.nullable = true;
        }
    }
    StructType::new(fields)
}

fn agg_expr_to_field(expr: &Expression, schema: &StructType) -> Option<StructField> {
    match expr {
        Expression::Alias(a) => {
            // Use aggregate_return_type for aggregate FunctionCalls inside an alias,
            // since function_return_type() (scalar) doesn't know about COUNT/SUM/AVG.
            let dt = match a.expr.as_ref() {
                Expression::FunctionCall(f) => {
                    let arg_types: Vec<_> = f.args.iter().map(|e| e.data_type(schema)).collect();
                    crate::types::TypeInferenceEngine::aggregate_return_type(
                        &f.name,
                        arg_types.first().unwrap_or(&DataType::Unresolved),
                    )
                }
                other => other.data_type(schema),
            };
            Some(StructField::nullable(a.alias.clone(), dt))
        }
        Expression::FunctionCall(f) => {
            let arg_types: Vec<_> = f.args.iter().map(|a| a.data_type(schema)).collect();
            let dt = crate::types::TypeInferenceEngine::aggregate_return_type(
                &f.name,
                arg_types.first().unwrap_or(&DataType::Unresolved),
            );
            Some(StructField::nullable(spark_column_name(expr), dt))
        }
        other => projection_to_field(other, schema),
    }
}

fn infer_with_columns_schema(w: &WithColumns) -> StructType {
    let input_schema = w.input.infer_schema();
    // If the input schema is unknown (e.g. SqlRelation), return empty so the
    // DuckDB schema-probe fallback is triggered. Adding only the new columns
    // would produce an incomplete schema (missing all existing columns).
    if input_schema.is_empty() {
        return StructType::empty();
    }
    let mut schema = input_schema;
    for (new_name, expr) in &w.columns {
        // Detect pure rename: expression is a column ref with a different name.
        let old_col_name: Option<&str> = match expr {
            Expression::UnresolvedColumn(uc) if uc.name != *new_name => Some(&uc.name),
            Expression::ColumnReference(cr) if cr.name != *new_name => Some(&cr.name),
            _ => None,
        };
        if let Some(old_name) = old_col_name {
            // Rename: find the old column and change its name in-place.
            if let Some(idx) = schema.field_index(old_name) {
                let dt = schema.fields[idx].data_type.clone();
                schema.fields[idx] = StructField::nullable(new_name.clone(), dt);
            }
        } else {
            let dt = expr.data_type(&schema);
            // Replace existing field or append
            if let Some(idx) = schema.field_index(new_name) {
                schema.fields[idx] = StructField::nullable(new_name.clone(), dt);
            } else {
                schema.fields.push(StructField::nullable(new_name.clone(), dt));
            }
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
    let extra_start = t.column_names.len().min(child.fields.len());
    let mut fields: Vec<StructField> = child.fields.into_iter()
        .zip(t.column_names.iter())
        .map(|(mut f, name)| { f.name = name.clone(); f })
        .collect();
    // Extra names beyond child column count → String fields (matches Java behaviour)
    for name in t.column_names.iter().skip(extra_start) {
        fields.push(StructField::nullable(name.clone(), DataType::String));
    }
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

    #[test]
    fn single_row_schema_is_empty() {
        let plan = LogicalPlan::SingleRow(SingleRowRelation);
        let schema = plan.infer_schema();
        assert!(schema.is_empty());
    }
}
