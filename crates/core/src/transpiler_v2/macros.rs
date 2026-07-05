//! τ boundary-error bail macros — thin `return Err(EmissionError::*)`
//! constructors used at ~200 sites across `emission`, `parser_v2`, and the
//! connect-server converter.
//!
//! Each macro expands to a `return Err(...)` of the shared
//! [`crate::transpiler_v2::error::EmissionError::Unsupported`] variant tagged
//! with the appropriate
//! [`crate::transpiler_v2::error::UnsupportedKind`] (or, for the `rule!`
//! macro, [`crate::transpiler_v2::analyzer::AnalyzerError::UnsupportedRule`])
//! with the canonical field shape (`{ kind, name: String, reason: String }`).
//! Wire `Display` output is byte-identical to the hand-written struct
//! literals they replace — every argument is coerced via `.to_owned()` so
//! callers can pass `&str`, `String`, `&String`, `format!(...)`, etc.
//!
//! # Design notes
//!
//! - The macros expand to `return Err(...)` **without** a trailing
//!   semicolon, so callers can invoke them both at statement position
//!   (`bail_boundary_op!(...);`) and as a match-arm body / tail expression
//!   (`Foo => bail_boundary_op!(...)`). The `return` expression is `!`,
//!   which coerces to any type.
//! - `.ok_or_else(|| ...)` / `.map_err(|e| ...)` closure sites do NOT use
//!   these macros — a `return` inside the closure would return from the
//!   closure, not from the enclosing function. Those sites are covered
//!   separately by OPP-HHH (`ProtoFieldExt::require_proto`, Pass 8).
//! - `#[macro_export]` puts each macro at the crate root of
//!   `thunderduck_core`. Downstream crates (e.g. `thunderduck-connect-server`)
//!   invoke them as `thunderduck_core::bail_boundary_*!(...)`.

/// Bail with an `Op`-kinded [`EmissionError::Unsupported`]: the top-level
/// operator has no τ emission arm yet.
///
/// [`EmissionError::Unsupported`]: crate::transpiler_v2::error::EmissionError::Unsupported
#[macro_export]
macro_rules! bail_boundary_op {
    ($op:expr, $reason:expr $(,)?) => {
        return Err($crate::transpiler_v2::error::EmissionError::Unsupported {
            kind: $crate::transpiler_v2::error::UnsupportedKind::Op,
            name: ($op).to_owned(),
            reason: ($reason).to_owned(),
        })
    };
}

/// Bail with an `Expression`-kinded [`EmissionError::Unsupported`]: the
/// expression shape has no τ emission arm yet.
///
/// [`EmissionError::Unsupported`]: crate::transpiler_v2::error::EmissionError::Unsupported
#[macro_export]
macro_rules! bail_boundary_expr {
    ($shape:expr, $reason:expr $(,)?) => {
        return Err($crate::transpiler_v2::error::EmissionError::Unsupported {
            kind: $crate::transpiler_v2::error::UnsupportedKind::Expression,
            name: ($shape).to_owned(),
            reason: ($reason).to_owned(),
        })
    };
}

/// Bail with a `Function`-kinded [`EmissionError::Unsupported`]: the function
/// name has no τ emission arm (native or extension).
///
/// [`EmissionError::Unsupported`]: crate::transpiler_v2::error::EmissionError::Unsupported
#[macro_export]
macro_rules! bail_boundary_fn {
    ($name:expr, $reason:expr $(,)?) => {
        return Err($crate::transpiler_v2::error::EmissionError::Unsupported {
            kind: $crate::transpiler_v2::error::UnsupportedKind::Function,
            name: ($name).to_owned(),
            reason: ($reason).to_owned(),
        })
    };
}

/// Bail with a `ProtoShape`-kinded [`EmissionError::Unsupported`]: the input
/// proto / SQL shape has no lowering rule yet (the input never reached
/// [`CommonAst`]).
///
/// [`CommonAst`]: crate::transpiler_v2::ast::CommonAst
/// [`EmissionError::Unsupported`]: crate::transpiler_v2::error::EmissionError::Unsupported
#[macro_export]
macro_rules! bail_boundary_proto {
    ($shape:expr, $reason:expr $(,)?) => {
        return Err($crate::transpiler_v2::error::EmissionError::Unsupported {
            kind: $crate::transpiler_v2::error::UnsupportedKind::ProtoShape,
            name: ($shape).to_owned(),
            reason: ($reason).to_owned(),
        })
    };
}

/// Bail with [`AnalyzerError::UnsupportedRule`]: an analyzer rule has no
/// implementation yet.
///
/// [`AnalyzerError::UnsupportedRule`]: crate::transpiler_v2::analyzer::AnalyzerError::UnsupportedRule
#[macro_export]
macro_rules! bail_boundary_rule {
    ($rule:expr, $reason:expr $(,)?) => {
        return Err(
            $crate::transpiler_v2::analyzer::AnalyzerError::UnsupportedRule {
                rule: ($rule).to_owned(),
                reason: ($reason).to_owned(),
            },
        )
    };
}

/// Extension trait covering the closure-form of `bail_boundary_proto!`: the
/// missing-proto-field unwrap idiom that turns `Option::None` into a
/// `ProtoShape`-kinded [`EmissionError::Unsupported`]. `bail_boundary_proto!`
/// cannot be used inside a `|| { ... }` closure (its `return` would leave the
/// closure rather than the enclosing function), so the ~23 missing-field
/// sites across `parser_v2` and the connect-server converter use this trait
/// instead.
///
/// The wire message shape is byte-identical to the hand-written
/// `Option::ok_or_else` form it replaces — same
/// [`EmissionError::Unsupported`] variant, same `name`/`reason` fields, same
/// `.to_owned()` coercion. The `kind` is fixed to
/// [`UnsupportedKind::ProtoShape`].
///
/// # Example
///
/// ```ignore
/// use thunderduck_core::transpiler_v2::macros::ProtoFieldExt;
/// let x = obj.field.as_ref().require_proto("Shape", "Reason")?;
/// ```
///
/// [`EmissionError::Unsupported`]: crate::transpiler_v2::error::EmissionError::Unsupported
/// [`UnsupportedKind::ProtoShape`]: crate::transpiler_v2::error::UnsupportedKind::ProtoShape
pub trait ProtoFieldExt<T> {
    /// Unwrap `self`, returning a `ProtoShape`-kinded
    /// [`EmissionError::Unsupported`] with the given `shape` / `reason`
    /// when the option is `None`.
    ///
    /// [`EmissionError::Unsupported`]: crate::transpiler_v2::error::EmissionError::Unsupported
    fn require_proto(
        self,
        shape: &str,
        reason: &str,
    ) -> Result<T, crate::transpiler_v2::error::EmissionError>;
}

impl<T> ProtoFieldExt<T> for Option<T> {
    fn require_proto(
        self,
        shape: &str,
        reason: &str,
    ) -> Result<T, crate::transpiler_v2::error::EmissionError> {
        self.ok_or_else(|| crate::transpiler_v2::error::EmissionError::Unsupported {
            kind: crate::transpiler_v2::error::UnsupportedKind::ProtoShape,
            name: shape.to_owned(),
            reason: reason.to_owned(),
        })
    }
}
