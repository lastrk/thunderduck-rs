use std::pin::Pin;
use std::sync::Arc;

use futures::stream;
use thunderduck_core::generator::SqlGenerator;
use thunderduck_core::runtime::{RuntimeCompatMode, SchemaInferrer, SessionManager};
use thunderduck_core::types::DataType;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::arrow_ipc::record_batches_to_arrow_batches;
use crate::converter::type_converter::data_type_to_proto;
use crate::converter::PlanConverter;
use crate::error::ConnectError;
use crate::proto::spark::connect as proto;
use crate::proto::spark::connect::spark_connect_service_server::SparkConnectService;

type BoxStream<T> =
    Pin<Box<dyn futures::Stream<Item = Result<T, Status>> + Send + 'static>>;

pub struct ThunderduckService {
    session_manager: Arc<SessionManager>,
    #[allow(dead_code)]
    mode: RuntimeCompatMode,
}

impl ThunderduckService {
    pub fn new(session_manager: Arc<SessionManager>, mode: RuntimeCompatMode) -> Self {
        Self { session_manager, mode }
    }
}

static SERVER_SESSION_ID: &str = "thunderduck-server-1";

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

        let session = self
            .session_manager
            .get_or_create(&session_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let plan = req.plan.ok_or_else(|| Status::invalid_argument("missing plan"))?;

        let responses: Vec<proto::ExecutePlanResponse> = match plan.op_type {
            Some(proto::plan::OpType::Root(relation)) => {
                let logical_plan =
                    PlanConverter::convert_relation_with_session(&relation, Arc::clone(&session))
                        .map_err(Status::from)?;

                // Special case: ApproxQuantile needs a ListArray response.
                if let thunderduck_core::logical::LogicalPlan::ApproxQuantile(ref aq) = logical_plan {
                    let batch = execute_approx_quantile(&session, aq)
                        .await
                        .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
                    let responses = batches_to_responses(&session_id, &operation_id, &[batch])
                        .map_err(|e| Status::from(e))?;
                    let stream = stream::iter(responses.into_iter().map(Ok));
                    return Ok(Response::new(Box::pin(stream)));
                }

                let sql = SqlGenerator::relaxed()
                    .generate(&logical_plan)
                    .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;

                // Special case: DDL statements (DROP VIEW, etc.) return 0 rows.
                // Execute as DDL and synthesize a single boolean result row so that
                // PySpark's _execute_and_fetch gets a non-null table back.
                // For DROP VIEW, the bool indicates whether the view existed.
                let upper = sql.trim_start().to_uppercase();
                if upper.starts_with("DROP VIEW IF EXISTS ") {
                    // Extract view name — everything after "DROP VIEW IF EXISTS "
                    let view_name = sql.trim_start()[20..].trim().trim_matches('"');
                    // Check existence before dropping
                    let exists = session
                        .execute(&format!(
                            "SELECT COUNT(*) > 0 AS existed \
                             FROM information_schema.views \
                             WHERE table_name = '{}' AND table_schema = 'main'",
                            view_name.replace('\'', "''")
                        ))
                        .await
                        .ok()
                        .and_then(|b| {
                            use arrow::array::{Array, BooleanArray};
                            b.into_iter()
                                .next()
                                .and_then(|rb| rb.column(0).as_any().downcast_ref::<BooleanArray>()
                                    .and_then(|a| (!a.is_null(0)).then(|| a.value(0))))
                        })
                        .unwrap_or(false);
                    session.exec_ddl(&sql).await.ok();
                    bool_batch_responses(&session_id, &operation_id, exists)
                        .map_err(|e| Status::from(e))?
                } else {
                    let batches = session
                        .execute(&sql)
                        .await
                        .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
                    batches_to_responses(&session_id, &operation_id, &batches)
                        .map_err(|e| Status::from(e))?
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

        use proto::analyze_plan_request::Analyze;
        match req.analyze {
            Some(Analyze::Schema(schema_req)) => {
                let plan = schema_req
                    .plan
                    .ok_or_else(|| Status::invalid_argument("Schema analyze missing plan"))?;
                let relation = match plan.op_type {
                    Some(proto::plan::OpType::Root(r)) => r,
                    _ => {
                        return Err(Status::invalid_argument("Schema analyze requires root plan"));
                    }
                };
                let session = self
                    .session_manager
                    .get_or_create(&session_id)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?;
                let logical_plan =
                    PlanConverter::convert_relation_with_session(&relation, Arc::clone(&session))
                        .map_err(Status::from)?;
                let mut struct_type = logical_plan.infer_schema();
                let has_unresolved = struct_type.fields.iter()
                    .any(|f| f.data_type == thunderduck_core::types::DataType::Unresolved);
                if struct_type.is_empty() || has_unresolved {
                    // Static inference failed or produced Unresolved types — ask DuckDB
                    let sql = SqlGenerator::relaxed()
                        .generate(&logical_plan)
                        .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;
                    struct_type = SchemaInferrer::new(&session)
                        .infer_sql(&sql)
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                }
                let schema_proto = data_type_to_proto(&DataType::Struct(struct_type));

                let resp = proto::AnalyzePlanResponse {
                    session_id,
                    server_side_session_id: SERVER_SESSION_ID.to_string(),
                    result: Some(proto::analyze_plan_response::Result::Schema(
                        proto::analyze_plan_response::Schema { schema: Some(schema_proto) },
                    )),
                };
                Ok(Response::new(resp))
            }
            _ => Err(Status::unimplemented("Only Schema analyze_plan is supported")),
        }
    }

    async fn config(
        &self,
        request: Request<proto::ConfigRequest>,
    ) -> Result<Response<proto::ConfigResponse>, Status> {
        let req = request.into_inner();
        use proto::config_request::operation::OpType;
        let pairs = match req.operation.and_then(|op| op.op_type) {
            Some(OpType::Get(g)) => {
                // Return Spark defaults for known integer/boolean configs that PySpark
                // calls int() or bool() on. Unknown keys get empty string (safe for str usage).
                g.keys.into_iter().map(|k| {
                    let v = spark_config_default(&k).to_string();
                    proto::KeyValue { key: k, value: Some(v) }
                }).collect()
            }
            Some(OpType::GetWithDefault(gd)) => {
                // Return the provided default for each key
                gd.pairs
            }
            Some(OpType::GetOption(_)) | Some(OpType::GetAll(_)) => vec![],
            Some(OpType::IsModifiable(im)) => {
                im.keys.into_iter().map(|k| proto::KeyValue { key: k, value: Some("true".to_string()) }).collect()
            }
            // Set / Unset — acknowledge with empty pairs
            _ => vec![],
        };
        Ok(Response::new(proto::ConfigResponse {
            session_id: req.session_id,
            server_side_session_id: SERVER_SESSION_ID.to_string(),
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
            server_side_session_id: SERVER_SESSION_ID.to_string(),
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

// ── Helpers ────────────────────────────────────────────────────────────────────

async fn handle_command(
    session: &thunderduck_core::runtime::DuckDbSession,
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
            let logical_plan =
                PlanConverter::convert_relation(&relation).map_err(Status::from)?;
            let generator = SqlGenerator::relaxed();
            let sql = generator
                .generate(&logical_plan)
                .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;
            session
                .create_temp_view(&view_cmd.name, &sql)
                .await
                .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;

            Ok(vec![
                sql_command_result_response(session_id, operation_id),
                result_complete_response(session_id, operation_id),
            ])
        }
        Some(CommandType::SqlCommand(sql_cmd)) => {
            let sql = if let Some(input_rel) = sql_cmd.input {
                let logical_plan =
                    PlanConverter::convert_relation(&input_rel).map_err(Status::from)?;
                SqlGenerator::relaxed()
                    .generate(&logical_plan)
                    .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?
            } else if !sql_cmd.sql.is_empty() {
                // PySpark 4.x sends spark.sql() via the deprecated sql text field
                sql_cmd.sql.clone()
            } else {
                return Err(Status::invalid_argument(
                    "SqlCommand missing both input relation and sql text",
                ));
            };
            let batches = session
                .execute(&sql)
                .await
                .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
            batches_to_responses(session_id, operation_id, &batches).map_err(Status::from)
        }
        Some(CommandType::WriteOperation(write_cmd)) => {
            use proto::write_operation::SaveType;
            let input_rel = write_cmd
                .input
                .ok_or_else(|| Status::invalid_argument("WriteOperation missing input"))?;
            let logical_plan =
                PlanConverter::convert_relation(&input_rel).map_err(Status::from)?;
            let select_sql = SqlGenerator::relaxed()
                .generate(&logical_plan)
                .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;

            let path = match write_cmd.save_type {
                Some(SaveType::Path(p)) => p,
                _ => return Err(Status::invalid_argument("WriteOperation requires a path destination")),
            };

            // Determine format from the source proto field
            let format = match write_cmd.source.as_deref() {
                Some("parquet") => "PARQUET",
                Some("csv") => "CSV",
                Some("json") => "JSON",
                _ => "PARQUET",
            };

            let copy_sql = if format == "CSV" {
                format!("COPY ({select_sql}) TO '{path}' (FORMAT CSV, HEADER)")
            } else {
                format!("COPY ({select_sql}) TO '{path}' (FORMAT {format})")
            };

            session
                .execute(&copy_sql)
                .await
                .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;

            Ok(vec![
                sql_command_result_response(session_id, operation_id),
                result_complete_response(session_id, operation_id),
            ])
        }
        _ => Err(Status::unimplemented("Unsupported command type")),
    }
}

/// Convert DuckDB record batches to a complete `ExecutePlanResponse` sequence,
/// including the mandatory trailing `ResultComplete` frame.
/// Execute an ApproxQuantile plan: for each column, compute approx_quantile for each
/// probability, then build a RecordBatch with schema `list<list<double>>`, 1 row.
///
/// PySpark Connect client reads: `table[0][0]` (first cell) = ListScalar of N inner lists,
/// where N = number of input columns and each inner list has M doubles (one per probability).
async fn execute_approx_quantile(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    aq: &thunderduck_core::logical::ApproxQuantile,
) -> Result<arrow::record_batch::RecordBatch, String> {
    use arrow::array::{Float64Array, ListArray};
    use arrow::buffer::OffsetBuffer;
    use arrow::datatypes::{DataType as ArrowDataType, Field, Schema};
    use arrow::record_batch::RecordBatch;

    // Generate SQL for the input sub-plan.
    let gen = SqlGenerator::relaxed();
    let input_sql = gen
        .generate(&aq.input)
        .map_err(|e| format!("approx_quantile input gen: {e}"))?;

    let n_probs = aq.probabilities.len();
    let n_cols = aq.cols.len();

    // Collect quantile values: layout = [col0_p0, col0_p1, ..., col1_p0, col1_p1, ...]
    let mut all_values: Vec<f64> = Vec::with_capacity(n_cols * n_probs);
    for col in &aq.cols {
        let quoted = format!("\"{}\"", col.replace('"', "\"\""));
        for p in &aq.probabilities {
            let sql = format!(
                "SELECT approx_quantile({quoted}, {p:.17}) AS __q FROM ({input_sql}) __aq_input__"
            );
            let batches = session.execute(&sql).await.map_err(|e| e.to_string())?;
            let val = batches
                .iter()
                .flat_map(|b| b.columns())
                .next()
                .and_then(|col| {
                    use arrow::array::{Array, Float64Array};
                    col.as_any()
                        .downcast_ref::<Float64Array>()
                        .and_then(|a| if a.len() > 0 { Some(a.value(0)) } else { None })
                })
                .unwrap_or(f64::NAN);
            all_values.push(val);
        }
    }

    // Build inner ListArray: N entries (one per column), each with M doubles.
    let values_array = Arc::new(Float64Array::from(all_values));
    let inner_offsets: Vec<i32> = (0..=(n_cols as i32))
        .map(|i| i * n_probs as i32)
        .collect();
    let inner_offsets_buf = OffsetBuffer::new(inner_offsets.into());
    let float_field = Arc::new(Field::new("item", ArrowDataType::Float64, true));
    let inner_list_array = ListArray::new(float_field.clone(), inner_offsets_buf, values_array, None);

    // Build outer ListArray: 1 entry containing all N inner lists.
    // This is the single-row table cell the PySpark client expects.
    let outer_offsets: Vec<i32> = vec![0, n_cols as i32];
    let outer_offsets_buf = OffsetBuffer::new(outer_offsets.into());
    let inner_list_type = ArrowDataType::List(float_field);
    let inner_field = Arc::new(Field::new("item", inner_list_type.clone(), true));
    let outer_list_array =
        ListArray::new(inner_field.clone(), outer_offsets_buf, Arc::new(inner_list_array), None);

    let schema = Arc::new(Schema::new(vec![Field::new(
        "quantiles",
        ArrowDataType::List(inner_field),
        true,
    )]));
    RecordBatch::try_new(schema, vec![Arc::new(outer_list_array)])
        .map_err(|e| format!("approx_quantile batch: {e}"))
}

fn batches_to_responses(
    session_id: &str,
    operation_id: &str,
    batches: &[arrow::record_batch::RecordBatch],
) -> crate::error::Result<Vec<proto::ExecutePlanResponse>> {
    let arrow_batches = record_batches_to_arrow_batches(batches)?;
    let mut responses: Vec<proto::ExecutePlanResponse> = arrow_batches
        .into_iter()
        .enumerate()
        .map(|(i, ab)| proto::ExecutePlanResponse {
            session_id: session_id.to_string(),
            server_side_session_id: SERVER_SESSION_ID.to_string(),
            operation_id: operation_id.to_string(),
            response_id: format!("{operation_id}-{i}"),
            response_type: Some(proto::execute_plan_response::ResponseType::ArrowBatch(ab)),
            ..Default::default()
        })
        .collect();
    // Send ResultComplete even when there are no batches (0 rows).
    // Do NOT push an empty ArrowBatch (data: vec![]) — empty bytes are invalid Arrow IPC
    // and PySpark raises ArrowInvalid when it tries to deserialize them.
    responses.push(result_complete_response(session_id, operation_id));
    Ok(responses)
}

/// Create an ArrowBatch response with a single boolean `value` column = `val`.
/// Used for DDL operations (DropTempView etc.) that must return a non-null table.
fn bool_batch_responses(
    session_id: &str,
    operation_id: &str,
    val: bool,
) -> crate::error::Result<Vec<proto::ExecutePlanResponse>> {
    use arrow::array::BooleanArray;
    use arrow::datatypes::{Field, Schema};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;
    let schema = Arc::new(Schema::new(vec![Field::new("value", arrow::datatypes::DataType::Boolean, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(BooleanArray::from(vec![val]))],
    ).map_err(|e| crate::error::ConnectError::Arrow(e.to_string()))?;
    batches_to_responses(session_id, operation_id, &[batch])
}

fn empty_result_response(session_id: &str, operation_id: &str) -> proto::ExecutePlanResponse {
    proto::ExecutePlanResponse {
        session_id: session_id.to_string(),
        server_side_session_id: SERVER_SESSION_ID.to_string(),
        operation_id: operation_id.to_string(),
        response_id: format!("{operation_id}-0"),
        response_type: Some(proto::execute_plan_response::ResponseType::ArrowBatch(
            proto::execute_plan_response::ArrowBatch { row_count: 0, data: vec![], ..Default::default() },
        )),
        ..Default::default()
    }
}

fn result_complete_response(session_id: &str, operation_id: &str) -> proto::ExecutePlanResponse {
    proto::ExecutePlanResponse {
        session_id: session_id.to_string(),
        server_side_session_id: SERVER_SESSION_ID.to_string(),
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
) -> proto::ExecutePlanResponse {
    proto::ExecutePlanResponse {
        session_id: session_id.to_string(),
        server_side_session_id: SERVER_SESSION_ID.to_string(),
        operation_id: operation_id.to_string(),
        response_id: format!("{operation_id}-cmd"),
        response_type: Some(
            proto::execute_plan_response::ResponseType::SqlCommandResult(
                proto::execute_plan_response::SqlCommandResult { relation: None },
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
