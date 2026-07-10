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

    /// τ emission boundary error. Per ADR-022 the `Unsupported` variant is a
    /// Thunderduck-boundary failure (the input shape is not yet supported)
    /// and surfaces as [`Status::unimplemented`] so clients see it distinctly
    /// from server-internal (`Status::internal`) faults. The `SparkEmulated`
    /// variant (ADR-023 chunk 3b) is a Spark-analysis error re-surfaced with
    /// its Spark class token leading the message; it surfaces as
    /// [`Status::invalid_argument`] — see the `From<ConnectError> for
    /// Status` mapping below.
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
            // ADR-023 chunk 3b: SparkEmulated's Display already leads with
            // the real Spark class token (`[{class}] {message}`); surface it
            // verbatim so `spark_error_class` can extract it. Unsupported
            // stays UNIMPLEMENTED — it's still a Thunderduck-boundary gap.
            ConnectError::TranspilerV2Emission(e) => match &e {
                EmissionError::SparkEmulated { .. } => Status::invalid_argument(e.to_string()),
                EmissionError::Unsupported { .. } => Status::unimplemented(e.to_string()),
            },
        }
    }
}

pub type Result<T> = std::result::Result<T, ConnectError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-023 chunk 3b must-have: an ambiguous-column `EmissionError`
    /// surfaces as a `Status` whose message LEADS with the real Spark class
    /// token (`[AMBIGUOUS_REFERENCE]`), not `Status::unimplemented`'s
    /// `analyzer-spark-emulated` marker. This is what unblocks
    /// `tests/integration/utils/dataframe_diff.py::spark_error_class` on the
    /// client side.
    #[test]
    fn spark_emulated_ambiguous_column_status_leads_with_class_token() {
        let emission_err = EmissionError::SparkEmulated {
            class: "AMBIGUOUS_REFERENCE",
            message:
                "column `dept_id` is ambiguous, candidates: [\"emp.dept_id\", \"dept.dept_id\"]"
                    .to_owned(),
        };
        let connect_err: ConnectError = emission_err.into();
        let status: Status = connect_err.into();

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(
            status.message().starts_with("[AMBIGUOUS_REFERENCE]"),
            "expected Status message to lead with the Spark class token, got: {}",
            status.message()
        );
    }

    /// Guardrail: Thunderduck-boundary `Unsupported` errors keep their
    /// existing `Status::unimplemented` surfacing — unaffected by the
    /// `SparkEmulated` carve-out.
    #[test]
    fn unsupported_boundary_error_status_stays_unimplemented() {
        let emission_err = EmissionError::Unsupported {
            kind: thunderduck_core::transpiler_v2::error::UnsupportedKind::Op,
            name: "Unnest".to_owned(),
            reason: "not seeded".to_owned(),
        };
        let connect_err: ConnectError = emission_err.into();
        let status: Status = connect_err.into();

        assert_eq!(status.code(), tonic::Code::Unimplemented);
    }
}
