//! sqlparser-rs AST → τ [`CommonAst`] lowering.
//!
//! Scope (per architecture plan §4):
//! - `SELECT expr, … FROM table WHERE … GROUP BY … ORDER BY … LIMIT n OFFSET m`
//! - bare `SELECT literal`
//! - `SELECT … FROM (VALUES ...)` and other subquery-in-FROM forms
//! - basic joins (INNER / LEFT / RIGHT / FULL / CROSS / LEFT SEMI / LEFT ANTI)
//! - `SELECT *`
//!
//! Deferred (surface as [`EmissionError::Unsupported`] with `kind: ProtoShape`):
//! PIVOT, GROUPING SETS, ROLLUP, CUBE, LATERAL VIEW, TABLESAMPLE, CTE,
//! UNION/INTERSECT/EXCEPT, window functions, HOFs, `json_tuple` rewrites,
//! command statements.
//!
//! **INV10:** imports only value-level types from `crate::types` plus
//! intra-τ modules. No `crate::parser`, `crate::logical`, `crate::expression`.
//!
//! **Plan-id policy (Open Decision 12):** every [`UnresolvedColumn`] emitted
//! by this module has `plan_id = None`.

use sqlparser::ast::{
    BinaryOperator, CastKind, DataType as SqlDataType, DateTimeField, DuplicateTreatment,
    ExactNumberInfo, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, GroupByExpr, Interval, JoinConstraint, JoinOperator, LimitClause,
    NamedWindowDefinition, NamedWindowExpr, ObjectName, ObjectNamePart, OrderByExpr, OrderByKind,
    OrderByOptions, Query, Select, SelectItem, SetExpr, Statement, TableFactor, TableWithJoins,
    UnaryOperator, Value, ValueWithSpan, WindowFrame as SqlWindowFrame,
    WindowFrameBound as SqlWindowFrameBound, WindowFrameUnits, WindowSpec, WindowType,
};
use std::collections::HashMap;

use crate::bail_boundary_proto;
use crate::transpiler_v2::ast::{CommonAst, CommonOp, JoinType};
use crate::transpiler_v2::error::UnsupportedKind;
use crate::transpiler_v2::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression, Expression,
    FrameBoundary, FrameUnit, FunctionCall, InListExpression, IntervalExpression, LambdaExpression,
    LambdaVariableExpression, LikeExpression, Literal, LiteralValue, NullOrdering, SortDirection,
    SortOrder, StarExpression, UnaryExpression, UnaryOp, UnresolvedColumn, WindowFrame,
    WindowFunction,
};
use crate::transpiler_v2::macros::ProtoFieldExt;
use crate::transpiler_v2::type_inference::AGGREGATE_NAMES;
use crate::transpiler_v2::EmissionError;
use crate::types::DataType;

/// Lower a parsed sqlparser [`Statement`] into a [`CommonAst`].
pub fn lower_statement(stmt: Statement) -> Result<CommonAst, EmissionError> {
    match stmt {
        Statement::Query(q) => lower_query(*q),
        other => bail_boundary_proto!(
            format!("sql::{}", statement_kind(&other)),
            "parser_v2 only supports SELECT queries",
        ),
    }
}

fn statement_kind(stmt: &Statement) -> &'static str {
    match stmt {
        Statement::Query(_) => "query",
        Statement::Insert(_) => "insert",
        Statement::Delete(_) => "delete",
        Statement::Update { .. } => "update",
        Statement::Drop { .. } => "drop",
        Statement::CreateTable(_) => "create_table",
        Statement::CreateView(_) => "create_view",
        Statement::AlterTable { .. } => "alter_table",
        Statement::Truncate { .. } => "truncate",
        _ => "other",
    }
}

fn lower_query(query: Query) -> Result<CommonAst, EmissionError> {
    if query.with.is_some() {
        bail_boundary_proto!(
            "sql::cte",
            "CTEs (WITH clauses) not supported by τ\'s SparkSQL parser",
        );
    }

    let order_by_exprs: Vec<OrderByExpr> = match &query.order_by {
        Some(ob) => match &ob.kind {
            OrderByKind::Expressions(exprs) => exprs.clone(),
            // Spark `ORDER BY ALL` orders by every output column, left to
            // right, applying the ALL clause's asc/desc + nulls options to all.
            OrderByKind::All(opts) => order_by_all_exprs(&query.body, opts)?,
        },
        None => vec![],
    };

    let (limit_expr_opt, offset_expr_opt) = extract_limit_offset(query.limit_clause.as_ref())?;

    let body = lower_set_expr(*query.body)?;
    wrap_with_sort_limit(body, order_by_exprs, limit_expr_opt, offset_expr_opt)
}

fn extract_limit_offset(
    clause: Option<&LimitClause>,
) -> Result<(Option<Expr>, Option<Expr>), EmissionError> {
    match clause {
        None => Ok((None, None)),
        Some(LimitClause::LimitOffset { limit, offset, .. }) => {
            let off = offset.as_ref().map(|o| o.value.clone());
            Ok((limit.clone(), off))
        }
        Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
            Ok((Some(limit.clone()), Some(offset.clone())))
        }
    }
}

/// Expand `ORDER BY ALL` into one ordering key per output column.
///
/// Spark orders by every SELECT projection item, left to right, applying the
/// `ALL` clause's asc/desc + nulls options uniformly to each key. The output
/// columns are the projection of the query's `SELECT` body; each key reuses the
/// projection's underlying expression (an aliased item orders by its defining
/// expression, which is equivalent to ordering by the output column).
fn order_by_all_exprs(
    body: &SetExpr,
    opts: &OrderByOptions,
) -> Result<Vec<OrderByExpr>, EmissionError> {
    let projection = match body {
        SetExpr::Select(sel) => &sel.projection,
        _ => {
            bail_boundary_proto!(
                "sql::order_by_all_non_select",
                "ORDER BY ALL requires a SELECT body",
            );
        }
    };
    let mut exprs: Vec<OrderByExpr> = Vec::with_capacity(projection.len());
    for item in projection {
        let expr = match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e.clone(),
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                bail_boundary_proto!(
                    "sql::order_by_all_wildcard",
                    "ORDER BY ALL over a `*` projection not supported",
                );
            }
        };
        exprs.push(OrderByExpr {
            expr,
            options: opts.clone(),
            with_fill: None,
        });
    }
    Ok(exprs)
}

fn lower_set_expr(body: SetExpr) -> Result<CommonAst, EmissionError> {
    match body {
        SetExpr::Select(sel) => lower_select(*sel),
        SetExpr::Query(q) => lower_query(*q),
        SetExpr::Values(_) => bail_boundary_proto!(
            "sql::values_top_level",
            "top-level VALUES not supported (only VALUES in FROM)",
        ),
        SetExpr::SetOperation { op, .. } => Err(EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: format!("sql::set_operation::{op:?}").to_ascii_lowercase(),
            reason: "UNION / INTERSECT / EXCEPT not implemented in τ\'s SparkSQL parser".to_owned(),
        }),
        other => bail_boundary_proto!(
            format!("sql::set_expr::{other:?}"),
            "set expression not supported by τ\'s SparkSQL parser",
        ),
    }
}

fn lower_select(mut select: Select) -> Result<CommonAst, EmissionError> {
    if select.distinct.is_some() {
        bail_boundary_proto!(
            "sql::select_distinct",
            "SELECT DISTINCT not implemented in τ\'s SparkSQL parser",
        );
    }
    // Emission has no notion of a `WINDOW w AS (...)` clause, so resolve every
    // `OVER w` reference in the projection to its inline spec before lowering.
    if !select.named_window.is_empty() {
        let map = build_named_window_map(&select.named_window)?;
        for item in &mut select.projection {
            if let SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } = item {
                inline_named_windows(e, &map)?;
            }
        }
    }
    let base = lower_from(select.from)?;

    let filtered = if let Some(cond) = select.selection {
        CommonAst::new(CommonOp::Filter {
            input: Box::new(base),
            condition: lower_expr(cond)?,
        })
    } else {
        base
    };

    let has_group_by =
        !matches!(&select.group_by, GroupByExpr::Expressions(v, m) if v.is_empty() && m.is_empty());
    let has_aggregates = has_group_by
        || select
            .projection
            .iter()
            .any(|item| select_item_has_aggregate(item));

    let plan = if has_aggregates {
        lower_aggregate_select(filtered, select.projection, select.group_by, select.having)?
    } else {
        let projections: Result<Vec<Expression>, EmissionError> = select
            .projection
            .into_iter()
            .map(lower_select_item)
            .collect();
        CommonAst::new(CommonOp::Project {
            input: Box::new(filtered),
            projections: projections?,
        })
    };

    Ok(plan)
}

fn lower_aggregate_select(
    input: CommonAst,
    projection: Vec<SelectItem>,
    group_by: GroupByExpr,
    having: Option<Expr>,
) -> Result<CommonAst, EmissionError> {
    let grouping = match group_by {
        GroupByExpr::Expressions(exprs, modifiers) => {
            if !modifiers.is_empty() {
                bail_boundary_proto!(
                    "sql::group_by_modifiers",
                    "GROUP BY modifiers (ROLLUP/CUBE/GROUPING SETS) not implemented in τ",
                );
            }
            let mut plain: Vec<Expression> = Vec::with_capacity(exprs.len());
            for e in exprs {
                match e {
                    Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_) => {
                        bail_boundary_proto!(
                            "sql::grouping_sets",
                            "ROLLUP / CUBE / GROUPING SETS not implemented in τ",
                        );
                    }
                    other => plain.push(lower_expr(other)?),
                }
            }
            plain
        }
        GroupByExpr::All(modifiers) => {
            if !modifiers.is_empty() {
                bail_boundary_proto!(
                    "sql::group_by_all_modifiers",
                    "GROUP BY ALL modifiers (WITH ROLLUP/CUBE/TOTALS) not implemented in τ",
                );
            }
            // Spark `GROUP BY ALL` groups by every SELECT item that is not an
            // aggregate expression. Compute the grouping from the projection.
            let mut plain: Vec<Expression> = Vec::with_capacity(projection.len());
            for item in &projection {
                let expr = match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                        bail_boundary_proto!(
                            "sql::group_by_all_wildcard",
                            "GROUP BY ALL over a `*` projection not supported",
                        );
                    }
                };
                if !expr_has_aggregate(expr) {
                    plain.push(lower_expr(expr.clone())?);
                }
            }
            plain
        }
    };

    let projections: Result<Vec<Expression>, EmissionError> =
        projection.into_iter().map(lower_select_item).collect();
    let projections = projections?;
    // A.2 treats the aggregate projection list as the aggregate output list.
    // τ's emission substrate refines this into the {grouping, aggregates} split when the
    // canonical emission table lands; for now we push everything into
    // `aggregates` so the round-trip test can inspect the projection list.
    let aggregated = CommonAst::new(CommonOp::Aggregate {
        input: Box::new(input),
        grouping,
        aggregates: projections,
        grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
    });

    if let Some(h) = having {
        Ok(CommonAst::new(CommonOp::Filter {
            input: Box::new(aggregated),
            condition: lower_expr(h)?,
        }))
    } else {
        Ok(aggregated)
    }
}

fn lower_from(from: Vec<TableWithJoins>) -> Result<CommonAst, EmissionError> {
    if from.is_empty() {
        return Ok(CommonAst::new(CommonOp::SingleRow));
    }
    let mut plans: Vec<CommonAst> = from
        .into_iter()
        .map(lower_table_with_joins)
        .collect::<Result<_, _>>()?;
    let first = plans.remove(0);
    plans.into_iter().try_fold(first, |acc, next| {
        Ok(CommonAst::new(CommonOp::Join {
            left: Box::new(acc),
            right: Box::new(next),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        }))
    })
}

fn lower_table_with_joins(twj: TableWithJoins) -> Result<CommonAst, EmissionError> {
    let mut plan = lower_table_factor(twj.relation)?;
    for join in twj.joins {
        let right = lower_table_factor(join.relation)?;
        let (join_type, condition, using_columns) = lower_join_operator(join.join_operator)?;
        plan = CommonAst::new(CommonOp::Join {
            left: Box::new(plan),
            right: Box::new(right),
            join_type,
            condition,
            using_columns,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
    }
    Ok(plan)
}

fn lower_table_factor(factor: TableFactor) -> Result<CommonAst, EmissionError> {
    match factor {
        TableFactor::Table { name, alias, .. } => Ok(CommonAst::new(CommonOp::TableScan {
            table: object_name_to_string(&name),
            alias: alias.map(|a| a.name.value),
        })),
        TableFactor::Derived {
            subquery, alias: _, ..
        } => {
            // τ lowers subquery-in-FROM by inlining the inner plan.
            // AliasedRelation is a deferred variant; the alias
            // is discarded here — the analyzer will re-resolve.
            lower_query(*subquery)
        }
        TableFactor::TableFunction { expr, alias: _ } => {
            // Only bare identifier / function-call table functions covered.
            match expr {
                Expr::Function(f) => lower_table_function(f),
                other => bail_boundary_proto!(
                    format!("sql::table_function::{other:?}"),
                    "table function expr shape not supported by τ\'s SparkSQL parser",
                ),
            }
        }
        TableFactor::UNNEST {
            array_exprs,
            with_ordinality,
            ..
        } => {
            if array_exprs.len() != 1 {
                bail_boundary_proto!(
                    "sql::unnest_multi_arg",
                    "UNNEST with multiple array arguments not supported by τ\'s SparkSQL parser",
                );
            }
            let expr =
                array_exprs
                    .into_iter()
                    .next()
                    .ok_or_else(|| EmissionError::Unsupported {
                        kind: UnsupportedKind::ProtoShape,
                        name: "sql::unnest_empty".to_owned(),
                        reason: "UNNEST has no array argument".to_owned(),
                    })?;
            Ok(CommonAst::new(CommonOp::Unnest {
                expr: lower_expr(expr)?,
                with_ordinality,
            }))
        }
        TableFactor::Function {
            name,
            args,
            alias: _,
            ..
        } => {
            let func_name = object_name_to_string(&name);
            let arg_exprs: Vec<Expression> = args
                .into_iter()
                .map(function_arg_to_expr)
                .collect::<Result<_, _>>()?;
            Ok(CommonAst::new(CommonOp::TableFunction {
                name: func_name,
                args: arg_exprs,
                with_ordinality: false,
            }))
        }
        other => bail_boundary_proto!(
            format!("sql::table_factor::{other:?}"),
            "table factor not supported by τ\'s SparkSQL parser",
        ),
    }
}

fn lower_table_function(f: Function) -> Result<CommonAst, EmissionError> {
    let name = object_name_to_string(&f.name);
    let args = lower_function_args(f.args)?;
    Ok(CommonAst::new(CommonOp::TableFunction {
        name,
        args,
        with_ordinality: false,
    }))
}

fn lower_function_args(args: FunctionArguments) -> Result<Vec<Expression>, EmissionError> {
    match args {
        FunctionArguments::None => Ok(vec![]),
        FunctionArguments::Subquery(_) => bail_boundary_proto!(
            "sql::function_args_subquery",
            "subquery function arguments not implemented in τ\'s SparkSQL parser",
        ),
        FunctionArguments::List(list) => list
            .args
            .into_iter()
            .map(function_arg_to_expr)
            .collect::<Result<_, _>>(),
    }
}

fn function_arg_to_expr(arg: FunctionArg) -> Result<Expression, EmissionError> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => lower_expr(e),
        FunctionArg::Named {
            arg: FunctionArgExpr::Expr(e),
            ..
        } => lower_expr(e),
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
            Ok(Expression::Star(StarExpression { qualifier: None }))
        }
        FunctionArg::Unnamed(FunctionArgExpr::QualifiedWildcard(name)) => {
            Ok(Expression::Star(StarExpression {
                qualifier: Some(object_name_to_string(&name)),
            }))
        }
        other => bail_boundary_proto!(
            format!("sql::function_arg::{other:?}"),
            "function argument shape not supported by τ\'s SparkSQL parser",
        ),
    }
}

fn lower_join_operator(
    op: JoinOperator,
) -> Result<(JoinType, Option<Expression>, Vec<String>), EmissionError> {
    let (join_type, constraint) = match op {
        JoinOperator::Join(c) | JoinOperator::Inner(c) => (JoinType::Inner, c),
        JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => (JoinType::Left, c),
        JoinOperator::Right(c) | JoinOperator::RightOuter(c) => (JoinType::Right, c),
        JoinOperator::FullOuter(c) => (JoinType::Full, c),
        JoinOperator::CrossJoin(c) => (JoinType::Cross, c),
        JoinOperator::LeftSemi(c) => (JoinType::LeftSemi, c),
        JoinOperator::LeftAnti(c) => (JoinType::LeftAnti, c),
        other => {
            bail_boundary_proto!(
                format!("sql::join_operator::{other:?}"),
                "join operator not supported by τ\'s SparkSQL parser",
            );
        }
    };
    let (cond, using) = lower_join_constraint(constraint)?;
    Ok((join_type, cond, using))
}

fn lower_join_constraint(
    constraint: JoinConstraint,
) -> Result<(Option<Expression>, Vec<String>), EmissionError> {
    match constraint {
        JoinConstraint::On(expr) => Ok((Some(lower_expr(expr)?), vec![])),
        JoinConstraint::Using(cols) => {
            let names: Vec<String> = cols.iter().map(object_name_to_string).collect();
            Ok((None, names))
        }
        JoinConstraint::Natural | JoinConstraint::None => Ok((None, vec![])),
    }
}

fn lower_select_item(item: SelectItem) -> Result<Expression, EmissionError> {
    match item {
        SelectItem::UnnamedExpr(expr) => lower_expr(expr),
        SelectItem::ExprWithAlias { expr, alias } => {
            let inner = lower_expr(expr)?;
            Ok(Expression::Alias(AliasExpression {
                expr: Box::new(inner),
                alias: alias.value,
            }))
        }
        SelectItem::Wildcard(_) => Ok(Expression::Star(StarExpression { qualifier: None })),
        SelectItem::QualifiedWildcard(kind, _) => {
            use sqlparser::ast::SelectItemQualifiedWildcardKind;
            let q = match &kind {
                SelectItemQualifiedWildcardKind::ObjectName(n) => object_name_to_string(n),
                SelectItemQualifiedWildcardKind::Expr(e) => e.to_string(),
            };
            Ok(Expression::Star(StarExpression { qualifier: Some(q) }))
        }
    }
}

fn select_item_has_aggregate(item: &SelectItem) -> bool {
    match item {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
            expr_has_aggregate(e)
        }
        _ => false,
    }
}

fn expr_has_aggregate(expr: &Expr) -> bool {
    // τ fix pass (review M4): extend the walker to every composite
    // shape the projection can contain. A missed shape used to mis-classify
    // e.g. `SELECT count(x) IN (1, 2)` as non-aggregate.
    match expr {
        Expr::Function(f) => f.over.is_none() && is_aggregate_function_name(&f.name.to_string()),
        Expr::BinaryOp { left, right, .. } => expr_has_aggregate(left) || expr_has_aggregate(right),
        Expr::UnaryOp { expr, .. } => expr_has_aggregate(expr),
        Expr::Nested(e) => expr_has_aggregate(e),
        Expr::Cast { expr, .. } => expr_has_aggregate(expr),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            operand.as_deref().is_some_and(expr_has_aggregate)
                || conditions
                    .iter()
                    .any(|c| expr_has_aggregate(&c.condition) || expr_has_aggregate(&c.result))
                || else_result.as_deref().is_some_and(expr_has_aggregate)
        }
        Expr::InList { expr, list, .. } => {
            expr_has_aggregate(expr) || list.iter().any(expr_has_aggregate)
        }
        Expr::InSubquery { expr, .. } => expr_has_aggregate(expr),
        Expr::Between {
            expr, low, high, ..
        } => expr_has_aggregate(expr) || expr_has_aggregate(low) || expr_has_aggregate(high),
        Expr::Like {
            expr,
            pattern,
            any: _,
            ..
        }
        | Expr::ILike {
            expr,
            pattern,
            any: _,
            ..
        }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => {
            expr_has_aggregate(expr) || expr_has_aggregate(pattern)
        }
        Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsTrue(e)
        | Expr::IsNotTrue(e)
        | Expr::IsFalse(e)
        | Expr::IsNotFalse(e)
        | Expr::IsUnknown(e)
        | Expr::IsNotUnknown(e) => expr_has_aggregate(e),
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            expr_has_aggregate(a) || expr_has_aggregate(b)
        }
        Expr::Tuple(items) | Expr::Array(sqlparser::ast::Array { elem: items, .. }) => {
            items.iter().any(expr_has_aggregate)
        }
        Expr::Collate { expr, .. }
        | Expr::AtTimeZone {
            timestamp: expr, ..
        } => expr_has_aggregate(expr),
        // Leaves and shapes that can't syntactically host an aggregate at
        // A.2 (identifiers, literals, subqueries, wildcards, GROUPING SETS,
        // interval/map/tuple/JSON access, etc.) contribute no aggregate.
        _ => false,
    }
}

fn is_aggregate_function_name(name: &str) -> bool {
    // τ fix pass (review M3 + perf OPT-5): defer to τ's canonical
    // aggregate roster (`transpiler_v2::type_inference::AGGREGATE_NAMES`)
    // instead of a locally-drifted 32-name subset. `eq_ignore_ascii_case`
    // avoids the per-call `String` allocation from `to_ascii_uppercase()`.
    AGGREGATE_NAMES.iter().any(|a| name.eq_ignore_ascii_case(a))
}

fn lower_expr(expr: Expr) -> Result<Expression, EmissionError> {
    match expr {
        Expr::Identifier(ident) => Ok(Expression::UnresolvedColumn(UnresolvedColumn {
            name: ident.value,
            qualifier: None,
            plan_id: None,
        })),
        Expr::CompoundIdentifier(parts) => {
            let values: Vec<String> = parts.iter().map(|i| i.value.clone()).collect();
            let (qualifier, name) = if values.len() >= 2 {
                (
                    Some(values[values.len() - 2].clone()),
                    values[values.len() - 1].clone(),
                )
            } else {
                (None, values.into_iter().last().unwrap_or_default())
            };
            Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name,
                qualifier,
                plan_id: None,
            }))
        }
        Expr::Value(vw) => lower_value(vw),
        Expr::BinaryOp { left, op, right } => {
            // Spark's `a DIV b` integer-division operator lowers to a truncating
            // integer divide. Emit as `CAST(a / b AS BIGINT)` — DuckDB's `/`
            // on integer operands truncates, matching Spark's semantics for
            // integral inputs. The projection-slot Spark-return-cast keeps
            // the outer type consistent. Corpus witness: `type-007`.
            if matches!(op, BinaryOperator::MyIntegerDivide) {
                let l = lower_expr(*left)?;
                let r = lower_expr(*right)?;
                return Ok(Expression::Cast(CastExpression {
                    expr: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Div,
                        left: Box::new(l),
                        right: Box::new(r),
                    })),
                    to_type: DataType::Long,
                    try_cast: false,
                }));
            }
            Ok(Expression::Binary(BinaryExpression {
                op: lower_binary_op(op)?,
                left: Box::new(lower_expr(*left)?),
                right: Box::new(lower_expr(*right)?),
            }))
        }
        Expr::UnaryOp { op, expr } => match op {
            UnaryOperator::Not => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::Not,
                operand: Box::new(lower_expr(*expr)?),
            })),
            UnaryOperator::Minus => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::Negate,
                operand: Box::new(lower_expr(*expr)?),
            })),
            UnaryOperator::Plus => lower_expr(*expr),
            other => bail_boundary_proto!(
                format!("sql::unary_op::{other:?}"),
                "unary operator not supported by τ\'s SparkSQL parser",
            ),
        },
        Expr::Nested(e) => lower_expr(*e),
        Expr::Cast {
            kind,
            expr,
            data_type,
            ..
        } => {
            let try_cast = matches!(kind, CastKind::TryCast | CastKind::SafeCast);
            Ok(Expression::Cast(CastExpression {
                expr: Box::new(lower_expr(*expr)?),
                to_type: lower_data_type(data_type)?,
                try_cast,
            }))
        }
        Expr::Function(f) => lower_function(f),
        Expr::Interval(iv) => lower_interval(iv),
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            let branches = conditions
                .into_iter()
                .map(|c| Ok((lower_expr(c.condition)?, lower_expr(c.result)?)))
                .collect::<Result<Vec<_>, EmissionError>>()?;
            let else_expr = else_result
                .map(|e| lower_expr(*e))
                .transpose()?
                .map(Box::new);
            Ok(Expression::CaseWhen(CaseWhenExpression {
                branches,
                else_expr,
            }))
        }
        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let converted_list: Result<Vec<Expression>, EmissionError> =
                list.into_iter().map(lower_expr).collect();
            Ok(Expression::InList(InListExpression {
                expr: Box::new(lower_expr(*expr)?),
                list: converted_list?,
                negated,
            }))
        }
        Expr::IsNull(e) => Ok(Expression::Unary(UnaryExpression {
            op: UnaryOp::IsNull,
            operand: Box::new(lower_expr(*e)?),
        })),
        Expr::IsNotNull(e) => Ok(Expression::Unary(UnaryExpression {
            op: UnaryOp::IsNotNull,
            operand: Box::new(lower_expr(*e)?),
        })),
        Expr::Like {
            expr,
            pattern,
            negated,
            escape_char,
            ..
        } => Ok(Expression::Like(LikeExpression {
            value: Box::new(lower_expr(*expr)?),
            pattern: Box::new(lower_expr(*pattern)?),
            escape: escape_char.and_then(value_to_escape_char),
            negated,
            case_insensitive: false,
        })),
        Expr::Wildcard(_) => Ok(Expression::Star(StarExpression { qualifier: None })),
        // Spark's `EXTRACT(<field> FROM <expr>)` and `DATE_PART(<field>, <expr>)`
        // parse to `Expr::Extract`. Lower to a FunctionCall of
        // `date_part('<field>', <expr>)` — DuckDB accepts this form for all
        // date/timestamp fields (year, month, day, hour, ...). Corpus
        // witness: `dt-016` (`extract(YEAR FROM hire_date)`).
        Expr::Extract { field, expr, .. } => {
            // Spark's `EXTRACT(<field> FROM <expr>)` lowers to a direct
            // function call — `year(x)`, `month(x)`, `day(x)`, etc. — so
            // that the existing type_inference / emission arms for those
            // functions apply (they return INTEGER, matching Spark).
            // Fall back to `date_part('<field>', <expr>)` (DOUBLE return)
            // only for fields without a dedicated Spark function name.
            // Corpus witness: `dt-016` (`extract(YEAR FROM hire_date)`).
            let field_str = format!("{field}").to_lowercase();
            let inner = lower_expr(*expr)?;
            let (fn_name, use_date_part) = match field_str.as_str() {
                "year" => ("year", false),
                "month" => ("month", false),
                "day" | "dayofmonth" => ("day", false),
                "hour" => ("hour", false),
                "minute" => ("minute", false),
                "second" => ("second", false),
                "quarter" => ("quarter", false),
                "week" | "weekofyear" => ("weekofyear", false),
                "dayofweek" => ("dayofweek", false),
                "dayofyear" => ("dayofyear", false),
                _ => ("date_part", true),
            };
            let args = if use_date_part {
                vec![
                    Expression::Literal(Literal {
                        value: LiteralValue::String(field_str),
                        data_type: DataType::String,
                    }),
                    inner,
                ]
            } else {
                vec![inner]
            };
            Ok(Expression::FunctionCall(FunctionCall {
                name: fn_name.to_owned(),
                args,
                distinct: false,
            }))
        }
        Expr::Lambda(lambda) => {
            let params: Vec<String> = lambda.params.iter().map(|p| p.value.clone()).collect();
            let body = lower_expr(*lambda.body)?;
            // SparkSQL parses lambda-body identifiers as regular columns
            // (`Expr::Identifier("acc")` → `UnresolvedColumn(acc)`). The
            // analyzer treats `Lambda` opaquely (analyzer.rs:1747), so those
            // references never resolve. Rewrite them to `LambdaVariable`
            // so emission (emission.rs:1681) renders them as DuckDB lambda
            // parameters (`acc`, `x`). The protobuf front-end never hits this
            // — it receives `UnresolvedNamedLambdaVariable` directly.
            let body = rewrite_lambda_params_to_vars(body, &params);
            Ok(Expression::Lambda(LambdaExpression {
                params,
                body: Box::new(body),
            }))
        }
        other => bail_boundary_proto!(
            format!("sql::expr::{}", expr_kind(&other)),
            "expression shape not supported by τ\'s SparkSQL parser",
        ),
    }
}

/// Walk a lowered lambda body and replace every `UnresolvedColumn` whose name
/// matches one of `params` (and whose qualifier is `None`) with a
/// `LambdaVariable` of the same name. Handles nested Lambdas via shadowing:
/// if an inner lambda re-binds one of our params, that param is removed from
/// the "still-active" set for that subtree.
fn rewrite_lambda_params_to_vars(body: Expression, params: &[String]) -> Expression {
    if params.is_empty() {
        return body;
    }
    match body {
        Expression::UnresolvedColumn(u)
            if u.qualifier.is_none() && params.iter().any(|p| p == &u.name) =>
        {
            Expression::LambdaVariable(LambdaVariableExpression { name: u.name })
        }
        Expression::Binary(b) => Expression::Binary(BinaryExpression {
            op: b.op,
            left: Box::new(rewrite_lambda_params_to_vars(*b.left, params)),
            right: Box::new(rewrite_lambda_params_to_vars(*b.right, params)),
        }),
        Expression::Unary(u) => Expression::Unary(UnaryExpression {
            op: u.op,
            operand: Box::new(rewrite_lambda_params_to_vars(*u.operand, params)),
        }),
        Expression::FunctionCall(fc) => Expression::FunctionCall(FunctionCall {
            name: fc.name,
            args: fc
                .args
                .into_iter()
                .map(|a| rewrite_lambda_params_to_vars(a, params))
                .collect(),
            distinct: fc.distinct,
        }),
        Expression::Cast(c) => Expression::Cast(CastExpression {
            expr: Box::new(rewrite_lambda_params_to_vars(*c.expr, params)),
            to_type: c.to_type,
            try_cast: c.try_cast,
        }),
        Expression::CaseWhen(cw) => Expression::CaseWhen(CaseWhenExpression {
            branches: cw
                .branches
                .into_iter()
                .map(|(w, t)| {
                    (
                        rewrite_lambda_params_to_vars(w, params),
                        rewrite_lambda_params_to_vars(t, params),
                    )
                })
                .collect(),
            else_expr: cw
                .else_expr
                .map(|e| Box::new(rewrite_lambda_params_to_vars(*e, params))),
        }),
        Expression::Alias(a) => Expression::Alias(AliasExpression {
            expr: Box::new(rewrite_lambda_params_to_vars(*a.expr, params)),
            alias: a.alias,
        }),
        Expression::Lambda(inner) => {
            // Inner lambda's params shadow ours: drop them from the active set
            // before descending into the inner body.
            let remaining: Vec<String> = params
                .iter()
                .filter(|p| !inner.params.iter().any(|ip| ip == *p))
                .cloned()
                .collect();
            let new_body = rewrite_lambda_params_to_vars(*inner.body, &remaining);
            Expression::Lambda(LambdaExpression {
                params: inner.params,
                body: Box::new(new_body),
            })
        }
        Expression::Between(b) => {
            Expression::Between(crate::transpiler_v2::expression::BetweenExpression {
                expr: Box::new(rewrite_lambda_params_to_vars(*b.expr, params)),
                low: Box::new(rewrite_lambda_params_to_vars(*b.low, params)),
                high: Box::new(rewrite_lambda_params_to_vars(*b.high, params)),
                negated: b.negated,
            })
        }
        Expression::InList(i) => Expression::InList(InListExpression {
            expr: Box::new(rewrite_lambda_params_to_vars(*i.expr, params)),
            list: i
                .list
                .into_iter()
                .map(|e| rewrite_lambda_params_to_vars(e, params))
                .collect(),
            negated: i.negated,
        }),
        Expression::Like(l) => Expression::Like(LikeExpression {
            value: Box::new(rewrite_lambda_params_to_vars(*l.value, params)),
            pattern: Box::new(rewrite_lambda_params_to_vars(*l.pattern, params)),
            escape: l.escape,
            case_insensitive: l.case_insensitive,
            negated: l.negated,
        }),
        Expression::IsDistinctFrom(d) => {
            Expression::IsDistinctFrom(crate::transpiler_v2::expression::IsDistinctFromExpression {
                left: Box::new(rewrite_lambda_params_to_vars(*d.left, params)),
                right: Box::new(rewrite_lambda_params_to_vars(*d.right, params)),
                negated: d.negated,
            })
        }
        Expression::ExtractValue(ev) => {
            Expression::ExtractValue(crate::transpiler_v2::expression::ExtractValueExpression {
                child: Box::new(rewrite_lambda_params_to_vars(*ev.child, params)),
                extraction: Box::new(rewrite_lambda_params_to_vars(*ev.extraction, params)),
            })
        }
        Expression::ArrayLiteral(a) => {
            Expression::ArrayLiteral(crate::transpiler_v2::expression::ArrayLiteralExpression {
                element_type: a.element_type,
                elements: a
                    .elements
                    .into_iter()
                    .map(|e| rewrite_lambda_params_to_vars(e, params))
                    .collect(),
            })
        }
        // Leaf variants + shapes that don't carry column refs in typical
        // lambda bodies: return unchanged. Subqueries/windows/etc. inside a
        // Spark lambda body would themselves be `UnsupportedProtoShape` from
        // upstream lower_expr — never reaching this rewrite.
        other => other,
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Function(_) => "function",
        Expr::Subquery(_) => "subquery",
        Expr::Exists { .. } => "exists",
        Expr::InSubquery { .. } => "in_subquery",
        Expr::Between { .. } => "between",
        Expr::AnyOp { .. } => "any_op",
        Expr::AllOp { .. } => "all_op",
        Expr::Tuple(_) => "tuple",
        Expr::Array(_) => "array",
        Expr::Map(_) => "map",
        Expr::Interval(_) => "interval",
        Expr::Rollup(_) => "rollup",
        Expr::Cube(_) => "cube",
        Expr::GroupingSets(_) => "grouping_sets",
        Expr::Lambda(_) => "lambda",
        _ => "other",
    }
}

fn lower_binary_op(op: BinaryOperator) -> Result<BinaryOp, EmissionError> {
    Ok(match op {
        BinaryOperator::Plus => BinaryOp::Add,
        BinaryOperator::Minus => BinaryOp::Sub,
        BinaryOperator::Multiply => BinaryOp::Mul,
        BinaryOperator::Divide => BinaryOp::Div,
        BinaryOperator::Modulo => BinaryOp::Mod,
        BinaryOperator::Eq => BinaryOp::Eq,
        BinaryOperator::NotEq => BinaryOp::NotEq,
        BinaryOperator::Lt => BinaryOp::Lt,
        BinaryOperator::LtEq => BinaryOp::LtEq,
        BinaryOperator::Gt => BinaryOp::Gt,
        BinaryOperator::GtEq => BinaryOp::GtEq,
        BinaryOperator::And => BinaryOp::And,
        BinaryOperator::Or => BinaryOp::Or,
        BinaryOperator::StringConcat => BinaryOp::Concat,
        BinaryOperator::BitwiseAnd => BinaryOp::BitAnd,
        BinaryOperator::BitwiseOr => BinaryOp::BitOr,
        BinaryOperator::BitwiseXor => BinaryOp::BitXor,
        other => {
            bail_boundary_proto!(
                format!("sql::binary_op::{other:?}"),
                "binary operator not supported by τ\'s SparkSQL parser",
            );
        }
    })
}

fn lower_function(f: Function) -> Result<Expression, EmissionError> {
    let Function {
        name, args, over, ..
    } = f;
    let fn_name = object_name_to_string(&name);
    // Spark's `timestampadd(unit, quantity, ts)` / `timestampdiff(unit, start,
    // end)` carry the datetime-field UNIT (`MONTH`, `DAY`, …) as their first
    // argument, which sqlparser parses as `Expr::Identifier("MONTH")`. The
    // generic identifier arm (`lower_expr`, ~line 611) would lower that into an
    // `UnresolvedColumn`, so the analyzer would raise a spurious
    // `UnknownColumn { name: "MONTH" }`. Demote the unit to a string literal
    // (mirrors the `Expr::Extract` arm) and lower the remaining args through
    // the normal `function_arg_to_expr` path. Neither function takes an
    // `OVER (...)` clause.
    if fn_name.eq_ignore_ascii_case("timestampadd") || fn_name.eq_ignore_ascii_case("timestampdiff")
    {
        if over.is_some() {
            bail_boundary_proto!(
                format!("sql::window::{fn_name}"),
                "OVER is not valid on timestampadd/timestampdiff",
            );
        }
        return lower_timestamp_unit_fn(fn_name, args);
    }
    let (distinct, call_args) = lower_call_args(args)?;
    let call = Expression::FunctionCall(FunctionCall {
        name: fn_name,
        args: call_args,
        distinct,
    });
    match over {
        None => Ok(call),
        Some(WindowType::WindowSpec(spec)) => lower_window(call, spec),
        // Named-window references are inlined during `lower_select` (the only
        // node that carries the `WINDOW w AS (...)` map). Reaching here means
        // the reference sat somewhere the inline pre-pass did not descend into
        // (or the window name was unknown). Surface an honest boundary error
        // rather than silently dropping the window semantics (ADR-022).
        Some(WindowType::NamedWindow(ident)) => Err(EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: "sql::window::unresolved_named_window".to_owned(),
            reason: format!("window `{}` referenced but not resolvable", ident.value),
        }),
    }
}

/// Lower `timestampadd(unit, quantity, ts)` / `timestampdiff(unit, start, end)`.
///
/// The leading datetime-field UNIT is demoted from the identifier/string it
/// parses as into an `Expression::Literal(String)`, so the analyzer never
/// mistakes it for a column reference. The remaining arguments lower through
/// the normal [`function_arg_to_expr`] path.
fn lower_timestamp_unit_fn(
    fn_name: String,
    args: FunctionArguments,
) -> Result<Expression, EmissionError> {
    let list = match args {
        FunctionArguments::List(list) => list,
        _ => bail_boundary_proto!(
            format!("sql::{fn_name}::args"),
            "timestampadd/timestampdiff require a positional argument list",
        ),
    };
    let mut arg_iter = list.args.into_iter();
    let unit_arg = arg_iter.next().ok_or_else(|| EmissionError::Unsupported {
        kind: UnsupportedKind::ProtoShape,
        name: format!("sql::{fn_name}::unit"),
        reason: format!("`{fn_name}` requires a leading datetime unit argument"),
    })?;
    let mut lowered = Vec::with_capacity(3);
    lowered.push(lower_timestamp_unit_arg(&fn_name, unit_arg)?);
    for a in arg_iter {
        lowered.push(function_arg_to_expr(a)?);
    }
    Ok(Expression::FunctionCall(FunctionCall {
        name: fn_name,
        args: lowered,
        distinct: false,
    }))
}

/// Lower the leading UNIT argument of `timestampadd` / `timestampdiff` into a
/// string [`Literal`]. Accepts a bare field name (`MONTH`) — sqlparser's
/// `Expr::Identifier` — or a quoted string literal (`'MONTH'`).
fn lower_timestamp_unit_arg(fn_name: &str, arg: FunctionArg) -> Result<Expression, EmissionError> {
    let expr = match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
        FunctionArg::Named {
            arg: FunctionArgExpr::Expr(e),
            ..
        } => e,
        other => bail_boundary_proto!(
            format!("sql::{fn_name}::unit::{other:?}"),
            "datetime unit must be a bare field name or string literal",
        ),
    };
    match expr {
        Expr::Identifier(ident) => Ok(Expression::Literal(Literal {
            value: LiteralValue::String(ident.value),
            data_type: DataType::String,
        })),
        // A quoted string unit (`timestampadd('MONTH', …)`) lowers via the
        // normal value path; accept it only if it yields a string literal.
        other => {
            let lowered = lower_expr(other)?;
            if matches!(
                lowered,
                Expression::Literal(Literal {
                    value: LiteralValue::String(_),
                    ..
                })
            ) {
                Ok(lowered)
            } else {
                bail_boundary_proto!(
                    format!("sql::{fn_name}::unit"),
                    "datetime unit must be a bare field name or string literal",
                )
            }
        }
    }
}

/// Lower a function's argument list into `(distinct, args)` — the DISTINCT-aware
/// variant used by the call/window lowering path.
fn lower_call_args(args: FunctionArguments) -> Result<(bool, Vec<Expression>), EmissionError> {
    match args {
        FunctionArguments::None => Ok((false, vec![])),
        FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment,
            args,
            ..
        }) => {
            let distinct = matches!(duplicate_treatment, Some(DuplicateTreatment::Distinct));
            let converted: Result<Vec<Expression>, EmissionError> =
                args.into_iter().map(function_arg_to_expr).collect();
            Ok((distinct, converted?))
        }
        FunctionArguments::Subquery(_) => bail_boundary_proto!(
            "sql::function_args_subquery",
            "subquery function arguments not implemented in τ\'s SparkSQL parser",
        ),
    }
}

/// Wrap an already-lowered function call in an `Expression::Window`, lowering
/// the `OVER (...)` window spec. Mirrors the Spark Connect DataFrame path
/// (`v2_relation_converter.rs::ExprType::Window`): same target type, same
/// `partition_by` / `order_by` / `frame` shape. sqlparser encodes the frame
/// direction in the bound *variant* (PRECEDING/FOLLOWING) and gives the offset
/// as an unsigned magnitude, so — unlike the proto path — no sign inference is
/// applied.
fn lower_window(func: Expression, spec: WindowSpec) -> Result<Expression, EmissionError> {
    let WindowSpec {
        partition_by,
        order_by,
        window_frame,
        ..
    } = spec;
    let partition_by = partition_by
        .into_iter()
        .map(lower_expr)
        .collect::<Result<Vec<_>, _>>()?;
    let order_by = order_by
        .into_iter()
        .map(lower_order_by_expr)
        .collect::<Result<Vec<_>, _>>()?;
    let frame = window_frame.map(lower_window_frame).transpose()?;
    Ok(Expression::Window(WindowFunction {
        func: Box::new(func),
        partition_by,
        order_by,
        frame,
    }))
}

/// Lower a sqlparser `WindowFrame` into τ's [`WindowFrame`]. `GROUPS` frames
/// have no τ representation and are rejected. A missing `end_bound`
/// (shorthand `ROWS N PRECEDING`) means the upper bound is `CURRENT ROW`.
fn lower_window_frame(frame: SqlWindowFrame) -> Result<WindowFrame, EmissionError> {
    let SqlWindowFrame {
        units,
        start_bound,
        end_bound,
    } = frame;
    let unit = match units {
        WindowFrameUnits::Rows => FrameUnit::Rows,
        WindowFrameUnits::Range => FrameUnit::Range,
        WindowFrameUnits::Groups => {
            bail_boundary_proto!(
                "sql::window_frame::groups",
                "GROUPS window frames are not supported",
            );
        }
    };
    let lower = lower_frame_bound(start_bound)?;
    let upper = match end_bound {
        Some(b) => lower_frame_bound(b)?,
        None => FrameBoundary::CurrentRow,
    };
    Ok(WindowFrame { unit, lower, upper })
}

/// Lower a single sqlparser `WindowFrameBound`. The bound value is taken as the
/// absolute offset magnitude — the PRECEDING/FOLLOWING direction already lives
/// in the variant; no sign logic is re-applied.
fn lower_frame_bound(bound: SqlWindowFrameBound) -> Result<FrameBoundary, EmissionError> {
    match bound {
        SqlWindowFrameBound::CurrentRow => Ok(FrameBoundary::CurrentRow),
        SqlWindowFrameBound::Preceding(None) => Ok(FrameBoundary::UnboundedPreceding),
        SqlWindowFrameBound::Following(None) => Ok(FrameBoundary::UnboundedFollowing),
        SqlWindowFrameBound::Preceding(Some(e)) => {
            Ok(FrameBoundary::Preceding(Box::new(lower_expr(*e)?)))
        }
        SqlWindowFrameBound::Following(Some(e)) => {
            Ok(FrameBoundary::Following(Box::new(lower_expr(*e)?)))
        }
    }
}

/// Lower a sqlparser `INTERVAL '<n>' <unit>` literal into τ's normalized
/// [`IntervalExpression`] (months / days / microseconds). Per ADR-022,
/// rejects shapes τ cannot represent with a boundary error instead of
/// falling back to raw SQL.
fn lower_interval(iv: Interval) -> Result<Expression, EmissionError> {
    let field = iv.leading_field.as_ref().require_proto(
        "sql::interval::no_leading_field",
        "INTERVAL without a unit is not supported",
    )?;
    if iv.last_field.is_some() {
        bail_boundary_proto!(
            "sql::interval::compound",
            "compound (e.g. YEAR TO MONTH) intervals are not supported",
        );
    }
    let n = extract_interval_int(&iv.value).require_proto(
        "sql::interval::non_literal_value",
        "INTERVAL value must be an integer literal",
    )?;

    const MICROS_PER_SECOND: i64 = 1_000_000;
    const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
    const MICROS_PER_HOUR: i64 = 60 * MICROS_PER_MINUTE;

    let overflow = || EmissionError::Unsupported {
        kind: UnsupportedKind::ProtoShape,
        name: "sql::interval::overflow".to_owned(),
        reason: "INTERVAL magnitude overflows its normalized representation".to_owned(),
    };

    let ie = match field {
        DateTimeField::Year | DateTimeField::Years => IntervalExpression {
            months: n.checked_mul(12).ok_or_else(overflow)?,
            days: 0,
            microseconds: 0,
        },
        DateTimeField::Month | DateTimeField::Months => IntervalExpression {
            months: n,
            days: 0,
            microseconds: 0,
        },
        DateTimeField::Day | DateTimeField::Days => IntervalExpression {
            months: 0,
            days: n,
            microseconds: 0,
        },
        DateTimeField::Hour | DateTimeField::Hours => IntervalExpression {
            months: 0,
            days: 0,
            microseconds: i64::from(n)
                .checked_mul(MICROS_PER_HOUR)
                .ok_or_else(overflow)?,
        },
        DateTimeField::Minute | DateTimeField::Minutes => IntervalExpression {
            months: 0,
            days: 0,
            microseconds: i64::from(n)
                .checked_mul(MICROS_PER_MINUTE)
                .ok_or_else(overflow)?,
        },
        DateTimeField::Second | DateTimeField::Seconds => IntervalExpression {
            months: 0,
            days: 0,
            microseconds: i64::from(n)
                .checked_mul(MICROS_PER_SECOND)
                .ok_or_else(overflow)?,
        },
        other => {
            bail_boundary_proto!(
                format!("sql::interval::unit::{other:?}"),
                "interval unit not representable (only YEAR/MONTH/DAY/HOUR/MINUTE/SECOND)",
            );
        }
    };
    Ok(Expression::Interval(ie))
}

/// Extract a plain `i32` from an interval value expression, handling both the
/// quoted (`'3'`) and bare-numeric (`3`) parser shapes.
fn extract_interval_int(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => s.parse::<i32>().ok(),
            Value::Number(s, _) => s.parse::<i32>().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Build a `name → WindowSpec` map from a `WINDOW w AS (...)` clause, resolving
/// `WINDOW w AS other_window` alias chains to a concrete spec. An unknown name
/// or a reference cycle surfaces as a boundary error.
fn build_named_window_map(
    defs: &[NamedWindowDefinition],
) -> Result<HashMap<String, WindowSpec>, EmissionError> {
    let raw: HashMap<&str, &NamedWindowExpr> = defs
        .iter()
        .map(|NamedWindowDefinition(name, expr)| (name.value.as_str(), expr))
        .collect();
    let mut resolved: HashMap<String, WindowSpec> = HashMap::with_capacity(defs.len());
    for NamedWindowDefinition(name, _) in defs {
        let spec = resolve_named_window(name.value.as_str(), &raw, 0)?;
        resolved.insert(name.value.clone(), spec);
    }
    Ok(resolved)
}

/// Resolve a named-window reference to a concrete [`WindowSpec`], following
/// `NamedWindow` alias chains. `depth` guards against reference cycles.
fn resolve_named_window(
    name: &str,
    raw: &HashMap<&str, &NamedWindowExpr>,
    depth: usize,
) -> Result<WindowSpec, EmissionError> {
    if depth > 64 {
        bail_boundary_proto!(
            "sql::window::named_window_cycle",
            format!("window `{name}` forms a reference cycle"),
        );
    }
    let expr = raw.get(name).require_proto(
        "sql::window::unknown_named_window",
        &format!("window `{name}` is not defined in the WINDOW clause"),
    )?;
    match expr {
        NamedWindowExpr::WindowSpec(spec) => Ok(spec.clone()),
        NamedWindowExpr::NamedWindow(other) => {
            resolve_named_window(other.value.as_str(), raw, depth + 1)
        }
    }
}

/// Rewrite every `OVER w` (`WindowType::NamedWindow`) in `expr` to its resolved
/// inline `WindowType::WindowSpec`. Descends the projection-expression shapes
/// that can wrap a window call; anything left unresolved surfaces as a boundary
/// error at [`lower_function`].
fn inline_named_windows(
    expr: &mut Expr,
    map: &HashMap<String, WindowSpec>,
) -> Result<(), EmissionError> {
    match expr {
        Expr::Function(f) => {
            if let Some(WindowType::NamedWindow(ident)) = &f.over {
                let spec =
                    map.get(ident.value.as_str())
                        .ok_or_else(|| EmissionError::Unsupported {
                            kind: UnsupportedKind::ProtoShape,
                            name: "sql::window::unknown_named_window".to_owned(),
                            reason: format!(
                                "window `{}` is not defined in the WINDOW clause",
                                ident.value
                            ),
                        })?;
                f.over = Some(WindowType::WindowSpec(spec.clone()));
            }
            if let Some(filter) = &mut f.filter {
                inline_named_windows(filter, map)?;
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            inline_named_windows(left, map)?;
            inline_named_windows(right, map)?;
        }
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::Cast { expr, .. }
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr) => inline_named_windows(expr, map)?,
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            for c in conditions.iter_mut() {
                inline_named_windows(&mut c.condition, map)?;
                inline_named_windows(&mut c.result, map)?;
            }
            if let Some(e) = else_result {
                inline_named_windows(e, map)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn lower_value(vw: ValueWithSpan) -> Result<Expression, EmissionError> {
    match vw.value {
        Value::Number(s, _) => {
            if let Ok(i) = s.parse::<i64>() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    Ok(Expression::Literal(Literal {
                        value: LiteralValue::Int(i as i32),
                        data_type: DataType::Integer,
                    }))
                } else {
                    Ok(Expression::Literal(Literal {
                        value: LiteralValue::Long(i),
                        data_type: DataType::Long,
                    }))
                }
            } else if let Ok(d) = s.parse::<f64>() {
                Ok(Expression::Literal(Literal {
                    value: LiteralValue::Double(d),
                    data_type: DataType::Double,
                }))
            } else {
                Err(EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name: "sql::number_parse".to_owned(),
                    reason: format!("cannot parse numeric literal `{s}`"),
                })
            }
        }
        Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => {
            Ok(Expression::Literal(Literal {
                value: LiteralValue::String(s),
                data_type: DataType::String,
            }))
        }
        Value::Boolean(b) => Ok(Expression::Literal(Literal {
            value: LiteralValue::Boolean(b),
            data_type: DataType::Boolean,
        })),
        Value::Null => Ok(Expression::Literal(Literal {
            value: LiteralValue::Null,
            data_type: DataType::Null,
        })),
        other => bail_boundary_proto!(
            format!("sql::value::{other:?}"),
            "literal value shape not supported by τ\'s SparkSQL parser",
        ),
    }
}

fn lower_data_type(dt: SqlDataType) -> Result<DataType, EmissionError> {
    use SqlDataType::*;
    Ok(match dt {
        Boolean | Bool => DataType::Boolean,
        TinyInt(_) | Int8(_) => DataType::Byte,
        SmallInt(_) | Int16 => DataType::Short,
        Int(_) | Integer(_) | Int32 => DataType::Integer,
        BigInt(_) | Int64 => DataType::Long,
        Real | Float(_) | Float32 => DataType::Float,
        Double(_) | DoublePrecision | Float64 => DataType::Double,
        Varchar(_) | Text | String(_) | Char(_) | CharacterVarying(_) => DataType::String,
        Bytea | Binary(_) | Varbinary(_) | Blob(_) => DataType::Binary,
        Date => DataType::Date,
        Timestamp(_, _) => DataType::Timestamp,
        Numeric(info) | Decimal(info) => decimal_from_exact_number(&info),
        other => {
            bail_boundary_proto!(
                format!("sql::data_type::{other:?}"),
                "data type not supported by τ\'s SparkSQL parser",
            );
        }
    })
}

fn decimal_from_exact_number(info: &ExactNumberInfo) -> DataType {
    match info {
        ExactNumberInfo::None => DataType::Decimal {
            precision: 38,
            scale: 18,
        },
        ExactNumberInfo::Precision(p) => DataType::Decimal {
            precision: (*p as u8).min(38),
            scale: 0,
        },
        ExactNumberInfo::PrecisionAndScale(p, s) => DataType::Decimal {
            precision: (*p as u8).min(38),
            scale: (*s as u8).min(38),
        },
    }
}

fn wrap_with_sort_limit(
    plan: CommonAst,
    order_by: Vec<OrderByExpr>,
    limit: Option<Expr>,
    offset: Option<Expr>,
) -> Result<CommonAst, EmissionError> {
    let limit_i = limit.map(expr_to_i64).transpose()?;
    let offset_i = offset.map(expr_to_i64).transpose()?;
    if order_by.is_empty() && limit_i.is_none() && offset_i.is_none() {
        return Ok(plan);
    }
    if order_by.is_empty() {
        if let Some(l) = limit_i {
            return Ok(CommonAst::new(CommonOp::Limit {
                input: Box::new(plan),
                limit: l,
                offset: offset_i,
            }));
        }
        // OFFSET-only.
        return Ok(CommonAst::new(CommonOp::Sort {
            input: Box::new(plan),
            order: vec![],
            limit: None,
            offset: offset_i,
        }));
    }
    let order = order_by
        .into_iter()
        .map(lower_order_by_expr)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CommonAst::new(CommonOp::Sort {
        input: Box::new(plan),
        order,
        limit: limit_i,
        offset: offset_i,
    }))
}

fn lower_order_by_expr(ob: OrderByExpr) -> Result<SortOrder, EmissionError> {
    let direction = match ob.options.asc {
        Some(true) | None => SortDirection::Ascending,
        Some(false) => SortDirection::Descending,
    };
    let null_ordering = match ob.options.nulls_first {
        Some(true) => NullOrdering::NullsFirst,
        Some(false) => NullOrdering::NullsLast,
        None => match direction {
            SortDirection::Ascending => NullOrdering::NullsFirst,
            SortDirection::Descending => NullOrdering::NullsLast,
        },
    };
    Ok(SortOrder {
        expr: Box::new(lower_expr(ob.expr)?),
        direction,
        null_ordering,
    })
}

fn expr_to_i64(e: Expr) -> Result<i64, EmissionError> {
    match e {
        Expr::Value(ValueWithSpan {
            value: Value::Number(s, _),
            ..
        }) => s.parse::<i64>().map_err(|_| EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: "sql::limit_offset_parse".to_owned(),
            reason: format!("cannot parse LIMIT/OFFSET value `{s}` as i64"),
        }),
        other => bail_boundary_proto!(
            format!("sql::limit_offset_expr::{other:?}"),
            "LIMIT/OFFSET must be an integer literal",
        ),
    }
}

fn object_name_to_string(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(id) => id.value.clone(),
            // ObjectNamePart::Function is a non-exhaustive tail variant for
            // Snowflake-style function-in-name syntax. Not reachable from
            // the SparkSQL shapes at A.2 — render its Display form so we
            // never silently drop information.
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn value_to_escape_char(v: Value) -> Option<char> {
    match v {
        Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => s.chars().next(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser_v2::dialect::SparkDialect;
    use sqlparser::parser::Parser;

    fn parse(sql: &str) -> Result<CommonAst, EmissionError> {
        let dialect = SparkDialect::default();
        let mut stmts =
            Parser::parse_sql(&dialect, sql).map_err(|e| EmissionError::Unsupported {
                kind: UnsupportedKind::Op,
                name: "sql::parse".to_owned(),
                reason: e.to_string(),
            })?;
        assert_eq!(stmts.len(), 1);
        lower_statement(stmts.remove(0))
    }

    #[test]
    fn parse_select_literal_no_from() {
        let plan = parse("SELECT 1").expect("should parse");
        match plan.op {
            CommonOp::Project { input, projections } => {
                assert!(matches!(input.op, CommonOp::SingleRow));
                assert_eq!(projections.len(), 1);
                assert!(matches!(projections[0], Expression::Literal(_)));
            }
            _ => panic!("expected Project over SingleRow"),
        }
    }

    // ── timestampadd / timestampdiff unit demotion (intv-006 regression) ───
    //
    // sqlparser parses the leading datetime UNIT (`MONTH`, `DAY`, …) as an
    // `Expr::Identifier`; the generic identifier arm would lower it to an
    // `UnresolvedColumn`, and the analyzer would then raise a spurious
    // `UnknownColumn { name: "MONTH" }`. These tests pin the fix: the unit is
    // demoted to a string literal, and the plan analyzes to the Spark-parity
    // return type (TIMESTAMP for add, BIGINT/Long for diff).

    fn timestampadd_call(sql: &str) -> FunctionCall {
        let plan = parse(sql).expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        match projections.into_iter().next() {
            Some(Expression::FunctionCall(call)) => call,
            other => panic!("expected FunctionCall projection, got {other:?}"),
        }
    }

    #[test]
    fn parse_timestampadd_demotes_unit_to_string_literal() {
        let call = timestampadd_call("SELECT timestampadd(MONTH, 3, last_login) FROM t");
        assert!(call.name.eq_ignore_ascii_case("timestampadd"));
        assert_eq!(call.args.len(), 3);
        assert!(
            matches!(
                &call.args[0],
                Expression::Literal(Literal {
                    value: LiteralValue::String(u),
                    ..
                }) if u == "MONTH"
            ),
            "unit must be a string literal, got {:?}",
            call.args[0]
        );
        assert!(
            !call
                .args
                .iter()
                .any(|a| matches!(a, Expression::UnresolvedColumn(c) if c.name == "MONTH")),
            "unit must NOT lower to an UnresolvedColumn(MONTH)"
        );
    }

    #[test]
    fn parse_timestampdiff_demotes_unit_to_string_literal() {
        let call = timestampadd_call("SELECT timestampdiff(DAY, hire_date, last_login) FROM t");
        assert!(call.name.eq_ignore_ascii_case("timestampdiff"));
        assert_eq!(call.args.len(), 3);
        assert!(
            matches!(
                &call.args[0],
                Expression::Literal(Literal {
                    value: LiteralValue::String(u),
                    ..
                }) if u == "DAY"
            ),
            "unit must be a string literal, got {:?}",
            call.args[0]
        );
        assert!(
            !call
                .args
                .iter()
                .any(|a| matches!(a, Expression::UnresolvedColumn(c) if c.name == "DAY")),
            "unit must NOT lower to an UnresolvedColumn(DAY)"
        );
    }

    #[test]
    fn timestampadd_and_timestampdiff_analyze_to_spark_return_types() {
        use crate::transpiler_v2::analyzer::analyze;
        use crate::transpiler_v2::base_types::BaseTypes;
        use crate::types::{StructField, StructType};

        fn emp() -> StructType {
            StructType::new(vec![
                StructField::nullable("last_login", DataType::Timestamp),
                StructField::nullable("hire_date", DataType::Timestamp),
            ])
        }

        // timestampadd → TIMESTAMP, with no UnknownColumn { name: "MONTH" }.
        let plan = parse("SELECT timestampadd(MONTH, 3, last_login) FROM emp").expect("parse");
        let bt = BaseTypes::build_from_plan(&plan, |n| (n == "emp").then(emp));
        let typed = analyze(plan, &bt).expect("analyze must succeed (no UnknownColumn)");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(
            typed.resolved_schema.fields[0].data_type,
            DataType::Timestamp
        );

        // timestampdiff → BIGINT (Long), with no UnknownColumn { name: "DAY" }.
        let plan =
            parse("SELECT timestampdiff(DAY, hire_date, last_login) FROM emp").expect("parse");
        let bt = BaseTypes::build_from_plan(&plan, |n| (n == "emp").then(emp));
        let typed = analyze(plan, &bt).expect("analyze must succeed (no UnknownColumn)");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
    }

    #[test]
    fn parse_select_star_from_table() {
        let plan = parse("SELECT * FROM t").expect("should parse");
        match plan.op {
            CommonOp::Project { input, projections } => {
                assert_eq!(projections.len(), 1);
                assert!(matches!(projections[0], Expression::Star(_)));
                assert!(matches!(
                    input.op,
                    CommonOp::TableScan { ref table, .. } if table == "t"
                ));
            }
            _ => panic!("expected Project over TableScan"),
        }
    }

    #[test]
    fn parse_select_with_where() {
        let plan = parse("SELECT id FROM t WHERE id > 5").expect("should parse");
        match plan.op {
            CommonOp::Project { input, .. } => match input.op {
                CommonOp::Filter { input, .. } => {
                    assert!(matches!(input.op, CommonOp::TableScan { .. }));
                }
                _ => panic!("expected Filter under Project"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn parse_select_with_order_by_limit() {
        let plan =
            parse("SELECT id FROM t ORDER BY id DESC LIMIT 10 OFFSET 5").expect("should parse");
        match plan.op {
            CommonOp::Sort {
                order,
                limit,
                offset,
                ..
            } => {
                assert_eq!(order.len(), 1);
                assert_eq!(order[0].direction, SortDirection::Descending);
                assert_eq!(limit, Some(10));
                assert_eq!(offset, Some(5));
            }
            _ => panic!("expected Sort as top-level"),
        }
    }

    #[test]
    fn parse_select_with_group_by_and_aggregate() {
        let plan = parse("SELECT dept, COUNT(*) FROM t GROUP BY dept").expect("should parse");
        // Top-level is Aggregate (has GROUP BY).
        assert!(matches!(plan.op, CommonOp::Aggregate { .. }));
    }

    #[test]
    fn parse_group_by_all_groups_by_non_aggregate_items() {
        let plan = parse("SELECT a, b, count(*) FROM t GROUP BY ALL").expect("should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                aggregates,
                grouping_kind,
                ..
            } => {
                // GROUP BY ALL groups by the two non-aggregate projection items.
                assert_eq!(grouping.len(), 2);
                assert!(matches!(grouping[0], Expression::UnresolvedColumn(_)));
                assert!(matches!(grouping[1], Expression::UnresolvedColumn(_)));
                // The aggregate output list is the full projection.
                assert_eq!(aggregates.len(), 3);
                assert_eq!(
                    grouping_kind,
                    crate::transpiler_v2::ast::GroupingKind::GroupBy
                );
            }
            _ => panic!("expected Aggregate for GROUP BY ALL"),
        }
    }

    #[test]
    fn parse_order_by_all_orders_by_every_output_column() {
        let plan = parse("SELECT a, b FROM t ORDER BY ALL").expect("should parse");
        match plan.op {
            CommonOp::Sort { order, input, .. } => {
                assert_eq!(order.len(), 2);
                assert_eq!(order[0].direction, SortDirection::Ascending);
                assert_eq!(order[1].direction, SortDirection::Ascending);
                assert!(matches!(input.op, CommonOp::Project { .. }));
            }
            _ => panic!("expected Sort for ORDER BY ALL"),
        }
    }

    #[test]
    fn parse_select_unresolved_column_has_plan_id_none() {
        // Open Decision 12 anchor.
        let plan = parse("SELECT id FROM t").expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        match &projections[0] {
            Expression::UnresolvedColumn(u) => assert_eq!(u.plan_id, None),
            _ => panic!("expected UnresolvedColumn"),
        }
    }

    #[test]
    fn parse_pivot_returns_unsupported_proto_shape() {
        // `SELECT * FROM t PIVOT (...)` — sqlparser recognizes PIVOT clauses.
        // If the input doesn't parse, that's still a boundary error we detect.
        let result = parse("SELECT * FROM t PIVOT (SUM(x) FOR y IN (1, 2))");
        assert!(matches!(
            result,
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                ..
            }) | Err(EmissionError::Unsupported {
                kind: UnsupportedKind::Op,
                ..
            })
        ));
    }

    #[test]
    fn parse_grouping_sets_returns_unsupported_proto_shape() {
        let result = parse("SELECT dept, COUNT(*) FROM t GROUP BY GROUPING SETS ((dept))");
        assert!(matches!(
            result,
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                ..
            })
        ));
    }

    #[test]
    fn parse_union_returns_unsupported_proto_shape() {
        let result = parse("SELECT 1 UNION SELECT 2");
        assert!(matches!(
            result,
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                ..
            })
        ));
    }

    #[test]
    fn parse_div_keyword_lowers_to_integer_divide_cast() {
        // Pass 73: SparkSQL's `a DIV b` — the SparkDialect's `parse_infix`
        // registers DIV as an integer-division operator; v2_lowering
        // wraps the resulting binary in a `CAST(... AS BIGINT)`.
        let plan = parse("SELECT a div 2 FROM t").expect("should parse");
        match plan.op {
            CommonOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 1);
                assert!(
                    matches!(&projections[0], Expression::Cast(c)
                        if matches!(&*c.expr, Expression::Binary(b) if b.op == BinaryOp::Div)
                    ),
                    "expected Cast(Binary(Div)) for `a DIV 2`, got {:?}",
                    projections[0]
                );
            }
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn parse_extract_year_lowers_to_year_function() {
        // Pass 73: `EXTRACT(YEAR FROM col)` lowers to a FunctionCall to
        // `year(col)` (INTEGER return-type, matching Spark).
        let plan = parse("SELECT EXTRACT(YEAR FROM d) FROM t").expect("should parse");
        match plan.op {
            CommonOp::Project { projections, .. } => match &projections[0] {
                Expression::FunctionCall(fc) => {
                    assert_eq!(fc.name.to_lowercase(), "year");
                    assert_eq!(fc.args.len(), 1);
                }
                other => panic!("expected FunctionCall, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn single_arg_lambda_lowers_to_lambda_expression() {
        // Pass 84: `x -> upper(x)` inside `transform(tags, ...)` must lower to
        // `Expression::Lambda { params: ["x"], body: FunctionCall(upper) }`.
        // Pass 86 L1 witness: the identifier `x` inside the body must be
        // rewritten to `LambdaVariable("x")` — not left as `UnresolvedColumn`.
        let plan = parse("SELECT transform(tags, x -> upper(x)) FROM emp").expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        let Expression::FunctionCall(fc) = &projections[0] else {
            panic!("expected FunctionCall, got {:?}", projections[0]);
        };
        assert_eq!(fc.name.to_lowercase(), "transform");
        assert_eq!(fc.args.len(), 2);
        let Expression::Lambda(lambda) = &fc.args[1] else {
            panic!("expected Lambda as second arg, got {:?}", fc.args[1]);
        };
        assert_eq!(lambda.params, vec!["x".to_owned()]);
        let Expression::FunctionCall(body_fc) = &*lambda.body else {
            panic!("expected FunctionCall body, got {:?}", lambda.body);
        };
        assert_eq!(body_fc.name.to_lowercase(), "upper");
        assert_eq!(body_fc.args.len(), 1);
        match &body_fc.args[0] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "x"),
            other => panic!("expected LambdaVariable(x), got {other:?}"),
        }
    }

    #[test]
    fn multi_arg_lambda_lowers_to_lambda_expression() {
        // Pass 84: `(acc, x) -> concat(acc, x)` inside `reduce(...)` must lower
        // to `Expression::Lambda { params: ["acc", "x"], body: FunctionCall }`.
        // Pass 86 L1 witness: both `acc` and `x` inside the body must be
        // rewritten to `LambdaVariable` — not left as `UnresolvedColumn`.
        let plan = parse("SELECT reduce(tags, '', (acc, x) -> concat(acc, x)) FROM emp")
            .expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        let Expression::FunctionCall(fc) = &projections[0] else {
            panic!("expected FunctionCall, got {:?}", projections[0]);
        };
        assert_eq!(fc.name.to_lowercase(), "reduce");
        assert_eq!(fc.args.len(), 3);
        let Expression::Lambda(lambda) = &fc.args[2] else {
            panic!("expected Lambda as third arg, got {:?}", fc.args[2]);
        };
        assert_eq!(lambda.params, vec!["acc".to_owned(), "x".to_owned()]);
        let Expression::FunctionCall(body_fc) = &*lambda.body else {
            panic!("expected FunctionCall body, got {:?}", lambda.body);
        };
        assert_eq!(body_fc.name.to_lowercase(), "concat");
        assert_eq!(body_fc.args.len(), 2);
        match &body_fc.args[0] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "acc"),
            other => panic!("expected LambdaVariable(acc), got {other:?}"),
        }
        match &body_fc.args[1] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "x"),
            other => panic!("expected LambdaVariable(x), got {other:?}"),
        }
    }

    #[test]
    fn nested_lambda_shadowing_preserved() {
        // Pass 86 L2: nested-lambda shadowing witness. In
        // `transform(arr1, x -> transform(arr2, y -> concat(x, y)))`, the
        // inner-lambda body references BOTH the outer's `x` and the inner's
        // `y`. After lowering, both must be rewritten to `LambdaVariable`:
        // outer's `x` reaches through the inner-Lambda arm because
        // `remaining = params \ inner.params = ["x"] \ ["y"] = ["x"]` (the
        // outer param survives the shadow-filter). Inner's `y` is rewritten
        // by the inner-lambda pass itself.
        let plan = parse("SELECT transform(arr1, x -> transform(arr2, y -> concat(x, y))) FROM t")
            .expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        // Outer FunctionCall("transform", [_, outer_lambda]).
        let Expression::FunctionCall(outer_fc) = &projections[0] else {
            panic!("expected outer FunctionCall, got {:?}", projections[0]);
        };
        assert_eq!(outer_fc.name.to_lowercase(), "transform");
        assert_eq!(outer_fc.args.len(), 2);
        let Expression::Lambda(outer_lambda) = &outer_fc.args[1] else {
            panic!("expected outer Lambda, got {:?}", outer_fc.args[1]);
        };
        assert_eq!(outer_lambda.params, vec!["x".to_owned()]);
        // Outer body is `transform(arr2, y -> concat(x, y))`.
        let Expression::FunctionCall(inner_transform) = &*outer_lambda.body else {
            panic!(
                "expected inner transform FunctionCall, got {:?}",
                outer_lambda.body
            );
        };
        assert_eq!(inner_transform.name.to_lowercase(), "transform");
        assert_eq!(inner_transform.args.len(), 2);
        let Expression::Lambda(inner_lambda) = &inner_transform.args[1] else {
            panic!("expected inner Lambda, got {:?}", inner_transform.args[1]);
        };
        assert_eq!(inner_lambda.params, vec!["y".to_owned()]);
        // Inner body is `concat(x, y)` — both must be LambdaVariable.
        let Expression::FunctionCall(concat_fc) = &*inner_lambda.body else {
            panic!(
                "expected concat FunctionCall in inner body, got {:?}",
                inner_lambda.body
            );
        };
        assert_eq!(concat_fc.name.to_lowercase(), "concat");
        assert_eq!(concat_fc.args.len(), 2);
        match &concat_fc.args[0] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "x"),
            other => panic!("expected outer LambdaVariable(x), got {other:?}"),
        }
        match &concat_fc.args[1] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "y"),
            other => panic!("expected inner LambdaVariable(y), got {other:?}"),
        }
    }

    #[test]
    fn parse_syntax_error_returns_unsupported_proto_shape() {
        // Review M2: sqlparser errors are boundary failures (input never
        // reached CommonAst) → surface as `UnsupportedProtoShape`, not
        // `UnsupportedOp`. Exercised through the public entry point so the
        // top-level mapping (parser_v2::SparkSqlParserV2::parse) is anchored.
        use crate::parser_v2::SparkSqlParserV2;
        let result = SparkSqlParserV2::parse("SELCT bad");
        match result {
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: shape,
                ..
            }) => {
                assert_eq!(shape, "sql::parse_error");
            }
            other => panic!("expected UnsupportedProtoShape sql::parse_error, got {other:?}"),
        }
    }

    // ── Window-function lowering (pass 98) ───────────────────────────────────

    /// Extract the [`WindowFunction`] behind the first projection item, peeling
    /// an optional alias.
    fn window_of(sql: &str) -> WindowFunction {
        let plan = parse(sql).expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project, got {:?}", plan.op);
        };
        let expr = match &projections[0] {
            Expression::Alias(a) => a.expr.as_ref(),
            other => other,
        };
        match expr {
            Expression::Window(w) => w.clone(),
            other => panic!("expected Window, got {other:?}"),
        }
    }

    #[test]
    fn window_partition_order_no_frame() {
        let w = window_of("SELECT row_number() OVER (PARTITION BY a ORDER BY b) FROM t");
        assert_eq!(w.partition_by.len(), 1);
        assert_eq!(w.order_by.len(), 1);
        assert!(w.frame.is_none());
        assert!(matches!(w.func.as_ref(), Expression::FunctionCall(f) if f.name == "row_number"));
    }

    #[test]
    fn window_rows_unbounded_preceding_to_current_row() {
        let w = window_of(
            "SELECT sum(x) OVER (PARTITION BY a ORDER BY b \
             ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t",
        );
        let frame = w.frame.expect("frame present");
        assert_eq!(frame.unit, FrameUnit::Rows);
        assert!(matches!(frame.lower, FrameBoundary::UnboundedPreceding));
        assert!(matches!(frame.upper, FrameBoundary::CurrentRow));
    }

    #[test]
    fn window_rows_between_one_preceding_and_one_following() {
        let w = window_of(
            "SELECT avg(x) OVER (PARTITION BY a ORDER BY b \
             ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM t",
        );
        let frame = w.frame.expect("frame present");
        assert_eq!(frame.unit, FrameUnit::Rows);
        // Magnitude taken as-is (sqlparser encodes direction in the variant).
        match frame.lower {
            FrameBoundary::Preceding(e) => {
                assert!(matches!(
                    *e,
                    Expression::Literal(Literal {
                        value: LiteralValue::Int(1),
                        ..
                    })
                ));
            }
            other => panic!("expected Preceding(1), got {other:?}"),
        }
        match frame.upper {
            FrameBoundary::Following(e) => {
                assert!(matches!(
                    *e,
                    Expression::Literal(Literal {
                        value: LiteralValue::Int(1),
                        ..
                    })
                ));
            }
            other => panic!("expected Following(1), got {other:?}"),
        }
    }

    #[test]
    fn window_named_window_is_inlined() {
        let w = window_of("SELECT rank() OVER w FROM t WINDOW w AS (PARTITION BY a ORDER BY b)");
        assert_eq!(w.partition_by.len(), 1);
        assert_eq!(w.order_by.len(), 1);
        assert!(matches!(w.func.as_ref(), Expression::FunctionCall(f) if f.name == "rank"));
    }

    #[test]
    fn window_groups_frame_is_rejected() {
        let result = parse(
            "SELECT sum(x) OVER (ORDER BY b \
             GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t",
        );
        match result {
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: shape,
                ..
            }) => {
                assert_eq!(shape, "sql::window_frame::groups");
            }
            other => panic!("expected GROUPS rejection, got {other:?}"),
        }
    }

    #[test]
    fn unknown_named_window_is_rejected() {
        let result = parse("SELECT rank() OVER missing FROM t WINDOW w AS (ORDER BY b)");
        match result {
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: shape,
                ..
            }) => {
                assert_eq!(shape, "sql::window::unknown_named_window");
            }
            other => panic!("expected unknown-window rejection, got {other:?}"),
        }
    }

    #[test]
    fn interval_literal_day_lowers_to_interval_expression() {
        // Exercised via a RANGE frame bound, which is where SparkSQL surfaces
        // interval literals for windows (win-016).
        let w = window_of(
            "SELECT sum(x) OVER (ORDER BY b \
             RANGE BETWEEN INTERVAL '90' DAY PRECEDING AND CURRENT ROW) FROM t",
        );
        let frame = w.frame.expect("frame present");
        assert_eq!(frame.unit, FrameUnit::Range);
        match frame.lower {
            FrameBoundary::Preceding(e) => match *e {
                Expression::Interval(iv) => {
                    assert_eq!(iv.months, 0);
                    assert_eq!(iv.days, 90);
                    assert_eq!(iv.microseconds, 0);
                }
                other => panic!("expected Interval, got {other:?}"),
            },
            other => panic!("expected Preceding(interval), got {other:?}"),
        }
    }
}
