/// All errors produced by the thunderduck-core crate.
#[derive(thiserror::Error, Debug)]
pub enum ThunderduckError {
    #[error("SQL generation failed: {0}")]
    SqlGeneration(String),

    #[error("Type inference error: {0}")]
    TypeInference(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Schema error: {0}")]
    Schema(String),

    #[error("DuckDB error: {0}")]
    DuckDb(String),

    /// A Spark-emulated **runtime** error (ADR-006): a DuckDB engine-level throw
    /// that τ has re-clothed in Spark's error taxonomy. `class` is the Spark
    /// error-class token (e.g. `DIVIDE_BY_ZERO`); `message` is the full
    /// Spark-verbatim message and already begins with `[class]`, so the wire
    /// error and the differential oracle can key on the leading token.
    #[error("{message}")]
    SparkRuntime { class: String, message: String },

    #[error("τ emission error: {0}")]
    TranspilerV2Emission(#[from] crate::transpiler_v2::EmissionError),
}

impl From<duckdb::Error> for ThunderduckError {
    fn from(e: duckdb::Error) -> Self {
        ThunderduckError::DuckDb(e.to_string())
    }
}

/// Extract a Spark ANSI error class from a raw DuckDB error string produced by
/// τ's emitted `error('[CLASS] …')` guard (ADR-006). DuckDB prefixes its own
/// `"Invalid Input Error: "` before our payload, so we scan for the first
/// `[UPPER_SNAKE]` token and return `(class, clean_message)` where
/// `clean_message` starts at that token. Returns `None` for DuckDB errors that
/// carry no τ-emitted Spark class.
pub fn classify_spark_runtime_error(duckdb_msg: &str) -> Option<(String, String)> {
    let open = duckdb_msg.find('[')?;
    let rest = &duckdb_msg[open..];
    let close = rest.find(']')?;
    let token = &rest[1..close];
    let is_class = !token.is_empty()
        && token.as_bytes()[0].is_ascii_uppercase()
        && token
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
    if is_class {
        Some((token.to_string(), rest.to_string()))
    } else {
        None
    }
}

impl ThunderduckError {
    /// If this is a `DuckDb` error carrying a τ-emitted Spark error-class token,
    /// re-wrap it as a Spark-emulated runtime error (ADR-006). Other errors pass
    /// through unchanged.
    pub fn reclassified_spark_runtime(self) -> Self {
        if let ThunderduckError::DuckDb(ref msg) = self {
            if let Some((class, message)) = classify_spark_runtime_error(msg) {
                return ThunderduckError::SparkRuntime { class, message };
            }
        }
        self
    }
}

pub type Result<T> = std::result::Result<T, ThunderduckError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_extracts_class_from_duckdb_prefixed_message() {
        // DuckDB prepends its own "Invalid Input Error: " to our payload.
        let raw = "Invalid Input Error: [DIVIDE_BY_ZERO] Division by zero. SQLSTATE: 22012";
        let (class, clean) = classify_spark_runtime_error(raw).expect("classified");
        assert_eq!(class, "DIVIDE_BY_ZERO");
        assert!(
            clean.starts_with("[DIVIDE_BY_ZERO] Division by zero"),
            "got: {clean}"
        );
    }

    #[test]
    fn classify_ignores_non_class_brackets_and_plain_errors() {
        assert!(classify_spark_runtime_error("Binder Error: no column [x]").is_none());
        assert!(classify_spark_runtime_error("Catalog Error: table missing").is_none());
    }

    #[test]
    fn reclassify_rewraps_only_tokened_duckdb_errors() {
        let tokened = ThunderduckError::DuckDb("[REMAINDER_BY_ZERO] Remainder by zero".into());
        match tokened.reclassified_spark_runtime() {
            ThunderduckError::SparkRuntime { class, message } => {
                assert_eq!(class, "REMAINDER_BY_ZERO");
                assert!(message.starts_with("[REMAINDER_BY_ZERO]"));
            }
            other => panic!("expected SparkRuntime, got: {other:?}"),
        }
        // A plain DuckDB error is left untouched.
        let plain = ThunderduckError::DuckDb("Catalog Error: nope".into());
        assert!(matches!(
            plain.reclassified_spark_runtime(),
            ThunderduckError::DuckDb(_)
        ));
    }
}
