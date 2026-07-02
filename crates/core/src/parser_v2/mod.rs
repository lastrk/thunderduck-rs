//! τ's SparkSQL front-end — parses raw Spark SQL into [`CommonAst`].
//!
//! Owns the SQL text path in the τ substrate (Open Decision 1 Option 1b):
//! `V2RelationConverter` refuses `RelType::Sql` with
//! [`EmissionError::UnsupportedProtoShape`]; dispatch (Slice A.3) routes
//! `Sql` here instead.
//!
//! **INV10:** this file imports ONLY value-level types from `crate::types`
//! plus intra-τ modules. No `crate::parser`, `crate::logical`,
//! `crate::expression`, `crate::generator`.

mod dialect;
mod v2_lowering;

use crate::transpiler_v2::ast::CommonAst;
use crate::transpiler_v2::EmissionError;
use dialect::SparkDialect;

/// τ's public SparkSQL parser entry point.
pub struct SparkSqlParserV2;

impl SparkSqlParserV2 {
    /// Parse a raw Spark SQL string into a [`CommonAst`].
    ///
    /// Slice A.2 scope: `SELECT` queries with `FROM`, `WHERE`, `GROUP BY`,
    /// `ORDER BY`, `LIMIT/OFFSET`, joins, and subqueries in `FROM`.
    /// Everything else surfaces as
    /// [`EmissionError::UnsupportedProtoShape`].
    pub fn parse(sql: &str) -> Result<CommonAst, EmissionError> {
        use sqlparser::parser::Parser;
        let dialect = SparkDialect::default();
        // Slice A.2 fix pass (review M2): sqlparser errors are boundary
        // failures — the input never reached `CommonAst`, so the correct
        // category is `UnsupportedProtoShape` (input τ can't ingest), not
        // `UnsupportedOp` (emission arm not implemented).
        let mut stmts =
            Parser::parse_sql(&dialect, sql).map_err(|e| EmissionError::UnsupportedProtoShape {
                shape: "sql::parse_error".to_owned(),
                reason: e.to_string(),
            })?;
        if stmts.len() != 1 {
            return Err(EmissionError::UnsupportedProtoShape {
                shape: "sql::multi_statement".to_owned(),
                reason: format!("expected exactly one SQL statement, got {}", stmts.len()),
            });
        }
        v2_lowering::lower_statement(stmts.remove(0))
    }

    /// Parse a SparkSQL expression FRAGMENT (e.g. `age + 1`, `upper(name)`)
    /// into a single [`crate::transpiler_v2::Expression`]. Used by the
    /// protobuf front-end for `Expression::ExpressionString` — Spark's
    /// `F.expr("...")` / `df.selectExpr("...")`. Implemented by wrapping the
    /// fragment as `SELECT (<expr>) AS __td_expr` and extracting the single
    /// projection.
    pub fn parse_expression(
        expr_sql: &str,
    ) -> Result<crate::transpiler_v2::Expression, EmissionError> {
        // Spark's `F.expr("<x>")` / `selectExpr("<x>")` allows the fragment
        // to carry its own alias (e.g. `age + 1 as age1`), so we must NOT
        // wrap with our own `AS`. Simply parse `SELECT <fragment>` and take
        // the single projection — the alias, if any, is preserved by the
        // parser as an `Expression::Alias`.
        let wrapped = format!("SELECT {expr_sql}");
        let plan = Self::parse(&wrapped)?;
        use crate::transpiler_v2::ast::CommonOp;
        match plan.op {
            CommonOp::Project { mut projections, .. } if projections.len() == 1 => {
                Ok(projections.remove(0))
            }
            _ => Err(EmissionError::UnsupportedProtoShape {
                shape: "ExpressionString::not_a_scalar".to_owned(),
                reason: format!(
                    "expression fragment did not parse as a single scalar: {expr_sql}"
                ),
            }),
        }
    }
}
