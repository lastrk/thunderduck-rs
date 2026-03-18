use thunderduck_core::expression::Expression;
use thunderduck_core::logical::LogicalPlan;

use crate::converter::expression_converter::ExpressionConverter;
use crate::converter::relation_converter::RelationConverter;
use crate::error::Result;
use crate::proto::spark::connect as proto;

/// Stateless entry point for protobuf → AST conversion.
pub struct PlanConverter;

impl PlanConverter {
    /// Convert a proto Relation to a LogicalPlan.
    pub fn convert_relation(relation: &proto::Relation) -> Result<LogicalPlan> {
        let mut expr_conv = ExpressionConverter::new();
        let mut rel_conv = RelationConverter::new(&mut expr_conv);
        rel_conv.convert(relation)
    }

    /// Convert a proto Expression to a core Expression.
    #[allow(dead_code)]
    pub fn convert_expression(expr: &proto::Expression) -> Result<Expression> {
        let mut expr_conv = ExpressionConverter::new();
        expr_conv.convert(expr)
    }
}
