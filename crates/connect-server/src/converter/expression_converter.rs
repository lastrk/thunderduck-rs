use thunderduck_core::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CastExpression, Expression,
    ExtractValueExpression, FunctionCall, FrameBoundary, FrameUnit, LambdaExpression,
    LambdaVariableExpression, Literal, LiteralValue, NullOrdering as CoreNullOrdering,
    RawSqlExpression, SortDirection, SortOrder, StarExpression, UnaryExpression, UnaryOp,
    UnresolvedColumn, WindowFrame, WindowFunction,
};
use thunderduck_core::types::DataType;

use crate::converter::type_converter::proto_to_data_type;
use crate::error::{ConnectError, Result};
use crate::proto::spark::connect as proto;

/// Converts proto Expression messages to the core Expression AST.
///
/// Carries a lambda scope stack for nested lambda handling.
pub struct ExpressionConverter {
    /// Stack of lambda scopes; each scope is a list of bound variable names.
    lambda_scopes: Vec<Vec<String>>,
}

impl ExpressionConverter {
    pub fn new() -> Self {
        Self { lambda_scopes: Vec::new() }
    }

    /// Convert a proto Expression to a core Expression.
    pub fn convert(&mut self, expr: &proto::Expression) -> Result<Expression> {
        use proto::expression::ExprType;
        match &expr.expr_type {
            None => Err(ConnectError::PlanConversion("empty expression".into())),
            Some(ExprType::Literal(lit)) => self.convert_literal(lit),
            Some(ExprType::UnresolvedAttribute(attr)) => self.convert_unresolved_attribute(attr),
            Some(ExprType::UnresolvedFunction(func)) => self.convert_unresolved_function(func),
            Some(ExprType::Alias(alias)) => self.convert_alias(alias),
            Some(ExprType::Cast(cast)) => self.convert_cast(cast),
            Some(ExprType::UnresolvedStar(star)) => self.convert_star(star),
            Some(ExprType::Window(win)) => self.convert_window(win),
            Some(ExprType::LambdaFunction(lf)) => self.convert_lambda(lf),
            Some(ExprType::UnresolvedNamedLambdaVariable(v)) => self.convert_lambda_variable(v),
            Some(ExprType::CallFunction(cf)) => self.convert_call_function(cf),
            Some(ExprType::UnresolvedExtractValue(ev)) => self.convert_extract_value(ev),
            Some(ExprType::ExpressionString(es)) => {
                Ok(Expression::RawSql(RawSqlExpression { sql: es.expression.clone() }))
            }
            Some(ExprType::CommonInlineUserDefinedFunction(udf)) => {
                // Treat as a regular function call with the UDF name.
                let args: Result<Vec<Expression>> =
                    udf.arguments.iter().map(|a| self.convert(a)).collect();
                Ok(Expression::FunctionCall(FunctionCall {
                    name: udf.function_name.clone(),
                    args: args?,
                    distinct: udf.is_distinct,
                }))
            }
            Some(ExprType::SortOrder(_)) => Err(ConnectError::PlanConversion(
                "SortOrder should be handled by RelationConverter".into(),
            )),
            Some(ExprType::UpdateFields(_)) => {
                Err(ConnectError::Unsupported("UpdateFields not supported (Phase 4)".into()))
            }
            Some(ExprType::UnresolvedRegex(r)) => {
                // Treat as an unresolved column; regex expansion is a server-side concern
                Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                    name: r.col_name.clone(),
                    qualifier: None,
                }))
            }
            Some(ExprType::NamedArgumentExpression(na)) => {
                // Unwrap the value expression (name is used as alias)
                if let Some(val) = &na.value {
                    let inner = self.convert(val)?;
                    Ok(Expression::Alias(AliasExpression {
                        expr: Box::new(inner),
                        alias: na.key.clone(),
                    }))
                } else {
                    Err(ConnectError::PlanConversion("NamedArgument missing value".into()))
                }
            }
            Some(ExprType::MergeAction(_)) => {
                Err(ConnectError::Unsupported("MergeAction not supported".into()))
            }
            Some(ExprType::TypedAggregateExpression(_)) => {
                Err(ConnectError::Unsupported("TypedAggregateExpression not supported".into()))
            }
            Some(ExprType::SubqueryExpression(_)) => {
                Err(ConnectError::Unsupported("SubqueryExpression not supported".into()))
            }
            Some(ExprType::DirectShufflePartitionId(_)) => {
                Err(ConnectError::Unsupported("DirectShufflePartitionID not supported".into()))
            }
            Some(ExprType::Extension(_)) => {
                Err(ConnectError::Unsupported("Extension expression not supported".into()))
            }
        }
    }

    /// Convert a SortOrder expression (used by RelationConverter).
    pub fn convert_sort_order(
        &mut self,
        so: &proto::expression::SortOrder,
    ) -> Result<SortOrder> {
        use proto::expression::sort_order::{NullOrdering as ProtoNullOrdering, SortDirection as ProtoSortDir};
        let child = so
            .child
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("SortOrder missing child".into()))?;
        let expr = self.convert(child)?;
        let direction = match so.direction() {
            ProtoSortDir::Descending => SortDirection::Desc,
            _ => SortDirection::Asc,
        };
        let null_ordering = match so.null_ordering() {
            ProtoNullOrdering::SortNullsLast => CoreNullOrdering::NullsLast,
            _ => CoreNullOrdering::NullsFirst,
        };
        Ok(SortOrder { expr, direction, null_ordering })
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn convert_literal(&self, lit: &proto::expression::Literal) -> Result<Expression> {
        use proto::expression::literal::LiteralType;
        match &lit.literal_type {
            None => Ok(Literal::null()),
            Some(LiteralType::Null(_)) => Ok(Literal::null()),
            Some(LiteralType::Boolean(b)) => Ok(Literal::boolean(*b)),
            Some(LiteralType::Byte(v)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Byte(*v as i8),
                data_type: DataType::Byte,
            })),
            Some(LiteralType::Short(v)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Short(*v as i16),
                data_type: DataType::Short,
            })),
            Some(LiteralType::Integer(v)) => Ok(Literal::int(*v)),
            Some(LiteralType::Long(v)) => Ok(Literal::long(*v)),
            Some(LiteralType::Float(v)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Float(*v),
                data_type: DataType::Float,
            })),
            Some(LiteralType::Double(v)) => Ok(Literal::double(*v)),
            Some(LiteralType::String(s)) => Ok(Literal::string(s.clone())),
            Some(LiteralType::Binary(b)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Binary(b.clone()),
                data_type: DataType::Binary,
            })),
            Some(LiteralType::Date(d)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Date(*d),
                data_type: DataType::Date,
            })),
            Some(LiteralType::Timestamp(ts)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Timestamp(*ts),
                data_type: DataType::Timestamp,
            })),
            Some(LiteralType::TimestampNtz(ts)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::TimestampNtz(*ts),
                data_type: DataType::TimestampNtz,
            })),
            Some(LiteralType::Decimal(d)) => {
                let precision = d.precision.unwrap_or(38) as u8;
                let scale = d.scale.unwrap_or(18) as u8;
                Ok(Literal::decimal(d.value.clone(), precision, scale))
            }
            Some(LiteralType::YearMonthInterval(m)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Long(*m as i64),
                data_type: DataType::YearMonthInterval,
            })),
            Some(LiteralType::DayTimeInterval(micros)) => Ok(Expression::Literal(Literal {
                value: LiteralValue::Long(*micros),
                data_type: DataType::DayTimeInterval,
            })),
            Some(LiteralType::CalendarInterval(ci)) => {
                use thunderduck_core::expression::IntervalExpression;
                Ok(Expression::Interval(IntervalExpression {
                    months: ci.months,
                    days: ci.days,
                    microseconds: ci.microseconds,
                }))
            }
            Some(LiteralType::Array(_)) | Some(LiteralType::Map(_)) |
            Some(LiteralType::Struct(_)) | Some(LiteralType::SpecializedArray(_)) |
            Some(LiteralType::Time(_)) => {
                // Complex literal: fall back to null with appropriate type info
                Ok(Literal::null())
            }
        }
    }

    fn convert_unresolved_attribute(
        &self,
        attr: &proto::expression::UnresolvedAttribute,
    ) -> Result<Expression> {
        let name = &attr.unparsed_identifier;
        // Split dotted name on '.' to support qualifier.column
        let parts: Vec<&str> = name.splitn(2, '.').collect();
        if parts.len() == 2 {
            Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name: parts[1].to_string(),
                qualifier: Some(parts[0].to_string()),
            }))
        } else {
            Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name: name.clone(),
                qualifier: None,
            }))
        }
    }

    fn convert_unresolved_function(
        &mut self,
        func: &proto::expression::UnresolvedFunction,
    ) -> Result<Expression> {
        let args: Result<Vec<Expression>> =
            func.arguments.iter().map(|a| self.convert(a)).collect();
        let args = args?;

        // Binary operators sent as functions (e.g. col("x") > 5 → UnresolvedFunction(">", [x, 5]))
        if args.len() == 2 {
            let op = match func.function_name.as_str() {
                ">" => Some(BinaryOp::Gt),
                ">=" => Some(BinaryOp::GtEq),
                "<" => Some(BinaryOp::Lt),
                "<=" => Some(BinaryOp::LtEq),
                "=" | "==" => Some(BinaryOp::Eq),
                "!=" | "<>" => Some(BinaryOp::NotEq),
                "and" | "&&" => Some(BinaryOp::And),
                "or" | "||" => Some(BinaryOp::Or),
                "+" => Some(BinaryOp::Add),
                "-" => Some(BinaryOp::Sub),
                "*" => Some(BinaryOp::Mul),
                "/" => Some(BinaryOp::Div),
                "%" => Some(BinaryOp::Mod),
                _ => None,
            };
            if let Some(op) = op {
                let mut it = args.into_iter();
                let left = it.next().unwrap();
                let right = it.next().unwrap();
                return Ok(Expression::Binary(BinaryExpression {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                }));
            }
        }

        // Unary operators
        if args.len() == 1 {
            let op = match func.function_name.as_str() {
                "not" | "!" => Some(UnaryOp::Not),
                "-" => Some(UnaryOp::Negate),
                _ => None,
            };
            if let Some(op) = op {
                let operand = args.into_iter().next().unwrap();
                return Ok(Expression::Unary(UnaryExpression {
                    op,
                    operand: Box::new(operand),
                }));
            }
        }

        Ok(Expression::FunctionCall(FunctionCall {
            name: func.function_name.clone(),
            args,
            distinct: func.is_distinct,
        }))
    }

    fn convert_alias(&mut self, alias: &proto::expression::Alias) -> Result<Expression> {
        let inner = alias
            .expr
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Alias missing expr".into()))?;
        let expr = self.convert(inner)?;
        let name = alias
            .name
            .first()
            .cloned()
            .unwrap_or_else(|| "_col".to_string());
        Ok(Expression::Alias(AliasExpression { expr: Box::new(expr), alias: name }))
    }

    fn convert_cast(&mut self, cast: &proto::expression::Cast) -> Result<Expression> {
        use proto::expression::cast::CastToType;
        let inner = cast
            .expr
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Cast missing expr".into()))?;
        let expr = self.convert(inner)?;
        let to_type = match &cast.cast_to_type {
            Some(CastToType::Type(dt)) => proto_to_data_type(dt)?,
            Some(CastToType::TypeStr(_)) => DataType::Unresolved,
            None => DataType::Unresolved,
        };
        let try_cast = matches!(
            cast.eval_mode(),
            proto::expression::cast::EvalMode::Try
        );
        Ok(Expression::Cast(CastExpression {
            expr: Box::new(expr),
            to_type,
            try_cast,
        }))
    }

    fn convert_star(&self, star: &proto::expression::UnresolvedStar) -> Result<Expression> {
        let qualifier = star.unparsed_target.as_ref().map(|t| {
            // Strip trailing '.*' if present
            t.strip_suffix(".*").unwrap_or(t).to_string()
        });
        Ok(Expression::Star(StarExpression { qualifier }))
    }

    fn convert_window(&mut self, win: &proto::expression::Window) -> Result<Expression> {
        let func_expr = win
            .window_function
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Window missing function".into()))?;
        let func = self.convert(func_expr)?;

        let partition_by: Result<Vec<Expression>> =
            win.partition_spec.iter().map(|e| self.convert(e)).collect();
        let order_by: Result<Vec<SortOrder>> =
            win.order_spec.iter().map(|so| self.convert_sort_order(so)).collect();

        let frame = win.frame_spec.as_ref().map(|fs| self.convert_window_frame(fs)).transpose()?;

        Ok(Expression::Window(WindowFunction {
            func: Box::new(func),
            partition_by: partition_by?,
            order_by: order_by?,
            frame,
        }))
    }

    fn convert_window_frame(
        &mut self,
        fs: &proto::expression::window::WindowFrame,
    ) -> Result<WindowFrame> {
        use proto::expression::window::window_frame::FrameType;

        let unit = match fs.frame_type() {
            FrameType::Row => FrameUnit::Rows,
            _ => FrameUnit::Range,
        };

        let start = if let Some(lower) = &fs.lower {
            self.convert_frame_boundary(lower, true)?
        } else {
            FrameBoundary::UnboundedPreceding
        };
        let end = if let Some(upper) = &fs.upper {
            self.convert_frame_boundary(upper, false)?
        } else {
            FrameBoundary::UnboundedFollowing
        };

        Ok(WindowFrame { unit, start, end })
    }

    fn convert_frame_boundary(
        &mut self,
        fb: &proto::expression::window::window_frame::FrameBoundary,
        is_lower: bool,
    ) -> Result<FrameBoundary> {
        use proto::expression::window::window_frame::frame_boundary::Boundary;
        match &fb.boundary {
            None => Ok(if is_lower {
                FrameBoundary::UnboundedPreceding
            } else {
                FrameBoundary::UnboundedFollowing
            }),
            Some(Boundary::CurrentRow(true)) => Ok(FrameBoundary::CurrentRow),
            Some(Boundary::Unbounded(true)) => Ok(if is_lower {
                FrameBoundary::UnboundedPreceding
            } else {
                FrameBoundary::UnboundedFollowing
            }),
            Some(Boundary::Value(e)) => {
                let expr = self.convert(e)?;
                if is_lower {
                    Ok(FrameBoundary::Preceding(Box::new(expr)))
                } else {
                    Ok(FrameBoundary::Following(Box::new(expr)))
                }
            }
            // Default fallbacks
            Some(Boundary::CurrentRow(false)) | Some(Boundary::Unbounded(false)) => {
                Ok(FrameBoundary::CurrentRow)
            }
        }
    }

    fn convert_lambda(
        &mut self,
        lf: &proto::expression::LambdaFunction,
    ) -> Result<Expression> {
        // Collect param names and push a new scope
        let params: Vec<String> = lf
            .arguments
            .iter()
            .flat_map(|v| v.name_parts.clone())
            .collect();
        self.lambda_scopes.push(params.clone());

        let body_expr = lf
            .function
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("Lambda missing function body".into()))?;
        let body = self.convert(body_expr)?;

        self.lambda_scopes.pop();

        Ok(Expression::Lambda(LambdaExpression {
            params,
            body: Box::new(body),
        }))
    }

    fn convert_lambda_variable(
        &self,
        v: &proto::expression::UnresolvedNamedLambdaVariable,
    ) -> Result<Expression> {
        let name = v.name_parts.join(".");
        Ok(Expression::LambdaVariable(LambdaVariableExpression { name }))
    }

    fn convert_call_function(
        &mut self,
        cf: &proto::CallFunction,
    ) -> Result<Expression> {
        let args: Result<Vec<Expression>> =
            cf.arguments.iter().map(|a| self.convert(a)).collect();
        Ok(Expression::FunctionCall(FunctionCall {
            name: cf.function_name.clone(),
            args: args?,
            distinct: false,
        }))
    }

    fn convert_extract_value(
        &mut self,
        ev: &proto::expression::UnresolvedExtractValue,
    ) -> Result<Expression> {
        let child = ev
            .child
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("ExtractValue missing child".into()))?;
        let extraction = ev
            .extraction
            .as_ref()
            .ok_or_else(|| ConnectError::PlanConversion("ExtractValue missing extraction".into()))?;
        let child_expr = self.convert(child)?;
        let key_expr = self.convert(extraction)?;
        Ok(Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(child_expr),
            extraction: Box::new(key_expr),
        }))
    }
}
