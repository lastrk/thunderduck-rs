//! SqlConverter: sqlparser-rs AST → Thunderduck LogicalPlan + Expression.

use sqlparser::ast::{
    ArrayElemTypeDef, BinaryOperator, CastKind, DataType as SqlDataType, DuplicateTreatment,
    ExactNumberInfo, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr,
    JoinConstraint, JoinOperator, LimitClause, ObjectName, ObjectNamePart, OrderByExpr,
    OrderByKind, Query, Select, SelectItem, SelectItemQualifiedWildcardKind, SetExpr,
    SetQuantifier, Statement, TableFactor, TableWithJoins, UnaryOperator, Value,
    ValueWithSpan, WindowFrameBound, WindowFrameUnits, WindowSpec, WindowType,
};
use crate::error::{Result, ThunderduckError};
use crate::expression::*;
use crate::logical::*;
use crate::types::DataType;

pub struct SqlConverter;

impl SqlConverter {
    pub fn new() -> Self { Self }

    pub fn convert_statement(&self, stmt: Statement) -> Result<LogicalPlan> {
        match stmt {
            Statement::Query(q) => self.convert_query(*q),
            other => Err(ThunderduckError::Unsupported(
                format!("SQL statement type not yet supported: {}", other.to_string().split_whitespace().next().unwrap_or("?"))
            )),
        }
    }

    fn convert_query(&self, query: Query) -> Result<LogicalPlan> {
        let ctes: Vec<(String, Box<LogicalPlan>)> = if let Some(with) = query.with {
            with.cte_tables
                .into_iter()
                .map(|cte| {
                    let name = cte.alias.name.value.clone();
                    let plan = self.convert_query(*cte.query)?;
                    Ok((name, Box::new(plan)))
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            vec![]
        };

        // Extract order_by, limit, offset from the new Query structure
        let order_by_exprs: Vec<OrderByExpr> = match &query.order_by {
            Some(ob) => match &ob.kind {
                OrderByKind::Expressions(exprs) => exprs.clone(),
                OrderByKind::All(_) => vec![],
            },
            None => vec![],
        };
        let (limit_expr_opt, offset_opt) = self.extract_limit_offset(&query.limit_clause);

        let inner = self.convert_set_expr(*query.body, &order_by_exprs, limit_expr_opt, offset_opt)?;

        if ctes.is_empty() {
            Ok(inner)
        } else {
            Ok(LogicalPlan::WithCte(WithCte {
                ctes,
                input: Box::new(inner),
            }))
        }
    }

    fn extract_limit_offset(&self, clause: &Option<LimitClause>) -> (Option<Expr>, Option<Expr>) {
        match clause {
            None => (None, None),
            Some(LimitClause::LimitOffset { limit, offset, .. }) => {
                let off = offset.as_ref().map(|o| o.value.clone());
                (limit.clone(), off)
            }
            Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
                (Some(limit.clone()), Some(offset.clone()))
            }
        }
    }

    fn convert_set_expr(
        &self,
        body: SetExpr,
        order_by: &[OrderByExpr],
        limit: Option<Expr>,
        offset: Option<Expr>,
    ) -> Result<LogicalPlan> {
        match body {
            SetExpr::Select(sel) => {
                self.convert_select_body(*sel, order_by.to_vec(), limit, offset)
            }
            SetExpr::SetOperation { op, set_quantifier, left, right } => {
                let left_plan = self.convert_set_expr(*left, &[], None, None)?;
                let right_plan = self.convert_set_expr(*right, &[], None, None)?;
                let all = matches!(set_quantifier, SetQuantifier::All | SetQuantifier::AllByName);
                let base = match op {
                    sqlparser::ast::SetOperator::Union => LogicalPlan::Union(Union {
                        left: Box::new(left_plan),
                        right: Box::new(right_plan),
                        all,
                    }),
                    sqlparser::ast::SetOperator::Except | sqlparser::ast::SetOperator::Minus => {
                        LogicalPlan::Except(Except {
                            left: Box::new(left_plan),
                            right: Box::new(right_plan),
                            all,
                        })
                    }
                    sqlparser::ast::SetOperator::Intersect => LogicalPlan::Intersect(Intersect {
                        left: Box::new(left_plan),
                        right: Box::new(right_plan),
                        all,
                    }),
                };
                self.wrap_with_sort_limit(base, order_by.to_vec(), limit, offset)
            }
            SetExpr::Query(q) => self.convert_query(*q),
            other => Err(ThunderduckError::Unsupported(format!("set expression not supported: {:?}", other))),
        }
    }

    fn convert_select_body(
        &self,
        select: Select,
        order_by: Vec<OrderByExpr>,
        limit: Option<Expr>,
        offset: Option<Expr>,
    ) -> Result<LogicalPlan> {
        let base = self.convert_from(select.from)?;

        let base = if let Some(cond) = select.selection {
            LogicalPlan::Filter(Filter {
                input: Box::new(base),
                condition: self.convert_expr(cond)?,
            })
        } else {
            base
        };

        let has_group_by = !matches!(&select.group_by, GroupByExpr::Expressions(v, _) if v.is_empty());
        let has_aggregates = has_group_by
            || select.projection.iter().any(|item| self.select_item_has_aggregate(item));

        let plan = if has_aggregates {
            self.convert_aggregate_select(base, &select.projection, select.group_by, select.having)?
        } else {
            let projections: Result<Vec<Expression>> = select.projection
                .into_iter()
                .map(|item| self.convert_select_item(item))
                .collect();
            let projections = projections?;

            let projected = LogicalPlan::Project(Project {
                input: Box::new(base),
                projections,
            });

            if select.distinct.is_some() {
                LogicalPlan::Distinct(Distinct { input: Box::new(projected), columns: vec![] })
            } else {
                projected
            }
        };

        self.wrap_with_sort_limit(plan, order_by, limit, offset)
    }

    fn convert_from(&self, from: Vec<TableWithJoins>) -> Result<LogicalPlan> {
        if from.is_empty() {
            return Ok(LogicalPlan::SingleRow(SingleRowRelation));
        }
        let mut plans = from.into_iter()
            .map(|twj| self.convert_table_with_joins(twj))
            .collect::<Result<Vec<_>>>()?;

        let first = plans.remove(0);
        plans.into_iter().try_fold(first, |acc, next| {
            Ok(LogicalPlan::Join(Join {
                left: Box::new(acc),
                right: Box::new(next),
                join_type: JoinType::Cross,
                condition: None,
                using_columns: vec![],
                left_alias: None,
                right_alias: None,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            }))
        })
    }

    fn convert_table_with_joins(&self, twj: TableWithJoins) -> Result<LogicalPlan> {
        let mut plan = self.convert_table_factor(twj.relation)?;
        for join in twj.joins {
            let right = self.convert_table_factor(join.relation)?;
            let (join_type, condition, using_columns) = self.convert_join_operator(join.join_operator)?;
            plan = LogicalPlan::Join(Join {
                left: Box::new(plan),
                right: Box::new(right),
                join_type,
                condition,
                using_columns,
                left_alias: None,
                right_alias: None,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            });
        }
        Ok(plan)
    }

    fn convert_table_factor(&self, factor: TableFactor) -> Result<LogicalPlan> {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                let table = self.object_name_to_string(&name);
                let alias_str = alias.map(|a| a.name.value);
                Ok(LogicalPlan::TableScan(TableScan { table, alias: alias_str }))
            }
            TableFactor::Derived { subquery, alias, .. } => {
                let inner = self.convert_query(*subquery)?;
                let (alias_str, column_aliases) = match alias {
                    Some(a) => {
                        let alias_str = a.name.value;
                        let cols = a.columns.into_iter().map(|c| c.name.value).collect();
                        (alias_str, cols)
                    }
                    None => ("subquery".to_string(), vec![]),
                };
                Ok(LogicalPlan::AliasedRelation(AliasedRelation {
                    input: Box::new(inner),
                    alias: alias_str,
                    column_aliases,
                }))
            }
            other => Err(ThunderduckError::Unsupported(format!("table factor not supported: {:?}", other))),
        }
    }

    fn convert_join_operator(&self, op: JoinOperator) -> Result<(JoinType, Option<Expression>, Vec<String>)> {
        match op {
            JoinOperator::Join(constraint) | JoinOperator::Inner(constraint) => {
                let (cond, using) = self.convert_join_constraint(constraint)?;
                Ok((JoinType::Inner, cond, using))
            }
            JoinOperator::Left(constraint) | JoinOperator::LeftOuter(constraint) => {
                let (cond, using) = self.convert_join_constraint(constraint)?;
                Ok((JoinType::Left, cond, using))
            }
            JoinOperator::Right(constraint) | JoinOperator::RightOuter(constraint) => {
                let (cond, using) = self.convert_join_constraint(constraint)?;
                Ok((JoinType::Right, cond, using))
            }
            JoinOperator::FullOuter(constraint) => {
                let (cond, using) = self.convert_join_constraint(constraint)?;
                Ok((JoinType::Full, cond, using))
            }
            JoinOperator::CrossJoin(constraint) => {
                let (cond, using) = self.convert_join_constraint(constraint)?;
                Ok((JoinType::Cross, cond, using))
            }
            JoinOperator::LeftSemi(constraint) => {
                let (cond, using) = self.convert_join_constraint(constraint)?;
                Ok((JoinType::LeftSemi, cond, using))
            }
            JoinOperator::LeftAnti(constraint) => {
                let (cond, using) = self.convert_join_constraint(constraint)?;
                Ok((JoinType::LeftAnti, cond, using))
            }
            other => Err(ThunderduckError::Unsupported(format!("join operator not supported: {:?}", other))),
        }
    }

    fn convert_join_constraint(&self, constraint: JoinConstraint) -> Result<(Option<Expression>, Vec<String>)> {
        match constraint {
            JoinConstraint::On(expr) => Ok((Some(self.convert_expr(expr)?), vec![])),
            JoinConstraint::Using(cols) => {
                // Each col is an ObjectName in sqlparser 0.61
                let names: Vec<String> = cols.iter()
                    .map(|c| self.object_name_to_string(c))
                    .collect();
                Ok((None, names))
            }
            JoinConstraint::Natural => Ok((None, vec![])),
            JoinConstraint::None => Ok((None, vec![])),
        }
    }

    fn select_item_has_aggregate(&self, item: &SelectItem) -> bool {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                self.expr_has_aggregate(e)
            }
            _ => false,
        }
    }

    fn expr_has_aggregate(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Function(f) => {
                // Check if it's an aggregate (no OVER clause)
                f.over.is_none() && self.is_aggregate_function(&f.name.to_string())
            }
            Expr::BinaryOp { left, right, .. } => {
                self.expr_has_aggregate(left) || self.expr_has_aggregate(right)
            }
            Expr::UnaryOp { expr, .. } => self.expr_has_aggregate(expr),
            Expr::Nested(e) => self.expr_has_aggregate(e),
            Expr::Cast { expr, .. } => self.expr_has_aggregate(expr),
            Expr::Case { operand, conditions, else_result, .. } => {
                operand.as_ref().map_or(false, |e| self.expr_has_aggregate(e))
                    || conditions.iter().any(|c| self.expr_has_aggregate(&c.condition) || self.expr_has_aggregate(&c.result))
                    || else_result.as_ref().map_or(false, |e| self.expr_has_aggregate(e))
            }
            _ => false,
        }
    }

    fn is_aggregate_function(&self, name: &str) -> bool {
        matches!(
            name.to_uppercase().as_str(),
            "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "STDDEV" | "STDDEV_POP" | "STDDEV_SAMP"
            | "VARIANCE" | "VAR_POP" | "VAR_SAMP" | "COLLECT_LIST" | "COLLECT_SET"
            | "FIRST" | "LAST" | "FIRST_VALUE" | "LAST_VALUE" | "ANY_VALUE"
            | "APPROX_COUNT_DISTINCT" | "PERCENTILE_APPROX" | "CORR" | "COVAR_POP" | "COVAR_SAMP"
            | "KURTOSIS" | "SKEWNESS" | "REGR_AVGX" | "REGR_AVGY" | "REGR_COUNT"
            | "REGR_INTERCEPT" | "REGR_R2" | "REGR_SLOPE" | "REGR_SXX" | "REGR_SXY" | "REGR_SYY"
            | "BIT_AND" | "BIT_OR" | "BIT_XOR" | "BOOL_AND" | "BOOL_OR"
        )
    }

    fn convert_aggregate_select(
        &self,
        input: LogicalPlan,
        projection: &[SelectItem],
        group_by: GroupByExpr,
        having: Option<Expr>,
    ) -> Result<LogicalPlan> {
        let (grouping, grouping_sets) = self.convert_group_by(group_by)?;

        let mut aggregates: Vec<AggregateExpr> = vec![];
        let mut select_order: Vec<SelectEntry> = vec![];

        for item in projection {
            match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    let alias = if let SelectItem::ExprWithAlias { alias, .. } = item {
                        Some(alias.value.clone())
                    } else {
                        None
                    };

                    if self.is_aggregate_top_level(expr) {
                        let (agg_expr, _agg_alias) = self.extract_aggregate(expr, alias)?;
                        let idx = aggregates.len();
                        aggregates.push(agg_expr);
                        select_order.push(SelectEntry::AggregateExpr(idx));
                    } else if self.expr_has_aggregate(expr) {
                        // Aggregate embedded in expression (e.g., sum(a) * 1.2)
                        let converted = self.convert_expr(expr.clone())?;
                        let aliased = if let Some(al) = alias {
                            Expression::Alias(AliasExpression { expr: Box::new(converted), alias: al })
                        } else {
                            converted
                        };
                        select_order.push(SelectEntry::GroupingExpr(aliased));
                    } else {
                        let converted = self.convert_expr(expr.clone())?;
                        let aliased = if let Some(al) = alias {
                            Expression::Alias(AliasExpression { expr: Box::new(converted), alias: al })
                        } else {
                            converted
                        };
                        select_order.push(SelectEntry::GroupingExpr(aliased));
                    }
                }
                SelectItem::Wildcard(_) => {
                    select_order.push(SelectEntry::GroupingExpr(Expression::Star(StarExpression { qualifier: None })));
                }
                SelectItem::QualifiedWildcard(kind, _) => {
                    let qualifier = match kind {
                        SelectItemQualifiedWildcardKind::ObjectName(n) => self.object_name_to_string(n),
                        SelectItemQualifiedWildcardKind::Expr(e) => e.to_string(),
                    };
                    select_order.push(SelectEntry::GroupingExpr(Expression::Star(StarExpression { qualifier: Some(qualifier) })));
                }
            }
        }

        let having_expr = having.map(|e| self.convert_expr(e)).transpose()?;

        // If GROUP BY is non-empty but no grouping keys appear in the SELECT list, add a sentinel
        // to prevent gen_aggregate from auto-prepending the GROUP BY columns to the output.
        let has_grouping_in_select = select_order.iter().any(|e| matches!(e, SelectEntry::GroupingExpr(_)));
        if !grouping.is_empty() && !has_grouping_in_select {
            select_order.push(SelectEntry::GroupingNotSelected);
        }

        Ok(LogicalPlan::Aggregate(Aggregate {
            input: Box::new(input),
            grouping,
            aggregates,
            having: having_expr,
            grouping_sets,
            select_order,
        }))
    }

    fn is_aggregate_top_level(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Function(f) => f.over.is_none() && self.is_aggregate_function(&f.name.to_string()),
            _ => false,
        }
    }

    fn extract_aggregate(&self, expr: &Expr, outer_alias: Option<String>) -> Result<(AggregateExpr, Option<String>)> {
        match expr {
            Expr::Function(f) => {
                let func_name = self.object_name_to_string(&f.name).to_lowercase();
                let (is_distinct, args) = self.extract_function_args_and_distinct(f)?;
                let func_expr = Expression::FunctionCall(FunctionCall {
                    name: func_name,
                    args,
                    distinct: is_distinct,
                });
                let func_expr = if let Some(al) = &outer_alias {
                    Expression::Alias(AliasExpression { expr: Box::new(func_expr), alias: al.clone() })
                } else {
                    func_expr
                };
                // DISTINCT is already encoded inside FunctionCall.distinct; AggregateExpr.is_distinct
                // would cause render_agg_expr to inject it a second time.
                Ok((AggregateExpr::new(func_expr), outer_alias))
            }
            other => Err(ThunderduckError::Unsupported(format!("expected aggregate function, got: {:?}", other))),
        }
    }

    fn convert_group_by(&self, group_by: GroupByExpr) -> Result<(Vec<Expression>, Option<GroupingSets>)> {
        match group_by {
            GroupByExpr::Expressions(exprs, _modifiers) => {
                let mut plain = vec![];
                let mut sets_opt = None;
                for e in exprs {
                    match e {
                        Expr::Rollup(sets) => {
                            sets_opt = Some(GroupingSets::Rollup(self.convert_grouping_set_list(sets)?));
                        }
                        Expr::Cube(sets) => {
                            sets_opt = Some(GroupingSets::Cube(self.convert_grouping_set_list(sets)?));
                        }
                        Expr::GroupingSets(sets) => {
                            sets_opt = Some(GroupingSets::GroupingSets(self.convert_grouping_set_list(sets)?));
                        }
                        other => plain.push(self.convert_expr(other)?),
                    }
                }
                Ok((plain, sets_opt))
            }
            GroupByExpr::All(_) => Ok((vec![], None)),
        }
    }

    fn convert_grouping_set_list(&self, sets: Vec<Vec<Expr>>) -> Result<Vec<Vec<Expression>>> {
        sets.into_iter()
            .map(|row| row.into_iter().map(|e| self.convert_expr(e)).collect::<Result<Vec<_>>>())
            .collect()
    }

    fn convert_select_item(&self, item: SelectItem) -> Result<Expression> {
        match item {
            SelectItem::UnnamedExpr(expr) => self.convert_expr(expr),
            SelectItem::ExprWithAlias { expr, alias } => {
                let inner = self.convert_expr(expr)?;
                Ok(Expression::Alias(AliasExpression {
                    expr: Box::new(inner),
                    alias: alias.value,
                }))
            }
            SelectItem::Wildcard(_) => Ok(Expression::Star(StarExpression { qualifier: None })),
            SelectItem::QualifiedWildcard(kind, _) => {
                let q = match &kind {
                    SelectItemQualifiedWildcardKind::ObjectName(n) => self.object_name_to_string(n),
                    SelectItemQualifiedWildcardKind::Expr(e) => e.to_string(),
                };
                Ok(Expression::Star(StarExpression { qualifier: Some(q) }))
            }
        }
    }

    pub fn convert_expr(&self, expr: Expr) -> Result<Expression> {
        match expr {
            Expr::Identifier(ident) => Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name: ident.value,
                qualifier: None,
            })),
            Expr::CompoundIdentifier(parts) => {
                // Vec<Ident> in sqlparser 0.61
                let values: Vec<String> = parts.iter().map(|i| i.value.clone()).collect();
                if values.len() == 2 {
                    Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                        name: values[1].clone(),
                        qualifier: Some(values[0].clone()),
                    }))
                } else if values.len() >= 3 {
                    Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                        name: values[values.len() - 1].clone(),
                        qualifier: Some(values[values.len() - 2].clone()),
                    }))
                } else {
                    let name = values.into_iter().last().unwrap_or_default();
                    Ok(Expression::UnresolvedColumn(UnresolvedColumn { name, qualifier: None }))
                }
            }
            Expr::Value(val) => self.convert_value_with_span(val),
            Expr::BinaryOp { left, op, right } => {
                let bop = self.convert_binary_op(op)?;
                Ok(Expression::Binary(BinaryExpression {
                    op: bop,
                    left: Box::new(self.convert_expr(*left)?),
                    right: Box::new(self.convert_expr(*right)?),
                }))
            }
            Expr::UnaryOp { op, expr } => {
                match op {
                    UnaryOperator::Not => Ok(Expression::Unary(UnaryExpression {
                        op: UnaryOp::Not,
                        operand: Box::new(self.convert_expr(*expr)?),
                    })),
                    UnaryOperator::Minus => Ok(Expression::Unary(UnaryExpression {
                        op: UnaryOp::Negate,
                        operand: Box::new(self.convert_expr(*expr)?),
                    })),
                    UnaryOperator::Plus => self.convert_expr(*expr),
                    other => Err(ThunderduckError::Unsupported(format!("unary op not supported: {:?}", other))),
                }
            }
            Expr::Nested(e) => self.convert_expr(*e),
            Expr::Cast { kind, expr, data_type, .. } => {
                let try_cast = matches!(kind, CastKind::TryCast | CastKind::SafeCast);
                Ok(Expression::Cast(CastExpression {
                    expr: Box::new(self.convert_expr(*expr)?),
                    to_type: self.convert_data_type(data_type)?,
                    try_cast,
                }))
            }
            Expr::Function(f) => self.convert_function(f),
            Expr::Case { operand, conditions, else_result, .. } => {
                let base = operand.map(|e| self.convert_expr(*e)).transpose()?.map(Box::new);
                let branches: Result<Vec<(Expression, Expression)>> = conditions
                    .into_iter()
                    .map(|c| Ok((self.convert_expr(c.condition)?, self.convert_expr(c.result)?)))
                    .collect();
                let else_expr = else_result.map(|e| self.convert_expr(*e)).transpose()?.map(Box::new);
                Ok(Expression::CaseWhen(CaseWhenExpression { base, branches: branches?, else_expr }))
            }
            Expr::Between { expr, negated, low, high } => {
                Ok(Expression::Between(BetweenExpression {
                    expr: Box::new(self.convert_expr(*expr)?),
                    low: Box::new(self.convert_expr(*low)?),
                    high: Box::new(self.convert_expr(*high)?),
                    negated,
                }))
            }
            Expr::InList { expr, list, negated } => {
                let converted_list: Result<Vec<Expression>> = list.into_iter().map(|e| self.convert_expr(e)).collect();
                Ok(Expression::InList(InListExpression {
                    expr: Box::new(self.convert_expr(*expr)?),
                    list: converted_list?,
                    negated,
                }))
            }
            Expr::InSubquery { expr, subquery, negated } => {
                let subplan = self.convert_query(*subquery)?;
                Ok(Expression::InSubquery(InSubquery {
                    expr: Box::new(self.convert_expr(*expr)?),
                    subquery: Box::new(subplan),
                    negated,
                }))
            }
            Expr::Exists { subquery, negated } => {
                let subplan = self.convert_query(*subquery)?;
                Ok(Expression::ExistsSubquery(ExistsSubquery {
                    subquery: Box::new(subplan),
                    negated,
                }))
            }
            Expr::Subquery(q) => {
                let subplan = self.convert_query(*q)?;
                Ok(Expression::ScalarSubquery(ScalarSubquery {
                    subquery: Box::new(subplan),
                }))
            }
            Expr::IsNull(e) => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::IsNull,
                operand: Box::new(self.convert_expr(*e)?),
            })),
            Expr::IsNotNull(e) => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::IsNotNull,
                operand: Box::new(self.convert_expr(*e)?),
            })),
            Expr::Like { expr, pattern, negated, .. } => {
                Ok(Expression::Like(LikeExpression {
                    value: Box::new(self.convert_expr(*expr)?),
                    pattern: Box::new(self.convert_expr(*pattern)?),
                    negated,
                    case_insensitive: false,
                }))
            }
            Expr::ILike { expr, pattern, negated, .. } => {
                Ok(Expression::Like(LikeExpression {
                    value: Box::new(self.convert_expr(*expr)?),
                    pattern: Box::new(self.convert_expr(*pattern)?),
                    negated,
                    case_insensitive: true,
                }))
            }
            Expr::Wildcard(_) => Ok(Expression::Star(StarExpression { qualifier: None })),
            Expr::QualifiedWildcard(name, _) => {
                Ok(Expression::Star(StarExpression { qualifier: Some(self.object_name_to_string(&name)) }))
            }

            Expr::Substring { expr, substring_from, substring_for, .. } => {
                let mut args = vec![self.convert_expr(*expr)?];
                if let Some(from) = substring_from {
                    args.push(self.convert_expr(*from)?);
                }
                if let Some(for_len) = substring_for {
                    args.push(self.convert_expr(*for_len)?);
                }
                Ok(Expression::FunctionCall(FunctionCall { name: "substr".to_string(), args, distinct: false }))
            }
            Expr::Interval(interval) => {
                let unit = interval.leading_field
                    .map(|f| format!(" {}", f))
                    .unwrap_or_default();
                Ok(Expression::RawSql(RawSqlExpression {
                    sql: format!("INTERVAL {}{}", interval.value, unit),
                }))
            }
            Expr::Extract { field, expr, .. } => {
                let inner = self.convert_expr(*expr)?;
                Ok(Expression::FunctionCall(FunctionCall {
                    name: "extract".to_string(),
                    args: vec![
                        Expression::RawSql(RawSqlExpression { sql: format!("{}", field) }),
                        inner,
                    ],
                    distinct: false,
                }))
            }
            Expr::IsDistinctFrom(a, b) => {
                Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
                    left: Box::new(self.convert_expr(*a)?),
                    right: Box::new(self.convert_expr(*b)?),
                    negated: false,
                }))
            }
            Expr::IsNotDistinctFrom(a, b) => {
                Ok(Expression::IsDistinctFrom(IsDistinctFromExpression {
                    left: Box::new(self.convert_expr(*a)?),
                    right: Box::new(self.convert_expr(*b)?),
                    negated: true,
                }))
            }
            Expr::Tuple(exprs) => {
                let converted: Result<Vec<Expression>> = exprs.into_iter().map(|e| self.convert_expr(e)).collect();
                Ok(Expression::RowConstructor(RowConstructorExpression { fields: converted? }))
            }
            Expr::TypedString(ts) => {
                // ts.value is a ValueWithSpan; .to_string() includes surrounding quote
                // characters (e.g. "'1996-01-01'"), which would produce triple-quoted
                // literals when wrapped in Literal::string. Use convert_value_with_span
                // instead to correctly extract the bare string value.
                let dt = self.convert_data_type(ts.data_type)?;
                let inner = self.convert_value_with_span(ts.value)?;
                Ok(Expression::Cast(CastExpression {
                    expr: Box::new(inner),
                    to_type: dt,
                    try_cast: false,
                }))
            }
            other => Err(ThunderduckError::Unsupported(format!(
                "expression not yet supported: {}",
                other.to_string().split_whitespace().take(3).collect::<Vec<_>>().join(" ")
            ))),
        }
    }

    fn convert_function(&self, f: Function) -> Result<Expression> {
        let name = self.object_name_to_string(&f.name).to_lowercase();
        let (is_distinct, mut args) = self.extract_function_args_and_distinct(&f)?;

        // Spark outputs COUNT(*) as column "count(1)" (not "count_star()").
        // Replace COUNT(*) with COUNT(1) for consistent column naming.
        if name == "count" && !is_distinct && args.len() == 1
            && matches!(&args[0], Expression::Star(_))
        {
            args = vec![Literal::int(1)];
        }

        // Check if this is a window function (has OVER clause)
        if let Some(over) = f.over {
            let func_expr = Expression::FunctionCall(FunctionCall {
                name,
                args,
                distinct: is_distinct,
            });
            return self.convert_window_from_over(func_expr, over);
        }

        Ok(Expression::FunctionCall(FunctionCall { name, args, distinct: is_distinct }))
    }

    fn convert_window_from_over(&self, func: Expression, over: WindowType) -> Result<Expression> {
        let spec = match over {
            WindowType::WindowSpec(spec) => spec,
            WindowType::NamedWindow(_name) => {
                // Named windows - just return the function without window for now
                return Ok(func);
            }
        };
        self.convert_window(func, spec)
    }

    fn convert_window(&self, func: Expression, window_spec: WindowSpec) -> Result<Expression> {
        let partition_by: Result<Vec<Expression>> = window_spec.partition_by
            .into_iter()
            .map(|e| self.convert_expr(e))
            .collect();
        let order_by = self.convert_order_by_exprs(window_spec.order_by)?;
        let frame = window_spec.window_frame.map(|wf| -> Result<WindowFrame> {
            let unit = match wf.units {
                WindowFrameUnits::Rows => FrameUnit::Rows,
                WindowFrameUnits::Range => FrameUnit::Range,
                WindowFrameUnits::Groups => FrameUnit::Rows,
            };
            let start = self.convert_frame_boundary(wf.start_bound)?;
            let end = if let Some(end) = wf.end_bound {
                self.convert_frame_boundary(end)?
            } else {
                FrameBoundary::CurrentRow
            };
            Ok(WindowFrame { unit, start, end })
        }).transpose()?;

        Ok(Expression::Window(WindowFunction {
            func: Box::new(func),
            partition_by: partition_by?,
            order_by,
            frame,
        }))
    }

    fn convert_frame_boundary(&self, bound: WindowFrameBound) -> Result<FrameBoundary> {
        match bound {
            WindowFrameBound::CurrentRow => Ok(FrameBoundary::CurrentRow),
            WindowFrameBound::Preceding(None) => Ok(FrameBoundary::UnboundedPreceding),
            WindowFrameBound::Preceding(Some(e)) => {
                Ok(FrameBoundary::Preceding(Box::new(self.convert_expr(*e)?)))
            }
            WindowFrameBound::Following(None) => Ok(FrameBoundary::UnboundedFollowing),
            WindowFrameBound::Following(Some(e)) => {
                Ok(FrameBoundary::Following(Box::new(self.convert_expr(*e)?)))
            }
        }
    }

    /// Extract args and distinct flag from a Function node.
    fn extract_function_args_and_distinct(&self, f: &Function) -> Result<(bool, Vec<Expression>)> {
        match &f.args {
            FunctionArguments::None => Ok((false, vec![])),
            FunctionArguments::Subquery(_) => {
                Err(ThunderduckError::Unsupported("subquery in function args not supported".to_string()))
            }
            FunctionArguments::List(arg_list) => {
                let is_distinct = arg_list.duplicate_treatment
                    .as_ref()
                    .map(|d| matches!(d, DuplicateTreatment::Distinct))
                    .unwrap_or(false);
                let args = arg_list.args.iter()
                    .map(|arg| match arg {
                        FunctionArg::Named { arg, .. } => self.convert_function_arg_expr(arg),
                        FunctionArg::Unnamed(arg) => self.convert_function_arg_expr(arg),
                        FunctionArg::ExprNamed { arg, .. } => self.convert_function_arg_expr(arg),
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok((is_distinct, args))
            }
        }
    }

    fn convert_function_arg_expr(&self, arg: &FunctionArgExpr) -> Result<Expression> {
        match arg {
            FunctionArgExpr::Expr(e) => self.convert_expr(e.clone()),
            FunctionArgExpr::Wildcard => Ok(Expression::Star(StarExpression { qualifier: None })),
            FunctionArgExpr::QualifiedWildcard(name) => {
                Ok(Expression::Star(StarExpression { qualifier: Some(self.object_name_to_string(name)) }))
            }
        }
    }

    fn convert_value_with_span(&self, val: ValueWithSpan) -> Result<Expression> {
        self.convert_value(val.value)
    }

    fn convert_value(&self, val: Value) -> Result<Expression> {
        match val {
            Value::Number(s, _) => {
                if let Ok(i) = s.parse::<i64>() {
                    // Prefer INTEGER for values that fit — avoids BIGINT type errors in functions
                    // like ROUND(expr, 2::BIGINT) which DuckDB requires INTEGER for the scale arg.
                    if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                        Ok(Literal::int(i as i32))
                    } else {
                        Ok(Literal::long(i))
                    }
                } else if let Ok(f) = s.parse::<f64>() {
                    Ok(Literal::double(f))
                } else {
                    Ok(Expression::Literal(Literal {
                        value: LiteralValue::Decimal(s),
                        data_type: DataType::Decimal { precision: 38, scale: 18 },
                    }))
                }
            }
            Value::SingleQuotedString(s)
            | Value::DoubleQuotedString(s)
            | Value::EscapedStringLiteral(s)
            | Value::TripleSingleQuotedString(s)
            | Value::TripleDoubleQuotedString(s)
            | Value::UnicodeStringLiteral(s) => Ok(Literal::string(s)),
            Value::Boolean(b) => Ok(Literal::boolean(b)),
            Value::Null => Ok(Literal::null()),
            Value::Placeholder(p) => Err(ThunderduckError::Unsupported(format!("query parameters not supported: {}", p))),
            other => Err(ThunderduckError::Unsupported(format!("literal value not supported: {:?}", other))),
        }
    }

    fn convert_binary_op(&self, op: BinaryOperator) -> Result<BinaryOp> {
        match op {
            BinaryOperator::Plus => Ok(BinaryOp::Add),
            BinaryOperator::Minus => Ok(BinaryOp::Sub),
            BinaryOperator::Multiply => Ok(BinaryOp::Mul),
            BinaryOperator::Divide => Ok(BinaryOp::Div),
            BinaryOperator::Modulo => Ok(BinaryOp::Mod),
            BinaryOperator::Eq => Ok(BinaryOp::Eq),
            BinaryOperator::NotEq => Ok(BinaryOp::NotEq),
            BinaryOperator::Lt => Ok(BinaryOp::Lt),
            BinaryOperator::LtEq => Ok(BinaryOp::LtEq),
            BinaryOperator::Gt => Ok(BinaryOp::Gt),
            BinaryOperator::GtEq => Ok(BinaryOp::GtEq),
            BinaryOperator::And => Ok(BinaryOp::And),
            BinaryOperator::Or => Ok(BinaryOp::Or),
            BinaryOperator::StringConcat => Ok(BinaryOp::Concat),
            BinaryOperator::BitwiseAnd => Ok(BinaryOp::BitwiseAnd),
            BinaryOperator::BitwiseOr => Ok(BinaryOp::BitwiseOr),
            BinaryOperator::BitwiseXor => Ok(BinaryOp::BitwiseXor),
            other => Err(ThunderduckError::Unsupported(format!("binary op not supported: {:?}", other))),
        }
    }

    fn convert_data_type(&self, dt: SqlDataType) -> Result<DataType> {
        match dt {
            SqlDataType::Boolean | SqlDataType::Bool => Ok(DataType::Boolean),
            SqlDataType::TinyInt(_) => Ok(DataType::Byte),
            SqlDataType::SmallInt(_) | SqlDataType::Int2(_) => Ok(DataType::Short),
            SqlDataType::Int(_) | SqlDataType::Integer(_) | SqlDataType::Int4(_) => Ok(DataType::Integer),
            SqlDataType::BigInt(_) | SqlDataType::Int8(_) => Ok(DataType::Long),
            SqlDataType::Float(_) | SqlDataType::Real => Ok(DataType::Float),
            SqlDataType::Double(_) | SqlDataType::Float8 | SqlDataType::DoublePrecision => Ok(DataType::Double),
            SqlDataType::Decimal(info) | SqlDataType::Numeric(info) => {
                match info {
                    ExactNumberInfo::PrecisionAndScale(p, s) => Ok(DataType::Decimal { precision: p as u8, scale: s as u8 }),
                    ExactNumberInfo::Precision(p) => Ok(DataType::Decimal { precision: p as u8, scale: 0 }),
                    ExactNumberInfo::None => Ok(DataType::Decimal { precision: 38, scale: 18 }),
                }
            }
            SqlDataType::Varchar(_) | SqlDataType::Char(_) | SqlDataType::Text | SqlDataType::String(_) => Ok(DataType::String),
            SqlDataType::Binary(_) | SqlDataType::Varbinary(_) | SqlDataType::Blob(_) | SqlDataType::Bytea => Ok(DataType::Binary),
            SqlDataType::Date => Ok(DataType::Date),
            SqlDataType::Timestamp(_, _) | SqlDataType::TimestampNtz(_) => Ok(DataType::Timestamp),
            SqlDataType::Array(elem) => {
                match elem {
                    ArrayElemTypeDef::AngleBracket(inner) | ArrayElemTypeDef::SquareBracket(inner, _) | ArrayElemTypeDef::Parenthesis(inner) => {
                        let inner_type = self.convert_data_type(*inner)?;
                        Ok(DataType::Array(Box::new(inner_type)))
                    }
                    ArrayElemTypeDef::None => Ok(DataType::Array(Box::new(DataType::Unresolved))),
                }
            }
            other => Err(ThunderduckError::Unsupported(format!("data type not supported: {:?}", other))),
        }
    }

    fn convert_order_by_exprs(&self, items: Vec<OrderByExpr>) -> Result<Vec<SortOrder>> {
        items.into_iter().map(|item| {
            let expr = self.convert_expr(item.expr)?;
            let direction = if item.options.asc.unwrap_or(true) {
                SortDirection::Asc
            } else {
                SortDirection::Desc
            };
            let null_ordering = match item.options.nulls_first {
                Some(true) => NullOrdering::NullsFirst,
                Some(false) => NullOrdering::NullsLast,
                None => if matches!(direction, SortDirection::Asc) { NullOrdering::NullsFirst } else { NullOrdering::NullsLast },
            };
            Ok(SortOrder { expr, direction, null_ordering })
        }).collect()
    }

    fn wrap_with_sort_limit(
        &self,
        plan: LogicalPlan,
        order_by: Vec<OrderByExpr>,
        limit: Option<Expr>,
        offset: Option<Expr>,
    ) -> Result<LogicalPlan> {
        if order_by.is_empty() && limit.is_none() && offset.is_none() {
            return Ok(plan);
        }
        let sort_exprs = self.convert_order_by_exprs(order_by)?;
        let limit_expr = limit.map(|e| self.convert_expr(e)).transpose()?;
        let offset_expr = offset.map(|e| self.convert_expr(e)).transpose()?;

        Ok(LogicalPlan::Sort(Sort {
            input: Box::new(plan),
            order: sort_exprs,
            limit: limit_expr,
            offset: offset_expr,
        }))
    }

    fn object_name_to_string(&self, name: &ObjectName) -> String {
        name.0.iter()
            .map(|part| match part {
                ObjectNamePart::Identifier(i) => i.value.clone(),
                ObjectNamePart::Function(f) => f.to_string(),
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SparkSqlParser;

    #[test]
    fn test_parse_simple_select() {
        let plan = SparkSqlParser::parse("SELECT a, b FROM t WHERE a > 1").unwrap();
        // The logical plan is Project(input: Filter(input: TableScan))
        match plan {
            LogicalPlan::Project(p) => {
                assert_eq!(p.projections.len(), 2);
                match *p.input {
                    LogicalPlan::Filter(f) => {
                        match *f.input {
                            LogicalPlan::TableScan(ts) => assert_eq!(ts.table, "t"),
                            other => panic!("expected TableScan, got {:?}", other),
                        }
                    }
                    other => panic!("expected Filter, got {:?}", other),
                }
            }
            other => panic!("expected Project, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_cte() {
        let sql = "WITH cte AS (SELECT 1 AS x) SELECT x FROM cte";
        let plan = SparkSqlParser::parse(sql).unwrap();
        match plan {
            LogicalPlan::WithCte(w) => {
                assert_eq!(w.ctes.len(), 1);
                assert_eq!(w.ctes[0].0, "cte");
            }
            other => panic!("expected WithCte, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_union() {
        let sql = "SELECT 1 AS x UNION ALL SELECT 2 AS x";
        let plan = SparkSqlParser::parse(sql).unwrap();
        match plan {
            LogicalPlan::Union(u) => {
                assert!(u.all);
            }
            other => panic!("expected Union, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_aggregate() {
        let sql = "SELECT dept, COUNT(*) AS cnt FROM employees GROUP BY dept";
        let plan = SparkSqlParser::parse(sql).unwrap();
        match plan {
            LogicalPlan::Aggregate(a) => {
                assert_eq!(a.grouping.len(), 1);
                assert_eq!(a.aggregates.len(), 1);
            }
            other => panic!("expected Aggregate, got {:?}", other),
        }
    }
}
