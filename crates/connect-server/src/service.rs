use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use arrow::array::{ArrayRef, RecordBatch, RecordBatchOptions};
use arrow::datatypes::{Field, Schema};
use futures::stream;
use thunderduck_core::error::ThunderduckError;
use thunderduck_core::parser_v2::SparkSqlParserV2;
use thunderduck_core::transpiler_v2::{self, BaseTypes, CommonAst, Qualifier};
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

pub struct ThunderduckService {
    session_manager: Arc<thunderduck_core::runtime::SessionManager>,
}

impl ThunderduckService {
    pub fn new(session_manager: Arc<thunderduck_core::runtime::SessionManager>) -> Self {
        Self { session_manager }
    }

    /// Fetch or create a session, mapping failures to `Status::internal`.
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

/// Route a Spark Connect [`proto::Relation`] to the correct τ front-end and
/// produce a [`CommonAst`].
///
/// SQL relations use `parser_v2`; other relations use `V2RelationConverter`.
/// Data-dependent schema discovery runs in the async layer because τ's analyzer
/// cannot access the live session.
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

/// Intercept catalog relations before the normal τ pipeline.
async fn relation_to_common_ast_with_session(
    relation: &proto::Relation,
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
) -> Result<CommonAst, Status> {
    if let Some(ast) = crate::catalog_ops::resolve_catalog_relation(relation, session).await? {
        return Ok(
            match relation.common.as_ref().and_then(|common| common.plan_id) {
                Some(plan_id) => ast.with_plan_id(plan_id),
                None => ast,
            },
        );
    }
    relation_to_common_ast(relation)
}

/// Convert a Spark Connect [`proto::Relation`] into a [`CommonAst`] and finalize
/// it into DuckDB SQL + resolved schema via τ.
///
/// Runs data-dependent schema discovery before finalization for command paths.
pub(crate) async fn transpile_relation(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    relation: &proto::Relation,
) -> Result<(CommonAst, String, StructType), Status> {
    let mut common_ast = relation_to_common_ast_with_session(relation, session).await?;
    resolve_implicit_pivots(&mut common_ast, session).await?;
    let (sql, schema) = finalize(session, &common_ast).await?;
    Ok((common_ast, sql, schema))
}

/// Build the per-path `BaseTypes` overlay and emit SQL with its resolved schema.
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
/// Used by `AnalyzePlan(Schema)`; `ExecutePlan` receives the schema from
/// [`finalize`].
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
/// Collects empty-scan tables once and resolves them from the session's
/// temp-view cache, the sole runtime→analyzer bridge. Plans without empty
/// scans short-circuit to [`BaseTypes::empty`].
async fn build_base_types(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    common_ast: &CommonAst,
) -> BaseTypes {
    let tables = thunderduck_core::transpiler_v2::base_types::empty_scan_tables(common_ast);
    if tables.is_empty() {
        return BaseTypes::empty();
    }
    let mut map: HashMap<Qualifier, StructType> = HashMap::new();
    for table in tables {
        if map.contains_key(&table) {
            continue;
        }
        let lookup_name = match table.parts() {
            [part] => part.clone(),
            _ => table.display_name(),
        };
        if let Some(schema) = session.get_view_schema(&lookup_name).await {
            map.insert(table, schema);
        }
    }
    BaseTypes::from_entries(map)
}

/// `spark.sql.pivotMaxValues` default (Spark 4.1.1). A values-less pivot whose
/// pivot column has more than this many distinct values is a Spark-emulated
/// compile error (`_LEGACY_ERROR_TEMP_1324`).
const PIVOT_MAX_VALUES: usize = 10000;

/// Resolve values-less pivots, crosstabs, and schema-less file scans before
/// finalization. Their schemas depend on runtime data or catalog state, so the
/// async service layer must resolve them before synchronous τ analysis.
async fn resolve_implicit_pivots(
    ast: &mut CommonAst,
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
) -> Result<(), Status> {
    use thunderduck_core::transpiler_v2::CommonOp;

    for child in ast.op.children_mut() {
        Box::pin(resolve_implicit_pivots(child, session)).await?;
    }

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

    if matches!(ast.op, CommonOp::Crosstab { .. }) {
        let op = std::mem::replace(&mut ast.op, CommonOp::SingleRow);
        let CommonOp::Crosstab { input, col1, col2 } = op else {
            unreachable!("guarded by the matches! above");
        };
        let col2_expr = thunderduck_core::transpiler_v2::Expression::UnresolvedColumn(
            thunderduck_core::transpiler_v2::expression::UnresolvedColumn {
                name_parts: vec![col2.clone()],
                plan_id: None,
                is_metadata_column: false,
            },
        );
        // NULL is a real Spark crosstab bucket; retain it with DISTINCT and
        // place it first with Spark's NULLS FIRST ordering.
        let distinct_values = discover_pivot_values(&input, &col2_expr, session).await?;
        ast.op = thunderduck_core::transpiler_v2::analyzer::crosstab_to_aggregate(
            *input,
            &col1,
            &col2,
            distinct_values,
        );
    }

    if let CommonOp::FileScan {
        format:
            format @ (thunderduck_core::transpiler_v2::ast::FileFormat::Parquet
            | thunderduck_core::transpiler_v2::ast::FileFormat::Delta),
        paths,
        schema: schema @ None,
        options,
    } = &mut ast.op
    {
        let inferred = discover_file_schema(*format, paths, options, session).await?;
        *schema = Some(inferred);
    }

    Ok(())
}

/// Discover typed pivot values in Spark's ascending, NULLS FIRST order.
async fn discover_pivot_values(
    input: &CommonAst,
    pivot_column: &thunderduck_core::transpiler_v2::Expression,
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
) -> Result<Vec<thunderduck_core::transpiler_v2::Expression>, Status> {
    use thunderduck_core::transpiler_v2::CommonOp;

    let discovery_project = CommonAst::new(CommonOp::Project {
        input: Box::new(input.clone()),
        projections: vec![pivot_column.clone()],
    });
    let (project_sql, _schema) = finalize(session, &discovery_project).await?;
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

    if values.len() > PIVOT_MAX_VALUES {
        return Err(Status::invalid_argument(format!(
            "[_LEGACY_ERROR_TEMP_1324] The pivot column has more than {PIVOT_MAX_VALUES} distinct \
             values, this could indicate an error. If this was intended, set \
             spark.sql.pivotMaxValues to at least the number of distinct values of the pivot column."
        )));
    }
    Ok(values)
}

/// Discover a file-backed relation's schema with a zero-row reader query.
#[allow(clippy::result_large_err)]
async fn discover_file_schema(
    format: thunderduck_core::transpiler_v2::ast::FileFormat,
    paths: &[String],
    options: &[(String, String)],
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
) -> Result<StructType, Status> {
    let reader_call =
        thunderduck_core::transpiler_v2::emission::build_file_reader_sql(format, paths, options)
            .map_err(|e| Status::from(ConnectError::from(e)))?;
    let discovery_sql = format!("SELECT * FROM {reader_call} LIMIT 0");

    let batches = session
        .execute(&discovery_sql)
        .await
        .map_err(|e| Status::from(ConnectError::from(e)))?;

    let arrow_schema = batches.first().map(|b| b.schema()).ok_or_else(|| {
        Status::internal("discover_file_schema: DuckDB returned no batches for LIMIT 0 query")
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
                let mut common_ast =
                    relation_to_common_ast_with_session(&relation, &session).await?;
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
                let session = self.session(&session_id).await?;
                let mut common_ast =
                    relation_to_common_ast_with_session(&relation, &session).await?;
                resolve_implicit_pivots(&mut common_ast, &session).await?;
                let struct_type = analyze_schema(&session, &common_ast).await?;
                // Unresolved serializes as proto Unparsed, which PySpark rejects;
                // report the unsupported τ boundary instead.
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
            let (sql_text, input_rel) = extract_sql_command_text_and_rel(sql_cmd)?;

            handle_sql_command_dispatch(session, session_id, operation_id, &sql_text, input_rel)
                .await
        }
        Some(CommandType::WriteOperation(mut write_cmd)) => {
            let input_rel = write_cmd
                .input
                .take()
                .ok_or_else(|| Status::invalid_argument("WriteOperation missing input"))?;
            let (_common_ast, sql, _schema) = transpile_relation(session, &input_rel).await?;
            handle_write_operation(session, session_id, operation_id, &sql, &write_cmd).await
        }
        _ => Err(Status::unimplemented("Unsupported command type")),
    }
}

/// Build a schema-only `ExecutePlanResponse` from τ's `resolved_schema`.
///
/// The schema frame lets PySpark use its proto schema decoder, including for
/// interval types that its Arrow-schema fallback cannot decode.
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
/// A schema frame precedes batches. Transcoding and stamping run after the
/// mpsc hop, while DuckDB's `!Send` connection remains on its session thread;
/// the bounded channel provides backpressure.
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

/// Remove DuckDB's placeholder from a logical zero-column projection.
fn output_columns(
    batch: &RecordBatch,
    plan: &IntervalPlan,
    resolved_schema: &StructType,
) -> Result<Vec<ArrayRef>, arrow_interval_transcode::TranscodeError> {
    if resolved_schema.fields.is_empty() {
        Ok(Vec::new())
    } else {
        arrow_interval_transcode::apply(batch, plan)
    }
}

/// One iteration of the streaming state machine. Returns
/// `Some((frame, next_state))` while there is work to do; `None` terminates
/// the tonic stream cleanly.
async fn streaming_step(
    mut s: StreamingState,
) -> Option<(Result<proto::ExecutePlanResponse, Status>, StreamingState)> {
    if s.sent_complete_frame {
        return None;
    }
    if !s.sent_schema_frame {
        s.sent_schema_frame = true;
        let frame = build_schema_response(&s.resolved_schema, &s.session_id, &s.operation_id);
        return Some((Ok(frame), s));
    }
    match s.rx.recv().await {
        Some(thunderduck_core::runtime::StreamBatch::Batch(rb)) => {
            let cols: Vec<ArrayRef> = match output_columns(&rb, &s.plan, &s.resolved_schema) {
                Ok(cols) => cols,
                Err(e) => {
                    let status = Status::from(ConnectError::from(e));
                    s.sent_complete_frame = true;
                    return Some((Err(status), s));
                }
            };
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
            // Preserve Spark's error-class token across the gRPC status bridge.
            let err = ThunderduckError::DuckDb(msg).reclassified_spark_runtime();
            let status = Status::from(ConnectError::from(err));
            s.sent_complete_frame = true;
            Some((Err(status), s))
        }
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

/// Extract the SQL text and synthesise the echoed relation from a
/// `SqlCommand` proto. Handles both modern (`input` field) and
/// proto-deprecated (`sql` text field) shapes.
#[allow(clippy::result_large_err)]
fn extract_sql_command_text_and_rel(
    sql_cmd: proto::SqlCommand,
) -> Result<(String, proto::Relation), Status> {
    match sql_cmd.input {
        Some(input_rel) => {
            let text = match &input_rel.rel_type {
                Some(proto::relation::RelType::Sql(sql)) => sql.query.clone(),
                _ => {
                    return Err(Status::invalid_argument(
                        "SqlCommand input is not a Sql relation",
                    ));
                }
            };
            Ok((text, input_rel))
        }
        None => {
            #[allow(deprecated)]
            let text = sql_cmd.sql;
            if text.is_empty() {
                return Err(Status::invalid_argument(
                    "SqlCommand missing both input relation and sql text",
                ));
            }
            let rel = proto::Relation {
                common: None,
                rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                    query: text.clone(),
                    ..Default::default()
                })),
            };
            Ok((text, rel))
        }
    }
}

/// Dispatch a `SqlCommand` — eagerly execute DDL side-effects, or echo
/// the relation for lazy queries.
async fn handle_sql_command_dispatch(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    sql_text: &str,
    input_rel: proto::Relation,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    use thunderduck_core::transpiler_v2::{DdlStatement, SqlStatement};

    let stmt = SparkSqlParserV2::parse_statement(sql_text)
        .map_err(|e| Status::from(ConnectError::from(e)))?;

    match stmt {
        SqlStatement::Query(_) => {
            let _ = transpile_relation(session, &input_rel).await?;
            handle_sql_command_echo(session_id, operation_id, input_rel)
        }
        SqlStatement::Ddl(DdlStatement::CreateTempView {
            name,
            or_replace,
            query,
        }) => {
            handle_sql_create_temp_view(
                session,
                session_id,
                operation_id,
                &name,
                or_replace,
                &query,
            )
            .await
        }
        SqlStatement::Ddl(ddl) => handle_sql_ddl(session, session_id, operation_id, &ddl).await,
    }
}

/// Handle DDL/DML statements from a `SqlCommand`.
///
/// Renders the DDL to DuckDB SQL, executes it via `execute_ddl` with the
/// appropriate schema-cache side effect, and returns a `ResultComplete`
/// response (matching Spark's empty DataFrame result for DDL commands).
///
/// DuckDB catalog errors are mapped to Spark error classes by statement kind.
async fn handle_sql_ddl(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    ddl: &thunderduck_core::transpiler_v2::DdlStatement,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    use thunderduck_core::transpiler_v2::{render_ddl, DdlStatement};

    type CacheEffect = thunderduck_core::runtime::SchemaCacheEffect;

    let (body_sql, view_schema): (Option<String>, Option<StructType>) = match ddl {
        DdlStatement::CreateView { query, .. } => {
            let mut body_ast = query.clone();
            resolve_implicit_pivots(&mut body_ast, session).await?;
            let (sql, schema) = finalize(session, &body_ast).await?;
            (Some(sql), Some(schema))
        }
        DdlStatement::InsertSelect { query, .. } => {
            let mut body_ast = query.clone();
            resolve_implicit_pivots(&mut body_ast, session).await?;
            let (sql, _schema) = finalize(session, &body_ast).await?;
            (Some(sql), None)
        }
        _ => (None, None),
    };

    let sql =
        render_ddl(ddl, body_sql.as_deref()).map_err(|e| Status::from(ConnectError::from(e)))?;

    let effect = match ddl {
        DdlStatement::CreateTable {
            name,
            columns,
            if_not_exists,
        } => {
            if *if_not_exists {
                CacheEffect::CacheIfAbsent {
                    name: name.clone(),
                    schema: columns.clone(),
                }
            } else {
                CacheEffect::Cache {
                    name: name.clone(),
                    schema: columns.clone(),
                }
            }
        }
        DdlStatement::CreateView { name, .. } => {
            if let Some(schema) = view_schema {
                CacheEffect::Cache {
                    name: name.clone(),
                    schema,
                }
            } else {
                CacheEffect::None
            }
        }
        DdlStatement::DropTable { name, .. } | DdlStatement::DropView { name, .. } => {
            CacheEffect::Evict { name: name.clone() }
        }
        DdlStatement::TruncateTable { .. }
        | DdlStatement::InsertValues { .. }
        | DdlStatement::InsertSelect { .. } => CacheEffect::None,
        DdlStatement::CreateTempView { .. } => {
            // Handled by handle_sql_create_temp_view.
            CacheEffect::None
        }
    };

    match session.execute_ddl(&sql, effect).await {
        Ok(()) => Ok(vec![result_complete_response(session_id, operation_id)]),
        Err(e) => Err(map_ddl_error(ddl, e)),
    }
}

/// True when a DuckDB error string reports a missing catalog object — DuckDB
/// spells it two ways depending on which binder raised it.
fn duckdb_says_missing(msg: &str) -> bool {
    msg.contains("does not exist") || msg.contains("not found")
}

/// Spark's verbatim `TABLE_OR_VIEW_NOT_FOUND` status for `name`.
fn table_or_view_not_found(name: &str) -> Status {
    Status::not_found(format!(
        "[TABLE_OR_VIEW_NOT_FOUND] The table or view `{name}` \
         cannot be found. Verify the spelling and correctness of \
         the schema and catalog.\n\
         If you did not qualify the name with a schema, verify the \
         current_schema() output, or qualify the name with the \
         correct schema and catalog."
    ))
}

/// Map a DuckDB catalog error from a DDL statement to a Spark error class.
///
/// DuckDB's error messages for catalog conflicts are recognizable by
/// pattern. We re-clothe them in Spark's error taxonomy.
fn map_ddl_error(
    ddl: &thunderduck_core::transpiler_v2::DdlStatement,
    err: thunderduck_core::error::ThunderduckError,
) -> Status {
    use thunderduck_core::transpiler_v2::DdlStatement;

    let msg = err.to_string();

    match ddl {
        DdlStatement::CreateTable {
            name,
            if_not_exists,
            ..
        } => {
            if !if_not_exists && msg.contains("already exists") {
                return Status::already_exists(format!(
                    "[TABLE_OR_VIEW_ALREADY_EXISTS] The table or view `{name}` \
                     already exists. Choose a different name, drop or replace \
                     the existing object, or add the IF NOT EXISTS clause to \
                     tolerate a pre-existing object."
                ));
            }
        }
        DdlStatement::DropTable {
            name, if_exists, ..
        } => {
            if !if_exists && duckdb_says_missing(&msg) {
                return table_or_view_not_found(name);
            }
        }
        DdlStatement::DropView {
            name, if_exists, ..
        } => {
            if !if_exists && duckdb_says_missing(&msg) {
                return table_or_view_not_found(name);
            }
        }
        DdlStatement::InsertValues { table, .. } | DdlStatement::InsertSelect { table, .. }
            if duckdb_says_missing(&msg) =>
        {
            return table_or_view_not_found(table);
        }
        _ => {}
    }

    Status::from(ConnectError::from(err))
}

/// Handle a pure-query `SqlCommand` — echo the relation as a
/// `SqlCommandResult` so the client can re-send it as a `Root` plan.
#[allow(clippy::result_large_err)]
fn handle_sql_command_echo(
    session_id: &str,
    operation_id: &str,
    result_rel: proto::Relation,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Ok(vec![
        sql_command_result_response(session_id, operation_id, result_rel),
        result_complete_response(session_id, operation_id),
    ])
}

/// Handle `CREATE [OR REPLACE] TEMP VIEW` from a SQL command.
///
/// Finalize/analyze the body `CommonAst` to get (sql, schema), then
/// register the view using the same machinery as
/// `handle_create_dataframe_view`. Returns a lone `ResultComplete`.
async fn handle_sql_create_temp_view(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    name: &str,
    or_replace: bool,
    body: &CommonAst,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    if !or_replace && session.get_view_schema(name).await.is_some() {
        return Err(Status::already_exists(format!(
            "[TEMP_TABLE_OR_VIEW_ALREADY_EXISTS] Cannot create the temporary \
             view `{name}` because it already exists. Choose a different name, \
             drop or replace the existing view, or add the IF NOT EXISTS clause \
             to tolerate a pre-existing view.",
        )));
    }

    let mut body_ast = body.clone();
    resolve_implicit_pivots(&mut body_ast, session).await?;
    let (sql, schema) = finalize(session, &body_ast).await?;

    handle_create_dataframe_view(
        session,
        session_id,
        operation_id,
        name,
        false, // SQL temporary views are session-scoped.
        sql,
        schema,
    )
    .await
}

/// Handle `WriteOperation` after successful transpile.
///
/// Dispatches on `(format, mode, save_type)`:
/// - `(delta, Append, Path)` → ATTACH + INSERT INTO the pre-existing Delta table.
/// - `(parquet, Overwrite, Path)` → `COPY (<sql>) TO '<path>' (FORMAT parquet)`.
/// - `(delta, Overwrite|ErrorIfExists|Ignore, *)` → typed rejection (ADR-017).
/// - everything else → `Status::unimplemented`.
async fn handle_write_operation(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    source_sql: &str,
    write_cmd: &proto::WriteOperation,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    use proto::write_operation::{SaveMode, SaveType};

    let format = write_cmd
        .source
        .as_deref()
        .unwrap_or("parquet")
        .to_ascii_lowercase();
    let mode = SaveMode::try_from(write_cmd.mode).unwrap_or(SaveMode::Unspecified);
    let path = match &write_cmd.save_type {
        Some(SaveType::Path(p)) => p.as_str(),
        Some(SaveType::Table(_)) => {
            return Err(Status::unimplemented(
                "WriteOperation with SaveType::Table is not supported in Thunderduck",
            ));
        }
        None => {
            return Err(Status::invalid_argument(
                "WriteOperation missing save_type (path or table)",
            ));
        }
    };

    match (format.as_str(), mode) {
        ("delta", SaveMode::Append) => {
            write_delta_append(session, session_id, operation_id, source_sql, path).await
        }

        ("parquet", SaveMode::Overwrite) => {
            write_parquet_overwrite(session, session_id, operation_id, source_sql, path).await
        }

        ("delta", SaveMode::Overwrite) => Err(Status::unimplemented(
            "ADR-017: delta overwrite needs delete/truncate in duckdb-delta; \
             revisit when it ships delete",
        )),
        ("delta", SaveMode::ErrorIfExists) => Err(Status::unimplemented(
            "ADR-017: CREATE-on-write DDL is duckdb-delta future work; \
             revisit when confirmed in a pinned build",
        )),
        ("delta", SaveMode::Ignore) => Err(Status::unimplemented(
            "ADR-017: CREATE-on-write DDL is duckdb-delta future work; \
             revisit when confirmed in a pinned build",
        )),

        _ => Err(Status::unimplemented(format!(
            "WriteOperation format={format}, mode={} is not supported in Thunderduck",
            mode.as_str_name(),
        ))),
    }
}

/// Delta append: ATTACH the Delta table, INSERT INTO, DETACH.
///
/// INV9: a writable Delta table is reached via an ATTACH, never a path-scan.
/// ADR-017: one Spark write action → one DuckDB transaction → one Delta version.
///
/// The ATTACH alias `__td_dw` is session-scoped and deterministic — within a
/// single DuckDB connection there is no concurrency (the session thread
/// serializes commands).
async fn write_delta_append(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    source_sql: &str,
    path: &str,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    let escaped_path = path.replace('\'', "''");
    let attach_sql = format!("ATTACH '{escaped_path}' AS __td_dw (TYPE delta)");
    let insert_sql = format!("INSERT INTO __td_dw.main.__td_dw {source_sql}");
    let detach_sql = "DETACH __td_dw";

    session
        .execute(&attach_sql)
        .await
        .map_err(|e| Status::internal(format!("delta ATTACH failed: {e}")))?;

    let insert_result = session.execute(&insert_sql).await;
    if let Err(ref e) = insert_result {
        tracing::warn!(path, "delta INSERT failed, detaching: {e}");
        let _ = session.execute(detach_sql).await;
        return Err(Status::internal(format!("delta INSERT failed: {e}")));
    }

    session
        .execute(detach_sql)
        .await
        .map_err(|e| Status::internal(format!("delta DETACH failed: {e}")))?;

    Ok(vec![result_complete_response(session_id, operation_id)])
}

/// Parquet overwrite: `COPY (<sql>) TO '<path>' (FORMAT parquet)`.
async fn write_parquet_overwrite(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    session_id: &str,
    operation_id: &str,
    source_sql: &str,
    path: &str,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    let escaped_path = path.replace('\'', "''");
    let copy_sql = format!("COPY ({source_sql}) TO '{escaped_path}' (FORMAT parquet)");

    session
        .execute(&copy_sql)
        .await
        .map_err(|e| Status::internal(format!("parquet COPY failed: {e}")))?;

    Ok(vec![result_complete_response(session_id, operation_id)])
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

/// Return the Spark default value for well-known config keys.
///
/// PySpark calls `int()` or `bool()` on some config values, so we must return
/// valid strings for integer and boolean configs rather than empty strings.
fn spark_config_default(key: &str) -> &'static str {
    match key {
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
        "spark.sql.execution.arrow.enabled" => "true",
        "spark.sql.execution.arrow.pyspark.enabled" => "true",
        "spark.sql.execution.arrow.pyspark.fallback.enabled" => "true",
        "spark.sql.execution.pandas.convertToArrowArraySafely" => "false",
        "spark.sql.execution.arrow.pyspark.selfDestructEnabled" => "false",
        "spark.sql.repl.eagerEval.enabled" => "false",
        "spark.sql.adaptive.enabled" => "true",
        "spark.sql.ansi.enabled" => "false",
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
            // Clients parse several Spark defaults as bools or integers.
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
            // None tells the JVM client the key is not configured, disabling
            // plan compression through its missing-key fallback.
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
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
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

    #[test]
    fn zero_column_output_preserves_row_count_without_the_sql_placeholder() {
        use arrow::array::Int32Array;

        let placeholder: ArrayRef = Arc::new(Int32Array::from(vec![1, 1, 1]));
        let batch = RecordBatch::try_from_iter([("__td_empty_projection", placeholder)])
            .expect("placeholder batch");
        let resolved_schema = StructType::empty();
        let columns = output_columns(
            &batch,
            &IntervalPlan::build(&resolved_schema),
            &resolved_schema,
        )
        .expect("zero-column output");
        let schema = Arc::new(post_transcode_schema(&batch.schema(), &columns));
        let options = RecordBatchOptions::new()
            .with_match_field_names(false)
            .with_row_count(Some(batch.num_rows()));
        let output = RecordBatch::try_new_with_options(schema, columns, &options)
            .expect("zero-column Arrow batch");

        assert_eq!(output.num_columns(), 0);
        assert_eq!(output.num_rows(), 3);
    }

    fn col(name: &str) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name_parts: vec![name.to_owned()],
            plan_id: None,
            is_metadata_column: false,
        })
    }

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
        assert!(
            matches!(
                err.code(),
                tonic::Code::Unimplemented | tonic::Code::InvalidArgument
            ),
            "boundary/Spark-emulated errors must surface as Status::unimplemented or \
             Status::invalid_argument, not internal; got {err:?}"
        );
        let message = err.message();
        assert!(
            message.contains("unsupported operator")
                || message.contains("unsupported expression")
                || message.contains("<tau-analyzer-ok>")
                || message.contains("[TABLE_OR_VIEW_NOT_FOUND]")
                || message.contains("<a.2-substrate>"),
            "message must identify τ's boundary error; got: {message}",
        );
    }

    /// `RelType::Sql` MUST route through `parser_v2`, not `V2RelationConverter`.
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
        let (_common_ast, sql, _schema) = transpile_relation(&session, &sql_rel)
            .await
            .expect("τ must emit SQL for `SELECT 1`");
        assert!(
            sql.contains("SELECT"),
            "expected DuckDB SELECT emission; got: {sql}",
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_relation_preserves_plan_id() {
        let session = test_session("test-catalog-plan-id").await;
        let relation = proto::Relation {
            common: Some(proto::RelationCommon {
                plan_id: Some(42),
                ..Default::default()
            }),
            rel_type: Some(proto::relation::RelType::Catalog(proto::Catalog {
                cat_type: Some(proto::catalog::CatType::CurrentCatalog(
                    proto::CurrentCatalog {},
                )),
            })),
        };

        let ast = relation_to_common_ast_with_session(&relation, &session)
            .await
            .expect("supported catalog relation must convert");

        assert_eq!(ast.plan_id, Some(42));
        assert!(matches!(ast.op, transpiler_v2::CommonOp::Values { .. }));
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

    /// Plans without empty scans use `BaseTypes::empty()` and still emit SQL.
    #[tokio::test(flavor = "multi_thread")]
    async fn finalize_short_circuits_on_plans_without_empty_scan() {
        use thunderduck_core::transpiler_v2::ast::CommonOp;
        let session = test_session("test-finalize-short-circuit").await;
        let plan = CommonAst::new(CommonOp::SingleRow);
        let (sql, _schema) = finalize(&session, &plan)
            .await
            .expect("τ must emit for SingleRow");
        assert_eq!(sql, "SELECT 1");
    }

    /// End-to-end streaming query through DuckDB and Arrow IPC.
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_plan_single_row_round_trips_through_duckdb() {
        use arrow::array::Int32Array;
        use arrow_ipc::reader::StreamReader;
        use std::io::Cursor;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
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

        let resp = svc
            .execute_plan(Request::new(req))
            .await
            .expect("execute_plan must succeed");
        let frames = drain(resp).await;

        assert!(!frames.is_empty(), "expected at least one response frame");

        let arrow_frame = find_arrow_batch(&frames).expect("expected an ArrowBatch frame");
        assert!(
            !arrow_frame.data.is_empty(),
            "ArrowBatch data must be non-empty (schema+row IPC bytes)",
        );

        assert_trailing_result_complete(&frames);

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

    /// Modern PySpark path: `SqlCommand { input: Some(RelType::Sql{query}) }`.
    /// The command arm echoes the input relation verbatim.
    #[tokio::test(flavor = "multi_thread")]
    async fn sql_command_select_literals_returns_echoed_relation() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
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

        assert!(
            find_arrow_batch(&frames).is_none(),
            "command arm must not emit an ArrowBatch frame",
        );

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

        assert_trailing_result_complete(&frames);
    }

    /// Deprecated text path: `SqlCommand { sql: "SELECT 1", input: None }`
    /// synthesizes a `RelType::Sql` relation and echoes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn sql_command_deprecated_text_synthesizes_sql_relation() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
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

    /// The guard predicate matches `contains_unresolved` recursively —
    /// a nested Unresolved (Array<Unresolved>) must trip it too.
    #[test]
    fn unresolved_in_nested_array_is_detected() {
        use thunderduck_core::types::StructField;
        let dt = DataType::Array(Box::new(DataType::Unresolved), true);
        assert!(dt.contains_unresolved());
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

    /// Values-less pivots include a NULL bucket and sort discovered values
    /// ascending.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_implicit_pivots_discovers_sorted_typed_values_with_null_bucket() {
        use thunderduck_core::transpiler_v2::ast::CommonOp;
        use thunderduck_core::transpiler_v2::expression::FunctionCall;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("pivot-discovery-session")
            .await
            .expect("session must be created");

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

    /// Crosstab discovery produces a conditional-count aggregate with a
    /// Spark-compatible schema and executable SQL.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_implicit_pivots_desugars_crosstab_end_to_end() {
        use thunderduck_core::transpiler_v2::ast::CommonOp;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("crosstab-desugar-session")
            .await
            .expect("session must be created");

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
        assert!(
            matches!(ast.op, CommonOp::Aggregate { .. }),
            "crosstab must desugar into an Aggregate; got {:?}",
            ast.op
        );

        let (sql, schema) = finalize(&session, &ast)
            .await
            .expect("desugared crosstab must emit SQL");

        assert_eq!(schema.fields.len(), 3);
        assert_eq!(schema.fields[0].name, "dept_id_active");
        assert_eq!(schema.fields[0].data_type, DataType::String);
        assert!(!schema.fields[0].nullable);
        assert_eq!(schema.fields[1].name, "false");
        assert_eq!(schema.fields[1].data_type, DataType::Long);
        assert!(!schema.fields[1].nullable);
        assert_eq!(schema.fields[2].name, "true");
        assert_eq!(schema.fields[2].data_type, DataType::Long);
        assert!(!schema.fields[2].nullable);

        session
            .execute(&sql)
            .await
            .expect("desugared crosstab SQL must execute in DuckDB");
    }

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

    /// A registered temp view resolves through the analyzer catalog bridge.
    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_bridge_resolves_registered_view() {
        use arrow_ipc::reader::StreamReader;
        use std::io::Cursor;
        use thunderduck_core::types::StructField;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_bridge_preserves_quoted_dot_view_name() {
        use thunderduck_core::transpiler_v2::{CommonOp, Qualifier};
        use thunderduck_core::types::StructField;

        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let session = session_manager
            .get_or_create("catalog-bridge-quoted-dot-session")
            .await
            .expect("session must be creatable");
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        session
            .create_temp_view_with_schema("a.b", "SELECT 1 AS id", schema.clone())
            .await
            .expect("quoted-dot view registration must succeed");

        let plan = CommonAst::new(CommonOp::TableScan {
            table: Qualifier::single("a.b"),
        });
        let base_types = build_base_types(&session, &plan).await;
        assert_eq!(base_types.lookup(&Qualifier::single("a.b")), Some(&schema));
    }

    /// `CreateDataframeView` registers the view and returns a lone
    /// `ResultComplete`; a subsequent `SELECT` then resolves it.
    #[tokio::test(flavor = "multi_thread")]
    async fn create_view_command_returns_result_complete() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let svc = ThunderduckService::new(Arc::clone(&session_manager));

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
        assert_trailing_result_complete(&frames);

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

    #[tokio::test(flavor = "multi_thread")]
    async fn select_literal_makes_no_catalog_call_short_circuit() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
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

    /// Build an `ExecutePlanRequest` wrapping a `SqlCommand`.
    fn sql_command_plan(session_id: &str, sql: &str) -> proto::ExecutePlanRequest {
        proto::ExecutePlanRequest {
            session_id: session_id.to_owned(),
            operation_id: Some("test-op".to_owned()),
            plan: Some(proto::Plan {
                op_type: Some(proto::plan::OpType::Command(proto::Command {
                    command_type: Some(proto::command::CommandType::SqlCommand(
                        proto::SqlCommand {
                            #[allow(deprecated)]
                            sql: String::new(),
                            input: Some(proto::Relation {
                                common: None,
                                rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                                    query: sql.to_owned(),
                                    ..Default::default()
                                })),
                            }),
                            ..Default::default()
                        },
                    )),
                })),
            }),
            ..Default::default()
        }
    }

    /// SQL DDL registers a temp view that a subsequent query can resolve.
    #[tokio::test(flavor = "multi_thread")]
    async fn sql_create_temp_view_then_select() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let svc = ThunderduckService::new(Arc::clone(&session_manager));
        let session_id = "sql-create-temp-view-session";

        let create_req = sql_command_plan(session_id, "CREATE TEMP VIEW v AS SELECT 1 AS id");
        let frames = drain(
            svc.execute_plan(Request::new(create_req))
                .await
                .expect("CREATE TEMP VIEW via SqlCommand must succeed"),
        )
        .await;
        assert_eq!(frames.len(), 1, "DDL returns exactly one frame");
        assert_trailing_result_complete(&frames);

        let select_req = proto::ExecutePlanRequest {
            session_id: session_id.to_owned(),
            operation_id: Some("test-op-2".to_owned()),
            plan: Some(sql_plan("SELECT * FROM v")),
            ..Default::default()
        };
        let frames = drain(
            svc.execute_plan(Request::new(select_req))
                .await
                .expect("SELECT * FROM v must resolve the SQL-created temp view"),
        )
        .await;
        assert!(
            find_arrow_batch(&frames).is_some(),
            "SELECT over the SQL-created view must stream an ArrowBatch",
        );
        assert_trailing_result_complete(&frames);
    }

    /// `CREATE OR REPLACE TEMP VIEW` via SqlCommand overwrites an existing
    /// view without error.
    #[tokio::test(flavor = "multi_thread")]
    async fn sql_create_or_replace_temp_view_overwrites() {
        let session_manager = Arc::new(thunderduck_core::runtime::SessionManager::new());
        let svc = ThunderduckService::new(Arc::clone(&session_manager));
        let session_id = "sql-replace-view-session";

        let req1 = sql_command_plan(session_id, "CREATE OR REPLACE TEMP VIEW w AS SELECT 1 AS a");
        let frames = drain(
            svc.execute_plan(Request::new(req1))
                .await
                .expect("first CREATE OR REPLACE must succeed"),
        )
        .await;
        assert_eq!(frames.len(), 1);

        let req2 = sql_command_plan(session_id, "CREATE OR REPLACE TEMP VIEW w AS SELECT 2 AS b");
        let frames = drain(
            svc.execute_plan(Request::new(req2))
                .await
                .expect("second CREATE OR REPLACE must succeed"),
        )
        .await;
        assert_eq!(frames.len(), 1);
    }

    /// Build a minimal `WriteOperation` proto for dispatch testing.
    fn write_op(format: &str, mode: i32, path: &str) -> proto::WriteOperation {
        proto::WriteOperation {
            input: None,
            source: Some(format.to_owned()),
            mode,
            sort_column_names: vec![],
            partitioning_columns: vec![],
            bucket_by: None,
            options: Default::default(),
            clustering_columns: vec![],
            save_type: Some(proto::write_operation::SaveType::Path(path.to_owned())),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_delta_overwrite_returns_unimplemented() {
        let session = test_session("test-write-delta-ow").await;
        let cmd = write_op(
            "delta",
            proto::write_operation::SaveMode::Overwrite as i32,
            "/tmp/t",
        );
        let err = handle_write_operation(&session, "s", "o", "SELECT 1", &cmd)
            .await
            .expect_err("delta overwrite must be rejected");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(
            err.message().contains("ADR-017"),
            "rejection must cite ADR-017; got: {}",
            err.message(),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_delta_error_if_exists_returns_unimplemented() {
        let session = test_session("test-write-delta-eie").await;
        let cmd = write_op(
            "delta",
            proto::write_operation::SaveMode::ErrorIfExists as i32,
            "/tmp/t",
        );
        let err = handle_write_operation(&session, "s", "o", "SELECT 1", &cmd)
            .await
            .expect_err("delta error_if_exists must be rejected");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(err.message().contains("ADR-017"), "got: {}", err.message());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_delta_ignore_returns_unimplemented() {
        let session = test_session("test-write-delta-ign").await;
        let cmd = write_op(
            "delta",
            proto::write_operation::SaveMode::Ignore as i32,
            "/tmp/t",
        );
        let err = handle_write_operation(&session, "s", "o", "SELECT 1", &cmd)
            .await
            .expect_err("delta ignore must be rejected");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(err.message().contains("ADR-017"), "got: {}", err.message());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_unsupported_format_returns_unimplemented() {
        let session = test_session("test-write-unsup-fmt").await;
        let cmd = write_op(
            "orc",
            proto::write_operation::SaveMode::Overwrite as i32,
            "/tmp/t",
        );
        let err = handle_write_operation(&session, "s", "o", "SELECT 1", &cmd)
            .await
            .expect_err("unsupported format must be rejected");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(
            err.message().contains("orc"),
            "message must name the format; got: {}",
            err.message(),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_missing_save_type_returns_invalid_argument() {
        let session = test_session("test-write-no-save-type").await;
        let cmd = proto::WriteOperation {
            input: None,
            source: Some("parquet".to_owned()),
            mode: proto::write_operation::SaveMode::Overwrite as i32,
            sort_column_names: vec![],
            partitioning_columns: vec![],
            bucket_by: None,
            options: Default::default(),
            clustering_columns: vec![],
            save_type: None,
        };
        let err = handle_write_operation(&session, "s", "o", "SELECT 1", &cmd)
            .await
            .expect_err("missing save_type must be rejected");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_table_save_type_returns_unimplemented() {
        let session = test_session("test-write-table-save-type").await;
        let cmd = proto::WriteOperation {
            input: None,
            source: Some("delta".to_owned()),
            mode: proto::write_operation::SaveMode::Append as i32,
            sort_column_names: vec![],
            partitioning_columns: vec![],
            bucket_by: None,
            options: Default::default(),
            clustering_columns: vec![],
            save_type: Some(proto::write_operation::SaveType::Table(
                proto::write_operation::SaveTable {
                    table_name: "t".to_owned(),
                    save_method: 0,
                },
            )),
        };
        let err = handle_write_operation(&session, "s", "o", "SELECT 1", &cmd)
            .await
            .expect_err("SaveType::Table must be rejected");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
    }
}
