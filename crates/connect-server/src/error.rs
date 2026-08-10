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

    /// τ emission errors map to gRPC status codes according to their category:
    /// unsupported inputs are `UNIMPLEMENTED`, Spark-emulated errors are
    /// `INVALID_ARGUMENT`, and internal invariants are `INTERNAL`.
    #[error("τ emission error: {0}")]
    TranspilerV2Emission(#[from] EmissionError),
}

impl From<ThunderduckError> for ConnectError {
    fn from(e: ThunderduckError) -> Self {
        match e {
            // The message already carries the Spark error class when present.
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
            ConnectError::TranspilerV2Emission(e) => match &e {
                EmissionError::SparkEmulated { .. } => Status::invalid_argument(e.to_string()),
                EmissionError::Unsupported { .. } => Status::unimplemented(e.to_string()),
                EmissionError::Internal { .. } => Status::internal(e.to_string()),
            },
        }
    }
}

pub type Result<T> = std::result::Result<T, ConnectError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Spark-emulated errors preserve the Spark class token and use
    /// `INVALID_ARGUMENT`.
    #[test]
    fn spark_emulated_ambiguous_column_status_leads_with_class_token() {
        let emission_err = EmissionError::SparkEmulated {
            class: Some("AMBIGUOUS_REFERENCE"),
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

    /// Classless Spark-emulated errors remain `INVALID_ARGUMENT` without an
    /// invented class token.
    #[test]
    fn classless_spark_emulated_is_invalid_argument_with_no_bogus_token() {
        let emission_err = EmissionError::SparkEmulated {
            class: None,
            message: "requirement failed: Unsupported natural join type LeftSemi".to_owned(),
        };
        let connect_err: ConnectError = emission_err.into();
        let status: Status = connect_err.into();

        assert_eq!(
            status.code(),
            tonic::Code::InvalidArgument,
            "a classless Spark-emulated error is still Spark-emulated"
        );
        assert!(
            !status.message().starts_with('['),
            "a classless error must not lead with any bracketed token — the \
             oracle would read it as a real Spark class; got: {}",
            status.message()
        );
        assert_eq!(
            status.message(),
            "requirement failed: Unsupported natural join type LeftSemi"
        );
    }

    /// Thunderduck-boundary errors remain `UNIMPLEMENTED`.
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

    /// τ-internal invariant violations surface as `INTERNAL`.
    #[test]
    fn internal_invariant_violation_status_is_internal() {
        let emission_err = EmissionError::Internal {
            message: "join left/right resolved_schema expr_id sets must be disjoint".to_owned(),
        };
        let connect_err: ConnectError = emission_err.into();
        let status: Status = connect_err.into();

        assert_eq!(status.code(), tonic::Code::Internal);
    }
}
