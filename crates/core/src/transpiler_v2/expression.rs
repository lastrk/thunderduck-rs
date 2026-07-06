//! τ's Expression enum — Spark 4.1.1 parity.
//!
//! **INV10:** this file imports ONLY from `crate::types` (`DataType`,
//! `StructField`, `StructType`) plus intra-τ modules. No `crate::expression`,
//! `crate::logical`, `crate::generator`, `crate::functions`, or
//! `crate::types::TypeInferenceEngine`.

use super::analyzer::TypedAst;
use super::ast::CommonAst;
use super::type_inference::TypeInferenceEngine;
use crate::types::{DataType, StructField, StructType};

/// Extract a compile-time integer value from an integral [`Literal`] expression.
///
/// Returns `None` for any non-literal or non-integral expression. Used by the
/// multi-arg type-inference pre-pass and by emission to read a function's
/// literal scale argument (e.g. `ceil(x, 2)`).
pub(crate) fn int_literal_value(expr: &Expression) -> Option<i32> {
    match expr {
        Expression::Literal(l) => match &l.value {
            LiteralValue::Int(i) => Some(*i),
            LiteralValue::Long(i) => i32::try_from(*i).ok(),
            LiteralValue::Short(i) => Some(*i as i32),
            LiteralValue::Byte(i) => Some(*i as i32),
            _ => None,
        },
        _ => None,
    }
}

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
// We do not need them for τ; the derives above are minimal.

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
/// plan node the reference belongs to. τ's analyzer uses this field as a
/// resolution hint on join-side disambiguation. SparkSQL entries set
/// `plan_id = None` (Open Decision 12).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnresolvedColumn {
    pub name: String,
    pub qualifier: Option<String>,
    pub plan_id: Option<i64>,
}

/// Pattern-driven column expander (Spark `df.colRegex("`.*_id`")`).
///
/// Produced by the Spark Connect converter for `ExprType::UnresolvedRegex`.
/// The analyzer's `Project` pre-pass expands this variant into N
/// `UnresolvedColumn` references — one per input-schema field whose name
/// matches [`Self::pattern`], preserving schema order. This variant MUST NOT
/// reach emission; the defensive arm in `render_expr` returns
/// `UnsupportedExpression` if it does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnresolvedRegexExpression {
    /// The regex pattern, backticks already stripped by the converter.
    pub pattern: String,
    /// Optional Spark Connect plan_id — propagated to every synthesized
    /// [`UnresolvedColumn`] the analyzer produces.
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

/// The two states an embedded subquery's inner plan can be in.
///
/// The front-end (lowering / proto-converter) produces the un-analyzed
/// [`CommonAst`]; the analyzer (layer A) rewrites it into an analyzed
/// [`TypedAst`] so emission can render it node-local via `dispatch_op`
/// (ADR-007 A / INV2). Making the two states an enum keeps illegal states
/// unrepresentable — the field is never both, never neither.
#[derive(Debug, Clone, PartialEq)]
pub enum SubqueryPlan {
    /// Front-end output — not yet analyzed.
    Unanalyzed(Box<CommonAst>),
    /// Analyzer output — the typed inner plan, carried so emission renders it.
    Analyzed(Box<TypedAst>),
}

/// `expr IN (subquery)`.
#[derive(Debug, Clone, PartialEq)]
pub struct InSubquery {
    pub expr: Box<Expression>,
    pub subquery: SubqueryPlan,
    pub negated: bool,
}

/// `EXISTS (subquery)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistsSubquery {
    pub subquery: SubqueryPlan,
    pub negated: bool,
}

/// `(scalar subquery)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScalarSubquery {
    pub subquery: SubqueryPlan,
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

/// `withField` / `dropFields` on a struct.
///
/// Each entry in [`Self::updates`] is either an add/replace (`Some(expr)`) or a
/// drop (`None`). Consecutive Spark Connect `UpdateFields` proto nodes chain
/// via nested `struct_expr`s — the converter flattens them for emission but
/// they remain semantically equivalent to one op per node.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateFieldsExpression {
    pub struct_expr: Box<Expression>,
    /// Ordered list of ops: `(field_name, Some(new_value))` = add/replace,
    /// `(field_name, None)` = drop.
    pub updates: Vec<(String, Option<Expression>)>,
}

// ── Expression enum (28 variants — Spark 4.1.1 parity) ───────────────────────

/// τ's canonical expression tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    Literal(Literal),
    ColumnReference(ColumnReference),
    UnresolvedColumn(UnresolvedColumn),
    UnresolvedRegex(UnresolvedRegexExpression),
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
            // Analyzer's Project pre-pass expands this variant before any
            // downstream inference; a defensive Unresolved is returned in the
            // unreachable case that it escapes.
            Expression::UnresolvedRegex(_) => DataType::Unresolved,
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
            // Post-analysis the inner plan is `Analyzed` and its single output
            // column's type is the scalar's type; pre-analysis it is still
            // `Unresolved` (the analyzer must run first).
            Expression::ScalarSubquery(s) => match &s.subquery {
                SubqueryPlan::Analyzed(t) => t
                    .resolved_schema
                    .fields
                    .first()
                    .map(|f| f.data_type.clone())
                    .unwrap_or(DataType::Unresolved),
                SubqueryPlan::Unanalyzed(_) => DataType::Unresolved,
            },
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
            // Analyzer's Project pre-pass expands this variant before any
            // downstream inference; conservatively nullable in the unreachable
            // escape case.
            Expression::UnresolvedRegex(_) => true,
            Expression::Binary(b) => b.left.nullable(schema) || b.right.nullable(schema),
            Expression::Unary(u) => match u.op {
                UnaryOp::IsNull | UnaryOp::IsNotNull | UnaryOp::IsNaN | UnaryOp::IsNotNaN => false,
                _ => u.operand.nullable(schema),
            },
            Expression::FunctionCall(f) => Self::function_call_nullable(f, schema),
            Expression::Cast(c) => {
                if c.try_cast {
                    return true;
                }
                // Spark rule: casting a String to a non-String type may
                // fail silently and return NULL for unparseable inputs
                // (Date, Timestamp, numeric types). Result is nullable
                // even if the source expression is non-nullable.
                let src = c.expr.data_type(schema);
                let src_is_string = matches!(src, DataType::String);
                let dst_may_fail = matches!(
                    c.to_type,
                    DataType::Date
                        | DataType::Timestamp
                        | DataType::TimestampNtz
                        | DataType::Integer
                        | DataType::Long
                        | DataType::Short
                        | DataType::Byte
                        | DataType::Float
                        | DataType::Double
                        | DataType::Decimal { .. }
                );
                if src_is_string && dst_may_fail {
                    return true;
                }
                c.expr.nullable(schema)
            }
            Expression::CaseWhen(cw) => {
                cw.else_expr.is_none()
                    || cw.else_expr.as_ref().is_some_and(|e| e.nullable(schema))
                    || cw.branches.iter().any(|(_, then)| then.nullable(schema))
            }
            Expression::Window(w) => Self::window_nullable(w, schema),
            Expression::Alias(a) => a.expr.nullable(schema),
            Expression::Star(_) => false,
            // `x [NOT] IN (subquery)` is 3-valued: a NULL member (or NULL lhs)
            // yields UNKNOWN, so the predicate result is nullable.
            Expression::InSubquery(_) => true,
            // `[NOT] EXISTS (subquery)` is always a non-null boolean.
            Expression::ExistsSubquery(_) => false,
            // A scalar subquery returns NULL when the inner plan yields no row.
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
                u.struct_expr.nullable(schema)
                    || u.updates.iter().any(|(_, e)| match e {
                        Some(expr) => expr.nullable(schema),
                        None => false,
                    })
            }
        }
    }

    // ── Structural walker ───────────────────────────────────────────────────

    /// Iterate over the immediate child expressions of this node.
    ///
    /// **τ walker convention (behavior-preserving):**
    /// - Subquery-body children (`InSubquery.expr`, `Lambda.body`) are
    ///   included so downstream `map_children` calls can walk them.
    /// - `WindowFunction.frame` boundary expressions are NOT included —
    ///   they are always literal offsets in practice and τ's transform
    ///   walkers historically did not recurse into them.
    /// - The `CommonAst` subquery bodies inside `InSubquery`,
    ///   `ExistsSubquery`, and `ScalarSubquery` are opaque to expression
    ///   walkers by contract (future τ work owns subquery analysis).
    ///
    /// Analyzer walkers that punt on entire variants (subquery / lambda /
    /// raw-sql / interval — see [`Self::map_children`] doc) should
    /// custom-case those variants BEFORE falling through to the walker
    /// default, since `children()` still enumerates the structural
    /// expression children.
    pub fn children(&self) -> Box<dyn Iterator<Item = &Expression> + '_> {
        use Expression::*;
        match self {
            Literal(_) | ColumnReference(_) | UnresolvedColumn(_) | UnresolvedRegex(_)
            | Star(_) | LambdaVariable(_) | RawSql(_) | Interval(_) | ExistsSubquery(_)
            | ScalarSubquery(_) => Box::new(std::iter::empty()),

            Unary(u) => Box::new(std::iter::once(u.operand.as_ref())),
            Cast(c) => Box::new(std::iter::once(c.expr.as_ref())),
            Alias(a) => Box::new(std::iter::once(a.expr.as_ref())),
            Lambda(l) => Box::new(std::iter::once(l.body.as_ref())),
            InSubquery(i) => Box::new(std::iter::once(i.expr.as_ref())),

            Binary(b) => Box::new([b.left.as_ref(), b.right.as_ref()].into_iter()),
            Like(l) => Box::new([l.value.as_ref(), l.pattern.as_ref()].into_iter()),
            IsDistinctFrom(d) => Box::new([d.left.as_ref(), d.right.as_ref()].into_iter()),
            ExtractValue(ev) => Box::new([ev.child.as_ref(), ev.extraction.as_ref()].into_iter()),
            Between(b) => Box::new([b.expr.as_ref(), b.low.as_ref(), b.high.as_ref()].into_iter()),

            FunctionCall(f) => Box::new(f.args.iter()),
            ArrayLiteral(a) => Box::new(a.elements.iter()),
            RowConstructor(rc) => Box::new(rc.elements.iter()),

            CaseWhen(cw) => Box::new(
                cw.branches
                    .iter()
                    .flat_map(|(w, t)| [w, t])
                    .chain(cw.else_expr.as_deref()),
            ),
            InList(i) => Box::new(std::iter::once(i.expr.as_ref()).chain(i.list.iter())),
            MapLiteral(m) => Box::new(m.entries.iter().flat_map(|(k, v)| [k, v])),
            StructLiteral(s) => Box::new(s.fields.iter().map(|(_, e)| e)),

            Window(w) => Box::new(
                std::iter::once(w.func.as_ref())
                    .chain(w.partition_by.iter())
                    .chain(w.order_by.iter().map(|so| so.expr.as_ref())),
            ),
            UpdateFields(u) => Box::new(
                std::iter::once(u.struct_expr.as_ref())
                    .chain(u.updates.iter().filter_map(|(_, e)| e.as_ref())),
            ),
        }
    }

    /// Structural map: apply `f` to each immediate child expression,
    /// preserving the node's structure.
    ///
    /// **Same child set as [`Self::children`]** (see its docstring for the
    /// window-frame + subquery-body conventions).
    ///
    /// Transform walkers built on `map_children` should custom-case
    /// variants that must be opaque to the transform (τ's
    /// `resolve_and_stamp`, for example, treats `InSubquery`,
    /// `ExistsSubquery`, `ScalarSubquery`, `Lambda`, `LambdaVariable`,
    /// `RawSql`, and `Interval` as passthrough — matching semantics
    /// preserved when `map_children` is called as the default arm).
    pub fn map_children<E>(
        self,
        mut f: impl FnMut(Expression) -> Result<Expression, E>,
    ) -> Result<Expression, E> {
        use Expression::*;
        match self {
            Literal(_) | ColumnReference(_) | UnresolvedColumn(_) | UnresolvedRegex(_)
            | Star(_) | LambdaVariable(_) | RawSql(_) | Interval(_) | ExistsSubquery(_)
            | ScalarSubquery(_) => Ok(self),

            Unary(mut u) => {
                u.operand = Box::new(f(*u.operand)?);
                Ok(Unary(u))
            }
            Cast(mut c) => {
                c.expr = Box::new(f(*c.expr)?);
                Ok(Cast(c))
            }
            Alias(mut a) => {
                a.expr = Box::new(f(*a.expr)?);
                Ok(Alias(a))
            }
            Lambda(mut l) => {
                l.body = Box::new(f(*l.body)?);
                Ok(Lambda(l))
            }
            InSubquery(mut i) => {
                i.expr = Box::new(f(*i.expr)?);
                Ok(InSubquery(i))
            }

            Binary(mut b) => {
                b.left = Box::new(f(*b.left)?);
                b.right = Box::new(f(*b.right)?);
                Ok(Binary(b))
            }
            Like(mut l) => {
                l.value = Box::new(f(*l.value)?);
                l.pattern = Box::new(f(*l.pattern)?);
                Ok(Like(l))
            }
            IsDistinctFrom(mut d) => {
                d.left = Box::new(f(*d.left)?);
                d.right = Box::new(f(*d.right)?);
                Ok(IsDistinctFrom(d))
            }
            ExtractValue(mut ev) => {
                ev.child = Box::new(f(*ev.child)?);
                ev.extraction = Box::new(f(*ev.extraction)?);
                Ok(ExtractValue(ev))
            }
            Between(mut b) => {
                b.expr = Box::new(f(*b.expr)?);
                b.low = Box::new(f(*b.low)?);
                b.high = Box::new(f(*b.high)?);
                Ok(Between(b))
            }

            FunctionCall(mut fc) => {
                fc.args = fc
                    .args
                    .into_iter()
                    .map(&mut f)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(FunctionCall(fc))
            }
            ArrayLiteral(mut a) => {
                a.elements = a
                    .elements
                    .into_iter()
                    .map(&mut f)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ArrayLiteral(a))
            }
            RowConstructor(mut rc) => {
                rc.elements = rc
                    .elements
                    .into_iter()
                    .map(&mut f)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(RowConstructor(rc))
            }

            CaseWhen(mut cw) => {
                cw.branches = cw
                    .branches
                    .into_iter()
                    .map(|(w, t)| Ok::<_, E>((f(w)?, f(t)?)))
                    .collect::<Result<Vec<_>, E>>()?;
                if let Some(e) = cw.else_expr {
                    cw.else_expr = Some(Box::new(f(*e)?));
                }
                Ok(CaseWhen(cw))
            }
            InList(mut i) => {
                i.expr = Box::new(f(*i.expr)?);
                i.list = i
                    .list
                    .into_iter()
                    .map(&mut f)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(InList(i))
            }
            MapLiteral(mut m) => {
                m.entries = m
                    .entries
                    .into_iter()
                    .map(|(k, v)| Ok::<_, E>((f(k)?, f(v)?)))
                    .collect::<Result<Vec<_>, E>>()?;
                Ok(MapLiteral(m))
            }
            StructLiteral(mut s) => {
                s.fields = s
                    .fields
                    .into_iter()
                    .map(|(n, e)| Ok::<_, E>((n, f(e)?)))
                    .collect::<Result<Vec<_>, E>>()?;
                Ok(StructLiteral(s))
            }

            Window(mut w) => {
                w.func = Box::new(f(*w.func)?);
                w.partition_by = w
                    .partition_by
                    .into_iter()
                    .map(&mut f)
                    .collect::<Result<Vec<_>, _>>()?;
                let mut new_order = Vec::with_capacity(w.order_by.len());
                for so in w.order_by {
                    let e = f(*so.expr)?;
                    new_order.push(SortOrder {
                        expr: Box::new(e),
                        direction: so.direction,
                        null_ordering: so.null_ordering,
                    });
                }
                w.order_by = new_order;
                Ok(Window(w))
            }
            UpdateFields(mut u) => {
                u.struct_expr = Box::new(f(*u.struct_expr)?);
                u.updates = u
                    .updates
                    .into_iter()
                    .map(|(n, opt_e)| {
                        let mapped = match opt_e {
                            Some(e) => Some(f(e)?),
                            None => None,
                        };
                        Ok::<_, E>((n, mapped))
                    })
                    .collect::<Result<Vec<_>, E>>()?;
                Ok(UpdateFields(u))
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
        // Struct-constructor fast-paths — Spark's `struct` / `named_struct`
        // return a `DataType::Struct` whose field names depend on the shape
        // of the argument tree. `TypeInferenceEngine::function_return_type`
        // takes only the function name + first-arg type, so it cannot express
        // this; derive here where the full `&FunctionCall` is available.
        // Symmetric with emission's `struct` / `named_struct` arms.
        match f.name.to_lowercase().as_str() {
            "struct" => {
                let fields: Vec<StructField> = f
                    .args
                    .iter()
                    .enumerate()
                    .map(|(i, arg)| {
                        let name = super::struct_names::derive_struct_field_name(arg, i);
                        StructField::new(name, arg.data_type(schema), arg.nullable(schema))
                    })
                    .collect();
                return DataType::Struct(StructType::new(fields));
            }
            "named_struct" => {
                // `named_struct(k1, v1, k2, v2, ...)` — Spark rejects
                // non-literal keys with AnalysisException; emission enforces
                // the same. If any key here is not a string literal, fall
                // through to the default arm so the shared inference path
                // returns `DataType::Unresolved` rather than fabricating a
                // fake schema.
                if !f.args.is_empty() && f.args.len() % 2 == 0 {
                    let mut fields: Vec<StructField> = Vec::with_capacity(f.args.len() / 2);
                    let mut i = 0;
                    let mut ok = true;
                    while i < f.args.len() {
                        let key = match &f.args[i] {
                            Expression::Literal(l) => match &l.value {
                                LiteralValue::String(s) => s.clone(),
                                _ => {
                                    ok = false;
                                    break;
                                }
                            },
                            _ => {
                                ok = false;
                                break;
                            }
                        };
                        let val = &f.args[i + 1];
                        fields.push(StructField::new(
                            key,
                            val.data_type(schema),
                            val.nullable(schema),
                        ));
                        i += 2;
                    }
                    if ok {
                        return DataType::Struct(StructType::new(fields));
                    }
                }
                // Fall through — malformed named_struct; let the shared
                // inference path decide (typically Unresolved).
            }
            // Spark's `arrays_zip(a, b, ...)` returns
            // `Array<Struct<f0: T0, f1: T1, ...>>` where each field's type is
            // the element type of the corresponding input array. Field
            // names follow Spark's rules: alias > column-ref name > positional
            // integer string. Emission matches this shape exactly (see
            // `emission.rs`, `"arrays_zip"` arm). Corpus anchor: `arr-012`.
            "arrays_zip" if !f.args.is_empty() => {
                // Derive per-arg field names — same rules as emission.
                let mut names: Vec<String> = Vec::with_capacity(f.args.len());
                for (i, arg) in f.args.iter().enumerate() {
                    let name = match arg {
                        Expression::Alias(a) => a.alias.clone(),
                        Expression::ColumnReference(c) => c.name.clone(),
                        Expression::UnresolvedColumn(u) => u.name.clone(),
                        _ => i.to_string(),
                    };
                    names.push(name);
                }
                // Note: Spark tolerates duplicate field names in the returned
                // struct schema (unlike DuckDB `struct_pack` which requires
                // unique names). Preserve Spark's schema (duplicates and all);
                // emission separately falls back to positional names to keep
                // DuckDB happy.
                let mut fields: Vec<StructField> = Vec::with_capacity(f.args.len());
                for (name, arg) in names.iter().zip(f.args.iter()) {
                    let arg_ty = arg.data_type(schema);
                    let (elem_ty, elem_nullable) = match arg_ty {
                        DataType::Array(inner, contains_null) => (*inner, contains_null),
                        _ => (DataType::Unresolved, true),
                    };
                    fields.push(StructField::new(name.clone(), elem_ty, elem_nullable));
                }
                // Spark stamps the outer array as non-nullable elements
                // (containsNull=false) — the struct itself is always present
                // per input row.
                return DataType::Array(Box::new(DataType::Struct(StructType::new(fields))), false);
            }
            // Spark's `coalesce(a, b, c, ...)` returns the least-common
            // (widening) type across all args. First-arg-only inference
            // misses e.g. `coalesce(decimal(9,2), decimal(2,2)) → decimal(10,2)`.
            // Corpus anchor: `cond-004`.
            "coalesce" | "nvl" | "ifnull" if !f.args.is_empty() => {
                let mut acc = f.args[0].data_type(schema);
                for a in f.args.iter().skip(1) {
                    let dt = a.data_type(schema);
                    acc = TypeInferenceEngine::promote_numeric(&acc, &dt);
                }
                return acc;
            }
            // Spark's `greatest` / `least` — same widening rule as coalesce.
            "greatest" | "least" if !f.args.is_empty() => {
                let mut acc = f.args[0].data_type(schema);
                for a in f.args.iter().skip(1) {
                    let dt = a.data_type(schema);
                    acc = TypeInferenceEngine::promote_numeric(&acc, &dt);
                }
                return acc;
            }
            // Spark's `nvl2(cond, ifNotNull, ifNull)` — returns `ifNotNull` if
            // `cond IS NOT NULL`, otherwise `ifNull`. Result type is the least
            // common type of args[1] and args[2] (both branches are
            // evaluated). The shared resolver only sees `args[0]`; use args[1]
            // as the anchor (matches Spark's promotion when args[1]/args[2]
            // agree). Corpus anchor: `cond-007`.
            "nvl2" if f.args.len() == 3 => {
                return f.args[1].data_type(schema);
            }
            // Spark's `if(cond, then, else)` / `ifnull(a, b)` / `iif(...)` —
            // return-type derives from the branch args (not the condition).
            "if" if f.args.len() == 3 => {
                return f.args[1].data_type(schema);
            }
            "iif" if f.args.len() == 3 => {
                return f.args[1].data_type(schema);
            }
            "ifnull" if f.args.len() == 2 => {
                // Both branches meaningful; use the first (Spark spec).
                let first = f.args[0].data_type(schema);
                if matches!(first, DataType::Unresolved) {
                    return f.args[1].data_type(schema);
                }
                return first;
            }
            // Spark's `array(a, b, ...)` — element type is the least-common
            // (widening) type of the args. First-arg-only inference misses
            // the mixed-numeric case (e.g., `array(1, 2.0, 3)` → Double).
            // Corpus anchor: `type-020`.
            "array" | "list_value" | "make_array" | "list" if !f.args.is_empty() => {
                let mut acc = f.args[0].data_type(schema);
                for a in f.args.iter().skip(1) {
                    let dt = a.data_type(schema);
                    acc = TypeInferenceEngine::promote_numeric(&acc, &dt);
                }
                // Spark reports the array as `containsNull` = any element
                // nullable. Result nullability is handled separately in
                // `function_call_nullable`; here we just carry the flag
                // conservatively as `true` (any-null-permitted) matching
                // the shared resolver's behavior.
                let contains_null = f.args.iter().any(|a| a.nullable(schema));
                return DataType::Array(Box::new(acc), contains_null);
            }
            // Spark's `aggregate(arr, init, (acc, x) -> f [, finish])` folds
            // the array with `init` as the seed; the result type is the
            // finish-lambda's return type (or, if `finish` is absent, the
            // seed / accumulator type). The shared `function_return_type`
            // resolver only receives the first arg's type, so it cannot
            // express this. Derive here where the whole `FunctionCall` is
            // available. Corpus anchor: `hof-003`.
            "aggregate" | "reduce" | "list_reduce" if f.args.len() >= 2 => {
                // Prefer the init's type (arg[1]) — Spark accepts init as
                // any expression and the accumulator inherits its type.
                return f.args[1].data_type(schema);
            }
            // Spark's `to_number(str, fmt)` / `try_to_number(str, fmt)` return
            // DECIMAL(p, s) derived from the format string. Emission parses
            // the same format literal to build the CAST; mirror the
            // precision/scale derivation here so the projection schema
            // matches Spark. Falls through to the shared resolver (returns
            // String, matching arg[0]) when the format is not a literal or
            // not a recognized digit template. Corpus anchor: `parse-004`.
            "to_number" | "try_to_number" if f.args.len() == 2 => {
                if let Expression::Literal(Literal {
                    value: LiteralValue::String(fmt),
                    ..
                }) = &f.args[1]
                {
                    if let Some((precision, scale)) =
                        super::emission::parse_number_format_for_type_inference(fmt)
                    {
                        return DataType::Decimal { precision, scale };
                    }
                }
            }
            // Spark's `from_json(json_str, ddl_schema)` returns a Struct
            // typed per the DDL literal. Mirror emission's DDL translation
            // for type inference so the projection schema matches Spark.
            // Corpus anchors: `json-003`, `json-004`.
            "from_json" if f.args.len() == 2 => {
                if let Expression::Literal(Literal {
                    value: LiteralValue::String(ddl),
                    ..
                }) = &f.args[1]
                {
                    if let Some(st) =
                        super::emission::from_json_ddl_to_struct_for_type_inference(ddl)
                    {
                        return DataType::Struct(st);
                    }
                }
            }
            // Spark's `from_csv(csv_str, ddl_schema)` returns a Struct typed
            // per the DDL literal (flat primitives only — Spark's own
            // surface). Mirror emission's DDL translation so the projection
            // schema matches Spark. Corpus anchor: `json-007`.
            "from_csv" if f.args.len() == 2 => {
                if let Expression::Literal(Literal {
                    value: LiteralValue::String(ddl),
                    ..
                }) = &f.args[1]
                {
                    if let Some(st) = super::emission::from_csv_ddl_to_struct(ddl) {
                        return DataType::Struct(st);
                    }
                }
            }
            // Pass 90 — synthetic per-field FunctionCall names produced by
            // the analyzer's Project pre-pass for `F.inline` / `F.inline_outer`
            // (see `analyzer::expand_inline_projections`). `args[0]` is the
            // resolved `Array<Struct<...>>`; `args[1]` is a `Literal::String`
            // carrying the target field name. Return type = struct field's
            // type (case-insensitive lookup, matching Spark). Corpus: inl-001,
            // inl-002.
            "inline_field" | "inline_outer_field" if f.args.len() == 2 => {
                if let (
                    arr,
                    Expression::Literal(Literal {
                        value: LiteralValue::String(field_name),
                        ..
                    }),
                ) = (&f.args[0], &f.args[1])
                {
                    if let DataType::Array(inner, _) = arr.data_type(schema) {
                        if let DataType::Struct(st) = *inner {
                            for field in &st.fields {
                                if field.name.eq_ignore_ascii_case(field_name) {
                                    return field.data_type.clone();
                                }
                            }
                        }
                    }
                }
                return DataType::Unresolved;
            }
            // Pass 91 — synthetic per-key FunctionCall names produced by the
            // analyzer's Project pre-pass for `F.json_tuple` (see
            // `analyzer::expand_json_tuple_projections`). `args[0]` is the
            // JSON string expression; `args[1]` is a `Literal::String`
            // carrying the target key. Return type is always `String` per
            // Spark's `JsonTuple.elementSchema`. Corpus: json-002.
            "json_tuple_field" if f.args.len() == 2 => {
                return DataType::String;
            }
            // Spark's 2-arg `ceil(x, t)` / `floor(x, t)` (`RoundCeil`/
            // `RoundFloor`) implicitly cast the child to Decimal and return a
            // scaled Decimal derived from the child type + literal target
            // scale. The shared `function_return_type` resolver only sees the
            // first arg's type and cannot read the scale literal, so derive it
            // here where the whole `FunctionCall` is available. A non-literal
            // scale is a Thunderduck boundary → `Unresolved`. 1-arg ceil/floor
            // falls through to the shared resolver. Corpus: `num-003`.
            "ceil" | "ceiling" | "floor" if f.args.len() == 2 => {
                let input = f.args[0].data_type(schema);
                match int_literal_value(&f.args[1]) {
                    Some(t) => return TypeInferenceEngine::ceil_floor_type(&input, Some(t)),
                    None => return DataType::Unresolved,
                }
            }
            _ => {}
        }
        let first_arg_type = f.args.first().map(|a| a.data_type(schema));
        TypeInferenceEngine::function_return_type(&f.name, first_arg_type.as_ref())
    }

    // ── FunctionCall nullability ─────────────────────────────────────────────

    /// Names in this list report `nullable = false` regardless of arg nullability.
    ///
    /// **Precondition:** `name_lower` MUST already be lowercase. Debug builds
    /// `debug_assert!` this; release builds trust the contract to avoid an
    /// unnecessary allocation.
    ///
    /// Contains the count family (checklist §1.1) and the hash family
    /// (checklist §1.2). Extending this list requires adding to the
    /// symmetric-omission tests (§8) as well.
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
                | "collect_list"
                | "collect_set"
                | "array_agg"
                | "approx_count_distinct"
                | "count_approx_distinct"
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
            // Spark's `format_string(fmt, args...)` returns non-nullable —
            // NULL args render as the literal string "null" rather than
            // propagating NULL. Corpus witness: `str-015`.
            "format_string" | "printf" => false,
            "typeof" | "spark_partition_id" | "monotonically_increasing_id" => false,
            // Spark scalars declared nullable regardless of arg nullability
            // (overflow / parse-fail / undefined-domain producers).
            "factorial" | "url_encode" | "url_decode" | "parse_url"
            | "to_number" | "try_to_number" | "to_date_ntz"
            // Spark's `from_unixtime(secs[, fmt])` declares nullable=True
            // even for a non-null seconds literal — the value can be NULL
            // when the format is invalid. Corpus witness dt-014.
            | "from_unixtime"
            | "map_from_entries" | "try_add" | "try_subtract"
            | "try_multiply" | "try_divide" | "try_element_at"
            // Spark's `to_json(struct)` / `to_csv(struct)` — schema-declared
            // nullable=True even when the argument is a non-null `struct(...)`
            // constructor. PySpark's projection semantics: the result column
            // comes back nullable=True even though the Catalyst
            // `CreateStruct` value is non-null. Corpus: `json-005`, `json-008`.
            // Note: `schema_of_json` is NOT in this list — Spark reports its
            // result as nullable=False when the JSON literal is a
            // non-null literal (corpus witness: `json-006` requires
            // Reference=False), so it falls through to the default
            // `any(arg.nullable)` path which correctly yields false for
            // literal arguments.
            | "to_json" | "to_csv" => true,
            "greatest" | "least" => f.args.iter().all(|a| a.nullable(schema)),
            "nvl2" => {
                f.args.get(1).is_none_or(|a| a.nullable(schema))
                    || f.args.get(2).is_none_or(|a| a.nullable(schema))
            }
            "array" | "make_array" | "create_map" | "map" | "named_struct" | "struct" => false,
            // `F.window(ts, dur)` — Spark's `TimeWindow` is rewritten by the
            // analyzer into `CreateNamedStruct(start := ..., end := ...)`
            // whose `nullable = false` (Spark's struct-construction is never
            // null; only the fields inside carry per-field nullability).
            // τ's default `any(arg.nullable)` fallback would incorrectly
            // propagate the timestamp arg's nullable into the struct itself;
            // pin `false` here to match Spark's observable schema. Corpus:
            // `win2-002`.
            "window" => false,
            // Generator functions (row-multiplying via UNNEST at emission).
            // `explode(arr)` / `posexplode_val(arr)` — element nullability
            // follows the array's `containsNull` flag AND the array arg's own
            // nullability (a NULL array in `explode` produces zero rows).
            // Corpus: arr-015, arr-017, type-012.
            "explode" | "posexplode_val" => match f.args.first() {
                Some(arg) => {
                    let contains_null = matches!(
                        arg.data_type(schema),
                        DataType::Array(_, true) | DataType::Map { .. }
                    );
                    contains_null || arg.nullable(schema)
                }
                None => true,
            },
            // `explode_outer(arr)` — always nullable: empty / NULL arrays
            // emit exactly one row with a NULL value. Corpus: arr-016.
            "explode_outer" => true,
            // `posexplode_pos(arr)` — the position column is a synthetic
            // 0-indexed integer, never NULL. Non-nullable regardless of the
            // input array's nullability. Corpus: arr-017.
            "posexplode_pos" => false,
            // Pass 90 — synthetic per-field FunctionCalls for
            // `F.inline` / `F.inline_outer` (analyzer's Project pre-pass —
            // `expand_inline_projections`). Args: (arr, field_name_literal).
            //
            // `inline_field` — nullability follows Spark's `Inline`:
            //   * struct field's own nullability (case-insensitive lookup),
            //   * OR the array's `containsNull` flag,
            //   * OR the array arg's nullability (a NULL array yields zero
            //     rows for the inner variant, so array-nullability doesn't
            //     truly propagate to the produced column; keep the
            //     conservative disjunction — matches `explode`'s posexplode_val
            //     arm above for parity).
            // Corpus: inl-001.
            "inline_field" => match (f.args.first(), f.args.get(1)) {
                (
                    Some(arr),
                    Some(Expression::Literal(Literal {
                        value: LiteralValue::String(field_name),
                        ..
                    })),
                ) => {
                    let arr_ty = arr.data_type(schema);
                    let (contains_null, field_nullable) = match &arr_ty {
                        DataType::Array(inner, cn) => match inner.as_ref() {
                            DataType::Struct(st) => {
                                let field_null = st
                                    .fields
                                    .iter()
                                    .find(|f0| f0.name.eq_ignore_ascii_case(field_name))
                                    .map(|f0| f0.nullable)
                                    .unwrap_or(true);
                                (*cn, field_null)
                            }
                            _ => (true, true),
                        },
                        _ => (true, true),
                    };
                    arr.nullable(schema) || contains_null || field_nullable
                }
                _ => true,
            },
            // `inline_outer_field` — always nullable: the empty / NULL array
            // sentinel row is all-NULL by construction. Mirrors
            // `explode_outer`'s arm above. Corpus: inl-002.
            "inline_outer_field" => true,
            // Pass 91 — synthetic `json_tuple_field(json, "<key>")` produced
            // by the analyzer's Project pre-pass (`expand_json_tuple_projections`).
            // Always nullable — Spark returns NULL for missing key OR JSON
            // null value OR NULL `json_str`. Corpus: json-002.
            "json_tuple_field" => true,
            // Synthetic `map_explode_key(m)` / `map_explode_val(m)` (map-007).
            // Spark's `explode(map)` produces `(key, value)` rows where keys
            // are ALWAYS non-nullable (Spark's MAP invariant); a NULL map
            // arg emits zero rows, so a nullable outer map does not
            // propagate to the key column. Values inherit the map's
            // `valueContainsNull` flag.
            "map_explode_key" => false,
            "map_explode_val" => match f.args.first() {
                Some(arg) => matches!(
                    arg.data_type(schema),
                    DataType::Map {
                        value_nullable: true,
                        ..
                    }
                ),
                None => true,
            },
            // Spark's `flatten(Array<Array<T>>)` returns NULL if the outer
            // array contains any NULL inner array. Even a non-nullable
            // literal outer array (`F.array(...)`) produces a nullable
            // result per Spark's schema semantics. Corpus: `arr-013`.
            "flatten" | "list_flatten" => true,
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
                    // Spark rule: `lag(col, offset, default)` is nullable
                    // iff `col` is nullable OR `default` is nullable. When
                    // `default` is absent (no 3rd arg), the out-of-range
                    // return is NULL — so nullable=true regardless of
                    // col's nullability.
                    let col_nullable = f.args.first().is_none_or(|c| c.nullable(schema));
                    match f.args.get(2) {
                        Some(default) => col_nullable || default.nullable(schema),
                        None => true,
                    }
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
        // Delegate to the shared classifier so analyzer + emission cannot
        // drift. `data_type()` is infallible — a missing drop target here
        // silently leaves the struct unchanged. `resolve_and_stamp` runs
        // `validate_update_fields_ops` earlier and rejects such inputs with
        // an [`AnalyzerError::Other`] (Spark-emulated).
        apply_update_fields_ops(
            &mut st.fields,
            &u.updates,
            |name, new_val| {
                StructField::new(
                    name.to_owned(),
                    new_val.data_type(schema),
                    new_val.nullable(schema),
                )
            },
            |slot, name, new_val| {
                slot.name = name.to_owned();
                slot.data_type = new_val.data_type(schema);
                slot.nullable = new_val.nullable(schema);
            },
            |f| f.name.as_str(),
        );
        DataType::Struct(st)
    }
}

// ── UpdateFields shared classifier ──────────────────────────────────────────

/// Apply Spark `withField` / `dropFields` operations to a caller-owned field
/// list, preserving Spark 4.1 semantics:
///
/// * Add / replace matches existing fields **case-insensitively** (ASCII);
///   on match the *original* declared field name is preserved.
/// * Drop matches existing fields **case-insensitively** (ASCII).
/// * A drop target that does not match any current field is **silently
///   ignored** here — callers that need Spark-emulated rejection must invoke
///   [`validate_update_fields_ops`] first.
///
/// The callbacks let the caller decide the payload type `T`:
///
/// * `make_append` — produce a fresh `(name, T)` slot for an appended field.
/// * `update_in_place` — replace an existing slot (preserving position).
/// * `slot_name` — extract the current name from a slot for the CI match.
///
/// The analyzer's `update_fields_data_type` and emission's
/// `render_update_fields` both delegate here, guaranteeing identical
/// field-list evolution.
pub(super) fn apply_update_fields_ops<T>(
    fields: &mut Vec<T>,
    updates: &[(String, Option<Expression>)],
    make_append: impl Fn(&str, &Expression) -> T,
    mut update_in_place: impl FnMut(&mut T, &str, &Expression),
    slot_name: impl Fn(&T) -> &str,
) {
    for (name, op) in updates {
        match op {
            Some(new_val) => {
                let existing = fields
                    .iter()
                    .position(|slot| slot_name(slot).eq_ignore_ascii_case(name));
                if let Some(idx) = existing {
                    // Preserve the ORIGINAL field name — Spark `withField`
                    // keeps the struct's declared casing.
                    let original = slot_name(&fields[idx]).to_owned();
                    update_in_place(&mut fields[idx], &original, new_val);
                } else {
                    fields.push(make_append(name, new_val));
                }
            }
            None => {
                if let Some(idx) = fields
                    .iter()
                    .position(|slot| slot_name(slot).eq_ignore_ascii_case(name))
                {
                    fields.remove(idx);
                }
            }
        }
    }
}

/// Validate a Spark `UpdateFields` op list against a base struct's declared
/// field names. Returns the first drop target that does not resolve
/// (case-insensitive), which the analyzer surfaces as a Spark-emulated error.
///
/// Ordering: `updates` are applied left to right, so a drop-then-add sequence
/// on the same name is legal (the add would append). We walk a virtual
/// projected field-name list and check drops against it.
pub(super) fn validate_update_fields_ops(
    base_field_names: &[String],
    updates: &[(String, Option<Expression>)],
) -> Result<(), String> {
    let mut names: Vec<String> = base_field_names.to_vec();
    for (name, op) in updates {
        match op {
            Some(_) => {
                let existing = names.iter().position(|n| n.eq_ignore_ascii_case(name));
                if existing.is_none() {
                    names.push(name.clone());
                }
                // In-place replace preserves the declared name — no
                // change to the `names` vector.
            }
            None => {
                let idx = names
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(name))
                    .ok_or_else(|| name.clone())?;
                names.remove(idx);
            }
        }
    }
    Ok(())
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

    /// `F.window(ts, dur)` — Spark's `TimeWindow` rewrites to
    /// `CreateNamedStruct`, whose `nullable = false`. τ must report the
    /// struct itself as non-nullable regardless of the timestamp arg's own
    /// nullability (only the inner `start` / `end` fields carry
    /// per-field nullability). Corpus: `win2-002`.
    #[test]
    fn window_function_call_is_never_nullable() {
        // Nullable timestamp arg — struct itself must still be non-null.
        let schema = StructType::new(vec![StructField::nullable("ts", DataType::Timestamp)]);
        let expr = Expression::FunctionCall(FunctionCall {
            name: "window".to_owned(),
            args: vec![
                ColumnReference::untyped("ts"),
                Expression::Literal(Literal {
                    value: LiteralValue::String("1 day".to_owned()),
                    data_type: DataType::String,
                }),
            ],
            distinct: false,
        });
        assert!(!expr.nullable(&schema));
        // Also non-nullable when the timestamp arg is non-null.
        let schema_nn = StructType::new(vec![StructField::not_null("ts", DataType::Timestamp)]);
        assert!(!expr.nullable(&schema_nn));
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

    // ── Pass 85 — UnresolvedRegex variant ────────────────────────────────

    #[test]
    fn unresolved_regex_variant_construction() {
        let expr = Expression::UnresolvedRegex(UnresolvedRegexExpression {
            pattern: ".*_id".to_owned(),
            plan_id: Some(7),
        });
        match &expr {
            Expression::UnresolvedRegex(r) => {
                assert_eq!(r.pattern, ".*_id");
                assert_eq!(r.plan_id, Some(7));
            }
            _ => panic!("expected UnresolvedRegex variant"),
        }
    }

    #[test]
    fn unresolved_regex_data_type_is_unresolved_and_nullable_true() {
        let s = StructType::empty();
        let expr = Expression::UnresolvedRegex(UnresolvedRegexExpression {
            pattern: ".*".to_owned(),
            plan_id: None,
        });
        assert_eq!(expr.data_type(&s), DataType::Unresolved);
        assert!(expr.nullable(&s));
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

    // ── §7 subquery variants carry a SubqueryPlan ────────────────────────

    #[test]
    fn in_subquery_carries_unanalyzed_plan() {
        use super::super::ast::{CommonAst, CommonOp};
        let sub = CommonAst::new(CommonOp::SingleRow);
        let expr = Expression::InSubquery(InSubquery {
            expr: Box::new(ColumnReference::untyped("x")),
            subquery: SubqueryPlan::Unanalyzed(Box::new(sub)),
            negated: false,
        });
        // Compile-only sanity; ensures the field type is `SubqueryPlan`.
        assert!(matches!(expr, Expression::InSubquery(_)));
    }

    #[test]
    fn exists_subquery_data_type_boolean() {
        use super::super::ast::{CommonAst, CommonOp};
        let s = StructType::empty();
        let expr = Expression::ExistsSubquery(ExistsSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(CommonAst::new(CommonOp::SingleRow))),
            negated: false,
        });
        assert_eq!(expr.data_type(&s), DataType::Boolean);
        assert!(!expr.nullable(&s));
    }

    #[test]
    fn in_subquery_is_nullable_three_valued() {
        use super::super::ast::{CommonAst, CommonOp};
        let s = StructType::empty();
        let expr = Expression::InSubquery(InSubquery {
            expr: Box::new(ColumnReference::untyped("x")),
            subquery: SubqueryPlan::Unanalyzed(Box::new(CommonAst::new(CommonOp::SingleRow))),
            negated: true,
        });
        assert_eq!(expr.data_type(&s), DataType::Boolean);
        // 3VL: a NULL member yields UNKNOWN → the predicate is nullable.
        assert!(expr.nullable(&s));
    }

    #[test]
    fn scalar_subquery_unanalyzed_data_type_unresolved() {
        use super::super::ast::{CommonAst, CommonOp};
        let s = StructType::empty();
        let expr = Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(CommonAst::new(CommonOp::SingleRow))),
        });
        assert_eq!(expr.data_type(&s), DataType::Unresolved);
        assert!(expr.nullable(&s));
    }

    #[test]
    fn scalar_subquery_analyzed_data_type_from_inner_col() {
        use super::super::analyzer::{TypedAst, TypedOp};
        let s = StructType::empty();
        // A hand-built analyzed inner plan whose single output column is Long.
        let inner = TypedAst {
            op: TypedOp::SingleRow,
            resolved_schema: StructType::new(vec![StructField::nullable(
                "max_salary",
                DataType::Long,
            )]),
        };
        let expr = Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Analyzed(Box::new(inner)),
        });
        assert_eq!(expr.data_type(&s), DataType::Long);
        assert!(expr.nullable(&s));
    }

    // ── Struct-constructor fast-paths (§9 tests 7 & 8) ─────────────────────

    /// §9 test 7 — `struct(name, age)` reports
    /// `DataType::Struct{ name: String, age: Integer }`.
    #[test]
    fn struct_data_type_is_named_struct() {
        let schema = StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::not_null("age", DataType::Integer),
        ]);
        let expr = Expression::FunctionCall(FunctionCall {
            name: "struct".to_owned(),
            args: vec![
                ColumnReference::untyped("name"),
                ColumnReference::untyped("age"),
            ],
            distinct: false,
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 2);
                assert_eq!(st.fields[0].name, "name");
                assert_eq!(st.fields[0].data_type, DataType::String);
                assert!(st.fields[0].nullable);
                assert_eq!(st.fields[1].name, "age");
                assert_eq!(st.fields[1].data_type, DataType::Integer);
                assert!(!st.fields[1].nullable);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
        // Nullability of the struct expression itself is non-null.
        assert!(!expr.nullable(&schema));
    }

    /// §9 test 8 — alias name wins in `struct(...)` field-name derivation.
    #[test]
    fn struct_data_type_alias_wins() {
        let schema = StructType::new(vec![StructField::nullable("name", DataType::String)]);
        let aliased = Expression::Alias(AliasExpression {
            expr: Box::new(ColumnReference::untyped("name")),
            alias: "who".to_owned(),
        });
        let expr = Expression::FunctionCall(FunctionCall {
            name: "struct".to_owned(),
            args: vec![aliased],
            distinct: false,
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 1);
                assert_eq!(st.fields[0].name, "who");
                assert_eq!(st.fields[0].data_type, DataType::String);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    /// Companion — `named_struct` fast-path picks up literal keys.
    #[test]
    fn named_struct_data_type_uses_literal_keys() {
        let schema = StructType::new(vec![
            StructField::nullable("a", DataType::Integer),
            StructField::nullable("b", DataType::String),
        ]);
        let key_x = Expression::Literal(Literal {
            value: LiteralValue::String("x".to_owned()),
            data_type: DataType::String,
        });
        let key_y = Expression::Literal(Literal {
            value: LiteralValue::String("y".to_owned()),
            data_type: DataType::String,
        });
        let expr = Expression::FunctionCall(FunctionCall {
            name: "named_struct".to_owned(),
            args: vec![
                key_x,
                ColumnReference::untyped("a"),
                key_y,
                ColumnReference::untyped("b"),
            ],
            distinct: false,
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 2);
                assert_eq!(st.fields[0].name, "x");
                assert_eq!(st.fields[0].data_type, DataType::Integer);
                assert_eq!(st.fields[1].name, "y");
                assert_eq!(st.fields[1].data_type, DataType::String);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    // ── UpdateFields (Pass 61 — struct-005 / struct-006) ────────────────────

    fn address_struct_type() -> DataType {
        DataType::Struct(StructType::new(vec![
            StructField::nullable("street", DataType::String),
            StructField::nullable("city", DataType::String),
            StructField::nullable("geo", DataType::String),
        ]))
    }

    fn address_column() -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: "address".to_owned(),
            qualifier: None,
            data_type: Some(address_struct_type()),
            nullable: Some(true),
        })
    }

    /// `withField("country", "AT")` appends a new field to the struct's field
    /// list, preserving the existing fields.
    #[test]
    fn update_fields_with_field_adds_new_field() {
        let schema = StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]);
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![(
                "country".to_owned(),
                Some(Expression::Literal(Literal {
                    value: LiteralValue::String("AT".to_owned()),
                    data_type: DataType::String,
                })),
            )],
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 4);
                assert_eq!(st.fields[0].name, "street");
                assert_eq!(st.fields[3].name, "country");
                assert_eq!(st.fields[3].data_type, DataType::String);
                assert!(!st.fields[3].nullable);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    /// `withField("city", "SomeCity")` on an existing field replaces its
    /// type/nullability in place.
    #[test]
    fn update_fields_with_field_replaces_existing_field() {
        let schema = StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]);
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![(
                "city".to_owned(),
                Some(Expression::Literal(Literal {
                    value: LiteralValue::String("Vienna".to_owned()),
                    data_type: DataType::String,
                })),
            )],
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 3);
                assert_eq!(st.fields[1].name, "city");
                assert!(!st.fields[1].nullable);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    /// `dropFields("geo")` removes the named field from the struct.
    #[test]
    fn update_fields_drop_field_removes_field() {
        let schema = StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]);
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![("geo".to_owned(), None)],
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 2);
                assert_eq!(st.fields[0].name, "street");
                assert_eq!(st.fields[1].name, "city");
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    /// `dropFields` is case-insensitive per Spark semantics.
    #[test]
    fn update_fields_drop_field_is_case_insensitive() {
        let schema = StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]);
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![("GEO".to_owned(), None)],
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 2);
                assert!(!st.fields.iter().any(|f| f.name.eq_ignore_ascii_case("geo")));
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    /// Review-fix C1: `withField("CITY", ...)` on a struct declaring `city`
    /// replaces the existing slot (case-insensitive match) and preserves the
    /// original declared field name `"city"`, matching Spark 4.1.
    #[test]
    fn update_fields_with_field_is_case_insensitive_and_preserves_original_name() {
        let schema = StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]);
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![(
                "CITY".to_owned(),
                Some(Expression::Literal(Literal {
                    value: LiteralValue::String("Vienna".to_owned()),
                    data_type: DataType::String,
                })),
            )],
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                assert_eq!(st.fields.len(), 3);
                // Position preserved AND original casing kept.
                assert_eq!(st.fields[1].name, "city");
                assert_eq!(st.fields[1].data_type, DataType::String);
                assert!(!st.fields[1].nullable);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    /// Review-fix C2: analyzer's `update_fields_data_type` and emission's
    /// `render_update_fields` must produce the *same* struct schema for a
    /// mixed-case op sequence. This test locks the analyzer view; the
    /// matching emission-side lock lives in `emission.rs`
    /// (`render_update_fields_mixed_case_agrees_with_analyzer`).
    #[test]
    fn update_fields_analyzer_schema_matches_mixed_case_ops() {
        let schema = StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]);
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![
                (
                    "CITY".to_owned(),
                    Some(Expression::Literal(Literal {
                        value: LiteralValue::String("Vienna".to_owned()),
                        data_type: DataType::String,
                    })),
                ),
                ("GEO".to_owned(), None),
                (
                    "country".to_owned(),
                    Some(Expression::Literal(Literal {
                        value: LiteralValue::String("AT".to_owned()),
                        data_type: DataType::String,
                    })),
                ),
            ],
        });
        match expr.data_type(&schema) {
            DataType::Struct(st) => {
                let names: Vec<&str> = st.fields.iter().map(|f| f.name.as_str()).collect();
                // "city" preserved (not "CITY"), "geo" removed, "country" appended.
                assert_eq!(names, vec!["street", "city", "country"]);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
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

    /// Pass 70 anchor — `aggregate(arr, init, lambda)` must resolve to the
    /// init/seed type, not to `Array<T>`. Corpus: `hof-003` returns String
    /// because the seed `F.lit("")` is a String literal.
    #[test]
    fn aggregate_hof_returns_seed_type_not_array_type() {
        let s = StructType::new(vec![StructField::nullable(
            "tags",
            DataType::Array(Box::new(DataType::String), true),
        )]);
        let expr = Expression::FunctionCall(FunctionCall {
            name: "aggregate".to_owned(),
            args: vec![
                ColumnReference::untyped("tags"),
                Expression::Literal(Literal {
                    value: LiteralValue::String(String::new()),
                    data_type: DataType::String,
                }),
                // Real emissions place a Lambda here; the type inference
                // fast-path reads only args[0..2], so a placeholder col
                // is sufficient for this test.
                ColumnReference::untyped("__lambda_placeholder"),
            ],
            distinct: false,
        });
        assert_eq!(
            expr.data_type(&s),
            DataType::String,
            "aggregate should return the seed's type (String), not Array<String>",
        );
    }

    /// Numeric seed → aggregate returns numeric, not array.
    #[test]
    fn aggregate_hof_with_long_seed_returns_long() {
        let s = StructType::new(vec![StructField::nullable(
            "nums",
            DataType::Array(Box::new(DataType::Long), true),
        )]);
        let expr = Expression::FunctionCall(FunctionCall {
            name: "reduce".to_owned(),
            args: vec![
                ColumnReference::untyped("nums"),
                Expression::Literal(Literal {
                    value: LiteralValue::Long(0),
                    data_type: DataType::Long,
                }),
                ColumnReference::untyped("__lambda_placeholder"),
            ],
            distinct: false,
        });
        assert_eq!(expr.data_type(&s), DataType::Long);
    }

    // ── Pass 90 — inline_field / inline_outer_field type + nullability ──────

    /// Schema holding a single `arr : Array<Struct<name STRING?, age INT?>>`
    /// column — the canonical Pass-90 fixture.
    fn inline_test_schema(arr_contains_null: bool) -> StructType {
        let element = DataType::Struct(StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::nullable("age", DataType::Integer),
        ]));
        StructType::new(vec![StructField::nullable(
            "arr",
            DataType::Array(Box::new(element), arr_contains_null),
        )])
    }

    fn inline_field_call(arr_col: &str, field: &str, outer: bool) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: if outer {
                "inline_outer_field".to_owned()
            } else {
                "inline_field".to_owned()
            },
            args: vec![
                ColumnReference::untyped(arr_col),
                Expression::Literal(Literal {
                    value: LiteralValue::String(field.to_owned()),
                    data_type: DataType::String,
                }),
            ],
            distinct: false,
        })
    }

    /// `inline_field(arr, "name")` returns the struct field's own type.
    #[test]
    fn inline_field_data_type_is_struct_field_type() {
        let s = inline_test_schema(true);
        assert_eq!(
            inline_field_call("arr", "name", false).data_type(&s),
            DataType::String
        );
        assert_eq!(
            inline_field_call("arr", "age", false).data_type(&s),
            DataType::Integer
        );
    }

    /// Field lookup is case-insensitive (Spark's `StructType.fieldNames` uses
    /// case-insensitive resolution by default under ANSI mode).
    #[test]
    fn inline_field_data_type_case_insensitive_field_lookup() {
        let s = inline_test_schema(true);
        assert_eq!(
            inline_field_call("arr", "NAME", false).data_type(&s),
            DataType::String
        );
        assert_eq!(
            inline_field_call("arr", "AgE", false).data_type(&s),
            DataType::Integer
        );
    }

    /// `inline_outer_field` is always nullable (sentinel row is all-NULL).
    #[test]
    fn inline_outer_field_is_always_nullable() {
        // Every struct field non-nullable, containsNull=false, arr non-nullable.
        // Even with every input dimension "not null", outer still yields
        // nullable=true because the sentinel row is synthesized as all-NULL.
        let element = DataType::Struct(StructType::new(vec![
            StructField::not_null("name", DataType::String),
            StructField::not_null("age", DataType::Integer),
        ]));
        let s = StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element), false),
        )]);
        assert!(inline_field_call("arr", "name", true).nullable(&s));
        assert!(inline_field_call("arr", "age", true).nullable(&s));
    }

    /// `inline_field` nullability is the disjunction of arr nullability,
    /// arr's containsNull flag, and the struct field's own nullability.
    #[test]
    fn inline_field_nullable_propagates_from_arr() {
        // Case 1: everything not-null → not nullable.
        let element_notnull = DataType::Struct(StructType::new(vec![
            StructField::not_null("name", DataType::String),
            StructField::not_null("age", DataType::Integer),
        ]));
        let s1 = StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element_notnull.clone()), false),
        )]);
        assert!(!inline_field_call("arr", "name", false).nullable(&s1));

        // Case 2: containsNull=true → nullable.
        let s2 = StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element_notnull.clone()), true),
        )]);
        assert!(inline_field_call("arr", "name", false).nullable(&s2));

        // Case 3: struct field itself nullable → nullable.
        let element_field_null = DataType::Struct(StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::not_null("age", DataType::Integer),
        ]));
        let s3 = StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element_field_null), false),
        )]);
        assert!(inline_field_call("arr", "name", false).nullable(&s3));
        assert!(!inline_field_call("arr", "age", false).nullable(&s3));

        // Case 4: arr nullable → nullable.
        let s4 = StructType::new(vec![StructField::nullable(
            "arr",
            DataType::Array(Box::new(element_notnull), false),
        )]);
        assert!(inline_field_call("arr", "name", false).nullable(&s4));
    }

    // ── Pass 91 — json_tuple_field type + nullability ────────────────────

    fn json_tuple_field_call(json_col: &str, key: &str) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: "json_tuple_field".to_owned(),
            args: vec![
                ColumnReference::untyped(json_col),
                Expression::Literal(Literal {
                    value: LiteralValue::String(key.to_owned()),
                    data_type: DataType::String,
                }),
            ],
            distinct: false,
        })
    }

    /// `json_tuple_field(json_str, "<key>")` is always STRING per Spark's
    /// `JsonTuple.elementSchema`.
    #[test]
    fn json_tuple_field_data_type_is_string() {
        // json_str typed as String, non-null — return type STRING regardless.
        let s = StructType::new(vec![StructField::not_null("json_str", DataType::String)]);
        assert_eq!(
            json_tuple_field_call("json_str", "a").data_type(&s),
            DataType::String
        );
    }

    /// `json_tuple_field` is always nullable — missing key OR JSON null OR
    /// NULL `json_str` all yield NULL.
    #[test]
    fn json_tuple_field_is_always_nullable() {
        // Even with a non-nullable `json_str`, the field lookup can miss →
        // nullable=true.
        let s = StructType::new(vec![StructField::not_null("json_str", DataType::String)]);
        assert!(json_tuple_field_call("json_str", "a").nullable(&s));
    }

    // ── OPP-L — Expression::map_children / children walker ────────────────

    /// Constructs a nested expression exercising the shapes actual analyzer
    /// walkers care about — `Alias > FunctionCall > Binary > CaseWhen >
    /// Literal` — and verifies `children()` reaches each immediate leaf
    /// exactly once, and `map_children` visits the same immediate children
    /// exactly once (identity mapping preserves the tree structure).
    #[test]
    fn map_children_visits_each_immediate_child_exactly_once() {
        // Alias(FunctionCall("f", [Binary(Add, CaseWhen(when=Lit(1), then=Lit(2)) else=Lit(3), Lit(4))]))
        let leaf_lit_1 = Expression::Literal(Literal {
            value: LiteralValue::Int(1),
            data_type: DataType::Integer,
        });
        let leaf_lit_2 = Expression::Literal(Literal {
            value: LiteralValue::Int(2),
            data_type: DataType::Integer,
        });
        let leaf_lit_3 = Expression::Literal(Literal {
            value: LiteralValue::Int(3),
            data_type: DataType::Integer,
        });
        let leaf_lit_4 = Expression::Literal(Literal {
            value: LiteralValue::Int(4),
            data_type: DataType::Integer,
        });
        let case_when = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(leaf_lit_1, leaf_lit_2)],
            else_expr: Some(Box::new(leaf_lit_3)),
        });
        let binary = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(case_when),
            right: Box::new(leaf_lit_4),
        });
        let fcall = Expression::FunctionCall(FunctionCall {
            name: "f".to_owned(),
            args: vec![binary],
            distinct: false,
        });
        let alias = Expression::Alias(AliasExpression {
            expr: Box::new(fcall),
            alias: "x".to_owned(),
        });

        // Alias has exactly one immediate child (the FunctionCall).
        let children: Vec<&Expression> = alias.children().collect();
        assert_eq!(
            children.len(),
            1,
            "Alias should have exactly 1 immediate child (its inner expr)"
        );
        assert!(matches!(children[0], Expression::FunctionCall(_)));

        // map_children on Alias should visit its one immediate child exactly
        // once with the identity mapping and return an equal tree.
        let mut visits = 0usize;
        let mapped = alias
            .clone()
            .map_children(|e| {
                visits += 1;
                Ok::<_, ()>(e)
            })
            .expect("identity mapping cannot fail");
        assert_eq!(
            visits, 1,
            "Alias::map_children should visit its immediate child exactly once"
        );
        assert_eq!(
            mapped, alias,
            "identity map_children must preserve structure"
        );

        // Recursive full-tree visit via a manual walker built on `map_children`.
        // Verify the total leaf count (4 literals + 0 leaves visited by
        // Alias/FunctionCall/Binary/CaseWhen internal recursion — each of
        // those is an interior node) sums correctly.
        fn count_leaves(e: &Expression) -> usize {
            match e {
                Expression::Literal(_) => 1,
                _ => e.children().map(count_leaves).sum(),
            }
        }
        assert_eq!(
            count_leaves(&alias),
            4,
            "the tree contains exactly 4 leaf Literal nodes"
        );
    }

    /// `Window::children` must reach `func + partition_by + order_by.expr`
    /// but NOT into the frame boundary expressions (τ walker convention —
    /// see [`Expression::children`] doc).
    #[test]
    fn window_children_skip_frame_boundary_expressions() {
        let win = Expression::Window(WindowFunction {
            func: Box::new(ColumnReference::untyped("a")),
            partition_by: vec![ColumnReference::untyped("b")],
            order_by: vec![SortOrder {
                expr: Box::new(ColumnReference::untyped("c")),
                direction: SortDirection::Ascending,
                null_ordering: NullOrdering::NullsLast,
            }],
            frame: Some(WindowFrame {
                unit: FrameUnit::Rows,
                lower: FrameBoundary::Preceding(Box::new(Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }))),
                upper: FrameBoundary::CurrentRow,
            }),
        });
        let children: Vec<&Expression> = win.children().collect();
        // Exactly three children: func, one partition_by, one order_by.expr.
        assert_eq!(children.len(), 3);
    }
}
