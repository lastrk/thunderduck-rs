use crate::types::{DataType, StructType, TypeInferenceEngine};

// ── Supporting types ──────────────────────────────────────────────────────────

/// Binary operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    Concat, // String ||
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
}

impl BinaryOp {
    pub fn symbol(&self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Eq => "=",
            BinaryOp::NotEq => "<>",
            BinaryOp::Lt => "<",
            BinaryOp::LtEq => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::GtEq => ">=",
            BinaryOp::And => "AND",
            BinaryOp::Or => "OR",
            BinaryOp::Concat => "||",
            BinaryOp::BitwiseAnd => "&",
            BinaryOp::BitwiseOr => "|",
            BinaryOp::BitwiseXor => "^",
        }
    }

    /// Whether this operator needs space padding (word operators do, symbol ones don't).
    pub fn needs_spaces(&self) -> bool {
        matches!(self, BinaryOp::And | BinaryOp::Or)
    }
}

/// Unary prefix/suffix operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Negate,
    IsNull,
    IsNotNull,
    IsNaN,
    IsNotNaN,
}

/// Sort direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// How NULLs are sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NullOrdering {
    NullsFirst,
    NullsLast,
}

/// A sort expression with direction and null ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct SortOrder {
    pub expr: Expression,
    pub direction: SortDirection,
    pub null_ordering: NullOrdering,
}

impl SortOrder {
    pub fn asc(expr: Expression) -> Self {
        Self { expr, direction: SortDirection::Asc, null_ordering: NullOrdering::NullsFirst }
    }
    pub fn desc(expr: Expression) -> Self {
        Self { expr, direction: SortDirection::Desc, null_ordering: NullOrdering::NullsLast }
    }
}

/// Window frame unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameUnit {
    Rows,
    Range,
}

/// Window frame boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum FrameBoundary {
    UnboundedPreceding,
    Preceding(Box<Expression>),
    CurrentRow,
    Following(Box<Expression>),
    UnboundedFollowing,
}

/// Window frame specification.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowFrame {
    pub unit: FrameUnit,
    pub start: FrameBoundary,
    pub end: FrameBoundary,
}

/// A scalar literal value (typed).
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Null,
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Decimal(String), // stored as string to avoid precision loss
    String(String),
    Date(i32),       // days since epoch
    Timestamp(i64),  // microseconds since epoch
    TimestampNtz(i64),
    Binary(Vec<u8>),
}

// ── Expression enum ───────────────────────────────────────────────────────────

/// The closed set of all expression types understood by Thunderduck.
///
/// **Critical rule**: use `to_sql()` to generate SQL. `Display` / `Debug`
/// are for human-readable debugging only and MUST NEVER be used to produce
/// SQL strings sent to DuckDB.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    Literal(Literal),
    ColumnReference(ColumnReference),
    UnresolvedColumn(UnresolvedColumn),
    Binary(BinaryExpression),
    Unary(UnaryExpression),
    FunctionCall(FunctionCall),
    Cast(CastExpression),
    CaseWhen(CaseWhenExpression),
    Window(WindowFunction),
    Alias(AliasExpression),
    Star(StarExpression),
    InSubquery(InSubquery),
    ExistsSubquery(ExistsSubquery),
    ScalarSubquery(ScalarSubquery),
    Lambda(LambdaExpression),
    LambdaVariable(LambdaVariableExpression),
    RawSql(RawSqlExpression),
    ArrayLiteral(ArrayLiteralExpression),
    MapLiteral(MapLiteralExpression),
    StructLiteral(StructLiteralExpression),
    Between(BetweenExpression),
    InList(InListExpression),
    Like(LikeExpression),
    Interval(IntervalExpression),
    IsDistinctFrom(IsDistinctFromExpression),
    ExtractValue(ExtractValueExpression),
    RowConstructor(RowConstructorExpression),
}

// ── Expression inner types ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Literal {
    pub value: LiteralValue,
    pub data_type: DataType,
}

impl Literal {
    pub fn null() -> Expression {
        Expression::Literal(Literal { value: LiteralValue::Null, data_type: DataType::Null })
    }
    pub fn boolean(v: bool) -> Expression {
        Expression::Literal(Literal { value: LiteralValue::Boolean(v), data_type: DataType::Boolean })
    }
    pub fn int(v: i32) -> Expression {
        Expression::Literal(Literal { value: LiteralValue::Int(v), data_type: DataType::Integer })
    }
    pub fn long(v: i64) -> Expression {
        Expression::Literal(Literal { value: LiteralValue::Long(v), data_type: DataType::Long })
    }
    pub fn double(v: f64) -> Expression {
        Expression::Literal(Literal { value: LiteralValue::Double(v), data_type: DataType::Double })
    }
    pub fn string(v: impl Into<String>) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::String(v.into()),
            data_type: DataType::String,
        })
    }
    pub fn decimal(v: impl Into<String>, precision: u8, scale: u8) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Decimal(v.into()),
            data_type: DataType::Decimal { precision, scale },
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnReference {
    pub name: String,
    pub qualifier: Option<String>,
    pub data_type: DataType,
    pub nullable: bool,
}

impl ColumnReference {
    pub fn untyped(name: impl Into<String>) -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: name.into(),
            qualifier: None,
            data_type: DataType::Unresolved,
            nullable: true,
        })
    }
    pub fn qualified(qualifier: impl Into<String>, name: impl Into<String>) -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: name.into(),
            qualifier: Some(qualifier.into()),
            data_type: DataType::Unresolved,
            nullable: true,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnresolvedColumn {
    pub name: String,
    pub qualifier: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BinaryExpression {
    pub op: BinaryOp,
    pub left: Box<Expression>,
    pub right: Box<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnaryExpression {
    pub op: UnaryOp,
    pub operand: Box<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub args: Vec<Expression>,
    pub distinct: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CastExpression {
    pub expr: Box<Expression>,
    pub to_type: DataType,
    pub try_cast: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseWhenExpression {
    /// Optional base expression: CASE <base> WHEN val THEN result …
    pub base: Option<Box<Expression>>,
    pub branches: Vec<(Expression, Expression)>, // (condition/value, result)
    pub else_expr: Option<Box<Expression>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowFunction {
    pub func: Box<Expression>, // usually FunctionCall
    pub partition_by: Vec<Expression>,
    pub order_by: Vec<SortOrder>,
    pub frame: Option<WindowFrame>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AliasExpression {
    pub expr: Box<Expression>,
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StarExpression {
    pub qualifier: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InSubquery {
    pub expr: Box<Expression>,
    pub subquery: Box<crate::logical::LogicalPlan>,
    pub negated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExistsSubquery {
    pub subquery: Box<crate::logical::LogicalPlan>,
    pub negated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScalarSubquery {
    pub subquery: Box<crate::logical::LogicalPlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LambdaExpression {
    pub params: Vec<String>,
    pub body: Box<Expression>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LambdaVariableExpression {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawSqlExpression {
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArrayLiteralExpression {
    pub elements: Vec<Expression>,
    pub element_type: DataType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapLiteralExpression {
    pub keys: Vec<Expression>,
    pub values: Vec<Expression>,
    pub key_type: DataType,
    pub value_type: DataType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructLiteralExpression {
    pub fields: Vec<(String, Expression)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BetweenExpression {
    pub expr: Box<Expression>,
    pub low: Box<Expression>,
    pub high: Box<Expression>,
    pub negated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InListExpression {
    pub expr: Box<Expression>,
    pub list: Vec<Expression>,
    pub negated: bool,
}

/// LIKE / NOT LIKE / ILIKE / NOT ILIKE predicate.
#[derive(Debug, Clone, PartialEq)]
pub struct LikeExpression {
    pub value: Box<Expression>,
    pub pattern: Box<Expression>,
    /// When true, generates NOT LIKE / NOT ILIKE.
    pub negated: bool,
    /// When true, uses ILIKE (case-insensitive). DuckDB supports ILIKE natively.
    pub case_insensitive: bool,
}

/// An interval literal decomposed into month / day / microsecond components.
///
/// Three sub-types:
/// - Year-month: only `months` is set (days and microseconds are 0)
/// - Day-time: only `microseconds` is set (months and days are 0)
/// - Calendar: any combination of the three
#[derive(Debug, Clone, PartialEq)]
pub struct IntervalExpression {
    pub months: i32,
    pub days: i32,
    pub microseconds: i64,
}

/// IS [NOT] DISTINCT FROM — null-safe equality comparison.
///
/// Unlike `=`, this always returns a non-null boolean even when operands are NULL.
#[derive(Debug, Clone, PartialEq)]
pub struct IsDistinctFromExpression {
    pub left: Box<Expression>,
    pub right: Box<Expression>,
    /// When true, generates IS NOT DISTINCT FROM.
    pub negated: bool,
}

/// Extracts a value from a complex type (struct field, array element, map key).
///
/// The SQL form is `child[extraction]`. String-literal extractions use bracket
/// notation: `child['field']`. Numeric extractions use `child[idx]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractValueExpression {
    pub child: Box<Expression>,
    pub extraction: Box<Expression>,
}

/// Row / tuple constructor: `(a, b, c)`.
///
/// Used in tuple comparisons such as `WHERE (x, y) IN ((1, 2), (3, 4))`.
#[derive(Debug, Clone, PartialEq)]
pub struct RowConstructorExpression {
    pub fields: Vec<Expression>,
}

// ── Type inference on Expression ──────────────────────────────────────────────

impl Expression {
    /// Infer the Spark DataType of this expression in the context of `schema`.
    pub fn data_type(&self, schema: &StructType) -> DataType {
        match self {
            Expression::Literal(l) => l.data_type.clone(),
            Expression::ColumnReference(c) => {
                if c.data_type != DataType::Unresolved {
                    return c.data_type.clone();
                }
                if let Some(q) = &c.qualifier {
                    TypeInferenceEngine::column_type(&format!("{}.{}", q, c.name), schema)
                        .pipe_if_unresolved(|| TypeInferenceEngine::column_type(&c.name, schema))
                } else {
                    TypeInferenceEngine::column_type(&c.name, schema)
                }
            }
            Expression::UnresolvedColumn(u) => {
                TypeInferenceEngine::column_type(&u.name, schema)
            }
            Expression::Binary(b) => {
                use BinaryOp::*;
                match &b.op {
                    Eq | NotEq | Lt | LtEq | Gt | GtEq | And | Or => DataType::Boolean,
                    Concat => DataType::String,
                    _ => {
                        let lt = b.left.data_type(schema);
                        let rt = b.right.data_type(schema);
                        TypeInferenceEngine::promote_numeric(&lt, &rt)
                    }
                }
            }
            Expression::Unary(u) => match &u.op {
                UnaryOp::Not | UnaryOp::IsNull | UnaryOp::IsNotNull
                | UnaryOp::IsNaN | UnaryOp::IsNotNaN => DataType::Boolean,
                UnaryOp::Negate => u.operand.data_type(schema),
            },
            Expression::FunctionCall(f) => {
                let arg_types: Vec<DataType> =
                    f.args.iter().map(|a| a.data_type(schema)).collect();
                TypeInferenceEngine::function_return_type(&f.name, &arg_types)
            }
            Expression::Cast(c) => c.to_type.clone(),
            Expression::CaseWhen(cw) => {
                cw.branches
                    .first()
                    .map(|(_, r)| r.data_type(schema))
                    .unwrap_or(DataType::Unresolved)
            }
            Expression::Window(w) => match w.func.as_ref() {
                Expression::FunctionCall(f) => {
                    let arg_types: Vec<_> = f.args.iter().map(|a| a.data_type(schema)).collect();
                    TypeInferenceEngine::window_return_type(&f.name, arg_types.first())
                }
                other => other.data_type(schema),
            },
            Expression::Alias(a) => a.expr.data_type(schema),
            Expression::Star(_) => DataType::Unresolved,
            Expression::InSubquery(_) | Expression::ExistsSubquery(_) => DataType::Boolean,
            Expression::InList(_) => DataType::Boolean,
            Expression::ScalarSubquery(_) => DataType::Unresolved,
            Expression::Lambda(l) => l.body.data_type(schema),
            Expression::LambdaVariable(_) => DataType::Unresolved,
            Expression::RawSql(_) => DataType::Unresolved,
            Expression::ArrayLiteral(a) => DataType::Array(Box::new(a.element_type.clone())),
            Expression::MapLiteral(m) => DataType::Map {
                key: Box::new(m.key_type.clone()),
                value: Box::new(m.value_type.clone()),
                value_nullable: true,
            },
            Expression::StructLiteral(_) => DataType::Unresolved,
            Expression::Between(_) => DataType::Boolean,
            // New variants
            Expression::Like(_) | Expression::IsDistinctFrom(_) => DataType::Boolean,
            Expression::Interval(_) => DataType::String, // TODO: proper IntervalType
            Expression::ExtractValue(_) | Expression::RowConstructor(_) => DataType::Unresolved,
        }
    }

    /// Whether this expression can produce NULL values.
    pub fn nullable(&self, schema: &StructType) -> bool {
        match self {
            Expression::Literal(l) => matches!(l.value, LiteralValue::Null),
            Expression::ColumnReference(c) => TypeInferenceEngine::column_nullable(&c.name, schema),
            Expression::UnresolvedColumn(_) => true,
            Expression::Binary(b) => b.left.nullable(schema) || b.right.nullable(schema),
            Expression::Unary(u) => match u.op {
                UnaryOp::IsNull | UnaryOp::IsNotNull | UnaryOp::IsNaN | UnaryOp::IsNotNaN => false,
                _ => u.operand.nullable(schema),
            },
            Expression::FunctionCall(f) => {
                match f.name.to_lowercase().as_str() {
                    "count" | "count_distinct" => false,
                    "coalesce" => f.args.iter().all(|a| a.nullable(schema)),
                    _ => f.args.iter().any(|a| a.nullable(schema)),
                }
            }
            Expression::Cast(_) => true,
            Expression::CaseWhen(_) => true,
            Expression::Window(w) => match w.func.as_ref() {
                Expression::FunctionCall(f) => !TypeInferenceEngine::window_is_non_nullable(&f.name),
                _ => true,
            },
            Expression::Alias(a) => a.expr.nullable(schema),
            Expression::Star(_) => false,
            Expression::InSubquery(_) | Expression::ExistsSubquery(_) | Expression::InList(_) => false,
            Expression::ScalarSubquery(_) => true,
            Expression::Lambda(_) | Expression::LambdaVariable(_) => true,
            Expression::RawSql(_) => true,
            Expression::ArrayLiteral(_) | Expression::MapLiteral(_) | Expression::StructLiteral(_) => false,
            Expression::Between(_) => false,
            // New variants
            Expression::Like(l) => l.value.nullable(schema) || l.pattern.nullable(schema),
            Expression::IsDistinctFrom(_) => false,
            Expression::Interval(_) => false,
            Expression::ExtractValue(_) => true,
            Expression::RowConstructor(_) => false,
        }
    }
}

// ── Small helper trait ────────────────────────────────────────────────────────

trait PipeIfUnresolved {
    fn pipe_if_unresolved(self, f: impl FnOnce() -> Self) -> Self;
}

impl PipeIfUnresolved for DataType {
    fn pipe_if_unresolved(self, f: impl FnOnce() -> Self) -> Self {
        if self == DataType::Unresolved { f() } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::StructField;

    fn schema() -> StructType {
        StructType::new(vec![
            StructField::nullable("id", DataType::Long),
            StructField::not_null("price", DataType::Decimal { precision: 10, scale: 2 }),
        ])
    }

    #[test]
    fn literal_types() {
        assert_eq!(Literal::null().data_type(&StructType::empty()), DataType::Null);
        assert_eq!(Literal::int(42).data_type(&StructType::empty()), DataType::Integer);
        assert_eq!(Literal::string("hi").data_type(&StructType::empty()), DataType::String);
    }

    #[test]
    fn column_reference_lookup() {
        let s = schema();
        let col = ColumnReference::untyped("id");
        assert_eq!(col.data_type(&s), DataType::Long);
    }

    #[test]
    fn binary_comparison_is_boolean() {
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Gt,
            left: Box::new(ColumnReference::untyped("id")),
            right: Box::new(Literal::long(10)),
        });
        assert_eq!(expr.data_type(&schema()), DataType::Boolean);
    }

    #[test]
    fn cast_returns_target_type() {
        let expr = Expression::Cast(CastExpression {
            expr: Box::new(ColumnReference::untyped("id")),
            to_type: DataType::String,
            try_cast: false,
        });
        assert_eq!(expr.data_type(&StructType::empty()), DataType::String);
    }

    #[test]
    fn like_data_type_is_boolean() {
        let expr = Expression::Like(LikeExpression {
            value: Box::new(ColumnReference::untyped("name")),
            pattern: Box::new(Literal::string("%smith%")),
            negated: false,
            case_insensitive: false,
        });
        assert_eq!(expr.data_type(&StructType::empty()), DataType::Boolean);
    }

    #[test]
    fn interval_data_type_is_string() {
        let expr = Expression::Interval(IntervalExpression {
            months: 1,
            days: 0,
            microseconds: 0,
        });
        assert_eq!(expr.data_type(&StructType::empty()), DataType::String);
    }

    #[test]
    fn is_distinct_from_data_type_and_nullability() {
        let expr = Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(ColumnReference::untyped("a")),
            right: Box::new(Literal::null()),
            negated: false,
        });
        assert_eq!(expr.data_type(&StructType::empty()), DataType::Boolean);
        assert!(!expr.nullable(&StructType::empty()));
    }
}
