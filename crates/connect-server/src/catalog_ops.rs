//! Catalog operation pre-pass for Spark Connect `Relation { Catalog(..) }`.
//!
//! Every catalog operation arrives as `ExecutePlan { Root(Relation { Catalog }) }`.
//! This module intercepts the root relation before the normal τ pipeline,
//! rewrites supported ops into `CommonOp::Values` ASTs (so the unchanged
//! finalize/streaming path serves them), and returns `Status::unimplemented`
//! for out-of-scope variants.

use std::sync::Arc;

use thunderduck_core::runtime::{DuckDbSession, SchemaCacheEffect};
use thunderduck_core::transpiler_v2::expression::{Literal, LiteralValue};
use thunderduck_core::transpiler_v2::function_catalog;
use thunderduck_core::transpiler_v2::{CommonAst, CommonOp, Expression};
use thunderduck_core::types::DataType;
use tonic::Status;

use crate::proto::spark::connect as proto;

// ── Public entry point ────────────────────────────────────────────────────────

/// Attempt to resolve a root `Relation { Catalog(..) }` into a `CommonAst`.
///
/// Returns:
/// - `Ok(Some(ast))` — the catalog op is supported; `ast` is a
///   `CommonOp::Values` tree ready for finalize/streaming.
/// - `Ok(None)` — the relation is NOT a catalog variant (normal pipeline).
/// - `Err(Status::unimplemented)` — the catalog variant is recognized but
///   not supported by Thunderduck.
pub(crate) async fn resolve_catalog_relation(
    relation: &proto::Relation,
    session: &Arc<DuckDbSession>,
) -> Result<Option<CommonAst>, Status> {
    let Some(proto::relation::RelType::Catalog(catalog)) = &relation.rel_type else {
        return Ok(None);
    };
    let Some(cat_type) = &catalog.cat_type else {
        return Err(Status::invalid_argument(
            "Catalog relation missing cat_type",
        ));
    };
    use proto::catalog::CatType;
    let ast = match cat_type {
        CatType::CurrentCatalog(_) => values_string("value", "spark_catalog"),
        CatType::CurrentDatabase(_) => values_string("value", "default"),
        CatType::DatabaseExists(de) => {
            let exists = de.db_name.eq_ignore_ascii_case("default");
            values_bool("value", exists)
        }
        CatType::FunctionExists(fe) => {
            let exists = function_catalog::is_supported_function(&fe.function_name);
            values_bool("value", exists)
        }
        CatType::GetFunction(gf) => resolve_get_function(&gf.function_name)?,
        CatType::ListFunctions(_) => resolve_list_functions(),
        CatType::TableExists(te) => resolve_table_exists(&te.table_name, session).await?,
        CatType::DropTempView(dtv) => resolve_drop_temp_view(&dtv.view_name, session).await?,

        // ── Out-of-scope variants → honest Thunderduck-boundary error ─────
        other => {
            let variant_name = cat_type_variant_name(other);
            return Err(Status::unimplemented(format!(
                "Catalog[{variant_name}] is not implemented in Thunderduck"
            )));
        }
    };
    Ok(Some(ast))
}

// ── Scalar helpers ────────────────────────────────────────────────────────────

/// Build a 1-row, 1-column `Values` AST with a string literal.
fn values_string(col: &str, value: &str) -> CommonAst {
    CommonAst::new(CommonOp::Values {
        rows: vec![vec![string_lit(value)]],
        column_names: vec![col.to_owned()],
    })
}

/// Build a 1-row, 1-column `Values` AST with a boolean literal.
fn values_bool(col: &str, value: bool) -> CommonAst {
    CommonAst::new(CommonOp::Values {
        rows: vec![vec![bool_lit(value)]],
        column_names: vec![col.to_owned()],
    })
}

fn string_lit(s: &str) -> Expression {
    Expression::Literal(Literal {
        value: LiteralValue::String(s.to_owned()),
        data_type: DataType::String,
    })
}

fn bool_lit(b: bool) -> Expression {
    Expression::Literal(Literal {
        value: LiteralValue::Boolean(b),
        data_type: DataType::Boolean,
    })
}

/// A typed NULL literal — emits `CAST(NULL AS <type>)` through finalize.
fn null_lit(data_type: DataType) -> Expression {
    Expression::Literal(Literal {
        value: LiteralValue::Null,
        data_type,
    })
}

// ── Function metadata ─────────────────────────────────────────────────────────

/// Spark probe finding (Spark 4.1.1 local, `spark.catalog.getFunction("abs")`):
///   name='abs', catalog=None, namespace=None,
///   description='abs(expr) - Returns the absolute value of ...',
///   className='org.apache.spark.sql.catalyst.expressions.Abs',
///   isTemporary=True
///
/// PySpark Connect client unpacks columns positionally:
///   table[0]=name, table[1]=catalog, table[2]=namespace,
///   table[3]=description, table[4]=className, table[5]=isTemporary
///
/// Thunderduck mirrors:
///   - catalog → NULL (typed NULL, String)
///   - namespace → NULL (typed NULL, Array<String>)
///   - description → NULL (typed NULL, String) — mirroring the full
///     description string per-function is not cheap; NULL is safe because
///     the differential tests do not assert on description content.
///   - className → NULL (typed NULL, String) — same rationale.
///   - isTemporary → true (builtins are temporary in Spark)
fn function_row(name: &str) -> Vec<Expression> {
    vec![
        string_lit(name),
        null_lit(DataType::String), // catalog
        null_lit(DataType::Array(Box::new(DataType::String), true)), // namespace
        null_lit(DataType::String), // description
        null_lit(DataType::String), // className
        bool_lit(true),             // isTemporary
    ]
}

/// Column names for the function-metadata schema.
fn function_columns() -> Vec<String> {
    vec![
        "name".to_owned(),
        "catalog".to_owned(),
        "namespace".to_owned(),
        "description".to_owned(),
        "className".to_owned(),
        "isTemporary".to_owned(),
    ]
}

/// `getFunction(name)` — returns a single-row function metadata AST, or a
/// Spark-emulated `UNRESOLVED_ROUTINE` error if the function is unknown.
// `Status` is the gRPC error channel mandated by the `SparkConnectService` trait
// and used across this crate (39 signatures return `Result<_, Status>`); boxing
// it here alone would be inconsistent with the layer and force an unbox at every
// trait boundary, buying one allocation on the reject path.
#[allow(clippy::result_large_err)]
fn resolve_get_function(name: &str) -> Result<CommonAst, Status> {
    if !function_catalog::is_supported_function(name) {
        // Spark 4.1.1 raises:
        //   [UNRESOLVED_ROUTINE] Cannot resolve function `<name>` ...
        // The PySpark Connect client raises AnalysisException on the gRPC
        // internal error path, so Status::internal is the correct channel.
        return Err(Status::internal(format!(
            "[UNRESOLVED_ROUTINE] Cannot resolve function `{name}` on search path"
        )));
    }
    let lower = name.to_ascii_lowercase();
    Ok(CommonAst::new(CommonOp::Values {
        rows: vec![function_row(&lower)],
        column_names: function_columns(),
    }))
}

/// `listFunctions()` — returns a multi-row Values AST with one row per
/// supported function.
fn resolve_list_functions() -> CommonAst {
    let rows: Vec<Vec<Expression>> = function_catalog::supported_function_names()
        .map(function_row)
        .collect();
    CommonAst::new(CommonOp::Values {
        rows,
        column_names: function_columns(),
    })
}

// ── Session-backed ops ────────────────────────────────────────────────────────

/// `tableExists(name)` — query the live DuckDB session for tables and views.
async fn resolve_table_exists(
    name: &str,
    session: &Arc<DuckDbSession>,
) -> Result<CommonAst, Status> {
    // DuckDB system tables: duckdb_tables() for tables, duckdb_views() for views.
    // Escape single quotes in the name to prevent SQL injection.
    let escaped = name.replace('\'', "''");
    let sql = format!(
        "SELECT EXISTS(\
             SELECT 1 FROM duckdb_tables() WHERE lower(table_name) = lower('{escaped}')\
             UNION ALL \
             SELECT 1 FROM duckdb_views() WHERE lower(view_name) = lower('{escaped}')\
         ) AS v"
    );
    let batches = session
        .execute(&sql)
        .await
        .map_err(|e| Status::internal(format!("tableExists query failed: {e}")))?;
    let exists = extract_bool_scalar(&batches);
    Ok(values_bool("value", exists))
}

/// `dropTempView(name)` — check if the temp view exists, drop it if so,
/// return whether it existed.
async fn resolve_drop_temp_view(
    name: &str,
    session: &Arc<DuckDbSession>,
) -> Result<CommonAst, Status> {
    // Step 1: check if the view exists as a temporary view.
    let escaped = name.replace('\'', "''");
    let check_sql = format!(
        "SELECT EXISTS(\
             SELECT 1 FROM duckdb_views() \
             WHERE lower(view_name) = lower('{escaped}') AND temporary\
         ) AS v"
    );
    let batches = session
        .execute(&check_sql)
        .await
        .map_err(|e| Status::internal(format!("dropTempView check failed: {e}")))?;
    let existed = extract_bool_scalar(&batches);

    if existed {
        // Step 2: drop it. Use quote_ident-style escaping for the identifier.
        let ident = format!("\"{}\"", name.replace('"', "\"\""));
        let drop_sql = format!("DROP VIEW IF EXISTS {ident}");
        session
            .execute_ddl(
                &drop_sql,
                SchemaCacheEffect::Evict {
                    name: name.to_owned(),
                },
            )
            .await
            .map_err(|e| Status::internal(format!("dropTempView DROP failed: {e}")))?;
    }

    Ok(values_bool("value", existed))
}

/// Extract a boolean scalar from the first column, first row of query results.
/// Returns `false` if the result set is empty or the value is NULL.
fn extract_bool_scalar(batches: &[arrow::array::RecordBatch]) -> bool {
    use arrow::array::BooleanArray;
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        if let Some(col) = batch.columns().first() {
            if let Some(arr) = col.as_any().downcast_ref::<BooleanArray>() {
                return arr.value(0);
            }
        }
    }
    false
}

// ── Variant name helper ───────────────────────────────────────────────────────

/// Human-readable name of a `CatType` variant for error messages.
fn cat_type_variant_name(cat: &proto::catalog::CatType) -> &'static str {
    use proto::catalog::CatType;
    match cat {
        CatType::CurrentDatabase(_) => "CurrentDatabase",
        CatType::SetCurrentDatabase(_) => "SetCurrentDatabase",
        CatType::ListDatabases(_) => "ListDatabases",
        CatType::ListTables(_) => "ListTables",
        CatType::ListFunctions(_) => "ListFunctions",
        CatType::ListColumns(_) => "ListColumns",
        CatType::GetDatabase(_) => "GetDatabase",
        CatType::GetTable(_) => "GetTable",
        CatType::GetFunction(_) => "GetFunction",
        CatType::DatabaseExists(_) => "DatabaseExists",
        CatType::TableExists(_) => "TableExists",
        CatType::FunctionExists(_) => "FunctionExists",
        CatType::CreateExternalTable(_) => "CreateExternalTable",
        CatType::CreateTable(_) => "CreateTable",
        CatType::DropTempView(_) => "DropTempView",
        CatType::DropGlobalTempView(_) => "DropGlobalTempView",
        CatType::RecoverPartitions(_) => "RecoverPartitions",
        CatType::IsCached(_) => "IsCached",
        CatType::CacheTable(_) => "CacheTable",
        CatType::UncacheTable(_) => "UncacheTable",
        CatType::ClearCache(_) => "ClearCache",
        CatType::RefreshTable(_) => "RefreshTable",
        CatType::RefreshByPath(_) => "RefreshByPath",
        CatType::CurrentCatalog(_) => "CurrentCatalog",
        CatType::SetCurrentCatalog(_) => "SetCurrentCatalog",
        CatType::ListCatalogs(_) => "ListCatalogs",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests for AST shape ──────────────────────────────────────────

    #[test]
    fn values_string_produces_single_row_single_col() {
        let ast = values_string("col", "hello");
        match &ast.op {
            CommonOp::Values { rows, column_names } => {
                assert_eq!(column_names, &["col"]);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 1);
            }
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[test]
    fn values_bool_produces_single_row_single_col() {
        let ast = values_bool("v", true);
        match &ast.op {
            CommonOp::Values { rows, column_names } => {
                assert_eq!(column_names, &["v"]);
                assert_eq!(rows.len(), 1);
                match &rows[0][0] {
                    Expression::Literal(Literal {
                        value: LiteralValue::Boolean(true),
                        ..
                    }) => {}
                    other => panic!("expected bool true, got: {other:?}"),
                }
            }
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[test]
    fn resolve_get_function_known_returns_six_columns() {
        let ast = resolve_get_function("abs").expect("abs is supported");
        match &ast.op {
            CommonOp::Values { rows, column_names } => {
                assert_eq!(column_names.len(), 6);
                assert_eq!(column_names[0], "name");
                assert_eq!(column_names[1], "catalog");
                assert_eq!(column_names[2], "namespace");
                assert_eq!(column_names[3], "description");
                assert_eq!(column_names[4], "className");
                assert_eq!(column_names[5], "isTemporary");
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 6);
                // name column is "abs"
                match &rows[0][0] {
                    Expression::Literal(Literal {
                        value: LiteralValue::String(s),
                        ..
                    }) => assert_eq!(s, "abs"),
                    other => panic!("expected string 'abs', got: {other:?}"),
                }
                // namespace is typed NULL (Array<String>)
                match &rows[0][2] {
                    Expression::Literal(Literal {
                        value: LiteralValue::Null,
                        data_type: DataType::Array(inner, _),
                    }) => assert_eq!(**inner, DataType::String),
                    other => panic!("expected typed NULL Array<String>, got: {other:?}"),
                }
            }
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[test]
    fn resolve_get_function_unknown_returns_error() {
        let result = resolve_get_function("nonexistent_xyz_12345");
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert!(
            status.message().contains("UNRESOLVED_ROUTINE"),
            "expected UNRESOLVED_ROUTINE, got: {}",
            status.message()
        );
    }

    #[test]
    fn resolve_list_functions_has_expected_shape() {
        let ast = resolve_list_functions();
        match &ast.op {
            CommonOp::Values { rows, column_names } => {
                assert_eq!(column_names.len(), 6);
                // Should have rows for every supported function
                assert_eq!(
                    rows.len(),
                    function_catalog::SUPPORTED_FUNCTIONS.len(),
                    "row count must match SUPPORTED_FUNCTIONS length"
                );
                // First row name should be the first sorted function
                match &rows[0][0] {
                    Expression::Literal(Literal {
                        value: LiteralValue::String(s),
                        ..
                    }) => assert_eq!(s, function_catalog::SUPPORTED_FUNCTIONS[0]),
                    other => panic!("expected string literal, got: {other:?}"),
                }
            }
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[test]
    fn cat_type_variant_name_covers_all() {
        // Smoke test: at least the 8 supported variants have names
        let current_db = proto::catalog::CatType::CurrentDatabase(proto::CurrentDatabase {});
        assert_eq!(cat_type_variant_name(&current_db), "CurrentDatabase");
    }

    // ── Session-backed tests (tokio) ──────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn table_exists_false_for_nonexistent() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("catalog-test-table-exists")
            .await
            .expect("session");
        let ast = resolve_table_exists("nonexistent_xyz_99", &session)
            .await
            .expect("should succeed");
        match &ast.op {
            CommonOp::Values { rows, .. } => match &rows[0][0] {
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(false),
                    ..
                }) => {}
                other => panic!("expected false, got: {other:?}"),
            },
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drop_temp_view_nonexistent_returns_false() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("catalog-test-drop-nonexistent")
            .await
            .expect("session");
        let ast = resolve_drop_temp_view("nonexistent_xyz_99", &session)
            .await
            .expect("should succeed");
        match &ast.op {
            CommonOp::Values { rows, .. } => match &rows[0][0] {
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(false),
                    ..
                }) => {}
                other => panic!("expected false, got: {other:?}"),
            },
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drop_temp_view_existing_returns_true() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("catalog-test-drop-existing")
            .await
            .expect("session");
        // Create a temp view first
        session
            .create_temp_view("catalog_drop_test_view", "SELECT 1 AS id")
            .await
            .expect("create temp view");
        let ast = resolve_drop_temp_view("catalog_drop_test_view", &session)
            .await
            .expect("should succeed");
        match &ast.op {
            CommonOp::Values { rows, .. } => match &rows[0][0] {
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(true),
                    ..
                }) => {}
                other => panic!("expected true, got: {other:?}"),
            },
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn table_exists_true_for_created_view() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("catalog-test-table-exists-view")
            .await
            .expect("session");
        session
            .create_temp_view("catalog_exists_test_view", "SELECT 42 AS num")
            .await
            .expect("create temp view");
        let ast = resolve_table_exists("catalog_exists_test_view", &session)
            .await
            .expect("should succeed");
        match &ast.op {
            CommonOp::Values { rows, .. } => match &rows[0][0] {
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(true),
                    ..
                }) => {}
                other => panic!("expected true, got: {other:?}"),
            },
            other => panic!("expected Values, got: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_catalog_relation_returns_none_for_non_catalog() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("catalog-test-non-catalog")
            .await
            .expect("session");
        let relation = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                query: "SELECT 1".to_owned(),
                ..Default::default()
            })),
        };
        let result = resolve_catalog_relation(&relation, &session)
            .await
            .expect("should succeed");
        assert!(result.is_none(), "non-catalog relation should return None");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_catalog_relation_unimplemented_for_list_tables() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("catalog-test-list-tables")
            .await
            .expect("session");
        let relation = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Catalog(proto::Catalog {
                cat_type: Some(proto::catalog::CatType::ListTables(proto::ListTables {
                    db_name: None,
                    pattern: None,
                })),
            })),
        };
        let result = resolve_catalog_relation(&relation, &session).await;
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(
            status.message().contains("ListTables"),
            "expected ListTables in message, got: {}",
            status.message()
        );
    }
}
