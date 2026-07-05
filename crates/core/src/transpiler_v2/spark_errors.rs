//! Spark ANSI-mode error classes τ emits at emission time.
//!
//! ADR-016 (ANSI error emulation contract) + ADR-006 (divide/mod zero-guards).
//! At τ's emission substrate, whenever a Spark-verbatim runtime error is
//! required (divide-by-zero, remainder-by-zero, invalid array index, ...),
//! the emitted DuckDB SQL wraps the offending sub-expression in a CASE guard
//! that calls DuckDB's `error(...)` scalar. This module names the closed set
//! of throw classes τ currently emits AND centralises the SQL-fragment
//! synthesis so future classes (cast overflow, `to_number` mismatch, ...)
//! plug in via one enum variant + one `throw_expr()` arm — no third
//! near-duplicate helper.
//!
//! **Pass 11 (OPP-J).** The const message strings live here now — adjacent
//! to the enum + helpers that consume them. `emission.rs` test-side
//! assertions (`sql.contains(INVALID_ARRAY_INDEX_MSG_HEAD)` etc.) import
//! them via `use super::spark_errors::{...}`.

/// Spark ANSI-mode `[DIVIDE_BY_ZERO]` runtime message text. Interpolated
/// into the DuckDB `error('[<CLASS>] <message>')` throw at emission time.
pub(crate) const DIVIDE_BY_ZERO_MSG: &str = "Division by zero. Use `try_divide` to tolerate divisor being 0 and return NULL instead. If necessary set \"spark.sql.ansi.enabled\" to \"false\" to bypass this error. SQLSTATE: 22012";

/// Spark ANSI-mode `[REMAINDER_BY_ZERO]` runtime message text. Interpolated
/// into the DuckDB `error('[<CLASS>] <message>')` throw at emission time.
pub(crate) const REMAINDER_BY_ZERO_MSG: &str = "Remainder by zero. Use `try_mod` to tolerate divisor being 0 and return NULL instead. If necessary set \"spark.sql.ansi.enabled\" to \"false\" to bypass this error. SQLSTATE: 22012";

// Spark 4.1's `INVALID_ARRAY_INDEX_IN_ELEMENT_AT` message is runtime-templated
// — the index value and array size are interpolated per row. The three
// fragments below bracket the two `||`-concatenated substitutions:
//
//   HEAD || (idx)::VARCHAR || MID || len(arr)::VARCHAR || TAIL
//
// The backticks around `try_element_at` are safe inside a SQL single-quoted
// string literal (only apostrophes need `''` escaping) — verified in DuckDB.
pub(crate) const INVALID_ARRAY_INDEX_MSG_HEAD: &str =
    "[INVALID_ARRAY_INDEX_IN_ELEMENT_AT] The index ";
pub(crate) const INVALID_ARRAY_INDEX_MSG_MID: &str = " is out of bounds. The array has ";
pub(crate) const INVALID_ARRAY_INDEX_MSG_TAIL: &str = " elements. Use `try_element_at` to tolerate accessing element at invalid index and return NULL instead. SQLSTATE: 22003";

/// Spark ANSI-mode throw classes τ emits at emission time.
///
/// Each variant carries just enough data to synthesise Spark's
/// `[<CLASS>] <message>` error text. Static-message variants
/// ([`Self::DivideByZero`], [`Self::RemainderByZero`]) are unit variants;
/// runtime-templated variants ([`Self::InvalidArrayIndex`]) carry the
/// already-rendered SQL sub-expressions that get concatenated into the
/// message at DuckDB evaluation time.
#[derive(Debug, Clone)]
pub(crate) enum SparkError {
    /// `[DIVIDE_BY_ZERO]` — Spark ANSI class for `a / 0`, `a div 0`.
    DivideByZero,
    /// `[REMAINDER_BY_ZERO]` — Spark ANSI class for `a % 0`, `mod(a, 0)`,
    /// `pmod(a, 0)`.
    RemainderByZero,
    /// `[INVALID_ARRAY_INDEX_IN_ELEMENT_AT]` — Spark ANSI class for
    /// `element_at(arr, i)` when `i == 0` or `abs(i) > len(arr)`. The
    /// message body is runtime-templated on the index value AND the array
    /// length, so both are carried as already-rendered SQL fragments.
    InvalidArrayIndex { idx_sql: String, arr_sql: String },
}

impl SparkError {
    /// The Spark error class name (the bracketed prefix Spark shows to
    /// clients). Used as the runtime classifier key in the ANSI error
    /// re-wrapping path.
    pub(crate) fn class(&self) -> &'static str {
        match self {
            Self::DivideByZero => "DIVIDE_BY_ZERO",
            Self::RemainderByZero => "REMAINDER_BY_ZERO",
            Self::InvalidArrayIndex { .. } => "INVALID_ARRAY_INDEX_IN_ELEMENT_AT",
        }
    }

    /// Build the DuckDB `error('[CLASS] <message>')` SQL fragment for this
    /// Spark error. Static-message variants Spark-escape any embedded
    /// apostrophes (`'` → `''`) to survive the single-quoted string literal;
    /// the runtime-templated `InvalidArrayIndex` variant concatenates HEAD /
    /// MID / TAIL literal fragments with `(idx)::VARCHAR` and
    /// `len(arr)::VARCHAR` at DuckDB evaluation time (the fragments contain
    /// backticks, which are safe inside single-quoted SQL literals — only
    /// apostrophes need `''` escaping).
    ///
    /// Emitted shapes are byte-identical to Passes 94/95's original
    /// `ansi_zero_guard` / `array_index_error_expr` helpers.
    pub(crate) fn throw_expr(&self) -> String {
        match self {
            Self::DivideByZero => {
                let escaped = DIVIDE_BY_ZERO_MSG.replace('\'', "''");
                format!("error('[{class}] {escaped}')", class = self.class())
            }
            Self::RemainderByZero => {
                let escaped = REMAINDER_BY_ZERO_MSG.replace('\'', "''");
                format!("error('[{class}] {escaped}')", class = self.class())
            }
            Self::InvalidArrayIndex { idx_sql, arr_sql } => {
                format!(
                    "error('{head}' || ({idx_sql})::VARCHAR || '{mid}' || len(({arr_sql}))::VARCHAR || '{tail}')",
                    head = INVALID_ARRAY_INDEX_MSG_HEAD,
                    mid = INVALID_ARRAY_INDEX_MSG_MID,
                    tail = INVALID_ARRAY_INDEX_MSG_TAIL,
                )
            }
        }
    }
}

/// Wrap `inner_sql` so that if `cond_sql` evaluates to TRUE, DuckDB raises
/// Spark's `[<CLASS>] <message>` error via [`SparkError::throw_expr`];
/// otherwise `inner_sql` is evaluated normally.
///
/// Emitted shape:
/// ```text
/// CASE WHEN {cond_sql} THEN {err.throw_expr()} ELSE {inner_sql} END
/// ```
///
/// Callers own the parenthesisation of `cond_sql` and `inner_sql` — this
/// helper does not add extra parens. That preserves byte-identity with the
/// original `ansi_zero_guard` shape (which took `divisor` as a bare
/// fragment, parenthesised it as `({divisor}) = 0`, and left `inner`
/// unparenthesised).
pub(crate) fn ansi_throw_if(cond_sql: &str, err: SparkError, inner_sql: &str) -> String {
    format!(
        "CASE WHEN {cond_sql} THEN {throw} ELSE {inner_sql} END",
        throw = err.throw_expr(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DivideByZero throw fragment is byte-identical to the original
    /// `ansi_zero_guard` inner call — `error('[DIVIDE_BY_ZERO] ...')` with
    /// apostrophe-escaping applied even though the current message has none
    /// (protects future messages that might).
    #[test]
    fn divide_by_zero_throw_expr_matches_legacy() {
        let got = SparkError::DivideByZero.throw_expr();
        assert!(
            got.starts_with("error('[DIVIDE_BY_ZERO] "),
            "expected `[DIVIDE_BY_ZERO]` prefix, got: {got}"
        );
        assert!(got.ends_with("SQLSTATE: 22012')"), "got: {got}");
    }

    /// RemainderByZero throw fragment likewise byte-identical.
    #[test]
    fn remainder_by_zero_throw_expr_matches_legacy() {
        let got = SparkError::RemainderByZero.throw_expr();
        assert!(
            got.starts_with("error('[REMAINDER_BY_ZERO] "),
            "expected `[REMAINDER_BY_ZERO]` prefix, got: {got}"
        );
        assert!(got.ends_with("SQLSTATE: 22012')"), "got: {got}");
    }

    /// InvalidArrayIndex throw fragment weaves HEAD/MID/TAIL literals with
    /// the runtime `(idx)::VARCHAR` / `len((arr))::VARCHAR` casts — same
    /// concatenation shape the old `array_index_error_expr` produced.
    #[test]
    fn invalid_array_index_throw_expr_matches_legacy() {
        let err = SparkError::InvalidArrayIndex {
            idx_sql: "1".to_owned(),
            arr_sql: "tags".to_owned(),
        };
        assert_eq!(
            err.throw_expr(),
            "error('[INVALID_ARRAY_INDEX_IN_ELEMENT_AT] The index ' || (1)::VARCHAR \
             || ' is out of bounds. The array has ' || len((tags))::VARCHAR \
             || ' elements. Use `try_element_at` to tolerate accessing element at invalid index and return NULL instead. SQLSTATE: 22003')"
        );
    }

    /// `ansi_throw_if` reproduces the legacy `ansi_zero_guard` shape:
    /// `CASE WHEN ({divisor}) = 0 THEN error('[CLASS] msg') ELSE {inner} END`.
    /// Caller supplies the parenthesised condition; helper adds nothing.
    #[test]
    fn ansi_throw_if_matches_legacy_zero_guard_shape() {
        let sql = ansi_throw_if("(b) = 0", SparkError::DivideByZero, "(6) / (b)");
        assert!(
            sql.starts_with("CASE WHEN (b) = 0 THEN error('[DIVIDE_BY_ZERO] "),
            "got: {sql}"
        );
        assert!(sql.ends_with("ELSE (6) / (b) END"), "got: {sql}");
    }
}
