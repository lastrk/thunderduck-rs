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

                let sql = SqlGenerator::new(session.mode())
                    .generate(&logical_plan)
                    .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;

                // Special case: CREATE VIEW DDL — cache the inner query's schema
                // so that subsequent `spark.table()` calls get correct nullable
                // metadata. DuckDB views lose NOT NULL on all columns.
                cache_create_view_schema(&session, &logical_plan).await;

                // Special case: DDL statements (DROP VIEW, etc.) return 0 rows.
                // Execute as DDL and synthesize a single boolean result row so that
                // PySpark's _execute_and_fetch gets a non-null table back.
                // For DROP VIEW, the bool indicates whether the view existed.
                let upper = sql.trim_start().to_uppercase();
                const DROP_VIEW_PREFIX: &str = "DROP VIEW IF EXISTS ";
                if upper.starts_with(DROP_VIEW_PREFIX) {
                    // Extract view name — skip past the prefix (ASCII, same byte length in upper/original)
                    let view_name = sql.trim_start()[DROP_VIEW_PREFIX.len()..].trim().trim_matches('"');
                    // Check existence before dropping
                    let exists = session
                        .execute(&format!(
                            "SELECT COUNT(*) > 0 AS existed \
                             FROM information_schema.views \
                             WHERE table_name = '{}' AND table_schema = 'main'",
                            view_name.replace('\'', "''").replace('"', "\"\"")
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
                    session.exec_ddl(&sql).await
                        .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
                    bool_batch_responses(&session_id, &operation_id, exists)
                        .map_err(|e| Status::from(e))?
                } else {
                    let batches = session
                        .execute(&sql)
                        .await
                        .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
                    // DuckDB deduplicates duplicate column names by appending `_1`, `_2`, etc.
                    // Spark allows duplicate column names and does not rename them.
                    // Use the LogicalPlan's infer_schema() to restore Spark-expected column names.
                    let batches = rename_to_spark_schema(&logical_plan, batches);
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
                    .any(|f| f.data_type.contains_unresolved());
                if struct_type.is_empty() || has_unresolved || logical_plan.has_partial_schema() {
                    // Static inference failed or produced Unresolved types — ask DuckDB
                    // for the actual column types/nullability.
                    let sql = SqlGenerator::new(session.mode())
                        .generate(&logical_plan)
                        .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;
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
                        if let Some(pivot) = find_pivot(&logical_plan) {
                            apply_pivot_grouping_nullable(duckdb_schema, pivot)
                        } else {
                            duckdb_schema
                        }
                    } else if struct_type.fields.len() == duckdb_schema.fields.len() {
                        // Same field count: position-based merge.
                        // Use Spark type/nullability when resolved, DuckDB otherwise.
                        // This preserves precise Spark semantics (e.g. IntegerType for row_number,
                        // levenshtein, cardinality) instead of DuckDB's wider types (BIGINT).
                        let fields = struct_type.fields.iter()
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
                            struct_type.fields.iter()
                                .filter(|f| !f.data_type.contains_unresolved())
                                .map(|f| (f.name.to_lowercase(), f))
                                .collect();
                        let fields = duckdb_schema.fields.iter()
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
            let sql = SqlGenerator::new(session.mode())
                .generate(&logical_plan)
                .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;

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
                let plan =
                    PlanConverter::convert_relation(&input_rel).map_err(Status::from)?;
                let sql = SqlGenerator::new(session.mode())
                    .generate(&plan)
                    .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;
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
            let logical_plan =
                PlanConverter::convert_relation(&input_rel).map_err(Status::from)?;
            let select_sql = SqlGenerator::new(session.mode())
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
    use arrow::array::{Float64Array, ListArray};
    use arrow::buffer::OffsetBuffer;
    use arrow::datatypes::{DataType as ArrowDataType, Field, Schema};
    use arrow::record_batch::RecordBatch;

    // Generate SQL for the input sub-plan.
    let gen = SqlGenerator::new(session.mode());
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
                .ok_or_else(|| format!("approx_quantile: no float64 result for column {col} at p={p}"))?;
            all_values.push(val);
        }
    }

    // Build inner ListArray: N entries (one per column), each with M doubles.
    let values_array = Arc::new(Float64Array::from(all_values));
    let n_cols_i32 = i32::try_from(n_cols)
        .map_err(|_| "too many columns for approx_quantile".to_owned())?;
    let n_probs_i32 = i32::try_from(n_probs)
        .map_err(|_| "too many probabilities for approx_quantile".to_owned())?;
    let inner_offsets: Vec<i32> = (0..=n_cols_i32)
        .map(|i| i * n_probs_i32)
        .collect();
    let inner_offsets_buf = OffsetBuffer::new(inner_offsets.into());
    let float_field = Arc::new(Field::new("item", ArrowDataType::Float64, true));
    let inner_list_array = ListArray::new(float_field.clone(), inner_offsets_buf, values_array, None);

    // Build outer ListArray: 1 entry containing all N inner lists.
    // This is the single-row table cell the PySpark client expects.
    let outer_offsets: Vec<i32> = vec![0, n_cols_i32];
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

/// Rename Arrow RecordBatch columns to match Spark's expected output column names.
///
/// DuckDB deduplicates identically-named output columns by appending `_1`, `_2`, etc.
/// (e.g. a self-join of a CTE produces `col`, `col_1`). Spark allows duplicate column names.
/// This function uses `plan.infer_schema()` to obtain the Spark-expected names and renames
/// the DuckDB result columns accordingly, but only when the field count matches exactly and
/// the inferred schema is non-empty (to avoid misidentifying unrelated schema mismatches).
fn rename_to_spark_schema(
    plan: &thunderduck_core::logical::LogicalPlan,
    batches: Vec<arrow::record_batch::RecordBatch>,
) -> Vec<arrow::record_batch::RecordBatch> {
    use arrow::datatypes::{Field as ArrowField, Schema};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;

    if batches.is_empty() {
        return batches;
    }
    let expected = plan.infer_schema();
    let actual_count = batches[0].num_columns();
    if expected.is_empty() {
        return batches;
    }
    if expected.fields.len() != actual_count {
        return batches;
    }
    // Check whether any column name differs
    let needs_rename = batches[0]
        .schema()
        .fields()
        .iter()
        .zip(expected.fields.iter())
        .any(|(actual_f, expected_f)| actual_f.name() != &expected_f.name);
    if !needs_rename {
        return batches;
    }
    // Rebuild schema with Spark-expected names but DuckDB's actual data types / nullability.
    let new_fields: Vec<ArrowField> = batches[0]
        .schema()
        .fields()
        .iter()
        .zip(expected.fields.iter())
        .map(|(actual_f, expected_f)| {
            ArrowField::new(
                expected_f.name.as_str(),
                actual_f.data_type().clone(),
                actual_f.is_nullable(),
            )
        })
        .collect();
    let new_schema = Arc::new(Schema::new(new_fields));
    batches
        .into_iter()
        .map(|b| {
            RecordBatch::try_new(Arc::clone(&new_schema), b.columns().to_vec()).unwrap_or_else(|e| {
                eprintln!("column rename failed: {e}");
                b
            })
        })
        .collect()
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

/// If `plan` is a `SqlRelation` with a `view_name` (i.e., a CREATE VIEW DDL),
/// cache the inner query's plan-inferred schema. DuckDB views lose NOT NULL
/// metadata, so this preserves correct nullable flags for subsequent
/// `spark.table()` calls.
async fn cache_create_view_schema(
    session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    plan: &thunderduck_core::logical::LogicalPlan,
) {
    let sr = match plan {
        thunderduck_core::logical::LogicalPlan::SqlRelation(sr) => sr,
        _ => return,
    };
    let view_name = match &sr.view_name {
        Some(name) => name,
        None => return,
    };
    let schema = &sr.schema;
    if schema.is_empty() {
        return;
    }

    // If schema has unresolved types, merge with DuckDB schema for types
    // but preserve plan-level nullability where resolved.
    let final_schema = if schema.fields.iter().any(|f| f.data_type.contains_unresolved()) {
        use thunderduck_core::types::{StructField, StructType};
        let duckdb_schema = match SchemaInferrer::new(session)
            .infer_sql(&format!(
                "SELECT * FROM \"{}\"",
                view_name.replace('"', "\"\"")
            ))
            .await
        {
            Ok(s) => s,
            Err(_) => return,
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
            return; // Size mismatch — skip caching
        }
    } else {
        schema.clone()
    };

    session.cache_view_schema(view_name, final_schema).await;
}

/// Find the innermost `Pivot` node in a plan tree.
///
/// Plans like `Sort(Project(Pivot(...)))` wrap the Pivot in non-schema-changing
/// operators. This walks the single-child "passthrough" nodes to find a Pivot.
fn find_pivot(plan: &thunderduck_core::logical::LogicalPlan) -> Option<&thunderduck_core::logical::Pivot> {
    use thunderduck_core::logical::LogicalPlan;
    match plan {
        LogicalPlan::Pivot(p) => Some(p),
        LogicalPlan::Sort(s) => find_pivot(&s.input),
        LogicalPlan::Limit(l) => find_pivot(&l.input),
        LogicalPlan::Tail(t) => find_pivot(&t.input),
        LogicalPlan::Filter(f) => find_pivot(&f.input),
        LogicalPlan::Project(p) => find_pivot(&p.input),
        LogicalPlan::WithColumns(ref w) => find_pivot(&w.input),
        LogicalPlan::Aggregate(ref a) => find_pivot(&a.input),
        _ => None,
    }
}

/// Override nullable flags for Pivot grouping columns in a DuckDB-derived schema.
///
/// DuckDB reports all columns as `nullable=true`, but grouping columns in a
/// `GROUP BY` (which Pivot uses internally) preserve the nullability of their
/// input. This patches the schema to match Spark's behavior.
fn apply_pivot_grouping_nullable(
    schema: thunderduck_core::types::StructType,
    pivot: &thunderduck_core::logical::Pivot,
) -> thunderduck_core::types::StructType {
    use thunderduck_core::expression::Expression;
    use thunderduck_core::types::StructType;

    let input_schema = pivot.input.infer_schema();
    if input_schema.is_empty() {
        return schema;
    }
    let grouping_names: Vec<String> = pivot.grouping.iter().filter_map(|e| {
        match e {
            Expression::ColumnReference(c) => Some(c.name.to_lowercase()),
            Expression::UnresolvedColumn(u) => Some(u.name.to_lowercase()),
            Expression::Alias(a) => Some(a.alias.to_lowercase()),
            _ => None,
        }
    }).collect();
    let fields = schema.fields.into_iter().map(|mut f| {
        if grouping_names.iter().any(|g| g.eq_ignore_ascii_case(&f.name)) {
            if let Some(input_f) = input_schema.field_by_name(&f.name) {
                f.nullable = input_f.nullable;
            }
        }
        f
    }).collect();
    StructType::new(fields)
}
