//! τ's statement-level IR.
//!
//! [`SqlStatement`] distinguishes DDL/DML side-effects from pure queries.
//! The parser produces `SqlStatement`; the connect-server dispatch layer
//! inspects it to decide between the lazy-echo path (queries) and eager
//! execution (DDL).
//!
//! **INV10:** this module imports only τ-internal types — no `crate::runtime`.

use super::ast::CommonAst;

/// A parsed SQL statement — either a query or a DDL side-effect.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlStatement {
    /// A pure query (`SELECT ...`) — lowered into a [`CommonAst`].
    Query(CommonAst),
    /// A DDL statement that requires eager execution.
    Ddl(DdlStatement),
}

/// DDL statements that τ can lower from SparkSQL.
///
/// This enum grows as τ supports more DDL; this pass adds only
/// `CreateTempView`.
#[derive(Debug, Clone, PartialEq)]
pub enum DdlStatement {
    /// `CREATE [OR REPLACE] TEMP[ORARY] VIEW <name> AS <select>`.
    ///
    /// `IF NOT EXISTS` is not representable: Spark 4.1.1 rejects
    /// `IF NOT EXISTS` on any temporary view at parse time
    /// (`"It is not allowed to define a TEMPORARY view with IF NOT EXISTS."`).
    CreateTempView {
        /// The unqualified view name.
        name: String,
        /// Whether `OR REPLACE` was specified.
        or_replace: bool,
        /// The body query, lowered into a [`CommonAst`].
        query: CommonAst,
    },
}
