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

use std::collections::HashMap;

use sqlparser::ast::{
    BinaryOperator, CastKind, DataType as SqlDataType, DateTimeField, DuplicateTreatment,
    ExactNumberInfo, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, GroupByExpr, Interval, JoinConstraint, JoinOperator, LimitClause,
    NamedWindowDefinition, NamedWindowExpr, ObjectName, ObjectNamePart, OrderByExpr, OrderByKind,
    Query, Select, SelectItem, SetExpr, SetOperator, SetQuantifier, Statement, TableFactor,
    TableWithJoins, TrimWhereField, UnaryOperator, Value, ValueWithSpan,
    WindowFrame as SqlWindowFrame, WindowFrameBound, WindowFrameUnits, WindowSpec, WindowType,
};

use crate::transpiler_v2::ast::{CommonAst, CommonOp, GroupingKind, JoinType, SetOpKind};
use crate::transpiler_v2::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression, Expression,
    FrameBoundary, FrameUnit, FunctionCall, InListExpression, IntervalExpression, LambdaExpression,
    LambdaVariableExpression, LikeExpression, Literal, LiteralValue, NullOrdering, SortDirection,
    SortOrder, StarExpression, UnaryExpression, UnaryOp, UnresolvedColumn, WindowFrame,
    WindowFunction,
};
use crate::transpiler_v2::type_inference::AGGREGATE_NAMES;
use crate::transpiler_v2::EmissionError;
use crate::types::DataType;

/// Immutable CTE scope: lowercased CTE name → its already-lowered body.
///
/// Threaded through the query-body lowering chain so that a `FROM <cte>`
/// reference inlines the CTE body (ADR-004 — no new `CommonOp`) instead of a
/// catalog `TableScan`. Bodies are lowered once, eagerly, in `cte_tables`
/// order (each seeing its predecessors), and cloned per reference.
type CteScope = HashMap<String, CommonAst>;

/// Lower a parsed sqlparser [`Statement`] into a [`CommonAst`].
pub fn lower_statement(stmt: Statement) -> Result<CommonAst, EmissionError> {
    match stmt {
        Statement::Query(q) => lower_query(*q, &CteScope::new()),
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

fn lower_query(query: Query, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    // Build the effective CTE scope: inherit the outer scope, then fold in
    // this query's own `WITH` clause. Each CTE body is lowered with the scope
    // built so far, so a nested CTE (`b AS (... FROM a ...)`) sees its
    // predecessors. `WITH RECURSIVE` is not inlinable (self-reference) and is a
    // Thunderduck-boundary reject (ADR-022).
    let mut local_scope: CteScope;
    let effective_scope: &CteScope = match query.with {
        Some(with) => {
            if with.recursive {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::recursive_cte".to_owned(),
                    reason: "WITH RECURSIVE not supported".to_owned(),
                });
            }
            local_scope = cte_scope.clone();
            for cte in with.cte_tables {
                let body = lower_query(*cte.query, &local_scope)?;
                // Explicit column list `t(k, v)` → positional rename via ToDf.
                let body = if cte.alias.columns.is_empty() {
                    body
                } else {
                    let column_names = cte
                        .alias
                        .columns
                        .into_iter()
                        .map(|c| c.name.value)
                        .collect();
                    CommonAst::new(CommonOp::ToDf {
                        input: Box::new(body),
                        column_names,
                    })
                };
                local_scope.insert(cte.alias.name.value.to_lowercase(), body);
            }
            &local_scope
        }
        None => cte_scope,
    };

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

    let body = lower_set_expr(*query.body, effective_scope)?;
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

fn lower_set_expr(body: SetExpr, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    match body {
        SetExpr::Select(sel) => lower_select(*sel, cte_scope),
        SetExpr::Query(q) => lower_query(*q, cte_scope),
        SetExpr::Values(_) => Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::values_top_level".to_owned(),
            reason: "top-level VALUES not supported at Slice A.2 (only VALUES in FROM)".to_owned(),
        }),
        SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            let kind = match op {
                SetOperator::Union => SetOpKind::Union,
                SetOperator::Intersect => SetOpKind::Intersect,
                SetOperator::Except | SetOperator::Minus => SetOpKind::Except,
            };
            // `UNION BY NAME` is parseable in `SparkDialect` but positional
            // lowering would silently align columns by position — a wrong
            // result. Reject it as a Thunderduck-boundary error rather than
            // mis-lower (ADR-022; loud-fail per CLAUDE.md gotcha #9).
            if matches!(
                set_quantifier,
                SetQuantifier::ByName | SetQuantifier::AllByName | SetQuantifier::DistinctByName
            ) {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::set_operation::by_name".to_owned(),
                    reason: "UNION/INTERSECT/EXCEPT BY NAME not supported (positional only)"
                        .to_owned(),
                });
            }
            // Spark defaults bare UNION/INTERSECT/EXCEPT to DISTINCT (`all = false`);
            // only the explicit `ALL` quantifier preserves duplicates.
            let all = matches!(set_quantifier, SetQuantifier::All);
            let left = lower_set_expr(*left, cte_scope)?;
            let right = lower_set_expr(*right, cte_scope)?;
            Ok(CommonAst::new(CommonOp::SetOp {
                kind,
                all,
                by_name: false,
                allow_missing_columns: false,
                children: vec![left, right],
            }))
        }
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::set_expr::{other:?}"),
            reason: "set expression not supported at Slice A.2".to_owned(),
        }),
    }
}

fn lower_select(mut select: Select, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    if select.distinct.is_some() {
        return Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::select_distinct".to_owned(),
            reason: "SELECT DISTINCT deferred past Slice A.2".to_owned(),
        });
    }
    // Inline named `WINDOW w AS (...)` references into their `WindowSpec` before
    // lowering — τ's Window substrate has no named-window concept (win-012).
    resolve_named_windows_in_select(&mut select)?;
    let base = lower_from(select.from, cte_scope)?;

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
    let (grouping, grouping_kind) = match group_by {
        GroupByExpr::Expressions(exprs, modifiers) => {
            if !modifiers.is_empty() {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::group_by_modifiers".to_owned(),
                    reason: "GROUP BY modifiers (ROLLUP/CUBE/GROUPING SETS) deferred to Slice G"
                        .to_owned(),
                });
            }
            // Prefix-form `ROLLUP (...)` / `CUBE (...)` parses to a single
            // `Expr::Rollup`/`Expr::Cube` holding `Vec<Vec<Expr>>` grouping
            // terms (`ROLLUP (a, b)` → `[[a], [b]]`). Flatten the terms into
            // τ's flat grouping list and thread the kind — mirroring the
            // DataFrame path in `v2_relation_converter::convert_aggregate`,
            // where the grouping list is flat and the direction lives in the
            // `GroupingKind`. Spark's ROLLUP/CUBE always wraps the whole
            // grouping list, so a single wrapper element is the expected shape.
            if exprs.len() == 1 && matches!(exprs[0], Expr::Rollup(_) | Expr::Cube(_)) {
                let (sets, kind) = match exprs.into_iter().next() {
                    Some(Expr::Rollup(sets)) => (sets, GroupingKind::Rollup),
                    Some(Expr::Cube(sets)) => (sets, GroupingKind::Cube),
                    // The `len() == 1 && matches!` guard above guarantees the
                    // first (only) element is `Rollup` or `Cube`.
                    _ => unreachable!("single ROLLUP/CUBE guaranteed by guard"),
                };
                // sqlparser preserves parenthesized grouping terms: `ROLLUP
                // ((a, b), c)` → `[[a, b], [c]]`, which Spark treats as a
                // distinct set of levels that a flat `ROLLUP(a, b, c)` does NOT
                // reproduce. τ's grouping list is flat (one column per level),
                // so a multi-column term can't be represented — reject rather
                // than silently flatten to the wrong grouping sets (ADR-022,
                // loud-fail). Simple `ROLLUP (a, b)` = `[[a],[b]]` is unaffected.
                if sets.iter().any(|term| term.len() != 1) {
                    return Err(EmissionError::UnsupportedProtoShape {
                        shape: "sql::grouping_sets".to_owned(),
                        reason: "nested ROLLUP/CUBE grouping terms deferred to Slice G".to_owned(),
                    });
                }
                let mut flat: Vec<Expression> = Vec::new();
                for term in sets {
                    for e in term {
                        flat.push(lower_expr(e)?);
                    }
                }
                (flat, kind)
            } else {
                // Plain GROUP BY, or an unsupported shape: bare GROUPING SETS,
                // or a ROLLUP/CUBE mixed with other terms / repeated (Spark
                // wraps the whole list in one wrapper — anything else is a
                // Slice-G boundary reject).
                let mut plain: Vec<Expression> = Vec::with_capacity(exprs.len());
                for e in exprs {
                    match e {
                        Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_) => {
                            return Err(EmissionError::UnsupportedProtoShape {
                                shape: "sql::grouping_sets".to_owned(),
                                reason: "GROUPING SETS / mixed ROLLUP/CUBE deferred to Slice G"
                                    .to_owned(),
                            });
                        }
                        other => plain.push(lower_expr(other)?),
                    }
                }
                (plain, GroupingKind::GroupBy)
            }
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
        grouping_kind,
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

fn lower_from(from: Vec<TableWithJoins>, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    if from.is_empty() {
        return Ok(CommonAst::new(CommonOp::SingleRow));
    }
    let mut plans: Vec<CommonAst> = from
        .into_iter()
        .map(|twj| lower_table_with_joins(twj, cte_scope))
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

fn lower_table_with_joins(
    twj: TableWithJoins,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    let mut plan = lower_table_factor(twj.relation, cte_scope)?;
    for join in twj.joins {
        let right = lower_table_factor(join.relation, cte_scope)?;
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

fn lower_table_factor(
    factor: TableFactor,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    match factor {
        TableFactor::Table { name, alias, .. } => {
            let table = object_name_to_string(&name);
            // A single-part name matching a CTE in scope inlines the CTE body
            // (Spark: a CTE shadows a catalog table of the same name). The
            // reference's own alias wins over the CTE name so qualified refs
            // bind — `FROM e emp` → alias "emp" (cte-003).
            if let Some(body) = cte_scope.get(&table.to_lowercase()) {
                let alias = alias.map(|a| a.name.value).unwrap_or(table);
                Ok(CommonAst::new(CommonOp::AliasedRelation {
                    input: Box::new(body.clone()),
                    alias,
                }))
            } else {
                Ok(CommonAst::new(CommonOp::TableScan {
                    table,
                    alias: alias.map(|a| a.name.value),
                }))
            }
        }
        TableFactor::Derived {
            subquery, alias: _, ..
        } => {
            // Slice A.2 lowers subquery-in-FROM by inlining the inner plan.
            // AliasedRelation is a deferred variant (Slice C.1); the alias
            // is discarded here — the analyzer (Slice B) will re-resolve.
            lower_query(*subquery, cte_scope)
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
        // Spark's `SUBSTRING(<expr> FROM <from> [FOR <for>])` special syntax and
        // the `SUBSTR(<expr>, <from>, <for>)` shorthand both parse to
        // `Expr::Substring`. Lower to `substring(expr, from[, for])` — the
        // existing `substring` type_inference / emission arms apply. Corpus
        // witnesses: `fn-003` (SQL syntax), `fn-004` (`substr(...)`).
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            let mut args = vec![lower_expr(*expr)?];
            if let Some(from) = substring_from {
                args.push(lower_expr(*from)?);
            }
            if let Some(for_) = substring_for {
                args.push(lower_expr(*for_)?);
            }
            Ok(Expression::FunctionCall(FunctionCall {
                name: "substring".to_owned(),
                args,
                distinct: false,
            }))
        }
        // Spark's `TRIM([BOTH | LEADING | TRAILING] [<what> FROM] <expr>)`
        // special syntax. Map the trim side to the DuckDB function name
        // (`trim` / `ltrim` / `rtrim`) and emit `trim(expr[, what])`. DuckDB's
        // `trim(string, characters)` takes the string first and the trim
        // characters second, matching Spark's `TRIM(BOTH what FROM expr)` =
        // "remove `what` from both ends of `expr`". Corpus witness: `fn-005`.
        Expr::Trim {
            expr,
            trim_where,
            trim_what,
            ..
        } => {
            let name = match trim_where {
                Some(TrimWhereField::Leading) => "ltrim",
                Some(TrimWhereField::Trailing) => "rtrim",
                _ => "trim",
            };
            let mut args = vec![lower_expr(*expr)?];
            if let Some(what) = trim_what {
                args.push(lower_expr(*what)?);
            }
            Ok(Expression::FunctionCall(FunctionCall {
                name: name.to_owned(),
                args,
                distinct: false,
            }))
        }
        // Spark's `POSITION(<substr> IN <str>)` special syntax. Lower to
        // `locate(substr, str)` (NOT `position` — DuckDB has no `position`
        // scalar; `locate` emits 1-based `strpos`). Corpus witness: `fn-006`.
        Expr::Position { expr, r#in } => Ok(Expression::FunctionCall(FunctionCall {
            name: "locate".to_owned(),
            args: vec![lower_expr(*expr)?, lower_expr(*r#in)?],
            distinct: false,
        })),
        // Spark's `OVERLAY(<expr> PLACING <what> FROM <from> [FOR <for>])`
        // special syntax. Lower to `overlay(expr, what, from[, for])` — the
        // existing `overlay` emission arm rewrites it via substring/concat.
        // Corpus witness: `fn-007`.
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            let mut args = vec![
                lower_expr(*expr)?,
                lower_expr(*overlay_what)?,
                lower_expr(*overlay_from)?,
            ];
            if let Some(for_) = overlay_for {
                args.push(lower_expr(*for_)?);
            }
            Ok(Expression::FunctionCall(FunctionCall {
                name: "overlay".to_owned(),
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
        Expr::Interval(iv) => lower_interval(iv),
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::expr::{}", expr_kind(&other)),
            reason: "expression shape not supported at Slice A.2".to_owned(),
        }),
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
            return Err(EmissionError::UnsupportedProtoShape {
                shape: format!("sql::binary_op::{other:?}"),
                reason: "binary operator not supported at Slice A.2".to_owned(),
            });
        }
    })
}

fn lower_function(f: Function) -> Result<Expression, EmissionError> {
    let name = object_name_to_string(&f.name);
    let over = f.over;
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
    let call = Expression::FunctionCall(FunctionCall {
        name,
        args,
        distinct,
    });
    match over {
        None => Ok(call),
        Some(WindowType::WindowSpec(spec)) => {
            let partition_by: Vec<Expression> = spec
                .partition_by
                .into_iter()
                .map(lower_expr)
                .collect::<Result<_, _>>()?;
            let order_by: Vec<SortOrder> = spec
                .order_by
                .into_iter()
                .map(lower_order_by_expr)
                .collect::<Result<_, _>>()?;
            let frame = lower_window_frame(spec.window_frame)?;
            Ok(Expression::Window(WindowFunction {
                func: Box::new(call),
                partition_by,
                order_by,
                frame,
            }))
        }
        // Safety net: named-window references are normally rewritten into a
        // `WindowSpec` by `resolve_named_windows_in_select` before lowering.
        // Reaching here means the reference was never defined (e.g. no WINDOW
        // clause at all) — a Thunderduck-boundary error (ADR-022).
        Some(WindowType::NamedWindow(ident)) => Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::named_window::unresolved".to_owned(),
            reason: format!("window `{}` is not defined in a WINDOW clause", ident.value),
        }),
    }
}

/// Map a sqlparser [`SqlWindowFrame`] into τ's [`WindowFrame`].
///
/// `None` → no frame clause (emission omits it; DuckDB's default matches
/// Spark's). `GROUPS` frame units are a Thunderduck-boundary error (ADR-022).
fn lower_window_frame(frame: Option<SqlWindowFrame>) -> Result<Option<WindowFrame>, EmissionError> {
    let Some(SqlWindowFrame {
        units,
        start_bound,
        end_bound,
    }) = frame
    else {
        return Ok(None);
    };
    let unit = match units {
        WindowFrameUnits::Rows => FrameUnit::Rows,
        WindowFrameUnits::Range => FrameUnit::Range,
        WindowFrameUnits::Groups => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::window_frame::groups".to_owned(),
                reason: "GROUPS window frame units are not supported".to_owned(),
            });
        }
    };
    let lower = lower_frame_bound(start_bound)?;
    // Shorthand `ROWS N PRECEDING` (no BETWEEN) → upper bound is CURRENT ROW.
    let upper = match end_bound {
        Some(b) => lower_frame_bound(b)?,
        None => FrameBoundary::CurrentRow,
    };
    Ok(Some(WindowFrame { unit, lower, upper }))
}

/// Map a single sqlparser [`WindowFrameBound`] into τ's [`FrameBoundary`].
///
/// sqlparser encodes the direction in the variant (`Preceding` / `Following`),
/// so the offset expression is the absolute magnitude — no sign re-application.
fn lower_frame_bound(bound: WindowFrameBound) -> Result<FrameBoundary, EmissionError> {
    Ok(match bound {
        WindowFrameBound::CurrentRow => FrameBoundary::CurrentRow,
        WindowFrameBound::Preceding(None) => FrameBoundary::UnboundedPreceding,
        WindowFrameBound::Following(None) => FrameBoundary::UnboundedFollowing,
        WindowFrameBound::Preceding(Some(e)) => FrameBoundary::Preceding(Box::new(lower_expr(*e)?)),
        WindowFrameBound::Following(Some(e)) => FrameBoundary::Following(Box::new(lower_expr(*e)?)),
    })
}

/// Build a `name → WindowSpec` map from the `WINDOW` clause and inline each
/// `NamedWindow` reference in the projection into its `WindowSpec`.
fn resolve_named_windows_in_select(select: &mut Select) -> Result<(), EmissionError> {
    if select.named_window.is_empty() {
        return Ok(());
    }
    let mut defs: HashMap<String, WindowSpec> = HashMap::with_capacity(select.named_window.len());
    for NamedWindowDefinition(ident, expr) in &select.named_window {
        match expr {
            NamedWindowExpr::WindowSpec(spec) => {
                defs.insert(ident.value.clone(), spec.clone());
            }
            // `WINDOW w AS other_window` (alias-of-window) — not represented in
            // τ's substrate; boundary error rather than silent drop (ADR-022).
            NamedWindowExpr::NamedWindow(_) => {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::named_window::alias_of_window".to_owned(),
                    reason: format!("named window `{}` aliases another window", ident.value),
                });
            }
        }
    }
    for item in &mut select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                resolve_named_windows_in_expr(e, &defs)?;
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {}
        }
    }
    Ok(())
}

/// Rewrite every `Expr::Function` whose `OVER` clause is a `NamedWindow`
/// reference into an inline `WindowSpec`, descending through the composite
/// expression shapes a projection can nest a window call inside.
fn resolve_named_windows_in_expr(
    expr: &mut Expr,
    defs: &HashMap<String, WindowSpec>,
) -> Result<(), EmissionError> {
    match expr {
        Expr::Function(f) => {
            if let Some(WindowType::NamedWindow(name)) = &f.over {
                let spec =
                    defs.get(&name.value)
                        .ok_or_else(|| EmissionError::UnsupportedProtoShape {
                            shape: "sql::named_window::unknown".to_owned(),
                            reason: format!(
                                "window `{}` is not defined in the WINDOW clause",
                                name.value
                            ),
                        })?;
                f.over = Some(WindowType::WindowSpec(spec.clone()));
            }
        }
        Expr::Nested(inner)
        | Expr::UnaryOp { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            resolve_named_windows_in_expr(inner, defs)?;
        }
        Expr::BinaryOp { left, right, .. } => {
            resolve_named_windows_in_expr(left, defs)?;
            resolve_named_windows_in_expr(right, defs)?;
        }
        _ => {}
    }
    Ok(())
}

/// Lower a sqlparser [`Interval`] literal into τ's [`IntervalExpression`].
///
/// Single-field intervals only (`INTERVAL '90' DAY`, `INTERVAL 3 YEAR`, …).
/// Compound (`X TO Y`), non-literal, or unrepresentable-field shapes are
/// Thunderduck-boundary errors (ADR-022), never a RawSql fallback.
fn lower_interval(iv: Interval) -> Result<Expression, EmissionError> {
    if iv.last_field.is_some() {
        return Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::expr::interval::compound".to_owned(),
            reason: "compound `INTERVAL X TO Y` literals are not supported".to_owned(),
        });
    }
    let n =
        extract_interval_int(&iv.value).ok_or_else(|| EmissionError::UnsupportedProtoShape {
            shape: "sql::expr::interval::non_literal".to_owned(),
            reason: "interval value must be an integer literal".to_owned(),
        })?;
    let field = iv
        .leading_field
        .as_ref()
        .ok_or_else(|| EmissionError::UnsupportedProtoShape {
            shape: "sql::expr::interval::no_field".to_owned(),
            reason: "interval literal has no leading time field".to_owned(),
        })?;

    const MICROS_PER_SECOND: i64 = 1_000_000;
    const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
    const MICROS_PER_HOUR: i64 = 60 * MICROS_PER_MINUTE;

    let overflow = |unit: &str| EmissionError::UnsupportedProtoShape {
        shape: format!("sql::expr::interval::{unit}_overflow"),
        reason: format!("interval {unit} value overflows"),
    };

    let ie = match field {
        DateTimeField::Year | DateTimeField::Years => IntervalExpression {
            months: n.checked_mul(12).ok_or_else(|| overflow("year"))?,
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
                .ok_or_else(|| overflow("hour"))?,
        },
        DateTimeField::Minute | DateTimeField::Minutes => IntervalExpression {
            months: 0,
            days: 0,
            microseconds: i64::from(n)
                .checked_mul(MICROS_PER_MINUTE)
                .ok_or_else(|| overflow("minute"))?,
        },
        DateTimeField::Second | DateTimeField::Seconds => IntervalExpression {
            months: 0,
            days: 0,
            microseconds: i64::from(n)
                .checked_mul(MICROS_PER_SECOND)
                .ok_or_else(|| overflow("second"))?,
        },
        other => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::expr::interval::unsupported_field".to_owned(),
                reason: format!("interval field `{other}` is not representable"),
            });
        }
    };
    Ok(Expression::Interval(ie))
}

/// Extract a plain `i32` from an interval value expression — handles both
/// `INTERVAL '3' DAY` (string literal) and `INTERVAL 3 DAY` (numeric literal).
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
    fn parse_group_by_rollup() {
        let plan =
            parse("SELECT a, b, COUNT(*) FROM t GROUP BY ROLLUP (a, b)").expect("should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::Rollup);
                // `ROLLUP (a, b)` flattens to two flat grouping columns.
                assert_eq!(grouping.len(), 2);
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_group_by_cube() {
        let plan =
            parse("SELECT a, b, COUNT(*) FROM t GROUP BY CUBE (a, b)").expect("should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::Cube);
                assert_eq!(grouping.len(), 2);
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_group_by_grouping_sets_rejected() {
        // GROUPING SETS still needs set-membership substrate (Slice G) — reject.
        let err = parse("SELECT a, b, COUNT(*) FROM t GROUP BY GROUPING SETS ((a), (b))")
            .expect_err("GROUPING SETS should be rejected");
        match err {
            EmissionError::UnsupportedProtoShape { shape, .. } => {
                assert_eq!(shape, "sql::grouping_sets");
            }
            other => panic!("expected UnsupportedProtoShape(sql::grouping_sets), got {other:?}"),
        }
    }

    #[test]
    fn parse_group_by_nested_rollup_term_rejected() {
        // `ROLLUP ((a, b), c)` has a multi-column grouping term Spark treats as
        // a distinct level; τ's flat grouping can't represent it — reject rather
        // than silently flatten to `ROLLUP(a, b, c)` (ADR-022 loud-fail).
        let err = parse("SELECT a, b, COUNT(*) FROM t GROUP BY ROLLUP ((a, b), c)")
            .expect_err("nested ROLLUP term should be rejected");
        assert!(
            matches!(err, EmissionError::UnsupportedProtoShape { ref shape, .. }
                if shape == "sql::grouping_sets"),
            "expected UnsupportedProtoShape(sql::grouping_sets), got {err:?}",
        );
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
    fn parse_cte_single_reference_inlines_as_aliased_relation() {
        // A `FROM <cte>` reference lowers to an AliasedRelation over the CTE
        // body — NOT a TableScan named x (the CTE shadows any catalog table).
        let plan = parse("WITH x AS (SELECT id FROM t) SELECT * FROM x").expect("should parse");
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project");
        };
        match input.op {
            CommonOp::AliasedRelation { alias, input } => {
                assert_eq!(alias, "x");
                // The inlined body is the CTE's own Project, not a scan of `x`.
                assert!(
                    matches!(input.op, CommonOp::Project { .. }),
                    "expected the inlined CTE body, got {:?}",
                    input.op
                );
            }
            other => panic!("expected AliasedRelation over the CTE body, got {other:?}"),
        }
    }

    #[test]
    fn parse_cte_explicit_columns_wraps_in_todf() {
        // `t(k, v)` — the explicit column list becomes a positional ToDf rename
        // beneath the AliasedRelation.
        let plan =
            parse("WITH t(k, v) AS (SELECT a, COUNT(*) FROM u GROUP BY a) SELECT k, v FROM t")
                .expect("should parse");
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project");
        };
        let CommonOp::AliasedRelation { alias, input } = input.op else {
            panic!("expected AliasedRelation over the CTE body");
        };
        assert_eq!(alias, "t");
        match input.op {
            CommonOp::ToDf { column_names, .. } => {
                assert_eq!(column_names, vec!["k".to_owned(), "v".to_owned()]);
            }
            other => panic!("expected ToDf under the AliasedRelation, got {other:?}"),
        }
    }

    #[test]
    fn parse_cte_referenced_twice_yields_two_aliased_relations() {
        // A CTE referenced twice with distinct aliases inlines an independent
        // AliasedRelation clone per reference (mirrors the self-join shape).
        let plan = parse(
            "WITH e AS (SELECT id, manager_id FROM emp) \
             SELECT emp.id FROM e emp LEFT JOIN e mgr ON emp.manager_id = mgr.id",
        )
        .expect("should parse");
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project");
        };
        let CommonOp::Join { left, right, .. } = input.op else {
            panic!("expected Join");
        };
        assert!(
            matches!(left.op, CommonOp::AliasedRelation { ref alias, .. } if alias == "emp"),
            "left side should be AliasedRelation aliased emp, got {:?}",
            left.op
        );
        assert!(
            matches!(right.op, CommonOp::AliasedRelation { ref alias, .. } if alias == "mgr"),
            "right side should be AliasedRelation aliased mgr, got {:?}",
            right.op
        );
    }

    #[test]
    fn parse_recursive_cte_rejected() {
        // WITH RECURSIVE is not inlinable (self-reference) — honest boundary.
        let err = parse("WITH RECURSIVE r(n) AS (SELECT 1) SELECT * FROM r")
            .expect_err("WITH RECURSIVE should be rejected");
        match err {
            EmissionError::UnsupportedProtoShape { shape, .. } => {
                assert_eq!(shape, "sql::recursive_cte");
            }
            other => panic!("expected UnsupportedProtoShape(sql::recursive_cte), got {other:?}"),
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
    fn parse_union_all_lowers_to_setop_union_all() {
        let plan = parse("SELECT id FROM t UNION ALL SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp {
                kind,
                all,
                by_name,
                allow_missing_columns,
                children,
            } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(all);
                assert!(!by_name);
                assert!(!allow_missing_columns);
                assert_eq!(children.len(), 2);
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_union_bare_is_distinct() {
        let plan = parse("SELECT id FROM t UNION SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, all, .. } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(!all, "bare UNION is Spark-default DISTINCT");
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_intersect_lowers_to_setop_intersect() {
        let plan = parse("SELECT id FROM t INTERSECT SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, all, .. } => {
                assert_eq!(kind, SetOpKind::Intersect);
                assert!(!all);
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_except_lowers_to_setop_except() {
        let plan = parse("SELECT id FROM t EXCEPT SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, .. } => assert_eq!(kind, SetOpKind::Except),
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_minus_folds_to_setop_except() {
        let plan = parse("SELECT id FROM t MINUS SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, .. } => assert_eq!(kind, SetOpKind::Except),
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_three_way_union_all_nests_setops() {
        let plan = parse("SELECT id FROM t UNION ALL SELECT id FROM u UNION ALL SELECT id FROM v")
            .expect("should parse");
        match plan.op {
            CommonOp::SetOp {
                kind,
                all,
                children,
                ..
            } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(all);
                assert_eq!(children.len(), 2);
                // sqlparser left-nests: children[0] is itself a SetOp.
                assert!(
                    matches!(children[0].op, CommonOp::SetOp { .. }),
                    "3-way UNION ALL should nest a SetOp as the left child"
                );
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_setop_with_order_by_wraps_in_sort() {
        let plan =
            parse("SELECT id FROM t UNION SELECT id FROM u ORDER BY id").expect("should parse");
        match plan.op {
            CommonOp::Sort { input, .. } => {
                assert!(
                    matches!(input.op, CommonOp::SetOp { .. }),
                    "ORDER BY over a set op wraps the SetOp in a Sort"
                );
            }
            _ => panic!("expected Sort wrapping a SetOp"),
        }
    }

    #[test]
    fn parse_union_by_name_is_rejected_not_silently_positional() {
        // `UNION BY NAME` parses in SparkDialect but has no positional
        // lowering — must be a Thunderduck-boundary error, not a silent
        // by-position union (ADR-022; loud-fail).
        let err = parse("SELECT a, b FROM t UNION BY NAME SELECT b, a FROM u")
            .expect_err("UNION BY NAME must be rejected");
        assert!(
            matches!(err, EmissionError::UnsupportedProtoShape { ref shape, .. }
                if shape == "sql::set_operation::by_name"),
            "expected UnsupportedProtoShape(sql::set_operation::by_name), got {err:?}",
        );
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
            Err(EmissionError::UnsupportedProtoShape { shape, .. }) => {
                assert_eq!(shape, "sql::parse_error");
            }
            other => panic!("expected UnsupportedProtoShape sql::parse_error, got {other:?}"),
        }
    }

    /// Return the first projection expression of a `Project` plan, unwrapping a
    /// top-level `Alias` if present.
    fn first_projection(plan: CommonAst) -> Expression {
        let CommonOp::Project {
            mut projections, ..
        } = plan.op
        else {
            panic!("expected Project as top-level");
        };
        assert!(!projections.is_empty());
        match projections.remove(0) {
            Expression::Alias(a) => *a.expr,
            other => other,
        }
    }

    #[test]
    fn window_partition_order_no_frame() {
        let plan = parse("SELECT rank() OVER (PARTITION BY dept ORDER BY sal) FROM t")
            .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                assert_eq!(w.partition_by.len(), 1);
                assert_eq!(w.order_by.len(), 1);
                assert!(w.frame.is_none(), "no frame clause → frame None");
            }
            other => panic!("expected Window, got {other:?}"),
        }
    }

    #[test]
    fn window_rows_unbounded_preceding_to_current_row() {
        let plan = parse(
            "SELECT sum(x) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) \
             FROM t",
        )
        .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                let frame = w.frame.expect("frame present");
                assert_eq!(frame.unit, FrameUnit::Rows);
                assert!(matches!(frame.lower, FrameBoundary::UnboundedPreceding));
                assert!(matches!(frame.upper, FrameBoundary::CurrentRow));
            }
            other => panic!("expected Window, got {other:?}"),
        }
    }

    #[test]
    fn window_rows_between_one_preceding_and_one_following() {
        let plan = parse(
            "SELECT avg(x) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM t",
        )
        .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                let frame = w.frame.expect("frame present");
                assert_eq!(frame.unit, FrameUnit::Rows);
                match frame.lower {
                    FrameBoundary::Preceding(e) => {
                        assert!(matches!(*e, Expression::Literal(_)));
                    }
                    other => panic!("expected Preceding(1), got {other:?}"),
                }
                match frame.upper {
                    FrameBoundary::Following(e) => {
                        assert!(matches!(*e, Expression::Literal(_)));
                    }
                    other => panic!("expected Following(1), got {other:?}"),
                }
            }
            other => panic!("expected Window, got {other:?}"),
        }
    }

    #[test]
    fn window_named_window_is_inlined() {
        let plan =
            parse("SELECT rank() OVER w FROM t WINDOW w AS (PARTITION BY dept ORDER BY sal)")
                .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                assert_eq!(w.partition_by.len(), 1, "named window PARTITION BY inlined");
                assert_eq!(w.order_by.len(), 1, "named window ORDER BY inlined");
            }
            other => panic!("expected inlined Window, got {other:?}"),
        }
    }

    #[test]
    fn window_groups_frame_is_rejected() {
        let err = parse(
            "SELECT sum(x) OVER (ORDER BY id GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t",
        )
        .expect_err("GROUPS frame must be rejected");
        assert!(
            matches!(err, EmissionError::UnsupportedProtoShape { ref shape, .. }
                if shape == "sql::window_frame::groups"),
            "expected UnsupportedProtoShape(sql::window_frame::groups), got {err:?}",
        );
    }

    #[test]
    fn unknown_named_window_is_rejected() {
        let err = parse("SELECT rank() OVER w FROM t WINDOW v AS (ORDER BY id)")
            .expect_err("unknown named window must be rejected");
        assert!(
            matches!(err, EmissionError::UnsupportedProtoShape { ref shape, .. }
                if shape == "sql::named_window::unknown"),
            "expected UnsupportedProtoShape(sql::named_window::unknown), got {err:?}",
        );
    }

    #[test]
    fn interval_literal_day_lowers_to_interval_expression() {
        let plan = parse("SELECT INTERVAL '90' DAY FROM t").expect("should parse");
        match first_projection(plan) {
            Expression::Interval(ie) => {
                assert_eq!(ie.days, 90);
                assert_eq!(ie.months, 0);
                assert_eq!(ie.microseconds, 0);
            }
            other => panic!("expected Interval, got {other:?}"),
        }
    }

    /// Extract the top-level projection as a `FunctionCall`, panicking otherwise.
    fn first_function_call(sql: &str) -> FunctionCall {
        let plan = parse(sql).expect("should parse");
        match first_projection(plan) {
            Expression::FunctionCall(fc) => fc,
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn substring_from_for_lowers_to_substring() {
        let fc = first_function_call("SELECT substring(name FROM 1 FOR 2) FROM t");
        assert_eq!(fc.name, "substring");
        assert_eq!(fc.args.len(), 3);
        assert!(!fc.distinct);
    }

    #[test]
    fn substr_shorthand_lowers_to_substring() {
        let fc = first_function_call("SELECT substr(name, 2, 3) FROM t");
        assert_eq!(fc.name, "substring");
        assert_eq!(fc.args.len(), 3);
    }

    #[test]
    fn trim_both_lowers_to_trim_with_expr_first() {
        let fc = first_function_call("SELECT trim(BOTH 'A' FROM name) FROM t");
        assert_eq!(fc.name, "trim");
        assert_eq!(fc.args.len(), 2);
        // DuckDB `trim(string, characters)`: the trimmed value comes first,
        // the trim characters second.
        assert!(matches!(
            fc.args[0],
            Expression::UnresolvedColumn(ref c) if c.name == "name"
        ));
    }

    #[test]
    fn trim_leading_lowers_to_ltrim() {
        let fc = first_function_call("SELECT trim(LEADING 'A' FROM name) FROM t");
        assert_eq!(fc.name, "ltrim");
        assert_eq!(fc.args.len(), 2);
    }

    #[test]
    fn trim_trailing_lowers_to_rtrim() {
        let fc = first_function_call("SELECT trim(TRAILING 'A' FROM name) FROM t");
        assert_eq!(fc.name, "rtrim");
        assert_eq!(fc.args.len(), 2);
    }

    #[test]
    fn bare_trim_lowers_to_single_arg_trim() {
        let fc = first_function_call("SELECT trim(name) FROM t");
        assert_eq!(fc.name, "trim");
        assert_eq!(fc.args.len(), 1);
    }

    #[test]
    fn position_in_lowers_to_locate() {
        let fc = first_function_call("SELECT position('a' IN name) FROM t");
        assert_eq!(fc.name, "locate");
        assert_eq!(fc.args.len(), 2);
        // locate(substr, str): needle first, haystack second.
        assert!(matches!(
            fc.args[0],
            Expression::Literal(Literal {
                value: LiteralValue::String(ref s),
                ..
            }) if s == "a"
        ));
    }

    #[test]
    fn overlay_placing_lowers_to_overlay() {
        let fc = first_function_call("SELECT overlay(name PLACING 'XX' FROM 1 FOR 2) FROM t");
        assert_eq!(fc.name, "overlay");
        assert_eq!(fc.args.len(), 4);
    }
}
