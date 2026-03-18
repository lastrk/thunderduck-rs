use std::pin::Pin;
use std::sync::Arc;

use futures::stream;
use thunderduck_core::generator::SqlGenerator;
use thunderduck_core::runtime::{RuntimeCompatMode, SessionManager};
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
                let logical_plan = PlanConverter::convert_relation(&relation)
                    .map_err(|e| Status::from(e))?;

                let sql = SqlGenerator::relaxed()
                    .generate(&logical_plan)
                    .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;

                let batches = session
                    .execute(&sql)
                    .await
                    .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;

                batches_to_responses(&session_id, &operation_id, &batches)
                    .map_err(|e| Status::from(e))?
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
                let logical_plan =
                    PlanConverter::convert_relation(&relation).map_err(Status::from)?;
                let struct_type = logical_plan.infer_schema();
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
                // Return empty string for any unknown key so PySpark conf.get() doesn't crash
                g.keys.into_iter().map(|k| proto::KeyValue { key: k, value: Some(String::new()) }).collect()
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
            let input_rel = sql_cmd
                .input
                .ok_or_else(|| Status::unimplemented("SqlCommand without input relation"))?;
            let logical_plan =
                PlanConverter::convert_relation(&input_rel).map_err(Status::from)?;
            let sql = SqlGenerator::relaxed()
                .generate(&logical_plan)
                .map_err(|e| Status::from(ConnectError::SqlGeneration(e)))?;
            let batches = session
                .execute(&sql)
                .await
                .map_err(|e| Status::from(ConnectError::Session(e.to_string())))?;
            batches_to_responses(session_id, operation_id, &batches).map_err(Status::from)
        }
        _ => Err(Status::unimplemented("Unsupported command type")),
    }
}

/// Convert DuckDB record batches to a complete `ExecutePlanResponse` sequence,
/// including the mandatory trailing `ResultComplete` frame.
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
    if responses.is_empty() {
        responses.push(empty_result_response(session_id, operation_id));
    }
    responses.push(result_complete_response(session_id, operation_id));
    Ok(responses)
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
