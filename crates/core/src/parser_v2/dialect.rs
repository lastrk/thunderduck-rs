//! Spark SQL dialect for sqlparser-rs — τ's copy.
//!
//! Duplicated from `crate::parser::dialect` per Open Decision 1 Option 1b
//! (Slice A.2). Do not remove before Slice K deletes legacy — see ADR-022.
//!
//! **INV10:** duplication (rather than re-export) preserves the barrier at
//! the parser layer so `parser_v2/` remains structurally independent of the
//! legacy tree.

use sqlparser::dialect::Dialect;
use sqlparser::keywords::Keyword;

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
}
