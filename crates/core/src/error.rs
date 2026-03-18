/// All errors produced by the thunderduck-core crate.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
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
}

pub type Result<T> = std::result::Result<T, ThunderduckError>;
