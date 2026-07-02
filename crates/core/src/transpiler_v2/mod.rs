//! τ — the Spark → DuckDB transliterator (v2 substrate).
//!
//! ADR-021 (τ owns substrate) + ADR-022 (τ is the only path).
//!
//! At Slice A.2 this module tree carries the type substrate plus a stable
//! [`CommonAst`] + [`BaseTypes`] surface. Every input still surfaces
//! [`EmissionError::UnsupportedOp`] — a Thunderduck-boundary error per
//! ADR-022, returned directly to the caller with no fallback machinery.

pub mod ast;
pub mod base_types;
pub mod error;
pub mod expression;
pub mod invariants;
pub mod type_inference;

pub use ast::{CommonAst, CommonOp};
pub use base_types::BaseTypes;
pub use error::EmissionError;
pub use expression::Expression;
pub use type_inference::TypeInferenceEngine;

/// τ's schema type alias — points at the shared `StructType`.
pub type Schema = crate::types::StructType;

/// τ's top-level entry point.
///
/// **Slice A.2 behavior:** returns `Err(EmissionError::UnsupportedOp)` for
/// every input. Slice A.3 relocates dispatch to route every Spark Connect
/// request through this function; Slices B/C/D/E/F/G grow the coverage.
pub fn generate(plan: &CommonAst, base_types: &BaseTypes) -> Result<String, EmissionError> {
    let _ = (plan, base_types);
    Err(EmissionError::UnsupportedOp {
        op: "<a.2-substrate>".to_owned(),
        reason: "τ substrate is under construction (Slice A.2); \
                 emission not implemented yet"
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_returns_unsupported_op_for_any_input() {
        let plan = CommonAst::new(CommonOp::SingleRow);
        let base_types = BaseTypes::empty();
        let result = generate(&plan, &base_types);
        assert!(matches!(result, Err(EmissionError::UnsupportedOp { .. })));
    }

    /// Compile-only sanity: `generate()`'s signature accepts `&CommonAst` and
    /// `&BaseTypes`. Fails to compile if the placeholder types come back.
    #[test]
    fn generate_signature_uses_common_ast_and_base_types() {
        fn assert_signature(_f: fn(&CommonAst, &BaseTypes) -> Result<String, EmissionError>) {}
        assert_signature(generate);
    }
}
