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
    /// Parse a single SQL expression fragment (e.g. `"size(name) as cnt"` or `"id"`).
    ///
    /// Wraps the fragment in `SELECT <fragment>` and extracts the first projection from the
    /// resulting plan. Returns `Err` if parsing or extraction fails.
    pub fn parse_single_expr(expr_sql: &str) -> Result<crate::expression::Expression> {
        let full_sql = format!("SELECT {expr_sql}");
        match Self::parse(&full_sql)? {
            LogicalPlan::Project(p) => p
                .projections
                .into_iter()
                .next()
                .ok_or_else(|| ThunderduckError::Parse("empty projection list".into())),
            _ => Err(ThunderduckError::Parse(
                "unexpected plan type from expression parse".into(),
            )),
        }
    }

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
