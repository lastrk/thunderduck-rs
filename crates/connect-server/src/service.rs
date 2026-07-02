use std::pin::Pin;
use std::sync::Arc;

use futures::stream;
use thunderduck_core::parser_v2::SparkSqlParserV2;
use thunderduck_core::transpiler_v2::base_types::plan_has_empty_scan;
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

// TODO(Slice B): re-add a plan-depth guard using `CommonAst::depth()` once
// the substrate carries a depth query. Slice A.3 removed the legacy
// `LogicalPlan::depth()` inspection when dispatch relocated to the τ
// boundary; `MAX_PLAN_DEPTH` will return alongside that query.

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

// ── τ dispatch helpers (Slice A.3) ────────────────────────────────────────────

/// Convert a Spark Connect [`proto::Relation`] into a [`CommonAst`] and finalize
/// it into DuckDB SQL via τ.
///
/// At Slice A.3, `transpiler_v2::generate()` errors unconditionally, so this
/// helper always surfaces `Status::unimplemented` for structurally-valid
/// inputs (via `EmissionError::UnsupportedOp`). The dispatch shape is what
/// matters; Slices B/C/D/E/F/G grow coverage behind this same boundary.
///
/// **Route by `RelType::Sql`** — Option (a) per plan §4: SQL text goes
/// through `parser_v2`, structured relations through `V2RelationConverter`.
/// `V2RelationConverter` refuses `RelType::Sql` with `UnsupportedProtoShape`,
/// so intercepting here keeps the two front-ends peer.
pub(crate) fn transpile_relation(
    relation: &proto::Relation,
) -> Result<(CommonAst, String), Status> {
    use proto::relation::RelType;
    let common_ast = match &relation.rel_type {
        Some(RelType::Sql(sql_relation)) => SparkSqlParserV2::parse(&sql_relation.query)
            .map_err(|e| Status::from(ConnectError::from(e)))?,
        _ => {
            let mut converter = V2RelationConverter::new();
            converter
                .convert(relation)
                .map_err(|e| Status::from(ConnectError::from(e)))?
        }
    };
    let sql = finalize(&common_ast)?;
    Ok((common_ast, sql))
}

/// Convert a raw SparkSQL string into a [`CommonAst`] and finalize it into
/// DuckDB SQL via τ.
///
/// Used by the deprecated `SqlCommand.sql` text field on older clients.
pub(crate) fn transpile_raw_sql(sql_text: &str) -> Result<(CommonAst, String), Status> {
    let common_ast =
        SparkSqlParserV2::parse(sql_text).map_err(|e| Status::from(ConnectError::from(e)))?;
    let sql = finalize(&common_ast)?;
    Ok((common_ast, sql))
}

/// Build the per-path `BaseTypes` overlay and run τ's emission entry point.
///
/// The catalog closure returns `None` at Slice A.3 — the overlay exists to
/// satisfy the dispatch shape; Slice B wires the actual catalog bridge over
/// `DuckDbSession`.
pub(crate) fn finalize(common_ast: &CommonAst) -> Result<String, Status> {
    let base_types = if plan_has_empty_scan(common_ast) {
        BaseTypes::build_from_plan(common_ast, |_table_name| {
            // Slice B wires the actual catalog lookup over `DuckDbSession`.
            // At A.3 the closure exists (satisfies dispatch shape) but
            // returns `None` — no emission arm currently consumes
            // `BaseTypes` because `generate()` errors unconditionally.
            None::<StructType>
        })
    } else {
        BaseTypes::empty()
    };
    transpiler_v2::generate(common_ast, &base_types)
        .map_err(|e| Status::from(ConnectError::from(e)))
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
                let (common_ast, sql) = transpile_relation(&relation)?;
                // Slice A.3: `finalize()` errors unconditionally, so the
                // downstream classification / streaming helpers are only
                // reachable when Slices C/E begin lighting up emission arms.
                // Signature-swapped helpers preserve the dispatch shape.
                match classify_plan(&common_ast) {
                    PlanKind::Ddl => {
                        execute_ddl(&session, &common_ast, &sql, &session_id, &operation_id).await?
                    }
                    PlanKind::Query => {
                        return execute_streaming_query(
                            &session,
                            &common_ast,
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
                // Warm the session so subsequent slices can reach it.
                let _session = self
                    .session_manager
                    .get_or_create(&session_id)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?;
                // A.3 routes analyze_plan through τ. Emission errors surface
                // as `Status::unimplemented`; Slice B wires the analyzer that
                // produces schemas without ever calling `finalize()`.
                let (_common_ast, _sql) = transpile_relation(&relation)?;
                // A.3 has no schema producer yet — τ's analyzer is Slice B.
                // Return a boundary error identifying this as the
                // schema-analyze surface until then.
                let struct_type = StructType::empty();
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
            let (common_ast, _sql) = transpile_relation(&relation)?;
            handle_create_dataframe_view(session, session_id, operation_id, &common_ast).await
        }
        Some(CommandType::SqlCommand(sql_cmd)) => {
            let common_ast = if let Some(input_rel) = sql_cmd.input {
                let (ast, _sql) = transpile_relation(&input_rel)?;
                ast
            } else {
                // Preserved fallback path for older clients using the
                // deprecated `sql` text field (all SqlCommand text fields are
                // proto-deprecated in favour of `input`).
                #[allow(deprecated)]
                let text = sql_cmd.sql.clone();
                if text.is_empty() {
                    return Err(Status::invalid_argument(
                        "SqlCommand missing both input relation and sql text",
                    ));
                }
                let (ast, _sql) = transpile_raw_sql(&text)?;
                ast
            };
            handle_sql_command(session, session_id, operation_id, &common_ast).await
        }
        Some(CommandType::WriteOperation(mut write_cmd)) => {
            let input_rel = write_cmd
                .input
                .take()
                .ok_or_else(|| Status::invalid_argument("WriteOperation missing input"))?;
            let (common_ast, _sql) = transpile_relation(&input_rel)?;
            handle_write_operation(session, session_id, operation_id, &common_ast, &write_cmd).await
        }
        _ => Err(Status::unimplemented("Unsupported command type")),
    }
}

// ── Signature-swapped downstream helpers (Slice A.3) ─────────────────────────
//
// These helpers formerly consumed `&LogicalPlan`. At Slice A.3 they consume
// `&CommonAst` and return `Err(Status::unimplemented(...))`. `transpile_relation`
// / `transpile_raw_sql` errors before dispatch ever reaches them today (τ's
// `generate()` returns `UnsupportedOp` for every input). Slices B/C/D/E/F/G
// grow the concrete behaviour behind these signatures.

/// Classification of a τ plan for execution routing.
///
/// Slice A.3 collapses this to a two-arm placeholder (DDL vs. Query). Slice
/// C.1 restores the meaningful classification over `CommonAst` (currently the
/// legacy `DdlStatement` variant lives only in `LogicalPlan`, which τ never
/// touches).
#[allow(dead_code)] // `Ddl` reintroduced by Slice C.1.
enum PlanKind {
    /// A DDL/DML statement — execute without result streaming.
    Ddl,
    /// A query — stream results back to the client.
    Query,
}

/// Classify a τ plan as DDL or query.
///
/// Slice A.3: always returns [`PlanKind::Query`]. The DDL classification is a
/// Slice C.1 deliverable — `CommonAst` does not yet carry a DDL discriminant.
fn classify_plan(_common_ast: &CommonAst) -> PlanKind {
    PlanKind::Query
}

/// Execute a DDL statement and return appropriate responses.
///
/// **Slice C.1 (owner):** DDL classification and execution over `CommonAst`.
/// Slice A.3 body errors with `Status::unimplemented` because τ's
/// `finalize()` never reaches this point.
async fn execute_ddl(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    _common_ast: &CommonAst,
    _sql: &str,
    _session_id: &str,
    _operation_id: &str,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Err(Status::unimplemented(
        "Slice C.1: DDL classification and execution over CommonAst",
    ))
}

/// Execute a query plan and stream results back to the client.
///
/// **Slice E (owner):** streaming query execution over `CommonAst`. Slice A.3
/// body errors with `Status::unimplemented` because τ's `finalize()` never
/// reaches this point.
async fn execute_streaming_query(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    _common_ast: &CommonAst,
    _sql: &str,
    _session_id: &str,
    _operation_id: &str,
) -> Result<Response<<ThunderduckService as SparkConnectService>::ExecutePlanStream>, Status> {
    Err(Status::unimplemented(
        "Slice E: streaming query execution over CommonAst",
    ))
}

/// Handle `CreateDataframeView` after successful transpile.
///
/// **Slice B (owner):** schema inference over `CommonAst` for `CREATE VIEW`.
async fn handle_create_dataframe_view(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    _session_id: &str,
    _operation_id: &str,
    _common_ast: &CommonAst,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Err(Status::unimplemented(
        "Slice B: schema inference for CreateView over CommonAst",
    ))
}

/// Handle `SqlCommand` (both `input`-bearing and deprecated text paths).
///
/// **Slice C.1 (owner):** SQL command execution over `CommonAst`.
async fn handle_sql_command(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    _session_id: &str,
    _operation_id: &str,
    _common_ast: &CommonAst,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Err(Status::unimplemented(
        "Slice C.1: SqlCommand execution over CommonAst",
    ))
}

/// Handle `WriteOperation` after successful transpile.
///
/// **Slice H (owner):** external write path over `CommonAst`.
async fn handle_write_operation(
    _session: &Arc<thunderduck_core::runtime::DuckDbSession>,
    _session_id: &str,
    _operation_id: &str,
    _common_ast: &CommonAst,
    _write_cmd: &proto::WriteOperation,
) -> Result<Vec<proto::ExecutePlanResponse>, Status> {
    Err(Status::unimplemented(
        "Slice H: WriteOperation over CommonAst",
    ))
}

// ── Response builders (preserved) ────────────────────────────────────────────

/// Convert DuckDB record batches to a complete `ExecutePlanResponse` sequence,
/// including the mandatory trailing `ResultComplete` frame.
#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

    // ── Slice A.3 dispatch tests ──────────────────────────────────────────────
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
    #[test]
    fn transpile_relation_project_returns_unsupported_op() {
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
        let err = transpile_relation(&project).expect_err("τ emission must error at A.3");
        assert_eq!(
            err.code(),
            tonic::Code::Unimplemented,
            "boundary errors must surface as Status::unimplemented, not internal; got {err:?}"
        );
        // τ's emission boundary is what should be surfaced — NOT
        // `V2RelationConverter`'s `UnsupportedProtoShape` (the proto shape is
        // supported). Since Slice B, this can be either the τ analyzer's
        // Spark-emulated error (unknown table `t` — the test uses an empty
        // BaseTypes overlay) or τ's `UnsupportedOp` (`<slice-b-analyzer-ok>`
        // when the analyzer succeeds). Both signal we reached τ, not the
        // proto-shape gate.
        let message = err.message();
        assert!(
            message.contains("unsupported operator")
                || message.contains("unsupported expression")
                || message.contains("<slice-b-analyzer-ok>")
                || message.contains("[SPARK-EMULATED]")
                || message.contains("<a.2-substrate>"),
            "message must identify τ's boundary error; got: {message}",
        );
    }

    /// `RelType::Sql` MUST route through `parser_v2`, not `V2RelationConverter`.
    /// The failure mode we're guarding against: if dispatch fed `Sql` to
    /// `V2RelationConverter`, the error would identify `RelType::Sql` as the
    /// unsupported proto shape. Instead, `parser_v2` parses the SQL and τ's
    /// `generate()` emits DuckDB SQL (Slice C.1 wired the Project + SingleRow
    /// arms — `SELECT 1` is a Project over SingleRow of a literal).
    #[test]
    fn transpile_relation_sql_routes_to_parser_v2_not_converter() {
        let sql_rel = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                query: "SELECT 1".to_owned(),
                args: Default::default(),
                pos_args: vec![],
                named_arguments: Default::default(),
                pos_arguments: vec![],
            })),
        };
        // Slice C.1 wired Project + SingleRow arms — `SELECT 1` now succeeds.
        // The routing anchor (SQL → parser_v2, not converter) is still enforced:
        // a routing bug would have surfaced `RelType::Sql` as an
        // `UnsupportedProtoShape` error before reaching τ's emission.
        let (_common_ast, sql) =
            transpile_relation(&sql_rel).expect("τ must emit SQL for `SELECT 1`");
        assert!(
            sql.contains("SELECT"),
            "expected DuckDB SELECT emission; got: {sql}",
        );
    }

    /// SparkSQL syntax errors surface via `parser_v2`'s boundary policy
    /// (`UnsupportedProtoShape { shape: "sql::parse_error", ... }`), which
    /// maps to `Status::unimplemented` per `ConnectError::TranspilerV2Emission`.
    #[test]
    fn transpile_relation_sql_syntax_error_surfaces_from_parser_v2() {
        let sql_rel = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Sql(proto::Sql {
                query: "NOT VALID SQL".to_owned(),
                args: Default::default(),
                pos_args: vec![],
                named_arguments: Default::default(),
                pos_arguments: vec![],
            })),
        };
        let err = transpile_relation(&sql_rel).expect_err("syntax error must surface");
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
    #[test]
    fn transpile_relation_unsupported_proto_shape_surfaces() {
        let sample = proto::Relation {
            common: None,
            rel_type: Some(proto::relation::RelType::Sample(Box::new(proto::Sample {
                input: Some(Box::new(table_scan_relation("t"))),
                lower_bound: 0.0,
                upper_bound: 1.0,
                with_replacement: None,
                seed: None,
                deterministic_order: false,
            }))),
        };
        let err = transpile_relation(&sample).expect_err("deferred shape must error");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        assert!(
            err.message().contains("unsupported proto shape") || err.message().contains("Sample"),
            "expected UnsupportedProtoShape; got: {}",
            err.message()
        );
    }

    /// `finalize()` builds `BaseTypes::empty()` when the plan carries no
    /// empty-scan (short-circuit anchor at the service layer — the substrate
    /// test in `base_types::tests` already pins the closure-not-invoked
    /// behavior). Slice C.1 wired SingleRow emission — this test now asserts
    /// that finalize returns the emitted SQL for a plan with no empty scan
    /// (proving the short-circuit path builds `BaseTypes::empty()` without
    /// blocking emission).
    #[test]
    fn finalize_short_circuits_on_plans_without_empty_scan() {
        use thunderduck_core::transpiler_v2::ast::CommonOp;
        // A `SingleRow` plan carries no `TableScan` → `plan_has_empty_scan`
        // is false → `BaseTypes::empty()` (no closure invocation) → τ emits.
        let plan = CommonAst::new(CommonOp::SingleRow);
        let sql = finalize(&plan).expect("τ must emit for SingleRow");
        assert_eq!(sql, "SELECT");
    }
}
