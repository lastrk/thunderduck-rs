//! τ helper — Spark `struct(...)` field-name derivation.
//!
//! Spark 4.1.1's `pyspark.sql.functions.struct(*cols)` compiles to Catalyst
//! `CreateStruct(children)`. Each child expression contributes a field name
//! according to a fixed precedence: alias > column reference > synthetic
//! `col{i+1}` fallback. This module is the single owner of that derivation.
//!
//! Called by intra-τ `emission::render_function_call` and
//! `expression::function_call_data_type`.
//!
//! **INV3:** no imports from `crate::generator` / `crate::functions`.
//! **INV10:** imports only from `crate::types` (transitively via `super`)
//! plus intra-τ modules (`super::expression`).
//!
//! Pure function of `&Expression`; no schema dependency, no side effects.

use super::expression::Expression;

/// Derive the Spark-parity field name for the i-th argument of
/// `struct(*args)`.
///
/// Precedence (first match wins) — mirrors Catalyst's
/// `Alias.tryUnaliasedName`:
/// 1. `Alias(inner, name)` → `name`.
/// 2. `ColumnReference { name, .. }` → `name`.
/// 3. `UnresolvedColumn { name, .. }` → `name` (defensive; usually resolved
///    into `ColumnReference` before emission).
/// 4. Anything else → `col{i+1}` (1-indexed) — Spark's documented fallback.
///    This includes `Literal(String(_))`: Spark's `struct(lit("colA"))`
///    yields `struct<col1: string>`, not a field named `"colA"`.
///
/// Never returns an empty string.
pub(super) fn derive_struct_field_name(arg: &Expression, i: usize) -> String {
    match arg {
        Expression::Alias(a) => a.alias.clone(),
        Expression::ColumnReference(c) => c.name.clone(),
        Expression::UnresolvedColumn(u) => u.name.clone(),
        _ => format!("col{}", i + 1),
    }
}

/// Derive the Spark-parity field name for the i-th argument of
/// `arrays_zip(*args)`.
///
/// Same alias > column-ref > unresolved-column precedence as
/// [`derive_struct_field_name`], but the fallback for anything else is the
/// positional integer string `"0"`, `"1"`, ... (0-indexed) — Spark uses
/// integer strings, not `col{i+1}`, for `arrays_zip` specifically. Shared by
/// `expression::function_call_data_type` (schema side) and
/// `emission::render_function_call` (SQL side); the two MUST agree or the
/// wire schema desyncs from the emitted struct fields. Corpus anchor:
/// `arr-012`.
pub(super) fn derive_zip_field_name(arg: &Expression, i: usize) -> String {
    match arg {
        Expression::Alias(a) => a.alias.clone(),
        Expression::ColumnReference(c) => c.name.clone(),
        Expression::UnresolvedColumn(u) => u.name.clone(),
        _ => i.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::expression::{
        AliasExpression, BinaryExpression, BinaryOp, ColumnReference, Literal, LiteralValue,
        UnresolvedColumn,
    };
    use super::*;
    use crate::types::DataType;

    fn col_ref(name: &str) -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: name.to_owned(),
            qualifier: None,
            data_type: DataType::String,
            nullable: true,
            expr_id: None,
        })
    }

    fn unresolved(name: &str) -> Expression {
        Expression::UnresolvedColumn(UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: None,
        })
    }

    fn string_lit(s: &str) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::String(s.to_owned()),
            data_type: DataType::String,
        })
    }

    fn int_lit(v: i32) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Int(v),
            data_type: DataType::Integer,
        })
    }

    /// §9 test 6 — direct helper precedence check across all four branches.
    #[test]
    fn derive_struct_field_name_precedence() {
        // (1) Alias wins over inner ColumnReference.
        let aliased = Expression::Alias(AliasExpression {
            expr: Box::new(col_ref("name")),
            alias: "who".to_owned(),
        });
        assert_eq!(derive_struct_field_name(&aliased, 0), "who");

        // (2) ColumnReference → column name.
        assert_eq!(derive_struct_field_name(&col_ref("age"), 1), "age");

        // (3) UnresolvedColumn → column name (defensive).
        assert_eq!(derive_struct_field_name(&unresolved("dept"), 2), "dept");

        // (4) String literal → col{i+1} fallback (matches Spark's
        //     `Alias.tryUnaliasedName` for `struct(lit("hello"))`, which
        //     yields `struct<col4: string>`, NOT a field named `"hello"`).
        assert_eq!(derive_struct_field_name(&string_lit("hello"), 3), "col4");

        // (4) Non-string literal → col{i+1} fallback.
        assert_eq!(derive_struct_field_name(&int_lit(42), 4), "col5");

        // (4) Arbitrary computed expression → col{i+1} fallback.
        let computed = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(col_ref("a")),
            right: Box::new(int_lit(1)),
        });
        assert_eq!(derive_struct_field_name(&computed, 0), "col1");
        assert_eq!(derive_struct_field_name(&computed, 5), "col6");
    }

    /// Fallback is 1-indexed regardless of position.
    #[test]
    fn derive_struct_field_name_fallback_is_one_indexed() {
        let e = Expression::Literal(Literal {
            value: LiteralValue::Null,
            data_type: DataType::Null,
        });
        assert_eq!(derive_struct_field_name(&e, 0), "col1");
        assert_eq!(derive_struct_field_name(&e, 9), "col10");
    }
}
