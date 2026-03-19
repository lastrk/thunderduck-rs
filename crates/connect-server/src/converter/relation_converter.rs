use std::sync::Arc;

use thunderduck_core::expression::{Expression, Literal, LiteralValue, SortOrder, UnresolvedColumn};
use thunderduck_core::generator::SqlGenerator;
use thunderduck_core::logical::{
    Aggregate, AggregateExpr, AliasedRelation, Distinct, DropColumns, Except, Filter,
    GroupingSets, Intersect, Join, JoinType, Limit, LocalDataRelation, LogicalPlan, NADrop,
    NADropHow, NAFill, NAReplace, RangeRelation, Sample, SelectEntry, ShowString,
    SingleRowRelation, Sort, SqlRelation, TableScan, Tail, ToDataFrame, Union, Unpivot,
    WithColumns,
};
use thunderduck_core::runtime::{DuckDbSession, SchemaInferrer};
use thunderduck_core::types::{DataType, StructField, StructType};

use crate::converter::expression_converter::ExpressionConverter;
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
            Some(RelType::Summary(_)) => {
                Err(ConnectError::Unsupported("Summary not supported (Phase 5)".into()))
            }
            Some(RelType::Describe(_)) => {
                Err(ConnectError::Unsupported("Describe not supported (Phase 5)".into()))
            }
            Some(RelType::Cov(_)) => {
                Err(ConnectError::Unsupported("Cov not supported (Phase 5)".into()))
            }
            Some(RelType::Corr(_)) => {
                Err(ConnectError::Unsupported("Corr not supported (Phase 5)".into()))
            }
            Some(RelType::Unpivot(u)) => self.convert_unpivot(u),
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
        Ok(LogicalPlan::Project(thunderduck_core::logical::Project {
            input: Box::new(input),
            projections: projections?,
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
        Ok(LogicalPlan::Filter(Filter {
            input: Box::new(self.convert(input)?),
            condition: self.expr_conv.convert(condition)?,
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
        let left = j
            .left
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Join missing left".into()))?;
        let right = j
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

        let condition = if let Some(cond) = &j.join_condition {
            Some(self.expr_conv.convert(cond)?)
        } else {
            None
        };

        Ok(LogicalPlan::Join(Join {
            left: Box::new(self.convert(left)?),
            right: Box::new(self.convert(right)?),
            join_type,
            condition,
            using_columns: j.using_columns.clone(),
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
                let table =
                    ds.paths.first().cloned().unwrap_or_else(|| "unknown".to_string());
                Ok(LogicalPlan::TableScan(TableScan { table, alias: None }))
            }
            None => Err(ConnectError::PlanConversion("Read missing read_type".into())),
        }
    }

    fn convert_local_relation(&self, lr: &proto::LocalRelation) -> Result<LogicalPlan> {
        let schema = if let Some(data) = &lr.data {
            parse_arrow_schema(data)?
        } else {
            StructType::empty()
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
        Ok(LogicalPlan::SqlRelation(SqlRelation { sql: s.query.clone() }))
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
        Ok(LogicalPlan::Distinct(Distinct { input: Box::new(self.convert(input)?) }))
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
        Ok(LogicalPlan::ToDataFrame(ToDataFrame {
            input: Box::new(self.convert(input)?),
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

        // min_non_nulls unset → "any" (all must be non-null); set → threshold
        let (how, threshold) = if let Some(min) = d.min_non_nulls {
            (NADropHow::Any, Some(min)) // threshold = keep rows with >= min non-nulls
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
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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
        ArrowDT::List(f) => DataType::Array(Box::new(arrow_field_to_data_type(f.data_type()))),
        _ => DataType::Unresolved,
    }
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
