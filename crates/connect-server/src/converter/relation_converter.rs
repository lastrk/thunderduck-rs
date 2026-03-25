use std::sync::Arc;

use thunderduck_core::expression::{Expression, Literal, LiteralValue, SortOrder, UnresolvedColumn};
use thunderduck_core::generator::SqlGenerator;
use thunderduck_core::logical::{
    Aggregate, AggregateExpr, AliasedRelation, Distinct, DropColumns, Except, Filter,
    GroupingSets, Intersect, Join, JoinType, Limit, LocalDataRelation, LogicalPlan, NADrop,
    NADropHow, NAFill, NAReplace, Pivot, Project, RangeRelation, Sample, SelectEntry, ShowString,
    SingleRowRelation, Sort, SqlRelation, TableScan, Tail, ToDataFrame, Union, Unpivot,
    WithColumns,
};
use thunderduck_core::runtime::{DuckDbSession, SchemaInferrer};
use thunderduck_core::types::{DataType, StructField, StructType};

use crate::converter::expression_converter::ExpressionConverter;
use crate::converter::type_converter::parse_type_str;
use crate::error::{ConnectError, Result};
use crate::proto::spark::connect as proto;

/// Converts proto Relation messages to the core LogicalPlan AST.
pub struct RelationConverter<'a> {
    expr_conv: &'a mut ExpressionConverter,
    /// Optional session for schema inference (used by NADrop/NAFill/NAReplace/Unpivot).
    session: Option<Arc<DuckDbSession>>,
}

impl<'a> RelationConverter<'a> {
    pub fn new(expr_conv: &'a mut ExpressionConverter) -> Self {
        Self { expr_conv, session: None }
    }

    pub fn with_session(expr_conv: &'a mut ExpressionConverter, session: Arc<DuckDbSession>) -> Self {
        Self { expr_conv, session: Some(session) }
    }

    pub fn convert(&mut self, relation: &proto::Relation) -> Result<LogicalPlan> {
        use proto::relation::RelType;
        match &relation.rel_type {
            None => Err(ConnectError::PlanConversion("empty relation".into())),
            Some(RelType::Project(p)) => self.convert_project(p),
            Some(RelType::Filter(f)) => self.convert_filter(f),
            Some(RelType::Aggregate(a)) => self.convert_aggregate(a),
            Some(RelType::Sort(s)) => self.convert_sort(s),
            Some(RelType::Limit(l)) => self.convert_limit(l),
            Some(RelType::Offset(o)) => self.convert_offset(o),
            Some(RelType::Tail(t)) => self.convert_tail(t),
            Some(RelType::Join(j)) => self.convert_join(j),
            Some(RelType::SetOp(s)) => self.convert_set_op(s),
            Some(RelType::Read(r)) => self.convert_read(r),
            Some(RelType::LocalRelation(lr)) => self.convert_local_relation(lr),
            Some(RelType::Range(r)) => self.convert_range(r),
            Some(RelType::Sql(s)) => self.convert_sql(s),
            Some(RelType::SubqueryAlias(sa)) => self.convert_subquery_alias(sa),
            Some(RelType::WithColumns(wc)) => self.convert_with_columns(wc),
            Some(RelType::WithColumnsRenamed(wcr)) => self.convert_with_columns_renamed(wcr),
            Some(RelType::Deduplicate(d)) => self.convert_deduplicate(d),
            Some(RelType::Sample(s)) => self.convert_sample(s),
            Some(RelType::Drop(d)) => self.convert_drop(d),
            Some(RelType::ToDf(t)) => self.convert_to_df(t),
            Some(RelType::Hint(h)) => {
                let input = h
                    .input
                    .as_ref()
                    .ok_or_else(|| ConnectError::PlanConversion("Hint missing input".into()))?;
                self.convert(input)
            }
            Some(RelType::Repartition(r)) => {
                let input = r
                    .input
                    .as_ref()
                    .ok_or_else(|| ConnectError::PlanConversion("Repartition missing input".into()))?;
                self.convert(input)
            }
            Some(RelType::RepartitionByExpression(r)) => {
                let input = r
                    .input
                    .as_ref()
                    .ok_or_else(|| {
                        ConnectError::PlanConversion("RepartitionByExpression missing input".into())
                    })?;
                self.convert(input)
            }
            Some(RelType::ShowString(ss)) => self.convert_show_string(ss),
            Some(RelType::FillNa(f)) => self.convert_fill_na(f),
            Some(RelType::DropNa(d)) => self.convert_drop_na(d),
            Some(RelType::Replace(r)) => self.convert_replace(r),
            Some(RelType::Summary(s)) => self.convert_summary(s),
            Some(RelType::Describe(d)) => self.convert_describe(d),
            Some(RelType::WithRelations(wr)) => self.convert_with_relations(wr),
            Some(RelType::Cov(c)) => self.convert_stat_cov(c),
            Some(RelType::Corr(c)) => self.convert_stat_corr(c),
            Some(RelType::ApproxQuantile(aq)) => self.convert_approx_quantile(aq),
            Some(RelType::Crosstab(c)) => self.convert_stat_crosstab(c),
            Some(RelType::FreqItems(f)) => self.convert_stat_freq_items(f),
            Some(RelType::SampleBy(s)) => self.convert_stat_sample_by(s),
            Some(RelType::Unpivot(u)) => self.convert_unpivot(u),
            Some(RelType::ToSchema(ts)) => self.convert_to_schema(ts),
            Some(RelType::Catalog(cat)) => self.convert_catalog(cat),
            _ => Err(ConnectError::Unsupported(format!(
                "Unsupported relation type: {:?}",
                std::mem::discriminant(relation.rel_type.as_ref().unwrap())
            ))),
        }
    }

    // ── Relation converters ────────────────────────────────────────────────────

    fn convert_project(&mut self, p: &proto::Project) -> Result<LogicalPlan> {
        let input = if let Some(inp) = &p.input {
            self.convert(inp)?
        } else {
            LogicalPlan::SingleRow(SingleRowRelation)
        };
        let projections: Result<Vec<Expression>> =
            p.expressions.iter().map(|e| self.expr_conv.convert(e)).collect();
        let mut projections = projections?;

        // Expand explode(map_col) → UNNEST(map_keys) AS key + UNNEST(map_values) AS value
        let input_schema = input.infer_schema();
        expand_map_explodes(&input_schema, &mut projections);

        // Populate struct_fields for dropFields (UpdateFields { value: None })
        // Try fast path first; fall back to DuckDB schema inference for TableScan etc.
        if needs_drop_fields_inference(&projections) {
            let schema = if input_schema.is_empty() {
                self.infer_full_schema(&input)?
            } else {
                input_schema.clone()
            };
            populate_drop_fields_schema(&schema, &mut projections);
        } else {
            populate_drop_fields_schema(&input_schema, &mut projections);
        }

        // If input is a join with plan_id aliases, qualify column references in projections
        // so the outer SELECT can reference left/right subquery aliases unambiguously.
        qualify_exprs_for_join(&input, &mut projections);

        Ok(LogicalPlan::Project(thunderduck_core::logical::Project {
            input: Box::new(input),
            projections,
        }))
    }

    fn convert_filter(&mut self, f: &proto::Filter) -> Result<LogicalPlan> {
        let input = f
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Filter missing input".into()))?;
        let condition = f
            .condition
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Filter missing condition".into()))?;
        let input_plan = self.convert(input)?;
        let mut exprs = vec![self.expr_conv.convert(condition)?];
        qualify_exprs_for_join(&input_plan, &mut exprs);
        Ok(LogicalPlan::Filter(Filter {
            input: Box::new(input_plan),
            condition: exprs.remove(0),
        }))
    }

    fn convert_aggregate(&mut self, a: &proto::Aggregate) -> Result<LogicalPlan> {
        use proto::aggregate::GroupType;

        let input = a
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Aggregate missing input".into()))?;
        let input_plan = self.convert(input)?;

        let grouping: Result<Vec<Expression>> =
            a.grouping_expressions.iter().map(|e| self.expr_conv.convert(e)).collect();
        let grouping = grouping?;

        let mut aggregates: Vec<AggregateExpr> = Vec::new();
        let mut select_order: Vec<SelectEntry> = Vec::new();

        for agg_expr in &a.aggregate_expressions {
            let expr = self.expr_conv.convert(agg_expr)?;
            if let Some(grp_idx) = grouping.iter().position(|g| g == &expr) {
                select_order.push(SelectEntry::GroupingExpr(grouping[grp_idx].clone()));
            } else {
                let agg_idx = aggregates.len();
                aggregates.push(AggregateExpr::new(expr));
                select_order.push(SelectEntry::AggregateExpr(agg_idx));
            }
        }

        // Handle PIVOT separately — it becomes a Pivot plan node, not an Aggregate.
        if a.group_type() == GroupType::Pivot {
            let pivot_proto = a.pivot.as_ref()
                .ok_or_else(|| ConnectError::PlanConversion("Pivot missing pivot field".into()))?;
            let pivot_col = self.expr_conv.convert(
                pivot_proto.col.as_ref()
                    .ok_or_else(|| ConnectError::PlanConversion("Pivot missing col".into()))?
            )?;
            let pivot_values: Result<Vec<Expression>> = pivot_proto.values.iter()
                .map(|lit| self.expr_conv.convert_literal(lit))
                .collect();
            return Ok(LogicalPlan::Pivot(Pivot {
                input: Box::new(input_plan),
                grouping,
                pivot_col,
                pivot_values: pivot_values?,
                aggregates,
            }));
        }

        let singleton_sets: Vec<Vec<Expression>> =
            grouping.iter().map(|e| vec![e.clone()]).collect();
        let grouping_sets = match a.group_type() {
            GroupType::Rollup => Some(GroupingSets::Rollup(singleton_sets)),
            GroupType::Cube => Some(GroupingSets::Cube(singleton_sets)),
            GroupType::GroupingSets => {
                let sets: Result<Vec<Vec<Expression>>> = a
                    .grouping_sets
                    .iter()
                    .map(|gs| {
                        gs.grouping_set
                            .iter()
                            .map(|e| self.expr_conv.convert(e))
                            .collect()
                    })
                    .collect();
                Some(GroupingSets::GroupingSets(sets?))
            }
            _ => None,
        };

        Ok(LogicalPlan::Aggregate(Aggregate {
            input: Box::new(input_plan),
            grouping,
            aggregates,
            having: None,
            grouping_sets,
            select_order,
        }))
    }

    fn convert_sort(&mut self, s: &proto::Sort) -> Result<LogicalPlan> {
        let input = s
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Sort missing input".into()))?;
        let order: Result<Vec<SortOrder>> =
            s.order.iter().map(|so| self.expr_conv.convert_sort_order(so)).collect();
        Ok(LogicalPlan::Sort(Sort {
            input: Box::new(self.convert(input)?),
            order: order?,
            limit: None,
            offset: None,
        }))
    }

    fn convert_limit(&mut self, l: &proto::Limit) -> Result<LogicalPlan> {
        let input = l
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Limit missing input".into()))?;
        Ok(LogicalPlan::Limit(Limit {
            input: Box::new(self.convert(input)?),
            limit: Literal::int(l.limit),
        }))
    }

    fn convert_offset(&mut self, o: &proto::Offset) -> Result<LogicalPlan> {
        let input = o
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Offset missing input".into()))?;
        Ok(LogicalPlan::Sort(Sort {
            input: Box::new(self.convert(input)?),
            order: vec![],
            limit: None,
            offset: Some(Literal::int(o.offset)),
        }))
    }

    fn convert_tail(&mut self, t: &proto::Tail) -> Result<LogicalPlan> {
        let input = t
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Tail missing input".into()))?;
        Ok(LogicalPlan::Tail(Tail {
            input: Box::new(self.convert(input)?),
            limit: Literal::int(t.limit),
        }))
    }

    fn convert_join(&mut self, j: &proto::Join) -> Result<LogicalPlan> {
        use proto::join::JoinType as ProtoJoinType;
        let left_proto = j
            .left
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Join missing left".into()))?;
        let right_proto = j
            .right
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Join missing right".into()))?;

        let join_type = match j.join_type() {
            ProtoJoinType::Inner | ProtoJoinType::Unspecified => JoinType::Inner,
            ProtoJoinType::FullOuter => JoinType::Full,
            ProtoJoinType::LeftOuter => JoinType::Left,
            ProtoJoinType::RightOuter => JoinType::Right,
            ProtoJoinType::LeftAnti => JoinType::LeftAnti,
            ProtoJoinType::LeftSemi => JoinType::LeftSemi,
            ProtoJoinType::Cross => JoinType::Cross,
        };

        // Collect plan_ids from left and right proto trees. If any column reference in the join
        // condition carries a plan_id that matches a side, we wrap that side in a named subquery
        // (alias = "__plan_id_{outermost_id}__") so DuckDB can resolve ambiguous column names.
        let mut left_ids_set = std::collections::HashSet::<i64>::new();
        let mut right_ids_set = std::collections::HashSet::<i64>::new();
        collect_relation_plan_ids(left_proto, &mut left_ids_set);
        collect_relation_plan_ids(right_proto, &mut right_ids_set);

        let left_outer_id = left_proto.common.as_ref().and_then(|c| c.plan_id);
        let right_outer_id = right_proto.common.as_ref().and_then(|c| c.plan_id);

        let raw_condition = if let Some(cond) = &j.join_condition {
            Some(self.expr_conv.convert(cond)?)
        } else {
            None
        };

        // Determine whether any column in the condition is plan_id-qualified.
        let needs_aliases = raw_condition.as_ref()
            .map(|c| condition_has_plan_id(c))
            .unwrap_or(false)
            && left_outer_id.is_some()
            && right_outer_id.is_some();

        let (condition, left_alias, right_alias) = if needs_aliases {
            // Use a distinct alias format (__td_jl_N__ / __td_jr_M__) so the generator can
            // tell these apart from raw plan_id qualifiers (__plan_id_X__) and not strip them.
            let la = format!("__td_jl_{}__", left_outer_id.unwrap());
            let ra = format!("__td_jr_{}__", right_outer_id.unwrap());
            let qualified = raw_condition.map(|c| qualify_join_condition(c, &left_ids_set, &right_ids_set, &la, &ra));
            (qualified, Some(la), Some(ra))
        } else {
            (raw_condition, None, None)
        };

        let left_plan_ids: Vec<i64> = left_ids_set.into_iter().collect();
        let right_plan_ids: Vec<i64> = right_ids_set.into_iter().collect();

        Ok(LogicalPlan::Join(Join {
            left: Box::new(self.convert(left_proto)?),
            right: Box::new(self.convert(right_proto)?),
            join_type,
            condition,
            using_columns: j.using_columns.clone(),
            left_alias,
            right_alias,
            left_plan_ids,
            right_plan_ids,
        }))
    }

    fn convert_set_op(&mut self, s: &proto::SetOperation) -> Result<LogicalPlan> {
        use proto::set_operation::SetOpType;
        let left = s
            .left_input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("SetOp missing left".into()))?;
        let right = s
            .right_input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("SetOp missing right".into()))?;
        let all = s.is_all.unwrap_or(false);
        let left_plan = Box::new(self.convert(left)?);
        let right_plan = Box::new(self.convert(right)?);

        // Handle unionByName: reorder right side columns to match left side order.
        let right_plan = if s.by_name.unwrap_or(false) && s.set_op_type() == SetOpType::Union {
            let left_schema = left_plan.infer_schema();
            let right_schema = right_plan.infer_schema();
            if !left_schema.is_empty() && !right_schema.is_empty() {
                // Build a projection that reorders right columns to match left order.
                let allow_missing = s.allow_missing_columns.unwrap_or(false);
                let mut projections: Vec<Expression> = Vec::with_capacity(left_schema.fields.len());
                for lf in &left_schema.fields {
                    if right_schema.field_index(&lf.name).is_some() {
                        projections.push(Expression::UnresolvedColumn(UnresolvedColumn {
                            name: lf.name.clone(),
                            qualifier: None,
                        }));
                    } else if allow_missing {
                        // Missing column in right — fill with NULL
                        use thunderduck_core::expression::Literal;
                        projections.push(Literal::null());
                    } else {
                        return Err(ConnectError::PlanConversion(format!(
                            "unionByName: column '{}' missing from right side and allow_missing_columns is false",
                            lf.name
                        )));
                    }
                }
                Box::new(LogicalPlan::Project(thunderduck_core::logical::Project {
                    input: right_plan,
                    projections,
                }))
            } else {
                right_plan
            }
        } else {
            right_plan
        };

        match s.set_op_type() {
            SetOpType::Union => Ok(LogicalPlan::Union(Union {
                left: left_plan,
                right: right_plan,
                all,
            })),
            SetOpType::Except => Ok(LogicalPlan::Except(Except {
                left: left_plan,
                right: right_plan,
                all,
            })),
            SetOpType::Intersect => Ok(LogicalPlan::Intersect(Intersect {
                left: left_plan,
                right: right_plan,
                all,
            })),
            SetOpType::Unspecified => {
                Err(ConnectError::PlanConversion("SetOp type unspecified".into()))
            }
        }
    }

    fn convert_read(&mut self, r: &proto::Read) -> Result<LogicalPlan> {
        use proto::read::ReadType;
        match &r.read_type {
            Some(ReadType::NamedTable(nt)) => Ok(LogicalPlan::TableScan(TableScan {
                table: nt.unparsed_identifier.clone(),
                alias: None,
            })),
            Some(ReadType::DataSource(ds)) => {
                if ds.paths.is_empty() {
                    return Err(ConnectError::PlanConversion(
                        "DataSource has no paths".into(),
                    ));
                }
                let format = ds.format.as_deref().unwrap_or("").to_lowercase();
                let first_path = &ds.paths[0];
                // Infer DuckDB reader from explicit format or path extension.
                let reader = match format.as_str() {
                    "parquet" => "read_parquet",
                    "csv" | "text" => "read_csv_auto",
                    "json" => "read_json_auto",
                    "orc" => "read_orc",
                    _ => {
                        let lower = first_path.to_lowercase();
                        if lower.ends_with(".parquet") { "read_parquet" }
                        else if lower.ends_with(".csv") || lower.ends_with(".tsv") { "read_csv_auto" }
                        else if lower.ends_with(".json") || lower.ends_with(".jsonl") || lower.ends_with(".ndjson") { "read_json_auto" }
                        else { "read_parquet" }
                    }
                };
                let paths_sql = if ds.paths.len() == 1 {
                    format!("'{}'", first_path.replace('\'', "''"))
                } else {
                    let quoted: Vec<String> = ds.paths.iter()
                        .map(|p| format!("'{}'", p.replace('\'', "''")))
                        .collect();
                    format!("[{}]", quoted.join(", "))
                };
                let sql = format!("SELECT * FROM {reader}({paths_sql})");
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            None => Err(ConnectError::PlanConversion("Read missing read_type".into())),
        }
    }

    fn convert_local_relation(&self, lr: &proto::LocalRelation) -> Result<LogicalPlan> {
        if let Some(data) = &lr.data {
            if !data.is_empty() {
                // Try to materialise the Arrow IPC rows as a VALUES SQL expression.
                // Falls back to schema-only (0 rows) if anything goes wrong.
                if let Ok(sql) = local_relation_to_values_sql(data) {
                    let schema = parse_arrow_schema(data).unwrap_or_default();
                    return Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema }));
                }
            }
        }
        // Fallback: schema-only (0 rows).
        // Priority: Arrow IPC schema > DDL schema string > empty.
        let schema_from_ddl = || lr.schema.as_deref().and_then(|s| parse_ddl_schema(s).ok()).unwrap_or_default();
        let schema = match &lr.data {
            Some(data) if !data.is_empty() => parse_arrow_schema(data).unwrap_or_default(),
            _ => schema_from_ddl(),
        };
        Ok(LogicalPlan::LocalDataRelation(LocalDataRelation { schema }))
    }

    fn convert_range(&self, r: &proto::Range) -> Result<LogicalPlan> {
        Ok(LogicalPlan::RangeRelation(RangeRelation {
            start: r.start.unwrap_or(0),
            end: r.end,
            step: r.step,
            num_partitions: r.num_partitions,
        }))
    }

    fn convert_sql(&self, s: &proto::Sql) -> Result<LogicalPlan> {
        use thunderduck_core::parser::SparkSqlParser;
        SparkSqlParser::parse(&s.query).map_err(ConnectError::from)
    }

    fn convert_subquery_alias(&mut self, sa: &proto::SubqueryAlias) -> Result<LogicalPlan> {
        let input = sa
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("SubqueryAlias missing input".into()))?;
        Ok(LogicalPlan::AliasedRelation(AliasedRelation {
            input: Box::new(self.convert(input)?),
            alias: sa.alias.clone(),
            column_aliases: sa.qualifier.clone(),
        }))
    }

    fn convert_with_columns(&mut self, wc: &proto::WithColumns) -> Result<LogicalPlan> {
        let input = wc
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("WithColumns missing input".into()))?;
        let input_plan = self.convert(input)?;

        let mut columns: Vec<(String, Expression)> = Vec::new();
        for alias in &wc.aliases {
            let expr = alias
                .expr
                .as_ref()
                .ok_or_else(|| {
                    ConnectError::PlanConversion("WithColumns alias missing expr".into())
                })?;
            let name =
                alias.name.first().cloned().unwrap_or_else(|| "_col".to_string());
            let col_expr = self.expr_conv.convert(expr)?;
            columns.push((name, col_expr));
        }

        // Try to expand to an explicit Project to preserve column order.
        // When we know the input column list, we can place replacement columns
        // in-place and append new columns at the end — matching Spark's behavior.
        if let Ok(input_cols) = self.infer_columns(&input_plan) {
            use thunderduck_core::expression::AliasExpression;
            let mut projections: Vec<Expression> = input_cols.iter().map(|col_name| {
                // If this column is being replaced, use the replacement expression.
                let replacement = columns.iter().find(|(n, _)| n == col_name).map(|(_, e)| e.clone());
                let expr = replacement.unwrap_or_else(|| {
                    Expression::UnresolvedColumn(UnresolvedColumn { name: col_name.clone(), qualifier: None })
                });
                Expression::Alias(AliasExpression { expr: Box::new(expr), alias: col_name.clone() })
            }).collect();
            // Append new columns (those not in input_cols).
            for (name, expr) in &columns {
                if !input_cols.iter().any(|c| c == name) {
                    projections.push(Expression::Alias(AliasExpression {
                        expr: Box::new(expr.clone()),
                        alias: name.clone(),
                    }));
                }
            }
            return Ok(LogicalPlan::Project(Project {
                input: Box::new(input_plan),
                projections,
            }));
        }

        Ok(LogicalPlan::WithColumns(WithColumns {
            input: Box::new(input_plan),
            columns,
        }))
    }

    fn convert_with_columns_renamed(
        &mut self,
        wcr: &proto::WithColumnsRenamed,
    ) -> Result<LogicalPlan> {
        let input = wcr
            .input
            .as_ref()
            .ok_or_else(|| {
                ConnectError::PlanConversion("WithColumnsRenamed missing input".into())
            })?;
        let input_plan = self.convert(input)?;

        let mut columns: Vec<(String, Expression)> = wcr
            .renames
            .iter()
            .map(|r| {
                let col = Expression::UnresolvedColumn(UnresolvedColumn {
                    name: r.col_name.clone(),
                    qualifier: None,
                });
                (r.new_col_name.clone(), col)
            })
            .collect();

        // Also handle deprecated rename_columns_map
        for (old, new) in &wcr.rename_columns_map {
            let col = Expression::UnresolvedColumn(UnresolvedColumn {
                name: old.clone(),
                qualifier: None,
            });
            columns.push((new.clone(), col));
        }

        Ok(LogicalPlan::WithColumns(WithColumns {
            input: Box::new(input_plan),
            columns,
        }))
    }

    fn convert_deduplicate(&mut self, d: &proto::Deduplicate) -> Result<LogicalPlan> {
        let input = d
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Deduplicate missing input".into()))?;
        let input_plan = self.convert(input)?;
        // Convert column subset for dropDuplicates(cols); empty = all columns (SELECT DISTINCT *)
        let columns: Result<Vec<Expression>> = d.column_names.iter()
            .map(|name| Ok(Expression::UnresolvedColumn(UnresolvedColumn { name: name.clone(), qualifier: None })))
            .collect();
        Ok(LogicalPlan::Distinct(Distinct { input: Box::new(input_plan), columns: columns? }))
    }

    fn convert_sample(&mut self, s: &proto::Sample) -> Result<LogicalPlan> {
        let input = s
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Sample missing input".into()))?;
        let fraction = s.upper_bound - s.lower_bound;
        Ok(LogicalPlan::Sample(Sample {
            input: Box::new(self.convert(input)?),
            fraction,
            seed: s.seed,
            with_replacement: s.with_replacement.unwrap_or(false),
        }))
    }

    fn convert_drop(&mut self, d: &proto::Drop) -> Result<LogicalPlan> {
        let input = d
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Drop missing input".into()))?;
        let mut col_names: Vec<String> = d.column_names.clone();
        for col_expr in &d.columns {
            if let Some(proto::expression::ExprType::UnresolvedAttribute(attr)) =
                &col_expr.expr_type
            {
                col_names.push(attr.unparsed_identifier.clone());
            }
        }
        Ok(LogicalPlan::DropColumns(DropColumns {
            input: Box::new(self.convert(input)?),
            column_names: col_names,
        }))
    }

    fn convert_to_df(&mut self, t: &proto::ToDf) -> Result<LogicalPlan> {
        let input = t
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("ToDF missing input".into()))?;
        let input_plan = self.convert(input)?;

        // If the input schema is unknown (e.g. SqlRelation from read.parquet),
        // resolve column names via DuckDB so the generator can produce the
        // explicit "old AS new" rename SELECT instead of a pass-through SELECT *.
        if input_plan.infer_schema().is_empty() && !t.column_names.is_empty() {
            if let Ok(original_cols) = self.infer_columns(&input_plan) {
                if original_cols.len() == t.column_names.len() {
                    // Convert to a Project with explicit renames
                    let projections = original_cols
                        .into_iter()
                        .zip(t.column_names.iter())
                        .map(|(old, new)| {
                            thunderduck_core::expression::Expression::Alias(
                                thunderduck_core::expression::AliasExpression {
                                    expr: Box::new(thunderduck_core::expression::Expression::UnresolvedColumn(
                                        thunderduck_core::expression::UnresolvedColumn {
                                            name: old,
                                            qualifier: None,
                                        },
                                    )),
                                    alias: new.clone(),
                                },
                            )
                        })
                        .collect();
                    return Ok(LogicalPlan::Project(
                        thunderduck_core::logical::Project {
                            input: Box::new(input_plan),
                            projections,
                        },
                    ));
                }
            }
        }

        Ok(LogicalPlan::ToDataFrame(ToDataFrame {
            input: Box::new(input_plan),
            column_names: t.column_names.clone(),
        }))
    }

    fn convert_show_string(&mut self, ss: &proto::ShowString) -> Result<LogicalPlan> {
        let input = ss
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("ShowString missing input".into()))?;
        Ok(LogicalPlan::ShowString(ShowString {
            input: Box::new(self.convert(input)?),
            num_rows: ss.num_rows,
            truncate: ss.truncate,
            vertical: ss.vertical,
        }))
    }

    fn convert_drop_na(&mut self, d: &proto::NaDrop) -> Result<LogicalPlan> {
        let input = d
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("NADrop missing input".into()))?;
        let input_plan = self.convert(input)?;

        // Proto encoding: 'any' → min_non_nulls unset; 'all' → min_non_nulls=1; custom thresh → min_non_nulls=N
        let (how, threshold) = if let Some(min) = d.min_non_nulls {
            (NADropHow::All, Some(min)) // keep rows with >= min non-null values
        } else {
            (NADropHow::Any, None) // drop row if ANY column is null
        };

        let cols = if d.cols.is_empty() {
            self.infer_columns(&input_plan)?
        } else {
            d.cols.clone()
        };

        Ok(LogicalPlan::NADrop(NADrop { input: Box::new(input_plan), how, threshold, cols }))
    }

    fn convert_fill_na(&mut self, f: &proto::NaFill) -> Result<LogicalPlan> {
        let input = f
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("NAFill missing input".into()))?;
        let input_plan = self.convert(input)?;

        // Convert proto literals to core Literals
        let fill_literals: Result<Vec<Literal>> =
            f.values.iter().map(|v| proto_literal_to_core(v)).collect();
        let fill_literals = fill_literals?;

        let all_columns = self.infer_columns(&input_plan)?;

        let values: Vec<(String, Literal)> = if f.cols.is_empty() {
            // Apply single fill value to all type-compatible columns
            if let Some(lit) = fill_literals.first() {
                all_columns.iter().map(|c| (c.clone(), lit.clone())).collect()
            } else {
                vec![]
            }
        } else if fill_literals.len() == 1 {
            // Apply single fill value to specified columns
            f.cols.iter().map(|c| (c.clone(), fill_literals[0].clone())).collect()
        } else {
            // Each column paired with its own value
            f.cols.iter().zip(fill_literals.iter()).map(|(c, l)| (c.clone(), l.clone())).collect()
        };

        Ok(LogicalPlan::NAFill(NAFill {
            input: Box::new(input_plan),
            values,
            all_columns,
        }))
    }

    fn convert_replace(&mut self, r: &proto::NaReplace) -> Result<LogicalPlan> {
        let input = r
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("NAReplace missing input".into()))?;
        let input_plan = self.convert(input)?;
        let all_columns = self.infer_columns(&input_plan)?;

        let target_cols = if r.cols.is_empty() { all_columns.clone() } else { r.cols.clone() };

        let mut replacements: Vec<(String, Literal, Literal)> = Vec::new();
        for repl in &r.replacements {
            let old = repl
                .old_value
                .as_ref()
                .ok_or_else(|| ConnectError::PlanConversion("NAReplace missing old_value".into()))?;
            let new = repl
                .new_value
                .as_ref()
                .ok_or_else(|| ConnectError::PlanConversion("NAReplace missing new_value".into()))?;
            let old_lit = proto_literal_to_core(old)?;
            let new_lit = proto_literal_to_core(new)?;
            for col in &target_cols {
                replacements.push((col.clone(), old_lit.clone(), new_lit.clone()));
            }
        }

        Ok(LogicalPlan::NAReplace(NAReplace {
            input: Box::new(input_plan),
            replacements,
            all_columns,
        }))
    }

    fn convert_unpivot(&mut self, u: &proto::Unpivot) -> Result<LogicalPlan> {
        let input = u
            .input
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Unpivot missing input".into()))?;
        let input_plan = self.convert(input)?;

        // Extract id column names from expressions
        let ids: Vec<String> = u
            .ids
            .iter()
            .filter_map(|e| extract_column_name(e))
            .collect();

        // Extract value column names
        let values: Vec<String> = u
            .values
            .as_ref()
            .map(|v| v.values.iter().filter_map(|e| extract_column_name(e)).collect())
            .unwrap_or_default();

        Ok(LogicalPlan::Unpivot(Unpivot {
            input: Box::new(input_plan),
            ids,
            values,
            variable_column_name: u.variable_column_name.clone(),
            value_column_name: u.value_column_name.clone(),
            include_nulls: false,
        }))
    }

    fn convert_stat_cov(&mut self, c: &proto::StatCov) -> Result<LogicalPlan> {
        let input = c.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("StatCov missing input".into()))?;
        let input_plan = self.convert(input)?;
        Ok(LogicalPlan::StatCov(thunderduck_core::logical::StatCov {
            input: Box::new(input_plan),
            col1: c.col1.clone(),
            col2: c.col2.clone(),
        }))
    }

    fn convert_stat_corr(&mut self, c: &proto::StatCorr) -> Result<LogicalPlan> {
        let input = c.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("StatCorr missing input".into()))?;
        let input_plan = self.convert(input)?;
        Ok(LogicalPlan::StatCorr(thunderduck_core::logical::StatCorr {
            input: Box::new(input_plan),
            col1: c.col1.clone(),
            col2: c.col2.clone(),
            method: c.method.clone().unwrap_or_else(|| "pearson".to_string()),
        }))
    }

    fn convert_approx_quantile(&mut self, aq: &proto::StatApproxQuantile) -> Result<LogicalPlan> {
        let input = aq.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("ApproxQuantile missing input".into()))?;
        let input_plan = self.convert(input)?;
        Ok(LogicalPlan::ApproxQuantile(thunderduck_core::logical::ApproxQuantile {
            input: Box::new(input_plan),
            cols: aq.cols.clone(),
            probabilities: aq.probabilities.clone(),
            relative_error: aq.relative_error,
        }))
    }

    fn convert_stat_crosstab(&mut self, c: &proto::StatCrosstab) -> Result<LogicalPlan> {
        let input = c.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("StatCrosstab missing input".into()))?;
        Ok(LogicalPlan::StatCrosstab(thunderduck_core::logical::StatCrosstab {
            input: Box::new(self.convert(input)?),
            col1: c.col1.clone(),
            col2: c.col2.clone(),
        }))
    }

    fn convert_stat_freq_items(&mut self, f: &proto::StatFreqItems) -> Result<LogicalPlan> {
        let input = f.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("StatFreqItems missing input".into()))?;
        Ok(LogicalPlan::StatFreqItems(thunderduck_core::logical::StatFreqItems {
            input: Box::new(self.convert(input)?),
            cols: f.cols.clone(),
            support: f.support.unwrap_or(0.01),
        }))
    }

    fn convert_stat_sample_by(&mut self, s: &proto::StatSampleBy) -> Result<LogicalPlan> {
        let input = s.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("StatSampleBy missing input".into()))?;
        let col_proto = s.col.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("StatSampleBy missing col".into()))?;
        let col_expr = self.expr_conv.convert(col_proto)?;
        let fractions = s.fractions.iter()
            .map(|frac| {
                let stratum_proto = frac.stratum.as_ref()
                    .ok_or_else(|| ConnectError::PlanConversion("SampleBy fraction missing stratum".into()))?;
                let lit_expr = self.expr_conv.convert_literal(stratum_proto)?;
                match lit_expr {
                    thunderduck_core::expression::Expression::Literal(lit) => Ok((lit, frac.fraction)),
                    _ => Err(ConnectError::PlanConversion("SampleBy stratum not a literal".into())),
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LogicalPlan::StatSampleBy(thunderduck_core::logical::StatSampleBy {
            input: Box::new(self.convert(input)?),
            col_expr,
            fractions,
            seed: s.seed,
        }))
    }

    fn convert_with_relations(&mut self, wr: &proto::WithRelations) -> Result<LogicalPlan> {
        let root = wr.root.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("WithRelations missing root".into()))?;
        let mut ctes: Vec<(String, Box<LogicalPlan>)> = Vec::with_capacity(wr.references.len());
        for reference in &wr.references {
            if let Some(proto::relation::RelType::SubqueryAlias(sa)) = &reference.rel_type {
                let body = sa.input.as_ref()
                    .ok_or_else(|| ConnectError::PlanConversion("WithRelations CTE missing body".into()))?;
                ctes.push((sa.alias.clone(), Box::new(self.convert(body)?)));
            } else {
                return Err(ConnectError::PlanConversion(
                    "WithRelations: expected SubqueryAlias for each CTE reference".into(),
                ));
            }
        }
        Ok(LogicalPlan::WithCte(thunderduck_core::logical::WithCte {
            ctes,
            input: Box::new(self.convert(root)?),
        }))
    }

    fn convert_describe(&mut self, d: &proto::StatDescribe) -> Result<LogicalPlan> {
        let input = d.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Describe missing input".into()))?;
        let input_plan = self.convert(input)?;
        let cols = if d.cols.is_empty() {
            self.infer_columns(&input_plan)?
        } else {
            d.cols.clone()
        };
        Ok(LogicalPlan::Describe(thunderduck_core::logical::Describe {
            input: Box::new(input_plan),
            cols,
        }))
    }

    fn convert_summary(&mut self, s: &proto::StatSummary) -> Result<LogicalPlan> {
        let input = s.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Summary missing input".into()))?;
        let input_plan = self.convert(input)?;
        let cols = self.infer_columns(&input_plan)?;
        Ok(LogicalPlan::Summary(thunderduck_core::logical::Summary {
            input: Box::new(input_plan),
            statistics: s.statistics.clone(),
            cols,
        }))
    }

    fn convert_to_schema(&mut self, ts: &proto::ToSchema) -> Result<LogicalPlan> {
        use crate::converter::type_converter::{proto_to_data_type, proto_struct_to_struct_type};
        use thunderduck_core::expression::{AliasExpression, CastExpression, Expression, UnresolvedColumn};
        use thunderduck_core::logical::Project;

        let input = ts.input.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("ToSchema missing input".into()))?;
        let input_plan = self.convert(input)?;

        // Parse the target schema from the DataType proto
        let target_type_proto = ts.schema.as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("ToSchema missing schema".into()))?;
        let target_data_type = proto_to_data_type(target_type_proto)?;

        // The schema must be a struct type
        let struct_type = match target_data_type {
            thunderduck_core::types::DataType::Struct(s) => s,
            _ => {
                // Try to interpret directly as struct fields from the proto
                if let Some(proto::data_type::Kind::Struct(s)) = &target_type_proto.kind {
                    proto_struct_to_struct_type(s)?
                } else {
                    return Err(ConnectError::PlanConversion(
                        "ToSchema schema must be a struct type".into(),
                    ));
                }
            }
        };

        // Build a Project that casts each field to the target type
        let exprs: Vec<Expression> = struct_type.fields.iter()
            .map(|field| {
                let col = Expression::UnresolvedColumn(UnresolvedColumn {
                    name: field.name.clone(),
                    qualifier: None,
                });
                let cast = Expression::Cast(CastExpression {
                    expr: Box::new(col),
                    to_type: field.data_type.clone(),
                    try_cast: false,
                });
                Expression::Alias(AliasExpression {
                    expr: Box::new(cast),
                    alias: field.name.clone(),
                })
            })
            .collect();

        Ok(LogicalPlan::Project(Project {
            input: Box::new(input_plan),
            projections: exprs,
        }))
    }

    fn convert_catalog(&mut self, cat: &proto::Catalog) -> Result<LogicalPlan> {
        use proto::catalog::CatType;
        use thunderduck_core::logical::SqlRelation;

        match &cat.cat_type {
            Some(CatType::TableExists(te)) => {
                // Return a single boolean row: true if table exists, false otherwise
                let table_name = te.table_name.replace('\'', "''");
                let sql = format!(
                    "SELECT COUNT(*) > 0 AS value FROM information_schema.tables WHERE table_name = '{table_name}'"
                );
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::DatabaseExists(de)) => {
                let db_name = de.db_name.replace('\'', "''");
                let sql = format!(
                    "SELECT COUNT(*) > 0 AS value FROM information_schema.schemata WHERE schema_name = '{db_name}'"
                );
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::DropTempView(dtv)) => {
                let view_name = &dtv.view_name;
                let sql = format!("DROP VIEW IF EXISTS {view_name}");
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::CurrentDatabase(_)) => {
                let sql = "SELECT current_schema() AS value".to_string();
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::CurrentCatalog(_)) => {
                let sql = "SELECT current_catalog() AS value".to_string();
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::IsCached(_)) => {
                // DuckDB has no cache concept — always false
                let sql = "SELECT false AS value".to_string();
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::CacheTable(_))
            | Some(CatType::UncacheTable(_))
            | Some(CatType::ClearCache(_))
            | Some(CatType::RefreshTable(_))
            | Some(CatType::RefreshByPath(_)) => {
                // No-op: DuckDB has no cache to manage — return success
                let sql = "SELECT true AS value".to_string();
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::FunctionExists(fe)) => {
                let func_name = fe.function_name.to_lowercase().replace('\'', "''");
                let sql = format!(
                    "SELECT EXISTS(SELECT 1 FROM duckdb_functions() \
                     WHERE lower(function_name) = '{func_name}' \
                     AND schema_name NOT IN ('information_schema', 'pg_catalog')) AS value"
                );
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::ListFunctions(lf)) => {
                let mut conditions =
                    "schema_name NOT IN ('information_schema', 'pg_catalog')".to_string();
                if let Some(db) = &lf.db_name {
                    let db = db.replace('\'', "''");
                    conditions.push_str(&format!(" AND schema_name = '{db}'"));
                }
                if let Some(pat) = &lf.pattern {
                    let pat = pat.replace('\'', "''");
                    conditions.push_str(&format!(" AND function_name ILIKE '{pat}'"));
                }
                let sql = format!(
                    "SELECT DISTINCT \
                     function_name AS name, \
                     'spark_catalog' AS catalog, \
                     '\"' || schema_name || '\"' AS namespace, \
                     COALESCE(description, '') AS description, \
                     'org.duckdb.builtin.' || function_name AS className, \
                     false AS isTemporary \
                     FROM duckdb_functions() \
                     WHERE {conditions} \
                     ORDER BY function_name"
                );
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            Some(CatType::GetFunction(gf)) => {
                let func_name = gf.function_name.to_lowercase().replace('\'', "''");
                let mut conditions = format!(
                    "lower(function_name) = '{func_name}' \
                     AND schema_name NOT IN ('information_schema', 'pg_catalog')"
                );
                if let Some(db) = &gf.db_name {
                    let db = db.replace('\'', "''");
                    conditions.push_str(&format!(" AND schema_name = '{db}'"));
                }
                let sql = format!(
                    "SELECT DISTINCT \
                     function_name AS name, \
                     'spark_catalog' AS catalog, \
                     '\"' || schema_name || '\"' AS namespace, \
                     COALESCE(description, '') AS description, \
                     'org.duckdb.builtin.' || function_name AS className, \
                     false AS isTemporary \
                     FROM duckdb_functions() \
                     WHERE {conditions} \
                     LIMIT 1"
                );
                Ok(LogicalPlan::SqlRelation(SqlRelation { sql, schema: StructType::empty() }))
            }
            _ => Err(ConnectError::Unsupported(format!(
                "Unsupported catalog operation: {:?}",
                std::mem::discriminant(cat.cat_type.as_ref().unwrap())
            ))),
        }
    }

    /// Infer column names of a plan using SchemaInferrer (requires session).
    fn infer_columns(&self, plan: &LogicalPlan) -> Result<Vec<String>> {
        // Fast path: plan-level schema inference (works for many plan types)
        let schema = plan.infer_schema();
        if !schema.is_empty() {
            return Ok(schema.fields.into_iter().map(|f| f.name).collect());
        }
        // Slow path: execute a LIMIT 0 query via DuckDB
        if let Some(session) = &self.session {
            let sql = SqlGenerator::relaxed()
                .generate(plan)
                .map_err(|e| ConnectError::PlanConversion(format!("schema inference SQL gen: {e}")))?;
            let session = Arc::clone(session);
            let struct_type = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    SchemaInferrer::new(&session).infer_sql(&sql).await
                })
            })
            .map_err(|e| ConnectError::PlanConversion(format!("schema inference: {e}")))?;
            Ok(struct_type.fields.into_iter().map(|f| f.name).collect())
        } else {
            Err(ConnectError::Unsupported(
                "Schema inference required but no session available".into(),
            ))
        }
    }

    /// Infer the full schema (column names + types) of a plan using DuckDB.
    fn infer_full_schema(&self, plan: &LogicalPlan) -> Result<StructType> {
        if let Some(session) = &self.session {
            let sql = SqlGenerator::relaxed()
                .generate(plan)
                .map_err(|e| ConnectError::PlanConversion(format!("schema inference SQL gen: {e}")))?;
            let session = Arc::clone(session);
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    SchemaInferrer::new(&session).infer_sql(&sql).await
                })
            })
            .map_err(|e| ConnectError::PlanConversion(format!("schema inference: {e}")))
        } else {
            Err(ConnectError::Unsupported(
                "Schema inference required but no session available".into(),
            ))
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns true if any projection contains a `dropFields` call that still needs struct schema.
fn needs_drop_fields_inference(exprs: &[thunderduck_core::expression::Expression]) -> bool {
    use thunderduck_core::expression::Expression;
    exprs.iter().any(|e| {
        let uf = match e {
            Expression::UpdateFields(uf) => uf,
            Expression::Alias(alias) => match alias.expr.as_ref() {
                Expression::UpdateFields(uf) => uf,
                _ => return false,
            },
            _ => return false,
        };
        uf.value.is_none() && uf.struct_fields.is_none()
    })
}

/// For any `UpdateFields { value: None }` (dropFields) in projections, populate `struct_fields`
/// from the input schema so the SQL generator can build the correct `struct_pack(...)`.
fn populate_drop_fields_schema(
    schema: &thunderduck_core::types::StructType,
    exprs: &mut Vec<thunderduck_core::expression::Expression>,
) {
    use thunderduck_core::expression::Expression;
    use thunderduck_core::types::DataType;

    for expr in exprs.iter_mut() {
        // Drill through optional Alias wrapper
        let uf = match expr {
            Expression::UpdateFields(ref mut uf) => uf,
            Expression::Alias(ref mut alias) => {
                if let Expression::UpdateFields(ref mut uf) = *alias.expr {
                    uf
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        // Only for dropFields (value=None) that haven't been populated yet
        if uf.value.is_some() || uf.struct_fields.is_some() {
            continue;
        }
        // Extract the column name from the struct expression
        let col_name = match uf.struct_expr.as_ref() {
            Expression::UnresolvedColumn(c) => c.name.clone(),
            _ => continue,
        };
        // Find the column in the schema and extract its struct fields
        if let Some(sf) = schema.field_by_name(&col_name) {
            if let DataType::Struct(st) = &sf.data_type {
                uf.struct_fields = Some(st.field_names().iter().map(|s| s.to_string()).collect());
            }
        }
    }
}

fn parse_arrow_schema(data: &[u8]) -> Result<StructType> {
    use arrow_ipc::reader::StreamReader;
    use std::io::Cursor;

    let cursor = Cursor::new(data);
    let reader = StreamReader::try_new(cursor, None)
        .map_err(|e| ConnectError::Arrow(format!("Arrow IPC parse error: {e}")))?;
    let schema = reader.schema();
    let fields = schema
        .fields()
        .iter()
        .map(|f| {
            let dt = arrow_field_to_data_type(f.data_type());
            if f.is_nullable() {
                StructField::nullable(f.name().clone(), dt)
            } else {
                StructField::not_null(f.name().clone(), dt)
            }
        })
        .collect();
    Ok(StructType::new(fields))
}

fn arrow_field_to_data_type(dt: &arrow::datatypes::DataType) -> DataType {
    use arrow::datatypes::DataType as ArrowDT;
    match dt {
        ArrowDT::Boolean => DataType::Boolean,
        ArrowDT::Int8 => DataType::Byte,
        ArrowDT::Int16 => DataType::Short,
        ArrowDT::Int32 => DataType::Integer,
        ArrowDT::Int64 => DataType::Long,
        ArrowDT::Float32 => DataType::Float,
        ArrowDT::Float64 => DataType::Double,
        ArrowDT::Utf8 | ArrowDT::LargeUtf8 => DataType::String,
        ArrowDT::Binary | ArrowDT::LargeBinary => DataType::Binary,
        ArrowDT::Date32 => DataType::Date,
        ArrowDT::Timestamp(_, _) => DataType::Timestamp,
        ArrowDT::Decimal128(p, s) => DataType::Decimal { precision: *p, scale: *s as u8 },
        ArrowDT::List(f) | ArrowDT::LargeList(f) => {
            DataType::Array(Box::new(arrow_field_to_data_type(f.data_type())))
        }
        ArrowDT::Map(field, _) => {
            if let ArrowDT::Struct(fields) = field.data_type() {
                let key = fields.iter().find(|f| f.name() == "key")
                    .map(|f| arrow_field_to_data_type(f.data_type()))
                    .unwrap_or(DataType::Unresolved);
                let value = fields.iter().find(|f| f.name() == "value")
                    .map(|f| arrow_field_to_data_type(f.data_type()))
                    .unwrap_or(DataType::Unresolved);
                let value_nullable = fields.iter().find(|f| f.name() == "value")
                    .map(|f| f.is_nullable())
                    .unwrap_or(true);
                DataType::Map { key: Box::new(key), value: Box::new(value), value_nullable }
            } else {
                DataType::Unresolved
            }
        }
        ArrowDT::Struct(fields) => {
            let struct_fields = fields.iter().map(|f| {
                let dt = arrow_field_to_data_type(f.data_type());
                if f.is_nullable() {
                    StructField::nullable(f.name().clone(), dt)
                } else {
                    StructField::not_null(f.name().clone(), dt)
                }
            }).collect();
            DataType::Struct(StructType::new(struct_fields))
        }
        _ => DataType::Unresolved,
    }
}

/// Parse a Spark schema string into a StructType.
///
/// Handles two formats PySpark sends for empty DataFrames:
/// 1. JSON: `{"type":"struct","fields":[{"name":"id","type":"integer","nullable":false,...}]}`
/// 2. DDL:  `col1 TYPE1 [NOT NULL], col2 TYPE2 [NOT NULL], ...`
///          or with STRUCT wrapper: `STRUCT<col1: TYPE1, col2: TYPE2>`
fn parse_ddl_schema(s: &str) -> crate::error::Result<StructType> {
    let s = s.trim();
    // Detect JSON format (PySpark sends JSON schema string for empty DataFrames)
    if s.starts_with('{') {
        return parse_json_schema(s);
    }
    parse_ddl_schema_inner(s)
}

/// Parse Spark JSON schema format: {"type":"struct","fields":[...]}
fn parse_json_schema(json: &str) -> crate::error::Result<StructType> {
    // Find "fields":[ and extract the array content
    let fields_key = match json.find("\"fields\"") {
        Some(p) => p,
        None => return Ok(StructType::new(vec![])),
    };
    let after_key = &json[fields_key + 8..]; // skip `"fields"`
    let bracket_pos = match after_key.find('[') {
        Some(p) => p,
        None => return Ok(StructType::new(vec![])),
    };
    let array_content_start = fields_key + 8 + bracket_pos + 1;

    // Find the matching ] at the same depth
    let array_content = extract_json_array_content(&json[array_content_start..]);

    // Split array content into individual field objects `{...}`
    let field_jsons = split_json_objects(array_content);

    let mut fields = Vec::new();
    for obj in field_jsons {
        let obj = obj.trim();
        if obj.is_empty() { continue; }
        let name = json_string_value(obj, "name").unwrap_or_default();
        let nullable = json_bool_value(obj, "nullable").unwrap_or(true);
        // "type" can be a quoted string or a nested object
        let dt = json_type_value(obj);
        if nullable {
            fields.push(StructField::nullable(name, dt));
        } else {
            fields.push(StructField::not_null(name, dt));
        }
    }
    Ok(StructType::new(fields))
}

/// Return the content inside the first `[...]` at depth 0.
fn extract_json_array_content(s: &str) -> &str {
    let mut depth = 1i32;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return &s[..i];
                }
            }
            _ => {}
        }
    }
    s
}

/// Split a JSON array body into individual top-level `{...}` strings.
fn split_json_objects(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, c) in s.char_indices() {
        match c {
            '{' => {
                if depth == 0 { start = Some(i); }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s_pos) = start {
                        result.push(&s[s_pos..=i]);
                        start = None;
                    }
                }
            }
            _ => {}
        }
    }
    result
}

/// Extract a JSON string value for a given key from a shallow JSON object string.
fn json_string_value<'a>(obj: &'a str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let pos = obj.find(&needle)?;
    let after_key = &obj[pos + needle.len()..];
    // Skip : and whitespace
    let after_colon = after_key.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if !after_colon.starts_with('"') { return None; }
    let inner = &after_colon[1..];
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

/// Extract a JSON boolean value for a given key.
fn json_bool_value(obj: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{}\"", key);
    let pos = obj.find(&needle)?;
    let after_key = &obj[pos + needle.len()..];
    let after_colon = after_key.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if after_colon.starts_with("true") { Some(true) }
    else if after_colon.starts_with("false") { Some(false) }
    else { None }
}

/// Extract the DataType from the "type" field of a Spark JSON field object.
/// The type can be a simple string ("integer") or a nested object ({"type":"array",...}).
fn json_type_value(obj: &str) -> DataType {
    let needle = "\"type\"";
    // Find the "type" key — skip the outermost "type":"struct" if this is the root
    // We want the FIRST occurrence of "type" after the opening {
    let pos = match obj.find(needle) {
        Some(p) => p,
        None => return DataType::Unresolved,
    };
    let after_key = &obj[pos + needle.len()..];
    let after_colon = after_key.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if after_colon.starts_with('"') {
        // Simple type string
        let inner = &after_colon[1..];
        let end = inner.find('"').unwrap_or(inner.len());
        let type_str = &inner[..end];
        parse_type_str(type_str)
    } else if after_colon.starts_with('{') {
        // Nested type object — parse it
        parse_json_type_object(after_colon)
    } else {
        DataType::Unresolved
    }
}

/// Parse a nested Spark JSON type object like {"type":"array","elementType":"integer",...}.
fn parse_json_type_object(obj: &str) -> DataType {
    // Extract "type" from within this object
    let type_name = json_string_value(obj, "type").unwrap_or_default();
    match type_name.as_str() {
        "array" => {
            let elem = json_string_value(obj, "elementType")
                .map(|t| parse_type_str(&t))
                .unwrap_or(DataType::Unresolved);
            DataType::Array(Box::new(elem))
        }
        "map" => {
            let key_dt = json_string_value(obj, "keyType")
                .map(|t| parse_type_str(&t))
                .unwrap_or(DataType::Unresolved);
            let val_dt = json_string_value(obj, "valueType")
                .map(|t| parse_type_str(&t))
                .unwrap_or(DataType::Unresolved);
            DataType::Map { key: Box::new(key_dt), value: Box::new(val_dt), value_nullable: true }
        }
        "struct" => DataType::Struct(StructType::new(vec![])),
        _ => DataType::Unresolved,
    }
}

fn parse_ddl_schema_inner(s: &str) -> crate::error::Result<StructType> {
    // Unwrap STRUCT<...> wrapper if present
    let inner = {
        let upper = s.to_uppercase();
        if upper.starts_with("STRUCT<") && s.ends_with('>') {
            &s[7..s.len() - 1]
        } else {
            s
        }
    };

    // Split at top-level commas (ignore commas inside <> or ())
    let parts = split_ddl_fields(inner);
    let mut fields = Vec::new();
    for part in &parts {
        let part = part.trim();
        if part.is_empty() { continue; }
        // Each field: name type [NOT NULL]
        // name may be backtick-quoted or bare; first token up to whitespace
        let (name, rest) = match part.find(|c: char| c.is_whitespace() || c == ':') {
            Some(idx) => (&part[..idx], part[idx..].trim_start_matches(':').trim()),
            None => (part, ""),
        };
        let name = name.trim_matches('`').to_string();
        // Type is everything up to NOT NULL
        let upper_rest = rest.to_uppercase();
        let type_str = if let Some(p) = upper_rest.find(" NOT NULL") {
            &rest[..p]
        } else {
            rest
        };
        let nullable = !upper_rest.contains("NOT NULL");
        let dt = parse_type_str(type_str.trim());
        if nullable {
            fields.push(StructField::nullable(name, dt));
        } else {
            fields.push(StructField::not_null(name, dt));
        }
    }
    Ok(StructType::new(fields))
}

/// Split a DDL field list by top-level commas (ignoring commas inside < > or ( )).
fn split_ddl_fields(s: &str) -> Vec<&str> {
    let mut depth = 0i32;
    let mut start = 0;
    let mut result = Vec::new();
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            ',' if depth == 0 => {
                result.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    result.push(&s[start..]);
    result
}

/// Extract a column name from a proto Expression (UnresolvedAttribute only).
fn extract_column_name(expr: &proto::Expression) -> Option<String> {
    if let Some(proto::expression::ExprType::UnresolvedAttribute(attr)) = &expr.expr_type {
        Some(attr.unparsed_identifier.clone())
    } else {
        None
    }
}

/// Convert a proto literal to a core Literal.
fn proto_literal_to_core(lit: &proto::expression::Literal) -> Result<Literal> {
    use proto::expression::literal::LiteralType;
    let value = match &lit.literal_type {
        Some(LiteralType::Null(_)) => LiteralValue::Null,
        Some(LiteralType::Boolean(b)) => LiteralValue::Boolean(*b),
        Some(LiteralType::Byte(i)) => LiteralValue::Int(*i),
        Some(LiteralType::Short(i)) => LiteralValue::Int(*i),
        Some(LiteralType::Integer(i)) => LiteralValue::Int(*i),
        Some(LiteralType::Long(l)) => LiteralValue::Long(*l),
        Some(LiteralType::Float(f)) => LiteralValue::Float(*f),
        Some(LiteralType::Double(d)) => LiteralValue::Double(*d),
        Some(LiteralType::String(s)) => LiteralValue::String(s.clone()),
        _ => LiteralValue::Null,
    };
    Ok(Literal { value, data_type: DataType::Unresolved })
}

// ── LocalRelation data materialisation ─────────────────────────────────────────

/// Parse Arrow IPC bytes and emit a `(SELECT … UNION ALL SELECT …)` SQL expression
/// that can be used as a subquery anywhere a table expression is valid in DuckDB.
fn local_relation_to_values_sql(data: &[u8]) -> Result<String> {
    use arrow::array::{
        Array, BooleanArray, Date32Array, Float32Array, Float64Array, Int16Array,
        Int32Array, Int64Array, Int8Array, LargeStringArray, ListArray, MapArray,
        StringArray, StructArray, TimestampMicrosecondArray,
    };
    use arrow::datatypes::DataType as ArrowDT;
    use arrow_ipc::reader::StreamReader;
    use std::io::Cursor;
    use thunderduck_core::generator::quote_ident;

    let cursor = Cursor::new(data);
    let reader = StreamReader::try_new(cursor, None)
        .map_err(|e| ConnectError::Arrow(format!("Arrow IPC parse: {e}")))?;
    let schema = reader.schema();
    let col_names: Vec<String> = schema.fields().iter().map(|f| quote_ident(f.name())).collect();

    let batches: Vec<arrow::record_batch::RecordBatch> = reader
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| ConnectError::Arrow(format!("Arrow IPC collect: {e}")))?;

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    if total_rows == 0 {
        return Err(ConnectError::PlanConversion("empty arrow data".into()));
    }

    fn val(array: &dyn Array, row: usize) -> String {
        if array.is_null(row) {
            return "NULL".to_string();
        }
        match array.data_type() {
            ArrowDT::Boolean => {
                let a = array.as_any().downcast_ref::<BooleanArray>().unwrap();
                if a.value(row) { "true".to_string() } else { "false".to_string() }
            }
            ArrowDT::Int8 => {
                array.as_any().downcast_ref::<Int8Array>().unwrap().value(row).to_string()
            }
            ArrowDT::Int16 => {
                array.as_any().downcast_ref::<Int16Array>().unwrap().value(row).to_string()
            }
            ArrowDT::Int32 => {
                array.as_any().downcast_ref::<Int32Array>().unwrap().value(row).to_string()
            }
            ArrowDT::Int64 => {
                array.as_any().downcast_ref::<Int64Array>().unwrap().value(row).to_string()
            }
            ArrowDT::Float32 => {
                let v = array.as_any().downcast_ref::<Float32Array>().unwrap().value(row);
                if v.is_nan() { "'NaN'::FLOAT".to_string() }
                else if v == f32::INFINITY { "'Infinity'::FLOAT".to_string() }
                else if v == f32::NEG_INFINITY { "'-Infinity'::FLOAT".to_string() }
                else { format!("{v:.10}::FLOAT") }
            }
            ArrowDT::Float64 => {
                let v = array.as_any().downcast_ref::<Float64Array>().unwrap().value(row);
                if v.is_nan() { "'NaN'::DOUBLE".to_string() }
                else if v == f64::INFINITY { "'Infinity'::DOUBLE".to_string() }
                else if v == f64::NEG_INFINITY { "'-Infinity'::DOUBLE".to_string() }
                else { format!("{v:.17}::DOUBLE") }
            }
            ArrowDT::Utf8 => {
                let s = array.as_any().downcast_ref::<StringArray>().unwrap().value(row);
                format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
            }
            ArrowDT::LargeUtf8 => {
                let s = array.as_any().downcast_ref::<LargeStringArray>().unwrap().value(row);
                format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
            }
            ArrowDT::Date32 => {
                let days = array.as_any().downcast_ref::<Date32Array>().unwrap().value(row);
                // epoch days → DuckDB date arithmetic
                format!("(DATE '1970-01-01' + INTERVAL '{days}' DAY)")
            }
            ArrowDT::Timestamp(_, _) => {
                let micros = array
                    .as_any()
                    .downcast_ref::<TimestampMicrosecondArray>()
                    .unwrap()
                    .value(row);
                format!("(TIMESTAMP '1970-01-01' + INTERVAL '{micros}' MICROSECOND)")
            }
            ArrowDT::List(_) => {
                let a = array.as_any().downcast_ref::<ListArray>().unwrap();
                let list = a.value(row);
                let elements: Vec<String> =
                    (0..list.len()).map(|i| val(list.as_ref(), i)).collect();
                format!("[{}]", elements.join(", "))
            }
            ArrowDT::Map(_, _) => {
                // Arrow Map: entries array is a StructArray with "key" and "value" fields.
                let a = array.as_any().downcast_ref::<MapArray>().unwrap();
                let entries = a.value(row);
                let sa = entries.as_any().downcast_ref::<StructArray>().unwrap();
                let keys = sa.column(0);
                let vals = sa.column(1);
                if keys.len() == 0 {
                    return "MAP([], [])".to_string();
                }
                let k_sqls: Vec<String> = (0..keys.len()).map(|i| val(keys.as_ref(), i)).collect();
                let v_sqls: Vec<String> = (0..vals.len()).map(|i| val(vals.as_ref(), i)).collect();
                format!("MAP([{}], [{}])", k_sqls.join(", "), v_sqls.join(", "))
            }
            ArrowDT::Struct(_) => {
                // Struct: emit as DuckDB struct_pack syntax
                let a = array.as_any().downcast_ref::<StructArray>().unwrap();
                let pairs: Vec<String> = a.fields().iter().enumerate().map(|(ci, f)| {
                    let col = a.column(ci);
                    format!("{}: {}", f.name(), val(col.as_ref(), row))
                }).collect();
                format!("{{{}}}", pairs.join(", "))
            }
            _ => "NULL".to_string(),
        }
    }

    let mut rows: Vec<String> = Vec::with_capacity(total_rows);
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let values: Vec<String> =
                batch.columns().iter().map(|c| val(c.as_ref(), row)).collect();
            if rows.is_empty() {
                let pairs: Vec<String> = values
                    .iter()
                    .zip(col_names.iter())
                    .map(|(v, n)| format!("{v} AS {n}"))
                    .collect();
                rows.push(format!("SELECT {}", pairs.join(", ")));
            } else {
                rows.push(format!("SELECT {}", values.join(", ")));
            }
        }
    }

    // No outer parens — gen_from(SqlRelation) adds the wrapping parentheses.
    Ok(rows.join(" UNION ALL "))
}

/// Expand `explode(map_col)` / `explode_outer(map_col)` expressions into two RawSql expressions
/// (`UNNEST(map_keys(col)) AS "key"` and `UNNEST(map_values(col)) AS "value"`) when the column
/// is a MAP type. This must be called before SQL generation since DuckDB cannot UNNEST a MAP.
fn expand_map_explodes(input_schema: &thunderduck_core::types::StructType, projections: &mut Vec<Expression>) {
    use thunderduck_core::expression::RawSqlExpression;
    let needs_expansion = projections.iter().any(|e| {
        if let Expression::FunctionCall(fc) = e {
            let n = fc.name.to_ascii_lowercase();
            (n == "explode" || n == "explode_outer") && fc.args.len() == 1
        } else {
            false
        }
    });
    if !needs_expansion {
        return;
    }
    let mut new_proj = Vec::with_capacity(projections.len() + 1);
    for expr in projections.drain(..) {
        if let Expression::FunctionCall(ref fc) = expr {
            let fname = fc.name.to_ascii_lowercase();
            if (fname == "explode" || fname == "explode_outer") && fc.args.len() == 1 {
                let col_name = match &fc.args[0] {
                    Expression::UnresolvedColumn(u) if u.qualifier.is_none() => Some(u.name.clone()),
                    _ => None,
                };
                if let Some(ref name) = col_name {
                    let is_map = input_schema.fields.iter().any(|f| {
                        f.name.eq_ignore_ascii_case(name)
                            && matches!(f.data_type, DataType::Map { .. })
                    });
                    if is_map {
                        let col_sql = format!("\"{}\"", name.replace('"', "\"\""));
                        let outer = fname == "explode_outer";
                        if outer {
                            new_proj.push(Expression::RawSql(RawSqlExpression {
                                sql: format!(
                                    "UNNEST(CASE WHEN {col_sql} IS NULL THEN [NULL] ELSE map_keys({col_sql}) END) AS \"key\""
                                ),
                            }));
                            new_proj.push(Expression::RawSql(RawSqlExpression {
                                sql: format!(
                                    "UNNEST(CASE WHEN {col_sql} IS NULL THEN [NULL] ELSE map_values({col_sql}) END) AS \"value\""
                                ),
                            }));
                        } else {
                            new_proj.push(Expression::RawSql(RawSqlExpression {
                                sql: format!("UNNEST(map_keys({col_sql})) AS \"key\""),
                            }));
                            new_proj.push(Expression::RawSql(RawSqlExpression {
                                sql: format!("UNNEST(map_values({col_sql})) AS \"value\""),
                            }));
                        }
                        continue;
                    }
                }
            }
        }
        new_proj.push(expr);
    }
    *projections = new_proj;
}

/// If `plan` is a Join with plan_id aliases, qualify all plan_id column references in `exprs`
/// using the join's left/right alias mapping. This is called on Project/Filter expressions
/// that sit directly above an alias-qualified join.
fn qualify_exprs_for_join(plan: &LogicalPlan, exprs: &mut Vec<Expression>) {
    if let LogicalPlan::Join(j) = plan {
        if let (Some(la), Some(ra)) = (j.left_alias.clone(), j.right_alias.clone()) {
            let left_set: std::collections::HashSet<i64> = j.left_plan_ids.iter().copied().collect();
            let right_set: std::collections::HashSet<i64> = j.right_plan_ids.iter().copied().collect();
            let qualified: Vec<Expression> = exprs.drain(..)
                .map(|e| qualify_join_condition(e, &left_set, &right_set, &la, &ra))
                .collect();
            *exprs = qualified;
        }
    }
}

/// Walk a proto Relation tree and collect all `RelationCommon.plan_id` values.
/// These IDs are assigned by the PySpark client to uniquely identify each DataFrame/plan.
fn collect_relation_plan_ids(rel: &proto::Relation, ids: &mut std::collections::HashSet<i64>) {
    if let Some(common) = &rel.common {
        if let Some(id) = common.plan_id {
            ids.insert(id);
        }
    }
    use proto::relation::RelType;
    match &rel.rel_type {
        Some(RelType::Filter(f)) => { if let Some(i) = &f.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::Project(p)) => { if let Some(i) = &p.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::Aggregate(a)) => { if let Some(i) = &a.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::Sort(s)) => { if let Some(i) = &s.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::Limit(l)) => { if let Some(i) = &l.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::Deduplicate(d)) => { if let Some(i) = &d.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::SubqueryAlias(sa)) => { if let Some(i) = &sa.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::Sample(s)) => { if let Some(i) = &s.input { collect_relation_plan_ids(i, ids); } }
        Some(RelType::Join(j)) => {
            if let Some(l) = &j.left { collect_relation_plan_ids(l, ids); }
            if let Some(r) = &j.right { collect_relation_plan_ids(r, ids); }
        }
        Some(RelType::SetOp(s)) => {
            if let Some(l) = &s.left_input { collect_relation_plan_ids(l, ids); }
            if let Some(r) = &s.right_input { collect_relation_plan_ids(r, ids); }
        }
        Some(RelType::WithRelations(wr)) => {
            if let Some(r) = &wr.root { collect_relation_plan_ids(r, ids); }
            for ref_ in &wr.references { collect_relation_plan_ids(ref_, ids); }
        }
        _ => {}
    }
}

/// Return true if any column in the expression has a `__plan_id_*__` qualifier.
fn condition_has_plan_id(expr: &Expression) -> bool {
    use thunderduck_core::expression::Expression as E;
    match expr {
        E::UnresolvedColumn(u) => u.qualifier.as_ref()
            .map_or(false, |q| q.starts_with("__plan_id_") && q.ends_with("__")),
        E::Binary(b) => condition_has_plan_id(&b.left) || condition_has_plan_id(&b.right),
        E::Unary(u) => condition_has_plan_id(&u.operand),
        E::FunctionCall(f) => f.args.iter().any(condition_has_plan_id),
        E::Cast(c) => condition_has_plan_id(&c.expr),
        E::Alias(a) => condition_has_plan_id(&a.expr),
        E::CaseWhen(cw) => {
            cw.branches.iter().any(|(w, t)| condition_has_plan_id(w) || condition_has_plan_id(t))
                || cw.else_expr.as_ref().map_or(false, |e| condition_has_plan_id(e))
        }
        _ => false,
    }
}

/// Build an equijoin ON condition from a list of USING column names.
///
/// Walk the join ON condition and set the qualifier on any UnresolvedColumn whose plan_id
/// (embedded in qualifier field as "__plan_id_<N>__") maps to a join side alias.
fn qualify_join_condition(
    expr: Expression,
    left_ids: &std::collections::HashSet<i64>,
    right_ids: &std::collections::HashSet<i64>,
    left_alias: &str,
    right_alias: &str,
) -> Expression {
    use thunderduck_core::expression::{
        AliasExpression, BinaryExpression, CastExpression, CaseWhenExpression, Expression as E,
        FunctionCall, UnaryExpression, UnresolvedColumn,
    };
    let qjc = |e| qualify_join_condition(e, left_ids, right_ids, left_alias, right_alias);
    match expr {
        E::UnresolvedColumn(u) => {
            // Check if qualifier encodes a plan_id as "__plan_id_<N>__"
            if let Some(q) = &u.qualifier {
                if let Some(id_str) = q.strip_prefix("__plan_id_").and_then(|s| s.strip_suffix("__")) {
                    if let Ok(id) = id_str.parse::<i64>() {
                        let new_qualifier = if left_ids.contains(&id) {
                            Some(left_alias.to_string())
                        } else if right_ids.contains(&id) {
                            Some(right_alias.to_string())
                        } else {
                            u.qualifier.clone()
                        };
                        return E::UnresolvedColumn(UnresolvedColumn { name: u.name, qualifier: new_qualifier });
                    }
                }
            }
            E::UnresolvedColumn(u)
        }
        E::Binary(b) => E::Binary(BinaryExpression {
            op: b.op,
            left: Box::new(qjc(*b.left)),
            right: Box::new(qjc(*b.right)),
        }),
        E::Unary(u) => E::Unary(UnaryExpression {
            op: u.op,
            operand: Box::new(qjc(*u.operand)),
        }),
        E::FunctionCall(f) => E::FunctionCall(FunctionCall {
            name: f.name,
            args: f.args.into_iter().map(qjc).collect(),
            distinct: f.distinct,
        }),
        E::Cast(c) => E::Cast(CastExpression {
            expr: Box::new(qjc(*c.expr)),
            to_type: c.to_type,
            try_cast: c.try_cast,
        }),
        E::Alias(a) => E::Alias(AliasExpression {
            expr: Box::new(qjc(*a.expr)),
            alias: a.alias,
        }),
        E::CaseWhen(cw) => E::CaseWhen(CaseWhenExpression {
            base: cw.base.map(|b| Box::new(qjc(*b))),
            branches: cw.branches.into_iter().map(|(w, t)| (qjc(w), qjc(t))).collect(),
            else_expr: cw.else_expr.map(|e| Box::new(qjc(*e))),
        }),
        other => other,
    }
}
