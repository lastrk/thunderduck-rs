//! SparkSQL parser: SQL string → LogicalPlan.
//!
//! Entry point: `SparkSqlParser::parse(sql)`.
//! Returns `ThunderduckError::Unsupported` for constructs not yet implemented.
mod dialect;
mod sql_converter;

use crate::error::{Result, ThunderduckError};
use crate::logical::LogicalPlan;
use dialect::SparkDialect;

pub struct SparkSqlParser;

impl SparkSqlParser {
    /// Parse a Spark SQL string and return a typed `LogicalPlan`.
    pub fn parse(sql: &str) -> Result<LogicalPlan> {
        use sqlparser::parser::Parser;
        let dialect = SparkDialect::default();
        let mut stmts = Parser::parse_sql(&dialect, sql)
            .map_err(|e| ThunderduckError::Parse(e.to_string()))?;
        if stmts.len() != 1 {
            return Err(ThunderduckError::Unsupported(format!(
                "expected exactly one SQL statement, got {}",
                stmts.len()
            )));
        }
        sql_converter::SqlConverter::new().convert_statement(stmts.remove(0))
    }
}
