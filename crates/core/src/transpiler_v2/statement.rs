//! τ's statement-level IR.
//!
//! [`SqlStatement`] distinguishes DDL/DML side-effects from pure queries.
//! The parser produces `SqlStatement`; the connect-server dispatch layer
//! inspects it to decide between the lazy-echo path (queries) and eager
//! execution (DDL).
//!
//! **INV10:** this module imports only τ-internal types — no `crate::runtime`.

use super::ast::CommonAst;
use super::emission::{quote_ident, render_data_type, render_expr};
use super::error::EmissionError;
use super::expression::Expression;
use crate::types::StructType;

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
/// Each variant carries typed parts extracted from the parser tree;
/// [`render_ddl`] builds DuckDB SQL from these parts.
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
    /// `CREATE TABLE <name> (<columns>) [IF NOT EXISTS]`.
    CreateTable {
        /// The table name.
        name: String,
        /// Whether `IF NOT EXISTS` was specified.
        if_not_exists: bool,
        /// The declared column schema.
        columns: StructType,
    },
    /// `DROP TABLE [IF EXISTS] <name>`.
    DropTable {
        /// The table name.
        name: String,
        /// Whether `IF EXISTS` was specified.
        if_exists: bool,
    },
    /// `DROP VIEW [IF EXISTS] <name>`.
    DropView {
        /// The view name.
        name: String,
        /// Whether `IF EXISTS` was specified.
        if_exists: bool,
    },
    /// `INSERT INTO <table> VALUES (...), (...)` — literal rows only.
    InsertValues {
        /// The target table name.
        table: String,
        /// Literal rows to insert.
        rows: Vec<Vec<Expression>>,
    },
    /// `INSERT INTO <table> SELECT ...` — body query lowered to [`CommonAst`].
    InsertSelect {
        /// The target table name.
        table: String,
        /// The body SELECT query.
        query: CommonAst,
    },
    /// `TRUNCATE TABLE <name>`.
    TruncateTable {
        /// The table name.
        name: String,
    },
    /// `CREATE VIEW <name> AS <select>` (non-temporary, non-replace).
    CreateView {
        /// The view name.
        name: String,
        /// Whether `OR REPLACE` was specified.
        or_replace: bool,
        /// The body query, lowered into a [`CommonAst`].
        query: CommonAst,
    },
}

/// Render a [`DdlStatement`] into DuckDB SQL.
///
/// For variants whose body is a `CommonAst` (e.g. `InsertSelect`,
/// `CreateTempView`, `CreateView`), the caller must pass the finalized body
/// SQL in `body_sql`. For self-contained variants (`CreateTable`, `DropTable`,
/// `DropView`, `InsertValues`, `TruncateTable`) `body_sql` is ignored.
pub fn render_ddl(stmt: &DdlStatement, body_sql: Option<&str>) -> Result<String, EmissionError> {
    match stmt {
        DdlStatement::CreateTempView {
            name, or_replace, ..
        } => {
            let body = body_sql.ok_or_else(|| EmissionError::Unsupported {
                kind: super::error::UnsupportedKind::ProtoShape,
                name: "ddl::create_temp_view::missing_body".to_owned(),
                reason: "body SQL is required for CREATE TEMP VIEW".to_owned(),
            })?;
            let replace = if *or_replace { " OR REPLACE" } else { "" };
            Ok(format!(
                "CREATE{replace} TEMP VIEW {name} AS {body}",
                name = quote_ident(name),
            ))
        }
        DdlStatement::CreateTable {
            name,
            if_not_exists,
            columns,
        } => {
            let ine = if *if_not_exists { " IF NOT EXISTS" } else { "" };
            let col_defs: Vec<String> = columns
                .fields
                .iter()
                .map(|f| {
                    format!(
                        "{} {}",
                        quote_ident(&f.name),
                        render_data_type(&f.data_type)
                    )
                })
                .collect();
            Ok(format!(
                "CREATE TABLE{ine} {name}({cols})",
                name = quote_ident(name),
                cols = col_defs.join(", "),
            ))
        }
        DdlStatement::DropTable { name, if_exists } => {
            let ie = if *if_exists { " IF EXISTS" } else { "" };
            Ok(format!("DROP TABLE{ie} {name}", name = quote_ident(name)))
        }
        DdlStatement::DropView { name, if_exists } => {
            let ie = if *if_exists { " IF EXISTS" } else { "" };
            Ok(format!("DROP VIEW{ie} {name}", name = quote_ident(name)))
        }
        DdlStatement::InsertValues { table, rows } => {
            let empty_schema = crate::transpiler_v2::Schema::default();
            let rendered_rows: Vec<String> = rows
                .iter()
                .map(|row| {
                    let cells: Result<Vec<String>, EmissionError> = row
                        .iter()
                        .map(|cell| render_expr(cell, &empty_schema))
                        .collect();
                    cells.map(|c| format!("({})", c.join(", ")))
                })
                .collect::<Result<_, _>>()?;
            Ok(format!(
                "INSERT INTO {table} VALUES {rows}",
                table = quote_ident(table),
                rows = rendered_rows.join(", "),
            ))
        }
        DdlStatement::InsertSelect { table, .. } => {
            let body = body_sql.ok_or_else(|| EmissionError::Unsupported {
                kind: super::error::UnsupportedKind::ProtoShape,
                name: "ddl::insert_select::missing_body".to_owned(),
                reason: "body SQL is required for INSERT INTO ... SELECT".to_owned(),
            })?;
            Ok(format!(
                "INSERT INTO {table} {body}",
                table = quote_ident(table),
            ))
        }
        DdlStatement::TruncateTable { name } => {
            Ok(format!("TRUNCATE TABLE {name}", name = quote_ident(name)))
        }
        DdlStatement::CreateView {
            name, or_replace, ..
        } => {
            let body = body_sql.ok_or_else(|| EmissionError::Unsupported {
                kind: super::error::UnsupportedKind::ProtoShape,
                name: "ddl::create_view::missing_body".to_owned(),
                reason: "body SQL is required for CREATE VIEW".to_owned(),
            })?;
            let replace = if *or_replace { " OR REPLACE" } else { "" };
            Ok(format!(
                "CREATE{replace} VIEW {name} AS {body}",
                name = quote_ident(name),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler_v2::expression::{Literal, LiteralValue};
    use crate::types::{DataType, StructField};

    #[test]
    fn render_create_table_basic() {
        let stmt = DdlStatement::CreateTable {
            name: "my_table".to_owned(),
            if_not_exists: false,
            columns: StructType::new(vec![
                StructField::nullable("id", DataType::Integer),
                StructField::nullable("name", DataType::String),
            ]),
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "CREATE TABLE my_table(id INTEGER, name VARCHAR)");
    }

    #[test]
    fn render_create_table_if_not_exists() {
        let stmt = DdlStatement::CreateTable {
            name: "t".to_owned(),
            if_not_exists: true,
            columns: StructType::new(vec![StructField::nullable("x", DataType::Long)]),
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "CREATE TABLE IF NOT EXISTS t(x BIGINT)");
    }

    #[test]
    fn render_drop_table_basic() {
        let stmt = DdlStatement::DropTable {
            name: "t".to_owned(),
            if_exists: false,
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "DROP TABLE t");
    }

    #[test]
    fn render_drop_table_if_exists() {
        let stmt = DdlStatement::DropTable {
            name: "t".to_owned(),
            if_exists: true,
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "DROP TABLE IF EXISTS t");
    }

    #[test]
    fn render_drop_view_basic() {
        let stmt = DdlStatement::DropView {
            name: "v".to_owned(),
            if_exists: false,
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "DROP VIEW v");
    }

    #[test]
    fn render_drop_view_if_exists() {
        let stmt = DdlStatement::DropView {
            name: "v".to_owned(),
            if_exists: true,
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "DROP VIEW IF EXISTS v");
    }

    #[test]
    fn render_insert_values() {
        let stmt = DdlStatement::InsertValues {
            table: "t".to_owned(),
            rows: vec![
                vec![
                    Expression::Literal(Literal {
                        value: LiteralValue::Int(1),
                        data_type: DataType::Integer,
                    }),
                    Expression::Literal(Literal {
                        value: LiteralValue::String("alice".to_owned()),
                        data_type: DataType::String,
                    }),
                ],
                vec![
                    Expression::Literal(Literal {
                        value: LiteralValue::Int(2),
                        data_type: DataType::Integer,
                    }),
                    Expression::Literal(Literal {
                        value: LiteralValue::String("bob".to_owned()),
                        data_type: DataType::String,
                    }),
                ],
            ],
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "INSERT INTO t VALUES (1, 'alice'), (2, 'bob')");
    }

    #[test]
    fn render_insert_select() {
        let stmt = DdlStatement::InsertSelect {
            table: "dst".to_owned(),
            query: CommonAst::new(crate::transpiler_v2::CommonOp::SingleRow),
        };
        let sql = render_ddl(&stmt, Some("SELECT * FROM src")).unwrap();
        assert_eq!(sql, "INSERT INTO dst SELECT * FROM src");
    }

    #[test]
    fn render_truncate_table() {
        let stmt = DdlStatement::TruncateTable {
            name: "t".to_owned(),
        };
        let sql = render_ddl(&stmt, None).unwrap();
        assert_eq!(sql, "TRUNCATE TABLE t");
    }

    #[test]
    fn render_create_view() {
        let stmt = DdlStatement::CreateView {
            name: "v".to_owned(),
            or_replace: false,
            query: CommonAst::new(crate::transpiler_v2::CommonOp::SingleRow),
        };
        let sql = render_ddl(&stmt, Some("SELECT 1 AS x")).unwrap();
        assert_eq!(sql, "CREATE VIEW v AS SELECT 1 AS x");
    }

    #[test]
    fn render_create_view_or_replace() {
        let stmt = DdlStatement::CreateView {
            name: "v".to_owned(),
            or_replace: true,
            query: CommonAst::new(crate::transpiler_v2::CommonOp::SingleRow),
        };
        let sql = render_ddl(&stmt, Some("SELECT 2 AS y")).unwrap();
        assert_eq!(sql, "CREATE OR REPLACE VIEW v AS SELECT 2 AS y");
    }
}
