use thunderduck_core::error::ThunderduckError;
use thunderduck_core::transpiler_v2::EmissionError;
use tonic::Status;

/// All errors produced by the connect-server layer.
#[derive(thiserror::Error, Debug)]
pub enum ConnectError {
    #[error("SQL generation error: {0}")]
    SqlGeneration(ThunderduckError),

    /// Spark-emulated **runtime** error (ADR-006): a DuckDB engine throw that τ
    /// re-clothed in Spark's error taxonomy. The wrapped string already begins
    /// with the `[class]` token, so the wire message leads with it.
    #[error("{0}")]
    SparkRuntime(String),

    #[error("Arrow serialization error: {0}")]
    Arrow(String),

    /// τ emission boundary error. Per ADR-022 these are Thunderduck-boundary
    /// failures (the input shape is not yet supported); they surface as
    /// [`Status::unimplemented`] so clients see them distinctly from server-
    /// internal (`Status::internal`) faults.
    #[error("τ emission error: {0}")]
    TranspilerV2Emission(#[from] EmissionError),
}

impl From<ThunderduckError> for ConnectError {
    fn from(e: ThunderduckError) -> Self {
        match e {
            // ADR-006: a Spark-emulated runtime error keeps its Spark class on
            // the wire (message already leads with `[class]`); everything else
            // is an internal SQL-generation/execution fault.
            ThunderduckError::SparkRuntime { message, .. } => ConnectError::SparkRuntime(message),
            other => ConnectError::SqlGeneration(other),
        }
    }
}

impl From<ConnectError> for Status {
    fn from(e: ConnectError) -> Self {
        match e {
            ConnectError::SqlGeneration(e) => Status::internal(e.to_string()),
            ConnectError::SparkRuntime(msg) => Status::internal(msg),
            ConnectError::Arrow(msg) => Status::internal(msg),
            ConnectError::TranspilerV2Emission(e) => Status::unimplemented(e.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, ConnectError>;
