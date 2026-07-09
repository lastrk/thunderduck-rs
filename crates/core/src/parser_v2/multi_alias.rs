//! Multi-column alias handling for Spark's generator functions.
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
//! Two entry points serve different τ paths:
//!
//! - [`strip_trailing_multi_alias`] — operates on `F.expr()` fragments
//!   (trailing occurrence only). Used by `parse_expression` in `mod.rs`.
//!
//! - [`rewrite_multi_aliases`] — operates on full SQL statements (any
//!   number of occurrences at depth 0). Used by `SparkSqlParserV2::parse`
//!   in `mod.rs`. Each `AS (ident, ident+)` is replaced with a sentinel
//!   `AS __td_multi_alias_<N>` and the alias lists are returned for
//!   post-lowering splicing by [`splice_multi_aliases`].
//!
//! Both functions use the sqlparser-rs [`Tokenizer`](sqlparser::tokenizer::Tokenizer)
//! — NOT raw SQL text. Reading tokens (not bytes) keeps τ within CLAUDE.md
//! rule 1 (no string manipulation on SQL text).

use sqlparser::tokenizer::{Token, Tokenizer};

use crate::bail_boundary_proto;
use crate::transpiler_v2::ast::{CommonAst, CommonOp};
use crate::transpiler_v2::error::{EmissionError, UnsupportedKind};
use crate::transpiler_v2::expression::{
    AliasExpression, Expression, FunctionCall, Literal, LiteralValue,
};
use crate::types::DataType;

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

// ── Full-SQL multi-alias rewrite ────────────────────────────────────────────

/// Sentinel prefix used by [`rewrite_multi_aliases`] / [`splice_multi_aliases`].
/// The format is `__td_multi_alias_<N>` where N is a 0-based index into the
/// returned alias-lists vec.
const SENTINEL_PREFIX: &str = "__td_multi_alias_";

/// Cheap byte-level pre-check: does `sql` contain the literal (ASCII
/// case-insensitive) sequence "as" followed by optional whitespace and `(`,
/// anywhere? Every genuine multi-alias occurrence requires this sequence, so
/// its absence proves there is nothing to rewrite. A false positive (e.g. the
/// sequence appearing inside a string literal, or as `CAST(x AS DOUBLE)`
/// where a later unrelated `(` follows) is harmless: [`rewrite_multi_aliases`]
/// still runs its real tokenizer-based check and correctly finds nothing.
fn might_contain_as_paren(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i].eq_ignore_ascii_case(&b'a') && bytes[i + 1].eq_ignore_ascii_case(&b's') {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Scan a full SQL statement for all depth-0 `AS ( ident, ident+ )` occurrences
/// (N >= 2 identifiers) and replace each with a sentinel single-identifier alias
/// `AS __td_multi_alias_<N>`. Returns the rewritten SQL and the collected alias
/// lists (indexed by N).
///
/// Shapes that must NOT match (false-positive audit):
/// - `WITH a AS (SELECT ...)` — Word after AS is a keyword/ident, not LParen
///   directly; and even if we see `AS (`, interior starts with `SELECT`, not an
///   ident-comma-ident sequence.
/// - `WITH t(k,v) AS (...)` — parens follow `t`, not `AS`.
/// - `WINDOW w AS (PARTITION BY ...)` — interior has `PARTITION`, not ident-comma.
/// - `CAST(x AS DOUBLE)` — AS at depth > 0 (inside CAST parens).
/// - `FROM (...) AS t(a,b)` — AS is followed by a Word, not LParen.
/// - `AS (k)` — single ident, N < 2, rejected.
///
/// Returns `(sql, vec![])` when no multi-alias is found (zero-cost: the caller
/// skips `splice_multi_aliases` entirely).
pub(super) fn rewrite_multi_aliases(
    sql: &str,
) -> Result<(String, Vec<Vec<String>>), EmissionError> {
    // Fast pre-check: every multi-alias occurrence requires the literal
    // (case-insensitive) byte sequence "as" immediately followed by
    // whitespace-then-`(`, somewhere in the text. Skip the full tokenization
    // pass — otherwise paid on every parse, including the overwhelming
    // majority of queries with no multi-alias — when that sequence can't
    // occur. A false positive (e.g. inside a string literal) just falls
    // through to the real tokenizer below, which resolves it correctly; this
    // check only ever skips work, never changes behavior.
    if !might_contain_as_paren(sql) {
        return Ok((sql.to_owned(), Vec::new()));
    }

    let dialect = SparkDialect;
    let tokens =
        Tokenizer::new(&dialect, sql)
            .tokenize()
            .map_err(|e| EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: "sql::parse_error".to_owned(),
                reason: e.to_string(),
            })?;

    // Collect the token indices of every depth-0 `AS ( ident, ident+ )`
    // occurrence, along with the alias list for each.
    let mut occurrences: Vec<Occurrence> = Vec::new();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            Token::Word(w) if depth == 0 && w.value.eq_ignore_ascii_case("AS") => {
                // Look ahead, skipping whitespace, for `(`.
                if let Some(occ) = try_match_multi_alias_at(&tokens, i) {
                    let skip_to = occ.rparen_idx + 1;
                    occurrences.push(occ);
                    // Skip past the `)` so we don't re-enter the interior.
                    i = skip_to;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if occurrences.is_empty() {
        return Ok((sql.to_owned(), Vec::new()));
    }

    // Reconstruct the token stream, replacing each occurrence's
    // `AS ( ident, ident, ... )` span with `AS __td_multi_alias_<N>`.
    let mut alias_lists: Vec<Vec<String>> = Vec::with_capacity(occurrences.len());
    let mut result = String::new();
    let mut pos = 0usize;
    for occ in &occurrences {
        let idx = alias_lists.len();
        alias_lists.push(occ.aliases.clone());

        // Emit tokens before the AS keyword.
        for t in &tokens[pos..occ.as_idx] {
            result.push_str(&t.to_string());
        }
        // Emit the sentinel: `AS __td_multi_alias_<N>`.
        result.push_str(&format!("AS {SENTINEL_PREFIX}{idx}"));
        pos = occ.rparen_idx + 1;
    }
    // Emit remaining tokens after the last occurrence.
    for t in &tokens[pos..] {
        result.push_str(&t.to_string());
    }

    Ok((result, alias_lists))
}

/// A matched `AS ( ident, ident+ )` occurrence in the token stream.
struct Occurrence {
    /// Index of the `AS` keyword token.
    as_idx: usize,
    /// Index of the closing `)` token.
    rparen_idx: usize,
    /// The extracted alias identifiers (N >= 2).
    aliases: Vec<String>,
}

/// Starting at `tokens[as_idx]` (which must be the `AS` keyword), try to match
/// the pattern `AS ( ident, ident+ )` (N >= 2 identifiers, all at the current
/// paren depth). Returns `Some(Occurrence)` on success.
fn try_match_multi_alias_at(tokens: &[Token], as_idx: usize) -> Option<Occurrence> {
    // Skip whitespace after AS to find `(`.
    let lparen_idx = next_non_ws(tokens, as_idx + 1)?;
    if !matches!(tokens[lparen_idx], Token::LParen) {
        return None;
    }

    // Collect interior: must be `ident (, ident)+` (N >= 2 Word tokens).
    let mut aliases: Vec<String> = Vec::new();
    let mut expect_ident = true;
    let mut j = lparen_idx + 1;
    loop {
        // Skip whitespace inside the parens.
        while j < tokens.len() && matches!(tokens[j], Token::Whitespace(_)) {
            j += 1;
        }
        if j >= tokens.len() {
            return None;
        }
        if matches!(tokens[j], Token::RParen) {
            break;
        }
        if expect_ident {
            match &tokens[j] {
                Token::Word(w) => aliases.push(w.value.clone()),
                _ => return None,
            }
            expect_ident = false;
        } else {
            if !matches!(tokens[j], Token::Comma) {
                return None;
            }
            expect_ident = true;
        }
        j += 1;
    }

    // Must have at least 2 aliases and not end on a trailing comma.
    if expect_ident || aliases.len() < 2 {
        return None;
    }

    Some(Occurrence {
        as_idx,
        rparen_idx: j,
        aliases,
    })
}

/// Return the index of the next non-whitespace token after `start`, or `None`.
fn next_non_ws(tokens: &[Token], start: usize) -> Option<usize> {
    tokens[start..]
        .iter()
        .position(|t| !matches!(t, Token::Whitespace(_)))
        .map(|offset| start + offset)
}

// ── Post-lowering splice ────────────────────────────────────────────────────

/// Walk the `CommonAst` tree recursively and replace sentinel-aliased
/// projections (from [`rewrite_multi_aliases`]) with the appropriate
/// generator-specific expansions.
///
/// Dispatch:
/// - `stack(...)` + K >= 2 aliases -> `stack_multi_alias(inner, "a1", ..., "aK")`
/// - `explode`/`explode_outer(arg)` + exactly 2 aliases -> two projections:
///   `Alias(FunctionCall("map_explode_key", [arg]), a1)` and
///   `Alias(FunctionCall("map_explode_val", [arg]), a2)`
/// - Anything else -> boundary error.
///
/// After the walk, any unconsumed sentinel triggers a boundary error (the
/// sentinel must never reach the analyzer).
pub(super) fn splice_multi_aliases(
    ast: &mut CommonAst,
    alias_lists: &[Vec<String>],
) -> Result<(), EmissionError> {
    let mut consumed = vec![false; alias_lists.len()];
    splice_walk(ast, alias_lists, &mut consumed)?;

    // Sentinel-leak guard: every sentinel must have been consumed.
    for (idx, was_consumed) in consumed.iter().enumerate() {
        if !*was_consumed {
            bail_boundary_proto!(
                "sql::multi_alias::unconsumed_sentinel",
                format!(
                    "multi-alias sentinel __td_multi_alias_{idx} was not found in any \
                     Project node — it must not reach the analyzer"
                ),
            );
        }
    }
    Ok(())
}

/// Recursive walk over the AST, processing `Project` nodes and descending
/// into children.
fn splice_walk(
    ast: &mut CommonAst,
    alias_lists: &[Vec<String>],
    consumed: &mut [bool],
) -> Result<(), EmissionError> {
    // Process this node if it's a Project.
    if let CommonOp::Project {
        ref mut projections,
        ..
    } = ast.op
    {
        splice_projections(projections, alias_lists, consumed)?;
    }

    // Descend into children.
    for child in ast.op.children_mut() {
        splice_walk(child, alias_lists, consumed)?;
    }
    Ok(())
}

/// Process a single Project's projection list, replacing sentinel-aliased
/// items with their expanded forms. The replacement may be 1:1 (stack) or
/// 1:N (explode on MAP -> 2 projections), so we rebuild the vec.
fn splice_projections(
    projections: &mut Vec<Expression>,
    alias_lists: &[Vec<String>],
    consumed: &mut [bool],
) -> Result<(), EmissionError> {
    // Take ownership and rebuild, splicing replacements in place.
    let old = std::mem::take(projections);
    let mut out: Vec<Expression> = Vec::with_capacity(old.len());
    for proj in old {
        if let Some((sentinel_idx, aliases)) = extract_sentinel(&proj, alias_lists) {
            consumed[sentinel_idx] = true;
            // Extract the inner expression from the Alias wrapper.
            let inner = match proj {
                Expression::Alias(a) => *a.expr,
                _ => unreachable!("extract_sentinel matched an Alias"),
            };
            let replacements = dispatch_multi_alias(inner, aliases)?;
            out.extend(replacements);
        } else {
            out.push(proj);
        }
    }
    *projections = out;
    Ok(())
}

/// Check if an expression is `Alias(inner, "__td_multi_alias_<N>")` and return
/// `(N, &aliases)` if so.
fn extract_sentinel<'a>(
    expr: &Expression,
    alias_lists: &'a [Vec<String>],
) -> Option<(usize, &'a Vec<String>)> {
    let Expression::Alias(ref a) = expr else {
        return None;
    };
    let idx = parse_sentinel_index(&a.alias)?;
    if idx < alias_lists.len() {
        Some((idx, &alias_lists[idx]))
    } else {
        None
    }
}

/// Parse `__td_multi_alias_<N>` and return N, or None.
fn parse_sentinel_index(alias: &str) -> Option<usize> {
    alias.strip_prefix(SENTINEL_PREFIX)?.parse::<usize>().ok()
}

/// Dispatch the multi-alias expansion based on the inner expression and alias
/// list. Returns the replacement expression(s).
fn dispatch_multi_alias(
    inner: Expression,
    aliases: &[String],
) -> Result<Vec<Expression>, EmissionError> {
    match &inner {
        Expression::FunctionCall(fc) => {
            let name_lower = fc.name.to_ascii_lowercase();
            match name_lower.as_str() {
                "stack" => {
                    if aliases.len() < 2 {
                        bail_boundary_proto!(
                            "sql::multi_alias::stack_arity",
                            format!(
                                "stack multi-alias requires at least 2 aliases, got {}",
                                aliases.len()
                            ),
                        );
                    }
                    Ok(vec![build_stack_multi_alias(inner, aliases)])
                }
                "explode" | "explode_outer" => {
                    if aliases.len() != 2 {
                        bail_boundary_proto!(
                            "sql::multi_alias::explode_arity",
                            format!(
                                "explode/explode_outer multi-alias requires exactly 2 aliases \
                                 (key, value), got {}",
                                aliases.len()
                            ),
                        );
                    }
                    if fc.args.len() != 1 {
                        bail_boundary_proto!(
                            "sql::multi_alias::explode_arg_count",
                            format!(
                                "explode/explode_outer requires exactly 1 argument, got {}",
                                fc.args.len()
                            ),
                        );
                    }
                    let arg = fc.args[0].clone();
                    Ok(build_map_explode_pair(arg, &aliases[0], &aliases[1]))
                }
                other => {
                    bail_boundary_proto!(
                        format!("sql::multi_alias::unsupported_generator::{other}"),
                        format!(
                            "multi-column alias `AS ({})` on `{other}` is not implemented \
                             in τ's SparkSQL path",
                            aliases.join(", ")
                        ),
                    );
                }
            }
        }
        _ => {
            bail_boundary_proto!(
                "sql::multi_alias::non_function",
                format!(
                    "multi-column alias `AS ({})` on a non-function expression is not \
                     supported",
                    aliases.join(", ")
                ),
            );
        }
    }
}

// ── Shared builders ─────────────────────────────────────────────────────────

/// Build a `FunctionCall("stack_multi_alias", [inner, Literal(a1), ..., Literal(aK)])`
/// expression — the shape the analyzer's `expand_stack_projections` expects.
///
/// Shared by both the `F.expr()` fragment path ([`super::wrap_stack_multi_alias`])
/// and the full-SQL path ([`splice_multi_aliases`]).
pub(super) fn build_stack_multi_alias(inner: Expression, aliases: &[String]) -> Expression {
    let mut args: Vec<Expression> = Vec::with_capacity(1 + aliases.len());
    args.push(inner);
    for a in aliases {
        args.push(Expression::Literal(Literal {
            value: LiteralValue::String(a.clone()),
            data_type: DataType::String,
        }));
    }
    Expression::FunctionCall(FunctionCall {
        name: "stack_multi_alias".to_owned(),
        args,
        distinct: false,
    })
}

/// Build the two-projection explode-on-MAP expansion:
/// `[Alias(FunctionCall("map_explode_key", [arg]), a1),
///   Alias(FunctionCall("map_explode_val", [arg]), a2)]`.
///
/// This is the EXACT shape that `try_convert_posexplode_multi_alias` in
/// `v2_relation_converter.rs` produces for the DataFrame path — convergence
/// is tested.
pub(super) fn build_map_explode_pair(
    arg: Expression,
    key_alias: &str,
    val_alias: &str,
) -> Vec<Expression> {
    let key_fn = Expression::FunctionCall(FunctionCall {
        name: "map_explode_key".to_owned(),
        args: vec![arg.clone()],
        distinct: false,
    });
    let val_fn = Expression::FunctionCall(FunctionCall {
        name: "map_explode_val".to_owned(),
        args: vec![arg],
        distinct: false,
    });
    vec![
        Expression::Alias(AliasExpression {
            expr: Box::new(key_fn),
            alias: key_alias.to_owned(),
        }),
        Expression::Alias(AliasExpression {
            expr: Box::new(val_fn),
            alias: val_alias.to_owned(),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_trailing_multi_alias (existing, F.expr path) ──────────────────

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

    // ── rewrite_multi_aliases (full-SQL path) ───────────────────────────────

    #[test]
    fn rewrite_mid_list_occurrence_cx011() {
        // cx-011 shape: `SELECT id, explode(attrs) AS (k, v) FROM emp`
        let (rewritten, alias_lists) =
            rewrite_multi_aliases("SELECT id, explode(attrs) AS (k, v) FROM emp")
                .expect("must tokenize");
        assert_eq!(alias_lists.len(), 1);
        assert_eq!(alias_lists[0], vec!["k", "v"]);
        assert!(
            rewritten.contains("__td_multi_alias_0"),
            "sentinel must appear in rewritten SQL: {rewritten}"
        );
        assert!(
            !rewritten.contains("(k, v)"),
            "original alias parens must be removed: {rewritten}"
        );
        // Must still contain the FROM clause.
        assert!(
            rewritten.contains("FROM"),
            "FROM clause must be preserved: {rewritten}"
        );
    }

    #[test]
    fn rewrite_trailing_occurrence_pv006() {
        // pv-006 shape — trailing position.
        let sql = "SELECT id, stack(2, 'age', age, 'salary', salary) AS (metric, value) FROM emp";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert_eq!(alias_lists.len(), 1);
        assert_eq!(alias_lists[0], vec!["metric", "value"]);
        assert!(rewritten.contains("__td_multi_alias_0"));
    }

    #[test]
    fn rewrite_two_occurrences_in_one_statement() {
        let sql = "SELECT explode(m1) AS (k1, v1), explode(m2) AS (k2, v2) FROM t";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert_eq!(alias_lists.len(), 2);
        assert_eq!(alias_lists[0], vec!["k1", "v1"]);
        assert_eq!(alias_lists[1], vec!["k2", "v2"]);
        assert!(rewritten.contains("__td_multi_alias_0"));
        assert!(rewritten.contains("__td_multi_alias_1"));
    }

    // ── Non-match fixtures (must return unchanged / empty alias lists) ──────

    #[test]
    fn rewrite_ignores_cte_body() {
        // `WITH a AS (SELECT ...)` — interior starts with SELECT, not ident-comma.
        let sql = "WITH a AS (SELECT 1) SELECT * FROM a";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert!(alias_lists.is_empty(), "CTE body must not match");
        assert_eq!(rewritten, sql);
    }

    #[test]
    fn rewrite_ignores_cte_column_list() {
        // `WITH t(k,v) AS (...)` — parens follow `t`, not `AS`.
        let sql = "WITH t(k,v) AS (SELECT 1, 2) SELECT * FROM t";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert!(alias_lists.is_empty(), "CTE column list must not match");
        assert_eq!(rewritten, sql);
    }

    #[test]
    fn rewrite_ignores_window_spec() {
        // `WINDOW w AS (PARTITION BY ...)` — interior has keywords, not pure ident-comma.
        let sql = "SELECT count(*) OVER w FROM t WINDOW w AS (PARTITION BY id)";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert!(alias_lists.is_empty(), "WINDOW spec must not match");
        assert_eq!(rewritten, sql);
    }

    #[test]
    fn rewrite_ignores_cast_as() {
        // `CAST(x AS DOUBLE)` — AS at depth > 0.
        let sql = "SELECT CAST(x AS DOUBLE) FROM t";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert!(alias_lists.is_empty(), "CAST AS must not match");
        assert_eq!(rewritten, sql);
    }

    #[test]
    fn rewrite_ignores_derived_table_alias() {
        // `FROM (...) AS t(a,b)` — AS is followed by a Word, not LParen.
        let sql = "SELECT * FROM (SELECT 1, 2) AS t(a, b)";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert!(alias_lists.is_empty(), "derived table alias must not match");
        assert_eq!(rewritten, sql);
    }

    #[test]
    fn rewrite_ignores_single_alias_in_parens() {
        // `AS (k)` — single ident, N < 2, must not match.
        let sql = "SELECT explode(m) AS (k) FROM t";
        let (rewritten, alias_lists) = rewrite_multi_aliases(sql).expect("must tokenize");
        assert!(
            alias_lists.is_empty(),
            "single-ident paren alias must not match"
        );
        assert_eq!(rewritten, sql);
    }

    // ── build_stack_multi_alias ─────────────────────────────────────────────

    #[test]
    fn build_stack_multi_alias_shape() {
        let inner = Expression::FunctionCall(FunctionCall {
            name: "stack".to_owned(),
            args: vec![],
            distinct: false,
        });
        let result =
            build_stack_multi_alias(inner.clone(), &["metric".to_owned(), "value".to_owned()]);
        match result {
            Expression::FunctionCall(fc) => {
                assert_eq!(fc.name, "stack_multi_alias");
                assert_eq!(fc.args.len(), 3);
                assert!(matches!(&fc.args[0], Expression::FunctionCall(f) if f.name == "stack"));
                match &fc.args[1] {
                    Expression::Literal(Literal {
                        value: LiteralValue::String(s),
                        ..
                    }) => assert_eq!(s, "metric"),
                    other => panic!("expected string literal, got {other:?}"),
                }
                match &fc.args[2] {
                    Expression::Literal(Literal {
                        value: LiteralValue::String(s),
                        ..
                    }) => assert_eq!(s, "value"),
                    other => panic!("expected string literal, got {other:?}"),
                }
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    // ── build_map_explode_pair ───────────────────────────────────────────────

    #[test]
    fn build_map_explode_pair_shape() {
        let arg = Expression::FunctionCall(FunctionCall {
            name: "attrs".to_owned(),
            args: vec![],
            distinct: false,
        });
        let pair = build_map_explode_pair(arg, "k", "v");
        assert_eq!(pair.len(), 2);

        // First: Alias(map_explode_key(attrs), "k")
        match &pair[0] {
            Expression::Alias(a) => {
                assert_eq!(a.alias, "k");
                match a.expr.as_ref() {
                    Expression::FunctionCall(fc) => {
                        assert_eq!(fc.name, "map_explode_key");
                    }
                    other => panic!("expected FunctionCall, got {other:?}"),
                }
            }
            other => panic!("expected Alias, got {other:?}"),
        }

        // Second: Alias(map_explode_val(attrs), "v")
        match &pair[1] {
            Expression::Alias(a) => {
                assert_eq!(a.alias, "v");
                match a.expr.as_ref() {
                    Expression::FunctionCall(fc) => {
                        assert_eq!(fc.name, "map_explode_val");
                    }
                    other => panic!("expected FunctionCall, got {other:?}"),
                }
            }
            other => panic!("expected Alias, got {other:?}"),
        }
    }
}
