//! Type & nullability analyzer (the "A pass") — ADR-005, ADR-006.

use crate::transpiler_v2::ast::CommonAst;

/// Inference-only smoke test target required by [INV4 §CV.5].
///
/// **Stub.** No-op today; the analyzer is unimplemented. Exists so the
/// `core_v2` differential suite can name it as a prerequisite, and so
/// [`super::invariants::inv4_inference_validated_in_isolation`] has a
/// concrete symbol to reference.
///
/// TODO INV4: replace body once the analyzer (ADR-005/006) ships.
pub fn inference_smoke() {}

/// Post-analyzer predicate for [INV5 §CV.5]: returns `true` iff no node in
/// `ast` carries [`crate::types::DataType::Unresolved`].
///
/// **Stub.** The walker is empty today because `CommonAst` is a unit struct.
/// The predicate runs vacuously over an empty AST and returns `true`. The
/// *signature* is fixed so the future walker plugs in without renaming.
///
/// TODO INV5: walk every node once `CommonAst` carries real shape (ADR-003).
pub fn has_resolved_schema(_ast: &CommonAst) -> bool {
    true
}
