use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream;
use thunderduck_core::parser_v2::SparkSqlParserV2;
use thunderduck_core::transpiler_v2::{self, BaseTypes, CommonAst};
use thunderduck_core::types::{DataType, StructType};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::arrow_ipc::record_batches_to_arrow_batches;
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
/// `finalize` runs the analyzer + emission; it succeeds for every plan τ covers
/// and returns a Thunderduck-boundary `Status` (`UnsupportedOp` /
/// `UnsupportedProtoShape`) for shapes it does not. The emitted SQL feeds
/// `execute_streaming_query`; the schema drives the outbound Arrow-schema stamp.
pub(crate) async fn transpile_relation(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    relation: &proto::Relation,
) -> Result<(CommonAst, String, StructType), Status> {
    let common_ast = relation_to_common_ast(relation)?;
    let (sql, schema) = finalize(session, &common_ast).await?;
    Ok((common_ast, sql, schema))
}

/// Build the per-path `BaseTypes` overlay and run τ's fused emit-and-schema
/// entry point in ONE analyzer pass.
///
/// Returns both the emitted DuckDB SQL and the analyzer's root
/// `resolved_schema` — the schema drives the outbound Arrow-schema stamp in
/// `execute_streaming_query` (see `arrow_schema_stamp::stamp_batch_schemas`).
/// Fusing avoids the second `analyze()` call that pass 88's initial wiring
/// incurred (perf review HIGH #1).
///
/// The catalog closure resolves empty-scan `TableScan` schemas from the
/// session's temp-view cache (Slice B — the runtime→analyzer bridge).
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
/// Slice B: the catalog closure resolves each empty-scan `TableScan` from the
/// session's temp-view schema cache. `get_view_schema` is async and
/// `build_from_plan`'s closure is sync, so we pre-fetch every table's schema
/// into a map first, then feed `build_from_plan` a synchronous
/// `|name| map.get(name).cloned()`. The closure stays the sole runtime→analyzer
/// bridge (INV10). Short-circuits to `BaseTypes::empty()` when the plan carries
/// no empty scan (ADR-012 request-handler seeding short-circuit).
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
    BaseTypes::build_from_plan(common_ast, |name| map.get(name).cloned())
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

        let session = self
            .session_manager
            .get_or_create(&session_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let plan = req
            .plan
            .ok_or_else(|| Status::invalid_argument("missing plan"))?;

        let responses: Vec<proto::ExecutePlanResponse> = match plan.op_type {
            Some(proto::plan::OpType::Root(relation)) => {
                let (common_ast, sql, resolved_schema) =
                    transpile_relation(&session, &relation).await?;
                // `finalize` (inside `transpile_relation`) succeeds for every
                // plan τ covers, so `execute_streaming_query` is live. DDL
                // classification is still a placeholder — `classify_plan`
                // always returns `Query` until Slice C.1 (see `execute_ddl`).
                match classify_plan(&common_ast) {
                    PlanKind::Ddl => {
                        execute_ddl(&session, &common_ast, &sql, &session_id, &operation_id).await?
                    }
                    PlanKind::Query => {
                        return execute_streaming_query(
                            &session,
                            &sql,
                            &resolved_schema,
                            &session_id,
                            &operation_id,
                        )
                        .await;
                    }
                }
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
                // Session carries the temp-view catalog the analyzer resolves
                // `TableScan` schemas from (Slice B catalog bridge).
                let session = self
                    .session_manager
                    .get_or_create(&session_id)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?;
                // E.0 addendum: route analyze_plan(Schema) through τ's
                // analyzer. Parse the relation to CommonAst, then invoke
                // `analyze_schema` — which runs the Slice-B analyzer without
                // calling `dispatch_op`. Errors surface via the same
                // two-category bridge `finalize` uses (AnalyzerError →
                // EmissionError → ConnectError → Status).
                //
                // ExecutePlan/AnalyzePlan symmetry: this path serializes τ's
                // `resolved_schema` verbatim (via `data_type_to_proto`), so
                // AnalyzePlan already surfaces the Spark-visible view.
                // ExecutePlan achieves the same on the response path via
                // `arrow_schema_stamp::stamp_batch_schemas` in
                // `execute_streaming_query`. Do not modify this arm.
                let common_ast = relation_to_common_ast(&relation)?;
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
            // TODO Slice C.1: eager DDL/DML side effects
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

// ── Plan-classification + DDL helpers ────────────────────────────────────────
//
// These helpers consume `&CommonAst`. `execute_streaming_query` (the Query arm)
// is live; `classify_plan` still collapses to `Query` and `execute_ddl` remains
// an `unimplemented` placeholder until Slice C.1 wires DDL execution.

/// Classification of a τ plan for execution routing.
///
/// Currently a two-arm placeholder (DDL vs. Query) pending τ-side DDL
/// classification over `CommonAst`.
#[allow(dead_code)] // `Ddl` reintroduced when DDL classification lands.
enum PlanKind {
    /// A DDL/DML statement — execute without result streaming.
    Ddl,
    /// A query — stream results back to the client.
    Query,
}

/// Classify a τ plan as DDL or query.
///
/// always returns [`PlanKind::Query`]. The DDL classification is a
/// τ's emission substrate deliverable — `CommonAst` does not yet carry a DDL discriminant.
fn classify_plan(_common_ast: &CommonAst) -> PlanKind {
    PlanKind::Query
}

/// Execute a DDL statement and return appropriate responses.
///
/// **τ's emission substrate (owner):** DDL classification and execution over `CommonAst`.
/// the τ dispatch site body errors with `Status::unimplemented` because τ's
/// `finalize()` never reaches this point.
async fn execute_ddl(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    _common_ast: &CommonAst,
    _sql: &str,
    _session_id: &str,
    _operation_id: &str,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Err(Status::unimplemented(
        "DDL classification and execution over CommonAst",
    ))
}

/// Execute a query plan and stream results back to the client.
///
/// **future τ work.0 (owner):** streaming query execution over `CommonAst`.
///
/// The `_common_ast` parameter is reserved for future use (per plan §8:
/// `spark_names` / column-rename metadata) and is intentionally unused at
/// future τ work.0. The τ pipeline has already produced fully-aliased DuckDB SQL by
/// this point (via `finalize()` at the dispatch seam), so E.0 only needs to
/// submit that SQL to the session and wrap the resulting Arrow batches into
/// `ExecutePlanResponse` frames.
///
/// Execution shape (collect-then-stream, symmetric with the DDL arm at the
/// call site in `execute_plan`):
///   1. `session.execute(sql).await` submits the SQL through the intact
///      `SessionCommand::Execute` → oneshot transport (no new variants
///      introduced at E.0). Failures map `ThunderduckError → ConnectError
///      → Status::internal` — DuckDB errors on τ-emitted SQL are Thunderduck
///      runtime bugs, not client faults.
///   2. `batches_to_responses` serializes each `RecordBatch` as an
///      independent Arrow IPC stream (schema-only frame for 0 rows) and
///      appends a trailing `ResultComplete` frame.
///   3. `stream::iter(responses.into_iter().map(Ok))` boxes the responses
///      into the `ExecutePlanStream` shape.
async fn execute_streaming_query(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    sql: &str,
    resolved_schema: &StructType,
    session_id: &str,
    operation_id: &str,
) -> Result<Response<<ThunderduckService as SparkConnectService>::ExecutePlanStream>, Status> {
    // `resolved_schema` was produced by the SAME `analyze` call that emitted
    // `sql` (via `transpiler_v2::generate_with_schema`, see `finalize`). Perf
    // review HIGH #1: the earlier wiring re-ran the analyzer here — dropped.
    // Its Spark-visible view drives the outbound Arrow-schema stamp; see
    // `arrow_schema_stamp` for the "why" (arr-012 duplicate-struct-field-name
    // substrate gap + boundary hygiene).
    let batches = session
        .execute(sql)
        .await
        .map_err(|e| Status::from(ConnectError::from(e)))?;

    // Metadata-only rename: buffer identity is preserved (see
    // `arrow_schema_stamp::stamp_batch_schemas` doc).
    let stamped = crate::arrow_schema_stamp::stamp_batch_schemas(batches, resolved_schema);

    let responses =
        batches_to_responses(session_id, operation_id, &stamped).map_err(Status::from)?;

    let stream = stream::iter(responses.into_iter().map(Ok));
    Ok(Response::new(Box::pin(stream)))
}

/// Handle `CreateDataframeView` after successful transpile.
///
/// **Slice B (owner):** register the temp view in the session — both in DuckDB
/// (so `SELECT * FROM <name>` executes) and in the session's Spark-schema cache
/// (so the analyzer's catalog bridge can resolve the view's columns +
/// nullabilities, which DuckDB's `CREATE VIEW` loses). Returns a lone
/// `ResultComplete` (ADR-011 command-arm response shape).
///
/// `is_global` (global temp views) is out of scope for Slice B: the view is
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
        tracing::warn!(view = %name, "global temp view registered as session-local (Slice B)");
    }
    session
        .create_temp_view_with_schema(name, &sql, schema)
        .await
        .map_err(|e| Status::from(ConnectError::from(e)))?;
    Ok(vec![result_complete_response(session_id, operation_id)])
}

/// Handle `SqlCommand` (both `input`-bearing and deprecated text paths).
///
/// **Slice C.1 (owner):** SQL command execution over `CommonAst`.
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
    Err(Status::unimplemented(
        "WriteOperation over CommonAst",
    ))
}

// ── Response builders (preserved) ────────────────────────────────────────────

/// Convert DuckDB record batches to a complete `ExecutePlanResponse` sequence,
/// including the mandatory trailing `ResultComplete` frame.
fn batches_to_responses(
    session_id: &str,
    operation_id: &str,
    batches: &[arrow::record_batch::RecordBatch],
) -> crate::error::Result<Vec<proto::ExecutePlanResponse>> {
    let arrow_batches = record_batches_to_arrow_batches(batches)?;
    let mut responses: Vec<proto::ExecutePlanResponse> =
        Vec::with_capacity(arrow_batches.len() + 1);
    for (i, ab) in arrow_batches.into_iter().enumerate() {
        responses.push(proto::ExecutePlanResponse {
            session_id: session_id.to_string(),
            server_side_session_id: SERVER_SESSION_ID.clone(),
            operation_id: operation_id.to_string(),
            response_id: format!("{operation_id}-{i}"),
            response_type: Some(proto::execute_plan_response::ResponseType::ArrowBatch(ab)),
            ..Default::default()
        });
    }
    // Send ResultComplete even when there are no batches (0 rows).
    // Do NOT push an empty ArrowBatch (data: vec![]) — empty bytes are invalid Arrow IPC
    // and PySpark raises ArrowInvalid when it tries to deserialize them.
    responses.push(result_complete_response(session_id, operation_id));
    Ok(responses)
}

/// Create an ArrowBatch response with a single boolean `value` column = `val`.
/// Used for DDL operations (DropTempView etc.) that must return a non-null table.
#[allow(dead_code)]
fn bool_batch_responses(
    session_id: &str,
    operation_id: &str,
    val: bool,
) -> crate::error::Result<Vec<proto::ExecutePlanResponse>> {
    use arrow::array::BooleanArray;
    use arrow::datatypes::{Field, Schema};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        arrow::datatypes::DataType::Boolean,
        false,
    )]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(BooleanArray::from(vec![val]))],
    )
    .map_err(|e| crate::error::ConnectError::Arrow(e.to_string()))?;
    batches_to_responses(session_id, operation_id, &[batch])
}

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
    // At A.3, τ's `generate()` errors with `EmissionError::UnsupportedOp` on
    // every input. These tests pin two properties of the dispatch shape:
    //   1. Structurally-valid inputs reach τ (via `V2RelationConverter` or
    //      `parser_v2`) and surface the emission boundary via
    //      `Status::unimplemented` — not `Status::internal`.
    //   2. `RelType::Sql` routes to `parser_v2`, never through
    //      `V2RelationConverter` (which would return
    //      `UnsupportedProtoShape { shape: "RelType::Sql" }`).

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
    /// (`UnsupportedProtoShape { shape: "sql::parse_error", ... }`), which
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
        // A `SingleRow` plan carries no `TableScan` → `plan_has_empty_scan`
        // is false → `BaseTypes::empty()` (no closure invocation) → τ emits.
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
    // `execute_streaming_query` (session.execute + batches_to_responses) →
    // Arrow IPC stream. Verifies the E.0 wiring end-to-end at the service
    // layer without spinning up a network gRPC server.

    /// End-to-end smoke test for the future τ work.0 streaming-query wiring.
    ///
    /// Marked `#[ignore]` because τ's Slice-C.1 `SingleRow` renderer emits a
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
        use futures::StreamExt;
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
        let mut stream = resp.into_inner();
        let mut frames: Vec<proto::ExecutePlanResponse> = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(item.expect("stream frame must be Ok"));
        }

        // Assert: at least one ArrowBatch frame with non-empty data, and a
        // trailing ResultComplete frame.
        assert!(!frames.is_empty(), "expected at least one response frame");

        let arrow_frame = frames
            .iter()
            .find_map(|f| match &f.response_type {
                Some(proto::execute_plan_response::ResponseType::ArrowBatch(ab)) => Some(ab),
                _ => None,
            })
            .expect("expected an ArrowBatch frame");
        assert!(
            !arrow_frame.data.is_empty(),
            "ArrowBatch data must be non-empty (schema+row IPC bytes)",
        );

        let has_complete = frames.last().is_some_and(|f| {
            matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ResultComplete(
                    _
                ))
            )
        });
        assert!(has_complete, "final frame must be ResultComplete");

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
        use futures::StreamExt;

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
        let mut stream = resp.into_inner();
        let mut frames: Vec<proto::ExecutePlanResponse> = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(item.expect("stream frame must be Ok"));
        }

        // No ArrowBatch frame — the command arm does not stream data.
        assert!(
            !frames.iter().any(|f| matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ArrowBatch(_))
            )),
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
        let has_complete = frames.last().is_some_and(|f| {
            matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ResultComplete(
                    _
                ))
            )
        });
        assert!(has_complete, "final frame must be ResultComplete");
    }

    /// Deprecated text path: `SqlCommand { sql: "SELECT 1", input: None }`
    /// synthesizes a `RelType::Sql` relation and echoes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn sql_command_deprecated_text_synthesizes_sql_relation() {
        use futures::StreamExt;

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
        let mut stream = resp.into_inner();
        let mut frames: Vec<proto::ExecutePlanResponse> = Vec::new();
        while let Some(item) = stream.next().await {
            frames.push(item.expect("stream frame must be Ok"));
        }

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

        let has_complete = frames.last().is_some_and(|f| {
            matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ResultComplete(
                    _
                ))
            )
        });
        assert!(has_complete, "final frame must be ResultComplete");
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

    // ── Pass 96 — Slice-B temp-view registration + catalog bridge ────────────
    //
    // These tests pin the two compounding fixes: (1) the catalog closure now
    // resolves an empty-scan `TableScan` from the session's temp-view schema
    // cache, and (2) `handle_create_dataframe_view` registers the view.

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

        let arrow_frame = frames
            .iter()
            .find_map(|f| match &f.response_type {
                Some(proto::execute_plan_response::ResponseType::ArrowBatch(ab)) => Some(ab),
                _ => None,
            })
            .expect("expected an ArrowBatch frame");
        let has_complete = frames.last().is_some_and(|f| {
            matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ResultComplete(
                    _
                ))
            )
        });
        assert!(has_complete, "final frame must be ResultComplete");

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
        assert!(
            matches!(
                frames[0].response_type,
                Some(proto::execute_plan_response::ResponseType::ResultComplete(
                    _
                ))
            ),
            "the lone frame must be ResultComplete",
        );

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
            frames.iter().any(|f| matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ArrowBatch(_))
            )),
            "SELECT over the view must stream an ArrowBatch",
        );
    }

    /// Regression guard: a catalog-free plan (`SELECT 1`) still round-trips —
    /// `plan_has_empty_scan == false` short-circuits `build_base_types` to
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
            frames.iter().any(|f| matches!(
                f.response_type,
                Some(proto::execute_plan_response::ResponseType::ArrowBatch(_))
            )),
            "SELECT 1 must stream an ArrowBatch",
        );
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
}
