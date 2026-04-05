use crate::types::{DataType, StructField, StructType, TypeInferenceEngine};

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
    UpdateFields(UpdateFieldsExpression),
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
    /// Optional type hint for schema inference (avoids DuckDB fallback).
    pub data_type: Option<DataType>,
    /// Optional nullable hint for schema inference.
    pub nullable: Option<bool>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateFieldsExpression {
    /// The struct-typed expression to modify.
    pub struct_expr: Box<Expression>,
    /// Name of the field to add/update/drop.
    pub field_name: String,
    /// Value to set. When `None`, the field is dropped.
    pub value: Option<Box<Expression>>,
    /// All field names of the struct (populated at plan-conversion time via schema inference).
    /// Required for the `dropFields` path (`value = None`) so the generator knows which
    /// fields to keep when rebuilding the struct with `struct_pack`.
    pub struct_fields: Option<Vec<String>>,
}

// ── Type inference on Expression ──────────────────────────────────────────────

impl Expression {
    /// Check if this expression is an untyped NULL literal (Spark semantics).
    /// Untyped NULLs (Null, String, or Unresolved type) don't participate in CaseWhen type unification.
    fn is_untyped_null(&self) -> bool {
        matches!(self, Expression::Literal(lit)
            if lit.value == LiteralValue::Null
            && matches!(lit.data_type, DataType::Null | DataType::Unresolved))
    }

    /// For an integer literal expression, return a Decimal type with value-based precision
    /// (Spark's `DecimalPrecision` catalyst rule). For non-literal or non-integral expressions,
    /// fall back to type-based `integral_to_decimal`.
    ///
    /// Spark narrows integer literals to the minimum decimal precision that fits the value
    /// when they participate in binary arithmetic with decimals. For example, literal `100`
    /// becomes `Decimal(3,0)` instead of `Decimal(10,0)`.
    pub fn integral_to_decimal_for_arithmetic(&self) -> DataType {
        if let Expression::Literal(lit) = self {
            match &lit.value {
                LiteralValue::Byte(v) => {
                    let p = TypeInferenceEngine::decimal_digits_for_value(*v as i64);
                    DataType::Decimal { precision: p, scale: 0 }
                }
                LiteralValue::Short(v) => {
                    let p = TypeInferenceEngine::decimal_digits_for_value(*v as i64);
                    DataType::Decimal { precision: p, scale: 0 }
                }
                LiteralValue::Int(v) => {
                    let p = TypeInferenceEngine::decimal_digits_for_value(*v as i64);
                    DataType::Decimal { precision: p, scale: 0 }
                }
                LiteralValue::Long(v) => {
                    let p = TypeInferenceEngine::decimal_digits_for_value(*v);
                    DataType::Decimal { precision: p, scale: 0 }
                }
                _ => TypeInferenceEngine::integral_to_decimal(&lit.data_type),
            }
        } else {
            let dt = match self {
                Expression::ColumnReference(c) => c.data_type.clone(),
                other => other.data_type(&StructType::empty()),
            };
            TypeInferenceEngine::integral_to_decimal(&dt)
        }
    }

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
                TypeInferenceEngine::qualified_column_type(&u.name, u.qualifier.as_deref(), schema)
            }
            Expression::Binary(b) => {
                use BinaryOp::*;
                match &b.op {
                    Eq | NotEq | Lt | LtEq | Gt | GtEq | And | Or => DataType::Boolean,
                    Concat => DataType::String,
                    Div => {
                        let lt = b.left.data_type(schema);
                        let rt = b.right.data_type(schema);
                        use DataType::*;
                        match (&lt, &rt) {
                            // Spark: integer / integer → Double (unlike most languages)
                            (Byte | Short | Integer | Long, Byte | Short | Integer | Long) => Double,
                            (Decimal { precision: p1, scale: s1 }, Decimal { precision: p2, scale: s2 }) => {
                                TypeInferenceEngine::decimal_div_type(*p1, *s1, *p2, *s2)
                            }
                            (Decimal { precision: p1, scale: s1 }, i) if i.is_integral() => {
                                // Spark DecimalPrecision: use value-based precision for literals
                                if let Decimal { precision: p2, scale: s2 } = b.right.integral_to_decimal_for_arithmetic() {
                                    TypeInferenceEngine::decimal_div_type(*p1, *s1, p2, s2)
                                } else { Double }
                            }
                            (i, Decimal { precision: p2, scale: s2 }) if i.is_integral() => {
                                if let Decimal { precision: p1, scale: s1 } = b.left.integral_to_decimal_for_arithmetic() {
                                    TypeInferenceEngine::decimal_div_type(p1, s1, *p2, *s2)
                                } else { Double }
                            }
                            _ => TypeInferenceEngine::promote_numeric(&lt, &rt),
                        }
                    }
                    Mul => {
                        let lt = b.left.data_type(schema);
                        let rt = b.right.data_type(schema);
                        match (&lt, &rt) {
                            (DataType::Decimal { precision: p1, scale: s1 }, DataType::Decimal { precision: p2, scale: s2 }) => {
                                TypeInferenceEngine::decimal_mul_type(*p1, *s1, *p2, *s2)
                            }
                            (DataType::Decimal { precision: p1, scale: s1 }, i) if i.is_integral() => {
                                // Spark DecimalPrecision: use value-based precision for literals
                                if let DataType::Decimal { precision: p2, scale: s2 } = b.right.integral_to_decimal_for_arithmetic() {
                                    TypeInferenceEngine::decimal_mul_type(*p1, *s1, p2, s2)
                                } else { TypeInferenceEngine::promote_numeric(&lt, &rt) }
                            }
                            (i, DataType::Decimal { precision: p2, scale: s2 }) if i.is_integral() => {
                                if let DataType::Decimal { precision: p1, scale: s1 } = b.left.integral_to_decimal_for_arithmetic() {
                                    TypeInferenceEngine::decimal_mul_type(p1, s1, *p2, *s2)
                                } else { TypeInferenceEngine::promote_numeric(&lt, &rt) }
                            }
                            _ => TypeInferenceEngine::promote_numeric(&lt, &rt),
                        }
                    }
                    Add | Sub => {
                        let lt = b.left.data_type(schema);
                        let rt = b.right.data_type(schema);
                        match (&lt, &rt) {
                            (DataType::Decimal { precision: p1, scale: s1 }, DataType::Decimal { precision: p2, scale: s2 }) => {
                                TypeInferenceEngine::decimal_add_type(*p1, *s1, *p2, *s2)
                            }
                            (DataType::Decimal { precision: p1, scale: s1 }, i) if i.is_integral() => {
                                // Spark DecimalPrecision: use value-based precision for literals
                                if let DataType::Decimal { precision: p2, scale: s2 } = b.right.integral_to_decimal_for_arithmetic() {
                                    TypeInferenceEngine::decimal_add_type(*p1, *s1, p2, s2)
                                } else { TypeInferenceEngine::promote_numeric(&lt, &rt) }
                            }
                            (i, DataType::Decimal { precision: p2, scale: s2 }) if i.is_integral() => {
                                if let DataType::Decimal { precision: p1, scale: s1 } = b.left.integral_to_decimal_for_arithmetic() {
                                    TypeInferenceEngine::decimal_add_type(p1, s1, *p2, *s2)
                                } else { TypeInferenceEngine::promote_numeric(&lt, &rt) }
                            }
                            _ => TypeInferenceEngine::promote_numeric(&lt, &rt),
                        }
                    }
                    Mod => {
                        let lt = b.left.data_type(schema);
                        let rt = b.right.data_type(schema);
                        match (&lt, &rt) {
                            (DataType::Decimal { precision: p1, scale: s1 },
                             DataType::Decimal { precision: p2, scale: s2 }) => {
                                TypeInferenceEngine::decimal_mod_type(*p1, *s1, *p2, *s2)
                            }
                            _ => TypeInferenceEngine::promote_numeric(&lt, &rt),
                        }
                    }
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
                // Struct function special handling: resolve field types and nullable flags
                if f.name.eq_ignore_ascii_case("named_struct") && f.args.len() >= 2 {
                    let mut fields = Vec::new();
                    let mut i = 0;
                    while i + 1 < f.args.len() {
                        let name = match &f.args[i] {
                            Expression::Literal(l) => match &l.value {
                                LiteralValue::String(s) => s.clone(),
                                _ => format!("col{}", i / 2),
                            },
                            _ => format!("col{}", i / 2),
                        };
                        let val = &f.args[i + 1];
                        fields.push(StructField::new(name, val.data_type(schema), val.nullable(schema)));
                        i += 2;
                    }
                    return DataType::Struct(StructType::new(fields));
                }
                if f.name.eq_ignore_ascii_case("struct") {
                    let fields: Vec<StructField> = f.args.iter().enumerate().map(|(i, arg)| {
                        let name = match arg {
                            Expression::Alias(a) => a.alias.clone(),
                            _ => format!("col{i}"),
                        };
                        StructField::new(name, arg.data_type(schema), arg.nullable(schema))
                    }).collect();
                    return DataType::Struct(StructType::new(fields));
                }

                let arg_types: Vec<DataType> =
                    f.args.iter().map(|a| a.data_type(schema)).collect();
                let dt = TypeInferenceEngine::function_return_type(&f.name, &arg_types);

                // HOF-specific return type resolution (needs schema + lambda access)
                let lower = f.name.to_lowercase();
                match lower.as_str() {
                    "transform" | "list_transform" => {
                        if let Some(arr_type) = f.args.first().map(|a| a.data_type(schema)) {
                            if let DataType::Array(elem, elem_nullable) = arr_type {
                                if let Some(Expression::Lambda(lambda)) = f.args.get(1) {
                                    let augmented =
                                        TypeInferenceEngine::augment_schema_with_lambda_params(
                                            schema,
                                            &lambda.params,
                                            &elem,
                                            elem_nullable,
                                        );
                                    let body_type = lambda.body.data_type(&augmented);
                                    let body_nullable = lambda.body.nullable(&augmented);
                                    return DataType::Array(
                                        Box::new(body_type),
                                        body_nullable,
                                    );
                                }
                            }
                        }
                        dt
                    }
                    "filter" | "list_filter" | "array_filter" => {
                        f.args.first().map(|a| a.data_type(schema)).unwrap_or(dt)
                    }
                    "aggregate" | "reduce" | "list_reduce" => {
                        let init_type = f.args.get(1).map(|a| a.data_type(schema)).unwrap_or(dt);
                        if let Some(Expression::Lambda(finish)) = f.args.get(3) {
                            let init_nullable = f.args.get(1).map_or(true, |a| a.nullable(schema));
                            let aug = TypeInferenceEngine::augment_schema_with_lambda_params(
                                schema, &finish.params, &init_type, init_nullable,
                            );
                            finish.body.data_type(&aug)
                        } else {
                            init_type
                        }
                    }
                    // ROUND/BROUND: Spark adjusts decimal precision based on target scale
                    "round" | "bround" => {
                        if let Some(DataType::Decimal { precision, scale }) = f.args.first().map(|a| a.data_type(schema)) {
                            // Extract target scale from second arg (default: 0)
                            let new_scale = f.args.get(1).and_then(|e| match e {
                                Expression::Literal(l) => match &l.value {
                                    LiteralValue::Int(v) => Some(*v as u8),
                                    LiteralValue::Long(v) => Some(*v as u8),
                                    LiteralValue::Short(v) => Some(*v as u8),
                                    LiteralValue::Byte(v) => Some(*v as u8),
                                    _ => None,
                                },
                                _ => None,
                            }).unwrap_or(0);
                            if new_scale >= scale {
                                // Scale not reduced → keep original type
                                DataType::Decimal { precision, scale }
                            } else {
                                // Spark formula: precision = min(38, p - s + newScale + 1)
                                let int_digits = precision as i16 - scale as i16;
                                let result_p = ((int_digits + new_scale as i16 + 1).min(38)).max(1) as u8;
                                DataType::Decimal { precision: result_p, scale: new_scale }
                            }
                        } else {
                            dt
                        }
                    }
                    _ => {
                        match dt {
                            // Array constructors: containsNull from arg nullability
                            DataType::Array(elem, _)
                                if matches!(lower.as_str(), "array" | "make_array") =>
                            {
                                let contains_null =
                                    f.args.iter().any(|a| a.nullable(schema));
                                DataType::Array(elem, contains_null)
                            }
                            // Map constructors: value_nullable from odd-indexed args
                            DataType::Map { key, value, .. }
                                if matches!(lower.as_str(), "map" | "create_map") =>
                            {
                                let val_nullable = f.args.iter().skip(1).step_by(2)
                                    .any(|a| a.nullable(schema));
                                DataType::Map { key, value, value_nullable: val_nullable }
                            }
                            other => other,
                        }
                    }
                }
            }
            Expression::Cast(c) => c.to_type.clone(),
            Expression::CaseWhen(cw) => {
                use crate::types::TypeInferenceEngine;
                let branch_exprs = cw.branches.iter().map(|(_, r)| r);
                let else_exprs = cw.else_expr.iter().map(|e| e.as_ref());
                // Skip untyped NULL literals per Spark semantics
                let typed: Vec<DataType> = branch_exprs.chain(else_exprs)
                    .filter(|e| !e.is_untyped_null())
                    .map(|e| e.data_type(schema))
                    .collect();
                if typed.is_empty() {
                    DataType::String // all branches are untyped NULL → String
                } else {
                    typed.into_iter()
                        .reduce(|acc, t| TypeInferenceEngine::unify_types(&acc, &t))
                        .unwrap_or(DataType::Unresolved)
                }
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
            Expression::LambdaVariable(lv) => {
                TypeInferenceEngine::column_type(&lv.name, schema)
            }
            Expression::RawSql(r) => r.data_type.clone().unwrap_or(DataType::Unresolved),
            Expression::ArrayLiteral(a) => {
                // containsNull=true only if any element is an explicit NULL literal.
                let contains_null = a.elements.iter().any(|e| {
                    matches!(e, Expression::Literal(l) if matches!(l.value, LiteralValue::Null))
                });
                DataType::Array(Box::new(a.element_type.clone()), contains_null)
            }
            Expression::MapLiteral(m) => DataType::Map {
                key: Box::new(m.key_type.clone()),
                value: Box::new(m.value_type.clone()),
                value_nullable: true,
            },
            Expression::StructLiteral(s) => {
                let fields = s.fields.iter().map(|(name, expr)| {
                    StructField::new(name.clone(), expr.data_type(schema), expr.nullable(schema))
                }).collect();
                DataType::Struct(StructType::new(fields))
            }
            Expression::Between(_) => DataType::Boolean,
            Expression::Like(_) | Expression::IsDistinctFrom(_) => DataType::Boolean,
            Expression::Interval(_) => DataType::String, // TODO: proper IntervalType
            Expression::ExtractValue(ev) => {
                let base_type = ev.child.data_type(schema);
                let field_name = match ev.extraction.as_ref() {
                    Expression::Literal(Literal { value: LiteralValue::String(s), .. }) => Some(s.as_str()),
                    _ => None,
                };
                match (&base_type, field_name) {
                    (DataType::Struct(st), Some(name)) => {
                        st.field_by_name(name)
                            .map(|f| f.data_type.clone())
                            .unwrap_or(DataType::Unresolved)
                    }
                    (DataType::Array(elem, _), _) => elem.as_ref().clone(),
                    (DataType::Map { value, .. }, _) => value.as_ref().clone(),
                    _ => DataType::Unresolved,
                }
            }
            Expression::RowConstructor(_) => DataType::Unresolved,
            Expression::UpdateFields(u) => {
                let base_type = u.struct_expr.data_type(schema);
                if let DataType::Struct(st) = &base_type {
                    let mut fields: Vec<StructField> = st.fields.clone();
                    if let Some(ref val_expr) = u.value {
                        // Add or update the field
                        let new_type = val_expr.data_type(schema);
                        let new_nullable = val_expr.nullable(schema);
                        if let Some(existing) = fields.iter_mut().find(|f| f.name.eq_ignore_ascii_case(&u.field_name)) {
                            existing.data_type = new_type;
                            existing.nullable = new_nullable;
                        } else {
                            fields.push(StructField::new(u.field_name.clone(), new_type, new_nullable));
                        }
                    } else {
                        // Drop the field
                        fields.retain(|f| !f.name.eq_ignore_ascii_case(&u.field_name));
                    }
                    DataType::Struct(StructType::new(fields))
                } else {
                    DataType::Unresolved
                }
            }
        }
    }

    /// Whether this expression can produce NULL values.
    pub fn nullable(&self, schema: &StructType) -> bool {
        match self {
            Expression::Literal(l) => matches!(l.value, LiteralValue::Null),
            Expression::ColumnReference(c) => TypeInferenceEngine::column_nullable(&c.name, schema),
            Expression::UnresolvedColumn(u) => TypeInferenceEngine::qualified_column_nullable(&u.name, u.qualifier.as_deref(), schema),
            Expression::Binary(b) => b.left.nullable(schema) || b.right.nullable(schema),
            Expression::Unary(u) => match u.op {
                UnaryOp::IsNull | UnaryOp::IsNotNull | UnaryOp::IsNaN | UnaryOp::IsNotNaN => false,
                _ => u.operand.nullable(schema),
            },
            Expression::FunctionCall(f) => {
                let lower = f.name.to_lowercase();
                if matches!(lower.as_str(), "count" | "count_distinct" | "grouping" | "grouping_id") {
                    false
                } else if TypeInferenceEngine::aggregate_is_always_nullable(&lower) {
                    true
                } else if matches!(lower.as_str(), "coalesce" | "ifnull" | "nvl" | "iif") {
                    f.args.iter().all(|a| a.nullable(schema))
                } else if lower.as_str() == "when" {
                    // args layout: [cond1, val1, cond2, val2, ..., maybe_else]
                    // - Even total args: no ELSE clause → always nullable (NULL when nothing matches)
                    // - Odd total args: last arg is the ELSE value
                    if f.args.len() % 2 == 0 {
                        // No ELSE clause — always nullable (NULL when nothing matches)
                        true
                    } else {
                        // THEN values at odd indices (1, 3, 5, ...)
                        let then_nullable = f.args.iter().skip(1).step_by(2)
                            .any(|a| a.nullable(schema));
                        // ELSE value is the last arg (even index since total is odd)
                        let else_nullable = f.args.last()
                            .map_or(false, |a| a.nullable(schema));
                        then_nullable || else_nullable
                    }
                } else if matches!(lower.as_str(), "transform" | "list_transform" | "filter" | "list_filter" | "array_filter") {
                    f.args.first().map_or(true, |a| a.nullable(schema))
                } else if matches!(lower.as_str(), "exists" | "forall" | "list_bool_or" | "list_bool_and") {
                    f.args.first().map_or(true, |a| a.nullable(schema))
                } else if matches!(lower.as_str(), "aggregate" | "reduce" | "list_reduce") {
                    true
                } else if matches!(lower.as_str(),
                    "isnull" | "isnan" | "isnotnull" | "isnotnan"
                    | "is_nan" | "isinf"
                ) {
                    false
                } else if matches!(lower.as_str(),
                    "ceil" | "ceiling" | "floor" | "round" | "bround"
                    | "ln" | "log" | "log10" | "log2" | "log1p"
                    | "exp" | "expm1" | "pow" | "power" | "sqrt" | "cbrt"
                    | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
                    | "sinh" | "cosh" | "tanh" | "degrees" | "radians"
                    | "signum" | "sign"
                    | "factorial" | "hex" | "unhex" | "bin"
                ) {
                    true
                } else if lower.as_str() == "concat_ws" {
                    false
                } else if matches!(lower.as_str(), "greatest" | "least") {
                    f.args.iter().all(|a| a.nullable(schema))
                } else if lower.as_str() == "nvl2" {
                    f.args.get(1).map_or(true, |a| a.nullable(schema))
                        || f.args.get(2).map_or(true, |a| a.nullable(schema))
                } else if matches!(lower.as_str(),
                    "array" | "make_array" | "create_map" | "map"
                    | "named_struct" | "struct"
                    | "map_from_entries"
                ) {
                    false
                } else if lower.as_str() == "map_from_arrays" {
                    // map_from_arrays is nullable when any input array is nullable
                    f.args.iter().any(|a| a.nullable(schema))
                } else {
                    f.args.iter().any(|a| a.nullable(schema))
                }
            }
            Expression::Cast(c) => c.expr.nullable(schema),
            Expression::CaseWhen(cw) => {
                cw.else_expr.is_none()
                    || cw.else_expr.as_ref().is_some_and(|e| e.nullable(schema))
                    || cw.branches.iter().any(|(_, then)| then.nullable(schema))
            }
            Expression::Window(w) => match w.func.as_ref() {
                Expression::FunctionCall(f) => {
                    if TypeInferenceEngine::window_is_non_nullable(&f.name) {
                        false
                    } else if matches!(f.name.to_lowercase().as_str(), "lag" | "lead") {
                        // NOT NULL when 3rd arg (default) is present and non-nullable
                        f.args.get(2).map_or(true, |default| default.nullable(schema))
                    } else {
                        true
                    }
                }
                _ => true,
            },
            Expression::Alias(a) => a.expr.nullable(schema),
            Expression::Star(_) => false,
            Expression::InSubquery(_) | Expression::ExistsSubquery(_) | Expression::InList(_) => false,
            Expression::ScalarSubquery(_) => true,
            Expression::Lambda(_) => false,
            Expression::LambdaVariable(lv) => {
                TypeInferenceEngine::column_nullable(&lv.name, schema)
            }
            Expression::RawSql(r) => r.nullable.unwrap_or(true),
            Expression::ArrayLiteral(_) | Expression::MapLiteral(_) | Expression::StructLiteral(_) => false,
            Expression::Between(_) => false,
            Expression::Like(l) => l.value.nullable(schema) || l.pattern.nullable(schema),
            Expression::IsDistinctFrom(_) => false,
            Expression::Interval(_) => false,
            Expression::ExtractValue(ev) => {
                let base_nullable = ev.child.nullable(schema);
                let base_type = ev.child.data_type(schema);
                let field_name = match ev.extraction.as_ref() {
                    Expression::Literal(Literal { value: LiteralValue::String(s), .. }) => Some(s.as_str()),
                    _ => None,
                };
                match (&base_type, field_name) {
                    (DataType::Struct(st), Some(name)) => {
                        let field_nullable = st.field_by_name(name)
                            .map(|f| f.nullable)
                            .unwrap_or(true);
                        base_nullable || field_nullable
                    }
                    _ => true, // array/map access or unresolved: always nullable
                }
            }
            Expression::RowConstructor(_) => false,
            Expression::UpdateFields(u) => u.struct_expr.nullable(schema),
        }
    }
}

use crate::types::data_type::PipeIfUnresolved;

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

    fn struct_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("person", DataType::Struct(StructType::new(vec![
                StructField::not_null("name", DataType::String),
                StructField::nullable("age", DataType::Integer),
            ]))),
            StructField::nullable("nullable_struct", DataType::Struct(StructType::new(vec![
                StructField::not_null("street", DataType::String),
            ]))),
        ])
    }

    #[test]
    fn extract_value_struct_field_type() {
        let s = struct_schema();
        let expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "person".to_owned(),
                qualifier: None,
            })),
            extraction: Box::new(Literal::string("name")),
        });
        assert_eq!(expr.data_type(&s), DataType::String);
    }

    #[test]
    fn extract_value_struct_field_nullable_both_not_null() {
        let s = struct_schema();
        // person is NOT NULL, person.name is NOT NULL => not nullable
        let expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "person".to_owned(),
                qualifier: None,
            })),
            extraction: Box::new(Literal::string("name")),
        });
        assert!(!expr.nullable(&s));
    }

    #[test]
    fn extract_value_struct_field_nullable_field_nullable() {
        let s = struct_schema();
        // person is NOT NULL, person.age is NULLABLE => nullable
        let expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "person".to_owned(),
                qualifier: None,
            })),
            extraction: Box::new(Literal::string("age")),
        });
        assert!(expr.nullable(&s));
    }

    #[test]
    fn extract_value_struct_nullable_base() {
        let s = struct_schema();
        // nullable_struct IS NULLABLE, street is NOT NULL => nullable
        let expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "nullable_struct".to_owned(),
                qualifier: None,
            })),
            extraction: Box::new(Literal::string("street")),
        });
        assert!(expr.nullable(&s));
    }

    #[test]
    fn unresolved_column_qualified_struct_field_type() {
        let s = struct_schema();
        let expr = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "name".to_owned(),
            qualifier: Some("person".to_owned()),
        });
        assert_eq!(expr.data_type(&s), DataType::String);
    }

    #[test]
    fn unresolved_column_qualified_struct_field_nullable() {
        let s = struct_schema();
        // person NOT NULL, name NOT NULL => not nullable
        let expr = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "name".to_owned(),
            qualifier: Some("person".to_owned()),
        });
        assert!(!expr.nullable(&s));
    }

    #[test]
    fn update_fields_add_field_type() {
        let s = struct_schema();
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "person".to_owned(),
                qualifier: None,
            })),
            field_name: "email".to_owned(),
            value: Some(Box::new(Literal::string("test@example.com"))),
            struct_fields: None,
        });
        if let DataType::Struct(st) = expr.data_type(&s) {
            assert_eq!(st.fields.len(), 3); // name, age, email
            assert_eq!(st.field_by_name("email").unwrap().data_type, DataType::String);
        } else {
            panic!("expected Struct type");
        }
    }

    #[test]
    fn update_fields_drop_field_type() {
        let s = struct_schema();
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                name: "person".to_owned(),
                qualifier: None,
            })),
            field_name: "age".to_owned(),
            value: None,
            struct_fields: None,
        });
        if let DataType::Struct(st) = expr.data_type(&s) {
            assert_eq!(st.fields.len(), 1); // only name remains
            assert!(st.field_by_name("age").is_none());
        } else {
            panic!("expected Struct type");
        }
    }
}
