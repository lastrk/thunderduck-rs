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
//! Message fragments live beside the enum and helpers that consume them.

use super::emission::escape_sql_string;

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

// Spark 4.1's `INVALID_FORMAT.MISMATCH_INPUT` message is runtime-templated
// on the input value (rendered VARCHAR at eval time) — only the input string
// requires per-row interpolation, so a single HEAD / TAIL pair brackets the
// `||`-concatenated `(input)::VARCHAR` substitution. The format literal is
// baked into HEAD at emission time. Shape:
//
//   HEAD || (input_sql)::VARCHAR || TAIL
//
// Spark's message is byte-verbatim (no template parens around `<input>` — the
// parens seen in Spark diagnostics for input `(9.99)` are the input's own
// characters, not the template):
//   [INVALID_FORMAT.MISMATCH_INPUT] The format is invalid: <fmt>.
//   The input "STRING" <input> does not match the format. SQLSTATE: 42601
pub(crate) const INVALID_FORMAT_MISMATCH_MSG_HEAD_PREFIX: &str =
    "[INVALID_FORMAT.MISMATCH_INPUT] The format is invalid: ";
pub(crate) const INVALID_FORMAT_MISMATCH_MSG_HEAD_SUFFIX: &str = ". The input \"STRING\" ";
pub(crate) const INVALID_FORMAT_MISMATCH_MSG_TAIL: &str =
    " does not match the format. SQLSTATE: 42601";

// Spark 4.1's `INVALID_ARRAY_INDEX` message (raised by `GetArrayItem`, i.e. the
// SQL `arr[i]` subscript, on OOB / negative index in ANSI mode) is likewise
// runtime-templated. It is a SIBLING of `INVALID_ARRAY_INDEX_IN_ELEMENT_AT`
// above but a DISTINCT class: different class name and the message references
// the SQL `get()` function rather than `try_element_at`. Fragment shape mirrors
// the element_at trio:
//
//   HEAD || (idx)::VARCHAR || MID || len(arr)::VARCHAR || TAIL
pub(crate) const INVALID_ARRAY_INDEX_SUBSCRIPT_MSG_HEAD: &str = "[INVALID_ARRAY_INDEX] The index ";
pub(crate) const INVALID_ARRAY_INDEX_SUBSCRIPT_MSG_MID: &str = " is out of bounds. The array has ";
pub(crate) const INVALID_ARRAY_INDEX_SUBSCRIPT_MSG_TAIL: &str = " elements. Use the SQL function `get()` to tolerate accessing element at invalid index and return NULL instead. SQLSTATE: 22003";

// Spark 4.1's `INVALID_PARAMETER_VALUE.BIT_POSITION_RANGE` message
// (`bit_get`/`getbit(x, pos)` when `pos < 0 || pos >= bit_width(x)`) is
// runtime-templated on the invalid `pos` value; the bit-width upper bound is
// baked into HEAD at emission time (it depends only on arg0's static
// integral type). Shape:
//
//   HEAD{upper} || (pos)::VARCHAR || TAIL
//
// The message names `bit_get` even when invoked via the `getbit` alias —
// deliberately not parameterized: the differential oracle compares only the
// leading `[CLASS]` token, and whether Spark's own message canonicalizes the
// alias is unverified (probe before "fixing"; a wrong guess would diverge
// the message body for zero oracle benefit).
pub(crate) const BIT_POSITION_RANGE_MSG_HEAD_PREFIX: &str =
    "[INVALID_PARAMETER_VALUE.BIT_POSITION_RANGE] The value of parameter(s) `pos` in `bit_get` is invalid: expects an integer value in [0, ";
pub(crate) const BIT_POSITION_RANGE_MSG_HEAD_SUFFIX: &str = "), but got ";
pub(crate) const BIT_POSITION_RANGE_MSG_TAIL: &str = ". SQLSTATE: 22023";

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
    /// `[INVALID_FORMAT.MISMATCH_INPUT]` — Spark ANSI class for
    /// `to_number(str, fmt)` when the input string cannot be parsed under
    /// the format template. The format literal is baked into the emitted
    /// message at emission time; the input value is runtime-interpolated as
    /// a rendered SQL fragment.
    InvalidFormatMismatch { fmt: String, input_sql: String },
    /// `[INVALID_ARRAY_INDEX]` — Spark ANSI class for the SQL subscript
    /// `arr[i]` (`GetArrayItem`) when `i < 0` or `i >= len(arr)`. Distinct
    /// from [`Self::InvalidArrayIndex`] (its `_IN_ELEMENT_AT` sibling): a
    /// different class name and a message that references the SQL `get()`
    /// function rather than `try_element_at`. The message body is
    /// runtime-templated on the index value AND the array length, so both
    /// are carried as already-rendered SQL fragments.
    InvalidArrayIndexSubscript { idx_sql: String, arr_sql: String },
    /// `[INVALID_PARAMETER_VALUE.BIT_POSITION_RANGE]` — Spark ANSI class for
    /// `bit_get`/`getbit(x, pos)` when `pos < 0` or `pos >= bit_width(x)`.
    /// `upper` (the type's bit-width: 8/16/32/64) is baked at emission time;
    /// the bad `pos` value is runtime-interpolated as `(value_sql)::VARCHAR`.
    BitPositionRange { upper: u32, value_sql: String },
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
            Self::InvalidFormatMismatch { .. } => "INVALID_FORMAT.MISMATCH_INPUT",
            Self::InvalidArrayIndexSubscript { .. } => "INVALID_ARRAY_INDEX",
            Self::BitPositionRange { .. } => "INVALID_PARAMETER_VALUE.BIT_POSITION_RANGE",
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
    pub(crate) fn throw_expr(&self) -> String {
        match self {
            Self::DivideByZero | Self::RemainderByZero => {
                let msg = if matches!(self, Self::DivideByZero) {
                    DIVIDE_BY_ZERO_MSG
                } else {
                    REMAINDER_BY_ZERO_MSG
                };
                let escaped = escape_sql_string(msg);
                format!("error('[{class}] {escaped}')", class = self.class())
            }
            Self::InvalidArrayIndex { idx_sql, arr_sql }
            | Self::InvalidArrayIndexSubscript { idx_sql, arr_sql } => {
                let (head, mid, tail) = if matches!(self, Self::InvalidArrayIndex { .. }) {
                    (
                        INVALID_ARRAY_INDEX_MSG_HEAD,
                        INVALID_ARRAY_INDEX_MSG_MID,
                        INVALID_ARRAY_INDEX_MSG_TAIL,
                    )
                } else {
                    (
                        INVALID_ARRAY_INDEX_SUBSCRIPT_MSG_HEAD,
                        INVALID_ARRAY_INDEX_SUBSCRIPT_MSG_MID,
                        INVALID_ARRAY_INDEX_SUBSCRIPT_MSG_TAIL,
                    )
                };
                format!(
                    "error('{head}' || ({idx_sql})::VARCHAR || '{mid}' || len(({arr_sql}))::VARCHAR || '{tail}')"
                )
            }
            Self::InvalidFormatMismatch { fmt, input_sql } => {
                // The format literal is baked into HEAD (SQL-escape any
                // embedded apostrophes for the enclosing single-quoted
                // string literal). The input value is interpolated at
                // eval time via `(input_sql)::VARCHAR`.
                let fmt_escaped = escape_sql_string(fmt);
                format!(
                    "error('{prefix}{fmt_escaped}{suffix}' || ({input_sql})::VARCHAR || '{tail}')",
                    prefix = INVALID_FORMAT_MISMATCH_MSG_HEAD_PREFIX,
                    suffix = INVALID_FORMAT_MISMATCH_MSG_HEAD_SUFFIX,
                    tail = INVALID_FORMAT_MISMATCH_MSG_TAIL,
                )
            }
            Self::BitPositionRange { upper, value_sql } => {
                // The bit-width upper bound is baked into HEAD at emission
                // time (backticks are literal inside the single-quoted SQL
                // string — no apostrophes to escape). The invalid `pos`
                // value is interpolated at eval time via
                // `(value_sql)::VARCHAR`.
                format!(
                    "error('{prefix}{upper}{suffix}' || ({value_sql})::VARCHAR || '{tail}')",
                    prefix = BIT_POSITION_RANGE_MSG_HEAD_PREFIX,
                    suffix = BIT_POSITION_RANGE_MSG_HEAD_SUFFIX,
                    tail = BIT_POSITION_RANGE_MSG_TAIL,
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

    /// The throw fragment has Spark's class prefix and message text.
    #[test]
    fn divide_by_zero_throw_expr_matches_legacy() {
        let got = SparkError::DivideByZero.throw_expr();
        assert!(
            got.starts_with("error('[DIVIDE_BY_ZERO] "),
            "expected `[DIVIDE_BY_ZERO]` prefix, got: {got}"
        );
        assert!(got.ends_with("SQLSTATE: 22012')"), "got: {got}");
    }

    /// The throw fragment has Spark's class prefix and message text.
    #[test]
    fn remainder_by_zero_throw_expr_matches_legacy() {
        let got = SparkError::RemainderByZero.throw_expr();
        assert!(
            got.starts_with("error('[REMAINDER_BY_ZERO] "),
            "expected `[REMAINDER_BY_ZERO]` prefix, got: {got}"
        );
        assert!(got.ends_with("SQLSTATE: 22012')"), "got: {got}");
    }

    /// The fragment interpolates the index and array length at evaluation
    /// time.
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

    /// InvalidFormatMismatch throw fragment bakes the format literal into
    /// HEAD (apostrophes escaped) and interpolates the input value as
    /// `(input_sql)::VARCHAR` at eval time. The rendered message matches
    /// Spark 4.1's `[INVALID_FORMAT.MISMATCH_INPUT]` byte-verbatim.
    #[test]
    fn invalid_format_mismatch_throw_expr_matches_spark_message() {
        let err = SparkError::InvalidFormatMismatch {
            fmt: "9,999.99".to_owned(),
            input_sql: "num_str".to_owned(),
        };
        // Byte-verbatim vs Spark 4.1's INVALID_FORMAT.MISMATCH_INPUT template:
        // no template parens around <input> — for input `(9.99)` the parens
        // in Spark's diagnostic come from the input value itself.
        assert_eq!(
            err.throw_expr(),
            "error('[INVALID_FORMAT.MISMATCH_INPUT] The format is invalid: 9,999.99. \
             The input \"STRING\" ' || (num_str)::VARCHAR || \
             ' does not match the format. SQLSTATE: 42601')"
        );
        assert_eq!(err.class(), "INVALID_FORMAT.MISMATCH_INPUT");
    }

    /// Apostrophes in the format literal must be `''`-escaped so they
    /// survive the single-quoted SQL string literal wrapping the message.
    #[test]
    fn invalid_format_mismatch_escapes_apostrophes_in_fmt() {
        let err = SparkError::InvalidFormatMismatch {
            fmt: "a'b".to_owned(),
            input_sql: "x".to_owned(),
        };
        assert!(
            err.throw_expr().contains("invalid: a''b."),
            "got: {}",
            err.throw_expr()
        );
    }

    /// `InvalidArrayIndexSubscript` (the `GetArrayItem` / `arr[i]` throw)
    /// carries the distinct `[INVALID_ARRAY_INDEX]` class and the `get()`
    /// message body — NOT the `_IN_ELEMENT_AT` / `try_element_at` sibling.
    #[test]
    fn invalid_array_index_subscript_throw_expr_uses_get_class_and_message() {
        let err = SparkError::InvalidArrayIndexSubscript {
            idx_sql: "5".to_owned(),
            arr_sql: "arr".to_owned(),
        };
        assert_eq!(err.class(), "INVALID_ARRAY_INDEX");
        assert_eq!(
            err.throw_expr(),
            "error('[INVALID_ARRAY_INDEX] The index ' || (5)::VARCHAR \
             || ' is out of bounds. The array has ' || len((arr))::VARCHAR \
             || ' elements. Use the SQL function `get()` to tolerate accessing element at invalid index and return NULL instead. SQLSTATE: 22003')"
        );
    }

    /// `BitPositionRange` throw fragment bakes the type's bit-width into
    /// HEAD and interpolates the invalid `pos` value at eval time via
    /// `(value_sql)::VARCHAR`. Matches Spark 4.1's
    /// `INVALID_PARAMETER_VALUE.BIT_POSITION_RANGE` message shape (probe-
    /// confirmed leading token, live Spark 4.1.1).
    #[test]
    fn bit_position_range_throw_expr_matches_spark_message() {
        let err = SparkError::BitPositionRange {
            upper: 64,
            value_sql: "64".to_owned(),
        };
        assert_eq!(err.class(), "INVALID_PARAMETER_VALUE.BIT_POSITION_RANGE");
        let got = err.throw_expr();
        assert!(
            got.starts_with(
                "error('[INVALID_PARAMETER_VALUE.BIT_POSITION_RANGE] The value of \
                 parameter(s) `pos` in `bit_get` is invalid: expects an integer value in [0, "
            ),
            "got: {got}"
        );
        assert!(got.contains("[0, 64)"), "got: {got}");
        assert!(got.contains("(64)::VARCHAR"), "got: {got}");
        assert!(got.ends_with("SQLSTATE: 22023')"), "got: {got}");
    }

    /// The caller supplies the parenthesized condition and inner expression.
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
