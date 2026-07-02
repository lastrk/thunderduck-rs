//! τ emission errors — Thunderduck-boundary category per ADR-022.
//!
//! `EmissionError` is exclusively **Thunderduck-boundary**. Spark-emulated
//! errors (unknown column, ambiguous column, type mismatch) live in Slice B's
//! `AnalyzerError` and are not part of this type. No variant of
//! `EmissionError` may ever signal a fallback path — under ADR-022 there is
//! no fallback. Callers see boundary errors verbatim.

/// The categories of errors τ can surface at emission time.
///
/// All three variants are Thunderduck-boundary. Slice A.1 only exercises
/// `UnsupportedOp`; the other two variants are reserved for Slice C.
#[derive(thiserror::Error, Debug)]
pub enum EmissionError {
    /// The top-level operator of the input plan is not yet supported by τ.
    #[error("τ: unsupported operator `{op}`: {reason}")]
    UnsupportedOp {
        /// Human-readable operator name (e.g. `"Project"`, `"Join"`).
        op: String,
        /// Explanation for why this operator is not yet supported.
        reason: String,
    },

    /// The expression shape is not yet supported by τ.
    ///
    /// Reserved for Slice C when emission arms discover un-seeded expression
    /// forms.
    #[error("τ: unsupported expression `{shape}`: {reason}")]
    UnsupportedExpression {
        /// Human-readable expression shape (e.g. `"Lambda"`, `"ScalarSubquery"`).
        shape: String,
        /// Explanation for why this expression is not yet supported.
        reason: String,
    },

    /// The function name has no τ emission arm (native or extension).
    ///
    /// Reserved for Slice C.2 / D.
    #[error("τ: unsupported function `{name}`: {reason}")]
    UnsupportedFunction {
        /// Spark function name.
        name: String,
        /// Explanation for why this function is not yet supported.
        reason: String,
    },

    /// The input proto (or SQL) shape is not (yet) representable as a
    /// `CommonAst`. Emitted at the τ ingress boundary — `V2RelationConverter`
    /// and `parser_v2` — when the caller hands us a proto/SQL construct that
    /// the substrate has not yet grown a variant for.
    ///
    /// Semantically distinct from [`Self::UnsupportedOp`], which signals "the
    /// emission arm isn't there yet" for a valid `CommonAst`. This variant
    /// says "the input never reached `CommonAst`."
    #[error("τ: unsupported proto shape `{shape}`: {reason}")]
    UnsupportedProtoShape {
        /// Human-readable proto/SQL shape (e.g. `"RelType::Sql"`,
        /// `"arrow_value::Decimal256"`, `"sql::pivot"`).
        shape: String,
        /// Explanation for why this shape has no lowering rule yet.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ThunderduckError;

    #[test]
    fn unsupported_op_display() {
        let e = EmissionError::UnsupportedOp {
            op: "Project".to_owned(),
            reason: "not seeded".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ: unsupported operator `Project`: not seeded"
        );
    }

    #[test]
    fn unsupported_expression_display() {
        let e = EmissionError::UnsupportedExpression {
            shape: "Lambda".to_owned(),
            reason: "no emission arm".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ: unsupported expression `Lambda`: no emission arm"
        );
    }

    #[test]
    fn unsupported_function_display() {
        let e = EmissionError::UnsupportedFunction {
            name: "sha3".to_owned(),
            reason: "not implemented".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ: unsupported function `sha3`: not implemented"
        );
    }

    #[test]
    fn unsupported_proto_shape_display() {
        let e = EmissionError::UnsupportedProtoShape {
            shape: "RelType::Sql".to_owned(),
            reason: "parser_v2 owns SQL text".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ: unsupported proto shape `RelType::Sql`: parser_v2 owns SQL text"
        );
    }

    #[test]
    fn unsupported_proto_shape_composes_into_thunderduck_error() {
        let e = EmissionError::UnsupportedProtoShape {
            shape: "arrow_value::Decimal256".to_owned(),
            reason: "no dispatch arm".to_owned(),
        };
        let composed: ThunderduckError = e.into();
        assert!(matches!(
            composed,
            ThunderduckError::TranspilerV2Emission(_)
        ));
    }

    #[test]
    fn emission_error_from_composes_into_thunderduck_error() {
        let e = EmissionError::UnsupportedOp {
            op: "Project".to_owned(),
            reason: "not seeded".to_owned(),
        };
        let composed: ThunderduckError = e.into();
        assert!(matches!(
            composed,
            ThunderduckError::TranspilerV2Emission(_)
        ));
    }
}
