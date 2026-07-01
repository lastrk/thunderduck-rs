//! Rearchitected transpiler (`τ`) — work in progress.
//!
//! This module is the home of the principled Spark → DuckDB transliterator
//! described in [`docs/thunderduck-rearchitect-ADRs.md`]. It is being built
//! *alongside* the existing [`crate::generator`] path and is selected at
//! startup via [`TranspilerPath`] (CLI `--transpiler v2` /
//! `THUNDERDUCK_TRANSPILER=v2`); the default remains the legacy path so
//! existing behavior is unchanged until a caller opts in.
//!
//! The Slice-C.1 pipeline is: [`lowering::lower`] (legacy `LogicalPlan` →
//! common AST) → [`analyzer::analyze`] (typing / nullability) →
//! [`emission::dispatch`] (declarative table → SQL). Slice C.2 replaces the
//! per-expression legacy delegation inside emission rows with per-function
//! declarative rows carrying projection casts for Spark parity.

use crate::error::ThunderduckError;
use crate::logical::LogicalPlan;
use crate::transpiler_v2::analyzer::BaseTypes;

/// Which transpiler path a request is routed to. Set once at startup.
///
/// Resolution: a CLI flag wins, otherwise [`TranspilerPath::from_env`] is
/// consulted, defaulting to [`TranspilerPath::Legacy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranspilerPath {
    /// The existing [`crate::generator::SqlGenerator`] path (default).
    #[default]
    Legacy,
    /// The rearchitected path in this module.
    V2,
}

impl TranspilerPath {
    /// Parse a path selector from a string (case-insensitive).
    ///
    /// `"v2"` selects [`TranspilerPath::V2`]; everything else — including
    /// `"legacy"`, the empty string, and any unrecognized value — selects
    /// [`TranspilerPath::Legacy`], so the safe default is the existing path.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "v2" => TranspilerPath::V2,
            _ => TranspilerPath::Legacy,
        }
    }

    /// Read `THUNDERDUCK_TRANSPILER` from the environment.
    /// Defaults to [`TranspilerPath::Legacy`] when unset or unrecognized.
    pub fn from_env() -> Self {
        Self::parse(&std::env::var("THUNDERDUCK_TRANSPILER").unwrap_or_default())
    }
}

/// Translate a [`LogicalPlan`] into DuckDB SQL via the rearchitected pipeline.
///
/// Composes: [`lowering::lower`] → [`analyzer::analyze`] →
/// [`emission::dispatch`]. Errors from each stage compose into
/// [`ThunderduckError::V2Lowering`] / [`ThunderduckError::V2Analyzer`] /
/// [`ThunderduckError::V2Emission`] via `#[from]`.
///
/// A [`ast::CommonOp::Punt`] is not an error at this level — it propagates
/// as [`analyzer::AnalyzerError::PuntedOperator`], which the caller
/// interprets as fallback-eligible (see `service.rs`'s dispatch wrapper).
/// This function itself does *not* fall back — the caller decides. Silent
/// fallback would mask bugs in the v2 pipeline.
pub fn generate(plan: &LogicalPlan, base_types: &BaseTypes) -> Result<String, ThunderduckError> {
    let ast = lowering::lower(plan)?;
    let typed = analyzer::analyze(ast, base_types)?;
    let emitted = emission::dispatch(&typed.root)?;
    Ok(emitted.into_string())
}

// ── INV activation hooks ──────────────────────────────────────────────────────

/// [INV2 §CV.5] emit-time tap. Renamed from `set_serializer_tap` in Slice
/// C.1 — the placeholder name reflected a role INV1's harness (in
/// `tests/integration/`) will fill; INV2's role is the v2-emitter's
/// single-writer check, which is what this hook actually implements.
///
/// Any tap installed here is fired by [`emission::dispatch`] on every
/// call, exactly once per dispatched op. Tests use this to prove
/// `dispatch` is the sole writer of v2 SQL bytes.
pub fn set_emit_tap(tap: fn(&[u8])) {
    EMIT_TAP.store(tap as usize, std::sync::atomic::Ordering::SeqCst);
}

/// Remove any installed tap. Tests call this to isolate themselves from
/// each other.
pub fn clear_emit_tap() {
    EMIT_TAP.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Runtime tap alias kept for source-compatibility with pre-Slice-C.1
/// callers. Delegates to [`set_emit_tap`].
///
/// Renamed to `set_emit_tap` in Slice C.1 to reflect its actual role
/// (v2-emitter single-writer check, INV2). Kept exported so out-of-tree
/// integration harnesses that already referred to the old name keep
/// compiling; new code should use `set_emit_tap`.
///
/// TODO INV1: the differential harness in `tests/integration/` owns
/// INV1's serialize-once-send-twice check; this hook covers INV2 only.
#[deprecated(since = "0.1.0", note = "use set_emit_tap")]
pub fn set_serializer_tap(tap: fn(&[u8])) {
    set_emit_tap(tap)
}

/// Fire the installed tap with the emitted bytes.
///
/// Called by [`emission::dispatch`]'s single `EmittedSql` constructor.
/// This module-private path is what makes INV2 a type-system invariant:
/// no other code can construct an `EmittedSql`, and thus no other code
/// can fire the tap.
pub(crate) fn fire_emit_tap(bytes: &[u8]) {
    let raw = EMIT_TAP.load(std::sync::atomic::Ordering::SeqCst);
    if raw != 0 {
        // SAFETY: `raw` was stored from `tap as usize` where `tap: fn(&[u8])`;
        // reversing that transmute is defined for identity function pointers.
        let tap: fn(&[u8]) = unsafe { std::mem::transmute(raw) };
        tap(bytes);
    }
}

static EMIT_TAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Reviewed exception list for [INV2 §CV.5]: every τ decision that is not
/// node-local after the analyzer pass MUST appear here under a stable name.
///
/// Empty today; entries are added by the rearchitecture work in ADR-007
/// (B-layer structural transliterations) and ADR-009 (C escape hatches).
/// The [`invariants::inv2_node_local_or_labeled_escape_hatch`] test asserts
/// every entry is non-empty and unique.
pub const C_ESCAPE_HATCHES: &[&str] = &[];

pub mod analyzer;
pub mod ast;
pub mod emission;
pub mod lowering;
pub mod provenance;

#[cfg(test)]
mod invariants;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_selects_v2_only_for_v2_token() {
        assert_eq!(TranspilerPath::parse("v2"), TranspilerPath::V2);
        assert_eq!(TranspilerPath::parse("V2"), TranspilerPath::V2);
        assert_eq!(TranspilerPath::parse("  v2  "), TranspilerPath::V2);
    }

    #[test]
    fn parse_defaults_to_legacy() {
        assert_eq!(TranspilerPath::parse(""), TranspilerPath::Legacy);
        assert_eq!(TranspilerPath::parse("legacy"), TranspilerPath::Legacy);
        assert_eq!(TranspilerPath::parse("garbage"), TranspilerPath::Legacy);
        assert_eq!(TranspilerPath::default(), TranspilerPath::Legacy);
    }

    #[test]
    fn generate_rejects_punted_plan_via_analyzer() {
        // A `Pivot` legacy plan lowers to `CommonOp::Punt`, which the
        // analyzer rejects with `PuntedOperator`. That surfaces as
        // `ThunderduckError::V2Analyzer` — the *caller* decides whether
        // to fall back to legacy.
        let plan = LogicalPlan::Pivot(crate::logical::Pivot {
            input: Box::new(LogicalPlan::SingleRow(crate::logical::SingleRowRelation)),
            grouping: vec![],
            pivot_col: crate::expression::Literal::int(0),
            pivot_values: vec![],
            aggregates: vec![],
        });
        let err = generate(&plan, &BaseTypes::new()).expect_err("Pivot must not lower");
        assert!(matches!(err, ThunderduckError::V2Analyzer(_)));
    }
}
