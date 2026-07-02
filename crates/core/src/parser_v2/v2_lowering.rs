//! sqlparser-rs AST → τ [`CommonAst`] lowering.
//!
//! Scope at Slice A.2 (per architecture plan §4):
//! - `SELECT expr, … FROM table WHERE … GROUP BY … ORDER BY … LIMIT n OFFSET m`
//! - bare `SELECT literal`
//! - `SELECT … FROM (VALUES ...)` and other subquery-in-FROM forms
//! - basic joins (INNER / LEFT / RIGHT / FULL / CROSS / LEFT SEMI / LEFT ANTI)
//! - `SELECT *`
//!
//! Deferred (surface as [`EmissionError::UnsupportedProtoShape`]):
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
    BinaryOperator, CastKind, DataType as SqlDataType, DuplicateTreatment, ExactNumberInfo, Expr,
    Function, FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments, GroupByExpr,
    JoinConstraint, JoinOperator, LimitClause, ObjectName, ObjectNamePart, OrderByExpr,
    OrderByKind, Query, Select, SelectItem, SetExpr, Statement, TableFactor, TableWithJoins,
    UnaryOperator, Value, ValueWithSpan,
};

use crate::transpiler_v2::ast::{CommonAst, CommonOp, JoinType};
use crate::transpiler_v2::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression, Expression,
    FunctionCall, InListExpression, LikeExpression, Literal, LiteralValue, NullOrdering,
    SortDirection, SortOrder, StarExpression, UnaryExpression, UnaryOp, UnresolvedColumn,
};
use crate::transpiler_v2::type_inference::AGGREGATE_NAMES;
use crate::transpiler_v2::EmissionError;
use crate::types::DataType;

/// Lower a parsed sqlparser [`Statement`] into a [`CommonAst`].
pub fn lower_statement(stmt: Statement) -> Result<CommonAst, EmissionError> {
    match stmt {
        Statement::Query(q) => lower_query(*q),
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::{}", statement_kind(&other)),
            reason: "parser_v2 only supports SELECT queries at Slice A.2".to_owned(),
        }),
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
        return Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::cte".to_owned(),
            reason: "CTEs (WITH clauses) not supported at Slice A.2".to_owned(),
        });
    }

    let order_by_exprs: Vec<OrderByExpr> = match &query.order_by {
        Some(ob) => match &ob.kind {
            OrderByKind::Expressions(exprs) => exprs.clone(),
            OrderByKind::All(_) => {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::order_by_all".to_owned(),
                    reason: "ORDER BY ALL not supported at Slice A.2".to_owned(),
                });
            }
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

fn lower_set_expr(body: SetExpr) -> Result<CommonAst, EmissionError> {
    match body {
        SetExpr::Select(sel) => lower_select(*sel),
        SetExpr::Query(q) => lower_query(*q),
        SetExpr::Values(_) => Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::values_top_level".to_owned(),
            reason: "top-level VALUES not supported at Slice A.2 (only VALUES in FROM)".to_owned(),
        }),
        SetExpr::SetOperation { op, .. } => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::set_operation::{op:?}").to_ascii_lowercase(),
            reason: "UNION / INTERSECT / EXCEPT deferred past Slice A.2".to_owned(),
        }),
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::set_expr::{other:?}"),
            reason: "set expression not supported at Slice A.2".to_owned(),
        }),
    }
}

fn lower_select(select: Select) -> Result<CommonAst, EmissionError> {
    if select.distinct.is_some() {
        return Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::select_distinct".to_owned(),
            reason: "SELECT DISTINCT deferred past Slice A.2".to_owned(),
        });
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
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::group_by_modifiers".to_owned(),
                    reason: "GROUP BY modifiers (ROLLUP/CUBE/GROUPING SETS) deferred to Slice G"
                        .to_owned(),
                });
            }
            let mut plain: Vec<Expression> = Vec::with_capacity(exprs.len());
            for e in exprs {
                match e {
                    Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_) => {
                        return Err(EmissionError::UnsupportedProtoShape {
                            shape: "sql::grouping_sets".to_owned(),
                            reason: "ROLLUP / CUBE / GROUPING SETS deferred to Slice G".to_owned(),
                        });
                    }
                    other => plain.push(lower_expr(other)?),
                }
            }
            plain
        }
        GroupByExpr::All(_) => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::group_by_all".to_owned(),
                reason: "GROUP BY ALL deferred past Slice A.2".to_owned(),
            });
        }
    };

    let projections: Result<Vec<Expression>, EmissionError> =
        projection.into_iter().map(lower_select_item).collect();
    let projections = projections?;
    // A.2 treats the aggregate projection list as the aggregate output list.
    // Slice C.1 refines this into the {grouping, aggregates} split when the
    // canonical emission table lands; for now we push everything into
    // `aggregates` so the round-trip test can inspect the projection list.
    let aggregated = CommonAst::new(CommonOp::Aggregate {
        input: Box::new(input),
        grouping,
        aggregates: projections,
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
            // Slice A.2 lowers subquery-in-FROM by inlining the inner plan.
            // AliasedRelation is a deferred variant (Slice C.1); the alias
            // is discarded here — the analyzer (Slice B) will re-resolve.
            lower_query(*subquery)
        }
        TableFactor::TableFunction { expr, alias: _ } => {
            // Only bare identifier / function-call table functions covered.
            match expr {
                Expr::Function(f) => lower_table_function(f),
                other => Err(EmissionError::UnsupportedProtoShape {
                    shape: format!("sql::table_function::{other:?}"),
                    reason: "table function expr shape not supported at Slice A.2".to_owned(),
                }),
            }
        }
        TableFactor::UNNEST {
            array_exprs,
            with_ordinality,
            ..
        } => {
            if array_exprs.len() != 1 {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::unnest_multi_arg".to_owned(),
                    reason: "UNNEST with multiple array arguments not supported at Slice A.2"
                        .to_owned(),
                });
            }
            let expr = array_exprs.into_iter().next().ok_or_else(|| {
                EmissionError::UnsupportedProtoShape {
                    shape: "sql::unnest_empty".to_owned(),
                    reason: "UNNEST has no array argument".to_owned(),
                }
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
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::table_factor::{other:?}"),
            reason: "table factor not supported at Slice A.2".to_owned(),
        }),
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
        FunctionArguments::Subquery(_) => Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::function_args_subquery".to_owned(),
            reason: "subquery function arguments deferred past Slice A.2".to_owned(),
        }),
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
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::function_arg::{other:?}"),
            reason: "function argument shape not supported at Slice A.2".to_owned(),
        }),
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
            return Err(EmissionError::UnsupportedProtoShape {
                shape: format!("sql::join_operator::{other:?}"),
                reason: "join operator not supported at Slice A.2".to_owned(),
            });
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
    // Slice A.2 fix pass (review M4): extend the walker to every composite
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
    // Slice A.2 fix pass (review M3 + perf OPT-5): defer to τ's canonical
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
        Expr::BinaryOp { left, op, right } => Ok(Expression::Binary(BinaryExpression {
            op: lower_binary_op(op)?,
            left: Box::new(lower_expr(*left)?),
            right: Box::new(lower_expr(*right)?),
        })),
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
            other => Err(EmissionError::UnsupportedProtoShape {
                shape: format!("sql::unary_op::{other:?}"),
                reason: "unary operator not supported at Slice A.2".to_owned(),
            }),
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
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::expr::{}", expr_kind(&other)),
            reason: "expression shape not supported at Slice A.2".to_owned(),
        }),
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
            return Err(EmissionError::UnsupportedProtoShape {
                shape: format!("sql::binary_op::{other:?}"),
                reason: "binary operator not supported at Slice A.2".to_owned(),
            });
        }
    })
}

fn lower_function(f: Function) -> Result<Expression, EmissionError> {
    if f.over.is_some() {
        return Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::window_function".to_owned(),
            reason: "window functions deferred past Slice A.2".to_owned(),
        });
    }
    let name = object_name_to_string(&f.name);
    let (distinct, args) = match f.args {
        FunctionArguments::None => (false, vec![]),
        FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment,
            args,
            ..
        }) => {
            let distinct = matches!(duplicate_treatment, Some(DuplicateTreatment::Distinct));
            let converted: Result<Vec<Expression>, EmissionError> =
                args.into_iter().map(function_arg_to_expr).collect();
            (distinct, converted?)
        }
        FunctionArguments::Subquery(_) => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::function_args_subquery".to_owned(),
                reason: "subquery function arguments deferred past Slice A.2".to_owned(),
            });
        }
    };
    Ok(Expression::FunctionCall(FunctionCall {
        name,
        args,
        distinct,
    }))
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
                Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::number_parse".to_owned(),
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
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::value::{other:?}"),
            reason: "literal value shape not supported at Slice A.2".to_owned(),
        }),
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
            return Err(EmissionError::UnsupportedProtoShape {
                shape: format!("sql::data_type::{other:?}"),
                reason: "data type not supported at Slice A.2".to_owned(),
            });
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
        }) => s
            .parse::<i64>()
            .map_err(|_| EmissionError::UnsupportedProtoShape {
                shape: "sql::limit_offset_parse".to_owned(),
                reason: format!("cannot parse LIMIT/OFFSET value `{s}` as i64"),
            }),
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::limit_offset_expr::{other:?}"),
            reason: "LIMIT/OFFSET must be an integer literal at Slice A.2".to_owned(),
        }),
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
            Parser::parse_sql(&dialect, sql).map_err(|e| EmissionError::UnsupportedOp {
                op: "sql::parse".to_owned(),
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
            Err(EmissionError::UnsupportedProtoShape { .. })
                | Err(EmissionError::UnsupportedOp { .. })
        ));
    }

    #[test]
    fn parse_grouping_sets_returns_unsupported_proto_shape() {
        let result = parse("SELECT dept, COUNT(*) FROM t GROUP BY GROUPING SETS ((dept))");
        assert!(matches!(
            result,
            Err(EmissionError::UnsupportedProtoShape { .. })
        ));
    }

    #[test]
    fn parse_union_returns_unsupported_proto_shape() {
        let result = parse("SELECT 1 UNION SELECT 2");
        assert!(matches!(
            result,
            Err(EmissionError::UnsupportedProtoShape { .. })
        ));
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
            Err(EmissionError::UnsupportedProtoShape { shape, .. }) => {
                assert_eq!(shape, "sql::parse_error");
            }
            other => panic!("expected UnsupportedProtoShape sql::parse_error, got {other:?}"),
        }
    }
}
