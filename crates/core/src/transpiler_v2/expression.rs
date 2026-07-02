//! τ's Expression enum — Spark 4.1.1 parity.
//!
//! **INV10:** this file imports ONLY from `crate::types` (`DataType`,
//! `StructField`, `StructType`) plus intra-τ modules. No `crate::expression`,
//! `crate::logical`, `crate::generator`, `crate::functions`, or
//! `crate::types::TypeInferenceEngine`.

use super::ast::CommonAst;
use super::type_inference::TypeInferenceEngine;
use crate::types::{DataType, StructField, StructType};

// ── Supporting sub-types ─────────────────────────────────────────────────────

/// Binary arithmetic / comparison / logical / string / bitwise operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    IntDiv,
    // Comparison
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    // Logical
    And,
    Or,
    // String
    Concat,
    // Bitwise
    BitAnd,
    BitOr,
    BitXor,
}

impl BinaryOp {
    /// Whether this operator returns a Boolean regardless of operand types.
    pub fn is_boolean_result(&self) -> bool {
        matches!(
            self,
            BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq
                | BinaryOp::And
                | BinaryOp::Or
        )
    }

    /// Whether this operator is arithmetic (subject to numeric promotion).
    pub fn is_arithmetic(&self) -> bool {
        matches!(
            self,
            BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Mod
                | BinaryOp::IntDiv
        )
    }

    /// Whether this operator is bitwise (integer-only, same type as operands).
    pub fn is_bitwise(&self) -> bool {
        matches!(self, BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor)
    }
}

/// Unary operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Not,
    Negate,
    IsNull,
    IsNotNull,
    IsNaN,
    IsNotNaN,
}

/// Sort direction — ASC / DESC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// NULL ordering within a sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NullOrdering {
    NullsFirst,
    NullsLast,
}

/// A single sort key — expression + direction + null ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct SortOrder {
    pub expr: Box<Expression>,
    pub direction: SortDirection,
    pub null_ordering: NullOrdering,
}

/// Window frame unit — ROWS vs RANGE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub lower: FrameBoundary,
    pub upper: FrameBoundary,
}

/// A typed literal value.
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
    /// Decimal literal — carried as string to preserve precision.
    Decimal {
        value: String,
        precision: u8,
        scale: u8,
    },
    String(String),
    /// Days since Unix epoch.
    Date(i32),
    /// Microseconds since Unix epoch (with timezone).
    Timestamp(i64),
    /// Microseconds since Unix epoch (no timezone).
    TimestampNtz(i64),
    Binary(Vec<u8>),
}

// LiteralValue contains f32/f64, so PartialOrd / Hash / Eq are not derivable.
// We do not need them for Slice A.1; the derives above are minimal.

/// A typed literal expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Literal {
    pub value: LiteralValue,
    pub data_type: DataType,
}

/// A resolved column reference with schema-recorded type/nullability info.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnReference {
    pub name: String,
    pub qualifier: Option<String>,
    pub data_type: Option<DataType>,
    pub nullable: Option<bool>,
}

/// An unresolved (pre-analysis) column reference.
///
/// `plan_id` is first-class per §2.3 — it identifies the proto DataFrame /
/// plan node the reference belongs to, replacing the legacy path's
/// string-encoded `__plan_id_N__` qualifier. Slice B's analyzer uses this
/// field as a resolution hint on join-side disambiguation. SparkSQL entries
/// set `plan_id = None` (Open Decision 12).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnresolvedColumn {
    pub name: String,
    pub qualifier: Option<String>,
    pub plan_id: Option<i64>,
}

/// Binary expression: left OP right.
#[derive(Debug, Clone, PartialEq)]
pub struct BinaryExpression {
    pub op: BinaryOp,
    pub left: Box<Expression>,
    pub right: Box<Expression>,
}

/// Unary expression: OP operand.
#[derive(Debug, Clone, PartialEq)]
pub struct UnaryExpression {
    pub op: UnaryOp,
    pub operand: Box<Expression>,
}

/// Function call.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub args: Vec<Expression>,
    pub distinct: bool,
}

/// Cast / TRY_CAST expression.
#[derive(Debug, Clone, PartialEq)]
pub struct CastExpression {
    pub expr: Box<Expression>,
    pub to_type: DataType,
    /// `true` = TRY_CAST (nullable), `false` = CAST.
    pub try_cast: bool,
}

/// CASE WHEN expression.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseWhenExpression {
    pub branches: Vec<(Expression, Expression)>,
    pub else_expr: Option<Box<Expression>>,
}

/// A window function call.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowFunction {
    pub func: Box<Expression>,
    pub partition_by: Vec<Expression>,
    pub order_by: Vec<SortOrder>,
    pub frame: Option<WindowFrame>,
}

/// Aliased expression: expr AS alias.
#[derive(Debug, Clone, PartialEq)]
pub struct AliasExpression {
    pub expr: Box<Expression>,
    pub alias: String,
}

/// Star expression (`*` or `alias.*`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StarExpression {
    pub qualifier: Option<String>,
}

/// `expr IN (subquery)`.
#[derive(Debug, Clone, PartialEq)]
pub struct InSubquery {
    pub expr: Box<Expression>,
    pub subquery: Box<CommonAst>,
    pub negated: bool,
}

/// `EXISTS (subquery)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistsSubquery {
    pub subquery: Box<CommonAst>,
    pub negated: bool,
}

/// `(scalar subquery)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScalarSubquery {
    pub subquery: Box<CommonAst>,
}

/// Lambda expression `(x, y) -> body`.
#[derive(Debug, Clone, PartialEq)]
pub struct LambdaExpression {
    pub params: Vec<String>,
    pub body: Box<Expression>,
}

/// Lambda variable — reference to a lambda parameter within the body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LambdaVariableExpression {
    pub name: String,
}

/// Raw SQL passthrough (from `spark.expr(...)`).
#[derive(Debug, Clone, PartialEq)]
pub struct RawSqlExpression {
    pub sql: String,
    pub data_type: Option<DataType>,
    pub nullable: Option<bool>,
}

/// Array literal `array(a, b, c)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayLiteralExpression {
    pub elements: Vec<Expression>,
    /// Element type — required so an empty literal has a resolvable type.
    pub element_type: DataType,
}

/// Map literal `map(k1, v1, k2, v2, ...)`.
#[derive(Debug, Clone, PartialEq)]
pub struct MapLiteralExpression {
    pub entries: Vec<(Expression, Expression)>,
    pub key_type: DataType,
    pub value_type: DataType,
}

/// Struct literal `struct(a AS x, b AS y)`.
#[derive(Debug, Clone, PartialEq)]
pub struct StructLiteralExpression {
    pub fields: Vec<(String, Expression)>,
}

/// `expr BETWEEN low AND high`.
#[derive(Debug, Clone, PartialEq)]
pub struct BetweenExpression {
    pub expr: Box<Expression>,
    pub low: Box<Expression>,
    pub high: Box<Expression>,
    pub negated: bool,
}

/// `expr IN (v1, v2, ...)`.
#[derive(Debug, Clone, PartialEq)]
pub struct InListExpression {
    pub expr: Box<Expression>,
    pub list: Vec<Expression>,
    pub negated: bool,
}

/// `value LIKE pattern`.
#[derive(Debug, Clone, PartialEq)]
pub struct LikeExpression {
    pub value: Box<Expression>,
    pub pattern: Box<Expression>,
    pub escape: Option<char>,
    pub negated: bool,
    pub case_insensitive: bool,
}

/// Interval literal (year-month or day-time, or generic Interval).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IntervalExpression {
    pub months: i32,
    pub days: i32,
    pub microseconds: i64,
}

/// `a IS [NOT] DISTINCT FROM b`.
#[derive(Debug, Clone, PartialEq)]
pub struct IsDistinctFromExpression {
    pub left: Box<Expression>,
    pub right: Box<Expression>,
    pub negated: bool,
}

/// `child.field` / `child[idx]` / `child[key]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractValueExpression {
    pub child: Box<Expression>,
    pub extraction: Box<Expression>,
}

/// Row constructor `(a, b, c)` → Struct.
#[derive(Debug, Clone, PartialEq)]
pub struct RowConstructorExpression {
    pub elements: Vec<Expression>,
    pub field_names: Vec<String>,
}

/// `WITH_FIELD` / update-fields on a struct.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateFieldsExpression {
    pub struct_expr: Box<Expression>,
    pub updates: Vec<(String, Expression)>,
}

// ── Expression enum (28 variants — Spark 4.1.1 parity) ───────────────────────

/// τ's canonical expression tree.
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

impl Expression {
    /// The Spark-compatible data type of this expression given the input schema.
    pub fn data_type(&self, schema: &StructType) -> DataType {
        match self {
            Expression::Literal(l) => l.data_type.clone(),
            Expression::ColumnReference(c) => match &c.data_type {
                Some(dt) => dt.clone(),
                None => TypeInferenceEngine::column_type(&c.name, schema),
            },
            Expression::UnresolvedColumn(u) => {
                TypeInferenceEngine::qualified_column_type(&u.name, u.qualifier.as_deref(), schema)
            }
            Expression::Binary(b) => Self::binary_data_type(b, schema),
            Expression::Unary(u) => match u.op {
                UnaryOp::Not
                | UnaryOp::IsNull
                | UnaryOp::IsNotNull
                | UnaryOp::IsNaN
                | UnaryOp::IsNotNaN => DataType::Boolean,
                UnaryOp::Negate => u.operand.data_type(schema),
            },
            Expression::FunctionCall(f) => Self::function_call_data_type(f, schema),
            Expression::Cast(c) => c.to_type.clone(),
            Expression::CaseWhen(cw) => Self::case_when_data_type(cw, schema),
            Expression::Window(w) => Self::window_data_type(w, schema),
            Expression::Alias(a) => a.expr.data_type(schema),
            Expression::Star(_) => DataType::Unresolved,
            Expression::InSubquery(_) | Expression::ExistsSubquery(_) => DataType::Boolean,
            Expression::ScalarSubquery(_) => DataType::Unresolved,
            Expression::Lambda(l) => l.body.data_type(schema),
            Expression::LambdaVariable(lv) => TypeInferenceEngine::column_type(&lv.name, schema),
            Expression::RawSql(r) => r.data_type.clone().unwrap_or(DataType::Unresolved),
            Expression::ArrayLiteral(a) => {
                let contains_null = a
                    .elements
                    .iter()
                    .any(|e| matches!(e, Expression::Literal(l) if matches!(l.value, LiteralValue::Null)));
                DataType::Array(Box::new(a.element_type.clone()), contains_null)
            }
            Expression::MapLiteral(m) => DataType::Map {
                key: Box::new(m.key_type.clone()),
                value: Box::new(m.value_type.clone()),
                value_nullable: true,
            },
            Expression::StructLiteral(s) => {
                let fields: Vec<StructField> = s
                    .fields
                    .iter()
                    .map(|(name, expr)| {
                        StructField::new(
                            name.clone(),
                            expr.data_type(schema),
                            expr.nullable(schema),
                        )
                    })
                    .collect();
                DataType::Struct(StructType::new(fields))
            }
            Expression::Between(_)
            | Expression::InList(_)
            | Expression::Like(_)
            | Expression::IsDistinctFrom(_) => DataType::Boolean,
            Expression::Interval(_) => DataType::Interval,
            Expression::ExtractValue(ev) => Self::extract_value_data_type(ev, schema),
            Expression::RowConstructor(rc) => {
                let fields: Vec<StructField> = rc
                    .elements
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let name = rc
                            .field_names
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| format!("col{}", i + 1));
                        StructField::new(name, e.data_type(schema), e.nullable(schema))
                    })
                    .collect();
                DataType::Struct(StructType::new(fields))
            }
            Expression::UpdateFields(u) => Self::update_fields_data_type(u, schema),
        }
    }

    /// Whether this expression can produce NULL values.
    pub fn nullable(&self, schema: &StructType) -> bool {
        match self {
            Expression::Literal(l) => matches!(l.value, LiteralValue::Null),
            Expression::ColumnReference(c) => c
                .nullable
                .unwrap_or_else(|| TypeInferenceEngine::column_nullable(&c.name, schema)),
            Expression::UnresolvedColumn(u) => TypeInferenceEngine::qualified_column_nullable(
                &u.name,
                u.qualifier.as_deref(),
                schema,
            ),
            Expression::Binary(b) => b.left.nullable(schema) || b.right.nullable(schema),
            Expression::Unary(u) => match u.op {
                UnaryOp::IsNull | UnaryOp::IsNotNull | UnaryOp::IsNaN | UnaryOp::IsNotNaN => false,
                _ => u.operand.nullable(schema),
            },
            Expression::FunctionCall(f) => Self::function_call_nullable(f, schema),
            Expression::Cast(c) => {
                if c.try_cast {
                    true
                } else {
                    c.expr.nullable(schema)
                }
            }
            Expression::CaseWhen(cw) => {
                cw.else_expr.is_none()
                    || cw.else_expr.as_ref().is_some_and(|e| e.nullable(schema))
                    || cw.branches.iter().any(|(_, then)| then.nullable(schema))
            }
            Expression::Window(w) => Self::window_nullable(w, schema),
            Expression::Alias(a) => a.expr.nullable(schema),
            Expression::Star(_) => false,
            Expression::InSubquery(_) | Expression::ExistsSubquery(_) => false,
            Expression::ScalarSubquery(_) => true,
            Expression::Lambda(_) => false,
            Expression::LambdaVariable(lv) => {
                TypeInferenceEngine::column_nullable(&lv.name, schema)
            }
            Expression::RawSql(r) => r.nullable.unwrap_or(true),
            Expression::ArrayLiteral(_)
            | Expression::MapLiteral(_)
            | Expression::StructLiteral(_) => false,
            Expression::Between(b) => {
                b.expr.nullable(schema) || b.low.nullable(schema) || b.high.nullable(schema)
            }
            Expression::InList(i) => {
                i.expr.nullable(schema) || i.list.iter().any(|e| e.nullable(schema))
            }
            Expression::Like(l) => l.value.nullable(schema) || l.pattern.nullable(schema),
            Expression::Interval(_) => false,
            Expression::IsDistinctFrom(_) => false,
            Expression::ExtractValue(ev) => Self::extract_value_nullable(ev, schema),
            Expression::RowConstructor(rc) => rc.elements.iter().any(|e| e.nullable(schema)),
            Expression::UpdateFields(u) => {
                u.struct_expr.nullable(schema) || u.updates.iter().any(|(_, e)| e.nullable(schema))
            }
        }
    }

    // ── Binary data-type derivation ──────────────────────────────────────────

    fn binary_data_type(b: &BinaryExpression, schema: &StructType) -> DataType {
        if b.op.is_boolean_result() {
            return DataType::Boolean;
        }
        if matches!(b.op, BinaryOp::Concat) {
            return DataType::String;
        }
        let l = b.left.data_type(schema);
        let r = b.right.data_type(schema);
        if b.op.is_bitwise() {
            return TypeInferenceEngine::promote_numeric(&l, &r);
        }
        // Interval ± Date/Timestamp → preserve the date-like side.
        if b.op == BinaryOp::Add || b.op == BinaryOp::Sub {
            match (&l, &r) {
                (DataType::Date, dt) | (dt, DataType::Date) if dt.is_interval() => {
                    return DataType::Date
                }
                (DataType::Timestamp, dt) | (dt, DataType::Timestamp) if dt.is_interval() => {
                    return DataType::Timestamp
                }
                (DataType::TimestampNtz, dt) | (dt, DataType::TimestampNtz) if dt.is_interval() => {
                    return DataType::TimestampNtz
                }
                _ => {}
            }
        }
        // Decimal-aware arithmetic.
        if let (
            DataType::Decimal {
                precision: p1,
                scale: s1,
            },
            DataType::Decimal {
                precision: p2,
                scale: s2,
            },
        ) = (&l, &r)
        {
            return match b.op {
                BinaryOp::Add | BinaryOp::Sub => {
                    TypeInferenceEngine::decimal_add_type(*p1, *s1, *p2, *s2)
                }
                BinaryOp::Mul => TypeInferenceEngine::decimal_mul_type(*p1, *s1, *p2, *s2),
                BinaryOp::Div => TypeInferenceEngine::decimal_div_type(*p1, *s1, *p2, *s2),
                BinaryOp::Mod => TypeInferenceEngine::decimal_mod_type(*p1, *s1, *p2, *s2),
                _ => TypeInferenceEngine::promote_numeric(&l, &r),
            };
        }
        if b.op == BinaryOp::Div {
            // Spark int/int → Double.
            if l.is_integral() && r.is_integral() {
                return DataType::Double;
            }
        }
        if b.op == BinaryOp::IntDiv {
            return DataType::Long;
        }
        TypeInferenceEngine::promote_numeric(&l, &r)
    }

    // ── FunctionCall data-type derivation ────────────────────────────────────

    fn function_call_data_type(f: &FunctionCall, schema: &StructType) -> DataType {
        let first_arg_type = f.args.first().map(|a| a.data_type(schema));
        TypeInferenceEngine::function_return_type(&f.name, first_arg_type.as_ref())
    }

    // ── FunctionCall nullability ─────────────────────────────────────────────

    /// Names in this list report `nullable = false` regardless of arg nullability.
    ///
    /// Case-insensitive wrapper retained as the defensive surface for external
    /// callers that have not yet normalized their input. Prefer
    /// [`Self::is_non_nullable_function_name_lower`] on hot paths where the
    /// caller has already lowercased the name.
    ///
    /// Contains the count family (checklist §1.1) and the hash family
    /// (checklist §1.2). Extending this list requires adding to the
    /// symmetric-omission tests (§8) as well.
    #[allow(dead_code)] // defensive API; internal τ callers use `_lower` variant.
    pub(crate) fn is_non_nullable_function_name(name: &str) -> bool {
        Self::is_non_nullable_function_name_lower(&name.to_lowercase())
    }

    /// Fast-path sibling of [`Self::is_non_nullable_function_name`].
    ///
    /// **Precondition:** `name_lower` MUST already be lowercase. Debug builds
    /// `debug_assert!` this; release builds trust the contract to avoid an
    /// unnecessary allocation.
    pub(crate) fn is_non_nullable_function_name_lower(name_lower: &str) -> bool {
        debug_assert!(
            name_lower.chars().all(|c| !c.is_ascii_uppercase()),
            "is_non_nullable_function_name_lower requires pre-lowercased input; got `{name_lower}`",
        );
        matches!(
            name_lower,
            "count"
                | "count_distinct"
                | "count_if"
                | "grouping"
                | "grouping_id"
                | "hash"
                | "murmur3"
                | "xxhash64"
        )
    }

    fn function_call_nullable(f: &FunctionCall, schema: &StructType) -> bool {
        let lower = f.name.to_lowercase();
        if Self::is_non_nullable_function_name_lower(&lower) {
            return false;
        }
        if TypeInferenceEngine::aggregate_is_always_nullable_lower(&lower) {
            return true;
        }
        match lower.as_str() {
            "coalesce" | "ifnull" | "nvl" | "iif" => f.args.iter().all(|a| a.nullable(schema)),
            "when" => {
                if f.args.len() % 2 == 0 {
                    true
                } else {
                    let then_nullable =
                        f.args.iter().skip(1).step_by(2).any(|a| a.nullable(schema));
                    let else_nullable = f.args.last().is_some_and(|a| a.nullable(schema));
                    then_nullable || else_nullable
                }
            }
            "isnull" | "isnan" | "isnotnull" | "isnotnan" | "is_nan" | "isinf" => false,
            "concat_ws" => false,
            "greatest" | "least" => f.args.iter().all(|a| a.nullable(schema)),
            "nvl2" => {
                f.args.get(1).is_none_or(|a| a.nullable(schema))
                    || f.args.get(2).is_none_or(|a| a.nullable(schema))
            }
            "array" | "make_array" | "create_map" | "map" | "named_struct" | "struct"
            | "map_from_entries" => false,
            _ => f.args.iter().any(|a| a.nullable(schema)),
        }
    }

    // ── CaseWhen data-type unification ───────────────────────────────────────

    fn case_when_data_type(cw: &CaseWhenExpression, schema: &StructType) -> DataType {
        let mut types_iter = cw
            .branches
            .iter()
            .map(|(_, then)| then.data_type(schema))
            .chain(cw.else_expr.as_ref().map(|e| e.data_type(schema)))
            .filter(|dt| !matches!(dt, DataType::Null | DataType::Unresolved));
        let first = match types_iter.next() {
            Some(dt) => dt,
            None => return DataType::Unresolved,
        };
        types_iter.fold(first, |acc, dt| {
            // Identity short-circuit — skip the `unify_types` clone when the
            // accumulator already matches the next branch's type. Common when
            // every branch of a CASE has the same result type.
            if acc == dt {
                acc
            } else {
                TypeInferenceEngine::unify_types(&acc, &dt)
            }
        })
    }

    // ── Window data-type derivation ──────────────────────────────────────────

    fn window_data_type(w: &WindowFunction, schema: &StructType) -> DataType {
        match w.func.as_ref() {
            Expression::FunctionCall(f) => {
                let first_arg_type = f.args.first().map(|a| a.data_type(schema));
                TypeInferenceEngine::window_return_type(&f.name, first_arg_type.as_ref())
            }
            other => other.data_type(schema),
        }
    }

    fn window_nullable(w: &WindowFunction, schema: &StructType) -> bool {
        match w.func.as_ref() {
            Expression::FunctionCall(f) => {
                if TypeInferenceEngine::window_is_non_nullable(&f.name) {
                    false
                } else if matches!(f.name.to_lowercase().as_str(), "lag" | "lead") {
                    // If 3rd arg (default) is present and non-nullable, the result is non-nullable.
                    f.args.get(2).is_none_or(|default| default.nullable(schema))
                } else {
                    true
                }
            }
            _ => true,
        }
    }

    // ── ExtractValue derivations ─────────────────────────────────────────────

    fn extract_value_data_type(ev: &ExtractValueExpression, schema: &StructType) -> DataType {
        let base_type = ev.child.data_type(schema);
        let field_name = match ev.extraction.as_ref() {
            Expression::Literal(Literal {
                value: LiteralValue::String(s),
                ..
            }) => Some(s.as_str()),
            _ => None,
        };
        match (&base_type, field_name) {
            (DataType::Struct(st), Some(name)) => st
                .field_by_name(name)
                .map(|f| f.data_type.clone())
                .unwrap_or(DataType::Unresolved),
            (DataType::Array(elem, _), _) => (**elem).clone(),
            (DataType::Map { value, .. }, _) => (**value).clone(),
            _ => DataType::Unresolved,
        }
    }

    fn extract_value_nullable(ev: &ExtractValueExpression, schema: &StructType) -> bool {
        let base_nullable = ev.child.nullable(schema);
        let base_type = ev.child.data_type(schema);
        let field_name = match ev.extraction.as_ref() {
            Expression::Literal(Literal {
                value: LiteralValue::String(s),
                ..
            }) => Some(s.as_str()),
            _ => None,
        };
        match (&base_type, field_name) {
            (DataType::Struct(st), Some(name)) => {
                let field_nullable = st.field_by_name(name).map(|f| f.nullable).unwrap_or(true);
                base_nullable || field_nullable
            }
            _ => true,
        }
    }

    // ── UpdateFields derivation ──────────────────────────────────────────────

    fn update_fields_data_type(u: &UpdateFieldsExpression, schema: &StructType) -> DataType {
        let base = u.struct_expr.data_type(schema);
        let DataType::Struct(mut st) = base else {
            return DataType::Unresolved;
        };
        for (field_name, new_val) in &u.updates {
            let new_type = new_val.data_type(schema);
            let new_nullable = new_val.nullable(schema);
            if let Some(idx) = st.field_index(field_name) {
                // In-place update: overwrite type/nullability, keep the
                // existing `name` allocation to avoid re-cloning.
                st.fields[idx].data_type = new_type;
                st.fields[idx].nullable = new_nullable;
            } else {
                st.fields
                    .push(StructField::new(field_name.clone(), new_type, new_nullable));
            }
        }
        DataType::Struct(st)
    }
}

// ── Convenience constructors used by tests ───────────────────────────────────

impl ColumnReference {
    /// Construct an unresolved column reference (no type or nullability hint).
    pub fn untyped(name: impl Into<String>) -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: name.into(),
            qualifier: None,
            data_type: None,
            nullable: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::type_inference::{AGGREGATE_NAMES, CORR_FAMILY_NAMES, HASH_FAMILY_NAMES};
    use super::*;

    // ── Checklist §1.1 — `count_if` FunctionCall nullability ────────────────

    #[test]
    fn count_if_function_call_is_non_nullable() {
        let s = StructType::new(vec![StructField::nullable("active", DataType::Boolean)]);
        let expr = Expression::FunctionCall(FunctionCall {
            name: "count_if".to_owned(),
            args: vec![ColumnReference::untyped("active")],
            distinct: false,
        });
        assert!(!expr.nullable(&s));
    }

    /// Sanity anchor — `count` over a nullable column must still be non-null.
    #[test]
    fn count_of_nullable_column_is_non_nullable() {
        let s = StructType::new(vec![StructField::nullable("id", DataType::Long)]);
        let expr = Expression::FunctionCall(FunctionCall {
            name: "count".to_owned(),
            args: vec![ColumnReference::untyped("id")],
            distinct: false,
        });
        assert!(!expr.nullable(&s));
    }

    // ── Checklist §1.2 — hash family FunctionCall nullability ──────────────

    #[test]
    fn hash_and_xxhash64_are_non_nullable_regardless_of_args() {
        let s = StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ]);
        // Sanity: the args ARE nullable — proves the fix (not a default arm)
        // is responsible for the non-null result.
        assert!(ColumnReference::untyped("name").nullable(&s));
        assert!(ColumnReference::untyped("salary").nullable(&s));

        for name in HASH_FAMILY_NAMES {
            let single = Expression::FunctionCall(FunctionCall {
                name: (*name).to_owned(),
                args: vec![ColumnReference::untyped("name")],
                distinct: false,
            });
            assert!(
                !single.nullable(&s),
                "{name}(nullable_col) must report nullable=false",
            );

            let multi = Expression::FunctionCall(FunctionCall {
                name: (*name).to_owned(),
                args: vec![
                    ColumnReference::untyped("name"),
                    ColumnReference::untyped("salary"),
                ],
                distinct: false,
            });
            assert!(
                !multi.nullable(&s),
                "{name}(nullable_col, nullable_col) must report nullable=false",
            );
        }
    }

    // ── Symmetric-omission mechanical checks (§8) ───────────────────────────

    /// §8.2 — every name where `aggregate_is_non_nullable` is `true` must
    /// produce a `FunctionCall` that reports `nullable == false`.
    #[test]
    fn function_call_nullable_lists_are_symmetric_with_aggregate_is_non_nullable() {
        let schema = StructType::new(vec![StructField::nullable("x", DataType::Long)]);
        for name in AGGREGATE_NAMES {
            if !TypeInferenceEngine::aggregate_is_non_nullable(name) {
                continue;
            }
            let expr = Expression::FunctionCall(FunctionCall {
                name: (*name).to_owned(),
                args: vec![ColumnReference::untyped("x")],
                distinct: false,
            });
            assert!(
                !expr.nullable(&schema),
                "aggregate `{name}` is aggregate_is_non_nullable but \
                 FunctionCall::nullable returned true",
            );
        }
    }

    /// §8.3 — the hash family must be in the FunctionCall non-nullable literal list.
    #[test]
    fn hash_family_is_in_function_call_nullable_literal_list() {
        let schema = StructType::new(vec![StructField::nullable("x", DataType::String)]);
        for name in HASH_FAMILY_NAMES {
            let expr = Expression::FunctionCall(FunctionCall {
                name: (*name).to_owned(),
                args: vec![ColumnReference::untyped("x")],
                distinct: false,
            });
            assert!(
                !expr.nullable(&schema),
                "hash family `{name}` must report nullable=false",
            );
        }
    }

    // ── Data-type derivations sanity ────────────────────────────────────────

    #[test]
    fn literal_data_type_and_nullability() {
        let s = StructType::empty();
        let lit_int = Expression::Literal(Literal {
            value: LiteralValue::Int(42),
            data_type: DataType::Integer,
        });
        assert_eq!(lit_int.data_type(&s), DataType::Integer);
        assert!(!lit_int.nullable(&s));

        let lit_null = Expression::Literal(Literal {
            value: LiteralValue::Null,
            data_type: DataType::Null,
        });
        assert!(lit_null.nullable(&s));
    }

    #[test]
    fn binary_eq_is_boolean() {
        let s = StructType::new(vec![
            StructField::not_null("a", DataType::Integer),
            StructField::not_null("b", DataType::Integer),
        ]);
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(ColumnReference::untyped("a")),
            right: Box::new(ColumnReference::untyped("b")),
        });
        assert_eq!(expr.data_type(&s), DataType::Boolean);
        assert!(!expr.nullable(&s));
    }

    #[test]
    fn cast_data_type_is_target_type() {
        let s = StructType::new(vec![StructField::not_null("x", DataType::Integer)]);
        let expr = Expression::Cast(CastExpression {
            expr: Box::new(ColumnReference::untyped("x")),
            to_type: DataType::Double,
            try_cast: false,
        });
        assert_eq!(expr.data_type(&s), DataType::Double);
        assert!(!expr.nullable(&s));
    }

    #[test]
    fn try_cast_is_nullable() {
        let s = StructType::new(vec![StructField::not_null("x", DataType::Integer)]);
        let expr = Expression::Cast(CastExpression {
            expr: Box::new(ColumnReference::untyped("x")),
            to_type: DataType::Double,
            try_cast: true,
        });
        assert!(expr.nullable(&s));
    }

    #[test]
    fn alias_propagates_inner() {
        let s = StructType::new(vec![StructField::not_null("x", DataType::Long)]);
        let expr = Expression::Alias(AliasExpression {
            expr: Box::new(ColumnReference::untyped("x")),
            alias: "y".to_owned(),
        });
        assert_eq!(expr.data_type(&s), DataType::Long);
        assert!(!expr.nullable(&s));
    }

    // ── §7 UnresolvedColumn.plan_id ──────────────────────────────────────

    #[test]
    fn unresolved_column_carries_plan_id_field() {
        let u = UnresolvedColumn {
            name: "id".to_owned(),
            qualifier: None,
            plan_id: Some(42),
        };
        assert_eq!(u.plan_id, Some(42));
    }

    #[test]
    fn unresolved_column_plan_id_none_default() {
        // Open Decision 12 anchor: SparkSQL front-end sets plan_id = None.
        let u = UnresolvedColumn {
            name: "id".to_owned(),
            qualifier: None,
            plan_id: None,
        };
        assert!(u.plan_id.is_none());
    }

    // ── §7 subquery variants carry CommonAst ─────────────────────────────

    #[test]
    fn in_subquery_carries_common_ast() {
        use super::super::ast::{CommonAst, CommonOp};
        let sub = CommonAst::new(CommonOp::SingleRow);
        let expr = Expression::InSubquery(InSubquery {
            expr: Box::new(ColumnReference::untyped("x")),
            subquery: Box::new(sub),
            negated: false,
        });
        // Compile-only sanity; ensures the field type is Box<CommonAst>.
        assert!(matches!(expr, Expression::InSubquery(_)));
    }

    #[test]
    fn exists_subquery_data_type_boolean() {
        use super::super::ast::{CommonAst, CommonOp};
        let s = StructType::empty();
        let expr = Expression::ExistsSubquery(ExistsSubquery {
            subquery: Box::new(CommonAst::new(CommonOp::SingleRow)),
            negated: false,
        });
        assert_eq!(expr.data_type(&s), DataType::Boolean);
        assert!(!expr.nullable(&s));
    }

    #[test]
    fn scalar_subquery_data_type_unresolved() {
        use super::super::ast::{CommonAst, CommonOp};
        let s = StructType::empty();
        let expr = Expression::ScalarSubquery(ScalarSubquery {
            subquery: Box::new(CommonAst::new(CommonOp::SingleRow)),
        });
        assert_eq!(expr.data_type(&s), DataType::Unresolved);
        assert!(expr.nullable(&s));
    }

    #[test]
    fn corr_family_functioncall_returns_double() {
        let s = StructType::new(vec![
            StructField::nullable("a", DataType::Integer),
            StructField::nullable("b", DataType::Integer),
        ]);
        for name in CORR_FAMILY_NAMES {
            let expr = Expression::FunctionCall(FunctionCall {
                name: (*name).to_owned(),
                args: vec![ColumnReference::untyped("a"), ColumnReference::untyped("b")],
                distinct: false,
            });
            assert_eq!(
                expr.data_type(&s),
                DataType::Double,
                "{name} FunctionCall must have data_type Double",
            );
            assert!(expr.nullable(&s), "{name} FunctionCall must be nullable",);
        }
    }
}
