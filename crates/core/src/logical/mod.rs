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
        Self {
            func,
            is_distinct: false,
            filter: None,
        }
    }
    pub fn distinct(func: Expression) -> Self {
        Self {
            func,
            is_distinct: true,
            filter: None,
        }
    }
}

/// ROLLUP / CUBE / GROUPING SETS specification.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupingSets {
    Rollup(Vec<Vec<Expression>>),
    Cube(Vec<Vec<Expression>>),
    GroupingSets(Vec<Vec<Expression>>),
}

impl GroupingSets {
    /// Returns references to the inner set lists for any variant.
    fn sets(&self) -> &[Vec<Expression>] {
        match self {
            GroupingSets::Rollup(s) | GroupingSets::Cube(s) | GroupingSets::GroupingSets(s) => s,
        }
    }

    /// Collects the lowercase names of all columns that appear in any grouping
    /// set.  Used by `infer_aggregate_schema` to mark those columns nullable
    /// (ROLLUP / CUBE produce NULL super-aggregate rows).
    fn column_names(&self) -> std::collections::HashSet<String> {
        let mut names = std::collections::HashSet::new();
        for set in self.sets() {
            for expr in set {
                if let Some(name) = grouping_expr_name(expr) {
                    names.insert(name.to_lowercase());
                }
            }
        }
        names
    }
}

/// Extract the output column name from a grouping expression.
fn grouping_expr_name(expr: &Expression) -> Option<&str> {
    match expr {
        Expression::UnresolvedColumn(u) => Some(&u.name),
        Expression::ColumnReference(c) => Some(&c.name),
        Expression::Alias(a) => Some(&a.alias),
        _ => None,
    }
}

/// Describes a position in an Aggregate's SELECT list — either a grouping
/// column or an aggregate expression carried inline.
///
/// `AggregateExpr` carries the value directly rather than indexing into a
/// separate vec. The previous index-based design silently dropped entries
/// when the index was out of range, which is the kind of "default arm
/// silently drops unknowns" anti-pattern called out in CLAUDE.md.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectEntry {
    GroupingExpr(Expression),
    AggregateExpr(AggregateExpr),
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
    DdlStatement(DdlStatement),
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
    /// Pre-populated schema (from DuckDB table metadata). Empty when unknown.
    pub schema: crate::types::StructType,
}

/// Wraps a raw SQL string as a sub-relation (e.g., from `spark.sql(...)`).
#[derive(Debug, Clone, PartialEq)]
pub struct SqlRelation {
    pub sql: String,
    /// Optional known schema — populated when the SQL comes from a LocalRelation
    /// with Arrow IPC data so that type inference (e.g. SUM→BIGINT cast) can
    /// look up column types without issuing a DESCRIBE query.
    pub schema: StructType,
    /// When true, the SQL is already in DuckDB-native format and must NOT be run
    /// through `preprocess_spark_sql`. This prevents double-processing of constructs
    /// like `MAP([keys], [vals])` which would be incorrectly rewritten to
    /// `MAP([[keys]], [[vals]])`.
    #[doc(hidden)]
    pub duckdb_ready: bool,
    /// For CREATE VIEW DDL: the unquoted view name. When set, the service layer
    /// caches `self.schema` as the view's Spark-accurate schema so that
    /// subsequent `spark.table()` calls get correct nullable metadata.
    /// DuckDB views lose NOT NULL on all columns, so this preserves it.
    pub view_name: Option<String>,
}

impl SqlRelation {
    /// Create a new SqlRelation with raw Spark SQL that needs preprocessing.
    pub fn new(sql: String, schema: StructType) -> Self {
        Self {
            sql,
            schema,
            duckdb_ready: false,
            view_name: None,
        }
    }

    /// Create a new SqlRelation with DuckDB-ready SQL that must skip preprocessing.
    pub fn duckdb_native(sql: String, schema: StructType) -> Self {
        Self {
            sql,
            schema,
            duckdb_ready: true,
            view_name: None,
        }
    }
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

/// Typed DDL operations — replaces string-based DDL detection.
#[derive(Debug, Clone, PartialEq)]
pub enum DdlOperation {
    /// DROP VIEW with catalog-lookup-friendly name.
    DropView { view_name: String, if_exists: bool },
    /// DROP TABLE.
    DropTable { table_name: String, if_exists: bool },
    /// CREATE [OR REPLACE] [TEMP] VIEW ... AS ...
    CreateView {
        view_name: String,
        sql: String,
        schema: StructType,
    },
    /// CREATE TABLE ...
    CreateTable { sql: String },
    /// ALTER TABLE ...
    AlterTable { sql: String },
    /// TRUNCATE TABLE ...
    Truncate { sql: String },
    /// INSERT INTO ...
    Insert { sql: String },
    /// Fallback for DDL not yet structured.
    Other { sql: String },
}

/// A typed DDL statement with structured operation metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct DdlStatement {
    /// The typed DDL operation.
    pub operation: DdlOperation,
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
    pub replacements: Vec<(
        String,
        crate::expression::Literal,
        crate::expression::Literal,
    )>,
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
                let left_nullable = matches!(j.join_type, JoinType::Right | JoinType::Full);
                let merged = if left_nullable || right_nullable {
                    let fields = merged
                        .fields
                        .into_iter()
                        .enumerate()
                        .map(|(i, mut f)| {
                            if (i < left_len && left_nullable) || (i >= left_len && right_nullable)
                            {
                                f.nullable = true;
                            }
                            f
                        })
                        .collect();
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
                                if l == r {
                                    Some(vec![l])
                                } else {
                                    None
                                }
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
                        if !using_set.contains(f.name.as_str()) {
                            fields.push(f.clone());
                        }
                    }
                    // 3. Right non-USING columns
                    for f in &right_schema.fields {
                        if !using_set.contains(f.name.as_str()) {
                            fields.push(f.clone());
                        }
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
                let fields = left
                    .fields
                    .into_iter()
                    .zip(right.fields)
                    .map(|(mut lf, rf)| {
                        lf.data_type = crate::types::TypeInferenceEngine::unify_types(
                            &lf.data_type,
                            &rf.data_type,
                        );
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
            LogicalPlan::TableScan(t) => t.schema.clone(),
            LogicalPlan::SqlRelation(r) => r.schema.clone(),
            LogicalPlan::LocalRelation(r) => r.schema.clone(),
            LogicalPlan::LocalDataRelation(r) => r.schema.clone(),
            LogicalPlan::RangeRelation(_) => {
                StructType::new(vec![StructField::not_null("id", DataType::Long)])
            }
            LogicalPlan::InMemoryRelation(r) => r.schema.clone(),
            LogicalPlan::WithCte(c) => c.input.infer_schema(),
            LogicalPlan::WithColumns(w) => infer_with_columns_schema(w),
            LogicalPlan::AliasedRelation(a) => {
                let child = a.input.infer_schema();
                if !a.column_aliases.is_empty() && a.column_aliases.len() == child.fields.len() {
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
                }
            }
            LogicalPlan::DdlStatement(_) => StructType::empty(),
            LogicalPlan::ToDataFrame(t) => infer_to_dataframe_schema(t),
            LogicalPlan::SingleRow(_) => StructType::empty(),
            LogicalPlan::DropColumns(d) => {
                let child = d.input.infer_schema();
                let excluded: std::collections::HashSet<&str> =
                    d.column_names.iter().map(String::as_str).collect();
                StructType::new(
                    child
                        .fields
                        .into_iter()
                        .filter(|f| !excluded.contains(f.name.as_str()))
                        .collect(),
                )
            }
            LogicalPlan::ShowString(s) => s.input.infer_schema(),
            LogicalPlan::NADrop(n) => n.input.infer_schema(),
            LogicalPlan::NAFill(n) => {
                // na.fill(valueMap) guarantees no NULLs for filled columns → mark them NOT NULL.
                let mut schema = n.input.infer_schema();
                let filled_cols: std::collections::HashSet<String> = n
                    .values
                    .iter()
                    .map(|(name, _)| name.to_lowercase())
                    .collect();
                schema.fields = schema
                    .fields
                    .into_iter()
                    .map(|mut f| {
                        if filled_cols.contains(&f.name.to_lowercase()) {
                            f.nullable = false;
                        }
                        f
                    })
                    .collect();
                schema
            }
            LogicalPlan::NAReplace(n) => n.input.infer_schema(),
            LogicalPlan::Unpivot(u) => {
                let input_schema = u.input.infer_schema();
                let mut fields: Vec<StructField> = u
                    .ids
                    .iter()
                    .map(|name| {
                        if let Some(f) = input_schema.field_by_name(name) {
                            StructField::new(name.clone(), f.data_type.clone(), f.nullable)
                        } else {
                            StructField::nullable(name.clone(), DataType::Unresolved)
                        }
                    })
                    .collect();
                // Variable column is generated from column names → always NOT NULL.
                fields.push(StructField::not_null(
                    u.variable_column_name.clone(),
                    DataType::String,
                ));
                // Value column nullable = OR of all input value columns' nullable flags.
                let value_nullable = u
                    .values
                    .iter()
                    .any(|v| input_schema.field_by_name(v).map_or(true, |f| f.nullable));
                let value_type = u
                    .values
                    .iter()
                    .find_map(|v| input_schema.field_by_name(v).map(|f| f.data_type.clone()))
                    .unwrap_or(DataType::Unresolved);
                fields.push(StructField::new(
                    u.value_column_name.clone(),
                    value_type,
                    value_nullable,
                ));
                StructType::new(fields)
            }
            // Pivot schema depends on runtime data (distinct pivot values);
            // full DuckDB fallback in the service layer.
            LogicalPlan::Pivot(_) => StructType::empty(),
            // StatCov/StatCorr return a single DOUBLE column.
            LogicalPlan::StatCov(s) => StructType::new(vec![StructField::nullable(
                format!("cov({}, {})", s.col1, s.col2),
                DataType::Double,
            )]),
            LogicalPlan::StatCorr(s) => StructType::new(vec![StructField::nullable(
                format!("corr({}, {})", s.col1, s.col2),
                DataType::Double,
            )]),
            // ApproxQuantile returns one column of ARRAY<DOUBLE>, one row per input column.
            LogicalPlan::ApproxQuantile(_) => StructType::new(vec![StructField::nullable(
                "quantiles".to_string(),
                DataType::Array(Box::new(DataType::Double), true),
            )]),
            // Crosstab: pivot columns unknown at plan time → DuckDB fallback.
            LogicalPlan::StatCrosstab(_) => StructType::empty(),
            // FreqItems: one Array<String> column per input col, named "{col}_freqItems".
            LogicalPlan::StatFreqItems(s) => StructType::new(
                s.cols
                    .iter()
                    .map(|c| {
                        StructField::nullable(
                            format!("{}_freqItems", c),
                            DataType::Array(Box::new(DataType::String), true),
                        )
                    })
                    .collect(),
            ),
            // SampleBy: same schema as input.
            LogicalPlan::StatSampleBy(s) => s.input.infer_schema(),
            // Describe/Summary: "summary" VARCHAR + one VARCHAR column per input column.
            LogicalPlan::Describe(d) => {
                let mut fields = vec![StructField::nullable("summary", DataType::String)];
                for col in &d.cols {
                    fields.push(StructField::nullable(col.clone(), DataType::String));
                }
                StructType::new(fields)
            }
            LogicalPlan::Summary(s) => {
                let mut fields = vec![StructField::nullable("summary", DataType::String)];
                for col in &s.cols {
                    fields.push(StructField::nullable(col.clone(), DataType::String));
                }
                StructType::new(fields)
            }
        }
    }

    /// Maximum depth of the plan tree.
    ///
    /// Used to prevent stack overflow from deeply nested plans
    /// (e.g., from malicious clients). Leaf nodes have depth 1.
    pub fn depth(&self) -> usize {
        match self {
            // Single-child nodes
            LogicalPlan::Project(p) => 1 + p.input.depth(),
            LogicalPlan::Filter(f) => 1 + f.input.depth(),
            LogicalPlan::Aggregate(a) => 1 + a.input.depth(),
            LogicalPlan::Sort(s) => 1 + s.input.depth(),
            LogicalPlan::Limit(l) => 1 + l.input.depth(),
            LogicalPlan::Tail(t) => 1 + t.input.depth(),
            LogicalPlan::Distinct(d) => 1 + d.input.depth(),
            LogicalPlan::WithColumns(w) => 1 + w.input.depth(),
            LogicalPlan::DropColumns(d) => 1 + d.input.depth(),
            LogicalPlan::Sample(s) => 1 + s.input.depth(),
            LogicalPlan::AliasedRelation(a) => 1 + a.input.depth(),
            LogicalPlan::ToDataFrame(t) => 1 + t.input.depth(),
            LogicalPlan::ShowString(s) => 1 + s.input.depth(),
            LogicalPlan::NADrop(n) => 1 + n.input.depth(),
            LogicalPlan::NAFill(n) => 1 + n.input.depth(),
            LogicalPlan::NAReplace(n) => 1 + n.input.depth(),
            LogicalPlan::Unpivot(u) => 1 + u.input.depth(),
            LogicalPlan::Pivot(p) => 1 + p.input.depth(),
            LogicalPlan::StatCov(s) => 1 + s.input.depth(),
            LogicalPlan::StatCorr(s) => 1 + s.input.depth(),
            LogicalPlan::ApproxQuantile(a) => 1 + a.input.depth(),
            LogicalPlan::StatCrosstab(s) => 1 + s.input.depth(),
            LogicalPlan::StatFreqItems(s) => 1 + s.input.depth(),
            LogicalPlan::StatSampleBy(s) => 1 + s.input.depth(),
            LogicalPlan::Describe(d) => 1 + d.input.depth(),
            LogicalPlan::Summary(s) => 1 + s.input.depth(),
            // Two-child nodes
            LogicalPlan::Join(j) => 1 + j.left.depth().max(j.right.depth()),
            LogicalPlan::Union(u) => 1 + u.left.depth().max(u.right.depth()),
            LogicalPlan::Except(e) => 1 + e.left.depth().max(e.right.depth()),
            LogicalPlan::Intersect(i) => 1 + i.left.depth().max(i.right.depth()),
            // CTE: max of input and all CTE definitions
            LogicalPlan::WithCte(c) => {
                let cte_depth = c.ctes.iter().map(|(_, p)| p.depth()).max().unwrap_or(0);
                1 + c.input.depth().max(cte_depth)
            }
            // Leaf nodes
            LogicalPlan::TableScan(_)
            | LogicalPlan::SqlRelation(_)
            | LogicalPlan::LocalRelation(_)
            | LogicalPlan::LocalDataRelation(_)
            | LogicalPlan::RangeRelation(_)
            | LogicalPlan::InMemoryRelation(_)
            | LogicalPlan::DdlStatement(_)
            | LogicalPlan::SingleRow(_) => 1,
        }
    }

    /// Returns true when `infer_schema()` produces a partial schema
    /// Returns true when `infer_schema()` produces a partial schema
    /// that needs DuckDB merge for missing columns.
    pub fn has_partial_schema(&self) -> bool {
        false // No plan currently produces partial schemas
    }
}

fn infer_project_schema(p: &Project) -> StructType {
    let child_schema = p.input.infer_schema();
    let has_star = p
        .projections
        .iter()
        .any(|e| matches!(e, Expression::Star(_)));
    // If we have a wildcard but can't statically resolve the child schema (e.g. TableScan),
    // collect explicitly computed (non-star) projections with known types and return them
    // alongside a sentinel Unresolved field. The sentinel forces `has_unresolved = true` in
    // service.rs, triggering DuckDB schema lookup; the name-based merge then overlays the
    // computed types (e.g. row_number → Integer) onto DuckDB's full expanded schema.
    if has_star && child_schema.is_empty() {
        let mut computed: Vec<StructField> = p
            .projections
            .iter()
            .filter(|e| !matches!(e, Expression::Star(_)))
            .filter_map(|e| projection_to_field(e, &child_schema))
            .filter(|f| !f.data_type.contains_unresolved())
            .collect();
        if computed.is_empty() {
            return StructType::empty();
        }
        // Sentinel: forces has_unresolved=true so service.rs falls back to DuckDB
        // and then applies a name-based merge. Filtered from the final result by name-based
        // merge since it won't match any DuckDB column.
        computed.push(StructField::new(
            "__star_expansion_sentinel__".to_string(),
            DataType::Unresolved,
            true,
        ));
        return StructType::new(computed);
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
        Expression::Alias(a) => Some(StructField::new(
            a.alias.clone(),
            a.expr.data_type(schema),
            a.expr.nullable(schema),
        )),
        Expression::ColumnReference(c) => Some(StructField::new(
            c.name.clone(),
            c.data_type.clone().pipe_if_unresolved(|| {
                crate::types::TypeInferenceEngine::column_type(&c.name, schema)
            }),
            crate::types::TypeInferenceEngine::column_nullable(&c.name, schema),
        )),
        Expression::UnresolvedColumn(u) => {
            let dt = crate::types::TypeInferenceEngine::column_type(&u.name, schema);
            Some(StructField::new(
                u.name.clone(),
                dt,
                crate::types::TypeInferenceEngine::column_nullable(&u.name, schema),
            ))
        }
        Expression::Star(_) => None, // expanded by caller
        Expression::RawSql(r) => {
            let col_name = spark_column_name(expr);
            // Use type hints from RawSqlExpression if available (e.g. explode-map expansion).
            // Otherwise fall back to schema lookup (ExpressionString from selectExpr()).
            let dt = r.data_type.clone().unwrap_or_else(|| {
                crate::types::TypeInferenceEngine::column_type(&col_name, schema)
            });
            let nullable = r.nullable.unwrap_or_else(|| {
                crate::types::TypeInferenceEngine::column_nullable(&col_name, schema)
            });
            Some(StructField::new(col_name, dt, nullable))
        }
        other => {
            let dt = other.data_type(schema);
            Some(StructField::new(
                spark_column_name(other),
                dt,
                other.nullable(schema),
            ))
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
                && f.args.iter().any(|a| {
                    matches!(
                        a,
                        Expression::Star(_)
                            | Expression::Literal(crate::expression::Literal {
                                value: crate::expression::LiteralValue::Int(1),
                                ..
                            })
                    )
                });
            if is_count_star {
                return "count(1)".to_string();
            }
            let args = f
                .args
                .iter()
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
                let unquoted = if candidate.starts_with('"')
                    && candidate.ends_with('"')
                    && candidate.len() > 2
                {
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
        other => crate::types::TypeMapper::to_duckdb(other).into_owned(),
    }
}

fn infer_aggregate_schema(a: &Aggregate) -> StructType {
    let child_schema = a.input.infer_schema();
    // When the child schema is empty (e.g. SQL path in relaxed mode without table-scan
    // enrichment), aggregate output type inference is unreliable — CaseWhen with `ELSE 0`
    // can mis-infer as Integer even when THEN branches are Decimal. Returning empty here
    // ensures that downstream consumers (e.g. UNION, outer aggregates) don't propagate
    // wrong types that would trigger spurious CAST(SUM(...) AS BIGINT).
    if child_schema.is_empty() {
        return StructType::empty();
    }
    let mut fields = Vec::new();

    let use_select_order = !a.select_order.is_empty();

    if use_select_order {
        // If no grouping column appears in select_order (e.g. groupBy().count() shorthand),
        // prepend all grouping columns so the schema matches the actual SELECT output.
        // GroupingNotSelected suppresses the prepend for SQL path (GROUP BY not in SELECT).
        let has_grouping_in_order = a.select_order.iter().any(|e| {
            matches!(
                e,
                SelectEntry::GroupingExpr(_) | SelectEntry::GroupingNotSelected
            )
        });
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
                SelectEntry::AggregateExpr(agg) => {
                    if let Some(f) = agg_expr_to_field(&agg.func, &child_schema) {
                        fields.push(f);
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

    // ROLLUP/CUBE/GROUPING SETS produce NULL in grouping columns for
    // subtotal/grand-total rows.  Collect ALL grouping column names (from
    // both `a.grouping` and the grouping-sets specification) and mark any
    // matching field nullable — regardless of its position in the SELECT list.
    if let Some(gs) = &a.grouping_sets {
        let mut gs_names = gs.column_names();
        // Also include columns from `a.grouping` (plain GROUP BY keys that
        // accompany ROLLUP, e.g. `GROUP BY a, ROLLUP(b, c)`).
        for g in &a.grouping {
            if let Some(name) = grouping_expr_name(g) {
                gs_names.insert(name.to_lowercase());
            }
        }
        for f in &mut fields {
            if gs_names.contains(&f.name.to_lowercase()) {
                f.nullable = true;
            }
        }
    }
    StructType::new(fields)
}

fn agg_expr_to_field(expr: &Expression, schema: &StructType) -> Option<StructField> {
    match expr {
        Expression::Alias(a) => {
            // Use aggregate_return_type for aggregate FunctionCalls inside an alias,
            // since function_return_type() (scalar) doesn't know about COUNT/SUM/AVG.
            let (dt, nullable) = match a.expr.as_ref() {
                Expression::FunctionCall(f) => {
                    let arg_types: Vec<_> = f.args.iter().map(|e| e.data_type(schema)).collect();
                    let dt = crate::types::TypeInferenceEngine::aggregate_return_type(
                        &f.name,
                        arg_types.first().unwrap_or(&DataType::Unresolved),
                    );
                    (dt, a.expr.nullable(schema))
                }
                other => (other.data_type(schema), other.nullable(schema)),
            };
            Some(StructField::new(a.alias.clone(), dt, nullable))
        }
        Expression::FunctionCall(f) => {
            let arg_types: Vec<_> = f.args.iter().map(|a| a.data_type(schema)).collect();
            let dt = crate::types::TypeInferenceEngine::aggregate_return_type(
                &f.name,
                arg_types.first().unwrap_or(&DataType::Unresolved),
            );
            Some(StructField::new(
                spark_column_name(expr),
                dt,
                expr.nullable(schema),
            ))
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
            // Rename: find the old column and change its name in-place, preserving nullability.
            if let Some(idx) = schema.field_index(old_name) {
                let (dt, old_nullable) = {
                    let f = &schema.fields[idx];
                    (f.data_type.clone(), f.nullable)
                };
                schema.fields[idx] = StructField::new(new_name.clone(), dt, old_nullable);
            }
        } else {
            let dt = expr.data_type(&schema);
            let nullable = expr.nullable(&schema);
            // Replace existing field or append
            if let Some(idx) = schema.field_index(new_name) {
                schema.fields[idx] = StructField::new(new_name.clone(), dt, nullable);
            } else {
                schema
                    .fields
                    .push(StructField::new(new_name.clone(), dt, nullable));
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
    let mut fields: Vec<StructField> = child
        .fields
        .into_iter()
        .zip(t.column_names.iter())
        .map(|(mut f, name)| {
            f.name = name.clone();
            f
        })
        .collect();
    // Extra names beyond child column count → String fields (matches Java behaviour)
    for name in t.column_names.iter().skip(extra_start) {
        fields.push(StructField::nullable(name.clone(), DataType::String));
    }
    StructType::new(fields)
}

use crate::types::data_type::PipeIfUnresolved;

// ── Constructors ──────────────────────────────────────────────────────────────

impl LogicalPlan {
    pub fn table_scan(table: impl Into<String>) -> Self {
        LogicalPlan::TableScan(TableScan {
            table: table.into(),
            alias: None,
            schema: Default::default(),
        })
    }
    pub fn filter(input: LogicalPlan, condition: Expression) -> Self {
        LogicalPlan::Filter(Filter {
            input: Box::new(input),
            condition,
        })
    }
    pub fn project(input: LogicalPlan, projections: Vec<Expression>) -> Self {
        LogicalPlan::Project(Project {
            input: Box::new(input),
            projections,
        })
    }
    pub fn limit(input: LogicalPlan, n: i64) -> Self {
        use crate::expression::Literal;
        LogicalPlan::Limit(Limit {
            input: Box::new(input),
            limit: Literal::long(n),
        })
    }

    /// Collect pivot grouping column nullable overrides from any `Pivot` node
    /// in the plan tree. Returns a map of column_name -> nullable derived from
    /// the Pivot's input schema. Empty if no Pivot is found.
    ///
    /// Unlike the old `find_pivot()` in service.rs, this traverses ALL child
    /// nodes — including multi-child operators like Join and Union.
    pub fn pivot_grouping_nullable_overrides(&self) -> std::collections::HashMap<String, bool> {
        match self {
            LogicalPlan::Pivot(p) => {
                let input_schema = p.input.infer_schema();
                if input_schema.is_empty() {
                    return std::collections::HashMap::new();
                }
                p.grouping
                    .iter()
                    .filter_map(|expr| {
                        let name = match expr {
                            Expression::ColumnReference(c) => Some(c.name.clone()),
                            Expression::UnresolvedColumn(u) => Some(u.name.clone()),
                            Expression::Alias(a) => Some(a.alias.clone()),
                            _ => None,
                        };
                        name.and_then(|n| input_schema.field_by_name(&n).map(|f| (n, f.nullable)))
                    })
                    .collect()
            }
            // Single-child passthrough nodes
            LogicalPlan::Project(p) => p.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Filter(f) => f.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Sort(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Limit(l) => l.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Tail(t) => t.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Distinct(d) => d.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::WithColumns(w) => w.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::DropColumns(d) => d.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Aggregate(a) => a.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Sample(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::AliasedRelation(a) => a.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Unpivot(u) => u.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::NADrop(n) => n.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::NAFill(n) => n.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::NAReplace(n) => n.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::ShowString(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::ToDataFrame(t) => t.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Describe(d) => d.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::Summary(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::StatCov(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::StatCorr(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::ApproxQuantile(a) => a.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::StatCrosstab(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::StatFreqItems(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::StatSampleBy(s) => s.input.pivot_grouping_nullable_overrides(),
            LogicalPlan::WithCte(c) => c.input.pivot_grouping_nullable_overrides(),
            // Multi-child nodes: merge both sides
            LogicalPlan::Join(j) => {
                let mut m = j.left.pivot_grouping_nullable_overrides();
                m.extend(j.right.pivot_grouping_nullable_overrides());
                m
            }
            LogicalPlan::Union(u) => {
                let mut m = u.left.pivot_grouping_nullable_overrides();
                m.extend(u.right.pivot_grouping_nullable_overrides());
                m
            }
            LogicalPlan::Except(e) => {
                let mut m = e.left.pivot_grouping_nullable_overrides();
                m.extend(e.right.pivot_grouping_nullable_overrides());
                m
            }
            LogicalPlan::Intersect(i) => {
                let mut m = i.left.pivot_grouping_nullable_overrides();
                m.extend(i.right.pivot_grouping_nullable_overrides());
                m
            }
            // Leaf nodes — no pivot possible
            LogicalPlan::TableScan(_)
            | LogicalPlan::SqlRelation(_)
            | LogicalPlan::LocalRelation(_)
            | LogicalPlan::LocalDataRelation(_)
            | LogicalPlan::RangeRelation(_)
            | LogicalPlan::InMemoryRelation(_)
            | LogicalPlan::SingleRow(_)
            | LogicalPlan::DdlStatement(_) => std::collections::HashMap::new(),
        }
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

    #[test]
    fn depth_leaf_node_is_one() {
        let leaf = LogicalPlan::TableScan(TableScan {
            table: "t".into(),
            alias: None,
            schema: StructType::empty(),
        });
        assert_eq!(leaf.depth(), 1);
    }

    #[test]
    fn depth_single_child_chain() {
        let leaf = LogicalPlan::TableScan(TableScan {
            table: "t".into(),
            alias: None,
            schema: StructType::empty(),
        });
        let project = LogicalPlan::project(leaf, vec![]);
        assert_eq!(project.depth(), 2);

        let filtered = LogicalPlan::filter(project, Literal::boolean(true));
        assert_eq!(filtered.depth(), 3);
    }

    #[test]
    fn depth_join_takes_max_of_children() {
        let left = LogicalPlan::project(LogicalPlan::table_scan("a"), vec![]);
        // left depth = 2
        let right = LogicalPlan::table_scan("b");
        // right depth = 1

        let join = LogicalPlan::Join(Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_alias: None,
            right_alias: None,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        // join depth = 1 + max(2, 1) = 3
        assert_eq!(join.depth(), 3);
    }

    // ── pivot_grouping_nullable_overrides tests ──────────────────────────

    fn make_pivot_plan() -> LogicalPlan {
        use crate::expression::UnresolvedColumn;
        let input = LogicalPlan::LocalRelation(LocalRelation {
            schema: StructType::new(vec![
                StructField::not_null("product", DataType::String),
                StructField::nullable("region", DataType::String),
                StructField::nullable("sales", DataType::Double),
            ]),
        });
        LogicalPlan::Pivot(Pivot {
            input: Box::new(input),
            grouping: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "product".to_owned(),
                qualifier: None,
            })],
            pivot_col: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "region".to_owned(),
                qualifier: None,
            }),
            pivot_values: vec![],
            aggregates: vec![],
        })
    }

    #[test]
    fn pivot_overrides_returns_grouping_nullable() {
        let plan = make_pivot_plan();
        let overrides = plan.pivot_grouping_nullable_overrides();
        assert_eq!(overrides.len(), 1);
        // "product" is non-nullable in the input schema
        assert_eq!(overrides.get("product"), Some(&false));
    }

    #[test]
    fn pivot_overrides_through_sort_and_project() {
        let pivot = make_pivot_plan();
        let sorted = LogicalPlan::Sort(Sort {
            input: Box::new(pivot),
            order: vec![],
            limit: None,
            offset: None,
        });
        let projected = LogicalPlan::project(sorted, vec![]);
        let overrides = projected.pivot_grouping_nullable_overrides();
        assert_eq!(overrides.get("product"), Some(&false));
    }

    #[test]
    fn pivot_overrides_through_join() {
        let pivot = make_pivot_plan();
        let right = LogicalPlan::table_scan("other");
        let join = LogicalPlan::Join(Join {
            left: Box::new(pivot),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_alias: None,
            right_alias: None,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let overrides = join.pivot_grouping_nullable_overrides();
        assert_eq!(overrides.get("product"), Some(&false));
    }

    #[test]
    fn pivot_overrides_through_union() {
        let pivot = make_pivot_plan();
        let other = LogicalPlan::table_scan("other");
        let union_plan = LogicalPlan::Union(Union {
            left: Box::new(pivot),
            right: Box::new(other),
            all: true,
        });
        let overrides = union_plan.pivot_grouping_nullable_overrides();
        assert_eq!(overrides.get("product"), Some(&false));
    }

    #[test]
    fn pivot_overrides_empty_for_leaf() {
        let leaf = LogicalPlan::table_scan("t");
        let overrides = leaf.pivot_grouping_nullable_overrides();
        assert!(overrides.is_empty());
    }
}
