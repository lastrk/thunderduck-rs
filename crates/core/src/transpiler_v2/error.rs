//! τ emission errors — Thunderduck-boundary category per ADR-022, plus the
//! ADR-023 chunk 3b Spark-emulated re-surfacing carve-out.
//!
//! [`EmissionError::Unsupported`] is exclusively **Thunderduck-boundary**: no
//! variant may ever signal a fallback path — under ADR-022 there is no
//! fallback. Callers see boundary errors verbatim.
//!
//! [`EmissionError::SparkEmulated`] is the one carve-out: it re-surfaces a
//! Spark-emulated `AnalyzerError` (unknown column, ambiguous column, type
//! mismatch, ...) with the real Spark error-class token leading the wire
//! message, so the client sees the same error class Spark itself would
//! raise. It does not signal a Thunderduck-boundary gap.
//!
//! [`EmissionError::Internal`] is a third, narrow category: a τ-internal
//! invariant violation — a plan-shape guarantee the analyzer is supposed to
//! establish did not hold at emission time. This is neither an honest
//! "not yet implemented" boundary gap nor an input Spark itself would
//! reject; it signals a genuine τ bug and surfaces as `Status::internal`.

/// The categories of errors τ can surface at emission time.
///
/// [`EmissionError::Unsupported`] carries the four Thunderduck-boundary
/// flavours through its [`UnsupportedKind`] tag: `Op` (top-level operator not
/// yet emitted), `Expression` (un-seeded expression shape), `Function` (no
/// emission arm for a Spark function name), and `ProtoShape` (input never
/// reached [`CommonAst`]). [`EmissionError::SparkEmulated`] carries a
/// Spark-emulated analyzer error re-surfaced with its Spark class token
/// (ADR-023 chunk 3b). [`EmissionError::Internal`] carries a τ-internal
/// invariant-violation message (review finding 5, 2026-07-13).
///
/// [`CommonAst`]: crate::transpiler_v2::ast::CommonAst
#[derive(thiserror::Error, Debug)]
pub enum EmissionError {
    /// A τ-boundary reject: the referenced shape has no emission arm yet.
    ///
    /// `kind` selects which Display prefix appears in the wire message; the
    /// per-kind text is byte-identical to the four legacy variants
    /// (`UnsupportedOp`, `UnsupportedExpression`, `UnsupportedFunction`,
    /// `UnsupportedProtoShape`) this variant replaces.
    #[error("τ: {} `{name}`: {reason}", kind.display_prefix())]
    Unsupported {
        /// Which flavour of τ-boundary reject this is.
        kind: UnsupportedKind,
        /// The specific operator / expression / function / proto shape name
        /// that surfaced the reject.
        name: String,
        /// Explanation for why the shape is not yet supported.
        reason: String,
    },

    /// A Spark-emulated analyzer error, re-surfaced with its Spark
    /// error-class token leading the message (ADR-023 chunk 3b), bridged
    /// from `AnalyzerError` by
    /// `analyzer::analyzer_error_to_emission_error`. Unlike
    /// [`Self::Unsupported`], this does not signal a Thunderduck-boundary
    /// gap: `class` is the real Spark error-class token (e.g.
    /// `"AMBIGUOUS_REFERENCE"`) so the client-side differential harness can
    /// key off it exactly as it would for Spark itself.
    #[error("[{class}] {message}")]
    SparkEmulated {
        /// The Spark error-class token (e.g. `"AMBIGUOUS_REFERENCE"`,
        /// `"UNRESOLVED_COLUMN"`).
        class: &'static str,
        /// The human-readable message, without the analyzer's
        /// `[SPARK-EMULATED]` τ-internal prefix (the class token above
        /// replaces it as the leading token).
        message: String,
    },

    /// A τ-internal invariant violation — a plan-shape guarantee the
    /// analyzer is supposed to establish (e.g. join left/right id-set
    /// disjointness) did not hold at emission time. Unlike
    /// [`Self::Unsupported`] (an honest "not yet implemented" boundary gap)
    /// or [`Self::SparkEmulated`] (Spark itself would also reject this
    /// input), this signals a genuine τ bug: some invariant the analyzer
    /// promised was broken. Kept ALWAYS-ON rather than a `debug_assert!`
    /// precisely because its failure mode is silently wrong SQL, not a loud
    /// crash (review finding 5, `v2-review-findings-2026-07-13.md`).
    #[error("τ-internal invariant violation: {message}")]
    Internal {
        /// Description of the violated invariant and where it fired.
        message: String,
    },
}

/// Discriminator for [`EmissionError::Unsupported`] — selects the Display
/// prefix and encodes which category of τ-boundary reject fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedKind {
    /// Top-level operator has no τ emission arm.
    Op,
    /// Expression shape has no τ emission arm.
    Expression,
    /// Function name has no emission arm (native or extension).
    Function,
    /// Input proto / SQL shape never reached [`CommonAst`].
    ///
    /// [`CommonAst`]: crate::transpiler_v2::ast::CommonAst
    ProtoShape,
}

impl UnsupportedKind {
    /// The Display prefix emitted in the wire message for this kind.
    ///
    /// The four returned literals match the legacy variants' Display strings
    /// byte-for-byte so wire-error-string tests remain unchanged.
    pub fn display_prefix(&self) -> &'static str {
        match self {
            Self::Op => "unsupported operator",
            Self::Expression => "unsupported expression",
            Self::Function => "unsupported function",
            Self::ProtoShape => "unsupported proto shape",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_op_display() {
        let e = EmissionError::Unsupported {
            kind: UnsupportedKind::Op,
            name: "Project".to_owned(),
            reason: "not seeded".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ: unsupported operator `Project`: not seeded"
        );
    }

    #[test]
    fn unsupported_expression_display() {
        let e = EmissionError::Unsupported {
            kind: UnsupportedKind::Expression,
            name: "Lambda".to_owned(),
            reason: "no emission arm".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ: unsupported expression `Lambda`: no emission arm"
        );
    }

    #[test]
    fn unsupported_function_display() {
        let e = EmissionError::Unsupported {
            kind: UnsupportedKind::Function,
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
        let e = EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: "RelType::Sql".to_owned(),
            reason: "parser_v2 owns SQL text".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ: unsupported proto shape `RelType::Sql`: parser_v2 owns SQL text"
        );
    }

    #[test]
    fn spark_emulated_display_leads_with_class_token() {
        let e = EmissionError::SparkEmulated {
            class: "AMBIGUOUS_REFERENCE",
            message: "column `id` is ambiguous, candidates: [\"l.id\", \"r.id\"]".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "[AMBIGUOUS_REFERENCE] column `id` is ambiguous, candidates: [\"l.id\", \"r.id\"]"
        );
    }

    #[test]
    fn internal_display() {
        let e = EmissionError::Internal {
            message: "join left/right resolved_schema expr_id sets must be disjoint".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "τ-internal invariant violation: join left/right resolved_schema expr_id sets must be disjoint"
        );
    }
}
