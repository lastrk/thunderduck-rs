//! Trailing multi-column alias stripper for `F.expr(...)` fragments.
//!
//! Spark's generator functions (`stack`, `explode` on maps, `posexplode`,
//! `inline`, `json_tuple`) accept a Hive-style multi-column alias in raw SQL:
//!
//! ```text
//! stack(2, 'age', CAST(age AS DOUBLE), 'salary', salary) AS (metric, value)
//! ```
//!
//! sqlparser-rs 0.61's `SelectItem::ExprWithAlias` carries a single `Ident`
//! for the alias and its projection reader hard-fails at the `(` after `AS`
//! with `"Expected: an identifier after AS, found: ("`. The `Dialect` trait
//! exposes no hook to customise select-item alias parsing, so we cannot
//! extend `SparkDialect` to accept the syntax.
//!
//! [`strip_trailing_multi_alias`] operates on the sqlparser-rs
//! [`Tokenizer`](sqlparser::tokenizer::Tokenizer) — NOT on raw SQL text.
//! Reading tokens (not bytes) keeps τ within CLAUDE.md rule 1 (no string
//! manipulation on SQL text). If a trailing top-level
//! `AS ( ident1, ident2, ..., identN )` matches with N >= 2, the tail is
//! removed and the alias list returned; the shortened token stream is
//! rendered back through [`sqlparser::tokenizer::Token`]'s `Display` impl,
//! which is round-trip stable for the shapes τ ingests here.
//!
//! Scope: piv-006 (`stack`). Follow-up work will extend the analyzer /
//! emission side to support `posexplode` / `explode(map)` / `inline` /
//! `json_tuple` multi-alias from the SQL path — the stripper is generic
//! but only `stack` currently splices back cleanly (`parse_expression` in
//! `mod.rs` gates the wrap-in-`stack_multi_alias` step).

use sqlparser::tokenizer::{Token, Tokenizer};

use crate::transpiler_v2::error::{EmissionError, UnsupportedKind};

use super::dialect::SparkDialect;

/// If `expr_sql` ends with a Spark-style multi-column alias
/// `AS ( ident1, ident2, ..., identN )` (N >= 2) at parenthesis depth 0,
/// return `(stripped_sql, Some(aliases))`. Otherwise return
/// `(expr_sql.to_owned(), None)`.
///
/// Tokenizer errors surface as [`EmissionError::Unsupported`] with
/// `kind: ProtoShape` — matching the wrapping call in
/// [`super::SparkSqlParserV2::parse`].
pub(super) fn strip_trailing_multi_alias(
    expr_sql: &str,
) -> Result<(String, Option<Vec<String>>), EmissionError> {
    let dialect = SparkDialect::default();
    let tokens =
        Tokenizer::new(&dialect, expr_sql)
            .tokenize()
            .map_err(|e| EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: "sql::parse_error".to_owned(),
                reason: e.to_string(),
            })?;

    // Last non-whitespace token must be `)`; walk back to find its matching `(`
    // at depth 0.
    let last_significant = tokens
        .iter()
        .rposition(|t| !matches!(t, Token::Whitespace(_)));
    let last_close = match last_significant {
        Some(idx) if matches!(tokens[idx], Token::RParen) => idx,
        _ => return Ok((expr_sql.to_owned(), None)),
    };

    let mut depth = 0i32;
    let mut open_idx: Option<usize> = None;
    for i in (0..=last_close).rev() {
        match &tokens[i] {
            Token::RParen => depth += 1,
            Token::LParen => {
                depth -= 1;
                if depth == 0 {
                    open_idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let open_idx = match open_idx {
        Some(i) => i,
        None => return Ok((expr_sql.to_owned(), None)),
    };

    // Token immediately before the `(`, skipping whitespace, must be `AS`.
    let mut as_idx: Option<usize> = None;
    for i in (0..open_idx).rev() {
        if !matches!(tokens[i], Token::Whitespace(_)) {
            as_idx = Some(i);
            break;
        }
    }
    let as_idx = match as_idx {
        Some(i) => i,
        None => return Ok((expr_sql.to_owned(), None)),
    };
    let is_as = matches!(
        &tokens[as_idx],
        Token::Word(w) if w.value.eq_ignore_ascii_case("AS")
    );
    if !is_as {
        return Ok((expr_sql.to_owned(), None));
    }

    // Interior of `( ... )` must be `ident (, ident)+` — at least two.
    // `Token::Word` covers both unquoted keywords (`value`, `count`, ...)
    // and back-tick / double-quoted identifiers.
    let interior: Vec<&Token> = tokens[(open_idx + 1)..last_close]
        .iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect();
    if interior.is_empty() {
        return Ok((expr_sql.to_owned(), None));
    }
    let mut aliases: Vec<String> = Vec::new();
    let mut expect_ident = true;
    for t in interior {
        if expect_ident {
            match t {
                Token::Word(w) => aliases.push(w.value.clone()),
                _ => return Ok((expr_sql.to_owned(), None)),
            }
            expect_ident = false;
        } else {
            if !matches!(t, Token::Comma) {
                return Ok((expr_sql.to_owned(), None));
            }
            expect_ident = true;
        }
    }
    if expect_ident || aliases.len() < 2 {
        // Trailing comma, or only one alias — single-alias `AS x` is handled
        // by sqlparser natively and never reaches this stripper.
        return Ok((expr_sql.to_owned(), None));
    }

    // Reconstruct the SQL up to (but excluding) the `AS` keyword, dropping
    // any whitespace that immediately preceded it — sqlparser is tolerant of
    // either shape, but the tighter form keeps error messages readable.
    let mut trim_end = as_idx;
    while trim_end > 0 && matches!(tokens[trim_end - 1], Token::Whitespace(_)) {
        trim_end -= 1;
    }
    let mut stripped = String::new();
    for t in &tokens[..trim_end] {
        stripped.push_str(&t.to_string());
    }
    Ok((stripped, Some(aliases)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_two_column_alias() {
        let (sql, aliases) = strip_trailing_multi_alias(
            "stack(2, 'age', CAST(age AS DOUBLE), 'salary', salary) as (metric, value)",
        )
        .expect("token stream must tokenize");
        assert_eq!(aliases, Some(vec!["metric".to_owned(), "value".to_owned()]));
        assert_eq!(
            sql,
            "stack(2, 'age', CAST(age AS DOUBLE), 'salary', salary)"
        );
    }

    #[test]
    fn strips_three_column_alias_with_mixed_case_as() {
        let (sql, aliases) =
            strip_trailing_multi_alias("gen(x) AS (a, b, c)").expect("token stream must tokenize");
        assert_eq!(
            aliases,
            Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
        );
        assert_eq!(sql, "gen(x)");
    }

    #[test]
    fn ignores_single_column_alias() {
        // `AS x` is sqlparser-native; this stripper must not touch it.
        let (sql, aliases) = strip_trailing_multi_alias("age + 1 as age1").expect("must tokenize");
        assert_eq!(aliases, None);
        assert_eq!(sql, "age + 1 as age1");
    }

    #[test]
    fn ignores_inner_cast_as() {
        // `AS DOUBLE` inside a CAST is at paren depth > 0 and must not be
        // mistaken for the outer alias marker.
        let (sql, aliases) =
            strip_trailing_multi_alias("CAST(age AS DOUBLE)").expect("must tokenize");
        assert_eq!(aliases, None);
        assert_eq!(sql, "CAST(age AS DOUBLE)");
    }

    #[test]
    fn ignores_bare_function_call() {
        let (sql, aliases) =
            strip_trailing_multi_alias("stack(2, 'a', 1, 'b', 2)").expect("must tokenize");
        assert_eq!(aliases, None);
        assert_eq!(sql, "stack(2, 'a', 1, 'b', 2)");
    }

    #[test]
    fn ignores_non_ident_inside_alias_parens() {
        // `AS (1, 2)` is not a valid multi-column alias.
        let (sql, aliases) = strip_trailing_multi_alias("f(x) as (1, 2)").expect("must tokenize");
        assert_eq!(aliases, None);
        assert_eq!(sql, "f(x) as (1, 2)");
    }
}
