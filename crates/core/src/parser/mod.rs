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
        // Pre-parse rewrite: expand Spark generator function syntax that sqlparser-rs
        // cannot handle. e.g. json_tuple(col, 'k1', 'k2') AS (name, age) →
        // json_extract_string(col, '$.k1') AS name, json_extract_string(col, '$.k2') AS age
        let sql_rewritten;
        let sql = if sql.to_ascii_lowercase().contains("json_tuple") {
            sql_rewritten = crate::generator::rewrite_json_tuple(sql);
            &sql_rewritten
        } else {
            sql
        };
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
