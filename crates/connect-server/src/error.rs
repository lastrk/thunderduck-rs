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
    /// [`Status::invalid_argument`]. The `Internal` variant (review finding
    /// 5) is a genuine τ-internal invariant violation and surfaces as
    /// [`Status::internal`] — see the `From<ConnectError> for Status`
    /// mapping below.
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
            // Internal (review finding 5) is a genuine τ bug — INTERNAL.
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

    /// ADR-023 chunk 3b must-have: an ambiguous-column `EmissionError`
    /// surfaces as a `Status` whose message LEADS with the real Spark class
    /// token (`[AMBIGUOUS_REFERENCE]`) under INVALID_ARGUMENT, never as a
    /// τ-boundary UNIMPLEMENTED. This is what unblocks
    /// `tests/integration/utils/dataframe_diff.py::spark_error_class` on the
    /// client side.
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

    /// Review A1. A Spark-emulated error whose Spark class τ has NOT
    /// established must still exit as `INVALID_ARGUMENT` — Spark rejects the
    /// input too, so `UNIMPLEMENTED` ("τ doesn't support this") would be a lie.
    /// Before A1 every such error took the boundary path.
    ///
    /// It must ALSO render prefix-free. The client oracle recovers the class
    /// with `^\s*\[([A-Z][A-Z0-9_.]*)\]`
    /// (`tests/integration/utils/dataframe_diff.py`), so any leading
    /// bracketed token here would be compared against Spark's real class as if
    /// τ had emitted one. `None` must yield no token at all rather than a
    /// placeholder.
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

    /// Review finding 5: a τ-internal invariant violation (e.g. the join
    /// left/right disjointness check) surfaces as `Status::internal`, not
    /// `Unimplemented`/`InvalidArgument` — it is a genuine τ bug, distinct
    /// from both other categories.
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
