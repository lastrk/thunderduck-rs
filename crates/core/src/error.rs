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

    /// v2 lowering (`LogicalPlan → CommonAst` adapter) failed.
    ///
    /// See [`crate::transpiler_v2::lowering`]. Fallback-eligibility is
    /// decided by callers; a `LoweringError` is a structural adapter defect
    /// and is generally *not* fallback-eligible.
    #[error("v2 lowering: {0}")]
    V2Lowering(#[from] crate::transpiler_v2::lowering::LoweringError),

    /// v2 analyzer rejected the AST.
    ///
    /// [`crate::transpiler_v2::analyzer::AnalyzerError::PuntedOperator`]
    /// and `UnknownTable` are fallback-eligible (dispatch wrapper falls
    /// back to legacy); every other variant is a real analyzer bug that
    /// must surface.
    #[error("v2 analyzer: {0}")]
    V2Analyzer(#[from] crate::transpiler_v2::analyzer::AnalyzerError),

    /// v2 emission table failed to dispatch.
    ///
    /// [`crate::transpiler_v2::emission::EmissionError::UnsupportedOp`] is
    /// fallback-eligible; every other variant is a real emitter bug that
    /// must surface.
    #[error("v2 emission: {0}")]
    V2Emission(#[from] crate::transpiler_v2::emission::EmissionError),
}

impl From<duckdb::Error> for ThunderduckError {
    fn from(e: duckdb::Error) -> Self {
        ThunderduckError::DuckDb(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ThunderduckError>;
