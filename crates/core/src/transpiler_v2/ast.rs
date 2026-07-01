//! Common AST for the rearchitected transpiler (ADR-003, ADR-004).
//!
//! **Placeholder.** ADR-003 defines this as the type both front-ends
//! normalize to. The unit-struct shape is intentional: it reserves the
//! symbol name so [INV5] and [INV7] tests can refer to it, while letting
//! the eventual implementation replace the body without renaming.
//!
//! TODO INV5/INV7: replace with the real common-AST per ADR-003.

/// Placeholder for the common AST. See module docs.
#[derive(Debug, Default)]
pub struct CommonAst;
