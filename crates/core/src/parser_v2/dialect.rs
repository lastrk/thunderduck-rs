//! Spark SQL dialect for sqlparser-rs.

use sqlparser::ast::{BinaryOperator, Expr};
use sqlparser::dialect::Dialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::{Parser, ParserError};

/// Spark SQL dialect for sqlparser-rs.
#[derive(Debug, Default)]
pub struct SparkDialect;

impl Dialect for SparkDialect {
    fn is_identifier_start(&self, ch: char) -> bool {
        ch.is_alphabetic() || ch == '_'
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_'
    }

    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        ch == '`' || ch == '"'
    }

    fn identifier_quote_style(&self, _identifier: &str) -> Option<char> {
        Some('`')
    }

    fn supports_group_by_expr(&self) -> bool {
        true
    }

    /// Spark 3.4+ supports `ORDER BY ALL` / `GROUP BY ALL`. Without this the
    /// parser treats `ALL` as a column named "all".
    fn supports_order_by_all(&self) -> bool {
        true
    }

    fn supports_lambda_functions(&self) -> bool {
        true
    }

    /// Allow `exists`, `struct`, `trim`, `interval` to be used as function
    /// names in SparkSQL. These are normally reserved but Spark uses them
    /// as built-in function identifiers.
    fn is_reserved_for_identifier(&self, kw: Keyword) -> bool {
        // We only keep INTERVAL as reserved (needed for INTERVAL literal syntax).
        // EXISTS, STRUCT, TRIM can be function names in Spark.
        matches!(kw, Keyword::INTERVAL)
    }

    /// Parse Spark's `a DIV b` integer-division operator (identical to MySQL).
    /// sqlparser-rs recognizes `DIV` as a keyword but does not register an
    /// infix parser by default — the MySQL dialect provides one and we
    /// replicate that behavior here. Corpus witness: `type-007`.
    fn parse_infix(
        &self,
        parser: &mut Parser,
        expr: &Expr,
        _precedence: u8,
    ) -> Option<Result<Expr, ParserError>> {
        if parser.parse_keyword(Keyword::DIV) {
            let right = match parser.parse_expr() {
                Ok(e) => e,
                Err(err) => return Some(Err(err)),
            };
            Some(Ok(Expr::BinaryOp {
                left: Box::new(expr.clone()),
                op: BinaryOperator::MyIntegerDivide,
                right: Box::new(right),
            }))
        } else {
            None
        }
    }
}
