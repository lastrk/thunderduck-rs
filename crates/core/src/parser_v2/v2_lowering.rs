//! sqlparser-rs AST → τ [`CommonAst`] lowering.
//!
//! Scope (per architecture plan §4):
//! - `SELECT expr, … FROM table WHERE … GROUP BY … ORDER BY … LIMIT n OFFSET m`
//! - bare `SELECT literal`
//! - `SELECT … FROM (VALUES ...)` and other subquery-in-FROM forms
//! - basic joins (INNER / LEFT / RIGHT / FULL / CROSS / LEFT SEMI / LEFT ANTI)
//! - `SELECT *`
//!
//! Deferred (surface as [`EmissionError::Unsupported`] with `ProtoShape` kind):
//! PIVOT, GROUPING SETS, ROLLUP, CUBE, LATERAL VIEW, TABLESAMPLE, CTE,
//! UNION/INTERSECT/EXCEPT, window functions, HOFs, `json_tuple` rewrites,
//! command statements.
//!
//! **INV10:** imports only value-level types from `crate::types` plus
//! intra-τ modules. No `crate::parser`, `crate::logical`, `crate::expression`.
//!
//! **Plan-id policy (Open Decision 12):** every [`UnresolvedColumn`] emitted
//! by this module has `plan_id = None`.

use std::collections::HashMap;
use std::convert::Infallible;

use sqlparser::ast::{
    AccessExpr, BinaryOperator, CastKind, CeilFloorKind, DataType as SqlDataType, DateTimeField,
    Distinct, DuplicateTreatment, ExactNumberInfo, Expr, ExprWithAlias, Function, FunctionArg,
    FunctionArgExpr, FunctionArgumentList, FunctionArguments, GroupByExpr, GroupByWithModifier,
    Interval, JoinConstraint, JoinOperator, LateralView, LimitClause, NamedWindowDefinition,
    NamedWindowExpr, NullInclusion, ObjectName, ObjectNamePart, OrderByExpr, OrderByKind,
    OrderByOptions, PivotValueSource, Query, Select, SelectItem, SetExpr, SetOperator,
    SetQuantifier, Statement, Subscript, TableAlias, TableFactor, TableWithJoins, TrimWhereField,
    TypedString, UnaryOperator, Value, ValueWithSpan, WindowFrame as SqlWindowFrame,
    WindowFrameBound, WindowFrameUnits, WindowSpec, WindowType,
};

use crate::bail_boundary_proto;
use crate::transpiler_v2::ast::{
    CommonAst, CommonOp, FileFormat, GroupingKind, JoinType, PivotGrouping, SetOpKind, UnpivotIds,
};
use crate::transpiler_v2::error::UnsupportedKind;
use crate::transpiler_v2::expression::{
    decimal_value_precision_scale, AliasExpression, BetweenExpression, BinaryExpression, BinaryOp,
    CaseWhenExpression, CastExpression, ExistsSubquery, Expression, ExtractValueExpression,
    FrameBoundary, FrameUnit, FunctionCall, InListExpression, InSubquery, IntervalExpression,
    IntervalKind, IsDistinctFromExpression, LambdaExpression, LambdaVariableExpression,
    LikeExpression, Literal, LiteralValue, NullOrdering, ScalarSubquery, SortDirection, SortOrder,
    StarExpression, SubqueryPlan, UnaryExpression, UnaryOp, UnresolvedColumn, WindowFrame,
    WindowFunction,
};
use crate::transpiler_v2::macros::ProtoFieldExt;
use crate::transpiler_v2::type_inference::is_aggregate_classifier_name;
use crate::transpiler_v2::EmissionError;
use crate::types::DataType;

/// Immutable CTE scope: lowercased CTE name → its already-lowered body.
///
/// Threaded through the query-body lowering chain so that a `FROM <cte>`
/// reference inlines the CTE body (ADR-004 — no new `CommonOp`) instead of a
/// catalog `TableScan`. Bodies are lowered once, eagerly, in `cte_tables`
/// order (each seeing its predecessors), and cloned per reference.
type CteScope = HashMap<String, CommonAst>;

/// Lower a parsed sqlparser [`Statement`] into a [`CommonAst`].
pub fn lower_statement(stmt: Statement) -> Result<CommonAst, EmissionError> {
    match stmt {
        Statement::Query(q) => lower_query(*q, &CteScope::new()),
        other => bail_boundary_proto!(
            format!("sql::{}", statement_kind(&other)),
            "parser_v2 only supports SELECT queries in τ"
        ),
    }
}

/// Lower a parsed sqlparser [`Statement`] into a [`SqlStatement`].
///
/// Queries lower exactly as [`lower_statement`]. `CREATE [OR REPLACE]
/// TEMP[ORARY] VIEW` lowers to [`DdlStatement::CreateTempView`]. Every
/// other statement kind surfaces as a Thunderduck-boundary error.
pub fn lower_statement_or_ddl(
    stmt: Statement,
) -> Result<crate::transpiler_v2::SqlStatement, EmissionError> {
    use crate::transpiler_v2::statement::{DdlStatement, SqlStatement};
    use crate::types::{StructField, StructType};
    use sqlparser::ast::{CreateTable, CreateView, Insert, ObjectType, TableObject, Truncate};

    match stmt {
        Statement::Query(q) => {
            let ast = lower_query(*q, &CteScope::new())?;
            Ok(SqlStatement::Query(ast))
        }
        // ── CREATE [OR REPLACE] TEMP[ORARY] VIEW ──────────────────────────
        Statement::CreateView(CreateView {
            temporary: true,
            name,
            query,
            or_replace,
            if_not_exists,
            columns,
            comment,
            options,
            cluster_by,
            with_no_schema_binding,
            materialized,
            to,
            params,
            or_alter,
            secure,
            name_before_not_exists: _,
        }) => {
            // Spark-emulated parse errors (match Spark 4.1.1 ParseException
            // wording exactly). These fire before unsupported-clause guards
            // because Spark itself rejects these at parse time.
            if or_replace && if_not_exists {
                return Err(EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name: "sql::parse_error".to_owned(),
                    reason: "CREATE VIEW with both IF NOT EXISTS and REPLACE \
                             is not allowed."
                        .to_owned(),
                });
            }
            if if_not_exists {
                return Err(EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name: "sql::parse_error".to_owned(),
                    reason: "It is not allowed to define a TEMPORARY view \
                             with IF NOT EXISTS."
                        .to_owned(),
                });
            }
            reject_unsupported_view_clauses(
                &columns,
                &comment,
                &options,
                &cluster_by,
                with_no_schema_binding,
                materialized,
                &to,
                &params,
                or_alter,
                secure,
            )?;
            let view_name = extract_simple_name(&name, "sql::create_view::name")?;
            let body_ast = lower_query(*query, &CteScope::new())?;
            Ok(SqlStatement::Ddl(DdlStatement::CreateTempView {
                name: view_name,
                or_replace,
                query: body_ast,
            }))
        }

        // ── Non-temporary CREATE VIEW ─────────────────────────────────────
        Statement::CreateView(ref cv) if cv.or_replace && cv.if_not_exists => {
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: "sql::parse_error".to_owned(),
                reason: "CREATE VIEW with both IF NOT EXISTS and REPLACE \
                         is not allowed."
                    .to_owned(),
            })
        }
        Statement::CreateView(CreateView {
            temporary: false,
            name,
            query,
            or_replace,
            if_not_exists: _,
            columns,
            comment,
            options,
            cluster_by,
            with_no_schema_binding,
            materialized,
            to,
            params,
            or_alter,
            secure,
            name_before_not_exists: _,
        }) => {
            reject_unsupported_view_clauses(
                &columns,
                &comment,
                &options,
                &cluster_by,
                with_no_schema_binding,
                materialized,
                &to,
                &params,
                or_alter,
                secure,
            )?;
            let view_name = extract_simple_name(&name, "sql::create_view::name")?;
            let body_ast = lower_query(*query, &CteScope::new())?;
            Ok(SqlStatement::Ddl(DdlStatement::CreateView {
                name: view_name,
                or_replace,
                query: body_ast,
            }))
        }

        // ── CREATE TABLE ──────────────────────────────────────────────────
        Statement::CreateTable(CreateTable {
            name,
            columns,
            if_not_exists,
            constraints,
            query,
            or_replace,
            temporary,
            external,
            dynamic,
            global,
            transient,
            volatile,
            iceberg,
            hive_distribution: _,
            hive_formats: _,
            table_options,
            file_format,
            location,
            without_rowid,
            like,
            clone,
            version: _,
            comment,
            on_commit,
            on_cluster,
            primary_key,
            order_by,
            partition_by,
            cluster_by,
            clustered_by,
            inherits,
            partition_of,
            for_values,
            strict,
            copy_grants,
            enable_schema_evolution,
            change_tracking,
            data_retention_time_in_days,
            max_data_extension_time_in_days,
            default_ddl_collation,
            with_aggregation_policy,
            with_row_access_policy,
            with_tags,
            external_volume,
            base_location,
            catalog,
            catalog_sync,
            storage_serialization_policy,
            target_lag,
            warehouse,
            refresh_mode,
            initialize,
            require_user,
        }) => {
            // Bail loudly on unsupported clauses.
            if or_replace {
                bail_boundary_proto!(
                    "sql::create_table::or_replace",
                    "CREATE OR REPLACE TABLE is not implemented in τ"
                );
            }
            if temporary {
                bail_boundary_proto!(
                    "sql::create_table::temporary",
                    "CREATE TEMPORARY TABLE is not supported — Spark 4.1.1 \
                     does not allow CREATE TEMPORARY TABLE"
                );
            }
            if external {
                bail_boundary_proto!(
                    "sql::create_table::external",
                    "CREATE EXTERNAL TABLE is not implemented in τ"
                );
            }
            if dynamic {
                bail_boundary_proto!(
                    "sql::create_table::dynamic",
                    "CREATE DYNAMIC TABLE is not implemented in τ"
                );
            }
            if global.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::global",
                    "GLOBAL clause on CREATE TABLE is not implemented in τ"
                );
            }
            if transient {
                bail_boundary_proto!(
                    "sql::create_table::transient",
                    "CREATE TRANSIENT TABLE is not implemented in τ"
                );
            }
            if volatile {
                bail_boundary_proto!(
                    "sql::create_table::volatile",
                    "CREATE VOLATILE TABLE is not implemented in τ"
                );
            }
            if iceberg {
                bail_boundary_proto!(
                    "sql::create_table::iceberg",
                    "CREATE ICEBERG TABLE is not implemented in τ"
                );
            }
            if !constraints.is_empty() {
                bail_boundary_proto!(
                    "sql::create_table::constraints",
                    "table constraints on CREATE TABLE are not implemented in τ"
                );
            }
            if query.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::ctas",
                    "CREATE TABLE AS SELECT (CTAS) is not implemented in τ"
                );
            }
            if !matches!(table_options, sqlparser::ast::CreateTableOptions::None) {
                bail_boundary_proto!(
                    "sql::create_table::options",
                    "OPTIONS / USING / TBLPROPERTIES on CREATE TABLE is not implemented in τ"
                );
            }
            if file_format.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::file_format",
                    "STORED AS on CREATE TABLE is not implemented in τ"
                );
            }
            if location.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::location",
                    "LOCATION on CREATE TABLE is not implemented in τ"
                );
            }
            if without_rowid {
                bail_boundary_proto!(
                    "sql::create_table::without_rowid",
                    "WITHOUT ROWID is not implemented in τ"
                );
            }
            if like.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::like",
                    "LIKE on CREATE TABLE is not implemented in τ"
                );
            }
            if clone.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::clone",
                    "CLONE on CREATE TABLE is not implemented in τ"
                );
            }
            if comment.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::comment",
                    "COMMENT on CREATE TABLE is not implemented in τ"
                );
            }
            if on_commit.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::on_commit",
                    "ON COMMIT on CREATE TABLE is not implemented in τ"
                );
            }
            if on_cluster.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::on_cluster",
                    "ON CLUSTER on CREATE TABLE is not implemented in τ"
                );
            }
            if primary_key.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::primary_key",
                    "PRIMARY KEY on CREATE TABLE is not implemented in τ"
                );
            }
            if order_by.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::order_by",
                    "ORDER BY on CREATE TABLE is not implemented in τ"
                );
            }
            if partition_by.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::partition_by",
                    "PARTITION BY on CREATE TABLE is not implemented in τ"
                );
            }
            if cluster_by.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::cluster_by",
                    "CLUSTER BY on CREATE TABLE is not implemented in τ"
                );
            }
            if clustered_by.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::clustered_by",
                    "CLUSTERED BY on CREATE TABLE is not implemented in τ"
                );
            }
            if inherits.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::inherits",
                    "INHERITS on CREATE TABLE is not implemented in τ"
                );
            }
            if partition_of.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::partition_of",
                    "PARTITION OF on CREATE TABLE is not implemented in τ"
                );
            }
            if for_values.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::for_values",
                    "FOR VALUES on CREATE TABLE is not implemented in τ"
                );
            }
            if strict {
                bail_boundary_proto!(
                    "sql::create_table::strict",
                    "STRICT on CREATE TABLE is not implemented in τ"
                );
            }
            if copy_grants {
                bail_boundary_proto!(
                    "sql::create_table::copy_grants",
                    "COPY GRANTS on CREATE TABLE is not implemented in τ"
                );
            }
            if enable_schema_evolution.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::enable_schema_evolution",
                    "ENABLE_SCHEMA_EVOLUTION on CREATE TABLE is not implemented in τ"
                );
            }
            if change_tracking.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::change_tracking",
                    "CHANGE_TRACKING on CREATE TABLE is not implemented in τ"
                );
            }
            if data_retention_time_in_days.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::data_retention",
                    "DATA_RETENTION_TIME_IN_DAYS on CREATE TABLE is not implemented in τ"
                );
            }
            if max_data_extension_time_in_days.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::max_data_extension",
                    "MAX_DATA_EXTENSION_TIME_IN_DAYS on CREATE TABLE is not implemented in τ"
                );
            }
            if default_ddl_collation.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::default_ddl_collation",
                    "DEFAULT_DDL_COLLATION on CREATE TABLE is not implemented in τ"
                );
            }
            if with_aggregation_policy.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::aggregation_policy",
                    "WITH AGGREGATION POLICY on CREATE TABLE is not implemented in τ"
                );
            }
            if with_row_access_policy.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::row_access_policy",
                    "WITH ROW ACCESS POLICY on CREATE TABLE is not implemented in τ"
                );
            }
            if with_tags.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::with_tags",
                    "WITH TAG on CREATE TABLE is not implemented in τ"
                );
            }
            if external_volume.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::external_volume",
                    "EXTERNAL_VOLUME on CREATE TABLE is not implemented in τ"
                );
            }
            if base_location.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::base_location",
                    "BASE_LOCATION on CREATE TABLE is not implemented in τ"
                );
            }
            if catalog.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::catalog",
                    "CATALOG on CREATE TABLE is not implemented in τ"
                );
            }
            if catalog_sync.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::catalog_sync",
                    "CATALOG_SYNC on CREATE TABLE is not implemented in τ"
                );
            }
            if storage_serialization_policy.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::storage_serialization_policy",
                    "STORAGE_SERIALIZATION_POLICY on CREATE TABLE is not implemented in τ"
                );
            }
            if target_lag.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::target_lag",
                    "TARGET_LAG on CREATE TABLE is not implemented in τ"
                );
            }
            if warehouse.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::warehouse",
                    "WAREHOUSE on CREATE TABLE is not implemented in τ"
                );
            }
            if refresh_mode.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::refresh_mode",
                    "REFRESH_MODE on CREATE TABLE is not implemented in τ"
                );
            }
            if initialize.is_some() {
                bail_boundary_proto!(
                    "sql::create_table::initialize",
                    "INITIALIZE on CREATE TABLE is not implemented in τ"
                );
            }
            if require_user {
                bail_boundary_proto!(
                    "sql::create_table::require_user",
                    "REQUIRE USER on CREATE TABLE is not implemented in τ"
                );
            }
            if columns.is_empty() {
                bail_boundary_proto!(
                    "sql::create_table::no_columns",
                    "CREATE TABLE requires at least one column definition"
                );
            }

            let table_name = extract_simple_name(&name, "sql::create_table::name")?;

            // Lower column definitions: name + type, with bail on constraints
            // (DEFAULT, NOT NULL, CHECK, etc.).
            let fields: Vec<StructField> = columns
                .into_iter()
                .map(|col| {
                    if !col.options.is_empty() {
                        bail_boundary_proto!(
                            "sql::create_table::column_options",
                            format!(
                                "column options (DEFAULT, NOT NULL, etc.) on column `{}` \
                                 are not implemented in τ",
                                col.name.value
                            )
                        );
                    }
                    let dt = lower_data_type(col.data_type)?;
                    Ok(StructField::nullable(col.name.value, dt))
                })
                .collect::<Result<_, EmissionError>>()?;

            Ok(SqlStatement::Ddl(DdlStatement::CreateTable {
                name: table_name,
                if_not_exists,
                columns: StructType::new(fields),
            }))
        }

        // ── DROP TABLE / DROP VIEW ────────────────────────────────────────
        Statement::Drop {
            object_type: ObjectType::Table,
            if_exists,
            names,
            cascade,
            restrict,
            purge,
            temporary,
            table,
        } => {
            if cascade {
                bail_boundary_proto!(
                    "sql::drop_table::cascade",
                    "CASCADE on DROP TABLE is not implemented in τ"
                );
            }
            if restrict {
                bail_boundary_proto!(
                    "sql::drop_table::restrict",
                    "RESTRICT on DROP TABLE is not implemented in τ"
                );
            }
            if purge {
                bail_boundary_proto!(
                    "sql::drop_table::purge",
                    "PURGE on DROP TABLE is not implemented in τ"
                );
            }
            if temporary {
                bail_boundary_proto!(
                    "sql::drop_table::temporary",
                    "DROP TEMPORARY TABLE is not implemented in τ"
                );
            }
            if table.is_some() {
                bail_boundary_proto!(
                    "sql::drop_table::table_ref",
                    "table reference on DROP TABLE is not implemented in τ"
                );
            }
            if names.len() != 1 {
                bail_boundary_proto!(
                    "sql::drop_table::multi_name",
                    "DROP TABLE with multiple names is not implemented in τ"
                );
            }
            let table_name = extract_simple_name(&names[0], "sql::drop_table::name")?;
            Ok(SqlStatement::Ddl(DdlStatement::DropTable {
                name: table_name,
                if_exists,
            }))
        }
        Statement::Drop {
            object_type: ObjectType::View,
            if_exists,
            names,
            cascade,
            restrict,
            purge,
            temporary,
            table,
        } => {
            if cascade {
                bail_boundary_proto!(
                    "sql::drop_view::cascade",
                    "CASCADE on DROP VIEW is not implemented in τ"
                );
            }
            if restrict {
                bail_boundary_proto!(
                    "sql::drop_view::restrict",
                    "RESTRICT on DROP VIEW is not implemented in τ"
                );
            }
            if purge {
                bail_boundary_proto!(
                    "sql::drop_view::purge",
                    "PURGE on DROP VIEW is not implemented in τ"
                );
            }
            if temporary {
                bail_boundary_proto!(
                    "sql::drop_view::temporary",
                    "DROP TEMPORARY VIEW is not implemented in τ"
                );
            }
            if table.is_some() {
                bail_boundary_proto!(
                    "sql::drop_view::table_ref",
                    "table reference on DROP VIEW is not implemented in τ"
                );
            }
            if names.len() != 1 {
                bail_boundary_proto!(
                    "sql::drop_view::multi_name",
                    "DROP VIEW with multiple names is not implemented in τ"
                );
            }
            let view_name = extract_simple_name(&names[0], "sql::drop_view::name")?;
            Ok(SqlStatement::Ddl(DdlStatement::DropView {
                name: view_name,
                if_exists,
            }))
        }

        // ── INSERT INTO ───────────────────────────────────────────────────
        Statement::Insert(Insert {
            insert_token: _,
            optimizer_hint,
            or,
            ignore,
            into: _,
            table,
            table_alias,
            columns,
            overwrite,
            source,
            assignments,
            partitioned,
            after_columns,
            has_table_keyword: _,
            on,
            returning,
            replace_into,
            priority,
            insert_alias,
            settings,
            format_clause,
        }) => {
            // Bail loudly on unsupported clauses.
            if optimizer_hint.is_some() {
                bail_boundary_proto!(
                    "sql::insert::optimizer_hint",
                    "optimizer hints on INSERT are not implemented in τ"
                );
            }
            if or.is_some() {
                bail_boundary_proto!(
                    "sql::insert::or_conflict",
                    "ON CONFLICT on INSERT is not implemented in τ"
                );
            }
            if ignore {
                bail_boundary_proto!(
                    "sql::insert::ignore",
                    "INSERT IGNORE is not implemented in τ"
                );
            }
            if table_alias.is_some() {
                bail_boundary_proto!(
                    "sql::insert::table_alias",
                    "table alias on INSERT is not implemented in τ"
                );
            }
            if !columns.is_empty() {
                bail_boundary_proto!(
                    "sql::insert::column_list",
                    "INSERT INTO <table> (col1, col2, ...) column list is not implemented in τ"
                );
            }
            if overwrite {
                bail_boundary_proto!(
                    "sql::insert::overwrite",
                    "INSERT OVERWRITE is not implemented in τ"
                );
            }
            if !assignments.is_empty() {
                bail_boundary_proto!("sql::insert::set", "INSERT ... SET is not implemented in τ");
            }
            if partitioned.is_some() {
                bail_boundary_proto!(
                    "sql::insert::partitioned",
                    "PARTITION on INSERT is not implemented in τ"
                );
            }
            if !after_columns.is_empty() {
                bail_boundary_proto!(
                    "sql::insert::after_columns",
                    "columns after PARTITION on INSERT are not implemented in τ"
                );
            }
            if on.is_some() {
                bail_boundary_proto!(
                    "sql::insert::on",
                    "ON clause on INSERT is not implemented in τ"
                );
            }
            if returning.is_some() {
                bail_boundary_proto!(
                    "sql::insert::returning",
                    "RETURNING on INSERT is not implemented in τ"
                );
            }
            if replace_into {
                bail_boundary_proto!(
                    "sql::insert::replace_into",
                    "REPLACE INTO is not implemented in τ"
                );
            }
            if priority.is_some() {
                bail_boundary_proto!(
                    "sql::insert::priority",
                    "INSERT priority is not implemented in τ"
                );
            }
            if insert_alias.is_some() {
                bail_boundary_proto!(
                    "sql::insert::insert_alias",
                    "INSERT alias is not implemented in τ"
                );
            }
            if settings.is_some() {
                bail_boundary_proto!(
                    "sql::insert::settings",
                    "INSERT SETTINGS is not implemented in τ"
                );
            }
            if format_clause.is_some() {
                bail_boundary_proto!(
                    "sql::insert::format_clause",
                    "INSERT FORMAT is not implemented in τ"
                );
            }

            // Extract the table name.
            let table_name = match table {
                TableObject::TableName(ref obj_name) => {
                    extract_simple_name(obj_name, "sql::insert::table_name")?
                }
                TableObject::TableFunction(_) => {
                    bail_boundary_proto!(
                        "sql::insert::table_function",
                        "INSERT INTO TABLE FUNCTION is not implemented in τ"
                    );
                }
            };

            let source_query = source.ok_or_else(|| EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: "sql::insert::no_source".to_owned(),
                reason: "INSERT without a source query is not supported".to_owned(),
            })?;

            // Determine whether this is INSERT ... VALUES or INSERT ... SELECT.
            // VALUES bodies arrive as `Query { body: SetExpr::Values(..) }`.
            let body = *source_query;
            if body.with.is_some() || body.order_by.is_some() || body.limit_clause.is_some() {
                // INSERT ... SELECT with ORDER BY / LIMIT / WITH — lower as
                // a full query.
                let ast = lower_query(body, &CteScope::new())?;
                return Ok(SqlStatement::Ddl(DdlStatement::InsertSelect {
                    table: table_name,
                    query: ast,
                }));
            }
            match *body.body {
                SetExpr::Values(values) => {
                    // Lower each row of literal values.
                    let rows: Vec<Vec<Expression>> = values
                        .rows
                        .into_iter()
                        .map(|row| {
                            row.into_iter()
                                .map(|e| lower_expr(e, &CteScope::new()))
                                .collect::<Result<_, _>>()
                        })
                        .collect::<Result<_, _>>()?;
                    Ok(SqlStatement::Ddl(DdlStatement::InsertValues {
                        table: table_name,
                        rows,
                    }))
                }
                _ => {
                    // Reassemble the Query and lower it.
                    let reassembled = Query {
                        with: None,
                        body: body.body,
                        order_by: None,
                        limit_clause: None,
                        fetch: None,
                        locks: vec![],
                        for_clause: None,
                        settings: None,
                        format_clause: None,
                        pipe_operators: vec![],
                    };
                    let ast = lower_query(reassembled, &CteScope::new())?;
                    Ok(SqlStatement::Ddl(DdlStatement::InsertSelect {
                        table: table_name,
                        query: ast,
                    }))
                }
            }
        }

        // ── TRUNCATE TABLE ────────────────────────────────────────────────
        Statement::Truncate(Truncate {
            table_names,
            partitions,
            table: _,
            if_exists: _,
            identity,
            cascade,
            on_cluster,
        }) => {
            if partitions.is_some() {
                bail_boundary_proto!(
                    "sql::truncate::partitions",
                    "TRUNCATE TABLE with PARTITION is not implemented in τ"
                );
            }
            if identity.is_some() {
                bail_boundary_proto!(
                    "sql::truncate::identity",
                    "TRUNCATE TABLE with IDENTITY option is not implemented in τ"
                );
            }
            if cascade.is_some() {
                bail_boundary_proto!(
                    "sql::truncate::cascade",
                    "TRUNCATE TABLE with CASCADE/RESTRICT is not implemented in τ"
                );
            }
            if on_cluster.is_some() {
                bail_boundary_proto!(
                    "sql::truncate::on_cluster",
                    "TRUNCATE TABLE ON CLUSTER is not implemented in τ"
                );
            }
            if table_names.len() != 1 {
                bail_boundary_proto!(
                    "sql::truncate::multi_table",
                    "TRUNCATE with multiple tables is not implemented in τ"
                );
            }
            let name = extract_simple_name(&table_names[0].name, "sql::truncate::name")?;
            Ok(SqlStatement::Ddl(DdlStatement::TruncateTable { name }))
        }

        other => bail_boundary_proto!(
            format!("sql::{}", statement_kind(&other)),
            "parser_v2 only supports SELECT queries and basic DDL/DML in τ"
        ),
    }
}

/// Reject unsupported clauses common to both temp and non-temp CREATE VIEW.
fn reject_unsupported_view_clauses(
    columns: &[sqlparser::ast::ViewColumnDef],
    comment: &Option<String>,
    options: &sqlparser::ast::CreateTableOptions,
    cluster_by: &[sqlparser::ast::Ident],
    with_no_schema_binding: bool,
    materialized: bool,
    to: &Option<ObjectName>,
    params: &Option<sqlparser::ast::CreateViewParams>,
    or_alter: bool,
    secure: bool,
) -> Result<(), EmissionError> {
    if !columns.is_empty() {
        bail_boundary_proto!(
            "sql::create_view::column_list",
            "column alias list on CREATE VIEW is not implemented in τ"
        );
    }
    if comment.is_some() {
        bail_boundary_proto!(
            "sql::create_view::comment",
            "COMMENT on CREATE VIEW is not implemented in τ"
        );
    }
    if !matches!(options, sqlparser::ast::CreateTableOptions::None) {
        bail_boundary_proto!(
            "sql::create_view::options",
            "OPTIONS / WITH on CREATE VIEW is not implemented in τ"
        );
    }
    if !cluster_by.is_empty() {
        bail_boundary_proto!(
            "sql::create_view::cluster_by",
            "CLUSTER BY on CREATE VIEW is not implemented in τ"
        );
    }
    if with_no_schema_binding {
        bail_boundary_proto!(
            "sql::create_view::no_schema_binding",
            "WITH NO SCHEMA BINDING on CREATE VIEW is not implemented in τ"
        );
    }
    if materialized {
        bail_boundary_proto!(
            "sql::create_view::materialized",
            "CREATE MATERIALIZED VIEW is not implemented in τ"
        );
    }
    if to.is_some() {
        bail_boundary_proto!(
            "sql::create_view::to",
            "TO clause on CREATE VIEW is not implemented in τ"
        );
    }
    if params.is_some() {
        bail_boundary_proto!(
            "sql::create_view::params",
            "algorithm/security params on CREATE VIEW is not implemented in τ"
        );
    }
    if or_alter {
        bail_boundary_proto!(
            "sql::create_view::or_alter",
            "CREATE OR ALTER VIEW is not implemented in τ"
        );
    }
    if secure {
        bail_boundary_proto!(
            "sql::create_view::secure",
            "CREATE SECURE VIEW is not implemented in τ"
        );
    }
    Ok(())
}

/// Extract a simple (unqualified, single-part) identifier from an
/// [`ObjectName`], returning a boundary error for multi-part or
/// function-based names.
fn extract_simple_name(name: &ObjectName, shape: &str) -> Result<String, EmissionError> {
    let parts = &name.0;
    if parts.len() != 1 {
        bail_boundary_proto!(
            shape,
            format!("multi-part name `{name}` is not supported — expected a simple identifier")
        );
    }
    match &parts[0] {
        ObjectNamePart::Identifier(ident) => Ok(ident.value.clone()),
        ObjectNamePart::Function(_) => {
            bail_boundary_proto!(
                shape,
                format!("function-derived name `{name}` is not supported")
            )
        }
    }
}

fn statement_kind(stmt: &Statement) -> &'static str {
    match stmt {
        Statement::Query(_) => "query",
        Statement::Insert(_) => "insert",
        Statement::Delete(_) => "delete",
        Statement::Update { .. } => "update",
        Statement::Drop { .. } => "drop",
        Statement::CreateTable(_) => "create_table",
        Statement::CreateView(_) => "create_view",
        Statement::AlterTable { .. } => "alter_table",
        Statement::Truncate(_) => "truncate",
        _ => "other",
    }
}

fn lower_query(query: Query, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    // Build the effective CTE scope: inherit the outer scope, then fold in
    // this query's own `WITH` clause. Each CTE body is lowered with the scope
    // built so far, so a nested CTE (`b AS (... FROM a ...)`) sees its
    // predecessors. `WITH RECURSIVE` takes a fully separate code path
    // (`lower_recursive_with`) — the self-referencing body cannot be inlined.
    let mut local_scope: CteScope;
    let effective_scope: &CteScope = match query.with {
        Some(with) if with.recursive => {
            local_scope = lower_recursive_with(with, cte_scope)?;
            &local_scope
        }
        Some(with) => {
            local_scope = cte_scope.clone();
            for cte in with.cte_tables {
                let body = lower_query(*cte.query, &local_scope)?;
                // Explicit column list `t(k, v)` → positional rename via ToDf.
                let body = if cte.alias.columns.is_empty() {
                    body
                } else {
                    let column_names = cte
                        .alias
                        .columns
                        .into_iter()
                        .map(|c| c.name.value)
                        .collect();
                    CommonAst::new(CommonOp::ToDf {
                        input: Box::new(body),
                        column_names,
                    })
                };
                local_scope.insert(cte.alias.name.value.to_lowercase(), body);
            }
            &local_scope
        }
        None => cte_scope,
    };

    let order_by_exprs: Vec<OrderByExpr> = match &query.order_by {
        Some(ob) => match &ob.kind {
            OrderByKind::Expressions(exprs) => exprs.clone(),
            // Spark `ORDER BY ALL` orders by every output column, left to right,
            // applying the clause's asc/desc + nulls options uniformly. Build a
            // sort key per projection item (query.body is still borrowable here;
            // it is moved at `lower_set_expr(*query.body)` below).
            OrderByKind::All(options) => order_by_all_exprs(&query.body, options)?,
        },
        None => vec![],
    };

    let (limit_expr_opt, offset_expr_opt) = extract_limit_offset(query.limit_clause.as_ref())?;

    let body = lower_set_expr(*query.body, effective_scope)?;
    wrap_with_sort_limit(
        body,
        order_by_exprs,
        limit_expr_opt,
        offset_expr_opt,
        effective_scope,
    )
}

/// Lower a `WITH RECURSIVE` clause into the CTE scope.
///
/// A recursive CTE's body self-references its own name, so it CANNOT be
/// inlined (infinite expansion). Instead the parser builds a
/// [`CommonOp::RecursiveCte`] node that survives to emitted SQL as a genuine
/// `WITH RECURSIVE ... AS (anchor UNION ALL recursive_term)`.
///
/// **Boundary guards (ADR-022 cat-2):**
/// - Only a single CTE is allowed under one `WITH RECURSIVE`.
/// - The CTE body must be a `SetOperation { op: Union, .. }`.
/// - ORDER BY / LIMIT / nested `WITH` on the CTE body's own query are rejected.
fn lower_recursive_with(
    with: sqlparser::ast::With,
    cte_scope: &CteScope,
) -> Result<CteScope, EmissionError> {
    // Guard: only one CTE under a single WITH RECURSIVE.
    if with.cte_tables.len() != 1 {
        bail_boundary_proto!(
            "sql::recursive_cte::multiple",
            format!(
                "WITH RECURSIVE with {} CTEs not supported (expected exactly 1)",
                with.cte_tables.len()
            )
        );
    }

    let cte = with
        .cte_tables
        .into_iter()
        .next()
        .expect("cte_tables is non-empty (len checked above)");
    // Preserve declared-case in the AST node (used by emission + analyzer
    // BaseTypes injection); CteScope key is lowercased (matching non-recursive
    // CTE convention).
    let declared_name = cte.alias.name.value;
    let cte_name_lower = declared_name.to_lowercase();
    let column_names: Vec<String> = cte
        .alias
        .columns
        .into_iter()
        .map(|c| c.name.value)
        .collect();

    let inner_query = *cte.query;

    // Reject modifiers on the CTE body's own Query wrapper.
    if inner_query.order_by.is_some() {
        bail_boundary_proto!(
            "sql::recursive_cte::modifier",
            "ORDER BY on a recursive CTE body is not supported"
        );
    }
    if inner_query.limit_clause.is_some() {
        bail_boundary_proto!(
            "sql::recursive_cte::modifier",
            "LIMIT on a recursive CTE body is not supported"
        );
    }
    if inner_query.with.is_some() {
        bail_boundary_proto!(
            "sql::recursive_cte::modifier",
            "nested WITH inside a recursive CTE body is not supported"
        );
    }

    // The body must be a SetOperation { Union, .. }.
    let (set_quantifier, left, right) = match *inner_query.body {
        SetExpr::SetOperation {
            op: SetOperator::Union,
            set_quantifier,
            left,
            right,
        } => (set_quantifier, left, right),
        _ => {
            bail_boundary_proto!(
                "sql::recursive_cte::body",
                "recursive CTE body must be anchor UNION ALL recursive_term"
            );
        }
    };

    let union_all = matches!(set_quantifier, SetQuantifier::All);

    // Lower both legs with the CURRENT scope — the CTE's own name is NOT
    // added, so the self-reference falls through CteScope-miss into an
    // ordinary TableScan (or AliasedRelation { TableScan, alias }).
    let anchor = lower_set_expr(*left, cte_scope)?;
    let recursive_term = lower_set_expr(*right, cte_scope)?;

    let node = CommonAst::new(CommonOp::RecursiveCte {
        name: cte_name_lower.clone(),
        column_names,
        union_all,
        anchor: Box::new(anchor),
        recursive_term: Box::new(recursive_term),
    });

    // Register in scope — FROM <name> resolves to this node (cloned per ref,
    // mirroring non-recursive CTE registration). Uses lowercase key, matching
    // the non-recursive CTE convention.
    let mut scope = cte_scope.clone();
    scope.insert(cte_name_lower, node);
    Ok(scope)
}

/// Synthesize `ORDER BY ALL` into one sort key per SELECT output column, each
/// carrying the clause's asc/desc + nulls options. Only supported over a plain
/// `SELECT` body (not set ops / VALUES); `*` projections are rejected.
fn order_by_all_exprs(
    body: &SetExpr,
    options: &OrderByOptions,
) -> Result<Vec<OrderByExpr>, EmissionError> {
    let select = match body {
        SetExpr::Select(s) => s,
        _ => {
            bail_boundary_proto!(
                "sql::order_by_all",
                "ORDER BY ALL is only supported over a SELECT body"
            );
        }
    };
    let mut out: Vec<OrderByExpr> = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        let expr = select_item_expr(item)
            .require_proto(
                "sql::order_by_all_wildcard",
                "ORDER BY ALL over `*` projection not supported",
            )?
            .clone();
        out.push(OrderByExpr {
            expr,
            options: *options,
            with_fill: None,
        });
    }
    Ok(out)
}

fn extract_limit_offset(
    clause: Option<&LimitClause>,
) -> Result<(Option<Expr>, Option<Expr>), EmissionError> {
    match clause {
        None => Ok((None, None)),
        Some(LimitClause::LimitOffset { limit, offset, .. }) => {
            let off = offset.as_ref().map(|o| o.value.clone());
            Ok((limit.clone(), off))
        }
        Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
            Ok((Some(limit.clone()), Some(offset.clone())))
        }
    }
}

fn lower_set_expr(body: SetExpr, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    match body {
        SetExpr::Select(sel) => lower_select(*sel, cte_scope),
        SetExpr::Query(q) => lower_query(*q, cte_scope),
        SetExpr::Values(values) => {
            // Lower an inline `VALUES (..), (..)` clause to `CommonOp::Values`.
            // Default column names are `col1..colN`; an `AS t(a, b)` alias list
            // (parsed as a `TableFactor::Derived` alias) overrides them via the
            // existing `ToDf` rename arm in `lower_table_factor`.
            let rows: Vec<Vec<Expression>> = values
                .rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|e| lower_expr(e, cte_scope))
                        .collect::<Result<_, _>>()
                })
                .collect::<Result<_, _>>()?;
            let ncols = rows.first().map(|r| r.len()).unwrap_or(0);
            let column_names = (1..=ncols).map(|i| format!("col{i}")).collect();
            Ok(CommonAst::new(CommonOp::Values { rows, column_names }))
        }
        SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            let kind = match op {
                SetOperator::Union => SetOpKind::Union,
                SetOperator::Intersect => SetOpKind::Intersect,
                SetOperator::Except | SetOperator::Minus => SetOpKind::Except,
            };
            // `UNION BY NAME` is parseable in `SparkDialect` but positional
            // lowering would silently align columns by position — a wrong
            // result. Reject it as a Thunderduck-boundary error rather than
            // mis-lower (ADR-022; loud-fail per CLAUDE.md gotcha #9).
            if matches!(
                set_quantifier,
                SetQuantifier::ByName | SetQuantifier::AllByName | SetQuantifier::DistinctByName
            ) {
                bail_boundary_proto!(
                    "sql::set_operation::by_name",
                    "UNION/INTERSECT/EXCEPT BY NAME not supported (positional only)"
                );
            }
            // Spark defaults bare UNION/INTERSECT/EXCEPT to DISTINCT (`all = false`);
            // only the explicit `ALL` quantifier preserves duplicates.
            let all = matches!(set_quantifier, SetQuantifier::All);
            let left = lower_set_expr(*left, cte_scope)?;
            let right = lower_set_expr(*right, cte_scope)?;
            Ok(CommonAst::new(CommonOp::SetOp {
                kind,
                all,
                by_name: false,
                allow_missing_columns: false,
                children: vec![left, right],
            }))
        }
        other => bail_boundary_proto!(
            format!("sql::set_expr::{other:?}"),
            "set expression not supported in τ"
        ),
    }
}

/// Dispatch table mapping a generator function name + outer flag + column aliases
/// to the `(alias, FunctionCall)` column pairs consumed by `CommonOp::LateralView`.
///
/// Single source of truth for BOTH `LATERAL VIEW explode(...) t AS tag` syntax
/// (via `lower_lateral_views`) and `LATERAL explode(...) AS r(v)` comma-syntax
/// (via `lower_lateral_generator_item`). ADR-004/INV7 convergence by construction.
fn generator_view_columns(
    gen_name: &str,
    outer: bool,
    arg: Expression,
    aliases: Vec<String>,
) -> Result<Vec<(String, Expression)>, EmissionError> {
    // `explode` + OUTER and `explode_outer` emit the same shape — normalize once
    // here so the match below needs only a single arm for it.
    let gen_name = if outer && gen_name == "explode" {
        "explode_outer"
    } else {
        gen_name
    };
    match gen_name {
        "explode" if aliases.len() == 1 => Ok(vec![(
            aliases.into_iter().next().expect("len checked == 1"),
            Expression::FunctionCall(FunctionCall {
                name: "explode".to_owned(),
                args: vec![arg],
                distinct: false,
            }),
        )]),
        "explode_outer" if aliases.len() == 1 => Ok(vec![(
            aliases.into_iter().next().expect("len checked == 1"),
            Expression::FunctionCall(FunctionCall {
                name: "explode_outer".to_owned(),
                args: vec![arg],
                distinct: false,
            }),
        )]),
        "posexplode" if !outer && aliases.len() == 2 => {
            let mut it = aliases.into_iter();
            let pos_alias = it.next().expect("len checked == 2");
            let val_alias = it.next().expect("len checked == 2");
            Ok(vec![
                (
                    pos_alias,
                    Expression::FunctionCall(FunctionCall {
                        name: "posexplode_pos".to_owned(),
                        args: vec![arg.clone()],
                        distinct: false,
                    }),
                ),
                (
                    val_alias,
                    Expression::FunctionCall(FunctionCall {
                        name: "posexplode_val".to_owned(),
                        args: vec![arg],
                        distinct: false,
                    }),
                ),
            ])
        }
        "posexplode" if outer => {
            bail_boundary_proto!(
                "sql::lateral_view::outer_posexplode",
                "OUTER posexplode in LATERAL VIEW not implemented in τ"
            );
        }
        "posexplode" => {
            bail_boundary_proto!(
                "sql::lateral_view::posexplode_alias_count",
                "posexplode in LATERAL VIEW requires exactly 2 aliases"
            );
        }
        _ => {
            bail_boundary_proto!(
                format!("sql::lateral_view::generator::{gen_name}"),
                format!("LATERAL VIEW generator `{gen_name}` not supported in τ")
            );
        }
    }
}

/// Fold `lateral_views` into the plan tree — each `LATERAL VIEW [OUTER]
/// generator(arg) table_alias AS col1[, col2]` becomes a
/// [`CommonOp::LateralView`] wrapping `base`.
///
/// Dispatch table (generator name lowercased, alias count), via
/// [`generator_view_columns`]:
/// - `explode` / `explode_outer`, 1 alias → single-column LateralView
/// - `posexplode`, 2 aliases → split into `posexplode_pos` + `posexplode_val`
/// - Chained (2+) LATERAL VIEWs → boundary error
/// - OUTER posexplode → boundary error (no CASE-wrapped pos/val renderer)
/// - Everything else (wrong alias count, unknown generator, non-1-arg) → boundary error
fn lower_lateral_views(
    base: CommonAst,
    lateral_views: Vec<LateralView>,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    if lateral_views.is_empty() {
        return Ok(base);
    }
    if lateral_views.len() > 1 {
        bail_boundary_proto!(
            "sql::lateral_view::chained",
            "multiple LATERAL VIEW clauses not implemented in τ"
        );
    }
    let lv = lateral_views.into_iter().next().expect("len checked == 1");
    // Extract the generator function name and arguments from the parsed Expr.
    let (gen_name, gen_args) = match &lv.lateral_view {
        Expr::Function(func) => {
            let name = object_name_to_string(&func.name).to_lowercase();
            let args: Vec<Expression> = match &func.args {
                FunctionArguments::List(list) => list
                    .args
                    .iter()
                    .map(|a| function_arg_to_expr(a.clone(), cte_scope))
                    .collect::<Result<Vec<_>, _>>()?,
                _ => {
                    bail_boundary_proto!(
                        "sql::lateral_view::generator_args",
                        "LATERAL VIEW generator with non-list arguments not supported in τ"
                    );
                }
            };
            (name, args)
        }
        _ => {
            bail_boundary_proto!(
                "sql::lateral_view::generator",
                "LATERAL VIEW with non-function generator not supported in τ"
            );
        }
    };
    // Require exactly one generator argument.
    if gen_args.len() != 1 {
        bail_boundary_proto!(
            "sql::lateral_view::generator_arity",
            "LATERAL VIEW generator must have exactly 1 argument"
        );
    }
    let arg = gen_args.into_iter().next().expect("len checked == 1");
    let table_alias = object_name_to_string(&lv.lateral_view_name);
    let aliases: Vec<String> = lv
        .lateral_col_alias
        .iter()
        .map(|id| id.value.clone())
        .collect();
    if aliases.is_empty() {
        bail_boundary_proto!(
            "sql::lateral_view::empty_aliases",
            "LATERAL VIEW with no column aliases not supported in τ"
        );
    }

    let columns = generator_view_columns(&gen_name, lv.outer, arg, aliases)?;

    Ok(CommonAst::new(CommonOp::LateralView {
        input: Box::new(base),
        table_alias,
        columns,
    }))
}

fn lower_select(mut select: Select, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    // Capture DISTINCT before building the projection plan; the plain
    // `SELECT DISTINCT` lowers to a `Deduplicate` wrapping the final Project
    // (empty `on_columns` = dedupe the whole output row). `SELECT ALL` is the
    // default (keep duplicates) → no wrap. `DISTINCT ON (...)` is a Postgres
    // extension Spark SQL does not accept → Thunderduck-boundary reject.
    let dedupe = match select.distinct.take() {
        None | Some(Distinct::All) => false,
        Some(Distinct::Distinct) => true,
        Some(Distinct::On(_)) => {
            bail_boundary_proto!(
                "sql::distinct_on",
                "SELECT DISTINCT ON is not valid Spark SQL"
            );
        }
    };
    // Inline named `WINDOW w AS (...)` references into their `WindowSpec` before
    // lowering — τ's Window substrate has no named-window concept (win-012).
    resolve_named_windows_in_select(&mut select)?;
    let base = lower_from(select.from, cte_scope)?;
    let base = lower_lateral_views(base, select.lateral_views, cte_scope)?;

    let filtered = if let Some(cond) = select.selection {
        CommonAst::new(CommonOp::Filter {
            input: Box::new(base),
            condition: lower_expr(cond, cte_scope)?,
        })
    } else {
        base
    };

    let has_group_by =
        !matches!(&select.group_by, GroupByExpr::Expressions(v, m) if v.is_empty() && m.is_empty());
    let has_aggregates = has_group_by || select.projection.iter().any(select_item_has_aggregate);

    let plan = if has_aggregates {
        lower_aggregate_select(
            filtered,
            select.projection,
            select.group_by,
            select.having,
            cte_scope,
        )?
    } else {
        let projections: Result<Vec<Expression>, EmissionError> = select
            .projection
            .into_iter()
            .map(|item| lower_select_item(item, cte_scope))
            .collect();
        CommonAst::new(CommonOp::Project {
            input: Box::new(filtered),
            projections: projections?,
        })
    };

    // Plain `SELECT DISTINCT` dedupes the final projection. Wrapping here (below
    // `lower_query`'s `wrap_with_sort_limit`) yields `Sort(Deduplicate(Project))`
    // for `SELECT DISTINCT ... ORDER BY ...` — dedupe first, then order.
    let plan = if dedupe {
        CommonAst::new(CommonOp::Deduplicate {
            input: Box::new(plan),
            on_columns: vec![],
        })
    } else {
        plan
    };

    Ok(plan)
}

fn lower_aggregate_select(
    input: CommonAst,
    projection: Vec<SelectItem>,
    group_by: GroupByExpr,
    having: Option<Expr>,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    let (grouping, grouping_kind, grouping_sets) = match group_by {
        GroupByExpr::Expressions(exprs, modifiers) => {
            // Trailing `GROUP BY <cols> WITH ROLLUP` / `WITH CUBE` (Spark
            // postfix form). Only a single ROLLUP or CUBE modifier is a Spark
            // shape; WITH TOTALS (ClickHouse) and stacked modifiers are
            // Thunderduck-boundary rejects.
            let with_modifier_kind = match modifiers.as_slice() {
                [] => None,
                [GroupByWithModifier::Rollup] => Some(GroupingKind::Rollup),
                [GroupByWithModifier::Cube] => Some(GroupingKind::Cube),
                _ => bail_boundary_proto!(
                    "sql::group_by_modifiers",
                    "only a single trailing WITH ROLLUP or WITH CUBE is supported"
                ),
            };
            if let Some(kind) = with_modifier_kind {
                // Postfix modifier: the grouping list is flat and the direction
                // lives in the GroupingKind (mirrors the prefix `ROLLUP(...)`
                // form below and the DataFrame path). A prefix ROLLUP/CUBE/
                // GROUPING SETS wrapper mixed with a trailing modifier is not a
                // Spark shape — reject rather than silently mishandle.
                let mut flat: Vec<Expression> = Vec::with_capacity(exprs.len());
                for e in exprs {
                    if matches!(e, Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_)) {
                        bail_boundary_proto!(
                            "sql::group_by_modifiers",
                            "prefix ROLLUP/CUBE/GROUPING SETS combined with a trailing WITH modifier not supported in τ"
                        );
                    }
                    flat.push(lower_expr(e, cte_scope)?);
                }
                (flat, kind, Vec::new())
            } else {
                // Single-element grouping lists carry the prefix wrappers:
                // `GROUP BY GROUPING SETS ((a, b), (a), ())` parses to one
                // `Expr::GroupingSets(Vec<Vec<Expr>>)` (one inner vec per set;
                // `()` → empty inner vec), and prefix `ROLLUP (...)` /
                // `CUBE (...)` to one `Expr::Rollup`/`Expr::Cube` (Spark's
                // ROLLUP/CUBE always wraps the whole grouping list, so a
                // single wrapper element is the expected shape). Anything else
                // — including a ROLLUP/CUBE mixed with other terms / repeated
                // — flows to the plain GROUP BY path, which rejects the mixed
                // shapes (Thunderduck-boundary, ADR-022).
                match <[Expr; 1]>::try_from(exprs) {
                    Ok([Expr::GroupingSets(sets)]) => {
                        let (flat, index_sets) = lower_grouping_sets(sets, cte_scope)?;
                        (flat, GroupingKind::GroupingSets, index_sets)
                    }
                    Ok([Expr::Rollup(sets)]) => {
                        lower_prefix_rollup_cube(sets, GroupingKind::Rollup, cte_scope)?
                    }
                    Ok([Expr::Cube(sets)]) => {
                        lower_prefix_rollup_cube(sets, GroupingKind::Cube, cte_scope)?
                    }
                    Ok([other]) => (
                        lower_plain_group_by(vec![other], &projection, cte_scope)?,
                        GroupingKind::GroupBy,
                        Vec::new(),
                    ),
                    Err(exprs) => (
                        lower_plain_group_by(exprs, &projection, cte_scope)?,
                        GroupingKind::GroupBy,
                        Vec::new(),
                    ),
                }
            }
        }
        GroupByExpr::All(modifiers) => {
            if !modifiers.is_empty() {
                bail_boundary_proto!(
                    "sql::group_by_all_modifiers",
                    "GROUP BY ALL with ROLLUP/CUBE/GROUPING SETS modifiers not supported"
                );
            }
            // Spark `GROUP BY ALL` groups by every SELECT item that is NOT an
            // aggregate expression (the aggregates come from the projection fold
            // as usual). Compute the grouping from the projection here.
            let mut grouping: Vec<Expression> = Vec::new();
            for item in &projection {
                let expr = select_item_expr(item).require_proto(
                    "sql::group_by_all_wildcard",
                    "GROUP BY ALL over `*` projection not supported",
                )?;
                if !expr_has_aggregate(expr) {
                    grouping.push(lower_expr(expr.clone(), cte_scope)?);
                }
            }
            (grouping, GroupingKind::GroupBy, Vec::new())
        }
    };

    let projections: Result<Vec<Expression>, EmissionError> = projection
        .into_iter()
        .map(|item| lower_select_item(item, cte_scope))
        .collect();
    let projections = projections?;
    // τ treats the aggregate projection list as the aggregate output list.
    // This is refined into the {grouping, aggregates} split when the
    // canonical emission table lands; for now we push everything into
    // `aggregates` so the round-trip test can inspect the projection list.
    // SparkSQL HAVING lowers into the Aggregate's dedicated `having` field —
    // NOT a Filter over the Aggregate. HAVING is post-aggregation group
    // filtering that binds to the aggregate INPUT scope (aggregate exprs +
    // grouping keys), which the analyzer + emission handle directly. Wrapping
    // in a Filter would (a) resolve the predicate against the aggregate OUTPUT
    // schema and (b) emit an outer `WHERE <agg>` that DuckDB rejects.
    let having = having.map(|h| lower_expr(h, cte_scope)).transpose()?;
    Ok(CommonAst::new(CommonOp::Aggregate {
        input: Box::new(input),
        grouping,
        aggregates: projections,
        grouping_kind,
        grouping_sets,
        having,
    }))
}

/// Flat grouping list + grouping direction + per-set index membership — the
/// tuple `lower_aggregate_select` derives from its GROUP BY clause.
type GroupingSpec = (Vec<Expression>, GroupingKind, Vec<Vec<usize>>);

/// Lower a prefix-form `ROLLUP (...)` / `CUBE (...)` grouping wrapper into τ's
/// flat grouping list, threading the direction as the [`GroupingKind`] —
/// mirroring the DataFrame path in `v2_relation_converter::convert_aggregate`.
///
/// sqlparser preserves parenthesized grouping terms: `ROLLUP ((a, b), c)` →
/// `[[a, b], [c]]`, which Spark treats as a distinct set of levels that a flat
/// `ROLLUP(a, b, c)` does NOT reproduce. τ's grouping list is flat (one column
/// per level), so a multi-column term can't be represented — reject rather
/// than silently flatten to the wrong grouping sets (ADR-022, loud-fail).
/// Simple `ROLLUP (a, b)` = `[[a],[b]]` is unaffected.
fn lower_prefix_rollup_cube(
    sets: Vec<Vec<Expr>>,
    kind: GroupingKind,
    cte_scope: &CteScope,
) -> Result<GroupingSpec, EmissionError> {
    if sets.iter().any(|term| term.len() != 1) {
        bail_boundary_proto!(
            "sql::grouping_sets",
            "nested ROLLUP/CUBE grouping terms not supported in τ"
        );
    }
    let mut flat: Vec<Expression> = Vec::new();
    for term in sets {
        for e in term {
            flat.push(lower_expr(e, cte_scope)?);
        }
    }
    Ok((flat, kind, Vec::new()))
}

/// Lower a plain `GROUP BY` expression list.
///
/// Spark `spark.sql.groupByOrdinal=true` (ANSI default): a bare integer
/// literal `N` is an ordinal referencing the Nth (1-based) SELECT item, NOT a
/// constant grouping key — it resolves to that item's underlying expression so
/// `GROUP BY 1` groups by `dept_id`, not the literal `1`. Composite forms
/// (`1 + 1`, `1.5`, `'x'`) are not bare integer literals and fall through to
/// `lower_expr`. A ROLLUP/CUBE/GROUPING SETS mixed with other terms / repeated
/// is not a Spark shape (Spark wraps the whole list in one wrapper) — a
/// Thunderduck-boundary reject.
fn lower_plain_group_by(
    exprs: Vec<Expr>,
    projection: &[SelectItem],
    cte_scope: &CteScope,
) -> Result<Vec<Expression>, EmissionError> {
    let mut plain: Vec<Expression> = Vec::with_capacity(exprs.len());
    for e in exprs {
        match e {
            Expr::Rollup(_) | Expr::Cube(_) | Expr::GroupingSets(_) => {
                bail_boundary_proto!(
                    "sql::grouping_sets",
                    "mixed ROLLUP/CUBE/GROUPING SETS terms not supported in τ"
                );
            }
            Expr::Value(ref vw) if int_from_number_value(&vw.value).is_some() => {
                let n = int_from_number_value(&vw.value).expect("guarded by matches arm above");
                plain.push(resolve_group_by_ordinal(n, projection, cte_scope)?);
            }
            other => plain.push(lower_expr(other, cte_scope)?),
        }
    }
    Ok(plain)
}

/// Lower a `GROUP BY GROUPING SETS (...)` clause to a flat distinct grouping
/// list plus per-set index membership.
///
/// Each inner `Vec<Expr>` is one grouping set (`()` → empty). Columns are
/// deduplicated by structural [`Expression`] equality in first-appearance
/// order into `flat`; each set becomes a vector of indices into `flat`. The
/// emission layer renders `flat` once and indexes it per set.
fn lower_grouping_sets(
    sets: Vec<Vec<Expr>>,
    cte_scope: &CteScope,
) -> Result<(Vec<Expression>, Vec<Vec<usize>>), EmissionError> {
    let mut flat: Vec<Expression> = Vec::new();
    let mut index_sets: Vec<Vec<usize>> = Vec::with_capacity(sets.len());
    for set in sets {
        let mut idxs: Vec<usize> = Vec::with_capacity(set.len());
        for e in set {
            let lowered = lower_expr(e, cte_scope)?;
            // Bind the search result before pushing so the immutable borrow of
            // `flat` from `.position()` is released before the mutable push.
            let existing = flat.iter().position(|g| *g == lowered);
            let idx = match existing {
                Some(i) => i,
                None => {
                    flat.push(lowered);
                    flat.len() - 1
                }
            };
            idxs.push(idx);
        }
        index_sets.push(idxs);
    }
    Ok((flat, index_sets))
}

fn lower_from(from: Vec<TableWithJoins>, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    if from.is_empty() {
        return Ok(CommonAst::new(CommonOp::SingleRow));
    }
    let mut items = from.into_iter();
    let first = items.next().expect("from is non-empty");
    let mut acc = lower_table_with_joins(first, cte_scope)?;
    for twj in items {
        acc = if is_lateral_generator_item(&twj) {
            // Correlated LATERAL generator (e.g. `LATERAL explode(e.tags) AS r(v)`)
            // — redirect to CommonOp::LateralView so the existing analyzer/emission
            // machinery resolves the correlated arg against the left plan's schema.
            lower_lateral_generator_item(acc, twj.relation, cte_scope)?
        } else {
            // Detect comma-form LATERAL derived table: `, LATERAL (subquery) t`
            // (Spark treats it identically to `JOIN LATERAL (subquery) t`).
            let lateral = twj.joins.is_empty()
                && matches!(&twj.relation, TableFactor::Derived { lateral: true, .. });
            let right = lower_table_with_joins(twj, cte_scope)?;
            CommonAst::new(CommonOp::Join {
                left: Box::new(acc),
                right: Box::new(right),
                join_type: JoinType::Cross,
                condition: None,
                using_columns: vec![],
                natural: false,
                lateral,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            })
        };
    }
    Ok(acc)
}

/// True iff the raw comma-item is a `LATERAL <generator>(arg) AS alias(cols)` shape
/// that should redirect to `CommonOp::LateralView` instead of the normal
/// CrossJoin fold. Predicate is narrow by design (ADR-022 — no false positives):
/// trailing joins, non-generator functions, no-alias, and non-LATERAL items
/// all fall through to the existing CrossJoin path.
fn is_lateral_generator_item(twj: &TableWithJoins) -> bool {
    if !twj.joins.is_empty() {
        return false;
    }
    match &twj.relation {
        TableFactor::Function {
            lateral,
            name,
            args,
            alias,
        } => {
            if !lateral || alias.is_none() || args.len() != 1 {
                return false;
            }
            let func_name = object_name_to_string(name).to_lowercase();
            matches!(
                func_name.as_str(),
                "explode" | "explode_outer" | "posexplode"
            )
        }
        _ => false,
    }
}

/// Lower a correlated `LATERAL <generator>(arg) AS alias(cols)` comma-item
/// into `CommonOp::LateralView { input: acc, ... }`. The generator arg
/// references columns from the left plan (e.g. `e.tags`), which the
/// existing `analyze_lateral_view` resolves against the input's schema.
fn lower_lateral_generator_item(
    acc: CommonAst,
    relation: TableFactor,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    // The caller guarantees this is a matched Function variant via
    // `is_lateral_generator_item`, so this destructure is safe.
    let (name, raw_args, alias) = match relation {
        TableFactor::Function {
            name, args, alias, ..
        } => (name, args, alias),
        _ => unreachable!("is_lateral_generator_item verified Function variant"),
    };
    let gen_name = object_name_to_string(&name).to_lowercase();
    // Lower the single generator argument.
    let arg_exprs: Vec<Expression> = raw_args
        .into_iter()
        .map(|a| function_arg_to_expr(a, cte_scope))
        .collect::<Result<_, _>>()?;
    let arg = arg_exprs
        .into_iter()
        .next()
        .expect("is_lateral_generator_item checked args.len() == 1");
    // Extract alias name and column list from the `AS r(v)` alias.
    let table_alias_obj = alias.expect("is_lateral_generator_item checked alias.is_some()");
    let table_alias = table_alias_obj.name.value;
    let column_aliases: Vec<String> = table_alias_obj
        .columns
        .iter()
        .map(|c| c.name.value.clone())
        .collect();
    // For the comma-LATERAL syntax, `OUTER` is not expressible — always non-outer.
    let outer = gen_name == "explode_outer";
    // Use the shared generator dispatch table.
    let columns = generator_view_columns(&gen_name, outer, arg, column_aliases)?;
    Ok(CommonAst::new(CommonOp::LateralView {
        input: Box::new(acc),
        table_alias,
        columns,
    }))
}

fn lower_table_with_joins(
    twj: TableWithJoins,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    let mut plan = lower_table_factor(twj.relation, cte_scope)?;
    for join in twj.joins {
        // Read `lateral` from the right relation BEFORE moving it into
        // `lower_table_factor` (which swallows the flag with `..`).
        let lateral = matches!(&join.relation, TableFactor::Derived { lateral: true, .. });
        let right = lower_table_factor(join.relation, cte_scope)?;
        let (join_type, condition, using_columns, natural) =
            lower_join_operator(join.join_operator, cte_scope)?;
        plan = CommonAst::new(CommonOp::Join {
            left: Box::new(plan),
            right: Box::new(right),
            join_type,
            condition,
            using_columns,
            natural,
            lateral,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
    }
    Ok(plan)
}

fn lower_table_factor(
    factor: TableFactor,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    match factor {
        // `FROM range(5)` and other table-valued functions parse as
        // `TableFactor::Table` with `args: Some(..)`; a plain `FROM emp` has
        // `args: None`. Branch on `args` so the TVF path builds a
        // `CommonOp::TableFunction` while the bare-table path is preserved
        // verbatim (no regression for CTE inlining / TableScan / aliases).
        TableFactor::Table {
            name,
            alias,
            args,
            with_ordinality,
            ..
        } => match args {
            None => {
                // Spark's `<format>.`path`` table syntax: a 2-part ObjectName
                // whose first part (case-insensitive) is a file-format keyword
                // and whose second part is the filesystem path → FileScan.
                // Examples: `delta.`/tmp/t``, `parquet.`/data/f.parquet``.
                if name.0.len() == 2 {
                    let parts: Vec<&sqlparser::ast::ObjectNamePart> = name.0.iter().collect();
                    if let (
                        ObjectNamePart::Identifier(format_ident),
                        ObjectNamePart::Identifier(path_ident),
                    ) = (parts[0], parts[1])
                    {
                        let fmt_lower = format_ident.value.to_ascii_lowercase();
                        let file_format = match fmt_lower.as_str() {
                            "delta" => Some(FileFormat::Delta),
                            "parquet" => Some(FileFormat::Parquet),
                            "json" => Some(FileFormat::Json),
                            "csv" => Some(FileFormat::Csv),
                            "orc" => Some(FileFormat::Orc),
                            _ => None,
                        };
                        if let Some(format) = file_format {
                            let scan = CommonAst::new(CommonOp::FileScan {
                                format,
                                paths: vec![path_ident.value.clone()],
                                schema: None,
                                options: vec![],
                            });
                            return apply_table_alias(scan, alias);
                        }
                    }
                }

                let table = object_name_to_string(&name);
                // A single-part name matching a CTE in scope inlines the CTE
                // body (Spark: a CTE shadows a catalog table of the same name).
                // The reference's own alias wins over the CTE name so qualified
                // refs bind — `FROM e emp` → alias "emp" (cte-003).
                if let Some(body) = cte_scope.get(&table.to_lowercase()) {
                    let alias = alias.map(|a| a.name.value).unwrap_or(table);
                    Ok(CommonAst::new(CommonOp::AliasedRelation {
                        input: Box::new(body.clone()),
                        alias,
                    }))
                } else {
                    // Normalize an aliased bare table to
                    // `AliasedRelation { TableScan { alias: None }, alias }`,
                    // matching the DataFrame front-end (`df.alias("e")`) so both
                    // front-ends produce the same CommonAST node for the same
                    // meaning (INV7, ADR-004). Emission's alias-hoisting
                    // recognizes `AliasedRelation`; the old `TableScan { alias:
                    // Some(..) }` form buried the user alias inside a synthetic
                    // subquery. Mirrors the CTE branch above.
                    let scan = CommonAst::new(CommonOp::TableScan { table, alias: None });
                    match alias {
                        Some(a) => Ok(CommonAst::new(CommonOp::AliasedRelation {
                            input: Box::new(scan),
                            alias: a.name.value,
                        })),
                        None => Ok(scan),
                    }
                }
            }
            Some(tfa) => {
                // ClickHouse `SETTINGS` clause has no Spark equivalent.
                if tfa.settings.is_some() {
                    bail_boundary_proto!(
                        "sql::table_function::settings",
                        "table-function SETTINGS clause (ClickHouse) not supported in τ"
                    );
                }
                let args: Vec<Expression> = tfa
                    .args
                    .into_iter()
                    .map(|a| function_arg_to_expr(a, cte_scope))
                    .collect::<Result<_, _>>()?;
                let node = table_function_node(object_name_to_string(&name), args, with_ordinality);
                // A user alias (`range(5) AS t(id2)`) composes on top via
                // `ToDf` (positional column rename) then `AliasedRelation`
                // (scope qualifier) — shared with `TableFactor::Derived`.
                apply_table_alias(node, alias)
            }
        },
        TableFactor::Derived {
            subquery, alias, ..
        } => {
            // Subquery-in-FROM is lowered by inlining the inner plan. When the
            // derived table carries an alias, wrap the lowered subquery in
            // `AliasedRelation` so qualified refs (`t.dept_id`) bind to the user
            // alias in the emitted SQL instead of the synthetic `__td_proj`.
            // An explicit column list (`AS t(c1, c2)`) positionally renames the
            // subquery output via `ToDf` first. Mirrors the CTE-definition
            // branch above. Unaliased derived tables inline unchanged.
            let inner = lower_query(*subquery, cte_scope)?;
            apply_table_alias(inner, alias)
        }
        TableFactor::TableFunction { expr, alias } => {
            // Only bare identifier / function-call table functions covered.
            let node = match expr {
                Expr::Function(f) => lower_table_function(f, cte_scope)?,
                other => bail_boundary_proto!(
                    format!("sql::table_function::{other:?}"),
                    "table function expr shape not supported in τ"
                ),
            };
            // A user alias (`TABLE(range(3)) AS r(id)`) composes on top via
            // `ToDf` + `AliasedRelation`, same as the `Table`-with-args and
            // `Derived` branches above — previously silently dropped here.
            apply_table_alias(node, alias)
        }
        TableFactor::UNNEST {
            array_exprs,
            with_ordinality,
            ..
        } => {
            if array_exprs.len() != 1 {
                bail_boundary_proto!(
                    "sql::unnest_multi_arg",
                    "UNNEST with multiple array arguments not supported in τ"
                );
            }
            let expr = array_exprs
                .into_iter()
                .next()
                .require_proto("sql::unnest_empty", "UNNEST has no array argument")?;
            Ok(CommonAst::new(CommonOp::Unnest {
                expr: lower_expr(expr, cte_scope)?,
                with_ordinality,
            }))
        }
        TableFactor::Function {
            name, args, alias, ..
        } => {
            let func_name = object_name_to_string(&name);
            let arg_exprs: Vec<Expression> = args
                .into_iter()
                .map(|a| function_arg_to_expr(a, cte_scope))
                .collect::<Result<_, _>>()?;
            // A user alias (`LATERAL explode(arr) AS x(v)`) composes the same
            // way — previously silently dropped here. The `lateral: bool`
            // flag remains swallowed by `..`; lateral correlation semantics
            // are a separate, pre-existing gap this fix does not widen into.
            apply_table_alias(table_function_node(func_name, arg_exprs, false), alias)
        }
        // SQL `PIVOT` (BigQuery/Snowflake/Databricks). Unlike the DataFrame
        // path, SQL supplies no grouping list — the analyzer derives it from
        // the resolved input schema (`grouping: PivotGrouping::Implicit`).
        TableFactor::Pivot {
            table,
            aggregate_functions,
            value_column,
            value_source,
            default_on_null,
            alias: _,
        } => {
            // Spark has no PIVOT `DEFAULT ON NULL` clause — boundary reject.
            if default_on_null.is_some() {
                bail_boundary_proto!(
                    "sql::pivot::default_on_null",
                    "PIVOT DEFAULT ON NULL has no Spark equivalent"
                );
            }
            let input = Box::new(lower_table_factor(*table, cte_scope)?);
            if value_column.len() != 1 {
                bail_boundary_proto!(
                    "sql::pivot::multi_value_column",
                    "PIVOT supports exactly one FOR column"
                );
            }
            let pivot_column = lower_expr(
                value_column
                    .into_iter()
                    .next()
                    .expect("value_column length checked == 1"),
                cte_scope,
            )?;
            let pivot_values = match value_source {
                PivotValueSource::List(vals) => {
                    let mut out: Vec<Expression> = Vec::with_capacity(vals.len());
                    for ewa in vals {
                        out.push(lower_expr_with_alias(ewa, cte_scope)?);
                    }
                    out
                }
                // ANY / subquery = dynamic pivot values; requires an eager
                // DISTINCT query — Thunderduck-boundary (ADR-022),
                // mirrors the analyzer's `Pivot[implicit-values]` punt.
                PivotValueSource::Any(_) | PivotValueSource::Subquery(_) => {
                    bail_boundary_proto!(
                        "sql::pivot::dynamic_values",
                        "dynamic PIVOT values (ANY / subquery) require an eager DISTINCT query, not supported in τ"
                    );
                }
            };
            let mut aggregates: Vec<Expression> = Vec::with_capacity(aggregate_functions.len());
            for ewa in aggregate_functions {
                aggregates.push(lower_expr_with_alias(ewa, cte_scope)?);
            }
            Ok(CommonAst::new(CommonOp::Pivot {
                input,
                grouping: PivotGrouping::Implicit,
                pivot_column,
                pivot_values,
                aggregates,
            }))
        }
        // SQL `UNPIVOT`. SQL lists only value columns; the id columns are
        // implicit (`input − values`), derived by the analyzer.
        TableFactor::Unpivot {
            table,
            value,
            name,
            columns,
            null_inclusion,
            alias: _,
        } => {
            // τ's Unpivot variant has no include-nulls field; EXCLUDE NULLS is
            // the default. INCLUDE NULLS is unrepresentable — boundary reject.
            if matches!(null_inclusion, Some(NullInclusion::IncludeNulls)) {
                bail_boundary_proto!(
                    "sql::unpivot::include_nulls",
                    "UNPIVOT INCLUDE NULLS is not representable in τ (EXCLUDE NULLS is the default)"
                );
            }
            let input = Box::new(lower_table_factor(*table, cte_scope)?);
            let value_column_name = expr_to_ident_string(&value).require_proto(
                "sql::unpivot::value_non_ident",
                "UNPIVOT value must be a bare column name",
            )?;
            let variable_column_name = name.value;
            let mut values: Vec<String> = Vec::with_capacity(columns.len());
            for ewa in columns {
                if ewa.alias.is_some() {
                    bail_boundary_proto!(
                        "sql::unpivot::column_alias",
                        "UNPIVOT columns cannot be aliased in τ"
                    );
                }
                let col = expr_to_ident_string(&ewa.expr).require_proto(
                    "sql::unpivot::column_non_ident",
                    "UNPIVOT columns must be bare column names",
                )?;
                values.push(col);
            }
            Ok(CommonAst::new(CommonOp::Unpivot {
                input,
                ids: UnpivotIds::Implicit,
                values,
                variable_column_name,
                value_column_name,
            }))
        }
        other => bail_boundary_proto!(
            format!("sql::table_factor::{other:?}"),
            "table factor not supported in τ"
        ),
    }
}

/// Thread a SQL [`TableAlias`] onto an already-lowered relation. An explicit
/// column list (`AS t(c1, c2)`) positionally renames the relation's output via
/// [`CommonOp::ToDf`] first; the alias name is then attached as a scope
/// qualifier via [`CommonOp::AliasedRelation`] so qualified refs bind. An
/// absent alias returns the relation unchanged. Shared by the
/// `TableFactor::Derived` (subquery-in-FROM), `TableFactor::Table` with args
/// (table-valued function), `TableFactor::TableFunction` (`TABLE(f(...))`),
/// and `TableFactor::Function` (`LATERAL f(...)`) branches so all front-end
/// shapes agree (INV7, ADR-004).
fn apply_table_alias(
    inner: CommonAst,
    alias: Option<TableAlias>,
) -> Result<CommonAst, EmissionError> {
    match alias {
        Some(a) => {
            let renamed = if a.columns.is_empty() {
                inner
            } else {
                let column_names = a.columns.into_iter().map(|c| c.name.value).collect();
                CommonAst::new(CommonOp::ToDf {
                    input: Box::new(inner),
                    column_names,
                })
            };
            Ok(CommonAst::new(CommonOp::AliasedRelation {
                input: Box::new(renamed),
                alias: a.name.value,
            }))
        }
        None => Ok(inner),
    }
}

/// Lower a sqlparser [`ExprWithAlias`], wrapping the lowered expression in an
/// [`Expression::Alias`] only when an alias is present (mirrors
/// [`lower_select_item`]). Used for PIVOT aggregate functions and pivot
/// values, where `true AS act` must carry the alias but bare `10` must not.
fn lower_expr_with_alias(
    ewa: ExprWithAlias,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    let inner = lower_expr(ewa.expr, cte_scope)?;
    Ok(match ewa.alias {
        Some(a) => Expression::Alias(AliasExpression {
            expr: Box::new(inner),
            alias: a.value,
        }),
        None => inner,
    })
}

/// Extract a bare column name from a sqlparser [`Expr`] that must be a single
/// identifier (`UNPIVOT` value / column names are stored as plain strings in
/// τ). Returns `None` for any richer expression shape.
fn expr_to_ident_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.clone()),
        _ => None,
    }
}

/// Build a [`CommonOp::TableFunction`] node. Shared by the three
/// table-valued-function construction sites (`TableFactor::Table` with args,
/// `TableFactor::TableFunction`, `TableFactor::Function`) so all parse shapes
/// produce the identical node.
fn table_function_node(name: String, args: Vec<Expression>, with_ordinality: bool) -> CommonAst {
    CommonAst::new(CommonOp::TableFunction {
        name,
        args,
        with_ordinality,
    })
}

fn lower_table_function(f: Function, cte_scope: &CteScope) -> Result<CommonAst, EmissionError> {
    let name = object_name_to_string(&f.name);
    // DISTINCT cannot arrive in these TVF parse shapes (`FROM TABLE(f(...))`
    // takes no duplicate-treatment clause), so the flag is ignored here.
    let (_distinct, args) = lower_function_args(f.args, cte_scope)?;
    Ok(table_function_node(name, args, false))
}

/// Triage sqlparser [`FunctionArguments`] into `(distinct, args)`: the
/// `DISTINCT` duplicate-treatment flag plus the lowered argument expressions.
/// Subquery-shaped argument lists are a Thunderduck-boundary reject. Shared
/// by the scalar path ([`lower_function`], which consumes `distinct`) and the
/// table-function path ([`lower_table_function`], which ignores it).
fn lower_function_args(
    args: FunctionArguments,
    cte_scope: &CteScope,
) -> Result<(bool, Vec<Expression>), EmissionError> {
    match args {
        FunctionArguments::None => Ok((false, vec![])),
        FunctionArguments::Subquery(_) => bail_boundary_proto!(
            "sql::function_args_subquery",
            "subquery function arguments not supported in τ"
        ),
        FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment,
            args,
            ..
        }) => {
            let distinct = matches!(duplicate_treatment, Some(DuplicateTreatment::Distinct));
            let converted = args
                .into_iter()
                .map(|a| function_arg_to_expr(a, cte_scope))
                .collect::<Result<_, _>>()?;
            Ok((distinct, converted))
        }
    }
}

/// Unwrap the [`Expr`] carried by a positional (`Unnamed`) or named
/// [`FunctionArg`]. Wildcard and other shapes hand the argument back
/// unchanged so each caller can build its own shape-specific boundary error
/// (or handle wildcards, as [`function_arg_to_expr`] does).
// `Err` here is the give-the-argument-back channel, not an error type —
// boxing it would buy nothing but an allocation on the reject path.
#[allow(clippy::result_large_err)]
fn unnamed_or_named_expr(arg: FunctionArg) -> Result<Expr, FunctionArg> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
        | FunctionArg::Named {
            arg: FunctionArgExpr::Expr(e),
            ..
        } => Ok(e),
        other => Err(other),
    }
}

fn function_arg_to_expr(
    arg: FunctionArg,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    match unnamed_or_named_expr(arg) {
        Ok(e) => lower_expr(e, cte_scope),
        Err(FunctionArg::Unnamed(FunctionArgExpr::Wildcard)) => {
            Ok(Expression::Star(StarExpression { qualifier: None }))
        }
        Err(FunctionArg::Unnamed(FunctionArgExpr::QualifiedWildcard(name))) => {
            Ok(Expression::Star(StarExpression {
                qualifier: Some(object_name_to_string(&name)),
            }))
        }
        Err(other) => bail_boundary_proto!(
            format!("sql::function_arg::{other:?}"),
            "function argument shape not supported in τ"
        ),
    }
}

fn lower_join_operator(
    op: JoinOperator,
    cte_scope: &CteScope,
) -> Result<(JoinType, Option<Expression>, Vec<String>, bool), EmissionError> {
    let (join_type, constraint) = match op {
        JoinOperator::Join(c) | JoinOperator::Inner(c) => (JoinType::Inner, c),
        JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => (JoinType::Left, c),
        JoinOperator::Right(c) | JoinOperator::RightOuter(c) => (JoinType::Right, c),
        JoinOperator::FullOuter(c) => (JoinType::Full, c),
        JoinOperator::CrossJoin(c) => (JoinType::Cross, c),
        JoinOperator::LeftSemi(c) => (JoinType::LeftSemi, c),
        JoinOperator::LeftAnti(c) => (JoinType::LeftAnti, c),
        other => {
            bail_boundary_proto!(
                format!("sql::join_operator::{other:?}"),
                "join operator not supported in τ"
            );
        }
    };
    let (cond, using, natural) = lower_join_constraint(constraint, cte_scope)?;
    Ok((join_type, cond, using, natural))
}

fn lower_join_constraint(
    constraint: JoinConstraint,
    cte_scope: &CteScope,
) -> Result<(Option<Expression>, Vec<String>, bool), EmissionError> {
    match constraint {
        JoinConstraint::On(expr) => Ok((Some(lower_expr(expr, cte_scope)?), vec![], false)),
        JoinConstraint::Using(cols) => {
            let names: Vec<String> = cols.iter().map(object_name_to_string).collect();
            Ok((None, names, false))
        }
        // NATURAL carries no explicit condition/using of its own; the
        // analyzer desugars it into `using_columns` (name intersection) once
        // resolved child schemas are available (lowering has no schemas).
        JoinConstraint::Natural => Ok((None, vec![], true)),
        JoinConstraint::None => Ok((None, vec![], false)),
    }
}

fn lower_select_item(item: SelectItem, cte_scope: &CteScope) -> Result<Expression, EmissionError> {
    match item {
        SelectItem::UnnamedExpr(expr) => {
            let lowered = lower_expr(expr, cte_scope)?;
            // SparkSQL default column naming diverges from the DataFrame path
            // for `count(*)`: Spark rewrites `count(*)` to `count(1)` and the
            // unaliased output column is therefore named `count(1)` (whereas
            // the DataFrame `.count()` method names it `count`). The shared
            // `expression_output_name` yields `count` for both, which is right
            // for the DataFrame path but wrong here — so stamp the SparkSQL
            // default name on the unaliased top-level select item.
            match sparksql_default_select_name(&lowered) {
                Some(name) => Ok(Expression::Alias(AliasExpression {
                    expr: Box::new(lowered),
                    alias: name,
                })),
                None => Ok(lowered),
            }
        }
        SelectItem::ExprWithAlias { expr, alias } => {
            let inner = lower_expr(expr, cte_scope)?;
            Ok(Expression::Alias(AliasExpression {
                expr: Box::new(inner),
                alias: alias.value,
            }))
        }
        SelectItem::Wildcard(_) => Ok(Expression::Star(StarExpression { qualifier: None })),
        SelectItem::QualifiedWildcard(kind, _) => {
            use sqlparser::ast::SelectItemQualifiedWildcardKind;
            let q = match &kind {
                SelectItemQualifiedWildcardKind::ObjectName(n) => object_name_to_string(n),
                SelectItemQualifiedWildcardKind::Expr(e) => e.to_string(),
            };
            Ok(Expression::Star(StarExpression { qualifier: Some(q) }))
        }
    }
}

/// SparkSQL default output-column name for an unaliased top-level SELECT item,
/// where it diverges from τ's shared `expression_output_name`.
///
/// Currently the one divergence τ needs is `count(*)`: Spark analyzes it to
/// `count(1)` and names the column `count(1)`. Returns `None` for every other
/// shape, letting the default name flow from `expression_output_name`.
fn sparksql_default_select_name(expr: &Expression) -> Option<String> {
    if let Expression::FunctionCall(f) = expr {
        if f.name.eq_ignore_ascii_case("count")
            && !f.distinct
            && matches!(f.args.as_slice(), [Expression::Star(_)])
        {
            return Some("count(1)".to_owned());
        }
    }
    None
}

/// Undo a synthetic SparkSQL default-name alias added by
/// [`sparksql_default_select_name`], returning the bare underlying expression.
///
/// The `F.expr("...")` / `selectExpr("...")` fragment path
/// ([`SparkSqlParserV2::parse_expression`]) must yield the raw expression with
/// NO τ-synthesized alias — the DataFrame layer assigns the output name there.
/// A user-written alias (or any other alias) is preserved untouched.
pub(super) fn strip_synthetic_default_name(expr: Expression) -> Expression {
    if let Expression::Alias(a) = &expr {
        if sparksql_default_select_name(&a.expr).as_deref() == Some(a.alias.as_str()) {
            return (*a.expr).clone();
        }
    }
    expr
}

/// Resolve a Spark GROUP BY ordinal (1-based, `spark.sql.groupByOrdinal=true`)
/// to the Nth SELECT item's alias-stripped underlying expression.
///
/// Out-of-range positions and positions referencing an aggregate select item
/// are Spark-emulated errors; τ's lowering only produces `EmissionError`, so
/// they surface as Thunderduck-boundary rejects (the `distinct_on` precedent).
fn resolve_group_by_ordinal(
    n: i32,
    projection: &[SelectItem],
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    if n < 1 || (n as usize) > projection.len() {
        bail_boundary_proto!(
            "sql::group_by_position",
            format!(
                "GROUP BY position {n} is not in select list (valid range is [1, {}])",
                projection.len()
            )
        );
    }
    let item = &projection[(n - 1) as usize];
    if select_item_has_aggregate(item) {
        bail_boundary_proto!(
            "sql::group_by_position_aggregate",
            format!("GROUP BY position {n} is an aggregate function; not allowed in GROUP BY")
        );
    }
    let expr = select_item_expr(item).require_proto(
        "sql::group_by_position",
        &format!("GROUP BY position {n} references a wildcard select item"),
    )?;
    lower_expr(expr.clone(), cte_scope)
}

/// The projection expression carried by a [`SelectItem`], if any: both the
/// unnamed and aliased forms carry one; wildcard items (`*`, `t.*`) do not.
fn select_item_expr(item: &SelectItem) -> Option<&Expr> {
    match item {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => Some(e),
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => None,
    }
}

fn select_item_has_aggregate(item: &SelectItem) -> bool {
    select_item_expr(item).is_some_and(expr_has_aggregate)
}

/// Full-tree `exists` for a nested aggregate call (Spark's
/// `GlobalAggregates` / `ResolveGroupByAll` / GROUP BY ordinal checks).
///
/// The special-form arms (`Extract`/`Ceil`/`Floor`/`Substring`/`Position`/
/// `Trim`/`Overlay`/`CompoundFieldAccess`) MUST stay in lockstep with
/// [`resolve_named_windows_in_expr`]'s `&mut` mirror — the two walkers share
/// the "missed a composite shape" bug class, one classifying aggregates, the
/// other rewriting named windows. The parse-from-SQL parity test
/// `expr_has_aggregate_classifier_table` guards against drift.
fn expr_has_aggregate(expr: &Expr) -> bool {
    // Fix pass (review M4): extend the walker to every composite
    // shape the projection can contain. A missed shape used to mis-classify
    // e.g. `SELECT count(x) IN (1, 2)` as non-aggregate.
    match expr {
        Expr::Function(f) => function_call_has_aggregate(f),
        Expr::BinaryOp { left, right, .. } => expr_has_aggregate(left) || expr_has_aggregate(right),
        Expr::UnaryOp { expr, .. } => expr_has_aggregate(expr),
        Expr::Nested(e) => expr_has_aggregate(e),
        Expr::Cast { expr, .. } => expr_has_aggregate(expr),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            operand.as_deref().is_some_and(expr_has_aggregate)
                || conditions
                    .iter()
                    .any(|c| expr_has_aggregate(&c.condition) || expr_has_aggregate(&c.result))
                || else_result.as_deref().is_some_and(expr_has_aggregate)
        }
        Expr::InList { expr, list, .. } => {
            expr_has_aggregate(expr) || list.iter().any(expr_has_aggregate)
        }
        Expr::InSubquery { expr, .. } => expr_has_aggregate(expr),
        Expr::Between {
            expr, low, high, ..
        } => expr_has_aggregate(expr) || expr_has_aggregate(low) || expr_has_aggregate(high),
        Expr::Like {
            expr,
            pattern,
            any: _,
            ..
        }
        | Expr::ILike {
            expr,
            pattern,
            any: _,
            ..
        }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => {
            expr_has_aggregate(expr) || expr_has_aggregate(pattern)
        }
        Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsTrue(e)
        | Expr::IsNotTrue(e)
        | Expr::IsFalse(e)
        | Expr::IsNotFalse(e)
        | Expr::IsUnknown(e)
        | Expr::IsNotUnknown(e) => expr_has_aggregate(e),
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            expr_has_aggregate(a) || expr_has_aggregate(b)
        }
        Expr::Tuple(items) | Expr::Array(sqlparser::ast::Array { elem: items, .. }) => {
            items.iter().any(expr_has_aggregate)
        }
        Expr::Collate { expr, .. }
        | Expr::AtTimeZone {
            timestamp: expr, ..
        } => expr_has_aggregate(expr),
        // ── SQL special forms ────────────────────────────────────────────
        // sqlparser parses `EXTRACT`/`CEIL`/`FLOOR`/`SUBSTRING`/`POSITION`/
        // `TRIM`/`OVERLAY` and bracket field access to dedicated `Expr`
        // variants (NOT `Expr::Function`). These arms MUST stay in lockstep
        // with `resolve_named_windows_in_expr`'s mirror set; the parse-from-
        // SQL parity test `expr_has_aggregate_classifier_table` guards drift.
        Expr::Extract { expr, .. } | Expr::Ceil { expr, .. } | Expr::Floor { expr, .. } => {
            expr_has_aggregate(expr)
        }
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            expr_has_aggregate(expr)
                || substring_from.as_deref().is_some_and(expr_has_aggregate)
                || substring_for.as_deref().is_some_and(expr_has_aggregate)
        }
        Expr::Position { expr, r#in } => expr_has_aggregate(expr) || expr_has_aggregate(r#in),
        // `trim_characters` is elided via `..` — never produced under τ's
        // SparkDialect (always `None`), so recursing it would be a dead arm.
        Expr::Trim {
            expr, trim_what, ..
        } => expr_has_aggregate(expr) || trim_what.as_deref().is_some_and(expr_has_aggregate),
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            expr_has_aggregate(expr)
                || expr_has_aggregate(overlay_what)
                || expr_has_aggregate(overlay_from)
                || overlay_for.as_deref().is_some_and(expr_has_aggregate)
        }
        Expr::CompoundFieldAccess { root, access_chain } => {
            expr_has_aggregate(root)
                || access_chain.iter().any(|a| match a {
                    AccessExpr::Subscript(Subscript::Index { index }) => expr_has_aggregate(index),
                    AccessExpr::Dot(_) | AccessExpr::Subscript(Subscript::Slice { .. }) => false,
                })
        }
        // Leaves and shapes that can't syntactically host an aggregate at
        // A.2 (identifiers, literals, subqueries, wildcards, GROUPING SETS,
        // interval/map/tuple/JSON access, etc.) contribute no aggregate.
        _ => false,
    }
}

/// Spark's aggregate detection is a full-tree `exists` (`GlobalAggregates`,
/// `ResolveGroupByAll`, and the GROUP BY ordinal check all use
/// `exists(_.isInstanceOf[AggregateExpression])`), excluding only an
/// aggregate that IS the window function itself (`sum(x) OVER (...)` alone is
/// not an aggregate query). So: check the call's own name (non-windowed
/// only), then descend into its arguments (windowed or not) and into an
/// inline `OVER` spec's `PARTITION BY` / `ORDER BY` expressions — an
/// aggregate nested inside another call's arguments, or inside a window
/// spec's ordering, still makes the whole query an aggregate query
/// (`abs(count(x))`, `rank() OVER (ORDER BY count(*))`).
///
/// Deliberately NOT scanned: `f.filter`, `f.within_group`, `f.parameters`,
/// the argument-list `clauses`, and window frame bounds — an aggregate in any
/// of those positions is invalid Spark anyway, so the worst case is an
/// error-message divergence on an already-invalid query. A `NamedWindow`
/// reference in `f.over` is inlined into a `WindowSpec` earlier
/// (`resolve_named_windows_in_select` runs before this classifier in
/// `lower_select`); a surviving one is an undefined window, not this
/// function's concern.
fn function_call_has_aggregate(f: &Function) -> bool {
    if f.over.is_none() && is_aggregate_function_name(&f.name.to_string()) {
        return true;
    }
    let args_have_aggregate = match &f.args {
        FunctionArguments::List(list) => list.args.iter().any(function_arg_has_aggregate),
        // A subquery argument is its own aggregation scope — never counts
        // (mirrors the `InSubquery` / bare-subquery leaf handling above).
        FunctionArguments::None | FunctionArguments::Subquery(_) => false,
    };
    args_have_aggregate
        || match &f.over {
            Some(WindowType::WindowSpec(spec)) => {
                spec.partition_by.iter().any(expr_has_aggregate)
                    || spec.order_by.iter().any(|o| expr_has_aggregate(&o.expr))
            }
            Some(WindowType::NamedWindow(_)) | None => false,
        }
}

fn function_arg_has_aggregate(arg: &FunctionArg) -> bool {
    match arg {
        FunctionArg::Unnamed(fae)
        | FunctionArg::Named { arg: fae, .. }
        | FunctionArg::ExprNamed { arg: fae, .. } => match fae {
            FunctionArgExpr::Expr(e) => expr_has_aggregate(e),
            FunctionArgExpr::Wildcard | FunctionArgExpr::QualifiedWildcard(_) => false,
        },
    }
}

fn is_aggregate_function_name(name: &str) -> bool {
    // Fix pass (review M3 + perf OPT-5): defer to τ's canonical
    // aggregate roster (the classifier column of the `AGG_SPECS` table in
    // `transpiler_v2::type_inference`) instead of a locally-drifted 32-name
    // subset. The lookup is case-insensitive without a per-call `String`
    // allocation.
    is_aggregate_classifier_name(name)
}

/// Build a non-null boolean literal expression — used to lower `IS [NOT] TRUE`
/// / `IS [NOT] FALSE` onto τ's `IsDistinctFrom` substrate.
fn bool_literal(b: bool) -> Expression {
    Expression::Literal(Literal {
        value: LiteralValue::Boolean(b),
        data_type: DataType::Boolean,
    })
}

/// Build a non-null string literal expression.
fn str_lit(s: String) -> Expression {
    Expression::Literal(Literal {
        value: LiteralValue::String(s),
        data_type: DataType::String,
    })
}

/// Build a non-DISTINCT [`Expression::FunctionCall`] — the common shape every
/// special-syntax lowering (EXTRACT, SUBSTRING, TRIM, POSITION, …) produces.
fn fn_call(name: impl Into<String>, args: Vec<Expression>) -> Expression {
    Expression::FunctionCall(FunctionCall {
        name: name.into(),
        args,
        distinct: false,
    })
}

/// Build an `IS [NOT] DISTINCT FROM` node on τ's `IsDistinctFrom` substrate.
fn is_distinct(left: Expression, right: Expression, negated: bool) -> Expression {
    Expression::IsDistinctFrom(IsDistinctFromExpression {
        left: Box::new(left),
        right: Box::new(right),
        negated,
    })
}

fn lower_expr(expr: Expr, cte_scope: &CteScope) -> Result<Expression, EmissionError> {
    match expr {
        Expr::Identifier(ident) => Ok(Expression::UnresolvedColumn(UnresolvedColumn {
            name: ident.value,
            qualifier: None,
            plan_id: None,
        })),
        Expr::CompoundIdentifier(parts) => {
            // Lower a dotted reference as first-part qualifier / dotted
            // remainder — mirroring the Spark Connect converter's `splitn(2,'.')`
            // shape the analyzer's nested-struct rewrite (analyzer.rs
            // `try_rewrite_nested_struct_path`) is written for. A 3-part struct
            // path `address.geo.lat` becomes `UnresolvedColumn{qualifier:
            // "address", name:"geo.lat"}` so the analyzer can walk the struct;
            // 2-part refs `t.c` are byte-identical to before (parts[0] qualifier,
            // parts[1] name). Corpus witness: cx-004.
            let values: Vec<String> = parts.iter().map(|i| i.value.clone()).collect();
            let (qualifier, name) = match values.len() {
                0 => (None, String::new()),
                1 => (None, values.into_iter().next().unwrap_or_default()),
                _ => (Some(values[0].clone()), values[1..].join(".")),
            };
            Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name,
                qualifier,
                plan_id: None,
            }))
        }
        Expr::Value(vw) => lower_value(vw),
        Expr::BinaryOp { left, op, right } => {
            // Spark's `a DIV b` integer-division operator lowers to a truncating
            // integer divide. Emit as `CAST(a / b AS BIGINT)` — DuckDB's `/`
            // on integer operands truncates, matching Spark's semantics for
            // integral inputs. The projection-slot Spark-return-cast keeps
            // the outer type consistent. Corpus witness: `type-007`.
            if matches!(op, BinaryOperator::MyIntegerDivide) {
                let l = lower_expr(*left, cte_scope)?;
                let r = lower_expr(*right, cte_scope)?;
                return Ok(Expression::Cast(CastExpression {
                    expr: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Div,
                        left: Box::new(l),
                        right: Box::new(r),
                    })),
                    to_type: DataType::Long,
                    try_cast: false,
                }));
            }
            // Spark's null-safe equality `a <=> b` is defined as `NOT DISTINCT
            // FROM` — it returns a non-null boolean and treats `NULL <=> NULL`
            // as true. Lower directly onto τ's `IsDistinctFrom` substrate with
            // `negated: true` rather than routing through `lower_binary_op`
            // (which yields a `BinaryOp` enum and can't produce this shape).
            // Corpus witness: `whr-015`.
            if matches!(op, BinaryOperator::Spaceship) {
                return Ok(is_distinct(
                    lower_expr(*left, cte_scope)?,
                    lower_expr(*right, cte_scope)?,
                    true,
                ));
            }
            Ok(Expression::Binary(BinaryExpression {
                op: lower_binary_op(op)?,
                left: Box::new(lower_expr(*left, cte_scope)?),
                right: Box::new(lower_expr(*right, cte_scope)?),
            }))
        }
        Expr::UnaryOp { op, expr } => match op {
            UnaryOperator::Not => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::Not,
                operand: Box::new(lower_expr(*expr, cte_scope)?),
            })),
            UnaryOperator::Minus => Ok(Expression::Unary(UnaryExpression {
                op: UnaryOp::Negate,
                operand: Box::new(lower_expr(*expr, cte_scope)?),
            })),
            UnaryOperator::Plus => lower_expr(*expr, cte_scope),
            other => bail_boundary_proto!(
                format!("sql::unary_op::{other:?}"),
                "unary operator not supported in τ"
            ),
        },
        Expr::Nested(e) => lower_expr(*e, cte_scope),
        Expr::Cast {
            kind,
            expr,
            data_type,
            ..
        } => {
            let try_cast = matches!(kind, CastKind::TryCast | CastKind::SafeCast);
            Ok(Expression::Cast(CastExpression {
                expr: Box::new(lower_expr(*expr, cte_scope)?),
                to_type: lower_data_type(data_type)?,
                try_cast,
            }))
        }
        Expr::Function(f) => lower_function(f, cte_scope),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            // Simple CASE (`CASE e WHEN vᵢ THEN rᵢ ... ELSE rd`): Spark's
            // `AstBuilder.visitSimpleCase` rewrites each branch condition to
            // `EqualTo(e, vᵢ)` — a null-UNSAFE `=`, so a NULL operand yields
            // NULL comparisons and falls through to ELSE. Lower `e` once and
            // reuse it (Expression is Clone) across the branches. Searched
            // CASE (`operand: None`) keeps its raw predicate conditions.
            let operand_expr = operand.map(|op| lower_expr(*op, cte_scope)).transpose()?;
            let branches = conditions
                .into_iter()
                .map(|c| {
                    let cond = lower_expr(c.condition, cte_scope)?;
                    let cond = match &operand_expr {
                        Some(op_expr) => Expression::Binary(BinaryExpression {
                            op: BinaryOp::Eq,
                            left: Box::new(op_expr.clone()),
                            right: Box::new(cond),
                        }),
                        None => cond,
                    };
                    Ok((cond, lower_expr(c.result, cte_scope)?))
                })
                .collect::<Result<Vec<_>, EmissionError>>()?;
            let else_expr = else_result
                .map(|e| lower_expr(*e, cte_scope))
                .transpose()?
                .map(Box::new);
            Ok(Expression::CaseWhen(CaseWhenExpression {
                branches,
                else_expr,
            }))
        }
        // Row-value (multi-column) IN: `(c1,…,ck) IN ((v11,…,v1k), …)`. sqlparser
        // parses the LHS and each RHS element as `Expr::Tuple`. Spark 4.1.1 treats
        // this as NULL-SAFE struct equality (`In.eval` + `InterpretedOrdering`:
        // returns only TRUE/FALSE for literal tuples, never NULL), so desugar with
        // `IS NOT DISTINCT FROM` per component — NOT null-unsafe `=`, which would
        // diverge on the NOT form with a NULL column. Corpus witness: `pr-005`.
        // Scalar-LHS IN keeps the byte-identical `InListExpression` path below.
        Expr::InList {
            expr,
            list,
            negated,
        } => match *expr {
            Expr::Tuple(cols) => build_row_in_chain(cols, list, negated, cte_scope),
            other => {
                let converted_list: Result<Vec<Expression>, EmissionError> =
                    list.into_iter().map(|e| lower_expr(e, cte_scope)).collect();
                Ok(Expression::InList(InListExpression {
                    expr: Box::new(lower_expr(other, cte_scope)?),
                    list: converted_list?,
                    negated,
                }))
            }
        },
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => Ok(Expression::Between(BetweenExpression {
            expr: Box::new(lower_expr(*expr, cte_scope)?),
            low: Box::new(lower_expr(*low, cte_scope)?),
            high: Box::new(lower_expr(*high, cte_scope)?),
            negated,
        })),
        Expr::IsNull(e) => Ok(Expression::Unary(UnaryExpression {
            op: UnaryOp::IsNull,
            operand: Box::new(lower_expr(*e, cte_scope)?),
        })),
        Expr::IsNotNull(e) => Ok(Expression::Unary(UnaryExpression {
            op: UnaryOp::IsNotNull,
            operand: Box::new(lower_expr(*e, cte_scope)?),
        })),
        // `a IS [NOT] DISTINCT FROM b` — null-safe (in)equality yielding a
        // non-null boolean. Lower onto τ's `IsDistinctFrom` substrate; the
        // `IS NOT` form sets `negated: true`. Corpus witnesses: `pr-001`,
        // `pr-002`.
        Expr::IsDistinctFrom(a, b) => Ok(is_distinct(
            lower_expr(*a, cte_scope)?,
            lower_expr(*b, cte_scope)?,
            false,
        )),
        Expr::IsNotDistinctFrom(a, b) => Ok(is_distinct(
            lower_expr(*a, cte_scope)?,
            lower_expr(*b, cte_scope)?,
            true,
        )),
        // `x IS [NOT] TRUE` / `x IS [NOT] FALSE` — 3VL boolean tests yielding a
        // non-null boolean. Lower onto τ's `IsDistinctFrom` substrate:
        //   `x IS TRUE`      ⟺ `x IS NOT DISTINCT FROM TRUE`  (negated: true)
        //   `x IS NOT TRUE`  ⟺ `x IS DISTINCT FROM TRUE`      (negated: false)
        // and likewise for FALSE. NULL IS TRUE = false, NULL IS NOT TRUE = true.
        // Corpus witness: `pr-006`.
        Expr::IsTrue(e) => Ok(is_distinct(
            lower_expr(*e, cte_scope)?,
            bool_literal(true),
            true,
        )),
        Expr::IsNotTrue(e) => Ok(is_distinct(
            lower_expr(*e, cte_scope)?,
            bool_literal(true),
            false,
        )),
        Expr::IsFalse(e) => Ok(is_distinct(
            lower_expr(*e, cte_scope)?,
            bool_literal(false),
            true,
        )),
        Expr::IsNotFalse(e) => Ok(is_distinct(
            lower_expr(*e, cte_scope)?,
            bool_literal(false),
            false,
        )),
        // `x LIKE ANY (p1, …, pn)` ≡ `(x LIKE p1) OR … OR (x LIKE pn)` (Spark
        // 4.1.1). sqlparser flags this with `any: true` and parses the pattern
        // list as an `Expr::Tuple`. Desugar at lowering into an OR-chain of the
        // ordinary single-pattern `Expression::Like` (so ESCAPE / NULL 3VL are
        // identical to a plain LIKE). This arm MUST precede the generic
        // `Expr::Like` arm below (which ignores `any`), or Rust's first-match
        // ordering would render it unreachable. Corpus witness: `pr-003`.
        Expr::Like {
            any: true,
            negated,
            expr,
            pattern,
            escape_char,
        } => {
            let patterns = match *pattern {
                Expr::Tuple(ps) => ps,
                // `LIKE ANY (subquery)` and other non-list shapes are not
                // implemented; do NOT fall through to the single-pattern arm —
                // that would silently drop the ANY quantifier (wrong answer).
                _ => bail_boundary_proto!(
                    "sql::like_any_non_list",
                    "LIKE ANY requires a parenthesized list of patterns"
                ),
            };
            build_like_chain(
                *expr,
                patterns,
                BinaryOp::Or,
                negated,
                escape_char,
                cte_scope,
            )
        }
        // `x LIKE ALL (p1, …, pn)` ≡ AND-chain of single LIKEs. sqlparser 0.61
        // has NO native `LIKE ALL`: it leaves `any: false` and mis-parses the
        // `ALL (…)` right-hand side as a function call `ALL(p1, …, pn)`. Detect
        // that deterministic parser artifact (see `is_like_all_artifact`) and
        // fold into an AND-chain. Guarded tightly so a real user function named
        // `all` cannot misfire; when the guard is false this arm does not match
        // and an ordinary `x LIKE 'p'` flows to the unchanged generic arm.
        // Corpus witness: `pr-004`.
        Expr::Like {
            any: false,
            negated,
            expr,
            pattern,
            escape_char,
        } if is_like_all_artifact(&pattern) => {
            let patterns = like_all_patterns(*pattern);
            build_like_chain(
                *expr,
                patterns,
                BinaryOp::And,
                negated,
                escape_char,
                cte_scope,
            )
        }
        Expr::Like {
            expr,
            pattern,
            negated,
            escape_char,
            ..
        } => Ok(Expression::Like(LikeExpression {
            value: Box::new(lower_expr(*expr, cte_scope)?),
            pattern: Box::new(lower_expr(*pattern, cte_scope)?),
            escape: escape_char.and_then(value_to_escape_char),
            negated,
            case_insensitive: false,
        })),
        // `x ILIKE ANY (…)` is not implemented. The generic `Expr::ILike` arm
        // below ignores `any` (via `..`), so without this guard it would
        // silently drop the quantifier and return a wrong answer. Fail loud
        // with an honest boundary error instead (no corpus witness for the
        // desugar, so a full ILIKE-ANY fold would be dead code).
        Expr::ILike { any: true, .. } => bail_boundary_proto!(
            "sql::ilike_any_unsupported",
            "ILIKE ANY is not implemented in Thunderduck"
        ),
        // `x ILIKE 'p'` — case-insensitive LIKE. Mirrors the `Expr::Like` arm
        // but flags `case_insensitive: true`, which emission renders as
        // `ILIKE`. `NOT ILIKE` rides the same `negated` field as `NOT LIKE`.
        // Corpus witness: `whr-012` (`name ILIKE 'a%'`).
        Expr::ILike {
            expr,
            pattern,
            negated,
            escape_char,
            ..
        } => Ok(Expression::Like(LikeExpression {
            value: Box::new(lower_expr(*expr, cte_scope)?),
            pattern: Box::new(lower_expr(*pattern, cte_scope)?),
            escape: escape_char.and_then(value_to_escape_char),
            negated,
            case_insensitive: true,
        })),
        // `x RLIKE 'p'` / `x REGEXP 'p'` — regex match. Lower to a `rlike`
        // FunctionCall; emission's `rlike | regexp_like | regexp` arm renders
        // the Spark-correct regexp semantics. `NOT RLIKE` has no negated field
        // on the FunctionCall, so wrap the call in a `NOT` unary (same
        // substrate as `Expr::UnaryOp { Not, .. }`). Corpus witness: `whr-013`
        // (`name RLIKE '^[A-D]'`).
        Expr::RLike {
            expr,
            pattern,
            negated,
            ..
        } => {
            let call = fn_call(
                "rlike",
                vec![
                    lower_expr(*expr, cte_scope)?,
                    lower_expr(*pattern, cte_scope)?,
                ],
            );
            Ok(wrap_not(call, negated))
        }
        // `x SIMILAR TO 'p'` — SQL-standard regex is WHOLE-STRING (anchored) and
        // Spark has no `SIMILAR TO` operator at all. Borrowing `rlike`
        // (unanchored Java-regex `find`) would silently give wrong answers (e.g.
        // `'abc' SIMILAR TO 'b'` is FALSE but rlike would be TRUE). Reject as a
        // Thunderduck-boundary error per ADR-022 rather than mis-lower.
        Expr::SimilarTo { .. } => bail_boundary_proto!(
            "sql::expr::similar_to",
            "SIMILAR TO (anchored SQL-standard regex) has no Spark equivalent"
        ),
        Expr::Wildcard(_) => Ok(Expression::Star(StarExpression { qualifier: None })),
        // Spark's `EXTRACT(<field> FROM <expr>)` and `DATE_PART(<field>, <expr>)`
        // parse to `Expr::Extract`. Lower to a FunctionCall of
        // `date_part('<field>', <expr>)` — DuckDB accepts this form for all
        // date/timestamp fields (year, month, day, hour, ...). Corpus
        // witness: `dt-016` (`extract(YEAR FROM hire_date)`).
        Expr::Extract { field, expr, .. } => {
            // Spark's `EXTRACT(<field> FROM <expr>)` lowers to a direct
            // function call — `year(x)`, `month(x)`, `day(x)`, etc. — so
            // that the existing type_inference / emission arms for those
            // functions apply (they return INTEGER, matching Spark).
            // Fall back to `date_part('<field>', <expr>)` (DOUBLE return)
            // only for fields without a dedicated Spark function name.
            // Corpus witness: `dt-016` (`extract(YEAR FROM hire_date)`).
            let field_str = format!("{field}").to_lowercase();
            let inner = lower_expr(*expr, cte_scope)?;
            let direct_fn = match field_str.as_str() {
                "year" => Some("year"),
                "month" => Some("month"),
                "day" | "dayofmonth" => Some("day"),
                "hour" => Some("hour"),
                "minute" => Some("minute"),
                "second" => Some("second"),
                "quarter" => Some("quarter"),
                "week" | "weekofyear" => Some("weekofyear"),
                "dayofweek" => Some("dayofweek"),
                "dayofyear" => Some("dayofyear"),
                _ => None,
            };
            Ok(match direct_fn {
                Some(name) => fn_call(name, vec![inner]),
                None => fn_call("date_part", vec![str_lit(field_str), inner]),
            })
        }
        // Spark's `SUBSTRING(<expr> FROM <from> [FOR <for>])` special syntax and
        // the `SUBSTR(<expr>, <from>, <for>)` shorthand both parse to
        // `Expr::Substring`. Lower to `substring(expr, from[, for])` — the
        // existing `substring` type_inference / emission arms apply. Corpus
        // witnesses: `fn-003` (SQL syntax), `fn-004` (`substr(...)`).
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            let mut args = vec![lower_expr(*expr, cte_scope)?];
            if let Some(from) = substring_from {
                args.push(lower_expr(*from, cte_scope)?);
            }
            if let Some(for_) = substring_for {
                args.push(lower_expr(*for_, cte_scope)?);
            }
            Ok(fn_call("substring", args))
        }
        // Spark's `TRIM([BOTH | LEADING | TRAILING] [<what> FROM] <expr>)`
        // special syntax. Map the trim side to the DuckDB function name
        // (`trim` / `ltrim` / `rtrim`) and emit `trim(expr[, what])`. DuckDB's
        // `trim(string, characters)` takes the string first and the trim
        // characters second, matching Spark's `TRIM(BOTH what FROM expr)` =
        // "remove `what` from both ends of `expr`". Corpus witness: `fn-005`.
        Expr::Trim {
            expr,
            trim_where,
            trim_what,
            ..
        } => {
            let name = match trim_where {
                Some(TrimWhereField::Leading) => "ltrim",
                Some(TrimWhereField::Trailing) => "rtrim",
                _ => "trim",
            };
            let mut args = vec![lower_expr(*expr, cte_scope)?];
            if let Some(what) = trim_what {
                args.push(lower_expr(*what, cte_scope)?);
            }
            Ok(fn_call(name, args))
        }
        // Spark's `POSITION(<substr> IN <str>)` special syntax. Lower to
        // `locate(substr, str)` (NOT `position` — DuckDB has no `position`
        // scalar; `locate` emits 1-based `strpos`). Corpus witness: `fn-006`.
        Expr::Position { expr, r#in } => Ok(fn_call(
            "locate",
            vec![lower_expr(*expr, cte_scope)?, lower_expr(*r#in, cte_scope)?],
        )),
        // Spark's `OVERLAY(<expr> PLACING <what> FROM <from> [FOR <for>])`
        // special syntax. Lower to `overlay(expr, what, from[, for])` — the
        // existing `overlay` emission arm rewrites it via substring/concat.
        // Corpus witness: `fn-007`.
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            let mut args = vec![
                lower_expr(*expr, cte_scope)?,
                lower_expr(*overlay_what, cte_scope)?,
                lower_expr(*overlay_from, cte_scope)?,
            ];
            if let Some(for_) = overlay_for {
                args.push(lower_expr(*for_, cte_scope)?);
            }
            Ok(fn_call("overlay", args))
        }
        Expr::Lambda(lambda) => {
            let params: Vec<String> = lambda.params.iter().map(|p| p.value.clone()).collect();
            let body = lower_expr(*lambda.body, cte_scope)?;
            // SparkSQL parses lambda-body identifiers as regular columns
            // (`Expr::Identifier("acc")` → `UnresolvedColumn(acc)`). The
            // analyzer treats `Lambda` opaquely (analyzer.rs:1747), so those
            // references never resolve. Rewrite them to `LambdaVariable`
            // so emission (emission.rs:1681) renders them as DuckDB lambda
            // parameters (`acc`, `x`). The protobuf front-end never hits this
            // — it receives `UnresolvedNamedLambdaVariable` directly.
            let body = rewrite_lambda_params_to_vars(body, &params);
            Ok(Expression::Lambda(LambdaExpression {
                params,
                body: Box::new(body),
            }))
        }
        Expr::Interval(iv) => lower_interval(iv),
        // Uncorrelated subqueries (scalar / IN / EXISTS). The inner plan is
        // lowered with the enclosing query's CTE scope so a subquery's
        // `FROM <cte>` inlines the CTE body rather than reading a same-named
        // catalog table — Spark shadows the table with the CTE (cte-006).
        // The analyzer rewrites `Unanalyzed` → `Analyzed` (correlated inner
        // refs fail resolution → honest Thunderduck boundary, ADR-022).
        Expr::Subquery(q) => Ok(Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(lower_query(*q, cte_scope)?)),
        })),
        Expr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(Expression::InSubquery(InSubquery {
            expr: Box::new(lower_expr(*expr, cte_scope)?),
            subquery: SubqueryPlan::Unanalyzed(Box::new(lower_query(*subquery, cte_scope)?)),
            negated,
        })),
        Expr::Exists { subquery, negated } => Ok(Expression::ExistsSubquery(ExistsSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(lower_query(*subquery, cte_scope)?)),
            negated,
        })),
        // Typed-string literals `DATE '...'` / `TIMESTAMP '...'` (lit-001,
        // lit-002). Spark's DATE/TIMESTAMP literals are NON-NULL constants, so
        // lower them to non-null `LiteralValue::Date`/`Timestamp` values (a
        // Literal is non-null by construction) rather than a `CAST(str AS ..)`
        // (nullable=TRUE). The string→epoch-days/-micros conversion is a
        // self-contained proleptic-Gregorian parser (no chrono dep). Malformed
        // input and other typed-string data types stay a Thunderduck boundary
        // (ADR-022).
        Expr::TypedString(ts) => lower_typed_string(ts),
        // sqlparser parses `CEIL(x)` / `FLOOR(x)` (and the 2-arg
        // `CEIL(x, s)` / `FLOOR(x, s)` and `... TO <field>` forms) into
        // dedicated `Expr::Ceil` / `Expr::Floor` nodes, NOT `Expr::Function`.
        // Lower them to the shared `FunctionCall("ceil"/"floor", ..)` shape so
        // the existing type-inference / emission arms apply. Corpus: num-001,
        // num-002, num-003.
        Expr::Ceil { expr, field } => lower_ceil_floor("ceil", *expr, field, cte_scope),
        Expr::Floor { expr, field } => lower_ceil_floor("floor", *expr, field, cte_scope),
        // Bracket-chain field access: `array(1,2,3)[0]`, `map('a',1)['a']`.
        // sqlparser parses these as `CompoundFieldAccess{root, access_chain}`.
        // Fold each subscript into a nested `ExtractValue`; the analyzer resolves
        // the extraction type from the child (array elem / map value / struct
        // field) and emission dispatches on that child type. Only bracket
        // `Subscript::Index` lands live (int index or string key — cx-001/cx-002);
        // dot-in-bracket and slices are honest Thunderduck boundaries.
        Expr::CompoundFieldAccess { root, access_chain } => {
            let mut expr = lower_expr(*root, cte_scope)?;
            for acc in access_chain {
                let extraction = match acc {
                    AccessExpr::Subscript(Subscript::Index { index }) => {
                        lower_expr(index, cte_scope)?
                    }
                    AccessExpr::Dot(_) => bail_boundary_proto!(
                        "sql::field_access::dot",
                        "dot-in-bracket-chain field access not supported in τ"
                    ),
                    AccessExpr::Subscript(Subscript::Slice { .. }) => bail_boundary_proto!(
                        "sql::field_access::slice",
                        "array slice not supported in τ"
                    ),
                };
                expr = Expression::ExtractValue(ExtractValueExpression {
                    child: Box::new(expr),
                    extraction: Box::new(extraction),
                });
            }
            Ok(expr)
        }
        other => bail_boundary_proto!(
            format!("sql::expr::{}", expr_kind(&other)),
            "expression shape not supported in τ"
        ),
    }
}

/// Lower a sqlparser `Expr::Ceil` / `Expr::Floor` node to a τ
/// `FunctionCall("ceil"/"floor", ..)`.
///
/// - `CeilFloorKind::DateTimeField(NoDateTime)` → plain 1-arg `ceil(x)`.
/// - `CeilFloorKind::Scale(n)` → 2-arg `ceil(x, n)` carrying the target scale as
///   an `Int` literal (Spark `RoundCeil`/`RoundFloor`). A non-integer scale
///   literal is a Thunderduck boundary.
/// - `CeilFloorKind::DateTimeField(<field>)` (the `... TO <unit>` datetime form)
///   is a separate Spark feature τ has not implemented — honest boundary.
fn lower_ceil_floor(
    name: &str,
    expr: Expr,
    field: CeilFloorKind,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    let inner = lower_expr(expr, cte_scope)?;
    let args = match field {
        CeilFloorKind::DateTimeField(DateTimeField::NoDateTime) => vec![inner],
        CeilFloorKind::Scale(v) => {
            let Some(t) = int_from_number_value(&v) else {
                bail_boundary_proto!(
                    format!("sql::{name}::non_integer_scale"),
                    "ceil/floor with a non-integer scale is not supported in τ"
                );
            };
            vec![
                inner,
                Expression::Literal(Literal {
                    value: LiteralValue::Int(t),
                    data_type: DataType::Integer,
                }),
            ]
        }
        CeilFloorKind::DateTimeField(other) => bail_boundary_proto!(
            format!("sql::{name}::datetime_field::{other:?}"),
            "ceil/floor TO <datetime-field> not supported in τ"
        ),
    };
    Ok(fn_call(name, args))
}

/// Parse a sqlparser numeric [`Value`] into an `i32` scale (accepts negatives).
/// Returns `None` for non-numeric values or numbers that are not integral / are
/// out of `i32` range.
fn int_from_number_value(v: &Value) -> Option<i32> {
    match v {
        Value::Number(s, _) => s.parse::<i32>().ok(),
        _ => None,
    }
}

/// Walk a lowered lambda body and replace every `UnresolvedColumn` whose name
/// matches one of `params` (and whose qualifier is `None`) with a
/// `LambdaVariable` of the same name. Handles nested Lambdas via shadowing:
/// if an inner lambda re-binds one of our params, that param is removed from
/// the "still-active" set for that subtree.
fn rewrite_lambda_params_to_vars(body: Expression, params: &[String]) -> Expression {
    if params.is_empty() {
        return body;
    }
    match body {
        Expression::UnresolvedColumn(u)
            if u.qualifier.is_none() && params.iter().any(|p| p == &u.name) =>
        {
            Expression::LambdaVariable(LambdaVariableExpression { name: u.name })
        }
        Expression::Lambda(inner) => {
            // Inner lambda's params shadow ours: drop them from the active set
            // before descending into the inner body.
            let remaining: Vec<String> = params
                .iter()
                .filter(|p| !inner.params.iter().any(|ip| ip == *p))
                .cloned()
                .collect();
            let new_body = rewrite_lambda_params_to_vars(*inner.body, &remaining);
            Expression::Lambda(LambdaExpression {
                params: inner.params,
                body: Box::new(new_body),
            })
        }
        // The historical hand-rolled walker left `InSubquery` and `Window`
        // untouched (subqueries/windows inside a Spark lambda body would
        // themselves be `UnsupportedProtoShape` from upstream lower_expr —
        // never reaching this rewrite). `map_children` DOES descend into
        // them, so pass them through explicitly to keep the rewrite
        // byte-identical.
        e @ (Expression::InSubquery(_) | Expression::Window(_)) => e,
        // Every other variant recurses structurally via `map_children`
        // (leaves come back unchanged).
        other => {
            let rewritten: Result<Expression, Infallible> =
                other.map_children(|c| Ok(rewrite_lambda_params_to_vars(c, params)));
            match rewritten {
                Ok(e) => e,
                Err(never) => match never {},
            }
        }
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Function(_) => "function",
        Expr::Subquery(_) => "subquery",
        Expr::Exists { .. } => "exists",
        Expr::InSubquery { .. } => "in_subquery",
        Expr::Between { .. } => "between",
        Expr::AnyOp { .. } => "any_op",
        Expr::AllOp { .. } => "all_op",
        Expr::Tuple(_) => "tuple",
        Expr::Array(_) => "array",
        Expr::Map(_) => "map",
        Expr::Interval(_) => "interval",
        Expr::Rollup(_) => "rollup",
        Expr::Cube(_) => "cube",
        Expr::GroupingSets(_) => "grouping_sets",
        Expr::Lambda(_) => "lambda",
        _ => "other",
    }
}

fn lower_binary_op(op: BinaryOperator) -> Result<BinaryOp, EmissionError> {
    Ok(match op {
        BinaryOperator::Plus => BinaryOp::Add,
        BinaryOperator::Minus => BinaryOp::Sub,
        BinaryOperator::Multiply => BinaryOp::Mul,
        BinaryOperator::Divide => BinaryOp::Div,
        BinaryOperator::Modulo => BinaryOp::Mod,
        BinaryOperator::Eq => BinaryOp::Eq,
        BinaryOperator::NotEq => BinaryOp::NotEq,
        BinaryOperator::Lt => BinaryOp::Lt,
        BinaryOperator::LtEq => BinaryOp::LtEq,
        BinaryOperator::Gt => BinaryOp::Gt,
        BinaryOperator::GtEq => BinaryOp::GtEq,
        BinaryOperator::And => BinaryOp::And,
        BinaryOperator::Or => BinaryOp::Or,
        BinaryOperator::StringConcat => BinaryOp::Concat,
        BinaryOperator::BitwiseAnd => BinaryOp::BitAnd,
        BinaryOperator::BitwiseOr => BinaryOp::BitOr,
        BinaryOperator::BitwiseXor => BinaryOp::BitXor,
        other => {
            bail_boundary_proto!(
                format!("sql::binary_op::{other:?}"),
                "binary operator not supported in τ"
            );
        }
    })
}

fn lower_function(f: Function, cte_scope: &CteScope) -> Result<Expression, EmissionError> {
    let name = object_name_to_string(&f.name);
    // Spark's `timestampadd(unit, quantity, ts)` / `timestampdiff(unit, start,
    // end)` carry the datetime-field UNIT (`MONTH`, `DAY`, …) as their first
    // argument, which sqlparser parses as `Expr::Identifier("MONTH")`. The
    // generic identifier arm (`lower_expr`) would lower that into an
    // `UnresolvedColumn`, so the analyzer would raise a spurious
    // `UnknownColumn { name: "MONTH" }`. Demote the unit to a string literal
    // (mirrors the `Expr::Extract` arm) and lower the remaining args through
    // the normal `function_arg_to_expr` path. Neither function takes an
    // `OVER (...)` clause.
    if name.eq_ignore_ascii_case("timestampadd") || name.eq_ignore_ascii_case("timestampdiff") {
        if f.over.is_some() {
            bail_boundary_proto!(
                format!("sql::window::{name}"),
                "OVER is not valid on timestampadd/timestampdiff",
            );
        }
        return lower_timestamp_unit_fn(name, f.args, cte_scope);
    }
    let over = f.over;
    let filter = f.filter;
    let (distinct, mut args) = lower_function_args(f.args, cte_scope)?;
    if let Some(pred) = filter {
        args = desugar_aggregate_filter(&name, args, *pred, cte_scope)?;
    }
    let call = Expression::FunctionCall(FunctionCall {
        name,
        args,
        distinct,
    });
    match over {
        None => Ok(call),
        Some(window) => wrap_window(call, window, cte_scope),
    }
}

/// Desugar an aggregate `FILTER (WHERE <pred>)` clause into a CASE inside
/// each aggregate argument. `agg(a) FILTER (WHERE p)` aggregates only rows
/// where `p` is TRUE, which is exactly `agg(CASE WHEN p THEN a END)` for
/// every NULL-skipping aggregate (count/sum/avg/min/max/…): non-matching
/// rows become NULL and are skipped. `count(*)`/`count()` has no argument
/// to wrap, so synthesize a single `CASE WHEN p THEN 1 END` (the matching
/// rows contribute a non-NULL `1`). `distinct` is preserved by the caller so
/// `count(DISTINCT x) FILTER (WHERE p)` →
/// `count(DISTINCT CASE WHEN p THEN x END)`.
/// Corpus witness: `agg-017`. Precedent: `count_if` desugars the same way.
fn desugar_aggregate_filter(
    name: &str,
    args: Vec<Expression>,
    pred: Expr,
    cte_scope: &CteScope,
) -> Result<Vec<Expression>, EmissionError> {
    // Spark only accepts `FILTER (WHERE …)` on aggregate functions; a
    // scalar like `abs(x) FILTER (WHERE p)` is rejected. Guard the desugar
    // so it never silently converts a non-aggregate into valid SQL.
    if !is_aggregate_function_name(name) {
        bail_boundary_proto!(
            "sql::filter_on_non_aggregate",
            format!("FILTER (WHERE …) is only supported on aggregate functions, not `{name}`")
        );
    }
    let p = lower_expr(pred, cte_scope)?;
    let wrap = |arg: Expression, cond: Expression| {
        Expression::CaseWhen(CaseWhenExpression {
            branches: vec![(cond, arg)],
            else_expr: None,
        })
    };
    Ok(
        if args.is_empty() || matches!(args.as_slice(), [Expression::Star(_)]) {
            vec![wrap(
                Expression::Literal(Literal {
                    value: LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
                p,
            )]
        } else {
            args.into_iter().map(|a| wrap(a, p.clone())).collect()
        },
    )
}

/// Wrap a lowered function call in its `OVER (...)` window.
///
/// Safety net: named-window references are normally rewritten into a
/// `WindowSpec` by `resolve_named_windows_in_select` before lowering.
/// Reaching the `NamedWindow` arm means the reference was never defined
/// (e.g. no WINDOW clause at all) — a Thunderduck-boundary error (ADR-022).
fn wrap_window(
    call: Expression,
    window: WindowType,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    match window {
        WindowType::WindowSpec(spec) => {
            let partition_by: Vec<Expression> = spec
                .partition_by
                .into_iter()
                .map(|e| lower_expr(e, cte_scope))
                .collect::<Result<_, _>>()?;
            let order_by: Vec<SortOrder> = spec
                .order_by
                .into_iter()
                .map(|o| lower_order_by_expr(o, cte_scope))
                .collect::<Result<_, _>>()?;
            let frame = lower_window_frame(spec.window_frame, cte_scope)?;
            Ok(Expression::Window(WindowFunction {
                func: Box::new(call),
                partition_by,
                order_by,
                frame,
            }))
        }
        WindowType::NamedWindow(ident) => bail_boundary_proto!(
            "sql::named_window::unresolved",
            format!("window `{}` is not defined in a WINDOW clause", ident.value)
        ),
    }
}

/// Lower `timestampadd(unit, quantity, ts)` / `timestampdiff(unit, start, end)`.
///
/// The leading datetime-field UNIT is demoted from the identifier/string it
/// parses as into an `Expression::Literal(String)`, so the analyzer never
/// mistakes it for a column reference. The remaining arguments lower through
/// the normal [`function_arg_to_expr`] path.
fn lower_timestamp_unit_fn(
    fn_name: String,
    args: FunctionArguments,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    let list = match args {
        FunctionArguments::List(list) => list,
        _ => bail_boundary_proto!(
            format!("sql::{fn_name}::args"),
            "timestampadd/timestampdiff require a positional argument list",
        ),
    };
    let mut arg_iter = list.args.into_iter();
    let unit_arg = arg_iter.next().ok_or_else(|| EmissionError::Unsupported {
        kind: UnsupportedKind::ProtoShape,
        name: format!("sql::{fn_name}::unit"),
        reason: format!("`{fn_name}` requires a leading datetime unit argument"),
    })?;
    let mut lowered = Vec::with_capacity(3);
    lowered.push(lower_timestamp_unit_arg(&fn_name, unit_arg, cte_scope)?);
    for a in arg_iter {
        lowered.push(function_arg_to_expr(a, cte_scope)?);
    }
    Ok(fn_call(fn_name, lowered))
}

/// Lower the leading UNIT argument of `timestampadd` / `timestampdiff` into a
/// string [`Literal`]. Accepts a bare field name (`MONTH`) — sqlparser's
/// `Expr::Identifier` — or a quoted string literal (`'MONTH'`).
fn lower_timestamp_unit_arg(
    fn_name: &str,
    arg: FunctionArg,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    let expr = match unnamed_or_named_expr(arg) {
        Ok(e) => e,
        Err(other) => bail_boundary_proto!(
            format!("sql::{fn_name}::unit::{other:?}"),
            "datetime unit must be a bare field name or string literal",
        ),
    };
    match expr {
        Expr::Identifier(ident) => Ok(str_lit(ident.value)),
        // A quoted string unit (`timestampadd('MONTH', …)`) lowers via the
        // normal value path; accept it only if it yields a string literal.
        other => {
            let lowered = lower_expr(other, cte_scope)?;
            if matches!(
                lowered,
                Expression::Literal(Literal {
                    value: LiteralValue::String(_),
                    ..
                })
            ) {
                Ok(lowered)
            } else {
                bail_boundary_proto!(
                    format!("sql::{fn_name}::unit"),
                    "datetime unit must be a bare field name or string literal",
                )
            }
        }
    }
}

/// Map a sqlparser [`SqlWindowFrame`] into τ's [`WindowFrame`].
///
/// `None` → no frame clause (emission omits it; DuckDB's default matches
/// Spark's). `GROUPS` frame units are a Thunderduck-boundary error (ADR-022).
fn lower_window_frame(
    frame: Option<SqlWindowFrame>,
    cte_scope: &CteScope,
) -> Result<Option<WindowFrame>, EmissionError> {
    let Some(SqlWindowFrame {
        units,
        start_bound,
        end_bound,
    }) = frame
    else {
        return Ok(None);
    };
    let unit = match units {
        WindowFrameUnits::Rows => FrameUnit::Rows,
        WindowFrameUnits::Range => FrameUnit::Range,
        WindowFrameUnits::Groups => {
            bail_boundary_proto!(
                "sql::window_frame::groups",
                "GROUPS window frame units are not supported"
            );
        }
    };
    let lower = lower_frame_bound(start_bound, cte_scope)?;
    // Shorthand `ROWS N PRECEDING` (no BETWEEN) → upper bound is CURRENT ROW.
    let upper = match end_bound {
        Some(b) => lower_frame_bound(b, cte_scope)?,
        None => FrameBoundary::CurrentRow,
    };
    Ok(Some(WindowFrame { unit, lower, upper }))
}

/// Map a single sqlparser [`WindowFrameBound`] into τ's [`FrameBoundary`].
///
/// sqlparser encodes the direction in the variant (`Preceding` / `Following`),
/// so the offset expression is the absolute magnitude — no sign re-application.
fn lower_frame_bound(
    bound: WindowFrameBound,
    cte_scope: &CteScope,
) -> Result<FrameBoundary, EmissionError> {
    Ok(match bound {
        WindowFrameBound::CurrentRow => FrameBoundary::CurrentRow,
        WindowFrameBound::Preceding(None) => FrameBoundary::UnboundedPreceding,
        WindowFrameBound::Following(None) => FrameBoundary::UnboundedFollowing,
        WindowFrameBound::Preceding(Some(e)) => {
            FrameBoundary::Preceding(Box::new(lower_expr(*e, cte_scope)?))
        }
        WindowFrameBound::Following(Some(e)) => {
            FrameBoundary::Following(Box::new(lower_expr(*e, cte_scope)?))
        }
    })
}

/// Build a `name → WindowSpec` map from the `WINDOW` clause and inline each
/// `NamedWindow` reference in the projection into its `WindowSpec`.
fn resolve_named_windows_in_select(select: &mut Select) -> Result<(), EmissionError> {
    if select.named_window.is_empty() {
        return Ok(());
    }
    let mut defs: HashMap<String, WindowSpec> = HashMap::with_capacity(select.named_window.len());
    for NamedWindowDefinition(ident, expr) in &select.named_window {
        match expr {
            NamedWindowExpr::WindowSpec(spec) => {
                defs.insert(ident.value.clone(), spec.clone());
            }
            // `WINDOW w AS other_window` (alias-of-window) — not represented in
            // τ's substrate; boundary error rather than silent drop (ADR-022).
            NamedWindowExpr::NamedWindow(_) => {
                bail_boundary_proto!(
                    "sql::named_window::alias_of_window",
                    format!("named window `{}` aliases another window", ident.value)
                );
            }
        }
    }
    for item in &mut select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                resolve_named_windows_in_expr(e, &defs)?;
            }
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => {}
        }
    }
    Ok(())
}

/// Rewrite every `Expr::Function` whose `OVER` clause is a `NamedWindow`
/// reference into an inline `WindowSpec`, descending through the composite
/// expression shapes a projection can nest a window call inside. Mirrors
/// [`expr_has_aggregate`]'s shape list — including the SQL special forms
/// (`Extract`/`Ceil`/`Floor`/`Substring`/`Position`/`Trim`/`Overlay`/
/// `CompoundFieldAccess`) — kept in lockstep to defeat the same "walker
/// missed a composite shape" bug class (different walker); the parity test
/// `expr_has_aggregate_classifier_table` guards the bool half against drift.
/// Deliberately does NOT descend into a
/// subquery (`Expr::Subquery`, `Expr::Exists`, or the subquery half of
/// `InSubquery`) — a `WINDOW` clause is scoped to its containing `SELECT`
/// (Spark), and a nested subquery resolves its own named windows via its own
/// `lower_select` → `resolve_named_windows_in_select` call.
fn resolve_named_windows_in_expr(
    expr: &mut Expr,
    defs: &HashMap<String, WindowSpec>,
) -> Result<(), EmissionError> {
    match expr {
        Expr::Function(f) => {
            if let Some(WindowType::NamedWindow(name)) = &f.over {
                let spec = defs.get(&name.value).require_proto(
                    "sql::named_window::unknown",
                    &format!(
                        "window `{}` is not defined in the WINDOW clause",
                        name.value
                    ),
                )?;
                f.over = Some(WindowType::WindowSpec(spec.clone()));
            }
            if let FunctionArguments::List(list) = &mut f.args {
                for arg in &mut list.args {
                    let fae = match arg {
                        FunctionArg::Unnamed(fae)
                        | FunctionArg::Named { arg: fae, .. }
                        | FunctionArg::ExprNamed { arg: fae, .. } => fae,
                    };
                    if let FunctionArgExpr::Expr(e) = fae {
                        resolve_named_windows_in_expr(e, defs)?;
                    }
                }
            }
        }
        Expr::Nested(inner)
        | Expr::UnaryOp { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            resolve_named_windows_in_expr(inner, defs)?;
        }
        Expr::BinaryOp { left, right, .. } => {
            resolve_named_windows_in_expr(left, defs)?;
            resolve_named_windows_in_expr(right, defs)?;
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(o) = operand.as_deref_mut() {
                resolve_named_windows_in_expr(o, defs)?;
            }
            for c in conditions {
                resolve_named_windows_in_expr(&mut c.condition, defs)?;
                resolve_named_windows_in_expr(&mut c.result, defs)?;
            }
            if let Some(e) = else_result.as_deref_mut() {
                resolve_named_windows_in_expr(e, defs)?;
            }
        }
        Expr::InList { expr, list, .. } => {
            resolve_named_windows_in_expr(expr, defs)?;
            for e in list {
                resolve_named_windows_in_expr(e, defs)?;
            }
        }
        // The subquery half of `InSubquery` is a separate window scope; only
        // the LHS expression is walked.
        Expr::InSubquery { expr, .. } => {
            resolve_named_windows_in_expr(expr, defs)?;
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            resolve_named_windows_in_expr(expr, defs)?;
            resolve_named_windows_in_expr(low, defs)?;
            resolve_named_windows_in_expr(high, defs)?;
        }
        Expr::Like {
            expr,
            pattern,
            any: _,
            ..
        }
        | Expr::ILike {
            expr,
            pattern,
            any: _,
            ..
        }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => {
            resolve_named_windows_in_expr(expr, defs)?;
            resolve_named_windows_in_expr(pattern, defs)?;
        }
        Expr::IsNull(e)
        | Expr::IsNotNull(e)
        | Expr::IsTrue(e)
        | Expr::IsNotTrue(e)
        | Expr::IsFalse(e)
        | Expr::IsNotFalse(e)
        | Expr::IsUnknown(e)
        | Expr::IsNotUnknown(e) => {
            resolve_named_windows_in_expr(e, defs)?;
        }
        Expr::IsDistinctFrom(a, b) | Expr::IsNotDistinctFrom(a, b) => {
            resolve_named_windows_in_expr(a, defs)?;
            resolve_named_windows_in_expr(b, defs)?;
        }
        Expr::Tuple(items) | Expr::Array(sqlparser::ast::Array { elem: items, .. }) => {
            for e in items {
                resolve_named_windows_in_expr(e, defs)?;
            }
        }
        Expr::Collate { expr, .. }
        | Expr::AtTimeZone {
            timestamp: expr, ..
        } => {
            resolve_named_windows_in_expr(expr, defs)?;
        }
        // ── SQL special forms ────────────────────────────────────────────
        // `&mut` mirror of `expr_has_aggregate`'s special-form arms — a
        // named-window ref can legally nest inside any of these (e.g.
        // `extract(YEAR FROM lag(ts) OVER w)`). Keep in lockstep; the parity
        // test `expr_has_aggregate_classifier_table` guards the bool walker.
        Expr::Extract { expr, .. } | Expr::Ceil { expr, .. } | Expr::Floor { expr, .. } => {
            resolve_named_windows_in_expr(expr, defs)?;
        }
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            resolve_named_windows_in_expr(expr, defs)?;
            if let Some(e) = substring_from.as_deref_mut() {
                resolve_named_windows_in_expr(e, defs)?;
            }
            if let Some(e) = substring_for.as_deref_mut() {
                resolve_named_windows_in_expr(e, defs)?;
            }
        }
        Expr::Position { expr, r#in } => {
            resolve_named_windows_in_expr(expr, defs)?;
            resolve_named_windows_in_expr(r#in, defs)?;
        }
        // `trim_characters` is elided via `..` — never produced under τ's
        // SparkDialect (always `None`), so recursing it would be a dead arm.
        Expr::Trim {
            expr, trim_what, ..
        } => {
            resolve_named_windows_in_expr(expr, defs)?;
            if let Some(e) = trim_what.as_deref_mut() {
                resolve_named_windows_in_expr(e, defs)?;
            }
        }
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            resolve_named_windows_in_expr(expr, defs)?;
            resolve_named_windows_in_expr(overlay_what, defs)?;
            resolve_named_windows_in_expr(overlay_from, defs)?;
            if let Some(e) = overlay_for.as_deref_mut() {
                resolve_named_windows_in_expr(e, defs)?;
            }
        }
        Expr::CompoundFieldAccess { root, access_chain } => {
            resolve_named_windows_in_expr(root, defs)?;
            for a in access_chain {
                match a {
                    AccessExpr::Subscript(Subscript::Index { index }) => {
                        resolve_named_windows_in_expr(index, defs)?;
                    }
                    AccessExpr::Dot(_) | AccessExpr::Subscript(Subscript::Slice { .. }) => {}
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Lower a sqlparser [`Interval`] literal into τ's [`IntervalExpression`].
///
/// Single-field intervals (`INTERVAL '90' DAY`, `INTERVAL 3 YEAR`, …) lower
/// here directly; compound (`X TO Y`) literals route to
/// [`lower_compound_interval`], which supports the `YEAR TO MONTH` and
/// `DAY TO SECOND` pairs. Every other compound pair, any precision-annotated
/// form, non-literal, or unrepresentable-field shape is a Thunderduck-boundary
/// error (ADR-022), never a RawSql fallback.
fn lower_interval(iv: Interval) -> Result<Expression, EmissionError> {
    if iv.last_field.is_some() {
        return lower_compound_interval(iv);
    }
    let n = extract_interval_int(&iv.value).require_proto(
        "sql::expr::interval::non_literal",
        "interval value must be an integer literal",
    )?;
    let field = iv.leading_field.as_ref().require_proto(
        "sql::expr::interval::no_field",
        "interval literal has no leading time field",
    )?;

    const MICROS_PER_SECOND: i64 = 1_000_000;
    const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
    const MICROS_PER_HOUR: i64 = 60 * MICROS_PER_MINUTE;

    let overflow = |unit: &str| EmissionError::Unsupported {
        kind: UnsupportedKind::ProtoShape,
        name: format!("sql::expr::interval::{unit}_overflow"),
        reason: format!("interval {unit} value overflows"),
    };

    // Map the field to its Spark unit name (used in the overflow error name)
    // plus the IntervalExpression slot it fills: `Months(factor)` multiplies
    // `n` into months (×12 for YEAR), `Days` carries `n` verbatim, and
    // `Micros(per_unit)` scales `n` into microseconds.
    enum Slot {
        Months(i32),
        Days,
        Micros(i64),
    }
    let (unit, slot) = match field {
        DateTimeField::Year | DateTimeField::Years => ("year", Slot::Months(12)),
        DateTimeField::Month | DateTimeField::Months => ("month", Slot::Months(1)),
        DateTimeField::Day | DateTimeField::Days => ("day", Slot::Days),
        DateTimeField::Hour | DateTimeField::Hours => ("hour", Slot::Micros(MICROS_PER_HOUR)),
        DateTimeField::Minute | DateTimeField::Minutes => {
            ("minute", Slot::Micros(MICROS_PER_MINUTE))
        }
        DateTimeField::Second | DateTimeField::Seconds => {
            ("second", Slot::Micros(MICROS_PER_SECOND))
        }
        other => {
            bail_boundary_proto!(
                "sql::expr::interval::unsupported_field",
                format!("interval field `{other}` is not representable")
            );
        }
    };
    let ie = match slot {
        // The ×1 MONTH multiply can't overflow, but keep the checked path
        // for uniformity with YEAR.
        Slot::Months(factor) => IntervalExpression {
            months: n.checked_mul(factor).ok_or_else(|| overflow(unit))?,
            days: 0,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        },
        Slot::Days => IntervalExpression {
            months: 0,
            days: n,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        },
        Slot::Micros(per_unit) => IntervalExpression {
            months: 0,
            days: 0,
            microseconds: i64::from(n)
                .checked_mul(per_unit)
                .ok_or_else(|| overflow(unit))?,
            kind: IntervalKind::Calendar,
        },
    };
    Ok(Expression::Interval(ie))
}

/// Extract a plain `i32` from an interval value expression — handles both
/// `INTERVAL '3' DAY` (string literal) and `INTERVAL 3 DAY` (numeric literal).
fn extract_interval_int(expr: &Expr) -> Option<i32> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => s.parse::<i32>().ok(),
            Value::Number(s, _) => s.parse::<i32>().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Extract the string value of a compound interval literal (`'1-2'`,
/// `'1 02:30:00'`). Compound ANSI interval values are always single-quoted
/// strings; a non-string value is a Thunderduck boundary.
fn extract_interval_string(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Value(v) => match &v.value {
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// Lower a compound (`X TO Y`) interval literal. Supports ONLY the singular
/// field pairs `YEAR TO MONTH` → [`IntervalKind::YearMonth`] and `DAY TO
/// SECOND` → [`IntervalKind::DayTime`], the only two pairs that τ's field-less
/// interval `DataType`s encode wire-exactly (per architecture-pass-3). Every
/// other pair, or any field precision, is the existing
/// `sql::expr::interval::compound` Thunderduck boundary (ADR-022).
fn lower_compound_interval(iv: Interval) -> Result<Expression, EmissionError> {
    if iv.leading_precision.is_some() || iv.fractional_seconds_precision.is_some() {
        bail_boundary_proto!(
            "sql::expr::interval::compound",
            "compound `INTERVAL X TO Y` literals with field precision are not supported"
        );
    }
    let leading = iv.leading_field.as_ref().require_proto(
        "sql::expr::interval::no_field",
        "compound interval literal has no leading time field",
    )?;
    let last = iv.last_field.as_ref().require_proto(
        "sql::expr::interval::compound",
        "compound interval literal has no trailing time field",
    )?;
    let value = extract_interval_string(&iv.value).require_proto(
        "sql::expr::interval::non_literal",
        "compound interval value must be a string literal",
    )?;

    match (leading, last) {
        (DateTimeField::Year, DateTimeField::Month) => {
            let months = parse_year_month_value(value)?;
            Ok(Expression::Interval(IntervalExpression {
                months,
                days: 0,
                microseconds: 0,
                kind: IntervalKind::YearMonth,
            }))
        }
        (DateTimeField::Day, DateTimeField::Second) => {
            let (days, microseconds) = parse_day_second_value(value)?;
            Ok(Expression::Interval(IntervalExpression {
                months: 0,
                days,
                microseconds,
                kind: IntervalKind::DayTime,
            }))
        }
        (l, r) => {
            bail_boundary_proto!(
                "sql::expr::interval::compound",
                format!("compound `INTERVAL {l} TO {r}` literals are not supported")
            );
        }
    }
}

/// Parse a Spark `YEAR TO MONTH` interval value `[+|-]y-m` into a total month
/// count (`sign*(12*y + m)`), matching Spark `IntervalUtils.fromYearMonthString`
/// (`m` in `0..=11`). Malformed / out-of-range strings surface the
/// `sql::expr::interval::year_month_format` Thunderduck boundary.
fn parse_year_month_value(value: &str) -> Result<i32, EmissionError> {
    let fmt_err = || EmissionError::Unsupported {
        kind: UnsupportedKind::ProtoShape,
        name: "sql::expr::interval::year_month_format".to_owned(),
        reason: format!("cannot parse YEAR TO MONTH interval value `{value}`"),
    };
    let trimmed = value.trim();
    let (sign, rest) = match trimmed.strip_prefix('-') {
        Some(r) => (-1i32, r),
        None => (1i32, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let (y_str, m_str) = rest.split_once('-').ok_or_else(fmt_err)?;
    let years = parse_ascii_digits_i64(y_str).ok_or_else(fmt_err)?;
    let months = parse_ascii_digits_i64(m_str).ok_or_else(fmt_err)?;
    if !(0..=11).contains(&months) {
        return Err(fmt_err());
    }
    let total = years
        .checked_mul(12)
        .and_then(|v| v.checked_add(months))
        .and_then(|v| i32::try_from(v).ok())
        .and_then(|v| v.checked_mul(sign))
        .ok_or_else(fmt_err)?;
    Ok(total)
}

/// Parse a Spark `DAY TO SECOND` interval value `[+|-]d h:m:s[.f]` into
/// `(days, microseconds)`, matching Spark `IntervalUtils.fromDayTimeString`
/// (`h<=23`, `m<=59`, `s<=59`; fraction 1-9 digits, right-padded to 6 and
/// TRUNCATED beyond microseconds). The sign applies to the whole value. The
/// total is enforced i64-representable as microseconds
/// (`|d|*86_400_000_000 + time_µs <= i64::MAX`) so the connect-server
/// `Duration(µs)` transcode cannot overflow. Malformed / out-of-range strings
/// surface the `sql::expr::interval::day_time_format` Thunderduck boundary.
fn parse_day_second_value(value: &str) -> Result<(i32, i64), EmissionError> {
    const MICROS_PER_DAY: i64 = 86_400_000_000;
    let fmt_err = || EmissionError::Unsupported {
        kind: UnsupportedKind::ProtoShape,
        name: "sql::expr::interval::day_time_format".to_owned(),
        reason: format!("cannot parse DAY TO SECOND interval value `{value}`"),
    };
    let trimmed = value.trim();
    let (sign, rest) = match trimmed.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let (day_str, time_str) = rest.split_once(' ').ok_or_else(fmt_err)?;
    let days = parse_ascii_digits_i64(day_str).ok_or_else(fmt_err)?;

    let (hms, frac_str) = match time_str.split_once('.') {
        Some((hms, frac)) => (hms, Some(frac)),
        None => (time_str, None),
    };
    let mut parts = hms.split(':');
    let h = parts.next().and_then(parse_ascii_digits_i64);
    let m = parts.next().and_then(parse_ascii_digits_i64);
    let s = parts.next().and_then(parse_ascii_digits_i64);
    let (h, m, s) = match (h, m, s) {
        (Some(h), Some(m), Some(s)) if parts.next().is_none() => (h, m, s),
        _ => return Err(fmt_err()),
    };
    if h > 23 || m > 59 || s > 59 {
        return Err(fmt_err());
    }
    let frac_us = match frac_str {
        Some(f) => {
            if f.is_empty() || f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return Err(fmt_err());
            }
            // Right-pad to 6 digits; truncate digits 7-9 toward zero.
            let mut buf: String = f.chars().take(6).collect();
            while buf.len() < 6 {
                buf.push('0');
            }
            buf.parse::<i64>().map_err(|_| fmt_err())?
        }
        None => 0,
    };

    let time_us = (h * 3600 + m * 60 + s)
        .checked_mul(1_000_000)
        .and_then(|v| v.checked_add(frac_us))
        .ok_or_else(fmt_err)?;
    // Total-i64-representability guard (microseconds), on the unsigned magnitude.
    days.checked_mul(MICROS_PER_DAY)
        .and_then(|v| v.checked_add(time_us))
        .ok_or_else(fmt_err)?;

    let days_signed = days.checked_mul(sign).ok_or_else(fmt_err)?;
    let micros_signed = time_us.checked_mul(sign).ok_or_else(fmt_err)?;
    let days_i32 = i32::try_from(days_signed).map_err(|_| fmt_err())?;
    Ok((days_i32, micros_signed))
}

/// Parse a non-empty ASCII-digit run into a non-negative `i64`. Rejects empty
/// strings and any non-digit byte (so `+`/`-`/whitespace fail), matching
/// Spark's `\d+` interval-component grammar.
fn parse_ascii_digits_i64(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// Lower a `DATE '...'` / `TIMESTAMP '...'` typed-string literal to a NON-NULL
/// `LiteralValue::Date`/`Timestamp` value (Spark's DATE/TIMESTAMP literals are
/// non-null constants). See the `Expr::TypedString` arm.
fn lower_typed_string(ts: TypedString) -> Result<Expression, EmissionError> {
    let is_timestamp = match &ts.data_type {
        SqlDataType::Date => false,
        SqlDataType::Timestamp(_, _) => true,
        other => {
            bail_boundary_proto!(
                format!("sql::typed_string::{other:?}"),
                "only DATE and TIMESTAMP typed-string literals are supported"
            );
        }
    };
    let value = ts.value.into_string().require_proto(
        "sql::typed_string::non_string_value",
        "typed-string literal value must be a string",
    )?;
    let (literal, data_type) = if is_timestamp {
        let micros = parse_timestamp_to_epoch_micros(&value).require_proto(
            "sql::typed_string::malformed",
            &format!("cannot parse TIMESTAMP literal `{value}`"),
        )?;
        (LiteralValue::Timestamp(micros), DataType::Timestamp)
    } else {
        let days = parse_date_to_epoch_days(&value).require_proto(
            "sql::typed_string::malformed",
            &format!("cannot parse DATE literal `{value}`"),
        )?;
        (LiteralValue::Date(days), DataType::Date)
    };
    Ok(Expression::Literal(Literal {
        value: literal,
        data_type,
    }))
}

/// Parse a `YYYY-MM-DD` date string into days since the Unix epoch
/// (1970-01-01), using the proleptic-Gregorian civil algorithm (Howard
/// Hinnant `days_from_civil`). Returns `None` on malformed input.
fn parse_date_to_epoch_days(s: &str) -> Option<i32> {
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;
    // Bound the year to Spark's DATE domain [1, 9999] (M1). This both matches
    // Spark's supported range and keeps `days_from_civil` (era * 146097) and the
    // downstream timestamp micros multiply (`days * 86_400_000_000`) far from
    // i64 overflow — no panic in debug, no silent wrap in release.
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    // Validate the day against the actual length of the month, leap-year aware
    // (H1). Spark ANSI rejects e.g. `2026-02-30`, `2026-04-31`, `2023-02-29`
    // rather than silently rolling over to a wrong date.
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let max_day = days_in_month[(month - 1) as usize];
    if !(1..=max_day).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) as i32)
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01 for a
/// proleptic-Gregorian `(year, month, day)` with `month ∈ [1,12]`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Parse a `YYYY-MM-DD HH:MM:SS[.ffffff]` timestamp string (space or `T`
/// separator; optional fractional seconds) into microseconds since the Unix
/// epoch. No timezone handling — treated as a session-local wall-clock instant,
/// matching how τ's `Timestamp` literal is interpreted. Returns `None` on
/// malformed input.
fn parse_timestamp_to_epoch_micros(s: &str) -> Option<i64> {
    let trimmed = s.trim();
    let (date_part, time_part) = match trimmed.split_once(['T', ' ']) {
        Some((d, t)) => (d, t),
        None => (trimmed, "00:00:00"),
    };
    let days = parse_date_to_epoch_days(date_part)? as i64;

    let (hms, frac) = match time_part.split_once('.') {
        Some((h, f)) => (h, Some(f)),
        None => (time_part, None),
    };
    let time_fields: Vec<&str> = hms.split(':').collect();
    if time_fields.len() != 3 {
        return None;
    }
    let hh: i64 = time_fields[0].parse().ok()?;
    let mm: i64 = time_fields[1].parse().ok()?;
    let ss: i64 = time_fields[2].parse().ok()?;
    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }

    // Fractional seconds → microseconds: pad/truncate the digits to exactly 6.
    let frac_micros: i64 = match frac {
        None => 0,
        Some(f) => {
            if f.is_empty() || !f.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let mut digits: String = f.chars().take(6).collect();
            while digits.len() < 6 {
                digits.push('0');
            }
            digits.parse().ok()?
        }
    };

    Some(days * 86_400_000_000 + (hh * 3600 + mm * 60 + ss) * 1_000_000 + frac_micros)
}

/// Derive `(precision, scale)` for a bare SQL decimal literal: the shared
/// value-derived computation
/// (`transpiler_v2::expression::decimal_value_precision_scale`, Apache Spark
/// `Decimal.set()`) plus this front-end's clamp. `100.25`→(5,2);
/// `3.142`→(4,3); `0.00`→(2,2).
fn decimal_literal_precision_scale(s: &str) -> (u8, u8) {
    let (raw_precision, scale) = decimal_value_precision_scale(s);
    // Clamp precision to DECIMAL's MAX_PRECISION = 38 (M2). A literal with more
    // than 38 significant digits must not yield `Decimal(precision > 38)`, which is
    // invalid in both Spark and DuckDB.
    let mut precision = raw_precision.min(38);
    if scale > precision {
        precision = scale.min(38);
    }
    (precision, scale)
}

fn lower_value(vw: ValueWithSpan) -> Result<Expression, EmissionError> {
    match vw.value {
        Value::Number(s, _) => {
            if let Ok(i) = s.parse::<i64>() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    Ok(Expression::Literal(Literal {
                        value: LiteralValue::Int(i as i32),
                        data_type: DataType::Integer,
                    }))
                } else {
                    Ok(Expression::Literal(Literal {
                        value: LiteralValue::Long(i),
                        data_type: DataType::Long,
                    }))
                }
            } else if s.contains('.') && !s.contains(['e', 'E']) {
                // Spark parses a fixed-point numeric literal (a `.` with no
                // exponent) as DECIMAL, not DOUBLE — e.g. `100.25` is
                // Decimal(5,2). Preserve the literal string to keep precision;
                // exponent forms (`1.5e3`) still route to Double below (lit-007).
                let (precision, scale) = decimal_literal_precision_scale(&s);
                Ok(Expression::Literal(Literal {
                    value: LiteralValue::Decimal {
                        value: s,
                        precision,
                        scale,
                    },
                    data_type: DataType::Decimal { precision, scale },
                }))
            } else if let Ok(d) = s.parse::<f64>() {
                Ok(Expression::Literal(Literal {
                    value: LiteralValue::Double(d),
                    data_type: DataType::Double,
                }))
            } else {
                Err(EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name: "sql::number_parse".to_owned(),
                    reason: format!("cannot parse numeric literal `{s}`"),
                })
            }
        }
        Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => Ok(str_lit(s)),
        Value::Boolean(b) => Ok(Expression::Literal(Literal {
            value: LiteralValue::Boolean(b),
            data_type: DataType::Boolean,
        })),
        Value::Null => Ok(Expression::Literal(Literal {
            value: LiteralValue::Null,
            data_type: DataType::Null,
        })),
        // Spark hex/binary literal `X'1F2A'` → a BINARY value carrying the
        // decoded bytes (`[0x1F, 0x2A]`). sqlparser hands us the inner hex
        // string ("1F2A"); decode it into a byte vector. Odd length or a
        // non-hex digit is a malformed literal → boundary error, never panic.
        Value::HexStringLiteral(s) => {
            let bytes = decode_hex_literal(&s)?;
            Ok(Expression::Literal(Literal {
                value: LiteralValue::Binary(bytes),
                data_type: DataType::Binary,
            }))
        }
        other => bail_boundary_proto!(
            format!("sql::value::{other:?}"),
            "literal value shape not supported in τ"
        ),
    }
}

/// Decode the inner text of a Spark hex/binary literal (`X'1F2A'` → `"1F2A"`)
/// into its raw bytes. Hex digits are taken in pairs (each pair is one byte).
/// An odd number of digits or any non-hex character is a malformed literal and
/// yields a Thunderduck-boundary error rather than a panic.
fn decode_hex_literal(s: &str) -> Result<Vec<u8>, EmissionError> {
    if !s.len().is_multiple_of(2) {
        bail_boundary_proto!(
            "sql::value::hex_odd_length",
            format!("hex literal `X'{s}'` has an odd number of digits")
        );
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    // The even-length guard above means `chunks_exact(2)` leaves no remainder.
    for pair in s.as_bytes().chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => bytes.push((h * 16 + l) as u8),
            _ => bail_boundary_proto!(
                "sql::value::hex_invalid_digit",
                format!("hex literal `X'{s}'` contains a non-hex character")
            ),
        }
    }
    Ok(bytes)
}

fn lower_data_type(dt: SqlDataType) -> Result<DataType, EmissionError> {
    use SqlDataType::*;
    Ok(match dt {
        Boolean | Bool => DataType::Boolean,
        TinyInt(_) | Int8(_) => DataType::Byte,
        SmallInt(_) | Int16 => DataType::Short,
        Int(_) | Integer(_) | Int32 => DataType::Integer,
        BigInt(_) | Int64 => DataType::Long,
        Real | Float(_) | Float32 => DataType::Float,
        Double(_) | DoublePrecision | Float64 => DataType::Double,
        Varchar(_) | Text | String(_) | Char(_) | CharacterVarying(_) => DataType::String,
        Bytea | Binary(_) | Varbinary(_) | Blob(_) => DataType::Binary,
        Date => DataType::Date,
        Timestamp(_, _) => DataType::Timestamp,
        Numeric(info) | Decimal(info) => decimal_from_exact_number(&info),
        // Spark uses LONG, STRING, etc. as type names that sqlparser does
        // not recognise as keywords — they arrive as `Custom(ObjectName)`.
        // Handle the common Spark aliases case-insensitively.
        Custom(ref name, ref modifiers) if modifiers.is_empty() => {
            match lower_spark_custom_type(name) {
                Some(mapped) => mapped,
                None => {
                    bail_boundary_proto!(
                        format!("sql::data_type::{dt:?}"),
                        "data type not supported in τ"
                    );
                }
            }
        }
        other => {
            bail_boundary_proto!(
                format!("sql::data_type::{other:?}"),
                "data type not supported in τ"
            );
        }
    })
}

/// Map Spark-specific type names that sqlparser parses as `Custom(ObjectName)`
/// to τ `DataType`. Returns `None` for unrecognised names.
fn lower_spark_custom_type(name: &ObjectName) -> Option<DataType> {
    let parts = &name.0;
    if parts.len() != 1 {
        return None;
    }
    let ident = match &parts[0] {
        ObjectNamePart::Identifier(id) => &id.value,
        ObjectNamePart::Function(_) => return None,
    };
    match ident.to_uppercase().as_str() {
        "LONG" => Some(DataType::Long),
        "SHORT" => Some(DataType::Short),
        "BYTE" => Some(DataType::Byte),
        "STRING" => Some(DataType::String),
        _ => None,
    }
}

fn decimal_from_exact_number(info: &ExactNumberInfo) -> DataType {
    match info {
        ExactNumberInfo::None => DataType::Decimal {
            precision: 38,
            scale: 18,
        },
        ExactNumberInfo::Precision(p) => DataType::Decimal {
            precision: (*p as u8).min(38),
            scale: 0,
        },
        ExactNumberInfo::PrecisionAndScale(p, s) => DataType::Decimal {
            precision: (*p as u8).min(38),
            scale: (*s as u8).min(38),
        },
    }
}

fn wrap_with_sort_limit(
    plan: CommonAst,
    order_by: Vec<OrderByExpr>,
    limit: Option<Expr>,
    offset: Option<Expr>,
    cte_scope: &CteScope,
) -> Result<CommonAst, EmissionError> {
    let limit_i = limit.map(expr_to_i64).transpose()?;
    let offset_i = offset.map(expr_to_i64).transpose()?;
    if order_by.is_empty() && limit_i.is_none() && offset_i.is_none() {
        return Ok(plan);
    }
    if order_by.is_empty() {
        if let Some(l) = limit_i {
            return Ok(CommonAst::new(CommonOp::Limit {
                input: Box::new(plan),
                limit: l,
                offset: offset_i,
            }));
        }
        // OFFSET-only.
        return Ok(CommonAst::new(CommonOp::Sort {
            input: Box::new(plan),
            order: vec![],
            limit: None,
            offset: offset_i,
        }));
    }
    let order = order_by
        .into_iter()
        .map(|o| lower_order_by_expr(o, cte_scope))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CommonAst::new(CommonOp::Sort {
        input: Box::new(plan),
        order,
        limit: limit_i,
        offset: offset_i,
    }))
}

fn lower_order_by_expr(ob: OrderByExpr, cte_scope: &CteScope) -> Result<SortOrder, EmissionError> {
    let direction = match ob.options.asc {
        Some(true) | None => SortDirection::Ascending,
        Some(false) => SortDirection::Descending,
    };
    let null_ordering = match ob.options.nulls_first {
        Some(true) => NullOrdering::NullsFirst,
        Some(false) => NullOrdering::NullsLast,
        None => match direction {
            SortDirection::Ascending => NullOrdering::NullsFirst,
            SortDirection::Descending => NullOrdering::NullsLast,
        },
    };
    Ok(SortOrder {
        expr: Box::new(lower_expr(ob.expr, cte_scope)?),
        direction,
        null_ordering,
    })
}

fn expr_to_i64(e: Expr) -> Result<i64, EmissionError> {
    match e {
        Expr::Value(ValueWithSpan {
            value: Value::Number(s, _),
            ..
        }) => s.parse::<i64>().map_err(|_| EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: "sql::limit_offset_parse".to_owned(),
            reason: format!("cannot parse LIMIT/OFFSET value `{s}` as i64"),
        }),
        other => bail_boundary_proto!(
            format!("sql::limit_offset_expr::{other:?}"),
            "LIMIT/OFFSET must be an integer literal in τ"
        ),
    }
}

fn object_name_to_string(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(id) => id.value.clone(),
            // ObjectNamePart::Function is a non-exhaustive tail variant for
            // Snowflake-style function-in-name syntax. Not reachable from
            // the SparkSQL shapes at A.2 — render its Display form so we
            // never silently drop information.
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn value_to_escape_char(v: Value) -> Option<char> {
    match v {
        Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => s.chars().next(),
        _ => None,
    }
}

/// Left-associatively fold a list of boolean `Expression`s with a single
/// [`BinaryOp`] (`AND`/`OR`). Returns `None` for an empty list so the caller can
/// emit its own context-specific boundary error. A single-element list returns
/// that element unwrapped (no `Binary` node). Shared by the `LIKE ANY/ALL` and
/// row-value `IN` desugars.
fn reduce_binary(exprs: Vec<Expression>, op: BinaryOp) -> Option<Expression> {
    exprs.into_iter().reduce(|acc, next| {
        Expression::Binary(BinaryExpression {
            op: op.clone(),
            left: Box::new(acc),
            right: Box::new(next),
        })
    })
}

/// Wrap `e` in a `NOT` unary iff `negated`, else return it unchanged. Shared tail
/// of the quantified-predicate desugars (`NOT LIKE ANY/ALL`, `NOT IN`).
fn wrap_not(e: Expression, negated: bool) -> Expression {
    if negated {
        Expression::Unary(UnaryExpression {
            op: UnaryOp::Not,
            operand: Box::new(e),
        })
    } else {
        e
    }
}

/// Desugar a row-value IN — `(c1,…,ck) IN ((v11,…,v1k), …, (vm1,…,vmk))` — into a
/// boolean chain that is bit-exact with Spark 4.1.1 `In.eval` over a struct LHS.
/// Spark builds the LHS/elements as non-null `CreateNamedStruct`s and matches with
/// `InterpretedOrdering` (NULL-SAFE: `null==null` matches, `null` vs non-null does
/// not), so row IN yields only TRUE/FALSE — never NULL — for literal tuples. Each
/// component is therefore `IS NOT DISTINCT FROM` (τ `IsDistinctFrom{negated:true}`,
/// Spark `<=>`), NOT null-unsafe `=` (which would wrongly yield NULL on the NOT form
/// with a NULL column). Components are AND-folded per tuple, tuples OR-folded, and
/// `negated` (`NOT IN`) wraps the whole chain in `NOT` (exact complement, also never
/// NULL). Mirrors the pass-138 `build_like_chain` reduce-fold + NOT-wrap.
///
/// Every RHS element must be an `Expr::Tuple` of arity == `cols.len()`; a non-tuple
/// element or an arity mismatch is a Thunderduck-boundary error (Spark rejects these
/// as `DATATYPE_MISMATCH.DATA_DIFF_TYPES` at analysis — lowering's only vocabulary is
/// the boundary channel). Empty list / empty LHS is likewise a boundary error, not a
/// panic. Corpus witness: `pr-005`.
fn build_row_in_chain(
    cols: Vec<Expr>,
    rows: Vec<Expr>,
    negated: bool,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    let arity = cols.len();
    if arity == 0 {
        bail_boundary_proto!(
            "sql::in_row::empty_lhs",
            "row IN requires at least one left-hand column"
        );
    }
    let lowered_cols = cols
        .into_iter()
        .map(|c| lower_expr(c, cte_scope))
        .collect::<Result<Vec<_>, _>>()?;

    let mut tuple_preds = Vec::with_capacity(rows.len());
    for row in rows {
        let vs = match row {
            Expr::Tuple(vs) => vs,
            other => bail_boundary_proto!(
                "sql::in_row::non_tuple_element",
                format!("row IN requires each right-hand element to be a tuple, got {other:?}")
            ),
        };
        if vs.len() != arity {
            bail_boundary_proto!(
                "sql::in_row::arity_mismatch",
                format!(
                    "row IN element has arity {} but the left-hand side has arity {arity}",
                    vs.len()
                )
            );
        }
        // NULL-safe per-component equality: `col IS NOT DISTINCT FROM value`.
        let mut eqs = Vec::with_capacity(arity);
        for (col, v) in lowered_cols.iter().zip(vs) {
            eqs.push(is_distinct(col.clone(), lower_expr(v, cte_scope)?, true));
        }
        let Some(and_chain) = reduce_binary(eqs, BinaryOp::And) else {
            // arity ≥ 1 guaranteed above, so this is unreachable; guard anyway.
            bail_boundary_proto!("sql::in_row::empty_tuple", "row IN tuple is empty");
        };
        tuple_preds.push(and_chain);
    }

    let Some(chain) = reduce_binary(tuple_preds, BinaryOp::Or) else {
        bail_boundary_proto!(
            "sql::in_row::empty_list",
            "row IN requires at least one right-hand tuple"
        );
    };
    Ok(wrap_not(chain, negated))
}

/// Fold a `LIKE ANY`/`LIKE ALL` pattern list into a boolean chain of ordinary
/// single-pattern `LIKE`s. `connective` is the NON-negated fold: [`BinaryOp::Or`]
/// for `ANY` (`LikeAny` = ∃ match), [`BinaryOp::And`] for `ALL` (`LikeAll` = ∀
/// match). Each element reuses τ's ordinary [`Expression::Like`] lowering, so
/// ESCAPE and NULL (Kleene 3VL) semantics are identical to a plain `LIKE`. A
/// single-element list yields a bare `Like` (no `Binary` node). An empty list is
/// a Thunderduck-boundary error rather than a panic.
///
/// `negated` (`NOT LIKE ANY/ALL`) FLIPS the quantifier — matching Spark's
/// `NotLikeAny`/`NotLikeAll` (`regexpExpressions.scala`): `¬∃ = ∀¬` and
/// `¬∀ = ∃¬`. So `NOT LIKE ANY` = `NOT(AND-chain)` (= `NOT LikeAll`) and
/// `NOT LIKE ALL` = `NOT(OR-chain)` (= `NOT LikeAny`). Concretely we fold with the
/// OPPOSITE connective, then wrap the whole chain in `NOT`; this reproduces
/// Spark exactly, including NULL 3VL. Corpus: `pr-003`, `pr-004`.
fn build_like_chain(
    value: Expr,
    patterns: Vec<Expr>,
    connective: BinaryOp,
    negated: bool,
    escape_char: Option<Value>,
    cte_scope: &CteScope,
) -> Result<Expression, EmissionError> {
    let value = lower_expr(value, cte_scope)?;
    let escape = escape_char.and_then(value_to_escape_char);
    // NOT flips the quantifier (¬∃ = ∀¬, ¬∀ = ∃¬): fold with the opposite
    // connective when negated, then wrap the chain in NOT below.
    let fold_op = match (&connective, negated) {
        (BinaryOp::Or, true) => BinaryOp::And,
        (BinaryOp::And, true) => BinaryOp::Or,
        (op, _) => op.clone(),
    };
    let mut likes = Vec::with_capacity(patterns.len());
    for p in patterns {
        likes.push(Expression::Like(LikeExpression {
            value: Box::new(value.clone()),
            pattern: Box::new(lower_expr(p, cte_scope)?),
            escape,
            // Negation is applied once to the whole chain below, not per element.
            negated: false,
            case_insensitive: false,
        }));
    }
    let Some(chain) = reduce_binary(likes, fold_op) else {
        bail_boundary_proto!(
            "sql::like_quantifier_empty",
            "LIKE ANY/ALL requires at least one pattern"
        );
    };
    Ok(wrap_not(chain, negated))
}

/// True iff `pattern` is sqlparser 0.61's mis-parse of `LIKE ALL (p1, …, pn)`:
/// a bare `ALL(p1, …, pn)` function call with positional args only and no other
/// function features. Because sqlparser has no native `LIKE ALL`, `ALL (…)` on
/// a LIKE right-hand side is always this artifact — and Spark's grammar has no
/// competing reading of `x LIKE all(...)` (it parses as `LIKE ALL`), so
/// recovering it here is Spark-correct, not a guess. The tight guard prevents a
/// real user function named `all` from misfiring.
fn is_like_all_artifact(pattern: &Expr) -> bool {
    let Expr::Function(f) = pattern else {
        return false;
    };
    // Must be a single, UNQUOTED identifier `all` (case-insensitive). A
    // backtick-quoted `` `all`(...) `` is a genuine user function call, and a
    // qualified `schema.all(...)` is not the artifact — neither should misfire.
    let is_bare_all = matches!(
        f.name.0.as_slice(),
        [ObjectNamePart::Identifier(id)]
            if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("all")
    );
    is_bare_all
        && !f.uses_odbc_syntax
        && matches!(f.parameters, FunctionArguments::None)
        && f.filter.is_none()
        && f.null_treatment.is_none()
        && f.over.is_none()
        && f.within_group.is_empty()
        && matches!(&f.args, FunctionArguments::List(l)
            if l.duplicate_treatment.is_none()
                && l.clauses.is_empty()
                && !l.args.is_empty()
                && l.args.iter().all(|a|
                    matches!(a, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))))
}

/// Extract the pattern expressions from a `LIKE ALL` artifact. Precondition:
/// [`is_like_all_artifact`] holds for `pattern`; if it does not, this returns an
/// empty `Vec` (which [`build_like_chain`] turns into a boundary error) rather
/// than panicking.
fn like_all_patterns(pattern: Expr) -> Vec<Expr> {
    let Expr::Function(f) = pattern else {
        return Vec::new();
    };
    let FunctionArguments::List(l) = f.args else {
        return Vec::new();
    };
    l.args
        .into_iter()
        .filter_map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser_v2::dialect::SparkDialect;
    use sqlparser::parser::Parser;

    fn parse(sql: &str) -> Result<CommonAst, EmissionError> {
        let dialect = SparkDialect;
        let mut stmts =
            Parser::parse_sql(&dialect, sql).map_err(|e| EmissionError::Unsupported {
                kind: UnsupportedKind::Op,
                name: "sql::parse".to_owned(),
                reason: e.to_string(),
            })?;
        assert_eq!(stmts.len(), 1);
        lower_statement(stmts.remove(0))
    }

    /// Parse a single SparkDialect scalar expression into a sqlparser `Expr`,
    /// for the `expr_has_aggregate` classifier parity table.
    fn parse_expr(sql: &str) -> Expr {
        let dialect = SparkDialect;
        Parser::new(&dialect)
            .try_with_sql(sql)
            .expect("tokenize")
            .parse_expr()
            .expect("parse expr")
    }

    /// Parse `sql`, require a Thunderduck-boundary error
    /// (`EmissionError::Unsupported` with [`UnsupportedKind::ProtoShape`]),
    /// and return the shape name for the caller's `assert_eq!`.
    fn boundary_shape(sql: &str) -> String {
        boundary_shape_of(parse(sql))
    }

    /// [`boundary_shape`] over a pre-built lowering result, for helpers called
    /// directly (e.g. `decode_hex_literal`) instead of through [`parse`].
    fn boundary_shape_of<T: std::fmt::Debug>(result: Result<T, EmissionError>) -> String {
        match result {
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name,
                ..
            }) => name,
            other => panic!("expected a ProtoShape boundary error, got {other:?}"),
        }
    }

    /// [`boundary_shape`] variant asserting only a stable shape-name prefix,
    /// for shapes that embed a debug-formatted payload after the prefix.
    fn assert_boundary_shape_prefix(sql: &str, prefix: &str) {
        let shape = boundary_shape(sql);
        assert!(
            shape.starts_with(prefix),
            "expected boundary shape starting with `{prefix}`, got `{shape}`"
        );
    }

    #[test]
    fn parse_select_literal_no_from() {
        let plan = parse("SELECT 1").expect("should parse");
        match plan.op {
            CommonOp::Project { input, projections } => {
                assert!(matches!(input.op, CommonOp::SingleRow));
                assert_eq!(projections.len(), 1);
                assert!(matches!(projections[0], Expression::Literal(_)));
            }
            _ => panic!("expected Project over SingleRow"),
        }
    }

    // ── timestampadd / timestampdiff unit demotion (intv-006 regression) ───
    //
    // sqlparser parses the leading datetime UNIT (`MONTH`, `DAY`, …) as an
    // `Expr::Identifier`; the generic identifier arm would lower it to an
    // `UnresolvedColumn`, and the analyzer would then raise a spurious
    // `UnknownColumn { name: "MONTH" }`. These tests pin the fix: the unit is
    // demoted to a string literal, and the plan analyzes to the Spark-parity
    // return type (TIMESTAMP for add, BIGINT/Long for diff).

    fn timestampadd_call(sql: &str) -> FunctionCall {
        let plan = parse(sql).expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        match projections.into_iter().next() {
            Some(Expression::FunctionCall(call)) => call,
            other => panic!("expected FunctionCall projection, got {other:?}"),
        }
    }

    #[test]
    fn parse_timestampadd_demotes_unit_to_string_literal() {
        let call = timestampadd_call("SELECT timestampadd(MONTH, 3, last_login) FROM t");
        assert!(call.name.eq_ignore_ascii_case("timestampadd"));
        assert_eq!(call.args.len(), 3);
        assert!(
            matches!(
                &call.args[0],
                Expression::Literal(Literal {
                    value: LiteralValue::String(u),
                    ..
                }) if u == "MONTH"
            ),
            "unit must be a string literal, got {:?}",
            call.args[0]
        );
        assert!(
            !call
                .args
                .iter()
                .any(|a| matches!(a, Expression::UnresolvedColumn(c) if c.name == "MONTH")),
            "unit must NOT lower to an UnresolvedColumn(MONTH)"
        );
    }

    #[test]
    fn parse_timestampdiff_demotes_unit_to_string_literal() {
        let call = timestampadd_call("SELECT timestampdiff(DAY, hire_date, last_login) FROM t");
        assert!(call.name.eq_ignore_ascii_case("timestampdiff"));
        assert_eq!(call.args.len(), 3);
        assert!(
            matches!(
                &call.args[0],
                Expression::Literal(Literal {
                    value: LiteralValue::String(u),
                    ..
                }) if u == "DAY"
            ),
            "unit must be a string literal, got {:?}",
            call.args[0]
        );
        assert!(
            !call
                .args
                .iter()
                .any(|a| matches!(a, Expression::UnresolvedColumn(c) if c.name == "DAY")),
            "unit must NOT lower to an UnresolvedColumn(DAY)"
        );
    }

    #[test]
    fn timestampadd_and_timestampdiff_analyze_to_spark_return_types() {
        use crate::transpiler_v2::analyzer::analyze;
        use crate::transpiler_v2::base_types::BaseTypes;
        use crate::types::{StructField, StructType};

        fn emp() -> StructType {
            StructType::new(vec![
                StructField::nullable("last_login", DataType::Timestamp),
                StructField::nullable("hire_date", DataType::Timestamp),
            ])
        }

        // timestampadd → TIMESTAMP, with no UnknownColumn { name: "MONTH" }.
        let plan = parse("SELECT timestampadd(MONTH, 3, last_login) FROM emp").expect("parse");
        let bt = BaseTypes::build_from_plan(&plan, |n| (n == "emp").then(emp));
        let typed = analyze(plan, &bt).expect("analyze must succeed (no UnknownColumn)");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(
            typed.resolved_schema.fields[0].data_type,
            DataType::Timestamp
        );

        // timestampdiff → BIGINT (Long), with no UnknownColumn { name: "DAY" }.
        let plan =
            parse("SELECT timestampdiff(DAY, hire_date, last_login) FROM emp").expect("parse");
        let bt = BaseTypes::build_from_plan(&plan, |n| (n == "emp").then(emp));
        let typed = analyze(plan, &bt).expect("analyze must succeed (no UnknownColumn)");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        assert_eq!(typed.resolved_schema.fields[0].data_type, DataType::Long);
    }

    #[test]
    fn parse_select_star_from_table() {
        let plan = parse("SELECT * FROM t").expect("should parse");
        match plan.op {
            CommonOp::Project { input, projections } => {
                assert_eq!(projections.len(), 1);
                assert!(matches!(projections[0], Expression::Star(_)));
                assert!(matches!(
                    input.op,
                    CommonOp::TableScan { ref table, .. } if table == "t"
                ));
            }
            _ => panic!("expected Project over TableScan"),
        }
    }

    #[test]
    fn parse_inline_values_lowers_to_values_op() {
        // Top-level `VALUES` lowers to `CommonOp::Values` with default
        // `col1..colN` names (pass-129).
        let plan = parse("VALUES (1, 'a'), (2, 'b')").expect("should parse");
        match plan.op {
            CommonOp::Values { rows, column_names } => {
                assert_eq!(rows.len(), 2, "two rows");
                assert_eq!(rows[0].len(), 2, "two columns per row");
                assert_eq!(rows[1].len(), 2, "two columns per row");
                assert_eq!(
                    column_names,
                    vec!["col1".to_owned(), "col2".to_owned()],
                    "default column names"
                );
            }
            _ => panic!("expected Values"),
        }
    }

    #[test]
    fn parse_range_table_function_lowers_to_table_function_not_scan() {
        // `FROM range(5)` must lower to `CommonOp::TableFunction`, NOT a bare
        // `TableScan{"range"}` (pass-141; the `..` used to swallow `args`).
        let plan = parse("SELECT id FROM range(5)").expect("should parse");
        match plan.op {
            CommonOp::Project { input, .. } => match input.op {
                CommonOp::TableFunction {
                    name,
                    args,
                    with_ordinality,
                } => {
                    assert_eq!(name, "range");
                    assert_eq!(args.len(), 1, "one arg: end=5");
                    assert!(matches!(args[0], Expression::Literal(_)));
                    assert!(!with_ordinality);
                }
                other => panic!("expected TableFunction, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn parse_range_table_function_with_alias_columns_renames_via_todf() {
        // `range(5) AS t(id2)` → AliasedRelation{ ToDf{ TableFunction, [id2] }, t }
        // — a user alias composes on top of the TVF via the shared
        // `apply_table_alias` helper (pass-141).
        let plan = parse("SELECT id2 FROM range(5) AS t(id2)").expect("should parse");
        let aliased = match plan.op {
            CommonOp::Project { input, .. } => *input,
            other => panic!("expected Project, got {other:?}"),
        };
        let todf = match aliased.op {
            CommonOp::AliasedRelation { input, alias } => {
                assert_eq!(alias, "t");
                *input
            }
            other => panic!("expected AliasedRelation, got {other:?}"),
        };
        let tf = match todf.op {
            CommonOp::ToDf {
                input,
                column_names,
            } => {
                assert_eq!(column_names, vec!["id2".to_owned()]);
                *input
            }
            other => panic!("expected ToDf, got {other:?}"),
        };
        match tf.op {
            CommonOp::TableFunction { name, args, .. } => {
                assert_eq!(name, "range");
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected TableFunction, got {other:?}"),
        }
    }

    // ── TableFactor::TableFunction / TableFactor::Function alias handling
    // (finding 3: the alias was silently dropped, unlike the sibling
    // Table-with-args / Derived branches). Both `TABLE(f(...))` and
    // `LATERAL f(...)` compose their alias via the shared `apply_table_alias`
    // helper, mirroring `parse_range_table_function_with_alias_columns_renames_via_todf`. ──

    #[test]
    fn table_function_table_syntax_with_alias_columns_renames_via_todf() {
        // `TABLE(range(3)) AS r(id)` → AliasedRelation{ ToDf{ TableFunction, [id] }, r }
        let plan = parse("SELECT r.id FROM TABLE(range(3)) AS r(id)").expect("should parse");
        let aliased = match plan.op {
            CommonOp::Project { input, .. } => *input,
            other => panic!("expected Project, got {other:?}"),
        };
        let todf = match aliased.op {
            CommonOp::AliasedRelation { input, alias } => {
                assert_eq!(alias, "r");
                *input
            }
            other => panic!("expected AliasedRelation, got {other:?}"),
        };
        let tf = match todf.op {
            CommonOp::ToDf {
                input,
                column_names,
            } => {
                assert_eq!(column_names, vec!["id".to_owned()]);
                *input
            }
            other => panic!("expected ToDf, got {other:?}"),
        };
        match tf.op {
            CommonOp::TableFunction { name, .. } => assert_eq!(name, "range"),
            other => panic!("expected TableFunction, got {other:?}"),
        }
    }

    #[test]
    fn table_function_lateral_syntax_with_alias_columns_renames_via_todf() {
        // `LATERAL range(3) AS r(id)` → AliasedRelation{ ToDf{ TableFunction, [id] }, r }
        let plan = parse("SELECT r.id FROM LATERAL range(3) AS r(id)").expect("should parse");
        let aliased = match plan.op {
            CommonOp::Project { input, .. } => *input,
            other => panic!("expected Project, got {other:?}"),
        };
        let todf = match aliased.op {
            CommonOp::AliasedRelation { input, alias } => {
                assert_eq!(alias, "r");
                *input
            }
            other => panic!("expected AliasedRelation, got {other:?}"),
        };
        let tf = match todf.op {
            CommonOp::ToDf {
                input,
                column_names,
            } => {
                assert_eq!(column_names, vec!["id".to_owned()]);
                *input
            }
            other => panic!("expected ToDf, got {other:?}"),
        };
        match tf.op {
            CommonOp::TableFunction { name, .. } => assert_eq!(name, "range"),
            other => panic!("expected TableFunction, got {other:?}"),
        }
    }

    #[test]
    fn table_function_alias_without_column_list_wraps_in_aliased_relation_only() {
        // `TABLE(range(3)) AS r` (no explicit column list) → AliasedRelation
        // directly over TableFunction, no ToDf hop.
        let plan = parse("SELECT * FROM TABLE(range(3)) AS r").expect("should parse");
        match plan.op {
            CommonOp::Project { input, .. } => match input.op {
                CommonOp::AliasedRelation { input, alias } => {
                    assert_eq!(alias, "r");
                    assert!(
                        matches!(input.op, CommonOp::TableFunction { .. }),
                        "expected bare TableFunction (no ToDf), got {:?}",
                        input.op
                    );
                }
                other => panic!("expected AliasedRelation, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn parse_bare_table_still_lowers_to_table_scan() {
        // Regression: `FROM emp` (args None) must keep the bare-table path.
        let plan = parse("SELECT * FROM emp").expect("should parse");
        match plan.op {
            CommonOp::Project { input, .. } => {
                assert!(
                    matches!(input.op, CommonOp::TableScan { ref table, .. } if table == "emp"),
                    "expected TableScan, got {:?}",
                    input.op
                );
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn parse_bare_from_values_with_alias_columns_renames_via_todf() {
        // `SELECT * FROM VALUES (..) AS t(n, s)` → Project over AliasedRelation
        // over ToDf(["n","s"]) over Values (pass-129 + pass-118 Derived arm).
        let plan = parse("SELECT * FROM VALUES (1, 'a') AS t(n, s)").expect("should parse");
        let aliased = match plan.op {
            CommonOp::Project { input, .. } => *input,
            other => panic!("expected Project, got {other:?}"),
        };
        let todf = match aliased.op {
            CommonOp::AliasedRelation { input, alias } => {
                assert_eq!(alias, "t");
                *input
            }
            other => panic!("expected AliasedRelation, got {other:?}"),
        };
        let values = match todf.op {
            CommonOp::ToDf {
                input,
                column_names,
            } => {
                assert_eq!(column_names, vec!["n".to_owned(), "s".to_owned()]);
                *input
            }
            other => panic!("expected ToDf, got {other:?}"),
        };
        match values.op {
            CommonOp::Values { rows, column_names } => {
                assert_eq!(rows.len(), 1);
                assert_eq!(column_names, vec!["col1".to_owned(), "col2".to_owned()]);
            }
            other => panic!("expected Values, got {other:?}"),
        }
    }

    #[test]
    fn parse_select_with_where() {
        let plan = parse("SELECT id FROM t WHERE id > 5").expect("should parse");
        match plan.op {
            CommonOp::Project { input, .. } => match input.op {
                CommonOp::Filter { input, .. } => {
                    assert!(matches!(input.op, CommonOp::TableScan { .. }));
                }
                _ => panic!("expected Filter under Project"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn parse_select_distinct_wraps_project_in_deduplicate() {
        let plan = parse("SELECT DISTINCT a, b FROM t").expect("should parse");
        match plan.op {
            CommonOp::Deduplicate { input, on_columns } => {
                assert!(on_columns.is_empty(), "plain DISTINCT dedupes all columns");
                assert!(
                    matches!(input.op, CommonOp::Project { .. }),
                    "Deduplicate must wrap the Project"
                );
            }
            _ => panic!("expected Deduplicate over Project"),
        }
    }

    #[test]
    fn parse_select_distinct_with_order_by_sorts_deduplicate() {
        let plan = parse("SELECT DISTINCT a FROM t ORDER BY a").expect("should parse");
        // Dedupe first, then order: Sort(Deduplicate(Project)).
        match plan.op {
            CommonOp::Sort { input, .. } => match input.op {
                CommonOp::Deduplicate { input, on_columns } => {
                    assert!(on_columns.is_empty());
                    assert!(matches!(input.op, CommonOp::Project { .. }));
                }
                _ => panic!("expected Deduplicate under Sort"),
            },
            _ => panic!("expected Sort over Deduplicate"),
        }
    }

    #[test]
    fn parse_select_distinct_on_rejected() {
        assert_eq!(
            boundary_shape("SELECT DISTINCT ON (a) a, b FROM t"),
            "sql::distinct_on"
        );
    }

    // ── ceil/floor lowering (num-001/002/003) ────────────────────────────

    #[test]
    fn ceil_1arg_lowers_to_single_arg_function_call() {
        let plan = parse("SELECT ceil(a) FROM t").expect("should parse");
        match single_projection(&plan) {
            Expression::FunctionCall(fc) => {
                assert_eq!(fc.name, "ceil");
                assert_eq!(fc.args.len(), 1);
                assert!(matches!(fc.args[0], Expression::UnresolvedColumn(_)));
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn floor_2arg_lowers_carrying_int_scale_literal() {
        let plan = parse("SELECT floor(x, 2) FROM t").expect("should parse");
        match single_projection(&plan) {
            Expression::FunctionCall(fc) => {
                assert_eq!(fc.name, "floor");
                assert_eq!(fc.args.len(), 2);
                assert!(matches!(
                    fc.args[1],
                    Expression::Literal(Literal {
                        value: LiteralValue::Int(2),
                        data_type: DataType::Integer,
                    })
                ));
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn ceil_to_datetime_field_is_boundary() {
        assert_boundary_shape_prefix("SELECT ceil(ts TO DAY) FROM t", "sql::ceil::datetime_field");
    }

    /// Extract the single projection expression under a top-level `Project`.
    fn single_projection(plan: &CommonAst) -> &Expression {
        match &plan.op {
            CommonOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 1);
                &projections[0]
            }
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn typed_string_date_lowers_to_nonnull_date_literal() {
        let plan = parse("SELECT DATE '2026-01-15' AS d").expect("should parse");
        let inner = match single_projection(&plan) {
            Expression::Alias(a) => a.expr.as_ref(),
            other => panic!("expected Alias, got {other:?}"),
        };
        // 2026-01-15 is 20468 days after 1970-01-01 (non-null literal).
        match inner {
            Expression::Literal(Literal {
                value: LiteralValue::Date(days),
                data_type: DataType::Date,
            }) => assert_eq!(*days, 20468),
            other => panic!("expected Date literal, got {other:?}"),
        }
    }

    #[test]
    fn typed_string_timestamp_lowers_to_nonnull_timestamp_literal() {
        let plan = parse("SELECT TIMESTAMP '2026-01-15 10:30:00' AS ts").expect("should parse");
        let inner = match single_projection(&plan) {
            Expression::Alias(a) => a.expr.as_ref(),
            other => panic!("expected Alias, got {other:?}"),
        };
        // 20468 days * 86_400_000_000 + (10*3600 + 30*60) * 1_000_000.
        match inner {
            Expression::Literal(Literal {
                value: LiteralValue::Timestamp(micros),
                data_type: DataType::Timestamp,
            }) => assert_eq!(*micros, 1_768_473_000_000_000),
            other => panic!("expected Timestamp literal, got {other:?}"),
        }
    }

    #[test]
    fn typed_string_malformed_date_is_boundary_error() {
        assert_eq!(
            boundary_shape("SELECT DATE 'nope' AS d"),
            "sql::typed_string::malformed"
        );
    }

    #[test]
    fn parse_date_to_epoch_days_known_anchors() {
        assert_eq!(parse_date_to_epoch_days("1970-01-01"), Some(0));
        assert_eq!(parse_date_to_epoch_days("2000-01-01"), Some(10957));
        assert_eq!(parse_date_to_epoch_days("2026-01-15"), Some(20468));
        assert_eq!(parse_date_to_epoch_days("nope"), None);
    }

    #[test]
    fn parse_date_rejects_invalid_calendar_days() {
        // H1: days that overrun the month must be rejected, not rolled over.
        assert_eq!(parse_date_to_epoch_days("2026-02-30"), None);
        assert_eq!(parse_date_to_epoch_days("2026-04-31"), None);
        // 2023 is not a leap year → Feb 29 is invalid.
        assert_eq!(parse_date_to_epoch_days("2023-02-29"), None);
        // Month out of range.
        assert_eq!(parse_date_to_epoch_days("2026-13-01"), None);
        assert_eq!(parse_date_to_epoch_days("2026-00-01"), None);
        // Day out of range.
        assert_eq!(parse_date_to_epoch_days("2026-01-00"), None);
    }

    #[test]
    fn parse_date_accepts_leap_day() {
        // 2024 is a leap year → Feb 29 is valid. 2024-02-29 is 19782 days
        // after 1970-01-01.
        assert_eq!(parse_date_to_epoch_days("2024-02-29"), Some(19782));
    }

    #[test]
    fn parse_date_rejects_out_of_range_year() {
        // M1: years outside Spark's DATE domain [1, 9999] are rejected without
        // overflow/panic in the civil-day arithmetic.
        assert_eq!(parse_date_to_epoch_days("99999-01-01"), None);
        assert_eq!(parse_date_to_epoch_days("0000-01-01"), None);
    }

    #[test]
    fn typed_string_invalid_calendar_date_is_boundary_error() {
        assert_eq!(
            boundary_shape("SELECT DATE '2026-02-30' AS d"),
            "sql::typed_string::malformed"
        );
        // Year out of range must also be a boundary error, not a panic.
        assert_eq!(
            boundary_shape("SELECT DATE '99999-01-01' AS d"),
            "sql::typed_string::malformed"
        );
    }

    #[test]
    fn decimal_literal_lowers_with_precision_and_scale() {
        let plan = parse("SELECT 100.25").expect("should parse");
        match single_projection(&plan) {
            Expression::Literal(Literal {
                value:
                    LiteralValue::Decimal {
                        value,
                        precision,
                        scale,
                    },
                data_type,
            }) => {
                assert_eq!(value, "100.25");
                assert_eq!(*precision, 5);
                assert_eq!(*scale, 2);
                assert_eq!(
                    *data_type,
                    DataType::Decimal {
                        precision: 5,
                        scale: 2
                    }
                );
            }
            other => panic!("expected Decimal literal, got {other:?}"),
        }
    }

    #[test]
    fn decimal_literal_precision_scale_three_digit_fraction() {
        let plan = parse("SELECT 3.142").expect("should parse");
        match single_projection(&plan) {
            Expression::Literal(Literal {
                value:
                    LiteralValue::Decimal {
                        precision, scale, ..
                    },
                ..
            }) => {
                assert_eq!(*precision, 4);
                assert_eq!(*scale, 3);
            }
            other => panic!("expected Decimal literal, got {other:?}"),
        }
    }

    #[test]
    fn integer_literal_stays_integer_not_decimal() {
        let plan = parse("SELECT 42").expect("should parse");
        assert!(matches!(
            single_projection(&plan),
            Expression::Literal(Literal {
                value: LiteralValue::Int(42),
                data_type: DataType::Integer,
            })
        ));
    }

    #[test]
    fn decimal_literal_precision_scale_helper_matches_spark() {
        assert_eq!(decimal_literal_precision_scale("100.25"), (5, 2));
        assert_eq!(decimal_literal_precision_scale("3.142"), (4, 3));
        assert_eq!(decimal_literal_precision_scale("0.00"), (2, 2));
    }

    #[test]
    fn decimal_literal_precision_clamped_to_max_38() {
        // M2: a literal with more than 38 significant integer digits must not
        // produce Decimal(precision > 38) — clamp to MAX_PRECISION = 38, matching
        // normalize_decimal_literal in v2_relation_converter.rs.
        let forty_digits = "1234567890123456789012345678901234567890.5";
        let (precision, scale) = decimal_literal_precision_scale(forty_digits);
        assert_eq!(precision, 38);
        assert_eq!(scale, 1);
    }

    /// Extract the `Filter` predicate immediately under a top-level `Project`.
    fn where_predicate(plan: &CommonAst) -> &Expression {
        match &plan.op {
            CommonOp::Project { input, .. } => match &input.op {
                CommonOp::Filter { condition, .. } => condition,
                _ => panic!("expected Filter under Project"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn parse_is_distinct_from() {
        let plan = parse("SELECT * FROM t WHERE a IS DISTINCT FROM b").expect("should parse");
        match where_predicate(&plan) {
            Expression::IsDistinctFrom(idf) => assert!(!idf.negated),
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    #[test]
    fn parse_is_not_distinct_from() {
        let plan = parse("SELECT * FROM t WHERE a IS NOT DISTINCT FROM b").expect("should parse");
        match where_predicate(&plan) {
            Expression::IsDistinctFrom(idf) => assert!(idf.negated),
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    #[test]
    fn parse_null_safe_equals_spaceship() {
        let plan = parse("SELECT * FROM t WHERE a <=> b").expect("should parse");
        match where_predicate(&plan) {
            Expression::IsDistinctFrom(idf) => assert!(idf.negated),
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    /// Assert the `where_predicate` is an `IsDistinctFrom` whose right operand
    /// is a boolean literal `expected_bool` and whose `negated` flag matches.
    fn assert_bool_test(plan: &CommonAst, expected_bool: bool, expected_negated: bool) {
        match where_predicate(plan) {
            Expression::IsDistinctFrom(idf) => {
                assert_eq!(idf.negated, expected_negated);
                match idf.right.as_ref() {
                    Expression::Literal(Literal {
                        value: LiteralValue::Boolean(b),
                        data_type: DataType::Boolean,
                    }) => assert_eq!(*b, expected_bool),
                    other => panic!("expected Boolean literal, got {other:?}"),
                }
            }
            other => panic!("expected IsDistinctFrom, got {other:?}"),
        }
    }

    #[test]
    fn parse_is_true() {
        let plan = parse("SELECT * FROM t WHERE a IS TRUE").expect("should parse");
        assert_bool_test(&plan, true, true);
    }

    #[test]
    fn parse_is_not_true() {
        let plan = parse("SELECT * FROM t WHERE a IS NOT TRUE").expect("should parse");
        assert_bool_test(&plan, true, false);
    }

    #[test]
    fn parse_is_false() {
        let plan = parse("SELECT * FROM t WHERE a IS FALSE").expect("should parse");
        assert_bool_test(&plan, false, true);
    }

    #[test]
    fn parse_is_not_false() {
        let plan = parse("SELECT * FROM t WHERE a IS NOT FALSE").expect("should parse");
        assert_bool_test(&plan, false, false);
    }

    #[test]
    fn parse_select_with_order_by_limit() {
        let plan =
            parse("SELECT id FROM t ORDER BY id DESC LIMIT 10 OFFSET 5").expect("should parse");
        match plan.op {
            CommonOp::Sort {
                order,
                limit,
                offset,
                ..
            } => {
                assert_eq!(order.len(), 1);
                assert_eq!(order[0].direction, SortDirection::Descending);
                assert_eq!(limit, Some(10));
                assert_eq!(offset, Some(5));
            }
            _ => panic!("expected Sort as top-level"),
        }
    }

    #[test]
    fn parse_select_with_group_by_and_aggregate() {
        let plan = parse("SELECT dept, COUNT(*) FROM t GROUP BY dept").expect("should parse");
        // Top-level is Aggregate (has GROUP BY).
        assert!(matches!(plan.op, CommonOp::Aggregate { .. }));
    }

    #[test]
    fn parse_group_by_having_lowers_into_aggregate_field_not_filter() {
        // HAVING must lower into the Aggregate's `having` field, NOT a Filter
        // wrapping the Aggregate.
        let plan = parse("SELECT dept, COUNT(*) FROM t GROUP BY dept HAVING COUNT(*) > 1")
            .expect("should parse");
        match plan.op {
            CommonOp::Aggregate { having, .. } => {
                assert!(having.is_some(), "HAVING should populate the having field");
            }
            other => panic!("expected top-level Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn parse_group_by_no_having_leaves_having_none() {
        let plan = parse("SELECT dept, COUNT(*) FROM t GROUP BY dept").expect("should parse");
        match plan.op {
            CommonOp::Aggregate { having, .. } => assert!(having.is_none()),
            other => panic!("expected top-level Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn parse_group_by_rollup() {
        let plan =
            parse("SELECT a, b, COUNT(*) FROM t GROUP BY ROLLUP (a, b)").expect("should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::Rollup);
                // `ROLLUP (a, b)` flattens to two flat grouping columns.
                assert_eq!(grouping.len(), 2);
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_group_by_all_groups_by_non_aggregate_items() {
        // GROUP BY ALL groups by the non-aggregate SELECT items (a, b), not count(*).
        let plan = parse("SELECT a, b, COUNT(*) FROM t GROUP BY ALL").expect("should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                aggregates,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::GroupBy);
                assert_eq!(grouping.len(), 2, "GROUP BY ALL groups by a, b");
                assert_eq!(aggregates.len(), 3, "projection is a, b, count(*)");
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_order_by_all_orders_by_every_output_column() {
        let plan = parse("SELECT a, b FROM t ORDER BY ALL").expect("should parse");
        match plan.op {
            CommonOp::Sort { order, .. } => {
                assert_eq!(order.len(), 2, "ORDER BY ALL orders by both output columns");
            }
            _ => panic!("expected Sort over the projection"),
        }
    }

    #[test]
    fn parse_group_by_cube() {
        let plan =
            parse("SELECT a, b, COUNT(*) FROM t GROUP BY CUBE (a, b)").expect("should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::Cube);
                assert_eq!(grouping.len(), 2);
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_group_by_grouping_sets_lowers_flat_grouping_and_index_sets() {
        // `GROUPING SETS ((a), (b))` → flat grouping [a, b], per-set membership
        // [[0], [1]] indexing into the flat list.
        let plan = parse("SELECT a, b, COUNT(*) FROM t GROUP BY GROUPING SETS ((a), (b))")
            .expect("GROUPING SETS should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                grouping_sets,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::GroupingSets);
                assert_eq!(grouping.len(), 2, "flat distinct grouping cols a, b");
                assert_eq!(grouping_sets, vec![vec![0usize], vec![1usize]]);
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn parse_group_by_ordinal_resolves_to_select_item() {
        // `spark.sql.groupByOrdinal=true`: `GROUP BY 1` groups by the 1st
        // SELECT item (`dept_id`), NOT the literal `1`.
        let plan = parse("SELECT dept_id, count(*) FROM emp GROUP BY 1").expect("should parse");
        match plan.op {
            CommonOp::Aggregate { grouping, .. } => {
                assert_eq!(grouping.len(), 1);
                match &grouping[0] {
                    Expression::UnresolvedColumn(u) => assert_eq!(u.name, "dept_id"),
                    other => panic!("expected UnresolvedColumn(dept_id), got {other:?}"),
                }
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn unaliased_count_star_gets_sparksql_count_one_name() {
        // SparkSQL names an unaliased `count(*)` column `count(1)` (Spark
        // rewrites `count(*)` → `count(1)`), diverging from the DataFrame
        // `.count()` name. The last aggregate must be `count(*) AS count(1)`.
        let plan = parse("SELECT dept_id, count(*) FROM emp GROUP BY 1").expect("should parse");
        match plan.op {
            CommonOp::Aggregate { aggregates, .. } => {
                let last = aggregates.last().expect("count aggregate present");
                match last {
                    Expression::Alias(a) => {
                        assert_eq!(a.alias, "count(1)");
                        assert!(
                            matches!(a.expr.as_ref(), Expression::FunctionCall(f) if f.name.eq_ignore_ascii_case("count")),
                            "aliased expr must be the count call, got {:?}",
                            a.expr
                        );
                    }
                    other => panic!("expected Alias(count(*), \"count(1)\"), got {other:?}"),
                }
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn parse_group_by_ordinal_out_of_range_rejected() {
        // `GROUP BY 5` with only 2 SELECT items is out of range.
        assert_eq!(
            boundary_shape("SELECT dept_id, count(*) FROM emp GROUP BY 5"),
            "sql::group_by_position"
        );
    }

    #[test]
    fn parse_group_by_ordinal_pointing_at_aggregate_rejected() {
        // `GROUP BY 2` references `count(*)`, an aggregate select item.
        assert_eq!(
            boundary_shape("SELECT dept_id, count(*) FROM emp GROUP BY 2"),
            "sql::group_by_position_aggregate"
        );
    }

    // ── expr_has_aggregate descends into non-aggregate function args
    // (finding 1: `abs(count(x))` used to misclassify the whole statement) ──

    #[test]
    fn aggregate_nested_inside_non_aggregate_function_is_still_aggregate_query() {
        // `abs(count(x))` — the outer call `abs` is not an aggregate, but its
        // argument `count(x)` is; the whole SELECT must still lower to a
        // global Aggregate, not a Project that silently drops the aggregation.
        let plan = parse("SELECT abs(count(l_quantity)) FROM lineitem").expect("should parse");
        match plan.op {
            CommonOp::Aggregate { grouping, .. } => {
                assert!(grouping.is_empty(), "no GROUP BY ⇒ global aggregate");
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn group_by_all_excludes_items_with_nested_aggregate_in_function_args() {
        // `GROUP BY ALL` must exclude `abs(count(*))` from the grouping list
        // (it contains an aggregate), grouping only by `l_returnflag`.
        let plan = parse("SELECT l_returnflag, abs(count(*)) FROM lineitem GROUP BY ALL")
            .expect("should parse");
        match plan.op {
            CommonOp::Aggregate { grouping, .. } => {
                assert_eq!(
                    grouping.len(),
                    1,
                    "GROUP BY ALL groups only by l_returnflag"
                );
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn group_by_ordinal_pointing_at_nested_aggregate_in_function_args_rejected() {
        // `GROUP BY 1` points at `abs(count(*))`, which contains an aggregate
        // nested inside `abs`'s argument.
        assert_eq!(
            boundary_shape("SELECT abs(count(*)) FROM t GROUP BY 1"),
            "sql::group_by_position_aggregate"
        );
    }

    // ── SQL special-form aggregate classification (agg-022 / agg-023) ──────
    //
    // sqlparser parses EXTRACT / SUBSTRING / POSITION / TRIM / OVERLAY /
    // CEIL / FLOOR / bracket-access to dedicated `Expr` variants (NOT
    // `Expr::Function`); `expr_has_aggregate` must descend into them so an
    // aggregate nested inside is not mis-classified as a grouping key under
    // GROUP BY ALL. This table parses real SparkDialect SQL exprs (catching
    // both a missed arm AND a wrong-field-shape assumption).

    #[test]
    fn expr_has_aggregate_classifier_table() {
        let positive = [
            "extract(year from max(ts))",
            "substring(max(name) from 1 for 2)",
            "substring(name from 1 for max(n))",
            "overlay(name placing 'x' from 1 for max(n))",
            "position('x' in max(name))",
            "trim(both max(t) from name)",
            "ceil(max(salary))",
            "arr[max(i)]",
            "collect_list(name)[0]",
        ];
        for sql in positive {
            assert!(
                expr_has_aggregate(&parse_expr(sql)),
                "`{sql}` should classify as containing an aggregate"
            );
        }

        let negative = ["ceil(salary, 2)", "extract(year from hire_date)"];
        for sql in negative {
            assert!(
                !expr_has_aggregate(&parse_expr(sql)),
                "`{sql}` should NOT classify as containing an aggregate"
            );
        }
    }

    #[test]
    fn group_by_all_excludes_special_form_wrapped_aggregate() {
        // agg-022: `extract(YEAR FROM max(last_login))` contains an aggregate,
        // so GROUP BY ALL must group by `dept_id` only.
        let plan =
            parse("SELECT dept_id, extract(YEAR FROM max(last_login)) y FROM emp GROUP BY ALL")
                .expect("should parse");
        match plan.op {
            CommonOp::Aggregate { grouping, .. } => {
                assert_eq!(grouping.len(), 1, "GROUP BY ALL groups only by dept_id");
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn group_by_all_excludes_substring_wrapped_aggregate() {
        // agg-023: `substring(max(name) FROM 1 FOR 2)` contains an aggregate,
        // so GROUP BY ALL must group by `dept_id` only.
        let plan =
            parse("SELECT dept_id, substring(max(name) FROM 1 FOR 2) s FROM emp GROUP BY ALL")
                .expect("should parse");
        match plan.op {
            CommonOp::Aggregate { grouping, .. } => {
                assert_eq!(grouping.len(), 1, "GROUP BY ALL groups only by dept_id");
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn special_form_wrapped_aggregate_no_group_by_is_global_aggregate() {
        // The secondary blast radius: with no GROUP BY, an aggregate nested
        // inside EXTRACT must promote the query to a global Aggregate, not a
        // plain Project.
        let plan =
            parse("SELECT extract(YEAR FROM max(last_login)) FROM emp").expect("should parse");
        match plan.op {
            CommonOp::Aggregate { grouping, .. } => {
                assert!(grouping.is_empty(), "no GROUP BY ⇒ global aggregate");
            }
            other => panic!("expected global Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn named_window_ref_inside_extract_is_inlined() {
        // The `&mut` mirror walker must descend into EXTRACT to inline the
        // named window `w`; without the arm this raised a spurious
        // `sql::named_window::unknown` boundary error.
        parse(
            "SELECT extract(YEAR FROM lag(last_login) OVER w) FROM emp WINDOW w AS (ORDER BY id)",
        )
        .expect("named window inside EXTRACT should inline and lower Ok");
    }

    #[test]
    fn unknown_named_window_inside_extract_rejected() {
        // Negative: the arm reaches the defs lookup, so an undefined window
        // still yields the honest boundary shape.
        assert_eq!(
            boundary_shape(
                "SELECT extract(YEAR FROM lag(last_login) OVER wrong) FROM emp WINDOW w AS (ORDER BY id)"
            ),
            "sql::named_window::unknown"
        );
    }

    #[test]
    fn window_function_alone_is_not_an_aggregate_query() {
        // `sum(x) OVER ()` alone must remain a Project — the aggregate IS the
        // window function itself (Spark excludes this from aggregate detection).
        let plan = parse("SELECT sum(x) OVER () FROM t").expect("should parse");
        assert!(matches!(plan.op, CommonOp::Project { .. }));
    }

    #[test]
    fn aggregate_nested_inside_window_function_args_is_aggregate_query() {
        // `sum(count(x)) OVER ()` — the window function's own argument is an
        // aggregate, so the query IS an aggregate query (unlike the bare
        // `sum(x) OVER ()` case above).
        let plan = parse("SELECT sum(count(x)) OVER () FROM t").expect("should parse");
        assert!(matches!(plan.op, CommonOp::Aggregate { .. }));
    }

    #[test]
    fn aggregate_nested_inside_window_order_by_is_aggregate_query() {
        // `rank() OVER (ORDER BY count(*))` — the aggregate is in the window
        // spec's ORDER BY, not the window call's own args; still an aggregate
        // query.
        let plan = parse("SELECT rank() OVER (ORDER BY count(*)) FROM t").expect("should parse");
        assert!(matches!(plan.op, CommonOp::Aggregate { .. }));
    }

    #[test]
    fn parse_group_by_nested_rollup_term_rejected() {
        // `ROLLUP ((a, b), c)` has a multi-column grouping term Spark treats as
        // a distinct level; τ's flat grouping can't represent it — reject rather
        // than silently flatten to `ROLLUP(a, b, c)` (ADR-022 loud-fail).
        assert_eq!(
            boundary_shape("SELECT a, b, COUNT(*) FROM t GROUP BY ROLLUP ((a, b), c)"),
            "sql::grouping_sets"
        );
    }

    #[test]
    fn parse_group_by_with_rollup() {
        // Postfix `GROUP BY <cols> WITH ROLLUP` → flat grouping, Rollup kind,
        // empty grouping_sets (membership only applies to GROUPING SETS).
        // Corpus witness: `gx-010`.
        let plan = parse("SELECT a, b, COUNT(*) FROM t GROUP BY a, b WITH ROLLUP")
            .expect("WITH ROLLUP should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                grouping_sets,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::Rollup);
                assert_eq!(grouping.len(), 2);
                assert!(grouping_sets.is_empty());
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn parse_group_by_with_cube() {
        // Postfix `GROUP BY <cols> WITH CUBE` → flat grouping, Cube kind. No
        // corpus witness yet — this test keeps the Cube modifier arm live.
        let plan = parse("SELECT a, b, COUNT(*) FROM t GROUP BY a, b WITH CUBE")
            .expect("WITH CUBE should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                grouping_sets,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::Cube);
                assert_eq!(grouping.len(), 2);
                assert!(grouping_sets.is_empty());
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn parse_group_by_with_totals_rejected() {
        // ClickHouse `WITH TOTALS` is not a Spark shape — boundary reject.
        assert_eq!(
            boundary_shape("SELECT a, COUNT(*) FROM t GROUP BY a WITH TOTALS"),
            "sql::group_by_modifiers"
        );
    }

    #[test]
    fn parse_group_by_grouping_sets_with_empty_set_and_dedup() {
        // gx-003 shape: `((a, b), (a), ())`. Flat distinct grouping [a, b];
        // set membership [[0, 1], [0], []] (empty inner vec = grand-total set).
        let plan = parse("SELECT a, b, COUNT(*) FROM t GROUP BY GROUPING SETS ((a, b), (a), ())")
            .expect("GROUPING SETS should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                grouping_sets,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::GroupingSets);
                assert_eq!(grouping.len(), 2, "flat distinct grouping cols a, b");
                assert_eq!(
                    grouping_sets,
                    vec![vec![0usize, 1usize], vec![0usize], Vec::<usize>::new()]
                );
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn parse_select_unresolved_column_has_plan_id_none() {
        // Open Decision 12 anchor.
        let plan = parse("SELECT id FROM t").expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        match &projections[0] {
            Expression::UnresolvedColumn(u) => assert_eq!(u.plan_id, None),
            _ => panic!("expected UnresolvedColumn"),
        }
    }

    /// Unwrap the input relation beneath a top-level `Project`.
    fn project_input(plan: CommonAst) -> CommonAst {
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project as top-level, got {:?}", plan.op);
        };
        *input
    }

    /// Require an `AliasedRelation` carrying `alias` and return its input.
    fn expect_aliased(input: CommonAst, alias: &str) -> CommonAst {
        let CommonOp::AliasedRelation { alias: got, input } = input.op else {
            panic!("expected AliasedRelation `{alias}`, got {:?}", input.op);
        };
        assert_eq!(got, alias);
        *input
    }

    #[test]
    fn parse_cte_single_reference_inlines_as_aliased_relation() {
        // A `FROM <cte>` reference lowers to an AliasedRelation over the CTE
        // body — NOT a TableScan named x (the CTE shadows any catalog table).
        let plan = parse("WITH x AS (SELECT id FROM t) SELECT * FROM x").expect("should parse");
        let body = expect_aliased(project_input(plan), "x");
        // The inlined body is the CTE's own Project, not a scan of `x`.
        assert!(
            matches!(body.op, CommonOp::Project { .. }),
            "expected the inlined CTE body, got {:?}",
            body.op
        );
    }

    #[test]
    fn parse_cte_explicit_columns_wraps_in_todf() {
        // `t(k, v)` — the explicit column list becomes a positional ToDf rename
        // beneath the AliasedRelation.
        let plan =
            parse("WITH t(k, v) AS (SELECT a, COUNT(*) FROM u GROUP BY a) SELECT k, v FROM t")
                .expect("should parse");
        let body = expect_aliased(project_input(plan), "t");
        match body.op {
            CommonOp::ToDf { column_names, .. } => {
                assert_eq!(column_names, vec!["k".to_owned(), "v".to_owned()]);
            }
            other => panic!("expected ToDf under the AliasedRelation, got {other:?}"),
        }
    }

    #[test]
    fn parse_derived_table_with_alias_wraps_in_aliased_relation() {
        // `(SELECT ...) AS t` — the derived-table alias is preserved as an
        // AliasedRelation over the inlined aggregate so `t.dept_id`/`t.n` bind.
        let plan = parse(
            "SELECT t.dept_id, t.n \
             FROM (SELECT dept_id, count(*) n FROM emp GROUP BY dept_id) AS t",
        )
        .expect("should parse");
        let body = expect_aliased(project_input(plan), "t");
        assert!(
            matches!(body.op, CommonOp::Aggregate { .. }),
            "expected Aggregate under the AliasedRelation, got {:?}",
            body.op
        );
    }

    #[test]
    fn parse_derived_table_explicit_columns_wraps_in_todf() {
        // `(SELECT ...) AS t(c1, c2)` — the explicit column list becomes a
        // positional ToDf rename beneath the AliasedRelation.
        let plan = parse(
            "SELECT t.c1, t.c2 \
             FROM (SELECT dept_id, count(*) FROM emp GROUP BY dept_id) AS t(c1, c2)",
        )
        .expect("should parse");
        let body = expect_aliased(project_input(plan), "t");
        match body.op {
            CommonOp::ToDf { column_names, .. } => {
                assert_eq!(column_names, vec!["c1".to_owned(), "c2".to_owned()]);
            }
            other => panic!("expected ToDf under the AliasedRelation, got {other:?}"),
        }
    }

    #[test]
    fn parse_unaliased_derived_table_inlines_bare() {
        // An unaliased derived table inlines the inner op directly — NO
        // AliasedRelation wrapper (guards the win-014/pivot inlining path).
        let plan = parse("SELECT dept_id FROM (SELECT dept_id FROM emp)").expect("should parse");
        let input = project_input(plan);
        assert!(
            !matches!(input.op, CommonOp::AliasedRelation { .. }),
            "unaliased derived table must not be wrapped in AliasedRelation, got {:?}",
            input.op
        );
        assert!(
            matches!(input.op, CommonOp::Project { .. }),
            "expected the inner Project inlined directly, got {:?}",
            input.op
        );
    }

    #[test]
    fn parse_cte_referenced_twice_yields_two_aliased_relations() {
        // A CTE referenced twice with distinct aliases inlines an independent
        // AliasedRelation clone per reference (mirrors the self-join shape).
        let plan = parse(
            "WITH e AS (SELECT id, manager_id FROM emp) \
             SELECT emp.id FROM e emp LEFT JOIN e mgr ON emp.manager_id = mgr.id",
        )
        .expect("should parse");
        let CommonOp::Join { left, right, .. } = project_input(plan).op else {
            panic!("expected Join");
        };
        expect_aliased(*left, "emp");
        expect_aliased(*right, "mgr");
    }

    #[test]
    fn parse_natural_join_lowers_to_join_with_natural_flag_no_condition_no_using() {
        let plan = parse("SELECT * FROM emp NATURAL JOIN dept").expect("should parse");
        match project_input(plan).op {
            CommonOp::Join {
                join_type,
                condition,
                using_columns,
                natural,
                ..
            } => {
                assert_eq!(join_type, JoinType::Inner);
                assert!(condition.is_none());
                assert!(using_columns.is_empty());
                assert!(natural, "NATURAL JOIN must set natural: true");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn parse_natural_left_join_lowers_to_left_join_with_natural_flag() {
        let plan = parse("SELECT * FROM emp NATURAL LEFT JOIN dept").expect("should parse");
        match project_input(plan).op {
            CommonOp::Join {
                join_type,
                condition,
                using_columns,
                natural,
                ..
            } => {
                assert_eq!(join_type, JoinType::Left);
                assert!(condition.is_none());
                assert!(using_columns.is_empty());
                assert!(natural, "NATURAL LEFT JOIN must set natural: true");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn parse_plain_on_join_lowers_with_natural_false() {
        let plan = parse("SELECT * FROM emp JOIN dept ON emp.dept_id = dept.dept_id")
            .expect("should parse");
        match project_input(plan).op {
            CommonOp::Join { natural, .. } => {
                assert!(!natural, "plain ON join must not set natural");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn parse_comma_join_lowers_with_natural_false() {
        let plan = parse("SELECT * FROM emp, dept").expect("should parse");
        match project_input(plan).op {
            CommonOp::Join {
                natural, join_type, ..
            } => {
                assert_eq!(join_type, JoinType::Cross);
                assert!(!natural, "comma-join must not set natural");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn parse_aliased_bare_table_yields_aliased_relation() {
        // INV7 (ADR-004): an aliased bare table (`emp e`) lowers to the same
        // node the DataFrame front-end produces for `df.alias("e")` —
        // `AliasedRelation { input: TableScan { alias: None }, alias: "e" }` —
        // not the old `TableScan { alias: Some("e") }`.
        let plan = parse("SELECT e.id FROM emp e").expect("should parse");
        let scan = expect_aliased(project_input(plan), "e");
        assert!(
            matches!(
                scan.op,
                CommonOp::TableScan { ref table, alias: None } if table == "emp"
            ),
            "expected TableScan {{ table: emp, alias: None }} under the \
             AliasedRelation, got {:?}",
            scan.op
        );
    }

    #[test]
    fn parse_unaliased_bare_table_stays_table_scan() {
        // Without an alias, a bare table stays a plain `TableScan` (no
        // AliasedRelation wrapping) — the normalization only triggers on alias.
        let plan = parse("SELECT * FROM emp").expect("should parse");
        let input = project_input(plan);
        assert!(
            matches!(
                input.op,
                CommonOp::TableScan { ref table, alias: None } if table == "emp"
            ),
            "expected bare TableScan, got {:?}",
            input.op
        );
    }

    #[test]
    fn parse_recursive_cte_non_union_body_rejected() {
        // A recursive CTE whose body is a plain SELECT (not UNION ALL) is
        // rejected as a boundary error — the body must be anchor UNION ALL
        // recursive_term.
        assert_eq!(
            boundary_shape("WITH RECURSIVE r(n) AS (SELECT 1) SELECT * FROM r"),
            "sql::recursive_cte::body"
        );
    }

    // ── Pass 18: WITH RECURSIVE lowering ──────────────────────────────────

    #[test]
    fn parse_recursive_cte_009_lowers_to_recursive_cte_with_self_ref_table_scan() {
        // cte-009 shape: `WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL
        // SELECT n + 1 FROM seq WHERE n < 5) SELECT * FROM seq`.
        // The self-reference (`FROM seq`) in the recursive term falls through
        // CteScope-miss into a bare `TableScan { table: "seq" }`.
        let plan = parse(
            "WITH RECURSIVE seq(n) AS (\
               SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 5\
             ) SELECT * FROM seq",
        )
        .expect("should parse recursive CTE");
        // Top level: Project { AliasedRelation { RecursiveCte { .. }, "seq" } }
        let body = expect_aliased(project_input(plan), "seq");
        match body.op {
            CommonOp::RecursiveCte {
                ref name,
                ref column_names,
                union_all,
                ref anchor,
                ref recursive_term,
            } => {
                assert_eq!(name, "seq");
                assert_eq!(column_names, &["n".to_owned()]);
                assert!(union_all);
                // Anchor is a Project over SingleRow (SELECT 1).
                assert!(
                    matches!(anchor.op, CommonOp::Project { .. }),
                    "expected anchor Project, got {:?}",
                    anchor.op
                );
                // Recursive term: Filter over Project over TableScan("seq").
                // Drill into Filter → Project → input to find the self-ref.
                fn find_table_scan(ast: &CommonAst) -> Option<&str> {
                    if let CommonOp::TableScan { table, .. } = &ast.op {
                        return Some(table);
                    }
                    for child in ast.op.children() {
                        if let Some(t) = find_table_scan(child) {
                            return Some(t);
                        }
                    }
                    None
                }
                assert_eq!(
                    find_table_scan(recursive_term),
                    Some("seq"),
                    "recursive term must contain a TableScan(seq) self-reference"
                );
            }
            other => panic!("expected RecursiveCte, got {other:?}"),
        }
    }

    #[test]
    fn parse_recursive_cte_010_lowers_with_join_self_ref() {
        // cte-010 shape: `WITH RECURSIVE chain(id, name, manager_id, lvl) AS (
        //   SELECT id, name, manager_id, 0 FROM emp WHERE manager_id IS NULL
        //   UNION ALL
        //   SELECT e.id, e.name, e.manager_id, c.lvl + 1
        //   FROM emp e JOIN chain c ON e.manager_id = c.id
        // ) SELECT * FROM chain`.
        // The self-reference is `chain c` on the right side of the JOIN.
        let plan = parse(
            "WITH RECURSIVE chain(id, name, manager_id, lvl) AS (\
               SELECT id, name, manager_id, 0 FROM emp WHERE manager_id IS NULL \
               UNION ALL \
               SELECT e.id, e.name, e.manager_id, c.lvl + 1 \
               FROM emp e JOIN chain c ON e.manager_id = c.id\
             ) SELECT * FROM chain",
        )
        .expect("should parse recursive CTE with join");
        let body = expect_aliased(project_input(plan), "chain");
        match body.op {
            CommonOp::RecursiveCte {
                ref name,
                ref column_names,
                union_all,
                ref recursive_term,
                ..
            } => {
                assert_eq!(name, "chain");
                assert_eq!(
                    column_names,
                    &[
                        "id".to_owned(),
                        "name".to_owned(),
                        "manager_id".to_owned(),
                        "lvl".to_owned()
                    ]
                );
                assert!(union_all);
                // The recursive term's input (under the Project) is a Join.
                // The right side of the Join should be AliasedRelation("c")
                // wrapping TableScan("chain").
                let inner = match &recursive_term.op {
                    CommonOp::Project { input, .. } => input,
                    other => panic!("expected Project in recursive term, got {other:?}"),
                };
                let (right, _) = match &inner.op {
                    CommonOp::Join { right, left, .. } => (right, left),
                    other => panic!("expected Join in recursive term, got {other:?}"),
                };
                // Right side: AliasedRelation { input: TableScan("chain"), alias: "c" }
                match &right.op {
                    CommonOp::AliasedRelation { input, alias } => {
                        assert_eq!(alias, "c");
                        assert!(
                            matches!(&input.op, CommonOp::TableScan { table, .. } if table == "chain"),
                            "expected TableScan(chain) under AliasedRelation, got {:?}",
                            input.op
                        );
                    }
                    other => panic!("expected AliasedRelation(c) on join right, got {other:?}"),
                }
            }
            other => panic!("expected RecursiveCte, got {other:?}"),
        }
    }

    #[test]
    fn parse_recursive_cte_multiple_ctes_rejected() {
        // More than 1 CTE under a single WITH RECURSIVE is rejected.
        assert_eq!(
            boundary_shape(
                "WITH RECURSIVE a(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM a WHERE n<5), \
                 b(m) AS (SELECT 1 UNION ALL SELECT m+1 FROM b WHERE m<3) \
                 SELECT * FROM a"
            ),
            "sql::recursive_cte::multiple"
        );
    }

    #[test]
    fn parse_recursive_cte_intersect_body_rejected() {
        // A body using INTERSECT instead of UNION ALL is rejected.
        assert_eq!(
            boundary_shape(
                "WITH RECURSIVE r(n) AS (\
                   SELECT 1 INTERSECT SELECT n+1 FROM r WHERE n<5\
                 ) SELECT * FROM r"
            ),
            "sql::recursive_cte::body"
        );
    }

    #[test]
    fn parse_recursive_cte_order_by_on_body_rejected() {
        // ORDER BY on the CTE body's own query wrapper is rejected.
        assert_eq!(
            boundary_shape(
                "WITH RECURSIVE r(n) AS (\
                   SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<5 ORDER BY n\
                 ) SELECT * FROM r"
            ),
            "sql::recursive_cte::modifier"
        );
    }

    #[test]
    fn parse_recursive_cte_plain_union_carries_union_all_false() {
        // Bare UNION (without ALL) is NOT parser-rejected — it is carried as
        // union_all: false and the analyzer rejects it.
        let plan = parse(
            "WITH RECURSIVE seq(n) AS (\
               SELECT 1 UNION SELECT n + 1 FROM seq WHERE n < 5\
             ) SELECT * FROM seq",
        )
        .expect("should parse (UNION without ALL)");
        let body = expect_aliased(project_input(plan), "seq");
        match body.op {
            CommonOp::RecursiveCte { union_all, .. } => {
                assert!(!union_all, "bare UNION must carry union_all=false");
            }
            other => panic!("expected RecursiveCte, got {other:?}"),
        }
    }

    #[test]
    fn parse_pivot_lowers_to_common_op_pivot() {
        // Pass 107: `SELECT * FROM t PIVOT (...)` now lowers to a
        // `CommonOp::Pivot` with implicit (schema-derived) grouping.
        let plan =
            parse("SELECT * FROM t PIVOT (SUM(x) FOR y IN (1, 2))").expect("PIVOT should lower");
        match pivot_node(plan) {
            CommonOp::Pivot { grouping, .. } => {
                assert_eq!(grouping, PivotGrouping::Implicit);
            }
            other => panic!("expected Pivot, got {other:?}"),
        }
    }

    #[test]
    fn parse_grouping_sets_single_set_lowers_to_grouping_sets_kind() {
        let plan = parse("SELECT dept, COUNT(*) FROM t GROUP BY GROUPING SETS ((dept))")
            .expect("GROUPING SETS should parse");
        match plan.op {
            CommonOp::Aggregate {
                grouping,
                grouping_kind,
                grouping_sets,
                ..
            } => {
                assert_eq!(grouping_kind, GroupingKind::GroupingSets);
                assert_eq!(grouping.len(), 1);
                assert_eq!(grouping_sets, vec![vec![0usize]]);
            }
            other => panic!("expected Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn parse_union_all_lowers_to_setop_union_all() {
        let plan = parse("SELECT id FROM t UNION ALL SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp {
                kind,
                all,
                by_name,
                allow_missing_columns,
                children,
            } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(all);
                assert!(!by_name);
                assert!(!allow_missing_columns);
                assert_eq!(children.len(), 2);
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_union_bare_is_distinct() {
        let plan = parse("SELECT id FROM t UNION SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, all, .. } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(!all, "bare UNION is Spark-default DISTINCT");
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_intersect_lowers_to_setop_intersect() {
        let plan = parse("SELECT id FROM t INTERSECT SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, all, .. } => {
                assert_eq!(kind, SetOpKind::Intersect);
                assert!(!all);
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_except_lowers_to_setop_except() {
        let plan = parse("SELECT id FROM t EXCEPT SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, .. } => assert_eq!(kind, SetOpKind::Except),
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_minus_folds_to_setop_except() {
        let plan = parse("SELECT id FROM t MINUS SELECT id FROM u").expect("should parse");
        match plan.op {
            CommonOp::SetOp { kind, .. } => assert_eq!(kind, SetOpKind::Except),
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_three_way_union_all_nests_setops() {
        let plan = parse("SELECT id FROM t UNION ALL SELECT id FROM u UNION ALL SELECT id FROM v")
            .expect("should parse");
        match plan.op {
            CommonOp::SetOp {
                kind,
                all,
                children,
                ..
            } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(all);
                assert_eq!(children.len(), 2);
                // sqlparser left-nests: children[0] is itself a SetOp.
                assert!(
                    matches!(children[0].op, CommonOp::SetOp { .. }),
                    "3-way UNION ALL should nest a SetOp as the left child"
                );
            }
            _ => panic!("expected SetOp as top-level"),
        }
    }

    #[test]
    fn parse_setop_with_order_by_wraps_in_sort() {
        let plan =
            parse("SELECT id FROM t UNION SELECT id FROM u ORDER BY id").expect("should parse");
        match plan.op {
            CommonOp::Sort { input, .. } => {
                assert!(
                    matches!(input.op, CommonOp::SetOp { .. }),
                    "ORDER BY over a set op wraps the SetOp in a Sort"
                );
            }
            _ => panic!("expected Sort wrapping a SetOp"),
        }
    }

    #[test]
    fn parse_union_by_name_is_rejected_not_silently_positional() {
        // `UNION BY NAME` parses in SparkDialect but has no positional
        // lowering — must be a Thunderduck-boundary error, not a silent
        // by-position union (ADR-022; loud-fail).
        assert_eq!(
            boundary_shape("SELECT a, b FROM t UNION BY NAME SELECT b, a FROM u"),
            "sql::set_operation::by_name"
        );
    }

    #[test]
    fn parse_div_keyword_lowers_to_integer_divide_cast() {
        // Pass 73: SparkSQL's `a DIV b` — the SparkDialect's `parse_infix`
        // registers DIV as an integer-division operator; v2_lowering
        // wraps the resulting binary in a `CAST(... AS BIGINT)`.
        let plan = parse("SELECT a div 2 FROM t").expect("should parse");
        match plan.op {
            CommonOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 1);
                assert!(
                    matches!(&projections[0], Expression::Cast(c)
                        if matches!(&*c.expr, Expression::Binary(b) if b.op == BinaryOp::Div)
                    ),
                    "expected Cast(Binary(Div)) for `a DIV 2`, got {:?}",
                    projections[0]
                );
            }
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn parse_extract_year_lowers_to_year_function() {
        // Pass 73: `EXTRACT(YEAR FROM col)` lowers to a FunctionCall to
        // `year(col)` (INTEGER return-type, matching Spark).
        let plan = parse("SELECT EXTRACT(YEAR FROM d) FROM t").expect("should parse");
        match plan.op {
            CommonOp::Project { projections, .. } => match &projections[0] {
                Expression::FunctionCall(fc) => {
                    assert_eq!(fc.name.to_lowercase(), "year");
                    assert_eq!(fc.args.len(), 1);
                }
                other => panic!("expected FunctionCall, got {other:?}"),
            },
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn single_arg_lambda_lowers_to_lambda_expression() {
        // Pass 84: `x -> upper(x)` inside `transform(tags, ...)` must lower to
        // `Expression::Lambda { params: ["x"], body: FunctionCall(upper) }`.
        // Pass 86 L1 witness: the identifier `x` inside the body must be
        // rewritten to `LambdaVariable("x")` — not left as `UnresolvedColumn`.
        let plan = parse("SELECT transform(tags, x -> upper(x)) FROM emp").expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        let Expression::FunctionCall(fc) = &projections[0] else {
            panic!("expected FunctionCall, got {:?}", projections[0]);
        };
        assert_eq!(fc.name.to_lowercase(), "transform");
        assert_eq!(fc.args.len(), 2);
        let Expression::Lambda(lambda) = &fc.args[1] else {
            panic!("expected Lambda as second arg, got {:?}", fc.args[1]);
        };
        assert_eq!(lambda.params, vec!["x".to_owned()]);
        let Expression::FunctionCall(body_fc) = &*lambda.body else {
            panic!("expected FunctionCall body, got {:?}", lambda.body);
        };
        assert_eq!(body_fc.name.to_lowercase(), "upper");
        assert_eq!(body_fc.args.len(), 1);
        match &body_fc.args[0] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "x"),
            other => panic!("expected LambdaVariable(x), got {other:?}"),
        }
    }

    #[test]
    fn multi_arg_lambda_lowers_to_lambda_expression() {
        // Pass 84: `(acc, x) -> concat(acc, x)` inside `reduce(...)` must lower
        // to `Expression::Lambda { params: ["acc", "x"], body: FunctionCall }`.
        // Pass 86 L1 witness: both `acc` and `x` inside the body must be
        // rewritten to `LambdaVariable` — not left as `UnresolvedColumn`.
        let plan = parse("SELECT reduce(tags, '', (acc, x) -> concat(acc, x)) FROM emp")
            .expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        let Expression::FunctionCall(fc) = &projections[0] else {
            panic!("expected FunctionCall, got {:?}", projections[0]);
        };
        assert_eq!(fc.name.to_lowercase(), "reduce");
        assert_eq!(fc.args.len(), 3);
        let Expression::Lambda(lambda) = &fc.args[2] else {
            panic!("expected Lambda as third arg, got {:?}", fc.args[2]);
        };
        assert_eq!(lambda.params, vec!["acc".to_owned(), "x".to_owned()]);
        let Expression::FunctionCall(body_fc) = &*lambda.body else {
            panic!("expected FunctionCall body, got {:?}", lambda.body);
        };
        assert_eq!(body_fc.name.to_lowercase(), "concat");
        assert_eq!(body_fc.args.len(), 2);
        match &body_fc.args[0] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "acc"),
            other => panic!("expected LambdaVariable(acc), got {other:?}"),
        }
        match &body_fc.args[1] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "x"),
            other => panic!("expected LambdaVariable(x), got {other:?}"),
        }
    }

    #[test]
    fn nested_lambda_shadowing_preserved() {
        // Pass 86 L2: nested-lambda shadowing witness. In
        // `transform(arr1, x -> transform(arr2, y -> concat(x, y)))`, the
        // inner-lambda body references BOTH the outer's `x` and the inner's
        // `y`. After lowering, both must be rewritten to `LambdaVariable`:
        // outer's `x` reaches through the inner-Lambda arm because
        // `remaining = params \ inner.params = ["x"] \ ["y"] = ["x"]` (the
        // outer param survives the shadow-filter). Inner's `y` is rewritten
        // by the inner-lambda pass itself.
        let plan = parse("SELECT transform(arr1, x -> transform(arr2, y -> concat(x, y))) FROM t")
            .expect("should parse");
        let CommonOp::Project { projections, .. } = plan.op else {
            panic!("expected Project");
        };
        // Outer FunctionCall("transform", [_, outer_lambda]).
        let Expression::FunctionCall(outer_fc) = &projections[0] else {
            panic!("expected outer FunctionCall, got {:?}", projections[0]);
        };
        assert_eq!(outer_fc.name.to_lowercase(), "transform");
        assert_eq!(outer_fc.args.len(), 2);
        let Expression::Lambda(outer_lambda) = &outer_fc.args[1] else {
            panic!("expected outer Lambda, got {:?}", outer_fc.args[1]);
        };
        assert_eq!(outer_lambda.params, vec!["x".to_owned()]);
        // Outer body is `transform(arr2, y -> concat(x, y))`.
        let Expression::FunctionCall(inner_transform) = &*outer_lambda.body else {
            panic!(
                "expected inner transform FunctionCall, got {:?}",
                outer_lambda.body
            );
        };
        assert_eq!(inner_transform.name.to_lowercase(), "transform");
        assert_eq!(inner_transform.args.len(), 2);
        let Expression::Lambda(inner_lambda) = &inner_transform.args[1] else {
            panic!("expected inner Lambda, got {:?}", inner_transform.args[1]);
        };
        assert_eq!(inner_lambda.params, vec!["y".to_owned()]);
        // Inner body is `concat(x, y)` — both must be LambdaVariable.
        let Expression::FunctionCall(concat_fc) = &*inner_lambda.body else {
            panic!(
                "expected concat FunctionCall in inner body, got {:?}",
                inner_lambda.body
            );
        };
        assert_eq!(concat_fc.name.to_lowercase(), "concat");
        assert_eq!(concat_fc.args.len(), 2);
        match &concat_fc.args[0] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "x"),
            other => panic!("expected outer LambdaVariable(x), got {other:?}"),
        }
        match &concat_fc.args[1] {
            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "y"),
            other => panic!("expected inner LambdaVariable(y), got {other:?}"),
        }
    }

    #[test]
    fn parse_syntax_error_returns_unsupported_proto_shape() {
        // Review M2: sqlparser errors are boundary failures (input never
        // reached CommonAst) → surface as `UnsupportedProtoShape`, not
        // `UnsupportedOp`. Exercised through the public entry point so the
        // top-level mapping (parser_v2::SparkSqlParserV2::parse) is anchored.
        use crate::parser_v2::SparkSqlParserV2;
        let result = SparkSqlParserV2::parse("SELCT bad");
        match result {
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: shape,
                ..
            }) => {
                assert_eq!(shape, "sql::parse_error");
            }
            other => panic!("expected UnsupportedProtoShape sql::parse_error, got {other:?}"),
        }
    }

    /// Return the first projection expression of a `Project` plan, unwrapping a
    /// top-level `Alias` if present (owned counterpart of
    /// [`Expression::unaliased`]).
    fn first_projection(plan: CommonAst) -> Expression {
        let CommonOp::Project {
            mut projections, ..
        } = plan.op
        else {
            panic!("expected Project as top-level");
        };
        assert!(!projections.is_empty());
        projections.remove(0).unaliased().clone()
    }

    #[test]
    fn window_partition_order_no_frame() {
        let plan = parse("SELECT rank() OVER (PARTITION BY dept ORDER BY sal) FROM t")
            .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                assert_eq!(w.partition_by.len(), 1);
                assert_eq!(w.order_by.len(), 1);
                assert!(w.frame.is_none(), "no frame clause → frame None");
            }
            other => panic!("expected Window, got {other:?}"),
        }
    }

    #[test]
    fn window_rows_unbounded_preceding_to_current_row() {
        let plan = parse(
            "SELECT sum(x) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) \
             FROM t",
        )
        .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                let frame = w.frame.expect("frame present");
                assert_eq!(frame.unit, FrameUnit::Rows);
                assert!(matches!(frame.lower, FrameBoundary::UnboundedPreceding));
                assert!(matches!(frame.upper, FrameBoundary::CurrentRow));
            }
            other => panic!("expected Window, got {other:?}"),
        }
    }

    #[test]
    fn window_rows_between_one_preceding_and_one_following() {
        let plan = parse(
            "SELECT avg(x) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM t",
        )
        .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                let frame = w.frame.expect("frame present");
                assert_eq!(frame.unit, FrameUnit::Rows);
                match frame.lower {
                    FrameBoundary::Preceding(e) => {
                        assert!(matches!(*e, Expression::Literal(_)));
                    }
                    other => panic!("expected Preceding(1), got {other:?}"),
                }
                match frame.upper {
                    FrameBoundary::Following(e) => {
                        assert!(matches!(*e, Expression::Literal(_)));
                    }
                    other => panic!("expected Following(1), got {other:?}"),
                }
            }
            other => panic!("expected Window, got {other:?}"),
        }
    }

    #[test]
    fn window_named_window_is_inlined() {
        let plan =
            parse("SELECT rank() OVER w FROM t WINDOW w AS (PARTITION BY dept ORDER BY sal)")
                .expect("should parse");
        match first_projection(plan) {
            Expression::Window(w) => {
                assert_eq!(w.partition_by.len(), 1, "named window PARTITION BY inlined");
                assert_eq!(w.order_by.len(), 1, "named window ORDER BY inlined");
            }
            other => panic!("expected inlined Window, got {other:?}"),
        }
    }

    #[test]
    fn window_groups_frame_is_rejected() {
        assert_eq!(
            boundary_shape(
                "SELECT sum(x) OVER (ORDER BY id GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) \
                 FROM t"
            ),
            "sql::window_frame::groups"
        );
    }

    #[test]
    fn unknown_named_window_is_rejected() {
        assert_eq!(
            boundary_shape("SELECT rank() OVER w FROM t WINDOW v AS (ORDER BY id)"),
            "sql::named_window::unknown"
        );
    }

    // ── resolve_named_windows_in_expr descends into composite shapes
    // (finding 2: a named-window ref nested in CASE/fn-args used to hit a
    // spurious "not defined in WINDOW clause" error even though it was) ────

    #[test]
    fn named_window_ref_inside_case_branch_resolves() {
        let plan = parse(
            "SELECT CASE WHEN l_quantity > 0 THEN sum(l_extendedprice) OVER w ELSE 0 END \
             FROM lineitem WINDOW w AS (PARTITION BY l_returnflag)",
        )
        .expect("named window inside CASE must resolve");
        match first_projection(plan) {
            Expression::CaseWhen(_) => {}
            other => panic!("expected CaseWhen, got {other:?}"),
        }
    }

    #[test]
    fn named_window_ref_inside_function_args_resolves() {
        let plan = parse(
            "SELECT abs(sum(l_quantity) OVER w) FROM lineitem WINDOW w AS (ORDER BY l_orderkey)",
        )
        .expect("named window inside fn args must resolve");
        match first_projection(plan) {
            Expression::FunctionCall(fc) => {
                assert!(fc.name.eq_ignore_ascii_case("abs"));
                assert_eq!(fc.args.len(), 1);
                assert!(
                    matches!(&fc.args[0], Expression::Window(_)),
                    "abs's argument must be the inlined Window, got {:?}",
                    fc.args[0]
                );
            }
            other => panic!("expected FunctionCall(abs), got {other:?}"),
        }
    }

    #[test]
    fn named_window_ref_inside_between_resolves() {
        let plan = parse("SELECT sum(x) OVER w BETWEEN 0 AND 100 FROM t WINDOW w AS (ORDER BY id)")
            .expect("named window inside BETWEEN must resolve");
        match first_projection(plan) {
            Expression::Between(b) => {
                assert!(matches!(*b.expr, Expression::Window(_)));
            }
            other => panic!("expected Between, got {other:?}"),
        }
    }

    #[test]
    fn undefined_named_window_nested_in_case_is_rejected() {
        // The window ref is nested in a CASE branch and genuinely undefined —
        // must still surface the boundary error, not silently pass through.
        assert_eq!(
            boundary_shape(
                "SELECT CASE WHEN x > 0 THEN sum(y) OVER w ELSE 0 END \
                 FROM t WINDOW v AS (ORDER BY y)"
            ),
            "sql::named_window::unknown"
        );
    }

    #[test]
    fn named_window_scope_does_not_cross_into_subquery() {
        // A `WINDOW` clause is scoped to its containing SELECT; an outer-only
        // definition must NOT resolve a named-window ref inside a nested
        // subquery's own SELECT.
        assert_eq!(
            boundary_shape("SELECT (SELECT sum(a) OVER w FROM u) FROM t WINDOW w AS (ORDER BY b)"),
            "sql::named_window::unresolved"
        );
    }

    #[test]
    fn interval_literal_day_lowers_to_interval_expression() {
        let plan = parse("SELECT INTERVAL '90' DAY FROM t").expect("should parse");
        match first_projection(plan) {
            Expression::Interval(ie) => {
                assert_eq!(ie.days, 90);
                assert_eq!(ie.months, 0);
                assert_eq!(ie.microseconds, 0);
                // Scope guard: single-field literals stay generic Calendar
                // (retyping would regress the green date-arithmetic cases).
                assert_eq!(ie.kind, IntervalKind::Calendar);
            }
            other => panic!("expected Interval, got {other:?}"),
        }
    }

    /// Extract the top-level projection as an `IntervalExpression`, panicking
    /// otherwise.
    fn first_interval(sql: &str) -> IntervalExpression {
        let plan = parse(sql).expect("should parse");
        match first_projection(plan) {
            Expression::Interval(ie) => ie,
            other => panic!("expected Interval, got {other:?}"),
        }
    }

    #[test]
    fn interval_year_to_month_lowers_to_year_month_interval() {
        let ie = first_interval("SELECT INTERVAL '1-2' YEAR TO MONTH AS ym");
        assert_eq!(ie.months, 14);
        assert_eq!(ie.days, 0);
        assert_eq!(ie.microseconds, 0);
        assert_eq!(ie.kind, IntervalKind::YearMonth);
    }

    #[test]
    fn interval_day_to_second_lowers_to_day_time_interval() {
        let ie = first_interval("SELECT INTERVAL '1 02:30:00' DAY TO SECOND AS dts");
        assert_eq!(ie.days, 1);
        assert_eq!(ie.months, 0);
        assert_eq!(ie.microseconds, 9_000_000_000);
        assert_eq!(ie.kind, IntervalKind::DayTime);
    }

    #[test]
    fn interval_day_to_second_parses_fractional_seconds() {
        let ie = first_interval("SELECT INTERVAL '1 02:30:00.123456' DAY TO SECOND AS dts");
        assert_eq!(ie.days, 1);
        assert_eq!(ie.microseconds, 9_000_123_456);
        assert_eq!(ie.kind, IntervalKind::DayTime);
    }

    #[test]
    fn interval_day_to_second_truncates_fraction_beyond_micros() {
        // 7-digit fraction: digits 7-9 are truncated toward zero to microseconds.
        let ie = first_interval("SELECT INTERVAL '1 02:30:00.1234567' DAY TO SECOND AS dts");
        assert_eq!(ie.microseconds, 9_000_123_456);
    }

    #[test]
    fn interval_year_to_month_negative_sign() {
        let ie = first_interval("SELECT INTERVAL '-1-2' YEAR TO MONTH AS ym");
        assert_eq!(ie.months, -14);
        assert_eq!(ie.kind, IntervalKind::YearMonth);
    }

    #[test]
    fn interval_day_to_second_negative_sign() {
        let ie = first_interval("SELECT INTERVAL '-1 02:30:00' DAY TO SECOND AS dts");
        assert_eq!(ie.days, -1);
        assert_eq!(ie.microseconds, -9_000_000_000);
        assert_eq!(ie.kind, IntervalKind::DayTime);
    }

    #[test]
    fn interval_year_to_month_malformed_months_is_boundary() {
        // Month component out of `0..=11` → year_month_format boundary.
        assert_eq!(
            boundary_shape("SELECT INTERVAL '1-13' YEAR TO MONTH AS ym"),
            "sql::expr::interval::year_month_format"
        );
    }

    #[test]
    fn interval_day_to_second_malformed_hours_is_boundary() {
        // Hour component > 23 → day_time_format boundary.
        assert_eq!(
            boundary_shape("SELECT INTERVAL '1 25:00:00' DAY TO SECOND AS dts"),
            "sql::expr::interval::day_time_format"
        );
    }

    #[test]
    fn interval_out_of_scope_pair_is_compound_boundary() {
        // Only YEAR TO MONTH and DAY TO SECOND are supported; every other pair
        // keeps the existing compound Thunderduck boundary.
        assert_eq!(
            boundary_shape("SELECT INTERVAL '1 02' DAY TO HOUR AS dh"),
            "sql::expr::interval::compound"
        );
    }

    #[test]
    fn interval_year_to_month_bare_projection_analyzes_and_emits() {
        use crate::transpiler_v2::analyzer::analyze;
        use crate::transpiler_v2::base_types::BaseTypes;
        use crate::transpiler_v2::emission::dispatch_op;

        let plan = parse("SELECT INTERVAL '1-2' YEAR TO MONTH AS ym").expect("parse");
        // No FROM clause → SingleRow input, no TableScan → empty overlay.
        let bt = BaseTypes::empty();
        let typed = analyze(plan, &bt).expect("analyze");
        assert_eq!(typed.resolved_schema.fields.len(), 1);
        let field = &typed.resolved_schema.fields[0];
        assert_eq!(field.name, "ym");
        assert_eq!(field.data_type, DataType::YearMonthInterval);
        assert!(!field.nullable);

        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("emit");
        assert!(
            sql.contains("INTERVAL '14 months 0 days 0 microseconds'"),
            "got: {sql}"
        );
    }

    #[test]
    fn natural_join_emits_identical_sql_to_explicit_using() {
        // End-to-end parse → analyze → emit: NATURAL JOIN's analyzer desugar
        // must land on the exact same SQL as the equivalent explicit
        // `USING (dept_id)` join — proving the two are indistinguishable
        // past the analyzer (jn-008).
        use crate::transpiler_v2::analyzer::analyze;
        use crate::transpiler_v2::base_types::BaseTypes;
        use crate::transpiler_v2::emission::dispatch_op;
        use crate::types::{StructField, StructType};

        fn emp() -> StructType {
            StructType::new(vec![
                StructField::not_null("id", DataType::Long),
                StructField::nullable("name", DataType::String),
                StructField::nullable("dept_id", DataType::Integer),
                StructField::nullable("salary", DataType::Double),
            ])
        }

        fn dept() -> StructType {
            StructType::new(vec![
                StructField::not_null("dept_id", DataType::Integer),
                StructField::nullable("dept_name", DataType::String),
            ])
        }

        let bt = BaseTypes::from_entries(
            [("emp".to_owned(), emp()), ("dept".to_owned(), dept())]
                .into_iter()
                .collect(),
        );

        let natural_plan = parse("SELECT * FROM emp NATURAL JOIN dept").expect("parse natural");
        let using_plan = parse("SELECT * FROM emp JOIN dept USING (dept_id)").expect("parse using");

        let natural_typed = analyze(natural_plan, &bt).expect("analyze natural");
        let using_typed = analyze(using_plan, &bt).expect("analyze using");

        let natural_sql =
            dispatch_op(&natural_typed.op, &natural_typed.resolved_schema).expect("emit natural");
        let using_sql =
            dispatch_op(&using_typed.op, &using_typed.resolved_schema).expect("emit using");
        assert_eq!(natural_sql, using_sql);
    }

    /// Extract the top-level projection as a `FunctionCall`, panicking otherwise.
    fn first_function_call(sql: &str) -> FunctionCall {
        let plan = parse(sql).expect("should parse");
        match first_projection(plan) {
            Expression::FunctionCall(fc) => fc,
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn substring_from_for_lowers_to_substring() {
        let fc = first_function_call("SELECT substring(name FROM 1 FOR 2) FROM t");
        assert_eq!(fc.name, "substring");
        assert_eq!(fc.args.len(), 3);
        assert!(!fc.distinct);
    }

    #[test]
    fn substr_shorthand_lowers_to_substring() {
        let fc = first_function_call("SELECT substr(name, 2, 3) FROM t");
        assert_eq!(fc.name, "substring");
        assert_eq!(fc.args.len(), 3);
    }

    #[test]
    fn trim_both_lowers_to_trim_with_expr_first() {
        let fc = first_function_call("SELECT trim(BOTH 'A' FROM name) FROM t");
        assert_eq!(fc.name, "trim");
        assert_eq!(fc.args.len(), 2);
        // DuckDB `trim(string, characters)`: the trimmed value comes first,
        // the trim characters second.
        assert!(matches!(
            fc.args[0],
            Expression::UnresolvedColumn(ref c) if c.name == "name"
        ));
    }

    #[test]
    fn trim_leading_lowers_to_ltrim() {
        let fc = first_function_call("SELECT trim(LEADING 'A' FROM name) FROM t");
        assert_eq!(fc.name, "ltrim");
        assert_eq!(fc.args.len(), 2);
    }

    #[test]
    fn trim_trailing_lowers_to_rtrim() {
        let fc = first_function_call("SELECT trim(TRAILING 'A' FROM name) FROM t");
        assert_eq!(fc.name, "rtrim");
        assert_eq!(fc.args.len(), 2);
    }

    #[test]
    fn bare_trim_lowers_to_single_arg_trim() {
        let fc = first_function_call("SELECT trim(name) FROM t");
        assert_eq!(fc.name, "trim");
        assert_eq!(fc.args.len(), 1);
    }

    #[test]
    fn position_in_lowers_to_locate() {
        let fc = first_function_call("SELECT position('a' IN name) FROM t");
        assert_eq!(fc.name, "locate");
        assert_eq!(fc.args.len(), 2);
        // locate(substr, str): needle first, haystack second.
        assert!(matches!(
            fc.args[0],
            Expression::Literal(Literal {
                value: LiteralValue::String(ref s),
                ..
            }) if s == "a"
        ));
    }

    #[test]
    fn overlay_placing_lowers_to_overlay() {
        let fc = first_function_call("SELECT overlay(name PLACING 'XX' FROM 1 FOR 2) FROM t");
        assert_eq!(fc.name, "overlay");
        assert_eq!(fc.args.len(), 4);
    }

    // ── Pass 106 — uncorrelated subquery lowering ────────────────────────

    #[test]
    fn scalar_subquery_lowers_to_unanalyzed_scalar_subquery() {
        let plan = parse("SELECT (SELECT max(sal) FROM emp) AS gmax FROM emp").expect("parse");
        match first_projection(plan) {
            Expression::ScalarSubquery(s) => {
                assert!(
                    matches!(s.subquery, SubqueryPlan::Unanalyzed(_)),
                    "front-end must emit an Unanalyzed inner plan"
                );
            }
            other => panic!("expected ScalarSubquery, got {other:?}"),
        }
    }

    /// Extract the WHERE condition of a `SELECT * FROM t WHERE …` plan, which
    /// lowers to `Project(Star) → Filter → TableScan`.
    fn filter_condition(plan: CommonAst) -> Expression {
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project as top-level");
        };
        let CommonOp::Filter { condition, .. } = input.op else {
            panic!("expected Filter under Project");
        };
        condition
    }

    #[test]
    fn in_subquery_lowers_and_preserves_negated() {
        let plan = parse("SELECT * FROM emp WHERE dept_id NOT IN (SELECT dept_id FROM dept)")
            .expect("parse");
        match filter_condition(plan) {
            Expression::InSubquery(i) => {
                assert!(i.negated, "NOT IN → negated");
                assert!(matches!(i.subquery, SubqueryPlan::Unanalyzed(_)));
            }
            other => panic!("expected InSubquery, got {other:?}"),
        }
    }

    #[test]
    fn exists_subquery_lowers_to_unanalyzed_exists() {
        let plan = parse("SELECT * FROM emp WHERE EXISTS (SELECT 1 FROM dept)").expect("parse");
        match filter_condition(plan) {
            Expression::ExistsSubquery(e) => {
                assert!(!e.negated);
                assert!(matches!(e.subquery, SubqueryPlan::Unanalyzed(_)));
            }
            other => panic!("expected ExistsSubquery, got {other:?}"),
        }
    }

    #[test]
    fn subquery_sees_outer_cte_scope() {
        // Review M1: a subquery's `FROM <cte>` must inline the outer CTE body
        // (an AliasedRelation over the CTE's own plan), NOT a TableScan named
        // `c`. If a real table `c` existed, a TableScan would silently read it
        // instead of the CTE — Spark shadows the table with the CTE (cte-006).
        let plan = parse(
            "WITH c AS (SELECT dept_id FROM dept) \
             SELECT * FROM emp WHERE dept_id IN (SELECT dept_id FROM c)",
        )
        .expect("parse");
        let inner = match filter_condition(plan) {
            Expression::InSubquery(i) => match i.subquery {
                SubqueryPlan::Unanalyzed(inner) => *inner,
                other => panic!("expected Unanalyzed inner plan, got {other:?}"),
            },
            other => panic!("expected InSubquery, got {other:?}"),
        };
        // Inner plan: Project(dept_id) → AliasedRelation("c", <CTE body>).
        let CommonOp::Project { input, .. } = inner.op else {
            panic!(
                "expected Project as the subquery's top node, got {:?}",
                inner.op
            );
        };
        match input.op {
            CommonOp::AliasedRelation { alias, input } => {
                assert_eq!(alias, "c", "the CTE name is the AliasedRelation alias");
                assert!(
                    matches!(input.op, CommonOp::Project { .. }),
                    "expected the inlined CTE body (a Project), got {:?}",
                    input.op
                );
            }
            other => panic!(
                "expected AliasedRelation over the CTE body — a bare TableScan \
                 would mean the CTE was invisible inside the subquery, got {other:?}"
            ),
        }
    }

    // ── SQL PIVOT / UNPIVOT lowering (pass 107) ──────────────────────────

    /// Find the `CommonOp::Pivot` node under the outer `SELECT * FROM (…) PIVOT`.
    fn pivot_node(plan: CommonAst) -> CommonOp {
        match plan.op {
            CommonOp::Project { input, .. } => input.op,
            other => panic!("expected Project over Pivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_sql_pivot_marks_grouping_implicit_and_wraps_aliased_values() {
        // pv-001 shape: aliased FOR values must round-trip as `Alias` exprs so
        // the analyzer can name the output columns after the aliases.
        let plan = parse(
            "SELECT * FROM (SELECT dept_id, active, salary FROM emp) \
             PIVOT (avg(salary) FOR active IN (true AS act, false AS inact))",
        )
        .expect("should parse+lower");
        match pivot_node(plan) {
            CommonOp::Pivot {
                grouping,
                pivot_column,
                pivot_values,
                aggregates,
                ..
            } => {
                assert_eq!(grouping, PivotGrouping::Implicit);
                // Pivot column is the FOR column.
                assert!(
                    matches!(pivot_column, Expression::UnresolvedColumn(ref u) if u.name == "active")
                );
                // Both values are Alias-wrapped (true AS act / false AS inact).
                assert_eq!(pivot_values.len(), 2);
                match &pivot_values[0] {
                    Expression::Alias(a) => assert_eq!(a.alias, "act"),
                    other => panic!("expected Alias value, got {other:?}"),
                }
                match &pivot_values[1] {
                    Expression::Alias(a) => assert_eq!(a.alias, "inact"),
                    other => panic!("expected Alias value, got {other:?}"),
                }
                assert_eq!(aggregates.len(), 1);
            }
            other => panic!("expected Pivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_sql_pivot_bare_numeric_values_stay_bare() {
        // pv-005 shape: no aliases ⇒ values must NOT be wrapped in Alias.
        let plan = parse(
            "SELECT * FROM (SELECT dept_id, salary FROM emp) \
             PIVOT (avg(salary) FOR dept_id IN (10, 20, 30))",
        )
        .expect("should parse+lower");
        match pivot_node(plan) {
            CommonOp::Pivot {
                grouping,
                pivot_values,
                ..
            } => {
                assert_eq!(grouping, PivotGrouping::Implicit);
                assert_eq!(pivot_values.len(), 3);
                for v in &pivot_values {
                    assert!(
                        matches!(v, Expression::Literal(_)),
                        "bare pivot value must stay a Literal, got {v:?}"
                    );
                }
            }
            other => panic!("expected Pivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_sql_pivot_dynamic_values_rejected() {
        let err = parse(
            "SELECT * FROM (SELECT dept_id, active, salary FROM emp) \
             PIVOT (avg(salary) FOR active IN (ANY))",
        );
        // ANY / dynamic values are a Thunderduck-boundary reject.
        assert!(err.is_err(), "dynamic PIVOT values must be rejected");
    }

    #[test]
    fn lower_sql_unpivot_marks_ids_implicit_and_maps_names() {
        // pv-004 shape: value/name/columns map through; ids are Implicit.
        let plan = parse(
            "SELECT id, metric, val FROM (SELECT id, age, salary FROM emp) \
             UNPIVOT (val FOR metric IN (age, salary))",
        )
        .expect("should parse+lower");
        match pivot_node(plan) {
            CommonOp::Unpivot {
                ids,
                values,
                variable_column_name,
                value_column_name,
                ..
            } => {
                assert_eq!(ids, UnpivotIds::Implicit);
                assert_eq!(values, vec!["age".to_owned(), "salary".to_owned()]);
                assert_eq!(variable_column_name, "metric");
                assert_eq!(value_column_name, "val");
            }
            other => panic!("expected Unpivot, got {other:?}"),
        }
    }

    #[test]
    fn lower_ilike_sets_case_insensitive() {
        // whr-012 shape: `name ILIKE 'a%'` → case-insensitive LIKE.
        let plan = parse("SELECT id FROM t WHERE a ILIKE 'x%'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Like(l) => {
                assert!(l.case_insensitive, "ILIKE must flag case_insensitive");
                assert!(!l.negated);
            }
            other => panic!("expected Like, got {other:?}"),
        }
    }

    #[test]
    fn lower_not_ilike_sets_negated() {
        let plan = parse("SELECT id FROM t WHERE a NOT ILIKE 'x%'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Like(l) => {
                assert!(l.case_insensitive);
                assert!(l.negated, "NOT ILIKE must set negated");
            }
            other => panic!("expected Like, got {other:?}"),
        }
    }

    #[test]
    fn lower_like_any_folds_to_or_chain() {
        // pr-003: `name LIKE ANY ('A%', '%e')` → (name LIKE 'A%') OR (name LIKE '%e').
        let plan =
            parse("SELECT id FROM t WHERE name LIKE ANY ('A%', '%e')").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Binary(BinaryExpression { op, left, right }) => {
                assert_eq!(*op, BinaryOp::Or);
                for side in [&**left, &**right] {
                    match side {
                        Expression::Like(l) => {
                            assert!(!l.negated && !l.case_insensitive);
                        }
                        other => panic!("expected Like leaf, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Binary(Or), got {other:?}"),
        }
    }

    #[test]
    fn lower_like_all_folds_to_and_chain() {
        // pr-004: sqlparser 0.61 mis-parses `LIKE ALL (…)` as `LIKE ALL(...)`
        // (function call). We detect that artifact and fold into an AND-chain.
        // This test also PINS that parser artifact: a future sqlparser that
        // gains native `LIKE ALL` changes the parse and fails here loudly.
        let plan = parse("SELECT id FROM t WHERE name LIKE ALL ('%a%', '%e%')")
            .expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Binary(BinaryExpression { op, left, right }) => {
                assert_eq!(*op, BinaryOp::And);
                assert!(matches!(&**left, Expression::Like(_)));
                assert!(matches!(&**right, Expression::Like(_)));
            }
            other => panic!("expected Binary(And), got {other:?}"),
        }
    }

    #[test]
    fn lower_not_like_any_negates_flipped_chain() {
        // No corpus witness, kept Spark-correct: NOT flips the quantifier.
        // Spark `NotLikeANY` = ∃¬ = NOT(AND-chain) (= NOT LikeAll), NOT NOT(OR).
        let plan = parse("SELECT id FROM t WHERE name NOT LIKE ANY ('A%', '%e')")
            .expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Unary(UnaryExpression { op, operand }) => {
                assert_eq!(*op, UnaryOp::Not);
                assert!(
                    matches!(&**operand, Expression::Binary(b) if b.op == BinaryOp::And),
                    "NOT LIKE ANY must be NOT(AND-chain), got {operand:?}"
                );
            }
            other => panic!("expected Unary(Not), got {other:?}"),
        }
    }

    #[test]
    fn lower_not_like_all_negates_flipped_chain() {
        // Spark `NotLikeAll` = ∀¬ = NOT(OR-chain) (= NOT LikeAny).
        let plan = parse("SELECT id FROM t WHERE name NOT LIKE ALL ('%a%', '%e%')")
            .expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Unary(UnaryExpression { op, operand }) => {
                assert_eq!(*op, UnaryOp::Not);
                assert!(
                    matches!(&**operand, Expression::Binary(b) if b.op == BinaryOp::Or),
                    "NOT LIKE ALL must be NOT(OR-chain), got {operand:?}"
                );
            }
            other => panic!("expected Unary(Not), got {other:?}"),
        }
    }

    #[test]
    fn lower_like_all_guard_does_not_misfire_on_over() {
        // A real function call named `all` with an OVER clause fails the artifact
        // guard, so it is NOT treated as LIKE ALL — it flows to the generic
        // single-pattern Like arm (whose pattern lowering then handles the call).
        // Here we only assert it does NOT become an AND-chain of >1 Like.
        let plan = parse("SELECT id FROM t WHERE name LIKE all(x) OVER ()");
        // Either it lowers to a single Like (pattern = window fn) or errors; it
        // must NOT be a Binary(And) of multiple Likes.
        if let Ok(plan) = plan {
            if let Expression::Binary(BinaryExpression { op, .. }) = where_predicate(&plan) {
                assert_ne!(
                    *op,
                    BinaryOp::And,
                    "windowed all() must not be desugared as LIKE ALL"
                );
            }
        }
    }

    #[test]
    fn lower_plain_like_still_single_pattern() {
        // Regression guard: ordinary `LIKE 'p'` (any:false, non-ALL pattern) must
        // still reach the unchanged single-pattern arm.
        let plan = parse("SELECT id FROM t WHERE a LIKE 'x%'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Like(l) => assert!(!l.negated && !l.case_insensitive),
            other => panic!("expected bare Like, got {other:?}"),
        }
    }

    #[test]
    fn lower_ilike_any_is_boundary_error() {
        // ILIKE ANY is not implemented; must fail loud (not silently drop ANY).
        assert_eq!(
            boundary_shape("SELECT id FROM t WHERE a ILIKE ANY ('x%', 'y%')"),
            "sql::ilike_any_unsupported"
        );
    }

    #[test]
    fn lower_row_in_folds_to_or_of_and_of_null_safe_eq() {
        // pr-005: `(a, b) IN ((1, 2), (3, 4))` → NULL-SAFE (Spark row-IN is struct
        // equality): (a <=> 1 AND b <=> 2) OR (a <=> 3 AND b <=> 4). The leaves
        // MUST be IsDistinctFrom (negated:true), NOT Binary(Eq).
        let plan =
            parse("SELECT id FROM t WHERE (a, b) IN ((1, 2), (3, 4))").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Binary(BinaryExpression { op, left, right }) => {
                assert_eq!(*op, BinaryOp::Or);
                for tuple_pred in [&**left, &**right] {
                    match tuple_pred {
                        Expression::Binary(BinaryExpression {
                            op: inner_op,
                            left: il,
                            right: ir,
                        }) => {
                            assert_eq!(*inner_op, BinaryOp::And);
                            for eq in [&**il, &**ir] {
                                match eq {
                                    Expression::IsDistinctFrom(d) => assert!(
                                        d.negated,
                                        "row-IN component must be IS NOT DISTINCT FROM"
                                    ),
                                    other => {
                                        panic!("expected null-safe IsDistinctFrom, got {other:?}")
                                    }
                                }
                            }
                        }
                        other => panic!("expected inner Binary(And), got {other:?}"),
                    }
                }
            }
            other => panic!("expected Binary(Or), got {other:?}"),
        }
    }

    #[test]
    fn lower_row_not_in_wraps_in_not() {
        let plan = parse("SELECT id FROM t WHERE (a, b) NOT IN ((1, 2), (3, 4))")
            .expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Unary(UnaryExpression { op, operand }) => {
                assert_eq!(*op, UnaryOp::Not);
                assert!(matches!(&**operand, Expression::Binary(b) if b.op == BinaryOp::Or));
            }
            other => panic!("expected Unary(Not), got {other:?}"),
        }
    }

    #[test]
    fn lower_row_in_arity_mismatch_is_boundary_error() {
        // NB: sqlparser reads `(3)` as a parenthesized scalar, not a 1-tuple,
        // so the mismatch surfaces through the row-IN non-tuple arm — pin the
        // `sql::in_row::` family rather than the specific arm.
        assert_boundary_shape_prefix(
            "SELECT id FROM t WHERE (a, b) IN ((1, 2), (3))",
            "sql::in_row::",
        );
    }

    #[test]
    fn lower_row_in_non_tuple_element_is_boundary_error() {
        assert_eq!(
            boundary_shape("SELECT id FROM t WHERE (a, b) IN (5, (1, 2))"),
            "sql::in_row::non_tuple_element"
        );
    }

    #[test]
    fn lower_scalar_in_still_uses_inlist() {
        // Regression: single-column IN keeps the scalar InListExpression path.
        let plan = parse("SELECT id FROM t WHERE a IN (1, 2)").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::InList(l) => assert!(!l.negated),
            other => panic!("expected InList, got {other:?}"),
        }
        let plan = parse("SELECT id FROM t WHERE a NOT IN (1, 2)").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::InList(l) => assert!(l.negated),
            other => panic!("expected InList, got {other:?}"),
        }
    }

    #[test]
    fn lower_between_maps_to_between_not_negated() {
        // whr-007 shape: `age BETWEEN 30 AND 45` → inclusive Between.
        let plan =
            parse("SELECT * FROM emp WHERE age BETWEEN 30 AND 45").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Between(b) => {
                assert!(!b.negated, "BETWEEN must not set negated");
            }
            other => panic!("expected Between, got {other:?}"),
        }
    }

    #[test]
    fn lower_not_between_sets_negated() {
        let plan =
            parse("SELECT * FROM emp WHERE age NOT BETWEEN 30 AND 45").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Between(b) => {
                assert!(b.negated, "NOT BETWEEN must set negated");
            }
            other => panic!("expected Between, got {other:?}"),
        }
    }

    #[test]
    fn lower_rlike_maps_to_rlike_function() {
        // whr-013 shape: `name RLIKE 'p'` → rlike(name, 'p').
        let plan = parse("SELECT id FROM t WHERE a RLIKE 'p'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::FunctionCall(f) => {
                assert_eq!(f.name, "rlike");
                assert_eq!(f.args.len(), 2);
                assert!(!f.distinct);
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn lower_not_rlike_wraps_in_not() {
        let plan = parse("SELECT id FROM t WHERE a NOT RLIKE 'p'").expect("should parse+lower");
        match where_predicate(&plan) {
            Expression::Unary(u) => {
                assert!(matches!(u.op, UnaryOp::Not));
                match u.operand.as_ref() {
                    Expression::FunctionCall(f) => {
                        assert_eq!(f.name, "rlike");
                        assert_eq!(f.args.len(), 2);
                    }
                    other => panic!("expected rlike FunctionCall, got {other:?}"),
                }
            }
            other => panic!("expected Unary NOT, got {other:?}"),
        }
    }

    // ── Complex-type bracket access + nested struct paths (cx-001/002/004) ──

    #[test]
    fn lower_array_index_builds_extract_value_over_array() {
        let plan = parse("SELECT array(1,2,3)[0]").expect("should parse+lower");
        match first_projection(plan) {
            Expression::ExtractValue(ev) => {
                match ev.child.as_ref() {
                    Expression::FunctionCall(f) => assert_eq!(f.name, "array"),
                    other => panic!("expected array FunctionCall child, got {other:?}"),
                }
                match ev.extraction.as_ref() {
                    Expression::Literal(l) => {
                        assert!(matches!(l.value, LiteralValue::Int(0)));
                    }
                    other => panic!("expected Int(0) extraction, got {other:?}"),
                }
            }
            other => panic!("expected ExtractValue, got {other:?}"),
        }
    }

    #[test]
    fn lower_map_key_builds_extract_value_with_string_key() {
        let plan = parse("SELECT map('a',1)['a']").expect("should parse+lower");
        match first_projection(plan) {
            Expression::ExtractValue(ev) => match ev.extraction.as_ref() {
                Expression::Literal(l) => {
                    assert!(matches!(&l.value, LiteralValue::String(s) if s == "a"));
                }
                other => panic!("expected String(\"a\") extraction, got {other:?}"),
            },
            other => panic!("expected ExtractValue, got {other:?}"),
        }
    }

    #[test]
    fn lower_three_part_path_keeps_first_as_qualifier_and_dotted_remainder() {
        let plan = parse("SELECT address.geo.lat FROM emp").expect("should parse+lower");
        match first_projection(plan) {
            Expression::UnresolvedColumn(c) => {
                assert_eq!(c.qualifier.as_deref(), Some("address"));
                assert_eq!(c.name, "geo.lat");
            }
            other => panic!("expected UnresolvedColumn, got {other:?}"),
        }
    }

    #[test]
    fn lower_simple_case_wraps_branch_condition_in_eq_of_operand() {
        // `CASE x WHEN 10 THEN 'a' ELSE 'b' END` — Spark rewrites each branch
        // condition to `EqualTo(x, 10)`.
        let plan = parse("SELECT CASE x WHEN 10 THEN 'a' ELSE 'b' END FROM t")
            .expect("should parse+lower");
        match first_projection(plan) {
            Expression::CaseWhen(cw) => {
                let (cond, _) = &cw.branches[0];
                match cond {
                    Expression::Binary(b) => {
                        assert_eq!(b.op, BinaryOp::Eq);
                        assert!(
                            matches!(b.left.as_ref(), Expression::UnresolvedColumn(c) if c.name == "x"),
                            "left of Eq should be the CASE operand `x`, got {:?}",
                            b.left
                        );
                        assert!(
                            matches!(b.right.as_ref(), Expression::Literal(_)),
                            "right of Eq should be the branch value literal, got {:?}",
                            b.right
                        );
                    }
                    other => panic!("expected Binary(Eq, ...) branch condition, got {other:?}"),
                }
            }
            other => panic!("expected CaseWhen, got {other:?}"),
        }
    }

    #[test]
    fn lower_searched_case_keeps_raw_branch_predicate() {
        // Searched CASE (`operand: None`) — branch condition stays the raw
        // predicate, not wrapped in an operand Eq.
        let plan = parse("SELECT CASE WHEN x > 10 THEN 'a' ELSE 'b' END FROM t")
            .expect("should parse+lower");
        match first_projection(plan) {
            Expression::CaseWhen(cw) => {
                let (cond, _) = &cw.branches[0];
                match cond {
                    Expression::Binary(b) => assert_eq!(
                        b.op,
                        BinaryOp::Gt,
                        "searched CASE keeps its raw `>` predicate"
                    ),
                    other => panic!("expected raw Binary(Gt, ...) predicate, got {other:?}"),
                }
            }
            other => panic!("expected CaseWhen, got {other:?}"),
        }
    }

    /// Extract the last aggregate expression from a top-level `Aggregate` op,
    /// unwrapping a synthetic top-level `Alias` (the SparkSQL default name).
    fn last_aggregate(plan: CommonAst) -> Expression {
        let CommonOp::Aggregate { mut aggregates, .. } = plan.op else {
            panic!("expected Aggregate as top-level");
        };
        match aggregates.pop().expect("at least one aggregate") {
            Expression::Alias(a) => *a.expr,
            other => other,
        }
    }

    #[test]
    fn agg_filter_count_star_desugars_to_case_when_one() {
        // `count(*) FILTER (WHERE salary > 90000)` desugars to
        // `count(CASE WHEN salary > 90000 THEN 1 END)` — the star arg has no
        // value to wrap, so the matching rows contribute a non-NULL `1`.
        // Corpus witness: `agg-017`.
        let plan =
            parse("SELECT count(*) FILTER (WHERE salary > 90000) FROM emp").expect("should parse");
        match last_aggregate(plan) {
            Expression::FunctionCall(fc) => {
                assert!(fc.name.eq_ignore_ascii_case("count"));
                assert!(!fc.distinct);
                assert_eq!(fc.args.len(), 1, "single desugared arg");
                match &fc.args[0] {
                    Expression::CaseWhen(cw) => {
                        assert_eq!(cw.branches.len(), 1);
                        assert!(cw.else_expr.is_none(), "no ELSE — non-matching rows NULL");
                        let (cond, then) = &cw.branches[0];
                        assert!(
                            matches!(cond, Expression::Binary(b) if b.op == BinaryOp::Gt),
                            "predicate is `salary > 90000`"
                        );
                        assert!(
                            matches!(
                                then,
                                Expression::Literal(Literal {
                                    value: LiteralValue::Int(1),
                                    data_type: DataType::Integer,
                                })
                            ),
                            "count-star THEN branch is literal 1"
                        );
                    }
                    other => panic!("expected CaseWhen arg, got {other:?}"),
                }
            }
            other => panic!("expected FunctionCall(count), got {other:?}"),
        }
    }

    #[test]
    fn agg_filter_sum_wraps_argument_in_case_when() {
        // `sum(salary) FILTER (WHERE dept_id = 10)` desugars to
        // `sum(CASE WHEN dept_id = 10 THEN salary END)`.
        let plan =
            parse("SELECT sum(salary) FILTER (WHERE dept_id = 10) FROM emp").expect("should parse");
        match last_aggregate(plan) {
            Expression::FunctionCall(fc) => {
                assert!(fc.name.eq_ignore_ascii_case("sum"));
                assert!(!fc.distinct);
                assert_eq!(fc.args.len(), 1);
                match &fc.args[0] {
                    Expression::CaseWhen(cw) => {
                        assert_eq!(cw.branches.len(), 1);
                        assert!(cw.else_expr.is_none());
                        let (cond, then) = &cw.branches[0];
                        assert!(
                            matches!(cond, Expression::Binary(b) if b.op == BinaryOp::Eq),
                            "predicate is `dept_id = 10`"
                        );
                        assert!(
                            matches!(then, Expression::UnresolvedColumn(c) if c.name == "salary"),
                            "THEN branch is the wrapped `salary` argument"
                        );
                    }
                    other => panic!("expected CaseWhen arg, got {other:?}"),
                }
            }
            other => panic!("expected FunctionCall(sum), got {other:?}"),
        }
    }

    #[test]
    fn agg_filter_preserves_distinct() {
        // `count(DISTINCT id) FILTER (WHERE p)` keeps DISTINCT while wrapping the
        // argument → `count(DISTINCT CASE WHEN p THEN id END)`.
        let plan = parse("SELECT count(DISTINCT id) FILTER (WHERE dept_id = 10) FROM emp")
            .expect("should parse");
        match last_aggregate(plan) {
            Expression::FunctionCall(fc) => {
                assert!(fc.name.eq_ignore_ascii_case("count"));
                assert!(fc.distinct, "DISTINCT is preserved through the desugar");
                assert_eq!(fc.args.len(), 1);
                match &fc.args[0] {
                    Expression::CaseWhen(cw) => {
                        let (_, then) = &cw.branches[0];
                        assert!(
                            matches!(then, Expression::UnresolvedColumn(c) if c.name == "id"),
                            "THEN branch is the wrapped `id` argument"
                        );
                    }
                    other => panic!("expected CaseWhen arg, got {other:?}"),
                }
            }
            other => panic!("expected FunctionCall(count), got {other:?}"),
        }
    }

    #[test]
    fn filter_on_non_aggregate_is_boundary_error() {
        // Spark rejects `FILTER (WHERE …)` on a non-aggregate function; the
        // desugar must not silently turn `abs(x) FILTER (WHERE p)` into valid
        // SQL. τ surfaces an honest Thunderduck-boundary error instead.
        assert_eq!(
            boundary_shape("SELECT abs(x) FILTER (WHERE x > 0) FROM emp"),
            "sql::filter_on_non_aggregate"
        );
    }

    #[test]
    fn hex_literal_lowers_to_binary_bytes() {
        // Spark `X'1F2A'` is a 2-byte BINARY literal — [0x1F, 0x2A].
        let plan = parse("SELECT X'1F2A' AS h").expect("should parse");
        let projections = match plan.op {
            CommonOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        let lit = match &projections[0] {
            Expression::Alias(a) => &*a.expr,
            other => panic!("expected Alias, got {other:?}"),
        };
        match lit {
            Expression::Literal(Literal { value, data_type }) => {
                assert_eq!(*value, LiteralValue::Binary(vec![0x1F, 0x2A]));
                assert_eq!(*data_type, DataType::Binary);
            }
            other => panic!("expected Binary literal, got {other:?}"),
        }
    }

    #[test]
    fn string_literal_decodes_backslash_escapes() {
        // Spark decodes C-style escapes in single-quoted literals: `\n`→LF,
        // `\t`→TAB. With SparkDialect::supports_string_literal_backslash_escape
        // the tokenizer decodes them, so the τ String literal holds the real
        // control chars (not a literal backslash). Corpus witness: lit-009.
        // NB: in this Rust source `\\n` is the two chars backslash-n in the SQL
        // text; the expected value uses `\n` which is a real newline byte.
        let plan = parse(r"SELECT 'line1\nline2' AS s, 'tab\there' AS t").expect("should parse");
        let projections = match plan.op {
            CommonOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        let string_of = |e: &Expression| -> String {
            match e {
                Expression::Alias(a) => match &*a.expr {
                    Expression::Literal(Literal {
                        value: LiteralValue::String(s),
                        ..
                    }) => s.clone(),
                    other => panic!("expected String literal, got {other:?}"),
                },
                other => panic!("expected Alias, got {other:?}"),
            }
        };
        assert_eq!(string_of(&projections[0]), "line1\nline2");
        assert_eq!(string_of(&projections[1]), "tab\there");
    }

    #[test]
    fn decode_hex_literal_decodes_pairs() {
        assert_eq!(decode_hex_literal("1F2A").expect("valid"), vec![0x1F, 0x2A]);
        assert_eq!(decode_hex_literal("41").expect("valid"), vec![0x41]);
        assert_eq!(decode_hex_literal("").expect("valid"), Vec::<u8>::new());
    }

    #[test]
    fn decode_hex_literal_odd_length_is_boundary_error() {
        assert_eq!(
            boundary_shape_of(decode_hex_literal("1")),
            "sql::value::hex_odd_length"
        );
    }

    #[test]
    fn decode_hex_literal_invalid_digit_is_boundary_error() {
        assert_eq!(
            boundary_shape_of(decode_hex_literal("1G")),
            "sql::value::hex_invalid_digit"
        );
    }

    // ── LATERAL VIEW lowering (cx-007/cx-008/cx-009) ────────────────────

    /// Helper: extract a `CommonOp::LateralView` from a lowered SQL that
    /// produces `Project { input: LateralView { .. }, .. }`.
    fn lateral_view_of(plan: CommonAst) -> (String, Vec<(String, Expression)>) {
        match plan.op {
            CommonOp::Project { input, .. } => match input.op {
                CommonOp::LateralView {
                    table_alias,
                    columns,
                    ..
                } => (table_alias, columns),
                other => panic!("expected LateralView under Project, got {other:?}"),
            },
            other => panic!("expected Project at top, got {other:?}"),
        }
    }

    #[test]
    fn lateral_view_explode_fires_with_correct_alias_and_column() {
        let plan = parse("SELECT e.id, t.tag FROM emp e LATERAL VIEW explode(e.tags) t AS tag")
            .expect("should parse");
        let (alias, cols) = lateral_view_of(plan);
        assert_eq!(alias, "t");
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].0, "tag");
        match &cols[0].1 {
            Expression::FunctionCall(f) => {
                assert_eq!(f.name, "explode");
                assert_eq!(f.args.len(), 1);
            }
            other => panic!("expected explode FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn lateral_view_outer_folds_to_explode_outer() {
        let plan =
            parse("SELECT e.id, t.tag FROM emp e LATERAL VIEW OUTER explode(e.tags) t AS tag")
                .expect("should parse");
        let (_, cols) = lateral_view_of(plan);
        match &cols[0].1 {
            Expression::FunctionCall(f) => assert_eq!(f.name, "explode_outer"),
            other => panic!("expected explode_outer FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn lateral_view_posexplode_splits_into_pos_and_val() {
        let plan = parse(
            "SELECT e.id, t.pos, t.tag FROM emp e LATERAL VIEW posexplode(e.tags) t AS pos, tag",
        )
        .expect("should parse");
        let (alias, cols) = lateral_view_of(plan);
        assert_eq!(alias, "t");
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].0, "pos");
        assert_eq!(cols[1].0, "tag");
        match &cols[0].1 {
            Expression::FunctionCall(f) => assert_eq!(f.name, "posexplode_pos"),
            other => panic!("expected posexplode_pos, got {other:?}"),
        }
        match &cols[1].1 {
            Expression::FunctionCall(f) => assert_eq!(f.name, "posexplode_val"),
            other => panic!("expected posexplode_val, got {other:?}"),
        }
    }

    #[test]
    fn lateral_view_chained_is_boundary_error() {
        assert_eq!(
            boundary_shape(
                "SELECT * FROM emp e \
                 LATERAL VIEW explode(e.tags) t AS tag \
                 LATERAL VIEW explode(e.tags) t2 AS tag2"
            ),
            "sql::lateral_view::chained"
        );
    }

    #[test]
    fn lateral_view_posexplode_one_alias_is_boundary_error() {
        assert_eq!(
            boundary_shape("SELECT * FROM emp e LATERAL VIEW posexplode(e.tags) t AS tag"),
            "sql::lateral_view::posexplode_alias_count"
        );
    }

    #[test]
    fn lateral_view_outer_posexplode_is_boundary_error() {
        assert_eq!(
            boundary_shape(
                "SELECT * FROM emp e LATERAL VIEW OUTER posexplode(e.tags) t AS pos, tag"
            ),
            "sql::lateral_view::outer_posexplode"
        );
    }

    #[test]
    fn lateral_view_unknown_generator_is_boundary_error() {
        let shape = boundary_shape("SELECT * FROM emp e LATERAL VIEW inline(e.structs) t AS v");
        assert!(
            shape.starts_with("sql::lateral_view::generator::"),
            "expected generator boundary shape, got `{shape}`"
        );
    }

    // ── Pass 13 — LATERAL generator comma-syntax redirect to LateralView ──

    /// CONVERGENCE: `FROM emp e, LATERAL explode(e.tags) AS r(v)` produces a
    /// CommonAst structurally EQUAL to `FROM emp e LATERAL VIEW explode(e.tags) r
    /// AS v`. Both syntaxes must converge to the same LateralView node shape.
    #[test]
    fn comma_lateral_explode_converges_with_lateral_view_syntax() {
        let comma_plan = parse("SELECT e.id, r.v FROM emp e, LATERAL explode(e.tags) AS r(v)")
            .expect("comma LATERAL should parse");
        let lv_plan = parse("SELECT e.id, r.v FROM emp e LATERAL VIEW explode(e.tags) r AS v")
            .expect("LATERAL VIEW should parse");
        assert_eq!(
            comma_plan, lv_plan,
            "comma-LATERAL and LATERAL VIEW must produce identical CommonAst"
        );
    }

    /// DISCRIMINATOR regression guard: non-LATERAL `FROM emp, explode(array(1,2))`
    /// still lowers to a CrossJoin with right=TableFunction, NOT a LateralView.
    /// sqlparser parses this as `TableFactor::Table { args: Some(...) }` (not
    /// `TableFactor::Function`), so the redirect predicate never fires.
    #[test]
    fn non_lateral_comma_explode_lowers_to_cross_join_not_lateral_view() {
        let plan =
            parse("SELECT * FROM emp, explode(array(1, 2))").expect("non-LATERAL should parse");
        match plan.op {
            CommonOp::Project { input, .. } => match input.op {
                CommonOp::Join {
                    join_type,
                    ref right,
                    ..
                } => {
                    assert_eq!(join_type, JoinType::Cross, "must be a CrossJoin");
                    assert!(
                        matches!(right.op, CommonOp::TableFunction { .. }),
                        "right side must be TableFunction, got: {:?}",
                        right.op
                    );
                }
                other => panic!("expected Join under Project, got {other:?}"),
            },
            other => panic!("expected Project, got {other:?}"),
        }
    }

    /// The comma-LATERAL redirect handles `posexplode` with 2-alias correctly,
    /// producing the same split as LATERAL VIEW syntax.
    #[test]
    fn comma_lateral_posexplode_two_alias_matches_lateral_view() {
        let comma_plan = parse(
            "SELECT e.id, r.pos, r.val FROM emp e, LATERAL posexplode(e.tags) AS r(pos, val)",
        )
        .expect("comma LATERAL posexplode should parse");
        let lv_plan = parse(
            "SELECT e.id, r.pos, r.val FROM emp e LATERAL VIEW posexplode(e.tags) r AS pos, val",
        )
        .expect("LATERAL VIEW posexplode should parse");
        assert_eq!(
            comma_plan, lv_plan,
            "comma-LATERAL and LATERAL VIEW posexplode must produce identical CommonAst"
        );
    }

    // ── Pass-17: LATERAL derived-table join lowering ────────────────────

    #[test]
    fn lateral_join_no_on_lowers_to_join_with_lateral_true() {
        // tbl-005 shape: `JOIN LATERAL (subquery) t` with no ON clause.
        let plan = parse(
            "SELECT e.name, t.dept_avg \
             FROM emp e \
             JOIN LATERAL (SELECT avg(e2.salary) AS dept_avg FROM emp e2 WHERE e2.dept_id = e.dept_id) t",
        )
        .expect("LATERAL join should parse");
        match project_input(plan).op {
            CommonOp::Join {
                join_type,
                condition,
                natural,
                lateral,
                using_columns,
                ..
            } => {
                assert_eq!(join_type, JoinType::Inner, "no ON → Inner at parse time");
                assert!(condition.is_none(), "no ON clause");
                assert!(using_columns.is_empty());
                assert!(!natural);
                assert!(lateral, "LATERAL derived table must set lateral: true");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn lateral_join_with_on_lowers_to_join_with_lateral_true_and_condition() {
        // `JOIN LATERAL (...) t ON <cond>` → lateral:true + condition:Some.
        let plan = parse(
            "SELECT * FROM emp e \
             JOIN LATERAL (SELECT 1 AS x) t ON t.x = e.id",
        )
        .expect("LATERAL join with ON should parse");
        match project_input(plan).op {
            CommonOp::Join {
                lateral, condition, ..
            } => {
                assert!(lateral, "LATERAL must be true");
                assert!(condition.is_some(), "ON clause must be present");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn plain_on_join_lowers_with_lateral_false() {
        // Regression: a plain `JOIN ... ON` must not set lateral.
        let plan = parse("SELECT * FROM emp JOIN dept ON emp.dept_id = dept.dept_id")
            .expect("should parse");
        match project_input(plan).op {
            CommonOp::Join { lateral, .. } => {
                assert!(!lateral, "plain ON join must not set lateral");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn comma_lateral_subquery_lowers_with_lateral_true() {
        // Comma form: `, LATERAL (subquery) t` — Spark treats identically to
        // `JOIN LATERAL (subquery) t`.
        let plan = parse("SELECT e.name, t.x FROM emp e, LATERAL (SELECT 1 AS x) t")
            .expect("comma LATERAL subquery should parse");
        match project_input(plan).op {
            CommonOp::Join {
                lateral, join_type, ..
            } => {
                assert!(lateral, "comma LATERAL subquery must set lateral: true");
                assert_eq!(join_type, JoinType::Cross, "comma fold uses Cross");
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn delta_backtick_path_lowers_to_file_scan() {
        // `SELECT * FROM delta.`/tmp/t`` → Project { FileScan { Delta, ["/tmp/t"] } }
        let plan = parse("SELECT * FROM delta.`/tmp/t`").expect("should parse");
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project");
        };
        match input.op {
            CommonOp::FileScan {
                format,
                ref paths,
                ref schema,
                ..
            } => {
                assert_eq!(format, FileFormat::Delta);
                assert_eq!(paths, &["/tmp/t".to_owned()]);
                assert!(schema.is_none(), "schema should be None (discovered later)");
            }
            other => panic!("expected FileScan, got {other:?}"),
        }
    }

    #[test]
    fn parquet_backtick_path_lowers_to_file_scan() {
        // Also test the parquet variant of the same syntax.
        let plan = parse("SELECT * FROM parquet.`/data/f.parquet`").expect("should parse");
        let CommonOp::Project { input, .. } = plan.op else {
            panic!("expected Project");
        };
        match input.op {
            CommonOp::FileScan {
                format, ref paths, ..
            } => {
                assert_eq!(format, FileFormat::Parquet);
                assert_eq!(paths, &["/data/f.parquet".to_owned()]);
            }
            other => panic!("expected FileScan, got {other:?}"),
        }
    }
}
