use thunderduck_core::error::ThunderduckError;
use thunderduck_core::transpiler_v2::EmissionError;
use tonic::Status;

/// All errors produced by the connect-server layer.
#[derive(thiserror::Error, Debug)]
pub enum ConnectError {
    #[error("Plan conversion error: {0}")]
    PlanConversion(String),

    #[error("SQL generation error: {0}")]
    SqlGeneration(#[from] ThunderduckError),

    #[error("Arrow serialization error: {0}")]
    Arrow(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Session error: {0}")]
    Session(String),

    /// τ emission boundary error. Per ADR-022 these are Thunderduck-boundary
    /// failures (the input shape is not yet supported); they surface as
    /// [`Status::unimplemented`] so clients see them distinctly from server-
    /// internal (`Status::internal`) faults.
    #[error("τ emission error: {0}")]
    TranspilerV2Emission(#[from] EmissionError),
}

impl From<ConnectError> for Status {
    fn from(e: ConnectError) -> Self {
        match e {
            ConnectError::Unsupported(msg) => Status::unimplemented(msg),
            ConnectError::PlanConversion(msg) => Status::invalid_argument(msg),
            ConnectError::SqlGeneration(e) => Status::internal(e.to_string()),
            ConnectError::Arrow(msg) => Status::internal(msg),
            ConnectError::Session(msg) => Status::internal(msg),
            ConnectError::TranspilerV2Emission(e) => Status::unimplemented(e.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, ConnectError>;
