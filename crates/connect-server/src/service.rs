use std::pin::Pin;
use std::sync::Arc;

use futures::stream;
use thunderduck_core::generator::SqlGenerator;
use thunderduck_core::logical::LogicalPlan;
use thunderduck_core::runtime::{SchemaInferrer, SessionManager, StreamBatch};
use thunderduck_core::types::DataType;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::arrow_ipc::{record_batch_to_arrow_batch, record_batches_to_arrow_batches};
use crate::converter::type_converter::data_type_to_proto;
use crate::converter::PlanConverter;
use crate::error::ConnectError;
use crate::proto::spark::connect as proto;
use crate::proto::spark::connect::spark_connect_service_server::SparkConnectService;

type BoxStream<T> = Pin<Box<dyn futures::Stream<Item = Result<T, Status>> + Send + 'static>>;

/// Maximum allowed depth of a logical plan tree. Plans deeper than this are
/// rejected to prevent stack overflow from deeply nested plans (e.g., from
/// malicious clients).
const MAX_PLAN_DEPTH: usize = 256;

pub struct ThunderduckService {
    session_manager: Arc<SessionManager>,
}

impl ThunderduckService {
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self { session_manager }
    }
}

/// Translate a [`LogicalPlan`] into DuckDB SQL via the legacy [`SqlGenerator`].
///
/// The v2 dispatch machinery was removed in the 2026-07-02 restart (tag
/// `v2-morph-track-end` preserves the discarded implementation). Slice A
/// of the restart re-introduces v2 dispatch at the protobuf boundary per
/// ADR-021; until then, legacy is the sole active path.
fn generate_sql(plan: &LogicalPlan) -> Result<String, Status> {
    SqlGenerator::new()
        .generate(plan)
        .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))
}

static SERVER_SESSION_ID: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| uuid::Uuid::new_v4().to_string());

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
                let logical_plan =
                    PlanConverter::convert_relation_with_session(&relation, Arc::clone(&session))
                        .map_err(Status::from)?;

                let plan_depth = logical_plan.depth();
                if plan_depth > MAX_PLAN_DEPTH {
                    return Err(Status::invalid_argument(format!(
                        "Plan tree depth {plan_depth} exceeds maximum {MAX_PLAN_DEPTH}"
                    )));
                }

                // Special case: ApproxQuantile needs a ListArray response.
                if let thunderduck_core::logical::LogicalPlan::ApproxQuantile(ref aq) = logical_plan
                {
                    let batch = execute_approx_quantile(&session, aq)
                        .await
                        .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
                    let responses = batches_to_responses(&session_id, &operation_id, &[batch])
                        .map_err(|e| Status::from(e))?;
                    let stream = stream::iter(responses.into_iter().map(Ok));
                    return Ok(Response::new(Box::pin(stream)));
                }

                let sql = generate_sql(&logical_plan)?;

                match classify_plan(&logical_plan) {
                    PlanKind::Ddl(ddl) => {
                        execute_ddl(&session, ddl, &sql, &session_id, &operation_id).await?
                    }
                    PlanKind::Query => {
                        return execute_streaming_query(
                            &session,
                            &logical_plan,
                            &sql,
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
                let session = self
                    .session_manager
                    .get_or_create(&session_id)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?;
                let logical_plan =
                    PlanConverter::convert_relation_with_session(&relation, Arc::clone(&session))
                        .map_err(Status::from)?;

                let plan_depth = logical_plan.depth();
                if plan_depth > MAX_PLAN_DEPTH {
                    return Err(Status::invalid_argument(format!(
                        "Plan tree depth {plan_depth} exceeds maximum {MAX_PLAN_DEPTH}"
                    )));
                }

                let mut struct_type = logical_plan.infer_schema();
                let has_unresolved = struct_type
                    .fields
                    .iter()
                    .any(|f| f.data_type.contains_unresolved());
                if struct_type.is_empty() || has_unresolved || logical_plan.has_partial_schema() {
                    // Static inference failed or produced Unresolved types — ask DuckDB
                    // for the actual column types/nullability.
                    let sql = generate_sql(&logical_plan)?;
                    let duckdb_schema = SchemaInferrer::new(&session)
                        .infer_sql(&sql)
                        .await
                        .map_err(|e| Status::internal(e.to_string()))?;
                    // DuckDB auto-renames duplicate column names by appending `_1`, `_2`, etc.
                    // (e.g. a self-join produces `col`, `col_1`). Spark allows duplicate names.
                    // When the static inference produced names but not types (has_unresolved),
                    // use the Spark-expected names with DuckDB's types so the schema is correct.
                    use thunderduck_core::types::{StructField, StructType};
                    struct_type = if struct_type.is_empty() {
                        // No plan-level schema at all — use DuckDB entirely.
                        // For Pivot plans (possibly wrapped in Sort/Project/Filter):
                        // DuckDB reports all columns as nullable, but grouping columns
                        // inherit nullability from the input.
                        {
                            let pivot_overrides = logical_plan.pivot_grouping_nullable_overrides();
                            if pivot_overrides.is_empty() {
                                duckdb_schema
                            } else {
                                StructType::new(
                                    duckdb_schema
                                        .fields
                                        .into_iter()
                                        .map(|mut f| {
                                            for (name, nullable) in &pivot_overrides {
                                                if name.eq_ignore_ascii_case(&f.name) {
                                                    f.nullable = *nullable;
                                                    break;
                                                }
                                            }
                                            f
                                        })
                                        .collect(),
                                )
                            }
                        }
                    } else if struct_type.fields.len() == duckdb_schema.fields.len() {
                        // Same field count: position-based merge.
                        // Use Spark type/nullability when resolved, DuckDB otherwise.
                        // This preserves precise Spark semantics (e.g. IntegerType for row_number,
                        // levenshtein, cardinality) instead of DuckDB's wider types (BIGINT).
                        let fields = struct_type
                            .fields
                            .iter()
                            .zip(duckdb_schema.fields.iter())
                            .map(|(spark_f, duck_f)| {
                                if spark_f.data_type.contains_unresolved() {
                                    StructField {
                                        name: spark_f.name.clone(),
                                        data_type: duck_f.data_type.clone(),
                                        nullable: spark_f.nullable,
                                    }
                                } else {
                                    StructField {
                                        name: spark_f.name.clone(),
                                        data_type: spark_f.data_type.clone(),
                                        nullable: spark_f.nullable,
                                    }
                                }
                            })
                            .collect();
                        StructType::new(fields)
                    } else {
                        // Size mismatch (e.g. star expansion with unknown child schema):
                        // Use DuckDB schema as base, override by name for any explicitly-typed
                        // Spark fields (e.g. row_number → Integer, not DuckDB's BIGINT).
                        let spark_map: std::collections::HashMap<String, &StructField> =
                            struct_type
                                .fields
                                .iter()
                                .filter(|f| !f.data_type.contains_unresolved())
                                .map(|f| (f.name.to_lowercase(), f))
                                .collect();
                        let fields = duckdb_schema
                            .fields
                            .iter()
                            .map(|duck_f| {
                                if let Some(spark_f) = spark_map.get(&duck_f.name.to_lowercase()) {
                                    StructField {
                                        name: duck_f.name.clone(),
                                        data_type: spark_f.data_type.clone(),
                                        nullable: spark_f.nullable,
                                    }
                                } else {
                                    duck_f.clone()
                                }
                            })
                            .collect();
                        StructType::new(fields)
                    };
                    // Post-merge: for Pivot plans, override grouping column nullable
                    // from input schema (covers both empty and has_unresolved branches)
                    {
                        let pivot_overrides = logical_plan.pivot_grouping_nullable_overrides();
                        if !pivot_overrides.is_empty() {
                            struct_type = StructType::new(
                                struct_type
                                    .fields
                                    .into_iter()
                                    .map(|mut f| {
                                        for (name, nullable) in &pivot_overrides {
                                            if name.eq_ignore_ascii_case(&f.name) {
                                                f.nullable = *nullable;
                                                break;
                                            }
                                        }
                                        f
                                    })
                                    .collect(),
                            );
                        }
                    }
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

// ── Helpers ────────────────────────────────────────────────────────────────────

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
            let logical_plan =
                PlanConverter::convert_relation_with_session(&relation, Arc::clone(session))
                    .map_err(Status::from)?;
            let sql = generate_sql(&logical_plan)?;

            // Infer schema from the input relation and cache it so downstream
            // reads preserve Spark-declared nullable flags that DuckDB's
            // CREATE VIEW loses.
            let schema = logical_plan.infer_schema();
            if !schema.is_empty() {
                session
                    .create_temp_view_with_schema(&view_cmd.name, &sql, schema)
                    .await
                    .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
            } else {
                session
                    .create_temp_view(&view_cmd.name, &sql)
                    .await
                    .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
            }

            Ok(vec![
                sql_command_result_response(session_id, operation_id),
                result_complete_response(session_id, operation_id),
            ])
        }
        Some(CommandType::SqlCommand(sql_cmd)) => {
            let (sql, logical_plan) = if let Some(input_rel) = sql_cmd.input {
                let plan = PlanConverter::convert_relation(&input_rel).map_err(Status::from)?;
                let sql = generate_sql(&plan)?;
                (sql, Some(plan))
            } else {
                // Fallback: older clients send spark.sql() via the deprecated `sql` text field
                // (all SqlCommand text fields are proto-deprecated in favour of `input`)
                #[allow(deprecated)]
                let text = sql_cmd.sql.clone();
                if text.is_empty() {
                    return Err(Status::invalid_argument(
                        "SqlCommand missing both input relation and sql text",
                    ));
                }
                (text, None)
            };
            // Cache CREATE VIEW schema before execution so subsequent
            // spark.table() calls get correct nullable metadata.
            if let Some(ref plan) = logical_plan {
                cache_create_view_schema(session, plan).await;
            }
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
            let logical_plan = PlanConverter::convert_relation(&input_rel).map_err(Status::from)?;
            let select_sql = generate_sql(&logical_plan)?;

            let path = match write_cmd.save_type {
                Some(SaveType::Path(p)) => p,
                _ => {
                    return Err(Status::invalid_argument(
                        "WriteOperation requires a path destination",
                    ))
                }
            };

            // Determine format from the source proto field
            let format = match write_cmd.source.as_deref() {
                Some("parquet") => "PARQUET",
                Some("csv") => "CSV",
                Some("json") => "JSON",
                _ => "PARQUET",
            };

            let escaped_path = path.replace('\'', "''");
            let copy_sql = if format == "CSV" {
                format!("COPY ({select_sql}) TO '{escaped_path}' (FORMAT CSV, HEADER)")
            } else {
                format!("COPY ({select_sql}) TO '{escaped_path}' (FORMAT {format})")
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
    use arrow::array::{Array, Float64Array, ListArray};
    use arrow::buffer::OffsetBuffer;
    use arrow::datatypes::{DataType as ArrowDataType, Field, Schema};
    use arrow::record_batch::RecordBatch;

    // Generate SQL for the input sub-plan via the selected transpiler.
    let input_sql = generate_sql(&aq.input)
        .map_err(|s| format!("approx_quantile input gen: {}", s.message()))?;

    let n_probs = aq.probabilities.len();
    let n_cols = aq.cols.len();

    // Build a single query with all column×probability combinations.
    // Layout: col0_p0, col0_p1, ..., col1_p0, col1_p1, ...
    let mut select_exprs: Vec<String> = Vec::with_capacity(n_cols * n_probs);
    for (ci, col) in aq.cols.iter().enumerate() {
        let quoted = format!("\"{}\"", col.replace('"', "\"\""));
        for (pi, p) in aq.probabilities.iter().enumerate() {
            select_exprs.push(format!(
                "approx_quantile({quoted}, {p:.17}) AS __q_{ci}_{pi}"
            ));
        }
    }

    let sql = format!(
        "SELECT {} FROM ({input_sql}) __aq_input__",
        select_exprs.join(", ")
    );
    let batches = session.execute(&sql).await.map_err(|e| e.to_string())?;

    // Extract all N×M float64 values from the single-row result.
    let result_batch = batches
        .first()
        .ok_or_else(|| "approx_quantile: empty result".to_owned())?;
    if result_batch.num_rows() == 0 {
        return Err("approx_quantile: result has zero rows".to_owned());
    }

    let total = n_cols * n_probs;
    let mut all_values: Vec<f64> = Vec::with_capacity(total);
    for idx in 0..total {
        let col_arr = result_batch.column(idx);
        let val = col_arr
            .as_any()
            .downcast_ref::<Float64Array>()
            .and_then(|a| {
                if !a.is_empty() {
                    Some(a.value(0))
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                let ci = idx / n_probs;
                let pi = idx % n_probs;
                format!(
                    "approx_quantile: no float64 result for column {} at p={}",
                    aq.cols[ci], aq.probabilities[pi]
                )
            })?;
        all_values.push(val);
    }

    // Build inner ListArray: N entries (one per column), each with M doubles.
    let values_array = Arc::new(Float64Array::from(all_values));
    let n_cols_i32 =
        i32::try_from(n_cols).map_err(|_| "too many columns for approx_quantile".to_owned())?;
    let n_probs_i32 = i32::try_from(n_probs)
        .map_err(|_| "too many probabilities for approx_quantile".to_owned())?;
    let inner_offsets: Vec<i32> = (0..=n_cols_i32).map(|i| i * n_probs_i32).collect();
    let inner_offsets_buf = OffsetBuffer::new(inner_offsets.into());
    let float_field = Arc::new(Field::new("item", ArrowDataType::Float64, true));
    let inner_list_array =
        ListArray::new(float_field.clone(), inner_offsets_buf, values_array, None);

    // Build outer ListArray: 1 entry containing all N inner lists.
    // This is the single-row table cell the PySpark client expects.
    let outer_offsets: Vec<i32> = vec![0, n_cols_i32];
    let outer_offsets_buf = OffsetBuffer::new(outer_offsets.into());
    let inner_list_type = ArrowDataType::List(float_field);
    let inner_field = Arc::new(Field::new("item", inner_list_type.clone(), true));
    let outer_list_array = ListArray::new(
        inner_field.clone(),
        outer_offsets_buf,
        Arc::new(inner_list_array),
        None,
    );

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
            server_side_session_id: SERVER_SESSION_ID.clone(),
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

fn sql_command_result_response(session_id: &str, operation_id: &str) -> proto::ExecutePlanResponse {
    proto::ExecutePlanResponse {
        session_id: session_id.to_string(),
        server_side_session_id: SERVER_SESSION_ID.clone(),
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

/// If `plan` is a `SqlRelation` with a `view_name` (i.e., a CREATE VIEW DDL),
/// cache the inner query's plan-inferred schema. DuckDB views lose NOT NULL
/// metadata, so this preserves correct nullable flags for subsequent
/// `spark.table()` calls.
async fn cache_create_view_schema(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    plan: &thunderduck_core::logical::LogicalPlan,
) {
    // Extract view_name and schema from either legacy SqlRelation or new DdlStatement.
    let (view_name, schema) = match plan {
        thunderduck_core::logical::LogicalPlan::SqlRelation(sr) => {
            match (&sr.view_name, &sr.schema) {
                (Some(name), schema) => (name.as_str(), schema),
                _ => return,
            }
        }
        thunderduck_core::logical::LogicalPlan::DdlStatement(d) => match &d.operation {
            thunderduck_core::logical::DdlOperation::CreateView {
                view_name, schema, ..
            } => (view_name.as_str(), schema),
            _ => return,
        },
        _ => return,
    };
    if schema.is_empty() {
        return;
    }

    // If schema has unresolved types, merge with DuckDB schema for types
    // but preserve plan-level nullability where resolved.
    let final_schema = if schema
        .fields
        .iter()
        .any(|f| f.data_type.contains_unresolved())
    {
        use thunderduck_core::types::{StructField, StructType};
        let duckdb_schema = match SchemaInferrer::new(session)
            .infer_sql(&format!(
                "SELECT * FROM \"{}\"",
                view_name.replace('"', "\"\"")
            ))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(view_name, error = %e, "Failed to cache view schema: DuckDB schema inference failed");
                return;
            }
        };
        if schema.fields.len() == duckdb_schema.fields.len() {
            let fields = schema
                .fields
                .iter()
                .zip(duckdb_schema.fields.iter())
                .map(|(plan_f, duck_f)| {
                    if plan_f.data_type.contains_unresolved() {
                        StructField {
                            name: plan_f.name.clone(),
                            data_type: duck_f.data_type.clone(),
                            nullable: duck_f.nullable,
                        }
                    } else {
                        plan_f.clone()
                    }
                })
                .collect();
            StructType::new(fields)
        } else {
            tracing::warn!(
                view_name,
                "Failed to cache view schema: field count mismatch between plan and DuckDB"
            );
            return;
        }
    } else {
        schema.clone()
    };

    session.cache_view_schema(view_name, final_schema).await;
}

// ── Plan classification and decomposed execution ──────────────────────────────

/// Classify a logical plan for execution routing.
enum PlanKind<'a> {
    /// A DDL/DML statement — execute without result streaming.
    Ddl(&'a thunderduck_core::logical::DdlStatement),
    /// A query — stream results back to the client.
    Query,
}

/// Classify a plan as DDL or query.
fn classify_plan(plan: &thunderduck_core::logical::LogicalPlan) -> PlanKind<'_> {
    match plan {
        thunderduck_core::logical::LogicalPlan::DdlStatement(d) => PlanKind::Ddl(d),
        _ => PlanKind::Query,
    }
}

/// Execute a DDL statement and return appropriate responses.
///
/// For DROP VIEW, synthesizes a boolean result indicating whether the view
/// existed before the drop. For all other DDL, executes silently and returns
/// a boolean `true` result.
async fn execute_ddl(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    ddl: &thunderduck_core::logical::DdlStatement,
    sql: &str,
    session_id: &str,
    operation_id: &str,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    use thunderduck_core::logical::DdlOperation;

    // Cache schema for CREATE VIEW before executing.
    match &ddl.operation {
        DdlOperation::CreateView {
            view_name, schema, ..
        } if !schema.is_empty() => {
            // Execute DDL first so the view exists when we merge schemas.
            session
                .exec_ddl(sql)
                .await
                .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
            cache_create_view_schema_direct(session, view_name, schema).await;
            return bool_batch_responses(session_id, operation_id, true).map_err(Status::from);
        }
        _ => {}
    }

    match &ddl.operation {
        DdlOperation::DropView { view_name, .. } => {
            let existed = session.view_exists(view_name).await;
            session
                .exec_ddl(sql)
                .await
                .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
            bool_batch_responses(session_id, operation_id, existed).map_err(Status::from)
        }
        DdlOperation::DropTable { .. }
        | DdlOperation::CreateView { .. }
        | DdlOperation::CreateTable { .. }
        | DdlOperation::AlterTable { .. }
        | DdlOperation::Truncate { .. }
        | DdlOperation::Insert { .. }
        | DdlOperation::Other { .. } => {
            session
                .exec_ddl(sql)
                .await
                .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
            bool_batch_responses(session_id, operation_id, true).map_err(Status::from)
        }
    }
}

/// Cache a CREATE VIEW schema, merging with DuckDB when plan types are unresolved.
async fn cache_create_view_schema_direct(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    view_name: &str,
    schema: &thunderduck_core::types::StructType,
) {
    use thunderduck_core::types::{StructField, StructType};

    let final_schema = if schema
        .fields
        .iter()
        .any(|f| f.data_type.contains_unresolved())
    {
        let duckdb_schema = match SchemaInferrer::new(session)
            .infer_sql(&format!(
                "SELECT * FROM \"{}\"",
                view_name.replace('"', "\"\"")
            ))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(view_name, error = %e, "Failed to cache view schema (direct): DuckDB schema inference failed");
                return;
            }
        };
        if schema.fields.len() == duckdb_schema.fields.len() {
            let fields = schema
                .fields
                .iter()
                .zip(duckdb_schema.fields.iter())
                .map(|(plan_f, duck_f)| {
                    if plan_f.data_type.contains_unresolved() {
                        StructField {
                            name: plan_f.name.clone(),
                            data_type: duck_f.data_type.clone(),
                            nullable: duck_f.nullable,
                        }
                    } else {
                        plan_f.clone()
                    }
                })
                .collect();
            StructType::new(fields)
        } else {
            tracing::warn!(view_name, "Failed to cache view schema (direct): field count mismatch between plan and DuckDB");
            return;
        }
    } else {
        schema.clone()
    };

    session.cache_view_schema(view_name, final_schema).await;
}

/// Execute a query plan and stream results back to the client.
async fn execute_streaming_query(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    logical_plan: &thunderduck_core::logical::LogicalPlan,
    sql: &str,
    session_id: &str,
    operation_id: &str,
) -> Result<Response<<ThunderduckService as SparkConnectService>::ExecutePlanStream>, Status> {
    // Compute Spark column names for rename (pushed to session thread).
    let schema = logical_plan.infer_schema();
    let spark_names = if !schema.is_empty() {
        Some(
            schema
                .fields
                .iter()
                .map(|f| f.name.clone())
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };

    let rx = session
        .execute_streaming(sql, spark_names, 2)
        .await
        .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;

    let sid = session_id.to_string();
    let oid = operation_id.to_string();

    let stream = futures::stream::unfold(
        (rx, sid, oid, 0usize, false),
        |(mut rx, sid, oid, idx, done)| async move {
            if done {
                return None;
            }
            match rx.recv().await {
                Some(StreamBatch::Batch(batch)) => {
                    let resp = match record_batch_to_arrow_batch(&batch) {
                        Ok(arrow_batch) => Ok(proto::ExecutePlanResponse {
                            session_id: sid.clone(),
                            server_side_session_id: SERVER_SESSION_ID.clone(),
                            operation_id: oid.clone(),
                            response_id: format!("{oid}-{idx}"),
                            response_type: Some(
                                proto::execute_plan_response::ResponseType::ArrowBatch(arrow_batch),
                            ),
                            ..Default::default()
                        }),
                        Err(e) => Err(Status::internal(format!("IPC serialization: {e}"))),
                    };
                    Some((resp, (rx, sid, oid, idx + 1, false)))
                }
                Some(StreamBatch::Complete) => {
                    let resp = Ok(proto::ExecutePlanResponse {
                        session_id: sid.clone(),
                        server_side_session_id: SERVER_SESSION_ID.clone(),
                        operation_id: oid.clone(),
                        response_id: format!("{oid}-complete"),
                        response_type: Some(
                            proto::execute_plan_response::ResponseType::ResultComplete(
                                proto::execute_plan_response::ResultComplete::default(),
                            ),
                        ),
                        ..Default::default()
                    });
                    Some((resp, (rx, sid, oid, idx + 1, true)))
                }
                Some(StreamBatch::Error(e)) => {
                    Some((Err(Status::internal(e)), (rx, sid, oid, idx + 1, true)))
                }
                None => Some((
                    Err(Status::internal("session thread terminated unexpectedly")),
                    (rx, sid, oid, idx + 1, true),
                )),
            }
        },
    );

    Ok(Response::new(Box::pin(stream)))
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
}
