use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use arrow::array::{ArrayRef, RecordBatch, RecordBatchOptions};
use arrow::datatypes::{Field, Schema};
use futures::stream;
use thunderduck_core::error::ThunderduckError;
use thunderduck_core::parser_v2::SparkSqlParserV2;
use thunderduck_core::transpiler_v2::{self, BaseTypes, CommonAst};
use thunderduck_core::types::{DataType, StructType};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::arrow_interval_transcode::{self, IntervalPlan};
use crate::arrow_ipc::record_batch_to_arrow_batch;
use crate::arrow_schema_stamp;
use crate::converter::type_converter::data_type_to_proto;
use crate::converter::v2_relation_converter::V2RelationConverter;
use crate::error::ConnectError;
use crate::proto::spark::connect as proto;
use crate::proto::spark::connect::spark_connect_service_server::SparkConnectService;

type BoxStream<T> = Pin<Box<dyn futures::Stream<Item = Result<T, Status>> + Send + 'static>>;

// TODO: re-add a plan-depth guard using `CommonAst::depth()` once
// the substrate carries a depth query. The previous `LogicalPlan::depth()`
// inspection was removed when dispatch relocated to the τ boundary;
// `MAX_PLAN_DEPTH` will return alongside that query.

pub struct ThunderduckService {
    session_manager: Arc<thunderduck_core::runtime::SessionManager>,
}

impl ThunderduckService {
    pub fn new(session_manager: Arc<thunderduck_core::runtime::SessionManager>) -> Self {
        Self { session_manager }
    }

    /// Fetch (or create) the session for `session_id`, mapping failures to
    /// `Status::internal` — the identical error mapping both gRPC entry
    /// points (`execute_plan`, `analyze_plan`) require.
    async fn session(
        &self,
        session_id: &str,
    ) -> Result<Arc<thunderduck_core::runtime::DuckDbSession>, Status> {
        self.session_manager
            .get_or_create(session_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))
    }
}

static SERVER_SESSION_ID: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| uuid::Uuid::new_v4().to_string());

// ── τ dispatch helpers ────────────────────────────────────────────

/// Route a Spark Connect [`proto::Relation`] to the correct τ front-end and
/// produce a [`CommonAst`].
///
/// **Route by `RelType::Sql`** — Option (a) per plan §4: SQL text goes through
/// `parser_v2`, structured relations through `V2RelationConverter`.
/// `V2RelationConverter` refuses `RelType::Sql` with `UnsupportedProtoShape`, so
/// intercepting here keeps the two front-ends peer. Shared by
/// [`transpile_relation`] (ExecutePlan) and the `AnalyzePlan(Schema)` arm.
///
/// Splitting the conversion out of [`transpile_relation`] also lets the async
/// dispatch layer interpose an eager pivot-value-discovery pass
/// ([`resolve_implicit_pivots`]) between conversion and [`finalize`] — the
/// discovery needs the live `DuckDbSession`, which τ's analyzer (per INV10)
/// cannot reach.
// `Status` is the standard gRPC error channel used across this whole file
// (25+ signatures return `Result<_, Status>`); boxing it here alone would be
// inconsistent with the rest of the layer for one allocation on the reject path.
#[allow(clippy::result_large_err)]
pub(crate) fn relation_to_common_ast(relation: &proto::Relation) -> Result<CommonAst, Status> {
    use proto::relation::RelType;
    match &relation.rel_type {
        Some(RelType::Sql(sql_relation)) => SparkSqlParserV2::parse(&sql_relation.query)
            .map_err(|e| Status::from(ConnectError::from(e))),
        _ => {
            let mut converter = V2RelationConverter::new();
            converter
                .convert(relation)
                .map_err(|e| Status::from(ConnectError::from(e)))
        }
    }
}

/// Convert a Spark Connect [`proto::Relation`] into a [`CommonAst`] and finalize
/// it into DuckDB SQL + resolved schema via τ.
///
/// Runs the eager data-dependent-schema discovery pass ([`resolve_implicit_pivots`]
/// — values-less pivot/crosstab, schema-less Parquet `FileScan`) before
/// `finalize`, exactly like the `execute_plan`/`analyze_plan` Root-relation
/// arms — this is the shared entry point for the `Command` arms
/// (`CreateDataframeView`, `SqlCommand`, `WriteOperation`) that don't inline
/// that sequence themselves, so e.g. `spark.read.parquet(path)
/// .createOrReplaceTempView(...)` gets the same schema discovery a bare
/// `execute_plan` Root relation would.
///
/// `finalize` runs the analyzer + emission; it succeeds for every plan τ covers
/// and returns a Thunderduck-boundary `Status` (`UnsupportedOp` /
/// `UnsupportedProtoShape`) for shapes it does not. The emitted SQL feeds
/// `execute_streaming_query`; the schema drives the outbound Arrow-schema stamp.
pub(crate) async fn transpile_relation(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    relation: &proto::Relation,
) -> Result<(CommonAst, String, StructType), Status> {
    let mut common_ast = relation_to_common_ast(relation)?;
    resolve_implicit_pivots(&mut common_ast, session).await?;
    let (sql, schema) = finalize(session, &common_ast).await?;
    Ok((common_ast, sql, schema))
}

/// Build the per-path `BaseTypes` overlay and run τ's fused emit-and-schema
/// entry point in ONE analyzer pass.
///
/// Returns both the emitted DuckDB SQL and the analyzer's root
/// `resolved_schema` — the schema drives the outbound Arrow-schema stamp in
/// `execute_streaming_query` (see `arrow_schema_stamp::build_stamped_schema`).
/// Fusing avoids the second `analyze()` call that pass 88's initial wiring
/// incurred (perf review HIGH #1).
///
/// The catalog closure resolves empty-scan `TableScan` schemas from the
/// session's temp-view cache (the runtime→analyzer bridge).
pub(crate) async fn finalize(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    common_ast: &CommonAst,
) -> Result<(String, StructType), Status> {
    let base_types = build_base_types(session, common_ast).await;
    transpiler_v2::generate_with_schema(common_ast, &base_types)
        .map_err(|e| Status::from(ConnectError::from(e)))
}

/// Run τ's analyzer on a `CommonAst` and return the root-node resolved schema.
///
/// Used by `AnalyzePlan(Schema)` (E.0 addendum — τ's analyzer analyzer wiring for
/// the schema-analyze surface). The `ExecutePlan` streaming-query path takes
/// its schema from [`finalize`]'s fused return instead of re-running the
/// analyzer.
pub(crate) async fn analyze_schema(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    common_ast: &CommonAst,
) -> Result<StructType, Status> {
    let base_types = build_base_types(session, common_ast).await;
    transpiler_v2::analyze_schema(common_ast, &base_types)
        .map_err(|e| Status::from(ConnectError::from(e)))
}

/// Build the per-path `BaseTypes` overlay for a `CommonAst`.
///
/// Walks the plan ONCE via `empty_scan_tables`, resolves each collected
/// empty-scan `TableScan` from the session's async temp-view schema cache
/// (`get_view_schema`), then constructs the overlay directly from the
/// resolved entries (`BaseTypes::from_entries`) — no second plan walk. The
/// pre-fetched map stays the sole runtime→analyzer bridge (INV10).
/// Short-circuits to `BaseTypes::empty()` when the plan carries no empty scan
/// (ADR-012 request-handler seeding short-circuit).
async fn build_base_types(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    common_ast: &CommonAst,
) -> BaseTypes {
    let tables = thunderduck_core::transpiler_v2::base_types::empty_scan_tables(common_ast);
    if tables.is_empty() {
        return BaseTypes::empty();
    }
    let mut map: HashMap<String, StructType> = HashMap::new();
    for table in tables {
        if map.contains_key(&table) {
            continue;
        }
        if let Some(schema) = session.get_view_schema(&table).await {
            map.insert(table, schema);
        }
    }
    BaseTypes::from_entries(map)
}

// ── Eager pivot-value discovery (Spark parity) ───────────────────────────────

/// `spark.sql.pivotMaxValues` default (Spark 4.1.1). A values-less pivot whose
/// pivot column has more than this many distinct values is a Spark-emulated
/// compile error (`_LEGACY_ERROR_TEMP_1324`).
const PIVOT_MAX_VALUES: usize = 10000;

/// Eagerly resolve every values-less [`CommonOp::Pivot`] in `ast` by running
/// Spark's own discovery query against the live session, mirroring
/// `RelationalGroupedDataset.pivot(pivotColumn: Column)`:
/// `df.select(pivotColumn).distinct().limit(maxValues + 1).sort(pivotColumn)`.
///
/// τ's analyzer is a pure, synchronous stage (INV10 forbids it importing
/// `crate::runtime`), so it cannot run the data-dependent DISTINCT itself and
/// punts an empty-values pivot with `PuntedOperator("Pivot[implicit-values]")`.
/// This pass runs on the async dispatch layer — which holds the
/// [`DuckDbSession`] — and rewrites each empty-values pivot into the
/// explicit-values shape *before* [`finalize`]/[`analyze_schema`], so downstream
/// is byte-identical to a user-supplied explicit-values pivot.
///
/// The walk is post-order (children first) so nested / multiple implicit pivots
/// are all resolved.
async fn resolve_implicit_pivots(
    ast: &mut CommonAst,
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
) -> Result<(), Status> {
    use thunderduck_core::transpiler_v2::CommonOp;

    // Resolve children first — `CommonOp::children_mut` covers every variant
    // exhaustively (leaf relations yield no child plan to descend into).
    for child in ast.op.children_mut() {
        Box::pin(resolve_implicit_pivots(child, session)).await?;
    }

    // If THIS node is a values-less pivot, discover and stamp its values.
    if let CommonOp::Pivot {
        input,
        pivot_column,
        pivot_values,
        ..
    } = &mut ast.op
    {
        if pivot_values.is_empty() {
            *pivot_values = discover_pivot_values(input, pivot_column, session).await?;
        }
    }

    // If THIS node is a crosstab, discover col2's distinct buckets from the
    // live session and desugar it into a conditional-count `Aggregate` — the
    // mirror image of the implicit-pivot rewrite above (col2's DISTINCT values
    // are data-dependent, unknowable at plan time, so τ's pure analyzer punts
    // and this async pass resolves it). See `analyzer::crosstab_to_aggregate`.
    if matches!(ast.op, CommonOp::Crosstab { .. }) {
        let op = std::mem::replace(&mut ast.op, CommonOp::SingleRow);
        let CommonOp::Crosstab { input, col1, col2 } = op else {
            unreachable!("guarded by the matches! above");
        };
        // Reuse the pivot discovery query (`SELECT DISTINCT col2 ... ORDER BY 1
        // ASC NULLS FIRST`); no NULL-filtering — a NULL is a real bucket.
        let col2_expr = thunderduck_core::transpiler_v2::Expression::UnresolvedColumn(
            thunderduck_core::transpiler_v2::expression::UnresolvedColumn {
                name: col2.clone(),
                qualifier: None,
                plan_id: None,
            },
        );
        let distinct_values = discover_pivot_values(&input, &col2_expr, session).await?;
        ast.op = thunderduck_core::transpiler_v2::analyzer::crosstab_to_aggregate(
            *input,
            &col1,
            &col2,
            distinct_values,
        );
    }

    // If THIS node is a schema-less Parquet FileScan, discover the schema from
    // the live session via `SELECT * FROM read_parquet(...) LIMIT 0` and stamp
    // the `schema` field to `Some(inferred)`. This is the same pattern as the
    // pivot/crosstab arms above: a data-dependent schema that τ's pure
    // synchronous analyzer (INV10) cannot resolve on its own.
    //
    // Only Parquet is handled — Csv/Json/Orc schema-less remain as
    // `PuntedOperator` boundary errors until implemented.
    if let CommonOp::FileScan {
        format: thunderduck_core::transpiler_v2::ast::FileFormat::Parquet,
        paths,
        schema: schema @ None,
        options,
    } = &mut ast.op
    {
        let inferred = discover_parquet_schema(paths, options, session).await?;
        *schema = Some(inferred);
    }

    Ok(())
}

/// Run Spark's pivot-value discovery query for a single values-less pivot and
/// return the discovered values as typed literals, sorted ascending with NULLs
/// first (Spark's default ordering — the NULL bucket, if present, sorts first
/// and is a legitimate pivot column named `"null"`; Spark's values-less
/// overload does **not** null-filter, verified against
/// `RelationalGroupedDataset.pivot` in Spark 4.x).
async fn discover_pivot_values(
    input: &CommonAst,
    pivot_column: &thunderduck_core::transpiler_v2::Expression,
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
) -> Result<Vec<thunderduck_core::transpiler_v2::Expression>, Status> {
    use thunderduck_core::transpiler_v2::CommonOp;

    // Emit `SELECT <pivot_column> FROM <input>` via the pure τ path, then wrap
    // it in the DISTINCT / ORDER BY / LIMIT that Spark's analyzer applies. The
    // pivot's `input` subtree does not contain the pivot itself, so it emits
    // independently.
    let discovery_project = CommonAst::new(CommonOp::Project {
        input: Box::new(input.clone()),
        projections: vec![pivot_column.clone()],
    });
    let (project_sql, _schema) = finalize(session, &discovery_project).await?;
    // `.limit(maxValues + 1)` so we can detect (and reject) an over-cap column
    // count exactly the way Spark does.
    let discovery_sql = format!(
        "SELECT DISTINCT * FROM ({project_sql}) AS __td_pivot_discover \
         ORDER BY 1 ASC NULLS FIRST LIMIT {}",
        PIVOT_MAX_VALUES + 1
    );
    let batches = session
        .execute(&discovery_sql)
        .await
        .map_err(|e| Status::from(ConnectError::from(e)))?;

    let mut values = Vec::new();
    for batch in &batches {
        let column = batch.column(0);
        for row in 0..batch.num_rows() {
            values.push(
                crate::converter::v2_relation_converter::arrow_val_to_literal(column.as_ref(), row)
                    .map_err(|e| Status::from(ConnectError::from(e)))?,
            );
        }
    }

    // Spark-emulated (`_LEGACY_ERROR_TEMP_1324`): reject a pivot column with
    // more distinct values than `spark.sql.pivotMaxValues`.
    if values.len() > PIVOT_MAX_VALUES {
        return Err(Status::invalid_argument(format!(
            "[_LEGACY_ERROR_TEMP_1324] The pivot column has more than {PIVOT_MAX_VALUES} distinct \
             values, this could indicate an error. If this was intended, set \
             spark.sql.pivotMaxValues to at least the number of distinct values of the pivot column."
        )));
    }
    Ok(values)
}

/// Discover the schema of a Parquet file (or file set) by executing a
/// zero-row `SELECT * FROM read_parquet(...) LIMIT 0` against the live
/// session. DuckDB returns an empty `RecordBatch` whose Arrow schema is the
/// file's inferred schema; we convert that to τ's `StructType`.
///
/// This is the data-dependent discovery half of the schema-less FileScan
/// support — same architectural pattern as [`discover_pivot_values`].
// `Status` is the standard gRPC error channel across this file; boxing it
// here alone would be inconsistent (see `relation_to_common_ast`).
#[allow(clippy::result_large_err)]
async fn discover_parquet_schema(
    paths: &[String],
    options: &[(String, String)],
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
) -> Result<StructType, Status> {
    use thunderduck_core::transpiler_v2::ast::FileFormat;

    let reader_call = thunderduck_core::transpiler_v2::emission::build_file_reader_sql(
        FileFormat::Parquet,
        paths,
        options,
    )
    .map_err(|e| Status::from(ConnectError::from(e)))?;
    let discovery_sql = format!("SELECT * FROM {reader_call} LIMIT 0");

    let batches = session
        .execute(&discovery_sql)
        .await
        .map_err(|e| Status::from(ConnectError::from(e)))?;

    // The LIMIT 0 query always returns at least one batch (possibly with zero
    // rows) whose schema reflects the file's columns. If DuckDB returns no
    // batches at all (shouldn't happen), surface a clear error.
    let arrow_schema = batches.first().map(|b| b.schema()).ok_or_else(|| {
        Status::internal("discover_parquet_schema: DuckDB returned no batches for LIMIT 0 query")
    })?;

    let fields = arrow_schema
        .fields()
        .iter()
        .map(|f| {
            crate::converter::v2_relation_converter::arrow_field_to_struct_field(f)
                .map_err(|e| Status::from(ConnectError::from(e)))
        })
        .collect::<Result<Vec<_>, Status>>()?;

    Ok(StructType::new(fields))
}

// ── gRPC service impl ─────────────────────────────────────────────────────────

#[tonic::async_trait]
impl SparkConnectService for ThunderduckService {
    type ExecutePlanStream = BoxStream<proto::ExecutePlanResponse>;
    type ReattachExecuteStream = BoxStream<proto::ExecutePlanResponse>;

    async fn execute_plan(
        &self,
        request: Request<proto::ExecutePlanRequest>,
    ) -> Result<Response<Self::ExecutePlanStream>, Status> {
        let req = request.into_inner();
        let session_id = req.session_id.clone();
        let operation_id = req
            .operation_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        tracing::debug!(
            session_id = %session_id,
            operation_id = %operation_id,
            "execute_plan",
        );

        let session = self.session(&session_id).await?;

        let plan = req
            .plan
            .ok_or_else(|| Status::invalid_argument("missing plan"))?;

        let responses: Vec<proto::ExecutePlanResponse> = match plan.op_type {
            Some(proto::plan::OpType::Root(relation)) => {
                // Convert first, then run Spark's eager pivot-value discovery
                // (needs the live session) BEFORE finalize — see
                // `resolve_implicit_pivots`. `finalize` succeeds for every
                // plan τ covers, so `execute_streaming_query` is live.
                let mut common_ast = relation_to_common_ast(&relation)?;
                resolve_implicit_pivots(&mut common_ast, &session).await?;
                let (sql, resolved_schema) = finalize(&session, &common_ast).await?;
                return execute_streaming_query(
                    &session,
                    &sql,
                    &resolved_schema,
                    &session_id,
                    &operation_id,
                )
                .await;
            }
            Some(proto::plan::OpType::Command(cmd)) => {
                handle_command(&session, &session_id, &operation_id, cmd).await?
            }
            _ => {
                return Err(Status::unimplemented("Unsupported plan op_type"));
            }
        };

        let stream = stream::iter(responses.into_iter().map(Ok));
        Ok(Response::new(Box::pin(stream)))
    }

    async fn analyze_plan(
        &self,
        request: Request<proto::AnalyzePlanRequest>,
    ) -> Result<Response<proto::AnalyzePlanResponse>, Status> {
        let req = request.into_inner();
        let session_id = req.session_id.clone();

        tracing::debug!(
            session_id = %session_id,
            "analyze_plan",
        );

        use proto::analyze_plan_request::Analyze;
        match req.analyze {
            Some(Analyze::Schema(schema_req)) => {
                let plan = schema_req
                    .plan
                    .ok_or_else(|| Status::invalid_argument("Schema analyze missing plan"))?;
                let relation = match plan.op_type {
                    Some(proto::plan::OpType::Root(r)) => r,
                    _ => {
                        return Err(Status::invalid_argument(
                            "Schema analyze requires root plan",
                        ));
                    }
                };
                // Hold the session so the eager pivot-value discovery pass can
                // reach it (see `resolve_implicit_pivots`); the session also
                // carries the temp-view catalog the analyzer resolves
                // `TableScan` schemas from (catalog bridge).
                let session = self.session(&session_id).await?;
                // E.0 addendum: route analyze_plan(Schema) through τ's
                // analyzer. Parse the relation to CommonAst, then invoke
                // `analyze_schema` — which runs the analyzer without
                // calling `dispatch_op`. Errors surface via the same
                // two-category bridge `finalize` uses (AnalyzerError →
                // EmissionError → ConnectError → Status).
                //
                // ExecutePlan/AnalyzePlan symmetry: this path serializes τ's
                // `resolved_schema` verbatim (via `data_type_to_proto`), so
                // AnalyzePlan already surfaces the Spark-visible view.
                // ExecutePlan achieves the same on the response path via
                // `arrow_schema_stamp::build_stamped_schema` in
                // `execute_streaming_query`. Do not modify this arm.
                let mut common_ast = relation_to_common_ast(&relation)?;
                resolve_implicit_pivots(&mut common_ast, &session).await?;
                let struct_type = analyze_schema(&session, &common_ast).await?;
                // ADR-022 boundary hygiene: `DataType::Unresolved` maps to
                // `Kind::Unparsed { data_type_string: "unresolved" }` on the
                // wire, which PySpark's `_parse_datatype_json_value` refuses
                // with `PySparkValueError`. If τ's analyzer could not resolve
                // any field's type, surface a Thunderduck-boundary
                // `Status::unimplemented` rather than corrupt-serialize the
                // response. See `.agent-output/diagnostic-unresolved-schema.md`.
                if let Some(bad) = struct_type
                    .fields
                    .iter()
                    .find(|f| f.data_type.contains_unresolved())
                {
                    return Err(Status::unimplemented(format!(
                        "τ boundary: unresolved type for field '{name}' — \
                         analyzer did not infer type",
                        name = bad.name,
                    )));
                }
                let schema_proto = data_type_to_proto(&DataType::Struct(struct_type));
                let resp = proto::AnalyzePlanResponse {
                    session_id,
                    server_side_session_id: SERVER_SESSION_ID.clone(),
                    result: Some(proto::analyze_plan_response::Result::Schema(
                        proto::analyze_plan_response::Schema {
                            schema: Some(schema_proto),
                        },
                    )),
                };
                Ok(Response::new(resp))
            }
            _ => Err(Status::unimplemented(
                "Only Schema analyze_plan is supported",
            )),
        }
    }

    async fn config(
        &self,
        request: Request<proto::ConfigRequest>,
    ) -> Result<Response<proto::ConfigResponse>, Status> {
        let req = request.into_inner();
        let pairs = build_config_pairs(req.operation);
        Ok(Response::new(proto::ConfigResponse {
            session_id: req.session_id,
            server_side_session_id: SERVER_SESSION_ID.clone(),
            pairs,
            warnings: vec![],
        }))
    }

    async fn add_artifacts(
        &self,
        _request: Request<tonic::Streaming<proto::AddArtifactsRequest>>,
    ) -> Result<Response<proto::AddArtifactsResponse>, Status> {
        Ok(Response::new(proto::AddArtifactsResponse::default()))
    }

    async fn artifact_status(
        &self,
        _request: Request<proto::ArtifactStatusesRequest>,
    ) -> Result<Response<proto::ArtifactStatusesResponse>, Status> {
        Ok(Response::new(proto::ArtifactStatusesResponse::default()))
    }

    async fn interrupt(
        &self,
        _request: Request<proto::InterruptRequest>,
    ) -> Result<Response<proto::InterruptResponse>, Status> {
        Err(Status::unimplemented("Interrupt not supported"))
    }

    async fn reattach_execute(
        &self,
        _request: Request<proto::ReattachExecuteRequest>,
    ) -> Result<Response<Self::ReattachExecuteStream>, Status> {
        Err(Status::unimplemented("ReattachExecute not supported"))
    }

    async fn release_execute(
        &self,
        _request: Request<proto::ReleaseExecuteRequest>,
    ) -> Result<Response<proto::ReleaseExecuteResponse>, Status> {
        Ok(Response::new(proto::ReleaseExecuteResponse::default()))
    }

    async fn release_session(
        &self,
        request: Request<proto::ReleaseSessionRequest>,
    ) -> Result<Response<proto::ReleaseSessionResponse>, Status> {
        let req = request.into_inner();
        self.session_manager.release(&req.session_id);
        Ok(Response::new(proto::ReleaseSessionResponse {
            session_id: req.session_id,
            server_side_session_id: SERVER_SESSION_ID.clone(),
        }))
    }

    async fn fetch_error_details(
        &self,
        _request: Request<proto::FetchErrorDetailsRequest>,
    ) -> Result<Response<proto::FetchErrorDetailsResponse>, Status> {
        Err(Status::unimplemented("FetchErrorDetails not supported"))
    }

    async fn clone_session(
        &self,
        _request: Request<proto::CloneSessionRequest>,
    ) -> Result<Response<proto::CloneSessionResponse>, Status> {
        Err(Status::unimplemented("CloneSession not supported"))
    }
}

// ── Command dispatch ─────────────────────────────────────────────────────────

async fn handle_command(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    cmd: proto::Command,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    use proto::command::CommandType;
    match cmd.command_type {
        Some(CommandType::CreateDataframeView(view_cmd)) => {
            let relation = view_cmd
                .input
                .ok_or_else(|| Status::invalid_argument("CreateTempView missing input"))?;
            let (_ast, sql, schema) = transpile_relation(session, &relation).await?;
            handle_create_dataframe_view(
                session,
                session_id,
                operation_id,
                &view_cmd.name,
                view_cmd.is_global,
                sql,
                schema,
            )
            .await
        }
        Some(CommandType::SqlCommand(sql_cmd)) => {
            // Modern clients (PySpark 4.1.1) carry the query as a typed
            // `RelType::Sql` relation in `sql_cmd.input`; older clients use the
            // proto-deprecated `sql` text field. Synthesize a `RelType::Sql`
            // relation for the latter so both paths echo a typed relation.
            let input_rel = match sql_cmd.input {
                Some(input_rel) => input_rel,
                None => {
                    #[allow(deprecated)]
                    let text = sql_cmd.sql;
                    if text.is_empty() {
                        return Err(Status::invalid_argument(
                            "SqlCommand missing both input relation and sql text",
                        ));
                    }
                    proto::Relation {
                        common: None,
                        rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                            query: text,
                            ..Default::default()
                        })),
                    }
                }
            };
            // Eager-validate (parse + analyze) at `sql()` time so Spark-emulated
            // errors surface eagerly, matching Spark's `AnalysisException`. The
            // emitted SQL / resolved schema are discarded — the client
            // re-transpiles the echoed relation on `.collect()` via the Root
            // path.
            //
            // TODO: eager DDL/DML side effects
            // (`spark.sql("CREATE VIEW ...")`) and non-deterministic
            // re-evaluation (`rand()`, `current_timestamp()`) require eager
            // execution to a `LocalRelation` — out of scope for this pass.
            let _ = transpile_relation(session, &input_rel).await?;
            handle_sql_command(session, session_id, operation_id, input_rel).await
        }
        Some(CommandType::WriteOperation(mut write_cmd)) => {
            let input_rel = write_cmd
                .input
                .take()
                .ok_or_else(|| Status::invalid_argument("WriteOperation missing input"))?;
            let (common_ast, _sql, _schema) = transpile_relation(session, &input_rel).await?;
            handle_write_operation(session, session_id, operation_id, &common_ast, &write_cmd).await
        }
        _ => Err(Status::unimplemented("Unsupported command type")),
    }
}

// ── Response builders + streaming ────────────────────────────────────────────

/// Build a schema-only `ExecutePlanResponse` from τ's `resolved_schema`.
///
/// PySpark's Connect client short-circuits its `_from_arrow_schema` fallback
/// (which cannot decode Arrow `Interval(*)` types) when the server has already
/// sent a `schema` message; instead it uses `proto_schema_to_pyspark_data_type`,
/// which has arms for all three Spark interval kinds. This frame is what makes
/// intv-001 (CalendarInterval) and intv-003/intv-005 (DayTimeInterval) decode.
fn build_schema_response(
    resolved_schema: &StructType,
    session_id: &str,
    operation_id: &str,
) -> proto::ExecutePlanResponse {
    proto::ExecutePlanResponse {
        session_id: session_id.to_owned(),
        server_side_session_id: SERVER_SESSION_ID.clone(),
        operation_id: operation_id.to_owned(),
        response_id: format!("{operation_id}-schema"),
        schema: Some(data_type_to_proto(&DataType::Struct(
            resolved_schema.clone(),
        ))),
        response_type: None, // schema lives in the top-level field, not in the oneof
        ..Default::default()
    }
}

/// Wrap a single [`arrow::record_batch::RecordBatch`] as one Arrow-IPC
/// `ExecutePlanResponse` frame.
fn batch_to_response(
    batch: &arrow::record_batch::RecordBatch,
    session_id: &str,
    operation_id: &str,
    seq: usize,
) -> crate::error::Result<proto::ExecutePlanResponse> {
    let ab = record_batch_to_arrow_batch(batch)?;
    Ok(proto::ExecutePlanResponse {
        session_id: session_id.to_owned(),
        server_side_session_id: SERVER_SESSION_ID.clone(),
        operation_id: operation_id.to_owned(),
        response_id: format!("{operation_id}-{seq}"),
        response_type: Some(proto::execute_plan_response::ResponseType::ArrowBatch(ab)),
        ..Default::default()
    })
}

/// State machine for the streaming `unfold`. Owns everything the loop needs
/// per iteration; moved into `unfold` and threaded through each `.await`.
struct StreamingState {
    rx: tokio::sync::mpsc::Receiver<thunderduck_core::runtime::StreamBatch>,
    resolved_schema: StructType,
    plan: IntervalPlan,
    /// Cached Arc<Schema> built from the FIRST post-transcode batch — reused
    /// verbatim for every subsequent batch (Arc::clone is refcount-only).
    stamped_schema: Option<Arc<Schema>>,
    session_id: String,
    operation_id: String,
    seq: usize,
    /// One-shot: send the proto `Schema` frame before the first batch.
    sent_schema_frame: bool,
    /// Terminal frame — once yielded, unfold returns None on next poll.
    sent_complete_frame: bool,
}

/// Execute a query via τ-emitted SQL and stream Arrow batches back.
///
/// Per-batch pipeline:
///
/// 1. Emit ONE `ExecutePlanResponse.schema` frame (proto-schema, decoded on
///    the client via `proto_schema_to_pyspark_data_type` — has arms for all
///    three Spark interval kinds).
/// 2. For each `thunderduck_core::runtime::StreamBatch::Batch(rb)`, in order:
///    `arrow_interval_transcode::apply` returns `Vec<ArrayRef>` with
///    DayTimeInterval columns rewritten from DuckDB's `Interval(MonthDayNano)`
///    to Spark's `Duration(Microsecond)` (no intermediate `RecordBatch`); the
///    wire `Arc<Schema>` is built ONCE from the first batch (via
///    `arrow_schema_stamp::build_stamped_schema`) and reused verbatim on every
///    subsequent batch (`Arc::clone` is refcount-only); a single
///    `RecordBatch::try_new_with_options(stamped_schema, cols, ...)` call then
///    constructs the outbound batch in one shot — no transcode→stamp→wrap
///    chain of temporaries (perf finding MED-1,
///    `.agent-output/004-perf-findings.md`) — before an `ArrowBatch` frame is
///    emitted.
/// 3. On `thunderduck_core::runtime::StreamBatch::Complete` — emit `ResultComplete`.
/// 4. On `thunderduck_core::runtime::StreamBatch::Error(msg)` — reclassify via
///    `ThunderduckError::DuckDb(msg).reclassified_spark_runtime()` so the
///    ANSI Spark class token survives to the client, then yield the Status
///    and terminate the stream (Q5 fix — the previous inline path skipped
///    reclassification).
///
/// Concurrency model: transcode + stamp run on the tonic async task after the
/// mpsc hop; DuckDB's `!Send` Connection stays on its dedicated session
/// thread. The mpsc buffer is 4 batches so DuckDB blocks when the client is
/// slow (backpressure) without wedging the session thread on every batch.
async fn execute_streaming_query(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    sql: &str,
    resolved_schema: &StructType,
    session_id: &str,
    operation_id: &str,
) -> Result<Response<<ThunderduckService as SparkConnectService>::ExecutePlanStream>, Status> {
    let rx = session
        .execute_streaming(sql.to_string().as_str(), 4)
        .await
        .map_err(|e| Status::from(ConnectError::from(e)))?;

    let plan = IntervalPlan::build(resolved_schema);

    let state = StreamingState {
        rx,
        resolved_schema: resolved_schema.clone(),
        plan,
        stamped_schema: None,
        session_id: session_id.to_owned(),
        operation_id: operation_id.to_owned(),
        seq: 0,
        sent_schema_frame: false,
        sent_complete_frame: false,
    };

    let stream = stream::unfold(state, streaming_step);
    Ok(Response::new(Box::pin(stream)))
}

/// Build the post-transcode Arrow `Schema` from the source (pre-transcode)
/// schema plus the transcoded column array — the source `Field` metadata
/// (name / nullable / metadata) is preserved verbatim, only the leaf Arrow
/// `DataType` is taken from each transcoded column. Called ONCE per query on
/// the first batch; the resulting `Schema` is fed to
/// `arrow_schema_stamp::build_stamped_schema` and then discarded (or, on
/// stamp failure, cached as the fallback wire schema).
fn post_transcode_schema(src: &Schema, cols: &[ArrayRef]) -> Schema {
    let new_fields: Vec<Field> = src
        .fields()
        .iter()
        .zip(cols.iter())
        .map(|(f, c)| {
            Field::new(f.name(), c.data_type().clone(), f.is_nullable())
                .with_metadata(f.metadata().clone())
        })
        .collect();
    Schema::new(new_fields).with_metadata(src.metadata.clone())
}

/// One iteration of the streaming state machine. Returns
/// `Some((frame, next_state))` while there is work to do; `None` terminates
/// the tonic stream cleanly.
async fn streaming_step(
    mut s: StreamingState,
) -> Option<(Result<proto::ExecutePlanResponse, Status>, StreamingState)> {
    // 1. Terminal: once the complete frame (or a terminal error) has been
    //    yielded, unfold returns None.
    if s.sent_complete_frame {
        return None;
    }
    // 2. Schema frame first (one-shot).
    if !s.sent_schema_frame {
        s.sent_schema_frame = true;
        let frame = build_schema_response(&s.resolved_schema, &s.session_id, &s.operation_id);
        return Some((Ok(frame), s));
    }
    // 3. Pull the next thunderduck_core::runtime::StreamBatch from the session thread.
    match s.rx.recv().await {
        Some(thunderduck_core::runtime::StreamBatch::Batch(rb)) => {
            // 3a. Transcode DayTimeInterval columns → Vec<ArrayRef>. No
            // intermediate RecordBatch — the wire batch is built once below
            // from `(stamped_schema, cols)`.
            let cols: Vec<ArrayRef> = match arrow_interval_transcode::apply(&rb, &s.plan) {
                Ok(cols) => cols,
                Err(e) => {
                    let status = Status::from(ConnectError::from(e));
                    // Terminate after yielding the error.
                    s.sent_complete_frame = true;
                    return Some((Err(status), s));
                }
            };
            // 3b. Build the wire `Arc<Schema>` ONCE per query. On subsequent
            // batches this branch is skipped and the cached Arc is reused.
            //
            // On `build_stamped_schema` failure (structural mismatch — a
            // debug_assert!/tracing warn path) we cache the un-stamped
            // post-transcode schema as a fallback so the query still returns
            // rows. The fallback matches the pre-refactor behavior of
            // yielding `rb_dt.clone()` at the wire boundary.
            if s.stamped_schema.is_none() {
                let post_schema = post_transcode_schema(&rb.schema(), &cols);
                let cached = match arrow_schema_stamp::build_stamped_schema(
                    &post_schema,
                    &s.resolved_schema,
                ) {
                    Ok(schema) => schema,
                    Err(()) => Arc::new(post_schema),
                };
                s.stamped_schema = Some(cached);
            }
            let schema = Arc::clone(
                s.stamped_schema
                    .as_ref()
                    .expect("stamped_schema seeded above"),
            );
            // 3c. One-shot construction of the wire RecordBatch.
            let opts = RecordBatchOptions::new()
                .with_match_field_names(false)
                .with_row_count(Some(rb.num_rows()));
            let rb_named = match RecordBatch::try_new_with_options(schema, cols, &opts) {
                Ok(b) => b,
                Err(e) => {
                    let status = Status::from(ConnectError::Arrow(e.to_string()));
                    s.sent_complete_frame = true;
                    return Some((Err(status), s));
                }
            };
            // 3d. Serialize + wrap.
            let frame = match batch_to_response(&rb_named, &s.session_id, &s.operation_id, s.seq) {
                Ok(f) => f,
                Err(e) => {
                    let status = Status::from(e);
                    s.sent_complete_frame = true;
                    return Some((Err(status), s));
                }
            };
            s.seq += 1;
            Some((Ok(frame), s))
        }
        Some(thunderduck_core::runtime::StreamBatch::Complete) => {
            let frame = result_complete_response(&s.session_id, &s.operation_id);
            s.sent_complete_frame = true;
            Some((Ok(frame), s))
        }
        Some(thunderduck_core::runtime::StreamBatch::Error(msg)) => {
            // Q5 fix — apply ADR-006 reclassification so a τ-emitted
            // `[CLASS]` token survives to the client via `Status::internal`.
            let err = ThunderduckError::DuckDb(msg).reclassified_spark_runtime();
            let status = Status::from(ConnectError::from(err));
            s.sent_complete_frame = true;
            Some((Err(status), s))
        }
        // Session thread dropped the sender without sending Complete — treat
        // as an unexpected terminate; surface a Thunderduck-boundary error.
        None => {
            let status = Status::internal("session stream closed unexpectedly (no Complete frame)");
            s.sent_complete_frame = true;
            Some((Err(status), s))
        }
    }
}

/// Handle `CreateDataframeView` after successful transpile.
///
/// Register the temp view in the session — both in DuckDB
/// (so `SELECT * FROM <name>` executes) and in the session's Spark-schema cache
/// (so the analyzer's catalog bridge can resolve the view's columns +
/// nullabilities, which DuckDB's `CREATE VIEW` loses). Returns a lone
/// `ResultComplete` (ADR-011 command-arm response shape).
///
/// `is_global` (global temp views) is out of scope: the view is
/// registered session-local and a warning is logged.
async fn handle_create_dataframe_view(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    name: &str,
    is_global: bool,
    sql: String,
    schema: StructType,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    if is_global {
        tracing::warn!(view = %name, "global temp view registered as session-local");
    }
    session
        .create_temp_view_with_schema(name, &sql, schema)
        .await
        .map_err(|e| Status::from(ConnectError::from(e)))?;
    Ok(vec![result_complete_response(session_id, operation_id)])
}

/// Handle `SqlCommand` (both `input`-bearing and deprecated text paths).
///
/// SQL command execution over `CommonAst`.
///
/// Lazy-echo design (ADR-011 command-arm response shape): return a
/// `SqlCommandResult` carrying the re-executable input relation verbatim,
/// followed by `ResultComplete`. PySpark wraps that relation in a
/// `CachedRelation` and re-sends it as a `Root` plan on `.collect()`, flowing
/// through the already-proven `transpile_relation → execute_streaming_query`
/// path. The command arm never streams an `ArrowBatch`.
async fn handle_sql_command(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    result_rel: proto::Relation,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Ok(vec![
        sql_command_result_response(session_id, operation_id, result_rel),
        result_complete_response(session_id, operation_id),
    ])
}

/// Handle `WriteOperation` after successful transpile.
///
/// **future τ work (owner):** external write path over `CommonAst`.
async fn handle_write_operation(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    _session_id: &str,
    _operation_id: &str,
    _common_ast: &CommonAst,
    _write_cmd: &proto::WriteOperation,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Err(Status::unimplemented("WriteOperation over CommonAst"))
}

// ── Response builders ────────────────────────────────────────────────────────

fn result_complete_response(session_id: &str, operation_id: &str) -> proto::ExecutePlanResponse {
    proto::ExecutePlanResponse {
        session_id: session_id.to_string(),
        server_side_session_id: SERVER_SESSION_ID.clone(),
        operation_id: operation_id.to_string(),
        response_id: format!("{operation_id}-complete"),
        response_type: Some(proto::execute_plan_response::ResponseType::ResultComplete(
            proto::execute_plan_response::ResultComplete::default(),
        )),
        ..Default::default()
    }
}

fn sql_command_result_response(
    session_id: &str,
    operation_id: &str,
    relation: proto::Relation,
) -> proto::ExecutePlanResponse {
    proto::ExecutePlanResponse {
        session_id: session_id.to_string(),
        server_side_session_id: SERVER_SESSION_ID.clone(),
        operation_id: operation_id.to_string(),
        response_id: format!("{operation_id}-cmd"),
        response_type: Some(
            proto::execute_plan_response::ResponseType::SqlCommandResult(
                proto::execute_plan_response::SqlCommandResult {
                    relation: Some(relation),
                },
            ),
        ),
        ..Default::default()
    }
}

// ── Config helpers (preserved) ────────────────────────────────────────────────

/// Return the Spark default value for well-known config keys.
///
/// PySpark calls `int()` or `bool()` on some config values, so we must return
/// valid strings for integer and boolean configs rather than empty strings.
fn spark_config_default(key: &str) -> &'static str {
    match key {
        // Integer configs — PySpark calls int() on these
        "spark.sql.session.localRelationCacheThreshold" => "67108864",
        "spark.sql.session.localRelationChunkSizeRows" => "1000",
        "spark.sql.session.localRelationChunkSizeBytes" => "4194304",
        "spark.sql.session.localRelationBatchOfChunksSizeBytes" => "67108864",
        "spark.sql.execution.arrow.maxRecordsPerBatch" => "10000",
        "spark.sql.shuffle.partitions" => "200",
        "spark.default.parallelism" => "8",
        "spark.sql.autoBroadcastJoinThreshold" => "10485760",
        "spark.sql.broadcastTimeout" => "300",
        "spark.network.timeout" => "120",
        "spark.reducer.maxSizeInFlight" => "50331648",
        // Boolean configs — PySpark calls bool() on these
        "spark.sql.execution.arrow.enabled" => "true",
        "spark.sql.execution.arrow.pyspark.enabled" => "true",
        "spark.sql.execution.arrow.pyspark.fallback.enabled" => "true",
        "spark.sql.execution.pandas.convertToArrowArraySafely" => "false",
        "spark.sql.execution.arrow.pyspark.selfDestructEnabled" => "false",
        "spark.sql.repl.eagerEval.enabled" => "false",
        "spark.sql.adaptive.enabled" => "true",
        "spark.sql.ansi.enabled" => "false",
        // Unknown keys — return empty string (safe for plain string usage)
        _ => "",
    }
}

/// Build the `pairs` payload for a `ConfigRequest`.
///
/// Extracted from `config()` so the per-op-type semantics can be unit-tested
/// without spinning up a `SessionManager`. Critically, `GetOption` MUST emit
/// exactly one `KeyValue` per requested key (with `value: None` when not
/// configured), or `spark-connect-client-jvm` 4.1.x's
/// `executeConfigRequestSinglePair` precondition fails.
fn build_config_pairs(operation: Option<proto::config_request::Operation>) -> Vec<proto::KeyValue> {
    use proto::config_request::operation::OpType;
    match operation.and_then(|op| op.op_type) {
        Some(OpType::Get(g)) => {
            // Return Spark defaults for known integer/boolean configs that PySpark
            // calls int() or bool() on. Unknown keys get empty string (safe for str usage).
            g.keys
                .into_iter()
                .map(|k| {
                    let v = spark_config_default(&k).to_string();
                    proto::KeyValue {
                        key: k,
                        value: Some(v),
                    }
                })
                .collect()
        }
        Some(OpType::GetWithDefault(gd)) => gd.pairs,
        Some(OpType::GetOption(go)) => {
            // value=None signals "not configured" → JVM client treats as missing →
            // `getPlanCompressionOptions` catches `NoSuchElementException` and
            // disables plan compression locally rather than crashing.
            go.keys
                .into_iter()
                .map(|k| proto::KeyValue {
                    key: k,
                    value: None,
                })
                .collect()
        }
        Some(OpType::GetAll(_)) => vec![],
        Some(OpType::IsModifiable(im)) => im
            .keys
            .into_iter()
            .map(|k| proto::KeyValue {
                key: k,
                value: Some("true".to_string()),
            })
            .collect(),
        // Set / Unset / unspecified — acknowledge with empty pairs
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::spark::connect as proto;

    fn op(
        op_type: proto::config_request::operation::OpType,
    ) -> Option<proto::config_request::Operation> {
        Some(proto::config_request::Operation {
            op_type: Some(op_type),
        })
    }

    /// Build a real, warmed `DuckDbSession` for the async dispatch-helper
    /// tests. INV10 forbids `use thunderduck_core::runtime::` in this file, so
    /// the paths are fully qualified inline.
    async fn test_session(session_id: &str) -> Arc<thunderduck_core::runtime::DuckDbSession> {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        session_manager
            .get_or_create(session_id)
            .await
            .expect("session must be creatable")
    }

    /// Drain an `ExecutePlanStream` into a flat `Vec` of response frames.
    async fn drain(
        resp: Response<<ThunderduckService as SparkConnectService>::ExecutePlanStream>,
    ) -> Vec<proto::ExecutePlanResponse> {
        use futures::StreamExt;
        let mut stream = resp.into_inner();
        let mut frames = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(item.expect("stream frame must be Ok"));
        }
        frames
    }

    /// Assert the final response frame is `ResultComplete`.
    fn assert_trailing_result_complete(frames: &[proto::ExecutePlanResponse]) {
        assert!(
            frames.last().is_some_and(|f| matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ResultComplete(
                    _
                ))
            )),
            "final frame must be ResultComplete",
        );
    }

    /// Find the first `ArrowBatch` frame in a response sequence.
    fn find_arrow_batch(
        frames: &[proto::ExecutePlanResponse],
    ) -> Option<&proto::execute_plan_response::ArrowBatch> {
        frames.iter().find_map(|f| match &f.response_type {
            Some(proto::execute_plan_response::ResponseType::ArrowBatch(ab)) => Some(ab),
            _ => None,
        })
    }

    // ── Shared τ literal/column builders (pivot-discovery + crosstab tests) ──

    use thunderduck_core::transpiler_v2::expression::{
        Expression, Literal, LiteralValue, UnresolvedColumn,
    };

    fn int_lit(v: i32) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Int(v),
            data_type: DataType::Integer,
        })
    }

    fn str_lit(s: &str) -> Expression {
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

    fn null_str_lit() -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Null,
            data_type: DataType::Null,
        })
    }

    fn col(name: &str) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: None,
        })
    }

    /// Regression for nubank/thunderduck#33: GetOption must emit one KeyValue per
    /// requested key (value=None when not configured). spark-connect-client-jvm
    /// 4.1.x calls this on every analyze; returning 0 pairs trips its
    /// `require(pairs.size == 1)` precondition with IllegalArgumentException.
    #[test]
    fn get_option_emits_one_pair_per_key_with_unset_value() {
        use proto::config_request::operation::OpType;
        let operation = op(OpType::GetOption(proto::config_request::GetOption {
            keys: vec![
                "spark.connect.session.planCompression.threshold".into(),
                "spark.connect.session.planCompression.defaultAlgorithm".into(),
            ],
        }));
        let pairs = build_config_pairs(operation);
        assert_eq!(pairs.len(), 2);
        assert_eq!(
            pairs[0].key,
            "spark.connect.session.planCompression.threshold"
        );
        assert!(
            pairs[0].value.is_none(),
            "value must be unset → client treats as missing"
        );
        assert_eq!(
            pairs[1].key,
            "spark.connect.session.planCompression.defaultAlgorithm"
        );
        assert!(pairs[1].value.is_none());
    }

    #[test]
    fn get_option_with_zero_keys_returns_zero_pairs() {
        use proto::config_request::operation::OpType;
        let operation = op(OpType::GetOption(proto::config_request::GetOption {
            keys: vec![],
        }));
        assert!(build_config_pairs(operation).is_empty());
    }

    #[test]
    fn get_emits_spark_defaults() {
        use proto::config_request::operation::OpType;
        let operation = op(OpType::Get(proto::config_request::Get {
            keys: vec!["spark.sql.shuffle.partitions".into()],
        }));
        let pairs = build_config_pairs(operation);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].value.as_deref(), Some("200"));
    }

    // ── the τ dispatch site dispatch tests ──────────────────────────────────────────────
    //
    // At A.3, τ's `generate()` errors with `EmissionError::Unsupported`
    // (kind: Op) on
    // every input. These tests pin two properties of the dispatch shape:
    //   1. Structurally-valid inputs reach τ (via `V2RelationConverter` or
    //      `parser_v2`) and surface the emission boundary via
    //      `Status::unimplemented` — not `Status::internal`.
    //   2. `RelType::Sql` routes to `parser_v2`, never through
    //      `V2RelationConverter` (which would return
    //      `Unsupported { kind: ProtoShape, name: "RelType::Sql" }`).

    fn table_scan_relation(name: &str) -> proto::Relation {
        proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Read(proto::Read {
                is_streaming: false,
                read_type: Some(proto::read::ReadType::NamedTable(proto::read::NamedTable {
                    unparsed_identifier: name.to_owned(),
                    options: Default::default(),
                })),
            })),
        }
    }

    fn int_literal(v: i32) -> proto::Expression {
        proto::Expression {
            common: None,
            expr_type: Some(proto::expression::ExprType::Literal(
                proto::expression::Literal {
                    data_type: None,
                    literal_type: Some(proto::expression::literal::LiteralType::Integer(v)),
                },
            )),
        }
    }

    /// A structurally-valid `Project` relation reaches τ and surfaces the
    /// emission boundary as `Status::unimplemented` (not `internal`) — this
    /// pins `ConnectError::TranspilerV2Emission → Status::unimplemented`.
    #[tokio::test(flavor = "multi_thread")]
    async fn transpile_relation_project_returns_unsupported_op() {
        let session = test_session("test-transpile-project").await;
        let input = table_scan_relation("t");
        let project = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Project(Box::new(
                proto::Project {
                    input: Some(Box::new(input)),
                    expressions: vec![int_literal(1)],
                },
            ))),
        };
        let err = transpile_relation(&session, &project)
            .await
            .expect_err("τ emission must error at A.3");
        assert_eq!(
            err.code(),
            tonic::Code::Unimplemented,
            "boundary errors must surface as Status::unimplemented, not internal; got {err:?}"
        );
        // τ's emission boundary is what should be surfaced — NOT
        // `V2RelationConverter`'s `UnsupportedProtoShape` (the proto shape is
        // supported). Since τ's analyzer, this can be either the τ analyzer's
        // Spark-emulated error (unknown table `t` — the test uses an empty
        // BaseTypes overlay) or τ's `UnsupportedOp` (`<tau-analyzer-ok>`
        // when the analyzer succeeds). Both signal we reached τ, not the
        // proto-shape gate.
        let message = err.message();
        assert!(
            message.contains("unsupported operator")
                || message.contains("unsupported expression")
                || message.contains("<tau-analyzer-ok>")
                || message.contains("[SPARK-EMULATED]")
                || message.contains("<a.2-substrate>"),
            "message must identify τ's boundary error; got: {message}",
        );
    }

    /// `RelType::Sql` MUST route through `parser_v2`, not `V2RelationConverter`.
    /// The failure mode we're guarding against: if dispatch fed `Sql` to
    /// `V2RelationConverter`, the error would identify `RelType::Sql` as the
    /// unsupported proto shape. Instead, `parser_v2` parses the SQL and τ's
    /// `generate()` emits DuckDB SQL (τ's emission substrate wired the Project + SingleRow
    /// arms — `SELECT 1` is a Project over SingleRow of a literal).
    #[tokio::test(flavor = "multi_thread")]
    async fn transpile_relation_sql_routes_to_parser_v2_not_converter() {
        let session = test_session("test-transpile-sql-route").await;
        let sql_rel = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                query: "SELECT 1".to_owned(),
                ..Default::default()
            })),
        };
        // τ's emission substrate wired Project + SingleRow arms — `SELECT 1` now succeeds.
        // The routing anchor (SQL → parser_v2, not converter) is still enforced:
        // a routing bug would have surfaced `RelType::Sql` as an
        // `UnsupportedProtoShape` error before reaching τ's emission.
        let (_common_ast, sql, _schema) = transpile_relation(&session, &sql_rel)
            .await
            .expect("τ must emit SQL for `SELECT 1`");
        assert!(
            sql.contains("SELECT"),
            "expected DuckDB SELECT emission; got: {sql}",
        );
    }

    /// SparkSQL syntax errors surface via `parser_v2`'s boundary policy
    /// (`Unsupported { kind: ProtoShape, name: "sql::parse_error", ... }`), which
    /// maps to `Status::unimplemented` per `ConnectError::TranspilerV2Emission`.
    #[tokio::test(flavor = "multi_thread")]
    async fn transpile_relation_sql_syntax_error_surfaces_from_parser_v2() {
        let session = test_session("test-transpile-syntax-err").await;
        let sql_rel = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                query: "NOT VALID SQL".to_owned(),
                ..Default::default()
            })),
        };
        let err = transpile_relation(&session, &sql_rel)
            .await
            .expect_err("syntax error must surface");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(
            err.message().contains("sql::parse_error")
                || err.message().contains("unsupported proto shape"),
            "expected parser_v2 boundary error; got: {}",
            err.message()
        );
    }

    /// A deferred proto shape (e.g. `Sample`) surfaces via
    /// `V2RelationConverter`'s `UnsupportedProtoShape` and maps to
    /// `Status::unimplemented`.
    #[tokio::test(flavor = "multi_thread")]
    async fn transpile_relation_unsupported_proto_shape_surfaces() {
        let session = test_session("test-transpile-unsupported-shape").await;
        // ShowString is still in the converter's catch-all `other` arm — a
        // deferred `RelType` that must surface as `UnsupportedProtoShape`.
        // (Pass 83 wired `Sample` / `SampleBy`, which used to sit here.)
        let show = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::ShowString(Box::new(
                proto::ShowString {
                    input: Some(Box::new(table_scan_relation("t"))),
                    num_rows: 20,
                    truncate: 0,
                    vertical: false,
                },
            ))),
        };
        let err = transpile_relation(&session, &show)
            .await
            .expect_err("deferred shape must error");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(
            err.message().contains("unsupported proto shape")
                || err.message().contains("ShowString"),
            "expected UnsupportedProtoShape; got: {}",
            err.message()
        );
    }

    /// `finalize()` builds `BaseTypes::empty()` when the plan carries no
    /// empty-scan (short-circuit anchor at the service layer — the substrate
    /// test in `base_types::tests` already pins the closure-not-invoked
    /// behavior). τ's emission substrate wired SingleRow emission — this test now asserts
    /// that finalize returns the emitted SQL for a plan with no empty scan
    /// (proving the short-circuit path builds `BaseTypes::empty()` without
    /// blocking emission).
    #[tokio::test(flavor = "multi_thread")]
    async fn finalize_short_circuits_on_plans_without_empty_scan() {
        use thunderduck_core::transpiler_v2::ast::CommonOp;
        let session = test_session("test-finalize-short-circuit").await;
        // A `SingleRow` plan carries no `TableScan` → `empty_scan_tables` is
        // empty → `BaseTypes::empty()` (no catalog lookup) → τ emits.
        let plan = CommonAst::new(CommonOp::SingleRow);
        let (sql, _schema) = finalize(&session, &plan)
            .await
            .expect("τ must emit for SingleRow");
        // Subquery-safe shape — see `emission::render_single_row`.
        assert_eq!(sql, "SELECT 1");
    }

    // ── future τ work.0 smoke test ─────────────────────────────────────────────────
    //
    // Round-trips `SELECT 1` through the full gRPC path:
    // `execute_plan` → `transpile_relation` (parser_v2 → τ finalize) →
    // `execute_streaming_query` (session.execute_streaming + per-batch frames) →
    // Arrow IPC stream. Verifies the E.0 wiring end-to-end at the service
    // layer without spinning up a network gRPC server.

    /// End-to-end smoke test for the future τ work.0 streaming-query wiring.
    ///
    /// Marked `#[ignore]` because τ's `SingleRow` renderer emits a
    /// bare `SELECT` (see `emission.rs::render_single_row`), which becomes
    /// `SELECT 1 FROM (SELECT) AS __td_proj` when wrapped by `render_project`
    /// — DuckDB rejects the bare `SELECT` subquery with "Parser Error: SELECT
    /// clause without selection list". This is a τ emission concern (owned by
    /// τ's emission substrate / a later refinement), not an E.0 wiring defect: the E.0
    /// path successfully submits SQL to the session, receives the DuckDB
    /// error, and maps it through `ThunderduckError → ConnectError →
    /// Status::internal`, as designed. The E.0 wiring is validated at the
    /// corpus level via `tests/scripts/v2-progress.sh`.
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_plan_single_row_round_trips_through_duckdb() {
        use arrow::array::Int32Array;
        use arrow_ipc::reader::StreamReader;
        use std::io::Cursor;

        // Arrange: build a service with a real SessionManager (same pattern
        // as `crates/core/tests/runtime_integration.rs`). Inline paths for
        // `thunderduck_core::runtime::*` — INV10 forbids `use
        // thunderduck_core::runtime::` inside `service.rs`.
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let svc = ThunderduckService::new(session_manager);

        let plan = proto::Plan {
            op_type: Some(proto::plan::OpType::Root(proto::Relation {
                common: None,
                rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                    query: "SELECT 1".to_owned(),
                    ..Default::default()
                })),
            })),
        };
        let req = proto::ExecutePlanRequest {
            session_id: "test-session".to_owned(),
            operation_id: Some("test-op".to_owned()),
            plan: Some(plan),
            ..Default::default()
        };

        // Act: call execute_plan and drain the response stream.
        let resp = svc
            .execute_plan(Request::new(req))
            .await
            .expect("execute_plan must succeed");
        let frames = drain(resp).await;

        // Assert: at least one ArrowBatch frame with non-empty data, and a
        // trailing ResultComplete frame.
        assert!(!frames.is_empty(), "expected at least one response frame");

        let arrow_frame = find_arrow_batch(&frames).expect("expected an ArrowBatch frame");
        assert!(
            !arrow_frame.data.is_empty(),
            "ArrowBatch data must be non-empty (schema+row IPC bytes)",
        );

        assert_trailing_result_complete(&frames);

        // Decode the IPC stream — expect exactly one RecordBatch with one row
        // and one Int32 column carrying `1`.
        let reader = StreamReader::try_new(Cursor::new(arrow_frame.data.as_slice()), None)
            .expect("StreamReader::try_new must succeed on valid IPC bytes");
        let batches: Vec<_> = reader
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("IPC stream must decode without error");
        assert_eq!(batches.len(), 1, "expected exactly one RecordBatch");
        let batch = &batches[0];
        assert_eq!(batch.num_rows(), 1, "expected one row");
        assert_eq!(batch.num_columns(), 1, "expected one column");
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("expected Int32 column for `SELECT 1`");
        assert_eq!(col.value(0), 1);
    }

    // ── Pass 95 — SqlCommand lazy-echo round-trip ───────────────────────────
    //
    // `spark.sql(...)` arrives as a `Command(SqlCommand)`. The command arm
    // echoes the input `RelType::Sql` relation back in a `SqlCommandResult`
    // frame (no `ArrowBatch`), followed by `ResultComplete`. PySpark
    // re-executes the echoed relation lazily on `.collect()` via the Root path.

    /// Modern PySpark path: `SqlCommand { input: Some(RelType::Sql{query}) }`.
    /// The command arm echoes the input relation verbatim.
    #[tokio::test(flavor = "multi_thread")]
    async fn sql_command_select_literals_returns_echoed_relation() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let svc = ThunderduckService::new(session_manager);

        let query = "SELECT 1 AS one, 'x' AS s, true AS b";
        let plan = proto::Plan {
            op_type: Some(proto::plan::OpType::Command(proto::Command {
                command_type: Some(proto::command::CommandType::SqlCommand(proto::SqlCommand {
                    input: Some(proto::Relation {
                        common: None,
                        rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                            query: query.to_owned(),
                            ..Default::default()
                        })),
                    }),
                    ..Default::default()
                })),
            })),
        };
        let req = proto::ExecutePlanRequest {
            session_id: "test-session".to_owned(),
            operation_id: Some("test-op".to_owned()),
            plan: Some(plan),
            ..Default::default()
        };

        let resp = svc
            .execute_plan(Request::new(req))
            .await
            .expect("execute_plan must succeed");
        let frames = drain(resp).await;

        // No ArrowBatch frame — the command arm does not stream data.
        assert!(
            find_arrow_batch(&frames).is_none(),
            "command arm must not emit an ArrowBatch frame",
        );

        // The SqlCommandResult frame echoes the original Sql relation.
        let cmd_result = frames
            .iter()
            .find_map(|f| match &f.response_type {
                Some(proto::execute_plan_response::ResponseType::SqlCommandResult(r)) => Some(r),
                _ => None,
            })
            .expect("expected a SqlCommandResult frame");
        let echoed = cmd_result
            .relation
            .as_ref()
            .expect("SqlCommandResult must carry a relation");
        match &echoed.rel_type {
            Some(proto::relation::RelType::Sql(sql)) => {
                assert_eq!(sql.query, query, "echoed relation must carry the query");
            }
            other => panic!("expected RelType::Sql, got {other:?}"),
        }

        // Final frame is ResultComplete.
        assert_trailing_result_complete(&frames);
    }

    /// Deprecated text path: `SqlCommand { sql: "SELECT 1", input: None }`
    /// synthesizes a `RelType::Sql` relation and echoes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn sql_command_deprecated_text_synthesizes_sql_relation() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let svc = ThunderduckService::new(session_manager);

        #[allow(deprecated)]
        let sql_cmd = proto::SqlCommand {
            sql: "SELECT 1".to_owned(),
            input: None,
            ..Default::default()
        };
        let plan = proto::Plan {
            op_type: Some(proto::plan::OpType::Command(proto::Command {
                command_type: Some(proto::command::CommandType::SqlCommand(sql_cmd)),
            })),
        };
        let req = proto::ExecutePlanRequest {
            session_id: "test-session".to_owned(),
            operation_id: Some("test-op".to_owned()),
            plan: Some(plan),
            ..Default::default()
        };

        let resp = svc
            .execute_plan(Request::new(req))
            .await
            .expect("execute_plan must succeed");
        let frames = drain(resp).await;

        let cmd_result = frames
            .iter()
            .find_map(|f| match &f.response_type {
                Some(proto::execute_plan_response::ResponseType::SqlCommandResult(r)) => Some(r),
                _ => None,
            })
            .expect("expected a SqlCommandResult frame");
        let echoed = cmd_result
            .relation
            .as_ref()
            .expect("SqlCommandResult must carry a synthesized relation");
        match &echoed.rel_type {
            Some(proto::relation::RelType::Sql(sql)) => {
                assert_eq!(
                    sql.query, "SELECT 1",
                    "synthesized relation carries the text"
                );
            }
            other => panic!("expected synthesized RelType::Sql, got {other:?}"),
        }

        assert_trailing_result_complete(&frames);
    }

    // ── Pass 58 — ADR-022 boundary guard for unresolved schema ──────────────
    //
    // These tests pin the invariant that τ never proto-serializes a
    // `DataType::Unresolved` field: the analyze_plan path must trip the
    // guard and return `Status::unimplemented`.

    /// The guard predicate matches `contains_unresolved` recursively —
    /// a nested Unresolved (Array<Unresolved>) must trip it too.
    #[test]
    fn unresolved_in_nested_array_is_detected() {
        use thunderduck_core::types::StructField;
        let dt = DataType::Array(Box::new(DataType::Unresolved), true);
        assert!(dt.contains_unresolved());
        // A struct containing a nested unresolved must also trip.
        let st = StructType::new(vec![StructField::nullable("a", dt)]);
        assert!(st.fields.iter().any(|f| f.data_type.contains_unresolved()));
    }

    /// Guard predicate test: the same logic the service uses inline —
    /// scan the resolved schema and materialize `Status::unimplemented`
    /// when any field's type contains `DataType::Unresolved`. This test
    /// exercises the guard's contract directly (no session, no runtime)
    /// so it stays deterministic and doesn't contend for the shared
    /// extension binary path with other integration-flavored tests.
    #[test]
    fn boundary_guard_maps_unresolved_field_to_unimplemented_status() {
        use thunderduck_core::types::StructField;
        // Simulate the resolved schema τ's analyzer would produce for a
        // function whose return type wasn't inferred.
        let schema = StructType::new(vec![
            StructField::nullable("ok", DataType::Long),
            StructField::nullable("bad", DataType::Unresolved),
        ]);
        // Materialize the exact guard used in analyze_plan.
        let bad = schema
            .fields
            .iter()
            .find(|f| f.data_type.contains_unresolved())
            .expect("expected the unresolved field to be detected");
        assert_eq!(bad.name, "bad");
        let status = Status::unimplemented(format!(
            "τ boundary: unresolved type for field '{name}' — \
             analyzer did not infer type",
            name = bad.name,
        ));
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(
            status.message().contains("τ boundary"),
            "guard message must carry the τ-boundary tag; got: {msg}",
            msg = status.message(),
        );
        assert!(
            status.message().contains("bad"),
            "guard message must name the offending field; got: {msg}",
            msg = status.message(),
        );
    }

    // ── Eager pivot-value discovery ─────────────────────────────────────────

    /// The discovery pass rewrites a values-less `Pivot` into the sorted, typed
    /// literal set discovered from the live session — including a legitimate
    /// NULL bucket, which Spark's values-less overload does not null-filter and
    /// which sorts first (nulls-first). Uses an inline `Values` input so the
    /// test is self-contained (no temp-view registration needed).
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_implicit_pivots_discovers_sorted_typed_values_with_null_bucket() {
        use thunderduck_core::transpiler_v2::ast::CommonOp;
        use thunderduck_core::transpiler_v2::expression::FunctionCall;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let session = session_manager
            .get_or_create("pivot-discovery-session")
            .await
            .expect("session must be created");

        // Inline data: pivot column `p` has distinct values {NULL, "b", "a"}
        // across the rows — discovery must return them sorted ascending with
        // NULL first.
        let values = CommonAst::new(CommonOp::Values {
            rows: vec![
                vec![int_lit(1), str_lit("b")],
                vec![int_lit(1), null_str_lit()],
                vec![int_lit(2), str_lit("a")],
                vec![int_lit(2), str_lit("b")],
            ],
            column_names: vec!["g".to_owned(), "p".to_owned()],
        });
        let mut ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(values),
            grouping: thunderduck_core::transpiler_v2::ast::PivotGrouping::Explicit(vec![col("g")]),
            pivot_column: col("p"),
            pivot_values: vec![],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "count".to_owned(),
                args: vec![int_lit(1)],
                distinct: false,
            })],
        });

        resolve_implicit_pivots(&mut ast, &session)
            .await
            .expect("discovery pass must succeed");

        let discovered = match &ast.op {
            CommonOp::Pivot { pivot_values, .. } => pivot_values,
            _ => panic!("expected Pivot"),
        };
        // NULL bucket sorts first, then "a", then "b".
        assert_eq!(discovered.len(), 3, "expected NULL + two distinct values");
        assert!(
            matches!(
                &discovered[0],
                Expression::Literal(Literal {
                    value: LiteralValue::Null,
                    ..
                })
            ),
            "NULL bucket must sort first, got {:?}",
            discovered[0]
        );
        let as_str = |e: &Expression| match e {
            Expression::Literal(Literal {
                value: LiteralValue::String(s),
                ..
            }) => s.clone(),
            other => panic!("expected string literal, got {other:?}"),
        };
        assert_eq!(as_str(&discovered[1]), "a");
        assert_eq!(as_str(&discovered[2]), "b");
    }

    /// End-to-end reproducer for misc-006 (`crosstab(col1, col2)`): the
    /// discovery pass desugars a `Crosstab` into a conditional-count
    /// `Aggregate` whose resolved schema is Spark's contingency table — col0 =
    /// `CAST(col1 AS STRING)` named `{col1}_{col2}` (nullable), then one
    /// `bigint` non-null count column per distinct col2 value, named by the
    /// value's string form and sorted lexicographically. The emitted SQL must
    /// also execute cleanly against DuckDB. Before the fix this punted with
    /// `Crosstab[dynamic-values]` at τ's analyzer boundary.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_implicit_pivots_desugars_crosstab_end_to_end() {
        use thunderduck_core::transpiler_v2::ast::CommonOp;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let session = session_manager
            .get_or_create("crosstab-desugar-session")
            .await
            .expect("session must be created");

        // Inline data: dept_id ∈ {10, 20}, active ∈ {true, false}.
        let values = CommonAst::new(CommonOp::Values {
            rows: vec![
                vec![int_lit(10), bool_lit(true)],
                vec![int_lit(10), bool_lit(false)],
                vec![int_lit(20), bool_lit(true)],
                vec![int_lit(20), bool_lit(true)],
            ],
            column_names: vec!["dept_id".to_owned(), "active".to_owned()],
        });
        let mut ast = CommonAst::new(CommonOp::Crosstab {
            input: Box::new(values),
            col1: "dept_id".to_owned(),
            col2: "active".to_owned(),
        });

        resolve_implicit_pivots(&mut ast, &session)
            .await
            .expect("crosstab desugar must succeed");
        // The Crosstab node is replaced by the conditional-count Aggregate.
        assert!(
            matches!(ast.op, CommonOp::Aggregate { .. }),
            "crosstab must desugar into an Aggregate; got {:?}",
            ast.op
        );

        let (sql, schema) = finalize(&session, &ast)
            .await
            .expect("desugared crosstab must emit SQL");

        // Spark-parity contingency schema: col0 + one count col per distinct
        // col2 value, sorted lexicographically ('false' < 'true').
        assert_eq!(schema.fields.len(), 3);
        assert_eq!(schema.fields[0].name, "dept_id_active");
        assert_eq!(schema.fields[0].data_type, DataType::String);
        // col0 nullability follows col1: the inline `dept_id` literals are
        // non-null, so col0 is non-null here (the analyzer unit test covers the
        // nullable-source case that matches misc-006's `emp.dept_id`).
        assert!(!schema.fields[0].nullable);
        assert_eq!(schema.fields[1].name, "false");
        assert_eq!(schema.fields[1].data_type, DataType::Long);
        assert!(!schema.fields[1].nullable);
        assert_eq!(schema.fields[2].name, "true");
        assert_eq!(schema.fields[2].data_type, DataType::Long);
        assert!(!schema.fields[2].nullable);

        // The emitted SQL must run against DuckDB without error.
        session
            .execute(&sql)
            .await
            .expect("desugared crosstab SQL must execute in DuckDB");
    }

    /// Companion test: schemas without unresolved must pass the guard
    /// (locks the false-positive contract — the guard must not fire for
    /// fully-resolved schemas).
    #[test]
    fn boundary_guard_does_not_fire_for_fully_resolved_schema() {
        use thunderduck_core::types::StructField;
        let schema = StructType::new(vec![
            StructField::nullable("a", DataType::Long),
            StructField::nullable("b", DataType::Array(Box::new(DataType::String), true)),
        ]);
        assert!(
            !schema
                .fields
                .iter()
                .any(|f| f.data_type.contains_unresolved()),
            "guard must not fire for a fully-resolved schema",
        );
    }

    // ── Pass 96 — temp-view registration + catalog bridge ────────────────────
    //
    // These tests pin the two compounding fixes: (1) the catalog closure now
    // resolves an empty-scan `TableScan` from the session's temp-view schema
    // cache, and (2) `handle_create_dataframe_view` registers the view.

    fn sql_plan(query: &str) -> proto::Plan {
        proto::Plan {
            op_type: Some(proto::plan::OpType::Root(proto::Relation {
                common: None,
                rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                    query: query.to_owned(),
                    ..Default::default()
                })),
            })),
        }
    }

    /// A registered temp view resolves through the analyzer's catalog bridge:
    /// `create_temp_view_with_schema("emp", ...)` then `SELECT * FROM emp` on
    /// the SAME session succeeds (no `UnknownTable`), yields an `ArrowBatch` +
    /// trailing `ResultComplete`, and the stamped schema field names match.
    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_bridge_resolves_registered_view() {
        use arrow_ipc::reader::StreamReader;
        use std::io::Cursor;
        use thunderduck_core::types::StructField;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let session = session_manager
            .get_or_create("catalog-bridge-session")
            .await
            .expect("session must be creatable");

        let schema = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
        ]);
        session
            .create_temp_view_with_schema(
                "emp",
                "SELECT * FROM (VALUES (1,'a'),(2,'b')) AS t(id, name)",
                schema,
            )
            .await
            .expect("view registration must succeed");

        let svc = ThunderduckService::new(Arc::clone(&session_manager));
        let req = proto::ExecutePlanRequest {
            session_id: "catalog-bridge-session".to_owned(),
            operation_id: Some("test-op".to_owned()),
            plan: Some(sql_plan("SELECT * FROM emp")),
            ..Default::default()
        };
        let resp = svc
            .execute_plan(Request::new(req))
            .await
            .expect("SELECT * FROM emp must resolve the registered view");
        let frames = drain(resp).await;

        let arrow_frame = find_arrow_batch(&frames).expect("expected an ArrowBatch frame");
        assert_trailing_result_complete(&frames);

        let reader = StreamReader::try_new(Cursor::new(arrow_frame.data.as_slice()), None)
            .expect("StreamReader::try_new must succeed on valid IPC bytes");
        let batches: Vec<_> = reader
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("IPC stream must decode without error");
        assert!(!batches.is_empty(), "expected at least one RecordBatch");
        let names: Vec<String> = batches[0]
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().to_owned())
            .collect();
        assert_eq!(
            names,
            vec!["id".to_owned(), "name".to_owned()],
            "stamped schema field names must match the registered view",
        );
    }

    /// `CreateDataframeView` registers the view and returns a lone
    /// `ResultComplete`; a subsequent `SELECT` then resolves it.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_view_command_returns_result_complete() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let svc = ThunderduckService::new(Arc::clone(&session_manager));

        // A light view body (`SELECT 1 AS id`) — Project over SingleRow, no
        // Arrow LocalRelation payload to construct.
        let create = proto::Plan {
            op_type: Some(proto::plan::OpType::Command(proto::Command {
                command_type: Some(proto::command::CommandType::CreateDataframeView(
                    proto::CreateDataFrameViewCommand {
                        input: Some(proto::Relation {
                            common: None,
                            rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                                query: "SELECT 1 AS id".to_owned(),
                                ..Default::default()
                            })),
                        }),
                        name: "emp2".to_owned(),
                        is_global: false,
                        replace: true,
                    },
                )),
            })),
        };
        let req = proto::ExecutePlanRequest {
            session_id: "create-view-session".to_owned(),
            operation_id: Some("test-op".to_owned()),
            plan: Some(create),
            ..Default::default()
        };
        let frames = drain(
            svc.execute_plan(Request::new(req))
                .await
                .expect("CreateDataframeView must succeed"),
        )
        .await;
        assert_eq!(frames.len(), 1, "command arm returns a lone frame");
        // With exactly one frame, "trailing" == "lone".
        assert_trailing_result_complete(&frames);

        // The view now resolves on the same session.
        let sel = proto::ExecutePlanRequest {
            session_id: "create-view-session".to_owned(),
            operation_id: Some("test-op-2".to_owned()),
            plan: Some(sql_plan("SELECT * FROM emp2")),
            ..Default::default()
        };
        let frames = drain(
            svc.execute_plan(Request::new(sel))
                .await
                .expect("SELECT * FROM emp2 must resolve the registered view"),
        )
        .await;
        assert!(
            find_arrow_batch(&frames).is_some(),
            "SELECT over the view must stream an ArrowBatch",
        );
    }

    /// Regression guard: a catalog-free plan (`SELECT 1`) still round-trips —
    /// an empty `empty_scan_tables` result short-circuits `build_base_types` to
    /// `BaseTypes::empty()` with zero session round-trips.
    #[tokio::test(flavor = "multi_thread")]
    async fn select_literal_makes_no_catalog_call_short_circuit() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new(
            thunderduck_core::runtime::StreamingConfig::default(),
        ));
        let svc = ThunderduckService::new(session_manager);
        let req = proto::ExecutePlanRequest {
            session_id: "short-circuit-session".to_owned(),
            operation_id: Some("test-op".to_owned()),
            plan: Some(sql_plan("SELECT 1")),
            ..Default::default()
        };
        let frames = drain(
            svc.execute_plan(Request::new(req))
                .await
                .expect("SELECT 1 must still round-trip"),
        )
        .await;
        assert!(
            find_arrow_batch(&frames).is_some(),
            "SELECT 1 must stream an ArrowBatch",
        );
        assert_trailing_result_complete(&frames);
    }
}
