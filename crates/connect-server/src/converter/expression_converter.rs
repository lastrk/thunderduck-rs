use thunderduck_core::expression::{
    AliasExpression, ArrayLiteralExpression, BinaryExpression, BinaryOp, CastExpression,
    Expression, ExtractValueExpression, FrameBoundary, FrameUnit, FunctionCall, LambdaExpression,
    LambdaVariableExpression, Literal, LiteralValue, MapLiteralExpression,
    NullOrdering as CoreNullOrdering, RawSqlExpression, SortDirection, SortOrder, StarExpression,
    StructLiteralExpression, UnaryExpression, UnaryOp, UnresolvedColumn, UpdateFieldsExpression,
    WindowFrame, WindowFunction,
};
use thunderduck_core::types::DataType;

use crate::converter::type_converter::{parse_type_str, proto_to_data_type};
use crate::error::{ConnectError, Result};
use crate::proto::spark::connect as proto;

/// Converts proto Expression messages to the core Expression AST.
///
/// Carries a lambda scope stack for nested lambda handling.
#[derive(Default)]
pub struct ExpressionConverter {
    /// Stack of lambda scopes; each scope is a list of bound variable names.
    lambda_scopes: Vec<Vec<String>>,
}

impl ExpressionConverter {
    pub fn new() -> Self {
        Self::default()
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
                // Try to parse the expression string into a typed AST node so that
                // `data_type()` / `nullable()` work correctly (e.g. `size(name) → Integer`
                // instead of Unresolved). Fall back to RawSql on any parse error.
                Ok(
                    thunderduck_core::parser::SparkSqlParser::parse_single_expr(&es.expression)
                        .unwrap_or_else(|_| {
                            Expression::RawSql(RawSqlExpression {
                                sql: es.expression.clone(),
                                data_type: None,
                                nullable: None,
                            })
                        }),
                )
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
            Some(ExprType::UpdateFields(uf)) => self.convert_update_fields(uf),
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
                    Err(ConnectError::PlanConversion(
                        "NamedArgument missing value".into(),
                    ))
                }
            }
            Some(ExprType::MergeAction(_)) => Err(ConnectError::Unsupported(
                "MergeAction not supported".into(),
            )),
            Some(ExprType::TypedAggregateExpression(_)) => Err(ConnectError::Unsupported(
                "TypedAggregateExpression not supported".into(),
            )),
            Some(ExprType::SubqueryExpression(_)) => Err(ConnectError::Unsupported(
                "SubqueryExpression not supported".into(),
            )),
            Some(ExprType::DirectShufflePartitionId(_)) => Err(ConnectError::Unsupported(
                "DirectShufflePartitionID not supported".into(),
            )),
            Some(ExprType::Extension(_)) => Err(ConnectError::Unsupported(
                "Extension expression not supported".into(),
            )),
        }
    }

    /// Convert a SortOrder expression (used by RelationConverter).
    pub fn convert_sort_order(&mut self, so: &proto::expression::SortOrder) -> Result<SortOrder> {
        use proto::expression::sort_order::{
            NullOrdering as ProtoNullOrdering, SortDirection as ProtoSortDir,
        };
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
        Ok(SortOrder {
            expr,
            direction,
            null_ordering,
        })
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    pub fn convert_literal(&self, lit: &proto::expression::Literal) -> Result<Expression> {
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
            Some(LiteralType::Array(a)) => {
                let elements: Result<Vec<Expression>> =
                    a.elements.iter().map(|e| self.convert_literal(e)).collect();
                Ok(Expression::ArrayLiteral(ArrayLiteralExpression {
                    elements: elements?,
                    element_type: DataType::Unresolved,
                }))
            }
            Some(LiteralType::Map(m)) => {
                let keys: Result<Vec<Expression>> =
                    m.keys.iter().map(|e| self.convert_literal(e)).collect();
                let values: Result<Vec<Expression>> =
                    m.values.iter().map(|e| self.convert_literal(e)).collect();
                Ok(Expression::MapLiteral(MapLiteralExpression {
                    keys: keys?,
                    values: values?,
                    key_type: DataType::Unresolved,
                    value_type: DataType::Unresolved,
                }))
            }
            Some(LiteralType::Struct(s)) => {
                // Struct elements without field names — use positional names
                let fields: Result<Vec<(String, Expression)>> = s
                    .elements
                    .iter()
                    .enumerate()
                    .map(|(i, e)| self.convert_literal(e).map(|expr| (format!("_{i}"), expr)))
                    .collect();
                Ok(Expression::StructLiteral(StructLiteralExpression {
                    fields: fields?,
                }))
            }
            Some(LiteralType::SpecializedArray(sa)) => {
                use proto::expression::literal::specialized_array::ValueType;
                let elements: Vec<Expression> = match &sa.value_type {
                    Some(ValueType::Bools(b)) => b
                        .values
                        .iter()
                        .map(|v| Ok(Literal::boolean(*v)))
                        .collect::<Result<_>>()?,
                    Some(ValueType::Ints(iv)) => iv
                        .values
                        .iter()
                        .map(|v| Ok(Literal::int(*v)))
                        .collect::<Result<_>>()?,
                    Some(ValueType::Longs(lv)) => lv
                        .values
                        .iter()
                        .map(|v| Ok(Literal::long(*v)))
                        .collect::<Result<_>>()?,
                    Some(ValueType::Floats(fv)) => fv
                        .values
                        .iter()
                        .map(|v| {
                            Ok(Expression::Literal(Literal {
                                value: LiteralValue::Float(*v),
                                data_type: DataType::Float,
                            }))
                        })
                        .collect::<Result<_>>()?,
                    Some(ValueType::Doubles(dv)) => dv
                        .values
                        .iter()
                        .map(|v| Ok(Literal::double(*v)))
                        .collect::<Result<_>>()?,
                    Some(ValueType::Strings(sv)) => sv
                        .values
                        .iter()
                        .map(|v| Ok(Literal::string(v.clone())))
                        .collect::<Result<_>>()?,
                    None => vec![],
                };
                Ok(Expression::ArrayLiteral(ArrayLiteralExpression {
                    elements,
                    element_type: DataType::Unresolved,
                }))
            }
            Some(LiteralType::Time(_)) => {
                // Time literal not in core DataType — fall back to null
                Ok(Literal::null())
            }
        }
    }

    fn convert_unresolved_attribute(
        &self,
        attr: &proto::expression::UnresolvedAttribute,
    ) -> Result<Expression> {
        let name = &attr.unparsed_identifier;
        // "*" → Star expression (not a quoted column named asterisk)
        if name == "*" {
            return Ok(Expression::Star(StarExpression { qualifier: None }));
        }
        // plan_id takes priority over dot-qualifier: it precisely identifies which
        // DataFrame/plan a column belongs to, enabling correct join-side disambiguation.
        if let Some(plan_id) = attr.plan_id {
            // Use the last dot-separated part as the column name (e.g. "l1.l_suppkey" → "l_suppkey")
            let col_name = name.split('.').last().unwrap_or(name.as_str()).to_string();
            return Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name: col_name,
                qualifier: Some(format!("__plan_id_{plan_id}__")),
            }));
        }
        // Split dotted name on '.' to support qualifier.column (for SQL paths without plan_id)
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
                let mut args = args;
                let right = args.remove(1);
                let left = args.remove(0);
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
                let mut args = args;
                let operand = args.remove(0);
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
        Ok(Expression::Alias(AliasExpression {
            expr: Box::new(expr),
            alias: name,
        }))
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
            Some(CastToType::TypeStr(s)) => parse_type_str(s),
            None => DataType::Unresolved,
        };
        let try_cast = matches!(cast.eval_mode(), proto::expression::cast::EvalMode::Try);
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
        let order_by: Result<Vec<SortOrder>> = win
            .order_spec
            .iter()
            .map(|so| self.convert_sort_order(so))
            .collect();

        let frame = win
            .frame_spec
            .as_ref()
            .map(|fs| self.convert_window_frame(fs))
            .transpose()?;

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
                // Spark encodes frame offsets with sign: negative = preceding, positive = following, 0 = current row.
                // Extract the integer value if it's a literal to determine direction.
                let int_val = match &expr {
                    thunderduck_core::expression::Expression::Literal(l) => match &l.value {
                        LiteralValue::Int(n) => Some(*n as i64),
                        LiteralValue::Long(n) => Some(*n),
                        LiteralValue::Short(n) => Some(*n as i64),
                        LiteralValue::Byte(n) => Some(*n as i64),
                        _ => None,
                    },
                    _ => None,
                };
                match int_val {
                    Some(0) => Ok(FrameBoundary::CurrentRow),
                    Some(n) if n < 0 => Ok(FrameBoundary::Preceding(Box::new(Literal::long(-n)))),
                    Some(n) => Ok(FrameBoundary::Following(Box::new(Literal::long(n)))),
                    None => {
                        // Non-literal: fall back to position-based (is_lower → Preceding, else Following)
                        if is_lower {
                            Ok(FrameBoundary::Preceding(Box::new(expr)))
                        } else {
                            Ok(FrameBoundary::Following(Box::new(expr)))
                        }
                    }
                }
            }
            // Default fallbacks
            Some(Boundary::CurrentRow(false)) | Some(Boundary::Unbounded(false)) => {
                Ok(FrameBoundary::CurrentRow)
            }
        }
    }

    fn convert_lambda(&mut self, lf: &proto::expression::LambdaFunction) -> Result<Expression> {
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
        Ok(Expression::LambdaVariable(LambdaVariableExpression {
            name,
        }))
    }

    fn convert_call_function(&mut self, cf: &proto::CallFunction) -> Result<Expression> {
        let args: Result<Vec<Expression>> = cf.arguments.iter().map(|a| self.convert(a)).collect();
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
        let extraction = ev.extraction.as_ref().ok_or_else(|| {
            ConnectError::PlanConversion("ExtractValue missing extraction".into())
        })?;
        let child_expr = self.convert(child)?;
        let key_expr = self.convert(extraction)?;
        Ok(Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(child_expr),
            extraction: Box::new(key_expr),
        }))
    }

    fn convert_update_fields(
        &mut self,
        uf: &proto::expression::UpdateFields,
    ) -> Result<Expression> {
        let struct_expr = uf.struct_expression.as_ref().ok_or_else(|| {
            ConnectError::PlanConversion("UpdateFields missing struct_expression".into())
        })?;
        let struct_expr = self.convert(struct_expr)?;
        let value = uf
            .value_expression
            .as_ref()
            .map(|v| self.convert(v))
            .transpose()?;
        Ok(Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(struct_expr),
            field_name: uf.field_name.clone(),
            value: value.map(Box::new),
            struct_fields: None, // populated later by RelationConverter when struct type is known
        }))
    }
}
