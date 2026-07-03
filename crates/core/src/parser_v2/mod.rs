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
            CommonOp::Project {
                mut projections, ..
            } if projections.len() == 1 => Ok(projections.remove(0)),
            // Pass 71: when the fragment is a bare aggregate call
            // (e.g. `try_sum(lng)`, `every(active)`), `lower_select`
            // routes it through `lower_aggregate_select` and produces
            // `Aggregate { input: SingleRow, grouping: [], aggregates:
            // [<expr>], .. }`. Extract the single aggregate expression
            // — from the caller's perspective this is a scalar
            // aggregate expression, semantically identical to what
            // Spark hands us via `F.sum(...)`.
            CommonOp::Aggregate {
                input,
                grouping,
                mut aggregates,
                ..
            } if grouping.is_empty()
                && aggregates.len() == 1
                && matches!(input.op, CommonOp::SingleRow) =>
            {
                Ok(aggregates.remove(0))
            }
            _ => Err(EmissionError::UnsupportedProtoShape {
                shape: "ExpressionString::not_a_scalar".to_owned(),
                reason: format!("expression fragment did not parse as a single scalar: {expr_sql}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `SparkSqlParserV2::parse_expression` — the
    //! entry point used by `F.expr("...")` / `df.selectExpr("...")`
    //! via `ExpressionString`.

    use super::SparkSqlParserV2;
    use crate::transpiler_v2::Expression;

    /// Assert the parsed fragment resolves to a `FunctionCall` for `name`.
    fn assert_parses_as_function(expr_sql: &str, name: &str) {
        let parsed = SparkSqlParserV2::parse_expression(expr_sql)
            .unwrap_or_else(|e| panic!("parse_expression({expr_sql:?}) failed: {e:?}"));
        match parsed {
            Expression::FunctionCall(ref fc) => {
                assert!(
                    fc.name.eq_ignore_ascii_case(name),
                    "expected function {name:?}, got {:?} for {expr_sql:?}",
                    fc.name,
                );
            }
            other => panic!("expected FunctionCall({name}), got {other:?} for {expr_sql:?}"),
        }
    }

    #[test]
    fn parse_expression_scalar_arithmetic() {
        // Baseline: existing behavior — a non-aggregate scalar expression
        // continues to lower via `CommonOp::Project`.
        let parsed =
            SparkSqlParserV2::parse_expression("age + 1").expect("scalar arithmetic must parse");
        assert!(matches!(parsed, Expression::Binary(_)));
    }

    #[test]
    fn parse_expression_scalar_function_upper() {
        assert_parses_as_function("upper(name)", "upper");
    }

    // ── Pass 71: bare aggregate fragments ────────────────────────────────

    #[test]
    fn parse_expression_bare_try_sum() {
        // `F.expr("try_sum(lng)")` — Spark 4.x overflow-safe SUM.
        assert_parses_as_function("try_sum(lng)", "try_sum");
    }

    #[test]
    fn parse_expression_bare_try_avg() {
        // `F.expr("try_avg(a)")` — Spark 4.x overflow-safe AVG.
        assert_parses_as_function("try_avg(a)", "try_avg");
    }

    #[test]
    fn parse_expression_bare_every() {
        // `F.expr("every(active)")` — Spark boolean-all aggregate,
        // alias of `bool_and`.
        assert_parses_as_function("every(active)", "every");
    }

    #[test]
    fn parse_expression_bare_any_aggregate() {
        // `F.expr("any(active)")` — Spark boolean-any aggregate,
        // alias of `bool_or`. Confirms the sibling of `every` also
        // rides the same code path.
        assert_parses_as_function("any(active)", "any");
    }

    #[test]
    fn parse_expression_bare_any_value() {
        // `F.expr("any_value(name)")` — Spark's arbitrary-representative
        // aggregate. Same shape as the other bare aggregates.
        assert_parses_as_function("any_value(name)", "any_value");
    }

    #[test]
    fn parse_expression_bare_array_agg() {
        // `F.expr("array_agg(name)")` — Spark 4.x alias of `collect_list`.
        assert_parses_as_function("array_agg(name)", "array_agg");
    }

    #[test]
    fn parse_expression_bare_count_star() {
        // `F.expr("count(*)")` — the classic bare aggregate; make sure
        // the new `Aggregate` arm didn't regress the common case.
        let parsed = SparkSqlParserV2::parse_expression("count(*)").expect("count(*) must parse");
        match parsed {
            Expression::FunctionCall(ref fc) => {
                assert!(fc.name.eq_ignore_ascii_case("count"));
            }
            other => panic!("expected FunctionCall(count), got {other:?}"),
        }
    }

    #[test]
    fn parse_expression_bare_aggregate_with_alias() {
        // `F.expr("try_sum(lng) AS ts")` — an aliased bare aggregate
        // must still resolve. The alias is preserved by the SQL parser
        // as an `Expression::Alias` wrapping the aggregate call.
        let parsed = SparkSqlParserV2::parse_expression("try_sum(lng) AS ts")
            .expect("aliased bare aggregate must parse");
        match parsed {
            Expression::Alias(ref a) => {
                assert_eq!(a.alias, "ts");
                assert!(matches!(*a.expr, Expression::FunctionCall(_)));
            }
            other => panic!("expected Alias wrapping FunctionCall, got {other:?}"),
        }
    }
}
