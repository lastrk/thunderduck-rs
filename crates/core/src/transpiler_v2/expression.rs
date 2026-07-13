//! τ's Expression enum — Spark 4.1.1 parity.
//!
//! **INV10:** this file imports ONLY from `crate::types` (`DataType`,
//! `StructField`, `StructType`) plus intra-τ modules. No `crate::expression`,
//! `crate::logical`, `crate::generator`, `crate::functions`, or
//! `crate::types::TypeInferenceEngine`.

use super::analyzer::TypedAst;
use super::ast::CommonAst;
use super::schema::{ExprId, ResolvedSchema};
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

/// Extract a borrowed string value from a string [`Literal`] expression.
///
/// Returns `None` for any non-literal or non-string expression. Used by the
/// literal-keyed inference arms (`named_struct`, `to_number`, `from_json`,
/// `from_csv`, `inline_field`) and the `ExtractValue` field-name lookups.
pub(super) fn as_string_literal(e: &Expression) -> Option<&str> {
    match e {
        Expression::Literal(Literal {
            value: LiteralValue::String(s),
            ..
        }) => Some(s.as_str()),
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

/// Value-derived `(precision, scale)` of a decimal literal string, mirroring
/// Apache Spark's `Decimal.set(BigDecimal)`: `scale` is the fractional-digit
/// count; `precision` is significant integer digits (sign and leading zeros
/// excluded) plus `scale`, floored at `max(scale, 1)` — Spark bumps
/// `_precision` up to `max(bigDecimal.precision, bigDecimal.scale)` and never
/// below 1. `100.25`→(5,2); `3.142`→(4,3); `0.00`→(2,2).
///
/// **Unclamped** — no `MAX_PRECISION = 38` cap. Callers own their remaining
/// steps: `parser_v2::v2_lowering::decimal_literal_precision_scale` clamps to
/// 38; the connect-server's `normalize_decimal_literal` reconciles against
/// the wire-supplied shape before clamping.
pub fn decimal_value_precision_scale(s: &str) -> (u8, u8) {
    let trimmed = s.trim_start_matches(['+', '-']);
    let (int_part, frac_part) = match trimmed.split_once('.') {
        Some((i, f)) => (i, f),
        None => (trimmed, ""),
    };
    let raw_int_digits = int_part
        .trim_start_matches('0')
        .chars()
        .filter(|c| c.is_ascii_digit())
        .count() as u8;
    let scale = frac_part.chars().filter(|c| c.is_ascii_digit()).count() as u8;
    let precision = raw_int_digits.saturating_add(scale).max(scale).max(1);
    (precision, scale)
}

/// A resolved column reference with schema-recorded type/nullability info.
#[derive(Debug, Clone)]
pub struct ColumnReference {
    pub name: String,
    pub qualifier: Option<String>,
    pub data_type: Option<DataType>,
    pub nullable: Option<bool>,
    /// N9 increment 2 / ADR-024: the [`super::schema::ExprId`] of the
    /// [`super::schema::Attribute`] this reference resolved to — the
    /// PRODUCING node's own output schema for a local reference (`ctx.schema`
    /// at resolution time), or the ENCLOSING plan's output schema for a
    /// correlated outer reference (D2 — `resolve_column`'s tier-(g) arm /
    /// `resolve_in_outer` in `analyzer.rs`). `None` pre-analysis (`untyped`,
    /// never resolved), and on a RESOLVED (`data_type: Some`) reference only
    /// from the analyzer paths left open after D2 (`analyzer.rs`):
    /// * tier-(d) in `resolve_column` and its outer twin, the
    ///   struct-qualifier arm of `resolve_in_outer` — a qualifier naming a
    ///   top-level STRUCT column resolves to a nested FIELD's type, which
    ///   has no attribute identity of its own to stamp (only the struct
    ///   COLUMN does; Spark's `ExtractValue` likewise keeps the child's
    ///   `exprId` on the child only).
    /// * `derive_implicit_grouping` (SQL `PIVOT` with no explicit grouping
    ///   list) and the root reference `try_rewrite_nested_struct_path` builds
    ///   for a multi-level nested-struct `ExtractValue` chain: both DO
    ///   resolve to a real top-level attribute with an id available at
    ///   construction, but a pre-existing gap leaves it unstamped — out of
    ///   this pass's scope, left for a follow-up.
    ///
    /// Derived resolution data recording *which* attribute the reference
    /// bound to, not part of the reference's own logical identity — excluded
    /// from `PartialEq` (see below).
    pub expr_id: Option<ExprId>,
}

impl PartialEq for ColumnReference {
    /// Excludes `expr_id`: a derived resolution fact recording *which*
    /// attribute a column resolved to, not part of the reference's logical
    /// identity. Mirrors `TypedAst`'s scope-excluding `Eq` — keeps every
    /// pre-existing equality-based test unchanged.
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.qualifier == other.qualifier
            && self.data_type == other.data_type
            && self.nullable == other.nullable
    }
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
    /// analyzer-materialized coercion (N4): transparent to output naming and
    /// semantic_eq; never set by front-ends.
    pub implicit: bool,
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

/// Semantic kind of an interval literal. The `(months, days, microseconds)`
/// triple cannot disambiguate Spark's ANSI interval types (a bare month count
/// is ambiguous between `YearMonthInterval` and generic `CalendarInterval`), so
/// the kind is carried explicitly. This is emission-invisible (DuckDB has one
/// `INTERVAL` type); it only steers [`Expression::data_type`] so the wire
/// schema surfaces the correct Spark type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntervalKind {
    /// Spark `YearMonthIntervalType` — compound `YEAR TO MONTH` literals.
    YearMonth,
    /// Spark `DayTimeIntervalType` — compound `DAY TO SECOND` literals.
    DayTime,
    /// Spark `CalendarIntervalType` — single-field / generic interval literals.
    Calendar,
}

/// Interval literal (year-month or day-time, or generic Interval).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IntervalExpression {
    pub months: i32,
    pub days: i32,
    pub microseconds: i64,
    /// Semantic kind — emission-invisible; steers `data_type()`. See
    /// [`IntervalKind`].
    pub kind: IntervalKind,
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

/// Immediate-child classification shared by [`Expression::children`] and
/// [`Expression::children_mut`] — ONE exhaustive match (no `_` arm), so adding
/// an `Expression` variant fails to compile here until it is classified.
/// Parameterized on the borrow flavor: `as_ref`/`iter`/`as_deref` for the
/// shared walker, `as_mut`/`iter_mut`/`as_deref_mut` for the mutable one (the
/// deref pair exists for `CaseWhen.else_expr`'s `Option<Box<Expression>>`).
macro_rules! expression_children {
    ($expr:expr, $as_child:ident, $iter:ident, $as_deref:ident) => {
        match $expr {
            Expression::Literal(_)
            | Expression::ColumnReference(_)
            | Expression::UnresolvedColumn(_)
            | Expression::UnresolvedRegex(_)
            | Expression::Star(_)
            | Expression::LambdaVariable(_)
            | Expression::RawSql(_)
            | Expression::Interval(_)
            | Expression::ExistsSubquery(_)
            | Expression::ScalarSubquery(_) => Box::new(std::iter::empty()),

            Expression::Unary(u) => Box::new(std::iter::once(u.operand.$as_child())),
            Expression::Cast(c) => Box::new(std::iter::once(c.expr.$as_child())),
            Expression::Alias(a) => Box::new(std::iter::once(a.expr.$as_child())),
            Expression::Lambda(l) => Box::new(std::iter::once(l.body.$as_child())),
            Expression::InSubquery(i) => Box::new(std::iter::once(i.expr.$as_child())),

            Expression::Binary(b) => {
                Box::new([b.left.$as_child(), b.right.$as_child()].into_iter())
            }
            Expression::Like(l) => {
                Box::new([l.value.$as_child(), l.pattern.$as_child()].into_iter())
            }
            Expression::IsDistinctFrom(d) => {
                Box::new([d.left.$as_child(), d.right.$as_child()].into_iter())
            }
            Expression::ExtractValue(ev) => {
                Box::new([ev.child.$as_child(), ev.extraction.$as_child()].into_iter())
            }
            Expression::Between(b) => {
                Box::new([b.expr.$as_child(), b.low.$as_child(), b.high.$as_child()].into_iter())
            }

            Expression::FunctionCall(f) => Box::new(f.args.$iter()),
            Expression::ArrayLiteral(a) => Box::new(a.elements.$iter()),
            Expression::RowConstructor(rc) => Box::new(rc.elements.$iter()),

            Expression::CaseWhen(cw) => Box::new(
                cw.branches
                    .$iter()
                    .flat_map(|(w, t)| [w, t])
                    .chain(cw.else_expr.$as_deref()),
            ),
            Expression::InList(i) => {
                Box::new(std::iter::once(i.expr.$as_child()).chain(i.list.$iter()))
            }
            Expression::MapLiteral(m) => Box::new(m.entries.$iter().flat_map(|(k, v)| [k, v])),
            Expression::StructLiteral(s) => Box::new(s.fields.$iter().map(|(_, e)| e)),

            Expression::Window(w) => Box::new(
                std::iter::once(w.func.$as_child())
                    .chain(w.partition_by.$iter())
                    .chain(w.order_by.$iter().map(|so| so.expr.$as_child())),
            ),
            Expression::UpdateFields(u) => Box::new(
                std::iter::once(u.struct_expr.$as_child())
                    .chain(u.updates.$iter().filter_map(|(_, e)| e.$as_child())),
            ),
        }
    };
}

impl Expression {
    /// Strip a single outer `Alias` wrapper, returning the inner expression
    /// (`self` for every non-alias variant). Used where an expression must
    /// act bare (GROUP BY keys, structural comparisons, function-argument
    /// values) while its alias only contributes an output name elsewhere.
    pub fn unaliased(&self) -> &Expression {
        match self {
            Expression::Alias(a) => &a.expr,
            other => other,
        }
    }

    /// The Spark-compatible data type of this expression given the input schema.
    pub fn data_type(&self, schema: &ResolvedSchema) -> DataType {
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
            Expression::Interval(i) => match i.kind {
                IntervalKind::YearMonth => DataType::YearMonthInterval,
                IntervalKind::DayTime => DataType::DayTimeInterval,
                IntervalKind::Calendar => DataType::Interval,
            },
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
    pub fn nullable(&self, schema: &ResolvedSchema) -> bool {
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
                // Intended coupling: string → numeric/temporal casts may
                // fail in ANSI mode — every numeric type plus Date/Timestamp.
                let dst_may_fail = c.to_type.is_numeric()
                    || matches!(
                        c.to_type,
                        DataType::Date | DataType::Timestamp | DataType::TimestampNtz
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
        expression_children!(self, as_ref, iter, as_deref)
    }

    /// Mutable counterpart of [`Self::children`] — iterates over the SAME
    /// immediate child expressions in the SAME order (arm-for-arm parity;
    /// window-frame boundaries and subquery bodies excluded per the τ
    /// walker convention documented on `children`).
    pub fn children_mut(&mut self) -> Box<dyn Iterator<Item = &mut Expression> + '_> {
        expression_children!(self, as_mut, iter_mut, as_deref_mut)
    }

    /// Structural map: apply `f` to each immediate child expression,
    /// preserving the node's structure.
    ///
    /// **Same child set and visit order as [`Self::children`]** (see its
    /// docstring for the window-frame + subquery-body conventions).
    ///
    /// Transform walkers built on `map_children` should custom-case
    /// variants that must be opaque to the transform: the subquery variants
    /// (`InSubquery`, `ExistsSubquery`, `ScalarSubquery`) plus whatever
    /// [`Self::is_opaque_unit`] reports (τ's `resolve_and_stamp`, for
    /// example, treats all of these as passthrough — matching semantics
    /// preserved when `map_children` is called as the default arm).
    pub fn map_children<E>(
        mut self,
        mut f: impl FnMut(Expression) -> Result<Expression, E>,
    ) -> Result<Expression, E> {
        for slot in self.children_mut() {
            // Cheap placeholder to take ownership of the child; overwritten
            // immediately (or the whole node is dropped on error).
            let child =
                std::mem::replace(slot, Expression::Star(StarExpression { qualifier: None }));
            *slot = f(child)?;
        }
        Ok(self)
    }

    /// **N1 — single opacity authority.** `true` for the variant core every
    /// resolution walker in the analyzer treats as an opaque, atomic unit
    /// that must not be recursed into or rewritten in place: `Lambda`
    /// (analyzed lazily by its consumer function — its body closes over its
    /// own bound `LambdaVariable`, not the walker's outer scope),
    /// `LambdaVariable` (a leaf bound by its enclosing `Lambda`, never a
    /// column reference some other walker should rewrite), `RawSql`, and
    /// `Interval` (both already carry their own final type, never derived by
    /// a walker).
    ///
    /// This is the shared CORE only. Some walkers are opaque to strictly
    /// more variants for reasons specific to that walk — the two resolution
    /// walkers use [`Expression::is_resolve_opaque`] (core +
    /// `UnresolvedRegex`); `opaque_to_subtree_promotion` ORs on `Window`
    /// plus the subquery variants at the site, with a comment explaining
    /// the delta. Never hand-copy the whole roster.
    pub(crate) fn is_opaque_unit(&self) -> bool {
        matches!(
            self,
            Expression::Lambda(_)
                | Expression::LambdaVariable(_)
                | Expression::RawSql(_)
                | Expression::Interval(_)
        )
    }

    /// The opacity set of the two RESOLUTION walkers (`resolve_and_stamp`,
    /// `substitute_lateral_aliases`): the shared core plus `UnresolvedRegex`,
    /// which both pass through opaquely (Pass 85's `expand_regex_projections`
    /// rewrites it before it reaches either walker; residuals surface via
    /// emission's defensive arm). One authority for the pair so the extra
    /// cannot drift between them.
    pub(crate) fn is_resolve_opaque(&self) -> bool {
        self.is_opaque_unit() || matches!(self, Expression::UnresolvedRegex(_))
    }

    // ── Binary data-type derivation ──────────────────────────────────────────

    fn binary_data_type(b: &BinaryExpression, schema: &ResolvedSchema) -> DataType {
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
        if let Some(dt) = date_like_interval_result(&b.op, &l, &r) {
            return dt;
        }
        // Timestamp − Timestamp → DayTimeInterval (Spark 4.1 parity —
        // corpus intv-005). NTZ mixes coerce to DayTimeInterval too.
        if b.op == BinaryOp::Sub {
            match (&l, &r) {
                (DataType::Timestamp, DataType::Timestamp)
                | (DataType::TimestampNtz, DataType::TimestampNtz)
                | (DataType::Timestamp, DataType::TimestampNtz)
                | (DataType::TimestampNtz, DataType::Timestamp) => {
                    return DataType::DayTimeInterval;
                }
                _ => {}
            }
        }
        // Spark `div` (IntegralDivide) is LongType regardless of operand types
        // (`spark.sql.legacy.integralDivide.returnLong` defaults true). Resolve
        // it BEFORE the decimal block below, so a decimal operand does not drag
        // it into the decimal-arithmetic formulas (which would wrongly yield a
        // Decimal / non-floored value).
        if b.op == BinaryOp::IntDiv {
            return DataType::Long;
        }
        // Decimal-aware arithmetic. Resolve each side to a `(precision,
        // scale)` if it is a `Decimal`; if exactly one side is and the other
        // is integral (a column, an expression, or an integer literal),
        // coerce that other side to a decimal form too — Spark's
        // `DecimalPrecision` cast-then-apply-formula rule — rather than
        // falling through to `promote_numeric`'s union-widening below.
        let (lop, rop) = decimal_widen_operands(b, &l, &r);
        let lp = lop.map(DecimalOperand::parts);
        let rp = rop.map(DecimalOperand::parts);
        if let (Some((p1, s1)), Some((p2, s2))) = (lp, rp) {
            return match b.op {
                BinaryOp::Add | BinaryOp::Sub => {
                    TypeInferenceEngine::decimal_add_type(p1, s1, p2, s2)
                }
                BinaryOp::Mul => TypeInferenceEngine::decimal_mul_type(p1, s1, p2, s2),
                BinaryOp::Div => TypeInferenceEngine::decimal_div_type(p1, s1, p2, s2),
                BinaryOp::Mod => TypeInferenceEngine::decimal_mod_type(p1, s1, p2, s2),
                _ => TypeInferenceEngine::promote_numeric(&l, &r),
            };
        }
        if b.op == BinaryOp::Div {
            // Spark int/int → Double.
            if l.is_integral() && r.is_integral() {
                return DataType::Double;
            }
        }
        TypeInferenceEngine::promote_numeric(&l, &r)
    }

    /// Coerce a decimal-arithmetic operand's expression/type to a
    /// `(precision, scale)` pair per Spark's `DecimalPrecision` rule: an
    /// integer-literal operand casts to `DecimalType.fromLiteral`'s minimal
    /// precision (see [`Self::integer_literal_decimal`]); any other integral
    /// operand casts to `DecimalType.forType`
    /// (`TypeInferenceEngine::decimal_form`). Non-integral operands (Float,
    /// Double, ...) return `None` — those stay on the `promote_numeric` path
    /// (Spark: decimal ⊗ double → double).
    fn decimalize(expr: &Expression, dt: &DataType) -> Option<(u8, u8)> {
        Self::integer_literal_decimal(expr).or_else(|| {
            if dt.is_integral() {
                TypeInferenceEngine::decimal_form(dt)
            } else {
                None
            }
        })
    }

    /// Spark `DecimalType.fromLiteral`'s minimal precision for an integer
    /// literal expression: `(max(1, digit_count(|value|)), 0)`, capped at 38.
    /// `digit_count` is computed from the exact integer value (integer→string
    /// length, never floating-point `log`). Returns `None` for anything but a
    /// `Literal` with an integral `LiteralValue` (`Byte`/`Short`/`Int`/`Long`).
    fn integer_literal_decimal(expr: &Expression) -> Option<(u8, u8)> {
        let value = Self::const_int_index(expr)?;
        let digits = value.unsigned_abs().to_string().len().clamp(1, 38) as u8;
        Some((digits, 0))
    }

    // ── FunctionCall data-type derivation ────────────────────────────────────

    fn function_call_data_type(f: &FunctionCall, schema: &ResolvedSchema) -> DataType {
        // Pre-pass for return types that need the argument EXPRESSIONS —
        // literal schemas, literal scales, struct field naming, per-arg
        // nullability. `TypeInferenceEngine::function_return_type` sees only
        // the argument TYPES, so it cannot express these; derive here where
        // the full `&FunctionCall` is available. Rules that depend on
        // argument types alone live in `function_return_type` (single home).
        //
        // Struct-constructor fast-paths — Spark's `struct` / `named_struct`
        // return a `DataType::Struct` whose field names depend on the shape
        // of the argument tree.
        // Symmetric with emission's `struct` / `named_struct` arms.
        // N5: `f.name` is already canonical lowercase — no local re-derivation.
        match f.name.as_str() {
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
                if !f.args.is_empty() && f.args.len().is_multiple_of(2) {
                    let fields: Option<Vec<StructField>> = f
                        .args
                        .chunks_exact(2)
                        .map(|kv| {
                            as_string_literal(&kv[0]).map(|key| {
                                StructField::new(
                                    key,
                                    kv[1].data_type(schema),
                                    kv[1].nullable(schema),
                                )
                            })
                        })
                        .collect();
                    if let Some(fields) = fields {
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
                let names: Vec<String> = f
                    .args
                    .iter()
                    .enumerate()
                    .map(|(i, arg)| super::struct_names::derive_zip_field_name(arg, i))
                    .collect();
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
            // Spark's `array(a, b, ...)` — element type is the least-common
            // (widening) type of the args. First-arg-only inference misses
            // the mixed-numeric case (e.g., `array(1, 2.0, 3)` → Double).
            // Corpus anchor: `type-020`.
            "array" | "list_value" | "make_array" | "list" if !f.args.is_empty() => {
                let mut acc = f.args[0].data_type(schema);
                for a in f.args.iter().skip(1) {
                    let dt = a.data_type(schema);
                    // Spark's CreateArray element type = findWiderCommonType
                    // over the args (τ's `unify_types`), NOT numeric-only
                    // promotion — so heterogeneous non-numeric args (e.g.
                    // `array(1, 'x')` → Array<String>) widen correctly.
                    acc = TypeInferenceEngine::unify_types(&acc, &dt);
                }
                // Spark reports the array as `containsNull` = any element
                // nullable. Result nullability is handled separately in
                // `function_call_nullable`; here we just carry the flag
                // conservatively as `true` (any-null-permitted) matching
                // the shared resolver's behavior.
                let contains_null = f.args.iter().any(|a| a.nullable(schema));
                return DataType::Array(Box::new(acc), contains_null);
            }
            // Spark's `map(k1, v1, k2, v2, ...)` / `create_map(...)` — key type
            // is the least-common type of the even-index args, value type the
            // least-common type of the odd-index args, and `valueContainsNull`
            // is true iff any value arg is nullable. The shared
            // `function_return_type` resolver only sees the first arg and so
            // hard-codes `Map<String, String>`; derive the real key/value types
            // here where the whole arg list is available. Corpus anchor:
            // cx-002. An empty or odd-length arg list falls through to the
            // shared resolver.
            "map" | "create_map" if !f.args.is_empty() && f.args.len().is_multiple_of(2) => {
                let mut key_ty = f.args[0].data_type(schema);
                let mut val_ty = f.args[1].data_type(schema);
                let mut value_nullable = f.args[1].nullable(schema);
                let mut i = 2;
                while i < f.args.len() {
                    // Spark's CreateMap key/value types = findWiderCommonType
                    // (τ's `unify_types`), NOT numeric-only promotion — so
                    // heterogeneous non-numeric args (e.g.
                    // `map('a', 1, 'b', 'x')` → Map<String, String>) widen
                    // correctly. The homogeneous cx-002 result is preserved.
                    key_ty =
                        TypeInferenceEngine::unify_types(&key_ty, &f.args[i].data_type(schema));
                    val_ty =
                        TypeInferenceEngine::unify_types(&val_ty, &f.args[i + 1].data_type(schema));
                    value_nullable = value_nullable || f.args[i + 1].nullable(schema);
                    i += 2;
                }
                return DataType::Map {
                    key: Box::new(key_ty),
                    value: Box::new(val_ty),
                    value_nullable,
                };
            }
            // `map_from_arrays(keys, values)` needs only the two args'
            // DataTypes (key elem type + value elem type/containsNull all
            // live inside `DataType::Array`'s own shape) — no per-arg
            // *expression* nullability is required. That makes
            // `function_return_type` a sufficient home; the rule now lives
            // there as the resolver's map arm (N2 single-home). Differential:
            // `test_map_from_arrays`.
            //
            // Spark's `to_number(str, fmt)` / `try_to_number(str, fmt)` return
            // DECIMAL(p, s) derived from the format string. Emission parses
            // the same format literal to build the CAST; mirror the
            // precision/scale derivation here so the projection schema
            // matches Spark. When the format is not a literal or not a
            // recognized digit template, this falls through to the shared
            // resolver, which has no `to_number` arm — the result is
            // `Unresolved` (an honest ADR-022 boundary, not a String).
            // Corpus anchor: `parse-004`.
            "to_number" | "try_to_number" if f.args.len() == 2 => {
                if let Some(fmt) = as_string_literal(&f.args[1]) {
                    if let Some((precision, scale)) =
                        super::emission::parse_number_format_for_type_inference(fmt)
                    {
                        return DataType::Decimal { precision, scale };
                    }
                }
            }
            // Spark's `from_json(json_str, ddl_schema)` and
            // `from_csv(csv_str, ddl_schema)` return a Struct typed per the
            // DDL literal (from_csv: flat primitives only — Spark's own
            // surface). Mirror emission's DDL translation for type inference
            // so the projection schema matches Spark; only the DDL helper
            // differs between the two. Corpus anchors: `json-003`,
            // `json-004` (from_json); `json-007` (from_csv).
            name @ ("from_json" | "from_csv") if f.args.len() == 2 => {
                if let Some(ddl) = as_string_literal(&f.args[1]) {
                    let st = if name == "from_json" {
                        super::emission::from_json_ddl_to_struct_for_type_inference(ddl)
                    } else {
                        super::emission::from_csv_ddl_to_struct(ddl)
                    };
                    if let Some(st) = st {
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
                if let Some(field_name) = as_string_literal(&f.args[1]) {
                    if let DataType::Array(inner, _) = f.args[0].data_type(schema) {
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
            // `round(x[, scale])` / `bround(x[, scale])` share Spark's
            // `RoundBase.dataType`: for a Decimal child the scale (and
            // precision) decrease per the literal target scale; a non-decimal
            // child keeps its type unchanged (`case t => t`), independent of the
            // scale argument. The shared `function_return_type` resolver only
            // sees the first arg's type and cannot read the scale literal, so
            // derive the Decimal branch here. A missing 2nd arg ⇒ scale 0; a
            // non-literal scale on a Decimal child is a Thunderduck boundary →
            // `Unresolved`. The Decimal branch is byte-identical to
            // `ceil_floor_type(input, Some(scale))`. Corpus: `num-005`, `num-006`.
            "round" | "bround" if !f.args.is_empty() => {
                let input = f.args[0].data_type(schema);
                if !matches!(input, DataType::Decimal { .. }) {
                    // Non-decimal child keeps its type regardless of the scale
                    // form (e.g. `round(sml, -1)` where `-1` is a unary-minus
                    // expression, not an integer literal).
                    return input;
                }
                let scale = match f.args.get(1) {
                    Some(a) => int_literal_value(a),
                    None => Some(0),
                };
                return match scale {
                    Some(t) => TypeInferenceEngine::ceil_floor_type(&input, Some(t)),
                    None => DataType::Unresolved,
                };
            }
            _ => {}
        }
        // All remaining return-type rules depend on argument TYPES alone
        // (widening folds, arity-branch selection, decimal widening) — the
        // single home is `TypeInferenceEngine::function_return_type`, which now
        // receives the full argument-type list.
        let arg_types: Vec<DataType> = f.args.iter().map(|a| a.data_type(schema)).collect();
        TypeInferenceEngine::function_return_type(&f.name, &arg_types)
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
    fn is_non_nullable_function_name_lower(name_lower: &str) -> bool {
        debug_assert!(
            name_lower.chars().all(|c| !c.is_ascii_uppercase()),
            "is_non_nullable_function_name_lower requires pre-lowercased input; got `{name_lower}`",
        );
        // Non-nullable aggregates come from the AGG_SPECS table; the hash
        // family (checklist §1.2) is the only non-aggregate addition.
        TypeInferenceEngine::aggregate_is_non_nullable_lower(name_lower)
            || matches!(name_lower, "hash" | "murmur3" | "xxhash64")
    }

    fn function_call_nullable(f: &FunctionCall, schema: &ResolvedSchema) -> bool {
        // N5: `f.name` is already canonical lowercase; `lower` is kept as an
        // owned `String` (rather than renamed to a borrow) purely so the
        // match below, which threads it through several `_lower`-suffixed
        // fast-path calls, needs no further edits.
        let lower = f.name.clone();
        if Self::is_non_nullable_function_name_lower(&lower) {
            return false;
        }
        if TypeInferenceEngine::aggregate_is_always_nullable_lower(&lower) {
            return true;
        }
        match lower.as_str() {
            "coalesce" | "ifnull" | "nvl" | "greatest" | "least" => {
                f.args.iter().all(|a| a.nullable(schema))
            }
            "when" => {
                if f.args.len().is_multiple_of(2) {
                    true
                } else {
                    let then_nullable =
                        f.args.iter().skip(1).step_by(2).any(|a| a.nullable(schema));
                    let else_nullable = f.args.last().is_some_and(|a| a.nullable(schema));
                    then_nullable || else_nullable
                }
            }
            // ── Always non-nullable scalars — constant `false` regardless of
            // arg nullability ────────────────────────────────────────────────
            // * `isnull` / `isnan` / `isnotnull` / `isnotnan` / `is_nan` /
            //   `isinf`, `concat_ws`, `typeof` / `spark_partition_id` /
            //   `monotonically_increasing_id`.
            // * `format_string` / `printf` — Spark returns non-nullable; NULL
            //   args render as the literal string "null" rather than
            //   propagating NULL. Corpus witness: `str-015`.
            // * `array` / `make_array` / `create_map` / `map` /
            //   `named_struct` / `struct` — constructors are never NULL
            //   themselves (only elements / fields carry nullability).
            // * `window` — `F.window(ts, dur)`: Spark's `TimeWindow` is
            //   rewritten by the analyzer into
            //   `CreateNamedStruct(start := ..., end := ...)` whose
            //   `nullable = false` (Spark's struct-construction is never
            //   null; only the fields inside carry per-field nullability).
            //   τ's default `any(arg.nullable)` fallback would incorrectly
            //   propagate the timestamp arg's nullable into the struct
            //   itself; pin `false` here to match Spark's observable schema.
            //   Corpus: `win2-002`.
            // * `posexplode_pos` — the position column is a synthetic
            //   0-indexed integer, never NULL. Non-nullable regardless of the
            //   input array's nullability. Corpus: arr-017.
            // * `map_explode_key` — synthetic per-column call (map-007; see
            //   the v2 relation converter's alias-splitter). Spark's
            //   `explode(map)` produces `(key, value)` rows where keys are
            //   ALWAYS non-nullable (Spark's MAP invariant); a NULL map arg
            //   emits zero rows, so a nullable outer map does not propagate
            //   to the key column. (Values inherit the map's
            //   `valueContainsNull` flag — see the data-dependent
            //   `map_explode_val` arm below.)
            "isnull" | "isnan" | "isnotnull" | "isnotnan" | "is_nan" | "isinf" | "concat_ws"
            | "format_string" | "printf" | "typeof" | "spark_partition_id"
            | "monotonically_increasing_id" | "array" | "make_array" | "create_map" | "map"
            | "named_struct" | "struct" | "window" | "posexplode_pos" | "map_explode_key" => false,
            // ── Always nullable scalars — constant `true` regardless of arg
            // nullability ────────────────────────────────────────────────────
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
            | "to_json" | "to_csv"
            // `explode_outer(arr)` — always nullable: empty / NULL arrays
            // emit exactly one row with a NULL value. Corpus: arr-016.
            | "explode_outer"
            // `inline_outer_field` — always nullable: the empty / NULL array
            // sentinel row is all-NULL by construction. Mirrors
            // `explode_outer` in this same list. Corpus: inl-002.
            | "inline_outer_field"
            // Pass 91 — synthetic `json_tuple_field(json, "<key>")` produced
            // by the analyzer's Project pre-pass (`expand_json_tuple_projections`).
            // Always nullable — Spark returns NULL for missing key OR JSON
            // null value OR NULL `json_str`. Corpus: json-002.
            | "json_tuple_field"
            // Spark's `flatten(Array<Array<T>>)` returns NULL if the outer
            // array contains any NULL inner array. Even a non-nullable
            // literal outer array (`F.array(...)`) produces a nullable
            // result per Spark's schema semantics. Corpus: `arr-013`.
            | "flatten" | "list_flatten"
            // piv-006 — synthetic per-column call from
            // `expand_stack_projections`. Spark's `Stack.elementSchema`
            // pins every output column to `nullable = true` regardless
            // of whether the individual row-values are non-null literals.
            | "stack_col"
            // Spark's `ArrayAggregate.nullable` = `argument.nullable ||
            // finish.nullable`. In `bindInternal` the accumulator variable
            // is bound with `nullable = true` (hardcoded), so
            // `finish.nullable()` is always `true` — making the overall
            // result always nullable. Applies to `aggregate`, `reduce`,
            // and `list_reduce` HOFs.
            // Corpus: lambda aggregate_sum / aggregate_product /
            // aggregate_with_init / sql_aggregate / sql_aggregate_product.
            | "aggregate" | "reduce" | "list_reduce"
            // `ceil`/`ceiling`/`floor` (Spark's `Ceil`/`Floor`) declare
            // `nullable = true` unconditionally — the Double→Long widening
            // can overflow for inputs outside `Long`'s range, so Spark's
            // static schema is conservative regardless of the child's own
            // nullability. `round`/`bround` (`RoundBase`) follow the same
            // unconditional-`true` rule. Verified against Apache Spark
            // 4.1.1: `ceil(non_null_double)` / `floor(...)` /
            // `round(non_null_double, n)` / `bround(...)` all report
            // `nullable=True` on a non-nullable input column.
            // Differential: TestMathFunctions::test_ceil_floor, test_round.
            | "ceil" | "ceiling" | "floor" | "round" | "bround"
            // `exp`/`ln`/`log`/`log10`/`log2` (Spark's `UnaryMathExpression`
            // family) also declare `nullable = true` unconditionally — the
            // domain guard above (`x <= 0 -> NULL`) and NaN-to-NULL
            // conversion mean even a non-nullable input can still produce
            // NULL. Verified against Apache Spark 4.1.1.
            // Differential: TestMathFunctions::test_exp, test_log.
            | "exp" | "ln" | "log" | "log10" | "log2" => true,
            // Spark's `If.nullable = trueValue.nullable || falseValue.nullable`
            // — the predicate (args[0]) is excluded. `iif` is a Spark alias for
            // `If`, and `nvl2(cond, ifNotNull, ifNull)` shares the same
            // branch-only nullability rule. Corpus witness: cnd-009.
            "nvl2" | "if" | "iif" => {
                f.args.get(1).is_none_or(|a| a.nullable(schema))
                    || f.args.get(2).is_none_or(|a| a.nullable(schema))
            }
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
            "inline_field" => match (f.args.first(), f.args.get(1).and_then(as_string_literal)) {
                (Some(arr), Some(field_name)) => {
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
            // Synthetic `map_explode_val(m)` (map-007) — values inherit the
            // map's `valueContainsNull` flag. (Its `map_explode_key` sibling
            // is pinned non-nullable in the constant-`false` list above.)
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
            _ => f.args.iter().any(|a| a.nullable(schema)),
        }
    }

    // ── CaseWhen data-type unification ───────────────────────────────────────

    fn case_when_data_type(cw: &CaseWhenExpression, schema: &ResolvedSchema) -> DataType {
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

    fn window_data_type(w: &WindowFunction, schema: &ResolvedSchema) -> DataType {
        match w.func.as_ref() {
            Expression::FunctionCall(f) => {
                let first_arg_type = f.args.first().map(|a| a.data_type(schema));
                TypeInferenceEngine::window_return_type(&f.name, first_arg_type.as_ref())
            }
            other => other.data_type(schema),
        }
    }

    fn window_nullable(w: &WindowFunction, schema: &ResolvedSchema) -> bool {
        match w.func.as_ref() {
            Expression::FunctionCall(f) => {
                if TypeInferenceEngine::window_is_non_nullable(&f.name) {
                    false
                } else if matches!(f.name.as_str(), "lag" | "lead") {
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

    fn extract_value_data_type(ev: &ExtractValueExpression, schema: &ResolvedSchema) -> DataType {
        let base_type = ev.child.data_type(schema);
        let field_name = as_string_literal(ev.extraction.as_ref());
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

    fn extract_value_nullable(ev: &ExtractValueExpression, schema: &ResolvedSchema) -> bool {
        let base_nullable = ev.child.nullable(schema);
        let base_type = ev.child.data_type(schema);
        let field_name = as_string_literal(ev.extraction.as_ref());
        match (&base_type, field_name) {
            (DataType::Struct(st), Some(name)) => {
                let field_nullable = st.field_by_name(name).map(|f| f.nullable).unwrap_or(true);
                base_nullable || field_nullable
            }
            // Spark `GetArrayItem.nullable` (complexTypeExtractors.scala +
            // `GetArrayItemUtil.computeNullabilityFromArray`), ANSI
            // failOnError=true:
            //   * a foldable, non-null CONSTANT index into a `CreateArray`
            //     literal child, in-bounds -> that element's nullability
            //     (e.g. `array(1,2,3)[0]` -> non-null; corpus witness cx-001);
            //   * a foldable constant index into ANY OTHER child (notably a
            //     column `col[i]`) -> true;
            //   * a non-constant index -> the array's `containsNull` flag.
            // Spark's rule intentionally does NOT OR-in `child.nullable`, so a
            // nullable array column still yields the above (the array-is-NULL
            // row is handled by the null-safe eval, not the schema flag).
            (DataType::Array(_, contains_null), _) => {
                match Self::const_int_index(ev.extraction.as_ref()) {
                    Some(i) => match Self::create_array_elements(ev.child.as_ref()) {
                        Some(elems) if i >= 0 && (i as usize) < elems.len() => {
                            elems[i as usize].nullable(schema)
                        }
                        // OOB constant index into a literal array (ANSI throws
                        // at runtime) OR a non-literal-array child: Spark
                        // yields `true`.
                        _ => true,
                    },
                    None => *contains_null,
                }
            }
            _ => true,
        }
    }

    /// The constant integer index of a foldable, non-null literal subscript
    /// (Spark's `Literal(_: Int)` case in `computeNullabilityFromArray`), if
    /// the extraction is one. Non-literal or non-integral extractions — which
    /// Spark treats as non-foldable — return `None`.
    fn const_int_index(extraction: &Expression) -> Option<i64> {
        match extraction {
            Expression::Literal(Literal { value, .. }) => match value {
                LiteralValue::Byte(v) => Some(i64::from(*v)),
                LiteralValue::Short(v) => Some(i64::from(*v)),
                LiteralValue::Int(v) => Some(i64::from(*v)),
                LiteralValue::Long(v) => Some(*v),
                _ => None,
            },
            _ => None,
        }
    }

    /// The element expressions of a Spark `CreateArray`-equivalent child, i.e.
    /// an `array(...)` literal. Both front-ends are covered: the DataFrame
    /// path lowers to [`Expression::ArrayLiteral`]; the SQL path lowers
    /// `array(...)` to an array-family [`Expression::FunctionCall`]. Any other
    /// child (a column, another expression) returns `None`.
    fn create_array_elements(child: &Expression) -> Option<&[Expression]> {
        match child {
            Expression::ArrayLiteral(a) => Some(&a.elements),
            Expression::FunctionCall(f)
                if matches!(
                    f.name.as_str(),
                    "array" | "list_value" | "make_array" | "list"
                ) =>
            {
                Some(&f.args)
            }
            _ => None,
        }
    }

    // ── UpdateFields derivation ──────────────────────────────────────────────

    fn update_fields_data_type(u: &UpdateFieldsExpression, schema: &ResolvedSchema) -> DataType {
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

// ── N4: binary-coercion materialization ─────────────────────────────────────
//
// `binary_data_type` (above) infers two Spark coercions that are implicit in
// the *type* it returns but are never inserted into the *tree*: (1) a lone
// integral Div operand widened to a synthetic decimal form for the
// arithmetic formula, and (2) `Date ± Interval` staying `Date` (DuckDB
// natively promotes it to `TIMESTAMP`). Emission used to re-derive both from
// the tree's declared types at render time (a second, independently
// maintained copy of this logic); N4 instead materializes them once, as the
// analyzer resolves a `Binary` node, via [`materialize_binary_coercions`] —
// see `CastExpression::implicit`'s doc for the naming/semantic_eq
// transparency contract that makes this safe.

/// The Add/Sub date-preserving match extracted from `binary_data_type`:
/// `Interval ± Date/Timestamp` preserves the date-like side. Shared by
/// `binary_data_type`'s own inference and [`materialize_binary_coercions`]'s
/// Date rule (which fires only on `Some(DataType::Date)` — DuckDB already
/// natively preserves `Timestamp`/`TimestampNtz` under `±Interval`, so only
/// the `Date` case needs a corrective CAST).
fn date_like_interval_result(op: &BinaryOp, l: &DataType, r: &DataType) -> Option<DataType> {
    if !matches!(op, BinaryOp::Add | BinaryOp::Sub) {
        return None;
    }
    match (l, r) {
        (DataType::Date, dt) | (dt, DataType::Date) if dt.is_interval() => Some(DataType::Date),
        (DataType::Timestamp, dt) | (dt, DataType::Timestamp) if dt.is_interval() => {
            Some(DataType::Timestamp)
        }
        (DataType::TimestampNtz, dt) | (dt, DataType::TimestampNtz) if dt.is_interval() => {
            Some(DataType::TimestampNtz)
        }
        _ => None,
    }
}

/// Per-side decimal-arithmetic widening classification (Spark's
/// `DecimalPrecision` rule), shared by `binary_data_type`'s formula
/// application and [`materialize_binary_coercions`]'s Div-widening CAST
/// insertion: distinguishes an operand that is ALREADY `Decimal` from one
/// Spark widens to a synthetic decimal form for the arithmetic formula only
/// — the latter needs a materialized CAST at emission time so DuckDB sees a
/// real DECIMAL operand instead of the analyzer's synthetic type.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DecimalOperand {
    /// Operand's own declared type is already `Decimal(p, s)`.
    Native(u8, u8),
    /// Operand widened to `Decimal(p, s)` per Spark's `DecimalPrecision` rule
    /// (the other side is `Decimal`; this side is integral).
    Widened(u8, u8),
}

impl DecimalOperand {
    fn parts(self) -> (u8, u8) {
        match self {
            DecimalOperand::Native(p, s) | DecimalOperand::Widened(p, s) => (p, s),
        }
    }
}

/// Classify both operands of a binary op for decimal-arithmetic formula
/// application: when exactly one side is `Decimal`, the other (if integral)
/// widens to a synthetic decimal form so the arithmetic formula applies
/// uniformly (Spark's `DecimalPrecision` cast-then-apply-formula rule).
/// `None` on a side that neither is, nor widens to, `Decimal` (e.g. a
/// `Double` operand — Spark: decimal ⊗ double → double, not decimal
/// arithmetic at all).
fn decimal_widen_operands(
    b: &BinaryExpression,
    l: &DataType,
    r: &DataType,
) -> (Option<DecimalOperand>, Option<DecimalOperand>) {
    let decimal_parts = |dt: &DataType| match dt {
        DataType::Decimal { precision, scale } => Some((*precision, *scale)),
        _ => None,
    };
    let lp = decimal_parts(l);
    let rp = decimal_parts(r);
    match (lp, rp) {
        (Some((p, s)), None) => (
            Some(DecimalOperand::Native(p, s)),
            Expression::decimalize(b.right.as_ref(), r).map(|(p, s)| DecimalOperand::Widened(p, s)),
        ),
        (None, Some((p, s))) => (
            Expression::decimalize(b.left.as_ref(), l).map(|(p, s)| DecimalOperand::Widened(p, s)),
            Some(DecimalOperand::Native(p, s)),
        ),
        (Some((lp, ls)), Some((rp, rs))) => (
            Some(DecimalOperand::Native(lp, ls)),
            Some(DecimalOperand::Native(rp, rs)),
        ),
        (None, None) => (None, None),
    }
}

/// Build a naming/`semantic_eq`-transparent implicit CAST (N4) — see
/// [`CastExpression::implicit`]'s doc for the transparency contract.
fn cast_impl(expr: Expression, to_type: DataType) -> Expression {
    Expression::Cast(CastExpression {
        expr: Box::new(expr),
        to_type,
        try_cast: false,
        implicit: true,
    })
}

/// Materialize the two N4 binary coercions into the tree — see this
/// section's overview comment. Only `Binary` nodes are rewritten; every
/// other variant passes through unchanged. Called exactly once, from
/// `resolve_and_stamp`'s `Binary` arm, on an already-recursed (children
/// resolved) node — never anywhere else: `semantic_eq` relies on BOTH rebind
/// sides flowing through `resolve_and_stamp`, so materializing anywhere else
/// would desync them.
pub(crate) fn materialize_binary_coercions(
    expr: Expression,
    schema: &ResolvedSchema,
) -> Expression {
    let Expression::Binary(b) = expr else {
        return expr;
    };
    let l = b.left.data_type(schema);
    let r = b.right.data_type(schema);
    // Date rule: DuckDB promotes `DATE ± INTERVAL` to TIMESTAMP; Spark keeps
    // it DATE. Wrap the whole node in a corrective, naming-transparent CAST.
    if date_like_interval_result(&b.op, &l, &r) == Some(DataType::Date) {
        return cast_impl(Expression::Binary(b), DataType::Date);
    }
    // Div rule: Spark's `DecimalPrecision` rule widens a lone integral
    // operand to a synthetic decimal form for the divide formula
    // (`binary_data_type`'s own inference); mirror that at the tree level
    // with an implicit CAST so DuckDB sees a real DECIMAL operand.
    if b.op == BinaryOp::Div {
        let (lop, rop) = decimal_widen_operands(&b, &l, &r);
        match (lop, rop) {
            (Some(DecimalOperand::Widened(p, s)), Some(DecimalOperand::Native(_, _))) => {
                return Expression::Binary(BinaryExpression {
                    op: b.op,
                    left: Box::new(cast_impl(
                        *b.left,
                        DataType::Decimal {
                            precision: p,
                            scale: s,
                        },
                    )),
                    right: b.right,
                });
            }
            (Some(DecimalOperand::Native(_, _)), Some(DecimalOperand::Widened(p, s))) => {
                return Expression::Binary(BinaryExpression {
                    op: b.op,
                    left: b.left,
                    right: Box::new(cast_impl(
                        *b.right,
                        DataType::Decimal {
                            precision: p,
                            scale: s,
                        },
                    )),
                });
            }
            _ => {}
        }
    }
    Expression::Binary(b)
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
            expr_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::type_inference::{
        aggregate_classifier_names, CORR_FAMILY_NAMES, HASH_FAMILY_NAMES,
    };
    use super::*;

    // ── Shared expression constructors ──────────────────────────────────────

    fn int_lit(v: i32) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Int(v),
            data_type: DataType::Integer,
        })
    }

    fn long_lit(v: i64) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Long(v),
            data_type: DataType::Long,
        })
    }

    fn str_lit(s: &str) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::String(s.to_owned()),
            data_type: DataType::String,
        })
    }

    fn bool_lit(v: bool) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Boolean(v),
            data_type: DataType::Boolean,
        })
    }

    /// `name(args...)` — every test-constructed call is non-DISTINCT.
    fn fcall(name: &str, args: Vec<Expression>) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: name.to_owned(),
            args,
            distinct: false,
        })
    }

    // ── Checklist §1.1 — `count_if` FunctionCall nullability ────────────────

    #[test]
    fn count_if_function_call_is_non_nullable() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "active",
            DataType::Boolean,
        )]));
        let expr = fcall("count_if", vec![ColumnReference::untyped("active")]);
        assert!(!expr.nullable(&s));
    }

    /// Sanity anchor — `count` over a nullable column must still be non-null.
    #[test]
    fn count_of_nullable_column_is_non_nullable() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "id",
            DataType::Long,
        )]));
        let expr = fcall("count", vec![ColumnReference::untyped("id")]);
        assert!(!expr.nullable(&s));
    }

    /// Spark `If.nullable` excludes the predicate — a nullable predicate with
    /// two non-null branches is non-nullable. Corpus witness: cnd-009.
    #[test]
    fn if_with_nullable_predicate_and_non_null_branches_is_non_nullable() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "salary",
            DataType::Long,
        )]));
        let expr = fcall(
            "if",
            vec![
                // nullable predicate (references a nullable column)
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(ColumnReference::untyped("salary")),
                    right: Box::new(long_lit(100_000)),
                }),
                str_lit("high"),
                str_lit("low"),
            ],
        );
        assert!(!expr.nullable(&s));
    }

    /// `iif` is a Spark alias for `If`, so its nullability rule likewise
    /// excludes the predicate — a nullable predicate with two non-null
    /// branches is non-nullable.
    #[test]
    fn iif_with_nullable_predicate_and_non_null_branches_is_non_nullable() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "salary",
            DataType::Long,
        )]));
        let expr = fcall(
            "iif",
            vec![
                // nullable predicate (references a nullable column)
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::Gt,
                    left: Box::new(ColumnReference::untyped("salary")),
                    right: Box::new(long_lit(100_000)),
                }),
                str_lit("high"),
                str_lit("low"),
            ],
        );
        assert!(!expr.nullable(&s));
    }

    /// `if` with a nullable true-branch is nullable regardless of predicate.
    #[test]
    fn if_with_nullable_true_branch_is_nullable() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "v",
            DataType::String,
        )]));
        let expr = fcall(
            "if",
            vec![
                // non-null predicate
                bool_lit(true),
                // nullable true-branch
                ColumnReference::untyped("v"),
                // non-null false-branch
                str_lit("low"),
            ],
        );
        assert!(expr.nullable(&s));
    }

    // ── Complex-type constructor inference (cx-001 / cx-002) ───────────────

    #[test]
    fn map_constructor_infers_key_and_value_types_from_args() {
        // `map('a', 1, 'b', 2)` → Map<String, Integer> with non-null values,
        // not the shared resolver's hard-coded Map<String, String>.
        let expr = fcall(
            "map",
            vec![str_lit("a"), int_lit(1), str_lit("b"), int_lit(2)],
        );
        match expr.data_type(&ResolvedSchema::empty()) {
            DataType::Map {
                key,
                value,
                value_nullable,
            } => {
                assert_eq!(*key, DataType::String);
                assert_eq!(*value, DataType::Integer);
                assert!(!value_nullable);
            }
            other => panic!("expected Map<String, Integer>, got {other:?}"),
        }
    }

    #[test]
    fn map_from_arrays_infers_key_and_value_types_from_array_args() {
        // `map_from_arrays(array('x', 'y'), array(10, 20))` →
        // Map<String, Integer, valueContainsNull=false> — the KEYS array's
        // element type for the key, the VALUES array's element type +
        // `containsNull` for the value, NOT the shared resolver's hard-coded
        // Map<String, String, true>. Verified against Spark 4.1.1
        // (differential `test_map_from_arrays`).
        let keys = fcall("array", vec![str_lit("x"), str_lit("y")]);
        let values = fcall("array", vec![int_lit(10), int_lit(20)]);
        let expr = fcall("map_from_arrays", vec![keys, values]);
        match expr.data_type(&ResolvedSchema::empty()) {
            DataType::Map {
                key,
                value,
                value_nullable,
            } => {
                assert_eq!(*key, DataType::String);
                assert_eq!(*value, DataType::Integer);
                assert!(!value_nullable);
            }
            other => panic!("expected Map<String, Integer>, got {other:?}"),
        }
    }

    #[test]
    fn map_from_arrays_value_contains_null_follows_values_array() {
        // A NULL element in the values array literal flips
        // `valueContainsNull` to true (Spark's `MapFromArrays.dataType`
        // reads the VALUES array's `containsNull`, independent of the keys
        // array).
        let null_int = Expression::Literal(Literal {
            value: LiteralValue::Null,
            data_type: DataType::Null,
        });
        let keys = fcall("array", vec![str_lit("x"), str_lit("y")]);
        let values = fcall("array", vec![int_lit(10), null_int]);
        let expr = fcall("map_from_arrays", vec![keys, values]);
        match expr.data_type(&ResolvedSchema::empty()) {
            DataType::Map { value_nullable, .. } => assert!(value_nullable),
            other => panic!("expected Map<..>, got {other:?}"),
        }
    }

    #[test]
    fn extract_value_over_map_returns_value_type() {
        // `map('a', 1)['a']` → Integer (the map's value type).
        let map = fcall("map", vec![str_lit("a"), int_lit(1)]);
        let ev = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(map),
            extraction: Box::new(str_lit("a")),
        });
        assert_eq!(ev.data_type(&ResolvedSchema::empty()), DataType::Integer);
    }

    #[test]
    fn array_index_into_non_null_literal_array_is_non_nullable() {
        // Spark `array(1, 2, 3)[0]` — the array is non-nullable with no null
        // elements and the index is a literal, so GetArrayItem is non-nullable.
        let arr = fcall("array", vec![int_lit(1), int_lit(2), int_lit(3)]);
        let ev = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(arr),
            extraction: Box::new(int_lit(0)),
        });
        assert_eq!(ev.data_type(&ResolvedSchema::empty()), DataType::Integer);
        assert!(!ev.nullable(&ResolvedSchema::empty()));
    }

    // ── Checklist §1.2 — hash family FunctionCall nullability ──────────────

    #[test]
    fn hash_and_xxhash64_are_non_nullable_regardless_of_args() {
        let s = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ]));
        // Sanity: the args ARE nullable — proves the fix (not a default arm)
        // is responsible for the non-null result.
        assert!(ColumnReference::untyped("name").nullable(&s));
        assert!(ColumnReference::untyped("salary").nullable(&s));

        for name in HASH_FAMILY_NAMES {
            let single = fcall(name, vec![ColumnReference::untyped("name")]);
            assert!(
                !single.nullable(&s),
                "{name}(nullable_col) must report nullable=false",
            );

            let multi = fcall(
                name,
                vec![
                    ColumnReference::untyped("name"),
                    ColumnReference::untyped("salary"),
                ],
            );
            assert!(
                !multi.nullable(&s),
                "{name}(nullable_col, nullable_col) must report nullable=false",
            );
        }
    }

    // ── Symmetric-omission mechanical checks (§8) ───────────────────────────

    /// §8.2 — every name where `aggregate_is_non_nullable` is `true` must
    /// produce a `FunctionCall` that reports `nullable == false`.
    /// (Rewritten for the AGG_SPECS table: iterates the classifier column in
    /// place of the retired `AGGREGATE_NAMES` const — same membership.)
    #[test]
    fn function_call_nullable_lists_are_symmetric_with_aggregate_is_non_nullable() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "x",
            DataType::Long,
        )]));
        for name in aggregate_classifier_names() {
            if !TypeInferenceEngine::aggregate_is_non_nullable(name) {
                continue;
            }
            let expr = fcall(name, vec![ColumnReference::untyped("x")]);
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
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "ts",
            DataType::Timestamp,
        )]));
        let expr = fcall(
            "window",
            vec![ColumnReference::untyped("ts"), str_lit("1 day")],
        );
        assert!(!expr.nullable(&schema));
        // Also non-nullable when the timestamp arg is non-null.
        let schema_nn = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "ts",
            DataType::Timestamp,
        )]));
        assert!(!expr.nullable(&schema_nn));
    }

    /// §8.3 — the hash family must be in the FunctionCall non-nullable literal list.
    #[test]
    fn hash_family_is_in_function_call_nullable_literal_list() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "x",
            DataType::String,
        )]));
        for name in HASH_FAMILY_NAMES {
            let expr = fcall(name, vec![ColumnReference::untyped("x")]);
            assert!(
                !expr.nullable(&schema),
                "hash family `{name}` must report nullable=false",
            );
        }
    }

    /// `ceil`/`ceiling`/`floor`/`round`/`bround` and the `exp`/`ln`/`log`/
    /// `log10`/`log2` family declare `nullable = true` unconditionally in
    /// Spark, even over a non-nullable input column — verified against
    /// Apache Spark 4.1.1 (see the `function_call_nullable` comment).
    /// Differential: TestMathFunctions::test_ceil_floor / test_round /
    /// test_exp / test_log.
    #[test]
    fn math_functions_are_always_nullable_over_non_null_input() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "x",
            DataType::Double,
        )]));
        let unary_cases: &[&str] = &[
            "ceil", "ceiling", "floor", "exp", "ln", "log", "log10", "log2",
        ];
        for name in unary_cases {
            let expr = fcall(name, vec![ColumnReference::untyped("x")]);
            assert!(
                expr.nullable(&schema),
                "`{name}` over a non-nullable arg must still report nullable=true",
            );
        }
        for name in ["round", "bround"] {
            let expr = fcall(name, vec![ColumnReference::untyped("x"), int_lit(2)]);
            assert!(
                expr.nullable(&schema),
                "`{name}` over a non-nullable arg must still report nullable=true",
            );
        }
        // Two-arg `log(base, x)` form must also stay always-nullable.
        let expr = fcall("log", vec![int_lit(10), ColumnReference::untyped("x")]);
        assert!(
            expr.nullable(&schema),
            "`log(base, x)` must be nullable=true"
        );
    }

    // ── Data-type derivations sanity ────────────────────────────────────────

    #[test]
    fn literal_data_type_and_nullability() {
        let s = ResolvedSchema::empty();
        let lit_int = int_lit(42);
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
        let s = ResolvedSchema::minted(StructType::new(vec![
            StructField::not_null("a", DataType::Integer),
            StructField::not_null("b", DataType::Integer),
        ]));
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
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "x",
            DataType::Integer,
        )]));
        let expr = Expression::Cast(CastExpression {
            expr: Box::new(ColumnReference::untyped("x")),
            to_type: DataType::Double,
            try_cast: false,
            implicit: false,
        });
        assert_eq!(expr.data_type(&s), DataType::Double);
        assert!(!expr.nullable(&s));
    }

    #[test]
    fn try_cast_is_nullable() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "x",
            DataType::Integer,
        )]));
        let expr = Expression::Cast(CastExpression {
            expr: Box::new(ColumnReference::untyped("x")),
            to_type: DataType::Double,
            try_cast: true,
            implicit: false,
        });
        assert!(expr.nullable(&s));
    }

    #[test]
    fn alias_propagates_inner() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "x",
            DataType::Long,
        )]));
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
        let s = ResolvedSchema::empty();
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
        let s = ResolvedSchema::empty();
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
        let s = ResolvedSchema::empty();
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
        let s = ResolvedSchema::empty();
        let expr = Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(CommonAst::new(CommonOp::SingleRow))),
        });
        assert_eq!(expr.data_type(&s), DataType::Unresolved);
        assert!(expr.nullable(&s));
    }

    #[test]
    fn scalar_subquery_analyzed_data_type_from_inner_col() {
        use super::super::analyzer::{TypedAst, TypedOp};
        let s = ResolvedSchema::empty();
        // A hand-built analyzed inner plan whose single output column is Long.
        let inner = TypedAst::new(
            TypedOp::SingleRow,
            ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
                "max_salary",
                DataType::Long,
            )])),
        );
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
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::not_null("age", DataType::Integer),
        ]));
        let expr = fcall(
            "struct",
            vec![
                ColumnReference::untyped("name"),
                ColumnReference::untyped("age"),
            ],
        );
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
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "name",
            DataType::String,
        )]));
        let aliased = Expression::Alias(AliasExpression {
            expr: Box::new(ColumnReference::untyped("name")),
            alias: "who".to_owned(),
        });
        let expr = fcall("struct", vec![aliased]);
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
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("a", DataType::Integer),
            StructField::nullable("b", DataType::String),
        ]));
        let expr = fcall(
            "named_struct",
            vec![
                str_lit("x"),
                ColumnReference::untyped("a"),
                str_lit("y"),
                ColumnReference::untyped("b"),
            ],
        );
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
            expr_id: None,
        })
    }

    /// `withField("country", "AT")` appends a new field to the struct's field
    /// list, preserving the existing fields.
    #[test]
    fn update_fields_with_field_adds_new_field() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]));
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![("country".to_owned(), Some(str_lit("AT")))],
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
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]));
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![("city".to_owned(), Some(str_lit("Vienna")))],
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
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]));
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
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]));
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
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]));
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![("CITY".to_owned(), Some(str_lit("Vienna")))],
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
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "address",
            address_struct_type(),
        )]));
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_column()),
            updates: vec![
                ("CITY".to_owned(), Some(str_lit("Vienna"))),
                ("GEO".to_owned(), None),
                ("country".to_owned(), Some(str_lit("AT"))),
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
        let s = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("a", DataType::Integer),
            StructField::nullable("b", DataType::Integer),
        ]));
        for name in CORR_FAMILY_NAMES {
            let expr = fcall(
                name,
                vec![ColumnReference::untyped("a"), ColumnReference::untyped("b")],
            );
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
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "tags",
            DataType::Array(Box::new(DataType::String), true),
        )]));
        let expr = fcall(
            "aggregate",
            vec![
                ColumnReference::untyped("tags"),
                str_lit(""),
                // Real emissions place a Lambda here; the type inference
                // fast-path reads only args[0..2], so a placeholder col
                // is sufficient for this test.
                ColumnReference::untyped("__lambda_placeholder"),
            ],
        );
        assert_eq!(
            expr.data_type(&s),
            DataType::String,
            "aggregate should return the seed's type (String), not Array<String>",
        );
    }

    /// Numeric seed → aggregate returns numeric, not array.
    #[test]
    fn aggregate_hof_with_long_seed_returns_long() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "nums",
            DataType::Array(Box::new(DataType::Long), true),
        )]));
        let expr = fcall(
            "reduce",
            vec![
                ColumnReference::untyped("nums"),
                long_lit(0),
                ColumnReference::untyped("__lambda_placeholder"),
            ],
        );
        assert_eq!(expr.data_type(&s), DataType::Long);
    }

    /// Spark's `ArrayAggregate.nullable` is always `true` — the accumulator
    /// variable is bound with `nullable = true` in `bindInternal`, so
    /// `finish.nullable()` is always `true`. Verify this holds even when the
    /// array column AND the seed literal are both non-nullable.
    #[test]
    fn aggregate_hof_nullable_with_non_null_array_and_seed() {
        // Non-nullable array column + non-nullable seed → still nullable.
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "nums",
            DataType::Array(Box::new(DataType::Integer), false),
        )]));
        for name in ["aggregate", "reduce", "list_reduce"] {
            let expr = fcall(
                name,
                vec![
                    ColumnReference::untyped("nums"),
                    long_lit(0),
                    ColumnReference::untyped("__lambda_placeholder"),
                ],
            );
            assert!(
                expr.nullable(&s),
                "{name} HOF must be nullable (Spark ArrayAggregate rule)",
            );
        }
    }

    /// Same as above but with a nullable array column — still always nullable.
    #[test]
    fn aggregate_hof_nullable_with_nullable_array() {
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "nums",
            DataType::Array(Box::new(DataType::Long), true),
        )]));
        let expr = fcall(
            "aggregate",
            vec![
                ColumnReference::untyped("nums"),
                long_lit(0),
                ColumnReference::untyped("__lambda_placeholder"),
            ],
        );
        assert!(
            expr.nullable(&s),
            "aggregate HOF must be nullable even with nullable array input",
        );
    }

    // ── Pass 90 — inline_field / inline_outer_field type + nullability ──────

    /// Schema holding a single `arr : Array<Struct<name STRING?, age INT?>>`
    /// column — the canonical Pass-90 fixture.
    fn inline_test_schema(arr_contains_null: bool) -> ResolvedSchema {
        let element = DataType::Struct(StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::nullable("age", DataType::Integer),
        ]));
        ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "arr",
            DataType::Array(Box::new(element), arr_contains_null),
        )]))
    }

    fn inline_field_call(arr_col: &str, field: &str, outer: bool) -> Expression {
        let name = if outer {
            "inline_outer_field"
        } else {
            "inline_field"
        };
        fcall(
            name,
            vec![ColumnReference::untyped(arr_col), str_lit(field)],
        )
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
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element), false),
        )]));
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
        let s1 = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element_notnull.clone()), false),
        )]));
        assert!(!inline_field_call("arr", "name", false).nullable(&s1));

        // Case 2: containsNull=true → nullable.
        let s2 = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element_notnull.clone()), true),
        )]));
        assert!(inline_field_call("arr", "name", false).nullable(&s2));

        // Case 3: struct field itself nullable → nullable.
        let element_field_null = DataType::Struct(StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::not_null("age", DataType::Integer),
        ]));
        let s3 = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "arr",
            DataType::Array(Box::new(element_field_null), false),
        )]));
        assert!(inline_field_call("arr", "name", false).nullable(&s3));
        assert!(!inline_field_call("arr", "age", false).nullable(&s3));

        // Case 4: arr nullable → nullable.
        let s4 = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "arr",
            DataType::Array(Box::new(element_notnull), false),
        )]));
        assert!(inline_field_call("arr", "name", false).nullable(&s4));
    }

    // ── Pass 91 — json_tuple_field type + nullability ────────────────────

    fn json_tuple_field_call(json_col: &str, key: &str) -> Expression {
        fcall(
            "json_tuple_field",
            vec![ColumnReference::untyped(json_col), str_lit(key)],
        )
    }

    /// `json_tuple_field(json_str, "<key>")` is always STRING per Spark's
    /// `JsonTuple.elementSchema`.
    #[test]
    fn json_tuple_field_data_type_is_string() {
        // json_str typed as String, non-null — return type STRING regardless.
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "json_str",
            DataType::String,
        )]));
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
        let s = ResolvedSchema::minted(StructType::new(vec![StructField::not_null(
            "json_str",
            DataType::String,
        )]));
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
        let case_when = Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(int_lit(1), int_lit(2))],
            else_expr: Some(Box::new(int_lit(3))),
        });
        let binary = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(case_when),
            right: Box::new(int_lit(4)),
        });
        let func = fcall("f", vec![binary]);
        let alias = Expression::Alias(AliasExpression {
            expr: Box::new(func),
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
                lower: FrameBoundary::Preceding(Box::new(int_lit(1))),
                upper: FrameBoundary::CurrentRow,
            }),
        });
        let children: Vec<&Expression> = win.children().collect();
        // Exactly three children: func, one partition_by, one order_by.expr.
        assert_eq!(children.len(), 3);
    }

    // ── round/bround/mod/pmod multi-arg type-inference pre-pass ──────────────
    // (`function_call_data_type`; corpus num-005 round/bround, num-012 mod/pmod)

    fn dec(precision: u8, scale: u8) -> DataType {
        DataType::Decimal { precision, scale }
    }

    /// Resolve `name(args...)` against a single-column schema of `col_type`,
    /// where `args[0]` references that column.
    fn call_type(name: &str, args: Vec<Expression>, col_type: DataType) -> DataType {
        let schema =
            ResolvedSchema::minted(StructType::new(vec![StructField::nullable("c", col_type)]));
        fcall(name, args).data_type(&schema)
    }

    #[test]
    fn round_bround_decimal_scale_decreases() {
        // RoundBase decimal branch: round(Decimal(10,2), 1) → Decimal(10,1).
        assert_eq!(
            call_type(
                "round",
                vec![ColumnReference::untyped("c"), int_lit(1)],
                dec(10, 2)
            ),
            dec(10, 1)
        );
        // bround shares RoundBase: bround(Decimal(6,3), 2) → Decimal(6,2).
        assert_eq!(
            call_type(
                "bround",
                vec![ColumnReference::untyped("c"), int_lit(2)],
                dec(6, 3)
            ),
            dec(6, 2)
        );
        // round(Decimal(38,6), 3): ild=33, ns=3 → min(36,38)=36 → Decimal(36,3).
        assert_eq!(
            call_type(
                "round",
                vec![ColumnReference::untyped("c"), int_lit(3)],
                dec(38, 6)
            ),
            dec(36, 3)
        );
    }

    #[test]
    fn round_bround_non_decimal_type_unchanged() {
        // RoundBase `case t => t`: a non-decimal child keeps its type.
        assert_eq!(
            call_type(
                "round",
                vec![ColumnReference::untyped("c"), int_lit(1)],
                DataType::Double
            ),
            DataType::Double
        );
        assert_eq!(
            call_type(
                "bround",
                vec![ColumnReference::untyped("c"), int_lit(2)],
                DataType::Double
            ),
            DataType::Double
        );
    }

    #[test]
    fn round_one_arg_decimal_uses_scale_zero() {
        // Missing 2nd arg ⇒ scale 0: round(Decimal(10,2)) → Decimal(9,0).
        assert_eq!(
            call_type("round", vec![ColumnReference::untyped("c")], dec(10, 2)),
            dec(9, 0)
        );
    }

    #[test]
    fn round_non_literal_scale_is_unresolved() {
        // A non-literal scale argument is a Thunderduck boundary → Unresolved.
        assert_eq!(
            call_type(
                "round",
                vec![ColumnReference::untyped("c"), ColumnReference::untyped("c"),],
                dec(10, 2)
            ),
            DataType::Unresolved
        );
    }

    #[test]
    fn mod_pmod_both_decimal_widens() {
        // mod(Decimal(10,2), Decimal(6,3)) → Decimal(6,3) per decimal_mod_type.
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d1", dec(10, 2)),
            StructField::nullable("d2", dec(6, 3)),
        ]));
        let expr = fcall(
            "mod",
            vec![
                ColumnReference::untyped("d1"),
                ColumnReference::untyped("d2"),
            ],
        );
        assert_eq!(expr.data_type(&schema), dec(6, 3));
    }

    #[test]
    fn mod_non_decimal_delegates_to_first_arg_resolver() {
        // int/int and bigint/int fall through to function_return_type, which
        // types them via the first (wider) arg — the path that greens today.
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("a", DataType::Integer),
            StructField::nullable("b", DataType::Integer),
            StructField::nullable("lng", DataType::Long),
        ]));
        let mod_ii = fcall(
            "mod",
            vec![ColumnReference::untyped("a"), ColumnReference::untyped("b")],
        );
        assert_eq!(mod_ii.data_type(&schema), DataType::Integer);
        let mod_li = fcall(
            "mod",
            vec![
                ColumnReference::untyped("lng"),
                ColumnReference::untyped("a"),
            ],
        );
        assert_eq!(mod_li.data_type(&schema), DataType::Long);
    }

    // ── Decimal ⊗ integral arithmetic (Spark `DecimalPrecision`) ─────────────
    // Pass 15: exactly one side Decimal + the other integral must cast the
    // integral side to a decimal form and apply the arithmetic formula,
    // rather than falling through to `promote_numeric`'s union-widening.

    fn bin(op: BinaryOp, left: Expression, right: Expression) -> Expression {
        Expression::Binary(BinaryExpression {
            op,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    #[test]
    fn decimal_mul_long_column_promotes_via_decimal_form() {
        // Decimal(15,2) * Long column: the Long column casts via
        // `decimal_form` (Long → (20,0)), then decimal_mul_type(15,2,20,0)
        // = raw_precision 15+20+1=36, raw_scale 2+0=2 → Decimal(36,2).
        // (NOT `unify_decimal(15,2,20,0)` = Decimal(22,2), the old
        // union-widening result.)
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d", dec(15, 2)),
            StructField::nullable("n", DataType::Long),
        ]));
        let expr = bin(
            BinaryOp::Mul,
            ColumnReference::untyped("d"),
            ColumnReference::untyped("n"),
        );
        assert_eq!(expr.data_type(&schema), dec(36, 2));
    }

    #[test]
    fn decimal_mul_int_literal_uses_minimal_precision() {
        // Decimal(7,2) * Int literal 100: the literal uses `fromLiteral`'s
        // MINIMAL precision (3,0) — the digit count of 100 — NOT
        // `decimal_form(Integer)` = (10,0). decimal_mul_type(7,2,3,0) =
        // raw_precision 7+3+1=11, raw_scale 2 → Decimal(11,2).
        let schema =
            ResolvedSchema::minted(StructType::new(vec![StructField::nullable("d", dec(7, 2))]));
        let expr = bin(BinaryOp::Mul, ColumnReference::untyped("d"), int_lit(100));
        let result = expr.data_type(&schema);
        assert_eq!(result, dec(11, 2));
        // Explicitly NOT the `decimal_form(Integer)` = (10,0) result, which
        // would give decimal_mul_type(7,2,10,0) = Decimal(18,2).
        assert_ne!(result, dec(18, 2));
    }

    #[test]
    fn decimal_add_int_literal() {
        // Decimal(5,2) + Int literal 3: fromLiteral(3) = (1,0).
        // decimal_add_type(5,2,1,0): scale=max(2,0)=2,
        // int_digits=max(3,1)=3, precision=min(3+2+1,38)=6 → Decimal(6,2).
        let schema =
            ResolvedSchema::minted(StructType::new(vec![StructField::nullable("d", dec(5, 2))]));
        let expr = bin(BinaryOp::Add, ColumnReference::untyped("d"), int_lit(3));
        assert_eq!(expr.data_type(&schema), dec(6, 2));
    }

    #[test]
    fn decimal_div_int_literal() {
        // tpcds-q058 shape: Decimal(19,2) / Int literal 3. fromLiteral(3) =
        // (1,0). decimal_div_type(19,2,1,0): scale_raw=max(6,2+1+1)=6,
        // precision_raw=19-2+0+6=23 → Decimal(23,6).
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            dec(19, 2),
        )]));
        let expr = bin(BinaryOp::Div, ColumnReference::untyped("d"), int_lit(3));
        assert_eq!(expr.data_type(&schema), dec(23, 6));
    }

    #[test]
    fn decimal_times_double_stays_promote_numeric() {
        // Decimal ⊗ Double must NOT be coerced through decimal arithmetic —
        // Spark: decimal ⊗ double → double, unchanged from before this pass.
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d", dec(10, 2)),
            StructField::nullable("f", DataType::Double),
        ]));
        let expr = bin(
            BinaryOp::Mul,
            ColumnReference::untyped("d"),
            ColumnReference::untyped("f"),
        );
        assert_eq!(expr.data_type(&schema), DataType::Double);
    }

    #[test]
    fn int_div_int_still_double() {
        // Int/Int division still promotes to Double — this pass only
        // changes decimal ⊗ integral, not integral ⊗ integral.
        let schema = ResolvedSchema::empty();
        let expr = bin(BinaryOp::Div, int_lit(6), int_lit(2));
        assert_eq!(expr.data_type(&schema), DataType::Double);
    }

    #[test]
    fn both_decimal_unchanged() {
        // Both sides Decimal must still produce exactly today's result —
        // this path is untouched by this pass.
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d1", dec(10, 2)),
            StructField::nullable("d2", dec(6, 3)),
        ]));
        let expr = bin(
            BinaryOp::Mul,
            ColumnReference::untyped("d1"),
            ColumnReference::untyped("d2"),
        );
        // decimal_mul_type(10,2,6,3): raw_precision=10+6+1=17, raw_scale=5.
        assert_eq!(expr.data_type(&schema), dec(17, 5));
    }

    #[test]
    fn decimal_intdiv_stays_long() {
        // Spark `div` (IntegralDivide) is LongType regardless of operand types.
        // A decimal operand must NOT drag IntDiv into the decimal-arithmetic
        // formulas (regression guard: the decimal-coercion block must not
        // swallow IntDiv). Covers decimal // integer-literal AND decimal //
        // decimal (a pre-existing defect this also corrects).
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d1", dec(10, 2)),
            StructField::nullable("d2", dec(6, 3)),
        ]));
        assert_eq!(
            bin(BinaryOp::IntDiv, ColumnReference::untyped("d1"), int_lit(3)).data_type(&schema),
            DataType::Long,
        );
        assert_eq!(
            bin(
                BinaryOp::IntDiv,
                ColumnReference::untyped("d1"),
                ColumnReference::untyped("d2"),
            )
            .data_type(&schema),
            DataType::Long,
        );
    }

    #[test]
    fn int_literal_div_decimal_is_symmetric() {
        // Decimal on the RIGHT (the `(None, Some)` coercion arm) with a
        // non-commutative op: the int literal `100` coerces to (3,0) as the
        // NUMERATOR, the decimal column stays the denominator.
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            dec(10, 2),
        )]));
        let expr = bin(BinaryOp::Div, int_lit(100), ColumnReference::untyped("d"));
        // decimal_div_type(3,0,10,2) — left/numerator is the coerced literal.
        assert_eq!(
            expr.data_type(&schema),
            TypeInferenceEngine::decimal_div_type(3, 0, 10, 2),
        );
    }

    #[test]
    fn interval_kind_maps_to_spark_data_type() {
        let schema = ResolvedSchema::empty();
        let with_kind = |kind| {
            Expression::Interval(IntervalExpression {
                months: 0,
                days: 0,
                microseconds: 0,
                kind,
            })
        };
        assert_eq!(
            with_kind(IntervalKind::YearMonth).data_type(&schema),
            DataType::YearMonthInterval
        );
        assert_eq!(
            with_kind(IntervalKind::DayTime).data_type(&schema),
            DataType::DayTimeInterval
        );
        assert_eq!(
            with_kind(IntervalKind::Calendar).data_type(&schema),
            DataType::Interval
        );
    }

    #[test]
    fn interval_literal_is_non_nullable_for_all_kinds() {
        let schema = ResolvedSchema::empty();
        for kind in [
            IntervalKind::YearMonth,
            IntervalKind::DayTime,
            IntervalKind::Calendar,
        ] {
            let expr = Expression::Interval(IntervalExpression {
                months: 0,
                days: 0,
                microseconds: 0,
                kind,
            });
            assert!(!expr.nullable(&schema));
        }
    }

    // ── N4: binary-coercion materialization ──────────────────────────────────
    // `materialize_binary_coercions` — Div-widening + Date±Interval rules.

    fn calendar_interval() -> Expression {
        Expression::Interval(IntervalExpression {
            months: 0,
            days: 1,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        })
    }

    #[test]
    fn materialize_non_binary_passes_through_unchanged() {
        let schema = ResolvedSchema::empty();
        let expr = int_lit(1);
        let before = expr.clone();
        assert_eq!(materialize_binary_coercions(expr, &schema), before);
    }

    #[test]
    fn materialize_div_wraps_integral_right_side_with_decimal_form() {
        // `d / i` — Decimal(15,2) ÷ Integer: the integral RIGHT side widens
        // via `decimal_form` (Integer → (10,0)); only that side gets an
        // implicit CAST.
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d", dec(15, 2)),
            StructField::nullable("i", DataType::Integer),
        ]));
        let expr_before_type = bin(
            BinaryOp::Div,
            ColumnReference::untyped("d"),
            ColumnReference::untyped("i"),
        )
        .data_type(&schema);
        let expr = bin(
            BinaryOp::Div,
            ColumnReference::untyped("d"),
            ColumnReference::untyped("i"),
        );
        let materialized = materialize_binary_coercions(expr, &schema);
        let Expression::Binary(b) = &materialized else {
            panic!("expected Binary, got {materialized:?}");
        };
        assert!(matches!(b.left.as_ref(), Expression::ColumnReference(c) if c.name == "d"));
        match b.right.as_ref() {
            Expression::Cast(c) => {
                assert!(c.implicit);
                assert!(!c.try_cast);
                assert_eq!(c.to_type, dec(10, 0));
                assert!(
                    matches!(c.expr.as_ref(), Expression::ColumnReference(inner) if inner.name == "i")
                );
            }
            other => panic!("expected implicit Cast on the widened side, got {other:?}"),
        }
        // Wire schema unchanged: the materialized tree's declared type is
        // exactly what `binary_data_type` already inferred pre-N4.
        assert_eq!(materialized.data_type(&schema), expr_before_type);
    }

    #[test]
    fn materialize_div_wraps_integer_literal_left_side_with_from_literal_precision() {
        // `100 / d` — the int-literal LEFT side widens via
        // `DecimalType.fromLiteral` (single-digit → (1,0)), not
        // `decimal_form`.
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            dec(15, 2),
        )]));
        let expr = bin(BinaryOp::Div, int_lit(9), ColumnReference::untyped("d"));
        let materialized = materialize_binary_coercions(expr, &schema);
        let Expression::Binary(b) = &materialized else {
            panic!("expected Binary, got {materialized:?}");
        };
        match b.left.as_ref() {
            Expression::Cast(c) => {
                assert!(c.implicit);
                assert_eq!(c.to_type, dec(1, 0));
                assert!(matches!(c.expr.as_ref(), Expression::Literal(_)));
            }
            other => panic!("expected implicit Cast on the widened side, got {other:?}"),
        }
        assert!(matches!(b.right.as_ref(), Expression::ColumnReference(c) if c.name == "d"));
    }

    #[test]
    fn materialize_div_decimal_over_decimal_untouched() {
        // Both sides already `Decimal` — no widening, no Cast inserted
        // anywhere.
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d1", dec(15, 2)),
            StructField::nullable("d2", dec(10, 3)),
        ]));
        let expr = bin(
            BinaryOp::Div,
            ColumnReference::untyped("d1"),
            ColumnReference::untyped("d2"),
        );
        let before = expr.clone();
        assert_eq!(materialize_binary_coercions(expr, &schema), before);
    }

    #[test]
    fn materialize_div_decimal_over_double_untouched() {
        // Decimal ÷ Double: `decimalize` returns `None` for a non-integral
        // Double operand (Spark: decimal ⊗ double → double) — the Div rule
        // must not fire.
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("d", dec(15, 2)),
            StructField::nullable("f", DataType::Double),
        ]));
        let expr = bin(
            BinaryOp::Div,
            ColumnReference::untyped("d"),
            ColumnReference::untyped("f"),
        );
        let before = expr.clone();
        assert_eq!(materialize_binary_coercions(expr, &schema), before);
    }

    #[test]
    fn materialize_date_plus_interval_wraps_whole_node_in_implicit_cast_to_date() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            DataType::Date,
        )]));
        let expr = bin(
            BinaryOp::Add,
            ColumnReference::untyped("d"),
            calendar_interval(),
        );
        let materialized = materialize_binary_coercions(expr.clone(), &schema);
        match &materialized {
            Expression::Cast(c) => {
                assert!(c.implicit);
                assert!(!c.try_cast);
                assert_eq!(c.to_type, DataType::Date);
                assert_eq!(c.expr.as_ref(), &expr);
            }
            other => panic!("expected implicit Cast(.. AS Date), got {other:?}"),
        }
    }

    #[test]
    fn materialize_interval_plus_date_wraps_whole_node_in_implicit_cast_to_date() {
        // Commutative: `INTERVAL + d` must also wrap.
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            DataType::Date,
        )]));
        let expr = bin(
            BinaryOp::Add,
            calendar_interval(),
            ColumnReference::untyped("d"),
        );
        let materialized = materialize_binary_coercions(expr, &schema);
        assert!(
            matches!(&materialized, Expression::Cast(c) if c.implicit && c.to_type == DataType::Date)
        );
    }

    #[test]
    fn materialize_date_minus_interval_wraps_whole_node_in_implicit_cast_to_date() {
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "d",
            DataType::Date,
        )]));
        let expr = bin(
            BinaryOp::Sub,
            ColumnReference::untyped("d"),
            calendar_interval(),
        );
        let materialized = materialize_binary_coercions(expr, &schema);
        assert!(
            matches!(&materialized, Expression::Cast(c) if c.implicit && c.to_type == DataType::Date)
        );
    }

    #[test]
    fn materialize_timestamp_plus_interval_untouched() {
        // DuckDB already natively preserves `Timestamp ± Interval` as
        // Timestamp — only the Date case needs a corrective CAST.
        let schema = ResolvedSchema::minted(StructType::new(vec![StructField::nullable(
            "t",
            DataType::Timestamp,
        )]));
        let expr = bin(
            BinaryOp::Add,
            ColumnReference::untyped("t"),
            calendar_interval(),
        );
        let before = expr.clone();
        assert_eq!(materialize_binary_coercions(expr, &schema), before);
    }
}
