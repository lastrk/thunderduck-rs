//! sqlparser-rs AST → τ [`CommonAst`] lowering.
//!
//! Scope (per architecture plan §4):
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
    BinaryOperator, CastKind, DataType as SqlDataType, DateTimeField, Distinct, DuplicateTreatment,
    ExactNumberInfo, Expr, ExprWithAlias, Function, FunctionArg, FunctionArgExpr,
    FunctionArgumentList, FunctionArguments, GroupByExpr, Interval, JoinConstraint, JoinOperator,
    LimitClause, NamedWindowDefinition, NamedWindowExpr, NullInclusion, ObjectName, ObjectNamePart,
    OrderByExpr, OrderByKind, OrderByOptions, PivotValueSource, Query, Select, SelectItem, SetExpr,
    SetOperator, SetQuantifier, Statement, TableFactor, TableWithJoins, TrimWhereField,
    TypedString, UnaryOperator, Value, ValueWithSpan, WindowFrame as SqlWindowFrame,
    WindowFrameBound, WindowFrameUnits, WindowSpec, WindowType,
};

use crate::transpiler_v2::ast::{
    CommonAst, CommonOp, GroupingKind, JoinType, PivotGrouping, SetOpKind, UnpivotIds,
};
use crate::transpiler_v2::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression,
    ExistsSubquery, Expression, FrameBoundary, FrameUnit, FunctionCall, InListExpression,
    InSubquery, IntervalExpression, IsDistinctFromExpression, LambdaExpression,
    LambdaVariableExpression, LikeExpression, Literal, LiteralValue, NullOrdering, ScalarSubquery,
    SortDirection, SortOrder, StarExpression, SubqueryPlan, UnaryExpression, UnaryOp,
    UnresolvedColumn, WindowFrame, WindowFunction,
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
            reason: "parser_v2 only supports SELECT queries in τ".to_owned(),
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
            // Spark `ORDER BY ALL` orders by every output column, left to right,
            // applying the clause's asc/desc + nulls options uniformly. Build a
            // sort key per projection item (query.body is still borrowable here;
            // it is moved at `lower_set_expr(*query.body)` below).
            OrderByKind::All(options) => order_by_all_exprs(&query.body, options)?,
        },
        None => vec![],
    };

    let (limit_expr_opt, offset_expr_opt) = extract_limit_offset(query.limit_clause.as_ref())?;

    let body = lower_set_expr(*query.body, effective_scope)?;
    wrap_with_sort_limit(
        body,
        order_by_exprs,
        limit_expr_opt,
        offset_expr_opt,
        effective_scope,
    )
}

/// Synthesize `ORDER BY ALL` into one sort key per SELECT output column, each
/// carrying the clause's asc/desc + nulls options. Only supported over a plain
/// `SELECT` body (not set ops / VALUES); `*` projections are rejected.
fn order_by_all_exprs(
    body: &SetExpr,
    options: &OrderByOptions,
) -> Result<Vec<OrderByExpr>, EmissionError> {
    let select = match body {
        SetExpr::Select(s) => s,
        _ => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::order_by_all".to_owned(),
                reason: "ORDER BY ALL is only supported over a SELECT body".to_owned(),
            });
        }
    };
    let mut out: Vec<OrderByExpr> = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        let expr = match item {
            SelectItem::UnnamedExpr(e) => e.clone(),
            SelectItem::ExprWithAlias { expr, .. } => expr.clone(),
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::order_by_all_wildcard".to_owned(),
                    reason: "ORDER BY ALL over `*` projection not supported".to_owned(),
                });
            }
        };
        out.push(OrderByExpr {
            expr,
            options: options.clone(),
            with_fill: None,
        });
    }
    Ok(out)
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
            reason: "top-level VALUES not supported in τ (only VALUES in FROM)".to_owned(),
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
            reason: "set expression not supported in τ".to_owned(),
        }),
    }
}

fn lower_select(mut select: Select, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    // Capture DISTINCT before building the projection plan; the plain
    // `SELECT DISTINCT` lowers to a `Deduplicate` wrapping the final Project
    // (empty `on_columns` = dedupe the whole output row). `SELECT ALL` is the
    // default (keep duplicates) → no wrap. `DISTINCT ON (...)` is a Postgres
    // extension Spark SQL does not accept → Thunderduck-boundary reject.
    let dedupe = match select.distinct.take() {
        None | Some(Distinct::All) => false,
        Some(Distinct::Distinct) => true,
        Some(Distinct::On(_)) => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::distinct_on".to_owned(),
                reason: "SELECT DISTINCT ON is not valid Spark SQL".to_owned(),
            });
        }
    };
    // Inline named `WINDOW w AS (...)` references into their `WindowSpec` before
    // lowering — τ's Window substrate has no named-window concept (win-012).
    resolve_named_windows_in_select(&mut select)?;
    let base = lower_from(select.from, cte_scope)?;

    let filtered = if let Some(cond) = select.selection {
        CommonAst::new(CommonOp::Filter {
            input: Box::new(base),
            condition: lower_expr(cond, cte_scope)?,
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
        lower_aggregate_select(
            filtered,
            select.projection,
            select.group_by,
            select.having,
            cte_scope,
        )?
    } else {
        let projections: Result<Vec<Expression>, EmissionError> = select
            .projection
            .into_iter()
            .map(|item| lower_select_item(item, cte_scope))
            .collect();
        CommonAst::new(CommonOp::Project {
            input: Box::new(filtered),
            projections: projections?,
        })
    };

    // Plain `SELECT DISTINCT` dedupes the final projection. Wrapping here (below
    // `lower_query`'s `wrap_with_sort_limit`) yields `Sort(Deduplicate(Project))`
    // for `SELECT DISTINCT ... ORDER BY ...` — dedupe first, then order.
    let plan = if dedupe {
        CommonAst::new(CommonOp::Deduplicate {
            input: Box::new(plan),
            on_columns: vec![],
        })
    } else {
        plan
    };

    Ok(plan)
}

fn lower_aggregate_select(
    input: CommonAst,
    projection: Vec<SelectItem>,
    group_by: GroupByExpr,
    having: Option<Expr>,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    let (grouping, grouping_kind) = match group_by {
        GroupByExpr::Expressions(exprs, modifiers) => {
            if !modifiers.is_empty() {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::group_by_modifiers".to_owned(),
                    reason: "GROUP BY modifiers (ROLLUP/CUBE/GROUPING SETS) not supported in τ"
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
                        reason: "nested ROLLUP/CUBE grouping terms not supported in τ".to_owned(),
                    });
                }
                let mut flat: Vec<Expression> = Vec::new();
                for term in sets {
                    for e in term {
                        flat.push(lower_expr(e, cte_scope)?);
                    }
                }
                (flat, kind)
            } else {
                // Plain GROUP BY, or an unsupported shape: bare GROUPING SETS,
                // or a ROLLUP/CUBE mixed with other terms / repeated (Spark
                // wraps the whole list in one wrapper — anything else is a
                // Thunderduck-boundary reject).
                let mut plain: Vec<Expression> = Vec::with_capacity(exprs.len());
                for e in exprs {
                    match e {
                        Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_) => {
                            return Err(EmissionError::UnsupportedProtoShape {
                                shape: "sql::grouping_sets".to_owned(),
                                reason: "GROUPING SETS / mixed ROLLUP/CUBE not supported in τ"
                                    .to_owned(),
                            });
                        }
                        other => plain.push(lower_expr(other, cte_scope)?),
                    }
                }
                (plain, GroupingKind::GroupBy)
            }
        }
        GroupByExpr::All(modifiers) => {
            if !modifiers.is_empty() {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::group_by_all_modifiers".to_owned(),
                    reason: "GROUP BY ALL with ROLLUP/CUBE/GROUPING SETS modifiers not supported"
                        .to_owned(),
                });
            }
            // Spark `GROUP BY ALL` groups by every SELECT item that is NOT an
            // aggregate expression (the aggregates come from the projection fold
            // as usual). Compute the grouping from the projection here.
            let mut grouping: Vec<Expression> = Vec::new();
            for item in &projection {
                let expr = match item {
                    SelectItem::UnnamedExpr(e) => e,
                    SelectItem::ExprWithAlias { expr, .. } => expr,
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {
                        return Err(EmissionError::UnsupportedProtoShape {
                            shape: "sql::group_by_all_wildcard".to_owned(),
                            reason: "GROUP BY ALL over `*` projection not supported".to_owned(),
                        });
                    }
                };
                if !expr_has_aggregate(expr) {
                    grouping.push(lower_expr(expr.clone(), cte_scope)?);
                }
            }
            (grouping, GroupingKind::GroupBy)
        }
    };

    let projections: Result<Vec<Expression>, EmissionError> = projection
        .into_iter()
        .map(|item| lower_select_item(item, cte_scope))
        .collect();
    let projections = projections?;
    // τ treats the aggregate projection list as the aggregate output list.
    // This is refined into the {grouping, aggregates} split when the
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
            condition: lower_expr(h, cte_scope)?,
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
        let (join_type, condition, using_columns) =
            lower_join_operator(join.join_operator, cte_scope)?;
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
            // Subquery-in-FROM is lowered by inlining the inner plan. The
            // derived-table alias is still dropped here, so a
            // query that qualifies columns by the derived alias won't bind.
            // `AliasedRelation` IS live now (pass 101 wraps CTE references in it
            // with their alias) — preserving the derived alias the same way is a
            // follow-up (would green e.g. tbl-010).
            lower_query(*subquery, cte_scope)
        }
        TableFactor::TableFunction { expr, alias: _ } => {
            // Only bare identifier / function-call table functions covered.
            match expr {
                Expr::Function(f) => lower_table_function(f, cte_scope),
                other => Err(EmissionError::UnsupportedProtoShape {
                    shape: format!("sql::table_function::{other:?}"),
                    reason: "table function expr shape not supported in τ".to_owned(),
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
                    reason: "UNNEST with multiple array arguments not supported in τ".to_owned(),
                });
            }
            let expr = array_exprs.into_iter().next().ok_or_else(|| {
                EmissionError::UnsupportedProtoShape {
                    shape: "sql::unnest_empty".to_owned(),
                    reason: "UNNEST has no array argument".to_owned(),
                }
            })?;
            Ok(CommonAst::new(CommonOp::Unnest {
                expr: lower_expr(expr, cte_scope)?,
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
                .map(|a| function_arg_to_expr(a, cte_scope))
                .collect::<Result<_, _>>()?;
            Ok(CommonAst::new(CommonOp::TableFunction {
                name: func_name,
                args: arg_exprs,
                with_ordinality: false,
            }))
        }
        // SQL `PIVOT` (BigQuery/Snowflake/Databricks). Unlike the DataFrame
        // path, SQL supplies no grouping list — the analyzer derives it from
        // the resolved input schema (`grouping: PivotGrouping::Implicit`).
        TableFactor::Pivot {
            table,
            aggregate_functions,
            value_column,
            value_source,
            default_on_null,
            alias: _,
        } => {
            // Spark has no PIVOT `DEFAULT ON NULL` clause — boundary reject.
            if default_on_null.is_some() {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::pivot::default_on_null".to_owned(),
                    reason: "PIVOT DEFAULT ON NULL has no Spark equivalent".to_owned(),
                });
            }
            let input = Box::new(lower_table_factor(*table, cte_scope)?);
            if value_column.len() != 1 {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::pivot::multi_value_column".to_owned(),
                    reason: "PIVOT supports exactly one FOR column".to_owned(),
                });
            }
            let pivot_column = lower_expr(
                value_column
                    .into_iter()
                    .next()
                    .expect("value_column length checked == 1"),
                cte_scope,
            )?;
            let pivot_values = match value_source {
                PivotValueSource::List(vals) => {
                    let mut out: Vec<Expression> = Vec::with_capacity(vals.len());
                    for ewa in vals {
                        out.push(lower_expr_with_alias(ewa, cte_scope)?);
                    }
                    out
                }
                // ANY / subquery = dynamic pivot values; requires an eager
                // DISTINCT query — Thunderduck-boundary (ADR-022),
                // mirrors the analyzer's `Pivot[implicit-values]` punt.
                PivotValueSource::Any(_) | PivotValueSource::Subquery(_) => {
                    return Err(EmissionError::UnsupportedProtoShape {
                        shape: "sql::pivot::dynamic_values".to_owned(),
                        reason: "dynamic PIVOT values (ANY / subquery) require an eager DISTINCT query, not supported in τ".to_owned(),
                    });
                }
            };
            let mut aggregates: Vec<Expression> = Vec::with_capacity(aggregate_functions.len());
            for ewa in aggregate_functions {
                aggregates.push(lower_expr_with_alias(ewa, cte_scope)?);
            }
            Ok(CommonAst::new(CommonOp::Pivot {
                input,
                grouping: PivotGrouping::Implicit,
                pivot_column,
                pivot_values,
                aggregates,
            }))
        }
        // SQL `UNPIVOT`. SQL lists only value columns; the id columns are
        // implicit (`input − values`), derived by the analyzer.
        TableFactor::Unpivot {
            table,
            value,
            name,
            columns,
            null_inclusion,
            alias: _,
        } => {
            // τ's Unpivot variant has no include-nulls field; EXCLUDE NULLS is
            // the default. INCLUDE NULLS is unrepresentable — boundary reject.
            if matches!(null_inclusion, Some(NullInclusion::IncludeNulls)) {
                return Err(EmissionError::UnsupportedProtoShape {
                    shape: "sql::unpivot::include_nulls".to_owned(),
                    reason: "UNPIVOT INCLUDE NULLS is not representable in τ (EXCLUDE NULLS is the default)".to_owned(),
                });
            }
            let input = Box::new(lower_table_factor(*table, cte_scope)?);
            let value_column_name = expr_to_ident_string(&value).ok_or_else(|| {
                EmissionError::UnsupportedProtoShape {
                    shape: "sql::unpivot::value_non_ident".to_owned(),
                    reason: "UNPIVOT value must be a bare column name".to_owned(),
                }
            })?;
            let variable_column_name = name.value;
            let mut values: Vec<String> = Vec::with_capacity(columns.len());
            for ewa in columns {
                if ewa.alias.is_some() {
                    return Err(EmissionError::UnsupportedProtoShape {
                        shape: "sql::unpivot::column_alias".to_owned(),
                        reason: "UNPIVOT columns cannot be aliased in τ".to_owned(),
                    });
                }
                let col = expr_to_ident_string(&ewa.expr).ok_or_else(|| {
                    EmissionError::UnsupportedProtoShape {
                        shape: "sql::unpivot::column_non_ident".to_owned(),
                        reason: "UNPIVOT columns must be bare column names".to_owned(),
                    }
                })?;
                values.push(col);
            }
            Ok(CommonAst::new(CommonOp::Unpivot {
                input,
                ids: UnpivotIds::Implicit,
                values,
                variable_column_name,
                value_column_name,
            }))
        }
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::table_factor::{other:?}"),
            reason: "table factor not supported in τ".to_owned(),
        }),
    }
}

/// Lower a sqlparser [`ExprWithAlias`], wrapping the lowered expression in an
/// [`Expression::Alias`] only when an alias is present (mirrors
/// [`lower_select_item`]). Used for PIVOT aggregate functions and pivot
/// values, where `true AS act` must carry the alias but bare `10` must not.
fn lower_expr_with_alias(
    ewa: ExprWithAlias,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    let inner = lower_expr(ewa.expr, cte_scope)?;
    Ok(match ewa.alias {
        Some(a) => Expression::Alias(AliasExpression {
            expr: Box::new(inner),
            alias: a.value,
        }),
        None => inner,
    })
}

/// Extract a bare column name from a sqlparser [`Expr`] that must be a single
/// identifier (`UNPIVOT` value / column names are stored as plain strings in
/// τ). Returns `None` for any richer expression shape.
fn expr_to_ident_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.clone()),
        _ => None,
    }
}

fn lower_table_function(f: Function, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    let name = object_name_to_string(&f.name);
    let args = lower_function_args(f.args, cte_scope)?;
    Ok(CommonAst::new(CommonOp::TableFunction {
        name,
        args,
        with_ordinality: false,
    }))
}

fn lower_function_args(
    args: FunctionArguments,
    cte_scope: &CteScope,
) -> Result<Vec<Expression>, EmissionError> {
    match args {
        FunctionArguments::None => Ok(vec![]),
        FunctionArguments::Subquery(_) => Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::function_args_subquery".to_owned(),
            reason: "subquery function arguments not supported in τ".to_owned(),
        }),
        FunctionArguments::List(list) => list
            .args
            .into_iter()
            .map(|a| function_arg_to_expr(a, cte_scope))
            .collect::<Result<_, _>>(),
    }
}

fn function_arg_to_expr(
    arg: FunctionArg,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => lower_expr(e, cte_scope),
        FunctionArg::Named {
            arg: FunctionArgExpr::Expr(e),
            ..
        } => lower_expr(e, cte_scope),
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
            reason: "function argument shape not supported in τ".to_owned(),
        }),
    }
}

fn lower_join_operator(
    op: JoinOperator,
    cte_scope: &CteScope,
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
                reason: "join operator not supported in τ".to_owned(),
            });
        }
    };
    let (cond, using) = lower_join_constraint(constraint, cte_scope)?;
    Ok((join_type, cond, using))
}

fn lower_join_constraint(
    constraint: JoinConstraint,
    cte_scope: &CteScope,
) -> Result<(Option<Expression>, Vec<String>), EmissionError> {
    match constraint {
        JoinConstraint::On(expr) => Ok((Some(lower_expr(expr, cte_scope)?), vec![])),
        JoinConstraint::Using(cols) => {
            let names: Vec<String> = cols.iter().map(object_name_to_string).collect();
            Ok((None, names))
        }
        JoinConstraint::Natural | JoinConstraint::None => Ok((None, vec![])),
    }
}

fn lower_select_item(item: SelectItem, cte_scope: &CteScope) -> Result<Expression, EmissionError> {
    match item {
        SelectItem::UnnamedExpr(expr) => lower_expr(expr, cte_scope),
        SelectItem::ExprWithAlias { expr, alias } => {
            let inner = lower_expr(expr, cte_scope)?;
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
    // Fix pass (review M4): extend the walker to every composite
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
    // Fix pass (review M3 + perf OPT-5): defer to τ's canonical
    // aggregate roster (`transpiler_v2::type_inference::AGGREGATE_NAMES`)
    // instead of a locally-drifted 32-name subset. `eq_ignore_ascii_case`
    // avoids the per-call `String` allocation from `to_ascii_uppercase()`.
    AGGREGATE_NAMES.iter().any(|a| name.eq_ignore_ascii_case(a))
}

/// Build a non-null boolean literal expression — used to lower `IS [NOT] TRUE`
/// / `IS [NOT] FALSE` onto τ's `IsDistinctFrom` substrate.
fn bool_literal(b: bool) -> Expression {
    Expression::Literal(Literal {
        value: LiteralValue::Boolean(b),
        data_type: DataType::Boolean,
    })
}

fn lower_expr(expr: Expr, cte_scope: &CteScope) -> Result<Expression, EmissionError> {
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
                let l = lower_expr(*left, cte_scope)?;
                let r = lower_expr(*right, cte_scope)?;
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
            // Spark's null-safe equality `a <=> b` is defined as `NOT DISTINCT
            // FROM` — it returns a non-null boolean and treats `NULL <=> NULL`
            // as true. Lower directly onto τ's `IsDistinctFrom` substrate with
            // `negated: true` rather than routing through `lower_binary_op`
            // (which yields a `BinaryOp` enum and can't produce this shape).
            // Corpus witness: `whr-015`.
            if matches!(op, BinaryOperator::Spaceship) {
                return Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
                    left: Box::new(lower_expr(*left, cte_scope)?),
                    right: Box::new(lower_expr(*right, cte_scope)?),
                    negated: true,
                }));
            }
            Ok(Expression::Binary(BinaryExpression {
                op: lower_binary_op(op)?,
                left: Box::new(lower_expr(*left, cte_scope)?),
                right: Box::new(lower_expr(*right, cte_scope)?),
            }))
        }
        Expr::UnaryOp { op, expr } => match op {
            UnaryOperator::Not => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::Not,
                operand: Box::new(lower_expr(*expr, cte_scope)?),
            })),
            UnaryOperator::Minus => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::Negate,
                operand: Box::new(lower_expr(*expr, cte_scope)?),
            })),
            UnaryOperator::Plus => lower_expr(*expr, cte_scope),
            other => Err(EmissionError::UnsupportedProtoShape {
                shape: format!("sql::unary_op::{other:?}"),
                reason: "unary operator not supported in τ".to_owned(),
            }),
        },
        Expr::Nested(e) => lower_expr(*e, cte_scope),
        Expr::Cast {
            kind,
            expr,
            data_type,
            ..
        } => {
            let try_cast = matches!(kind, CastKind::TryCast | CastKind::SafeCast);
            Ok(Expression::Cast(CastExpression {
                expr: Box::new(lower_expr(*expr, cte_scope)?),
                to_type: lower_data_type(data_type)?,
                try_cast,
            }))
        }
        Expr::Function(f) => lower_function(f, cte_scope),
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            let branches = conditions
                .into_iter()
                .map(|c| {
                    Ok((
                        lower_expr(c.condition, cte_scope)?,
                        lower_expr(c.result, cte_scope)?,
                    ))
                })
                .collect::<Result<Vec<_>, EmissionError>>()?;
            let else_expr = else_result
                .map(|e| lower_expr(*e, cte_scope))
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
                list.into_iter().map(|e| lower_expr(e, cte_scope)).collect();
            Ok(Expression::InList(InListExpression {
                expr: Box::new(lower_expr(*expr, cte_scope)?),
                list: converted_list?,
                negated,
            }))
        }
        Expr::IsNull(e) => Ok(Expression::Unary(UnaryExpression {
            op: UnaryOp::IsNull,
            operand: Box::new(lower_expr(*e, cte_scope)?),
        })),
        Expr::IsNotNull(e) => Ok(Expression::Unary(UnaryExpression {
            op: UnaryOp::IsNotNull,
            operand: Box::new(lower_expr(*e, cte_scope)?),
        })),
        // `a IS [NOT] DISTINCT FROM b` — null-safe (in)equality yielding a
        // non-null boolean. Lower onto τ's `IsDistinctFrom` substrate; the
        // `IS NOT` form sets `negated: true`. Corpus witnesses: `pr-001`,
        // `pr-002`.
        Expr::IsDistinctFrom(a, b) => Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(lower_expr(*a, cte_scope)?),
            right: Box::new(lower_expr(*b, cte_scope)?),
            negated: false,
        })),
        Expr::IsNotDistinctFrom(a, b) => Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(lower_expr(*a, cte_scope)?),
            right: Box::new(lower_expr(*b, cte_scope)?),
            negated: true,
        })),
        // `x IS [NOT] TRUE` / `x IS [NOT] FALSE` — 3VL boolean tests yielding a
        // non-null boolean. Lower onto τ's `IsDistinctFrom` substrate:
        //   `x IS TRUE`      ⟺ `x IS NOT DISTINCT FROM TRUE`  (negated: true)
        //   `x IS NOT TRUE`  ⟺ `x IS DISTINCT FROM TRUE`      (negated: false)
        // and likewise for FALSE. NULL IS TRUE = false, NULL IS NOT TRUE = true.
        // Corpus witness: `pr-006`.
        Expr::IsTrue(e) => Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(lower_expr(*e, cte_scope)?),
            right: Box::new(bool_literal(true)),
            negated: true,
        })),
        Expr::IsNotTrue(e) => Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(lower_expr(*e, cte_scope)?),
            right: Box::new(bool_literal(true)),
            negated: false,
        })),
        Expr::IsFalse(e) => Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(lower_expr(*e, cte_scope)?),
            right: Box::new(bool_literal(false)),
            negated: true,
        })),
        Expr::IsNotFalse(e) => Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(lower_expr(*e, cte_scope)?),
            right: Box::new(bool_literal(false)),
            negated: false,
        })),
        Expr::Like {
            expr,
            pattern,
            negated,
            escape_char,
            ..
        } => Ok(Expression::Like(LikeExpression {
            value: Box::new(lower_expr(*expr, cte_scope)?),
            pattern: Box::new(lower_expr(*pattern, cte_scope)?),
            escape: escape_char.and_then(value_to_escape_char),
            negated,
            case_insensitive: false,
        })),
        // `x ILIKE 'p'` — case-insensitive LIKE. Mirrors the `Expr::Like` arm
        // but flags `case_insensitive: true`, which emission renders as
        // `ILIKE`. `NOT ILIKE` rides the same `negated` field as `NOT LIKE`.
        // Corpus witness: `whr-012` (`name ILIKE 'a%'`).
        Expr::ILike {
            expr,
            pattern,
            negated,
            escape_char,
            ..
        } => Ok(Expression::Like(LikeExpression {
            value: Box::new(lower_expr(*expr, cte_scope)?),
            pattern: Box::new(lower_expr(*pattern, cte_scope)?),
            escape: escape_char.and_then(value_to_escape_char),
            negated,
            case_insensitive: true,
        })),
        // `x RLIKE 'p'` / `x REGEXP 'p'` — regex match. Lower to a `rlike`
        // FunctionCall; emission's `rlike | regexp_like | regexp` arm renders
        // the Spark-correct regexp semantics. `NOT RLIKE` has no negated field
        // on the FunctionCall, so wrap the call in a `NOT` unary (same
        // substrate as `Expr::UnaryOp { Not, .. }`). Corpus witness: `whr-013`
        // (`name RLIKE '^[A-D]'`).
        Expr::RLike {
            expr,
            pattern,
            negated,
            ..
        } => {
            let call = Expression::FunctionCall(FunctionCall {
                name: "rlike".to_owned(),
                args: vec![
                    lower_expr(*expr, cte_scope)?,
                    lower_expr(*pattern, cte_scope)?,
                ],
                distinct: false,
            });
            if negated {
                Ok(Expression::Unary(UnaryExpression {
                    op: UnaryOp::Not,
                    operand: Box::new(call),
                }))
            } else {
                Ok(call)
            }
        }
        // `x SIMILAR TO 'p'` — SQL-standard regex is WHOLE-STRING (anchored) and
        // Spark has no `SIMILAR TO` operator at all. Borrowing `rlike`
        // (unanchored Java-regex `find`) would silently give wrong answers (e.g.
        // `'abc' SIMILAR TO 'b'` is FALSE but rlike would be TRUE). Reject as a
        // Thunderduck-boundary error per ADR-022 rather than mis-lower.
        Expr::SimilarTo { .. } => Err(EmissionError::UnsupportedProtoShape {
            shape: "sql::expr::similar_to".to_owned(),
            reason: "SIMILAR TO (anchored SQL-standard regex) has no Spark equivalent".to_owned(),
        }),
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
            let inner = lower_expr(*expr, cte_scope)?;
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
            let mut args = vec![lower_expr(*expr, cte_scope)?];
            if let Some(from) = substring_from {
                args.push(lower_expr(*from, cte_scope)?);
            }
            if let Some(for_) = substring_for {
                args.push(lower_expr(*for_, cte_scope)?);
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
            let mut args = vec![lower_expr(*expr, cte_scope)?];
            if let Some(what) = trim_what {
                args.push(lower_expr(*what, cte_scope)?);
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
            args: vec![lower_expr(*expr, cte_scope)?, lower_expr(*r#in, cte_scope)?],
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
                lower_expr(*expr, cte_scope)?,
                lower_expr(*overlay_what, cte_scope)?,
                lower_expr(*overlay_from, cte_scope)?,
            ];
            if let Some(for_) = overlay_for {
                args.push(lower_expr(*for_, cte_scope)?);
            }
            Ok(Expression::FunctionCall(FunctionCall {
                name: "overlay".to_owned(),
                args,
                distinct: false,
            }))
        }
        Expr::Lambda(lambda) => {
            let params: Vec<String> = lambda.params.iter().map(|p| p.value.clone()).collect();
            let body = lower_expr(*lambda.body, cte_scope)?;
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
        // Uncorrelated subqueries (scalar / IN / EXISTS). The inner plan is
        // lowered with the enclosing query's CTE scope so a subquery's
        // `FROM <cte>` inlines the CTE body rather than reading a same-named
        // catalog table — Spark shadows the table with the CTE (cte-006).
        // The analyzer rewrites `Unanalyzed` → `Analyzed` (correlated inner
        // refs fail resolution → honest Thunderduck boundary, ADR-022).
        Expr::Subquery(q) => Ok(Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(lower_query(*q, cte_scope)?)),
        })),
        Expr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(Expression::InSubquery(InSubquery {
            expr: Box::new(lower_expr(*expr, cte_scope)?),
            subquery: SubqueryPlan::Unanalyzed(Box::new(lower_query(*subquery, cte_scope)?)),
            negated,
        })),
        Expr::Exists { subquery, negated } => Ok(Expression::ExistsSubquery(ExistsSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(lower_query(*subquery, cte_scope)?)),
            negated,
        })),
        // Typed-string literals `DATE '...'` / `TIMESTAMP '...'` (lit-001,
        // lit-002). Spark's DATE/TIMESTAMP literals are NON-NULL constants, so
        // lower them to non-null `LiteralValue::Date`/`Timestamp` values (a
        // Literal is non-null by construction) rather than a `CAST(str AS ..)`
        // (nullable=TRUE). The string→epoch-days/-micros conversion is a
        // self-contained proleptic-Gregorian parser (no chrono dep). Malformed
        // input and other typed-string data types stay a Thunderduck boundary
        // (ADR-022).
        Expr::TypedString(ts) => lower_typed_string(ts),
        other => Err(EmissionError::UnsupportedProtoShape {
            shape: format!("sql::expr::{}", expr_kind(&other)),
            reason: "expression shape not supported in τ".to_owned(),
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
                reason: "binary operator not supported in τ".to_owned(),
            });
        }
    })
}

fn lower_function(f: Function, cte_scope: &CteScope) -> Result<Expression, EmissionError> {
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
            let converted: Result<Vec<Expression>, EmissionError> = args
                .into_iter()
                .map(|a| function_arg_to_expr(a, cte_scope))
                .collect();
            (distinct, converted?)
        }
        FunctionArguments::Subquery(_) => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::function_args_subquery".to_owned(),
                reason: "subquery function arguments not supported in τ".to_owned(),
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
                .map(|e| lower_expr(e, cte_scope))
                .collect::<Result<_, _>>()?;
            let order_by: Vec<SortOrder> = spec
                .order_by
                .into_iter()
                .map(|o| lower_order_by_expr(o, cte_scope))
                .collect::<Result<_, _>>()?;
            let frame = lower_window_frame(spec.window_frame, cte_scope)?;
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
fn lower_window_frame(
    frame: Option<SqlWindowFrame>,
    cte_scope: &CteScope,
) -> Result<Option<WindowFrame>, EmissionError> {
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
    let lower = lower_frame_bound(start_bound, cte_scope)?;
    // Shorthand `ROWS N PRECEDING` (no BETWEEN) → upper bound is CURRENT ROW.
    let upper = match end_bound {
        Some(b) => lower_frame_bound(b, cte_scope)?,
        None => FrameBoundary::CurrentRow,
    };
    Ok(Some(WindowFrame { unit, lower, upper }))
}

/// Map a single sqlparser [`WindowFrameBound`] into τ's [`FrameBoundary`].
///
/// sqlparser encodes the direction in the variant (`Preceding` / `Following`),
/// so the offset expression is the absolute magnitude — no sign re-application.
fn lower_frame_bound(
    bound: WindowFrameBound,
    cte_scope: &CteScope,
) -> Result<FrameBoundary, EmissionError> {
    Ok(match bound {
        WindowFrameBound::CurrentRow => FrameBoundary::CurrentRow,
        WindowFrameBound::Preceding(None) => FrameBoundary::UnboundedPreceding,
        WindowFrameBound::Following(None) => FrameBoundary::UnboundedFollowing,
        WindowFrameBound::Preceding(Some(e)) => {
            FrameBoundary::Preceding(Box::new(lower_expr(*e, cte_scope)?))
        }
        WindowFrameBound::Following(Some(e)) => {
            FrameBoundary::Following(Box::new(lower_expr(*e, cte_scope)?))
        }
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

/// Lower a `DATE '...'` / `TIMESTAMP '...'` typed-string literal to a NON-NULL
/// `LiteralValue::Date`/`Timestamp` value (Spark's DATE/TIMESTAMP literals are
/// non-null constants). See the `Expr::TypedString` arm.
fn lower_typed_string(ts: TypedString) -> Result<Expression, EmissionError> {
    let is_timestamp = match &ts.data_type {
        SqlDataType::Date => false,
        SqlDataType::Timestamp(_, _) => true,
        other => {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: format!("sql::typed_string::{other:?}"),
                reason: "only DATE and TIMESTAMP typed-string literals are supported".to_owned(),
            });
        }
    };
    let value = ts
        .value
        .into_string()
        .ok_or_else(|| EmissionError::UnsupportedProtoShape {
            shape: "sql::typed_string::non_string_value".to_owned(),
            reason: "typed-string literal value must be a string".to_owned(),
        })?;
    let (literal, data_type) = if is_timestamp {
        let micros = parse_timestamp_to_epoch_micros(&value).ok_or_else(|| {
            EmissionError::UnsupportedProtoShape {
                shape: "sql::typed_string::malformed".to_owned(),
                reason: format!("cannot parse TIMESTAMP literal `{value}`"),
            }
        })?;
        (LiteralValue::Timestamp(micros), DataType::Timestamp)
    } else {
        let days = parse_date_to_epoch_days(&value).ok_or_else(|| {
            EmissionError::UnsupportedProtoShape {
                shape: "sql::typed_string::malformed".to_owned(),
                reason: format!("cannot parse DATE literal `{value}`"),
            }
        })?;
        (LiteralValue::Date(days), DataType::Date)
    };
    Ok(Expression::Literal(Literal {
        value: literal,
        data_type,
    }))
}

/// Parse a `YYYY-MM-DD` date string into days since the Unix epoch
/// (1970-01-01), using the proleptic-Gregorian civil algorithm (Howard
/// Hinnant `days_from_civil`). Returns `None` on malformed input.
fn parse_date_to_epoch_days(s: &str) -> Option<i32> {
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;
    // Bound the year to Spark's DATE domain [1, 9999] (M1). This both matches
    // Spark's supported range and keeps `days_from_civil` (era * 146097) and the
    // downstream timestamp micros multiply (`days * 86_400_000_000`) far from
    // i64 overflow — no panic in debug, no silent wrap in release.
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    // Validate the day against the actual length of the month, leap-year aware
    // (H1). Spark ANSI rejects e.g. `2026-02-30`, `2026-04-31`, `2023-02-29`
    // rather than silently rolling over to a wrong date.
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let max_day = days_in_month[(month - 1) as usize];
    if !(1..=max_day).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) as i32)
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01 for a
/// proleptic-Gregorian `(year, month, day)` with `month ∈ [1,12]`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Parse a `YYYY-MM-DD HH:MM:SS[.ffffff]` timestamp string (space or `T`
/// separator; optional fractional seconds) into microseconds since the Unix
/// epoch. No timezone handling — treated as a session-local wall-clock instant,
/// matching how τ's `Timestamp` literal is interpreted. Returns `None` on
/// malformed input.
fn parse_timestamp_to_epoch_micros(s: &str) -> Option<i64> {
    let trimmed = s.trim();
    let (date_part, time_part) = match trimmed.split_once(['T', ' ']) {
        Some((d, t)) => (d, t),
        None => (trimmed, "00:00:00"),
    };
    let days = parse_date_to_epoch_days(date_part)? as i64;

    let (hms, frac) = match time_part.split_once('.') {
        Some((h, f)) => (h, Some(f)),
        None => (time_part, None),
    };
    let time_fields: Vec<&str> = hms.split(':').collect();
    if time_fields.len() != 3 {
        return None;
    }
    let hh: i64 = time_fields[0].parse().ok()?;
    let mm: i64 = time_fields[1].parse().ok()?;
    let ss: i64 = time_fields[2].parse().ok()?;
    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }

    // Fractional seconds → microseconds: pad/truncate the digits to exactly 6.
    let frac_micros: i64 = match frac {
        None => 0,
        Some(f) => {
            if f.is_empty() || !f.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let mut digits: String = f.chars().take(6).collect();
            while digits.len() < 6 {
                digits.push('0');
            }
            digits.parse().ok()?
        }
    };

    Some(days * 86_400_000_000 + (hh * 3600 + mm * 60 + ss) * 1_000_000 + frac_micros)
}

/// Derive `(precision, scale)` for a bare SQL decimal literal, mirroring the
/// value-derived branch of the connect-server `normalize_decimal_literal`
/// (Apache Spark `Decimal.set()`): `scale` = fractional digits; `precision` =
/// significant integer digits + scale, floored at `max(scale, 1)`. Sign and
/// leading integer zeros are not significant. `100.25`→(5,2); `3.142`→(4,3);
/// `0.00`→(2,2).
fn decimal_literal_precision_scale(s: &str) -> (u8, u8) {
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
    // Clamp precision to DECIMAL's MAX_PRECISION = 38 (M2), matching the mirrored
    // `normalize_decimal_literal` in v2_relation_converter.rs. A literal with more
    // than 38 significant digits must not yield `Decimal(precision > 38)`, which is
    // invalid in both Spark and DuckDB.
    let mut precision = raw_int_digits
        .saturating_add(scale)
        .max(scale)
        .max(1)
        .min(38);
    if scale > precision {
        precision = scale.min(38);
    }
    (precision, scale)
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
            } else if s.contains('.') && !s.contains(['e', 'E']) {
                // Spark parses a fixed-point numeric literal (a `.` with no
                // exponent) as DECIMAL, not DOUBLE — e.g. `100.25` is
                // Decimal(5,2). Preserve the literal string to keep precision;
                // exponent forms (`1.5e3`) still route to Double below (lit-007).
                let (precision, scale) = decimal_literal_precision_scale(&s);
                Ok(Expression::Literal(Literal {
                    value: LiteralValue::Decimal {
                        value: s,
                        precision,
                        scale,
                    },
                    data_type: DataType::Decimal { precision, scale },
                }))
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
            reason: "literal value shape not supported in τ".to_owned(),
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
                reason: "data type not supported in τ".to_owned(),
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
    cte_scope: &CteScope,
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
        .map(|o| lower_order_by_expr(o, cte_scope))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CommonAst::new(CommonOp::Sort {
        input: Box::new(plan),
        order,
        limit: limit_i,
        offset: offset_i,
    }))
}

fn lower_order_by_expr(ob: OrderByExpr, cte_scope: &CteScope) -> Result<SortOrder, EmissionError> {
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
        expr: Box::new(lower_expr(ob.expr, cte_scope)?),
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
            reason: "LIMIT/OFFSET must be an integer literal in τ".to_owned(),
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
    fn parse_select_distinct_wraps_project_in_deduplicate() {
        let plan = parse("SELECT DISTINCT a, b FROM t").expect("should parse");
        match plan.op {
            CommonOp::Deduplicate { input, on_columns } => {
                assert!(on_columns.is_empty(), "plain DISTINCT dedupes all columns");
                assert!(
                    matches!(input.op, CommonOp::Project { .. }),
                    "Deduplicate must wrap the Project"
                );
            }
            _ => panic!("expected Deduplicate over Project"),
        }
    }

    #[test]
    fn parse_select_distinct_with_order_by_sorts_deduplicate() {
        let plan = parse("SELECT DISTINCT a FROM t ORDER BY a").expect("should parse");
        // Dedupe first, then order: Sort(Deduplicate(Project)).
        match plan.op {
            CommonOp::Sort { input, .. } => match input.op {
                CommonOp::Deduplicate { input, on_columns } => {
                    assert!(on_columns.is_empty());
                    assert!(matches!(input.op, CommonOp::Project { .. }));
                }
                _ => panic!("expected Deduplicate under Sort"),
            },
            _ => panic!("expected Sort over Deduplicate"),
        }
    }

    #[test]
    fn parse_select_distinct_on_rejected() {
        let err = parse("SELECT DISTINCT ON (a) a, b FROM t").expect_err("DISTINCT ON is invalid");
        assert!(
            matches!(
                err,
                EmissionError::UnsupportedProtoShape { ref shape, .. } if shape == "sql::distinct_on"
            ),
            "expected sql::distinct_on boundary error, got {err:?}"
        );
    }

    /// Extract the single projection expression under a top-level `Project`.
    fn single_projection(plan: &CommonAst) -> &Expression {
        match &plan.op {
            CommonOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 1);
                &projections[0]
            }
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn typed_string_date_lowers_to_nonnull_date_literal() {
        let plan = parse("SELECT DATE '2026-01-15' AS d").expect("should parse");
        let inner = match single_projection(&plan) {
            Expression::Alias(a) => a.expr.as_ref(),
            other => panic!("expected Alias, got {other:?}"),
        };
        // 2026-01-15 is 20468 days after 1970-01-01 (non-null literal).
        match inner {
            Expression::Literal(Literal {
                value: LiteralValue::Date(days),
                data_type: DataType::Date,
            }) => assert_eq!(*days, 20468),
            other => panic!("expected Date literal, got {other:?}"),
        }
    }

    #[test]
    fn typed_string_timestamp_lowers_to_nonnull_timestamp_literal() {
        let plan = parse("SELECT TIMESTAMP '2026-01-15 10:30:00' AS ts").expect("should parse");
        let inner = match single_projection(&plan) {
            Expression::Alias(a) => a.expr.as_ref(),
            other => panic!("expected Alias, got {other:?}"),
        };
        // 20468 days * 86_400_000_000 + (10*3600 + 30*60) * 1_000_000.
        match inner {
            Expression::Literal(Literal {
                value: LiteralValue::Timestamp(micros),
                data_type: DataType::Timestamp,
            }) => assert_eq!(*micros, 1_768_473_000_000_000),
            other => panic!("expected Timestamp literal, got {other:?}"),
        }
    }

    #[test]
    fn typed_string_malformed_date_is_boundary_error() {
        let err = parse("SELECT DATE 'nope' AS d").expect_err("should reject");
        match err {
            EmissionError::UnsupportedProtoShape { shape, .. } => {
                assert_eq!(shape, "sql::typed_string::malformed");
            }
            other => panic!("expected boundary error, got {other:?}"),
        }
    }

    #[test]
    fn parse_date_to_epoch_days_known_anchors() {
        assert_eq!(parse_date_to_epoch_days("1970-01-01"), Some(0));
        assert_eq!(parse_date_to_epoch_days("2000-01-01"), Some(10957));
        assert_eq!(parse_date_to_epoch_days("2026-01-15"), Some(20468));
        assert_eq!(parse_date_to_epoch_days("nope"), None);
    }

    #[test]
    fn parse_date_rejects_invalid_calendar_days() {
        // H1: days that overrun the month must be rejected, not rolled over.
        assert_eq!(parse_date_to_epoch_days("2026-02-30"), None);
        assert_eq!(parse_date_to_epoch_days("2026-04-31"), None);
        // 2023 is not a leap year → Feb 29 is invalid.
        assert_eq!(parse_date_to_epoch_days("2023-02-29"), None);
        // Month out of range.
        assert_eq!(parse_date_to_epoch_days("2026-13-01"), None);
        assert_eq!(parse_date_to_epoch_days("2026-00-01"), None);
        // Day out of range.
        assert_eq!(parse_date_to_epoch_days("2026-01-00"), None);
    }

    #[test]
    fn parse_date_accepts_leap_day() {
        // 2024 is a leap year → Feb 29 is valid. 2024-02-29 is 19782 days
        // after 1970-01-01.
        assert_eq!(parse_date_to_epoch_days("2024-02-29"), Some(19782));
    }

    #[test]
    fn parse_date_rejects_out_of_range_year() {
        // M1: years outside Spark's DATE domain [1, 9999] are rejected without
        // overflow/panic in the civil-day arithmetic.
        assert_eq!(parse_date_to_epoch_days("99999-01-01"), None);
        assert_eq!(parse_date_to_epoch_days("0000-01-01"), None);
    }

    #[test]
    fn typed_string_invalid_calendar_date_is_boundary_error() {
        let err = parse("SELECT DATE '2026-02-30' AS d").expect_err("should reject");
        match err {
            EmissionError::UnsupportedProtoShape { shape, .. } => {
                assert_eq!(shape, "sql::typed_string::malformed");
            }
            other => panic!("expected boundary error, got {other:?}"),
        }
        // Year out of range must also be a boundary error, not a panic.
        let err = parse("SELECT DATE '99999-01-01' AS d").expect_err("should reject");
        assert!(matches!(err, EmissionError::UnsupportedProtoShape { .. }));
    }

    #[test]
    fn decimal_literal_lowers_with_precision_and_scale() {
        let plan = parse("SELECT 100.25").expect("should parse");
        match single_projection(&plan) {
            Expression::Literal(Literal {
                value:
                    LiteralValue::Decimal {
                        value,
                        precision,
                        scale,
                    },
                data_type,
            }) => {
                assert_eq!(value, "100.25");
                assert_eq!(*precision, 5);
                assert_eq!(*scale, 2);
                assert_eq!(
                    *data_type,
                    DataType::Decimal {
                        precision: 5,
                        scale: 2
                    }
                );
            }
            other => panic!("expected Decimal literal, got {other:?}"),
        }
    }

    #[test]
    fn decimal_literal_precision_scale_three_digit_fraction() {
        let plan = parse("SELECT 3.142").expect("should parse");
        match single_projection(&plan) {
            Expression::Literal(Literal {
                value:
                    LiteralValue::Decimal {
                        precision, scale, ..
                    },
                ..
            }) => {
                assert_eq!(*precision, 4);
                assert_eq!(*scale, 3);
            }
            other => panic!("expected Decimal literal, got {other:?}"),
        }
    }

    #[test]
    fn integer_literal_stays_integer_not_decimal() {
        let plan = parse("SELECT 42").expect("should parse");
        assert!(matches!(
            single_projection(&plan),
            Expression::Literal(Literal {
                value: LiteralValue::Int(42),
                data_type: DataType::Integer,
            })
        ));
    }

    #[test]
    fn decimal_literal_precision_scale_helper_matches_spark() {
        assert_eq!(decimal_literal_precision_scale("100.25"), (5, 2));
        assert_eq!(decimal_literal_precision_scale("3.142"), (4, 3));
        assert_eq!(decimal_literal_precision_scale("0.00"), (2, 2));
    }

    #[test]
    fn decimal_literal_precision_clamped_to_max_38() {
        // M2: a literal with more than 38 significant integer digits must not
        // produce Decimal(precision > 38) — clamp to MAX_PRECISION = 38, matching
        // normalize_decimal_literal in v2_relation_converter.rs.
        let forty_digits = "1234567890123456789012345678901234567890.5";
        let (precision, scale) = decimal_literal_precision_scale(forty_digits);
        assert_eq!(precision, 38);
        assert_eq!(scale, 1);
    }

    /// Extract the `Filter` predicate immediately under a top-level `Project`.
    fn where_predicate(plan: &CommonAst) -> &Expression {
        match &plan.op {
            CommonOp::Project { input, .. } => match &input.op {
                CommonOp::Filter { condition, .. } => condition,
                _ => panic!("expected Filter under Project"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn parse_is_distinct_from() {
        let plan = parse("SELECT * FROM t WHERE a IS DISTINCT FROM b").expect("should parse");
        match where_predicate(&plan) {
            Expression::IsDistinctFrom(idf) => assert!(!idf.negated),
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    #[test]
    fn parse_is_not_distinct_from() {
        let plan = parse("SELECT * FROM t WHERE a IS NOT DISTINCT FROM b").expect("should parse");
        match where_predicate(&plan) {
            Expression::IsDistinctFrom(idf) => assert!(idf.negated),
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    #[test]
    fn parse_null_safe_equals_spaceship() {
        let plan = parse("SELECT * FROM t WHERE a <=> b").expect("should parse");
        match where_predicate(&plan) {
            Expression::IsDistinctFrom(idf) => assert!(idf.negated),
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    /// Assert the `where_predicate` is an `IsDistinctFrom` whose right operand
    /// is a boolean literal `expected_bool` and whose `negated` flag matches.
    fn assert_bool_test(plan: &CommonAst, expected_bool: bool, expected_negated: bool) {
        match where_predicate(plan) {
            Expression::IsDistinctFrom(idf) => {
                assert_eq!(idf.negated, expected_negated);
                match idf.right.as_ref() {
                    Expression::Literal(Literal {
                        value: LiteralValue::Boolean(b),
                        data_type: DataType::Boolean,
                    }) => assert_eq!(*b, expected_bool),
                    other => panic!("expected Boolean literal, got {other:?}"),
                }
            }
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    #[test]
    fn parse_is_true() {
        let plan = parse("SELECT * FROM t WHERE a IS TRUE").expect("should parse");
        assert_bool_test(&plan, true, true);
    }

    #[test]
    fn parse_is_not_true() {
        let plan = parse("SELECT * FROM t WHERE a IS NOT TRUE").expect("should parse");
        assert_bool_test(&plan, true, false);
    }

    #[test]
    fn parse_is_false() {
        let plan = parse("SELECT * FROM t WHERE a IS FALSE").expect("should parse");
        assert_bool_test(&plan, false, true);
    }

    #[test]
    fn parse_is_not_false() {
        let plan = parse("SELECT * FROM t WHERE a IS NOT FALSE").expect("should parse");
        assert_bool_test(&plan, false, false);
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
    fn parse_group_by_all_groups_by_non_aggregate_items() {
        // GROUP BY ALL groups by the non-aggregate SELECT items (a, b), not count(*).
        let plan = parse("SELECT a, b, COUNT(*) FROM t GROUP BY ALL").expect("should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                aggregates,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::GroupBy);
                assert_eq!(grouping.len(), 2, "GROUP BY ALL groups by a, b");
                assert_eq!(aggregates.len(), 3, "projection is a, b, count(*)");
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_order_by_all_orders_by_every_output_column() {
        let plan = parse("SELECT a, b FROM t ORDER BY ALL").expect("should parse");
        match plan.op {
            CommonOp::Sort { order, .. } => {
                assert_eq!(order.len(), 2, "ORDER BY ALL orders by both output columns");
            }
            _ => panic!("expected Sort over the projection"),
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
        // GROUPING SETS still needs set-membership substrate — reject.
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
    fn parse_pivot_lowers_to_common_op_pivot() {
        // Pass 107: `SELECT * FROM t PIVOT (...)` now lowers to a
        // `CommonOp::Pivot` with implicit (schema-derived) grouping.
        let plan =
            parse("SELECT * FROM t PIVOT (SUM(x) FOR y IN (1, 2))").expect("PIVOT should lower");
        match pivot_node(plan) {
            CommonOp::Pivot { grouping, .. } => {
                assert_eq!(grouping, PivotGrouping::Implicit);
            }
            other => panic!("expected Pivot, got {other:?}"),
        }
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

    // ── Pass 106 — uncorrelated subquery lowering ────────────────────────

    #[test]
    fn scalar_subquery_lowers_to_unanalyzed_scalar_subquery() {
        let plan = parse("SELECT (SELECT max(sal) FROM emp) AS gmax FROM emp").expect("parse");
        match first_projection(plan) {
            Expression::ScalarSubquery(s) => {
                assert!(
                    matches!(s.subquery, SubqueryPlan::Unanalyzed(_)),
                    "front-end must emit an Unanalyzed inner plan"
                );
            }
            other => panic!("expected ScalarSubquery, got {other:?}"),
        }
    }

    /// Extract the WHERE condition of a `SELECT * FROM t WHERE …` plan, which
    /// lowers to `Project(Star) → Filter → TableScan`.
    fn filter_condition(plan: CommonAst) -> Expression {
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project as top-level");
        };
        let CommonOp::Filter { condition, .. } = input.op else {
            panic!("expected Filter under Project");
        };
        condition
    }

    #[test]
    fn in_subquery_lowers_and_preserves_negated() {
        let plan = parse("SELECT * FROM emp WHERE dept_id NOT IN (SELECT dept_id FROM dept)")
            .expect("parse");
        match filter_condition(plan) {
            Expression::InSubquery(i) => {
                assert!(i.negated, "NOT IN → negated");
                assert!(matches!(i.subquery, SubqueryPlan::Unanalyzed(_)));
            }
            other => panic!("expected InSubquery, got {other:?}"),
        }
    }

    #[test]
    fn exists_subquery_lowers_to_unanalyzed_exists() {
        let plan = parse("SELECT * FROM emp WHERE EXISTS (SELECT 1 FROM dept)").expect("parse");
        match filter_condition(plan) {
            Expression::ExistsSubquery(e) => {
                assert!(!e.negated);
                assert!(matches!(e.subquery, SubqueryPlan::Unanalyzed(_)));
            }
            other => panic!("expected ExistsSubquery, got {other:?}"),
        }
    }

    #[test]
    fn subquery_sees_outer_cte_scope() {
        // Review M1: a subquery's `FROM <cte>` must inline the outer CTE body
        // (an AliasedRelation over the CTE's own plan), NOT a TableScan named
        // `c`. If a real table `c` existed, a TableScan would silently read it
        // instead of the CTE — Spark shadows the table with the CTE (cte-006).
        let plan = parse(
            "WITH c AS (SELECT dept_id FROM dept) \
             SELECT * FROM emp WHERE dept_id IN (SELECT dept_id FROM c)",
        )
        .expect("parse");
        let inner = match filter_condition(plan) {
            Expression::InSubquery(i) => match i.subquery {
                SubqueryPlan::Unanalyzed(inner) => *inner,
                other => panic!("expected Unanalyzed inner plan, got {other:?}"),
            },
            other => panic!("expected InSubquery, got {other:?}"),
        };
        // Inner plan: Project(dept_id) → AliasedRelation("c", <CTE body>).
        let CommonOp::Project { input, .. } = inner.op else {
            panic!(
                "expected Project as the subquery's top node, got {:?}",
                inner.op
            );
        };
        match input.op {
            CommonOp::AliasedRelation { alias, input } => {
                assert_eq!(alias, "c", "the CTE name is the AliasedRelation alias");
                assert!(
                    matches!(input.op, CommonOp::Project { .. }),
                    "expected the inlined CTE body (a Project), got {:?}",
                    input.op
                );
            }
            other => panic!(
                "expected AliasedRelation over the CTE body — a bare TableScan \
                 would mean the CTE was invisible inside the subquery, got {other:?}"
            ),
        }
    }

    // ── SQL PIVOT / UNPIVOT lowering (pass 107) ──────────────────────────

    /// Find the `CommonOp::Pivot` node under the outer `SELECT * FROM (…) PIVOT`.
    fn pivot_node(plan: CommonAst) -> CommonOp {
        match plan.op {
            CommonOp::Project { input, .. } => input.op,
            other => panic!("expected Project over Pivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_sql_pivot_marks_grouping_implicit_and_wraps_aliased_values() {
        // pv-001 shape: aliased FOR values must round-trip as `Alias` exprs so
        // the analyzer can name the output columns after the aliases.
        let plan = parse(
            "SELECT * FROM (SELECT dept_id, active, salary FROM emp) \
             PIVOT (avg(salary) FOR active IN (true AS act, false AS inact))",
        )
        .expect("should parse+lower");
        match pivot_node(plan) {
            CommonOp::Pivot {
                grouping,
                pivot_column,
                pivot_values,
                aggregates,
                ..
            } => {
                assert_eq!(grouping, PivotGrouping::Implicit);
                // Pivot column is the FOR column.
                assert!(
                    matches!(pivot_column, Expression::UnresolvedColumn(ref u) if u.name == "active")
                );
                // Both values are Alias-wrapped (true AS act / false AS inact).
                assert_eq!(pivot_values.len(), 2);
                match &pivot_values[0] {
                    Expression::Alias(a) => assert_eq!(a.alias, "act"),
                    other => panic!("expected Alias value, got {other:?}"),
                }
                match &pivot_values[1] {
                    Expression::Alias(a) => assert_eq!(a.alias, "inact"),
                    other => panic!("expected Alias value, got {other:?}"),
                }
                assert_eq!(aggregates.len(), 1);
            }
            other => panic!("expected Pivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_sql_pivot_bare_numeric_values_stay_bare() {
        // pv-005 shape: no aliases ⇒ values must NOT be wrapped in Alias.
        let plan = parse(
            "SELECT * FROM (SELECT dept_id, salary FROM emp) \
             PIVOT (avg(salary) FOR dept_id IN (10, 20, 30))",
        )
        .expect("should parse+lower");
        match pivot_node(plan) {
            CommonOp::Pivot {
                grouping,
                pivot_values,
                ..
            } => {
                assert_eq!(grouping, PivotGrouping::Implicit);
                assert_eq!(pivot_values.len(), 3);
                for v in &pivot_values {
                    assert!(
                        matches!(v, Expression::Literal(_)),
                        "bare pivot value must stay a Literal, got {v:?}"
                    );
                }
            }
            other => panic!("expected Pivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_sql_pivot_dynamic_values_rejected() {
        let err = parse(
            "SELECT * FROM (SELECT dept_id, active, salary FROM emp) \
             PIVOT (avg(salary) FOR active IN (ANY))",
        );
        // ANY / dynamic values are a Thunderduck-boundary reject.
        assert!(err.is_err(), "dynamic PIVOT values must be rejected");
    }

    #[test]
    fn lower_sql_unpivot_marks_ids_implicit_and_maps_names() {
        // pv-004 shape: value/name/columns map through; ids are Implicit.
        let plan = parse(
            "SELECT id, metric, val FROM (SELECT id, age, salary FROM emp) \
             UNPIVOT (val FOR metric IN (age, salary))",
        )
        .expect("should parse+lower");
        match pivot_node(plan) {
            CommonOp::Unpivot {
                ids,
                values,
                variable_column_name,
                value_column_name,
                ..
            } => {
                assert_eq!(ids, UnpivotIds::Implicit);
                assert_eq!(values, vec!["age".to_owned(), "salary".to_owned()]);
                assert_eq!(variable_column_name, "metric");
                assert_eq!(value_column_name, "val");
            }
            other => panic!("expected Unpivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_ilike_sets_case_insensitive() {
        // whr-012 shape: `name ILIKE 'a%'` → case-insensitive LIKE.
        let plan = parse("SELECT id FROM t WHERE a ILIKE 'x%'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Like(l) => {
                assert!(l.case_insensitive, "ILIKE must flag case_insensitive");
                assert!(!l.negated);
            }
            other => panic!("expected Like, got {other:?}"),
        }
    }

    #[test]
    fn lower_not_ilike_sets_negated() {
        let plan = parse("SELECT id FROM t WHERE a NOT ILIKE 'x%'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Like(l) => {
                assert!(l.case_insensitive);
                assert!(l.negated, "NOT ILIKE must set negated");
            }
            other => panic!("expected Like, got {other:?}"),
        }
    }

    #[test]
    fn lower_rlike_maps_to_rlike_function() {
        // whr-013 shape: `name RLIKE 'p'` → rlike(name, 'p').
        let plan = parse("SELECT id FROM t WHERE a RLIKE 'p'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::FunctionCall(f) => {
                assert_eq!(f.name, "rlike");
                assert_eq!(f.args.len(), 2);
                assert!(!f.distinct);
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn lower_not_rlike_wraps_in_not() {
        let plan = parse("SELECT id FROM t WHERE a NOT RLIKE 'p'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Unary(u) => {
                assert!(matches!(u.op, UnaryOp::Not));
                match u.operand.as_ref() {
                    Expression::FunctionCall(f) => {
                        assert_eq!(f.name, "rlike");
                        assert_eq!(f.args.len(), 2);
                    }
                    other => panic!("expected rlike FunctionCall, got {other:?}"),
                }
            }
            other => panic!("expected Unary NOT, got {other:?}"),
        }
    }
}
