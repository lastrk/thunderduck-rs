//! τ's SparkSQL front-end — parses raw Spark SQL into [`CommonAst`].
//!
//! Owns the SQL text path in the τ substrate (Open Decision 1 Option 1b):
//! `V2RelationConverter` refuses `RelType::Sql` with
//! [`EmissionError::Unsupported`] (`kind: ProtoShape`); dispatch routes
//! `Sql` here instead.
//!
//! **INV10:** this file imports ONLY value-level types from `crate::types`
//! plus intra-τ modules. No `crate::parser`, `crate::logical`,
//! `crate::expression`, `crate::generator`.

mod dialect;
mod multi_alias;
mod v2_lowering;

use crate::bail_boundary_proto;
use crate::transpiler_v2::ast::CommonAst;
use crate::transpiler_v2::error::UnsupportedKind;
use crate::transpiler_v2::EmissionError;
use dialect::SparkDialect;
use sqlparser::parser::ParserError;
use sqlparser::tokenizer::{Token, Tokenizer};

/// ADR-022 classification for a `sqlparser` failure.
///
/// **The default is category 2 (Thunderduck-boundary), unchanged.** A parse
/// failure usually means `sqlparser`'s grammar lacks a Spark construct, and
/// telling a user their valid SQL is invalid is strictly worse than admitting
/// τ cannot ingest it. The classifier only ever *upgrades*, never relaxes.
///
/// It upgrades to category 1 (Spark-emulated `PARSE_SYNTAX_ERROR`) only on
/// evidence that holds **regardless of grammar coverage**:
///
/// 1. **Lexical failure** — the input is not valid SQL *text*. No grammar gap
///    can make a string literal terminate.
/// 2. **Unbalanced delimiters** — counted over the token stream, so it does not
///    depend on `sqlparser` understanding what is inside them.
/// 3. **An empty slot where an expression is required** — `found: EOF` or
///    `found: )`. There is no syntax there at all, so it cannot be *unsupported*
///    syntax.
///
/// Deliberately **not** upgraded: `Expected: end of statement, found: X`. A
/// live-Spark survey (`tasks/tau-error-class-audit-2026-08.md`) found that shape
/// produced by both malformed input (`SELECT * FRM emp`, `SELECT * FROM emp FROM
/// dept`) and genuine τ grammar gaps (HiveQL `TRANSFORM`, `VERSION AS OF`,
/// `CREATE TABLE … USING parquet`). It carries no information, and treating it
/// as malformed would slander valid Spark. Cases stranded by this are tracked as
/// deferred witnesses (`parseerr-001`) and in
/// `docs/future_work/adr-022-strict-rejection-enforcement.md`.
fn classify_parse_failure(parsed_sql: &str, e: &ParserError) -> EmissionError {
    if is_definitely_malformed(parsed_sql, e) {
        // Spark raises PARSE_SYNTAX_ERROR for these, so this is ADR-022
        // category 1 — an ordinary Spark-emulated error, not a new category.
        EmissionError::SparkEmulated {
            class: Some("PARSE_SYNTAX_ERROR"),
            message: format!("Syntax error in SQL: {e}"),
        }
    } else {
        EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: "sql::parse_error".to_owned(),
            reason: e.to_string(),
        }
    }
}

/// The three grammar-independent malformedness signals described on
/// [`classify_parse_failure`]. Conservative by construction: any doubt returns
/// `false`, leaving the boundary error in place.
fn is_definitely_malformed(parsed_sql: &str, e: &ParserError) -> bool {
    // (1) Lexical — the tokenizer itself gave up.
    if matches!(e, ParserError::TokenizerError(_)) {
        return true;
    }

    // (2) Delimiter balance, over tokens rather than raw text so parens inside
    // string literals, comments and quoted identifiers do not count. A
    // tokenizer failure here is itself signal (1).
    match Tokenizer::new(&SparkDialect, parsed_sql).tokenize() {
        Err(_) => return true,
        Ok(tokens) => {
            let mut depth: i64 = 0;
            for t in &tokens {
                match t {
                    Token::LParen => depth += 1,
                    Token::RParen => depth -= 1,
                    _ => continue,
                }
                if depth < 0 {
                    return true; // a close with no matching open
                }
            }
            if depth != 0 {
                return true; // an open never closed
            }
        }
    }

    // (3) An expression was required and the slot was EMPTY — end of input, or
    // an immediate close-paren. Restricted to those two on purpose: for any
    // other token, "expression expected" could equally mean τ's parser does not
    // recognise a construct that IS a valid Spark expression.
    if let ParserError::ParserError(msg) = e {
        const EMPTY_EXPRESSION_SLOT: &[&str] = &[
            "Expected: an expression, found: EOF",
            "Expected: an expression, found: )",
        ];
        if EMPTY_EXPRESSION_SLOT.iter().any(|p| msg.starts_with(p)) {
            return true;
        }
    }

    false
}

/// τ's public SparkSQL parser entry point.
pub struct SparkSqlParserV2;

impl SparkSqlParserV2 {
    /// Parse a raw Spark SQL string into a [`CommonAst`].
    ///
    /// τ scope: `SELECT` queries with `FROM`, `WHERE`, `GROUP BY`,
    /// `ORDER BY`, `LIMIT/OFFSET`, joins, and subqueries in `FROM`.
    /// Everything else surfaces as
    /// [`EmissionError::Unsupported`] with `kind: ProtoShape`.
    ///
    /// Before handing SQL to `sqlparser-rs`, a token-level pre-pass rewrites
    /// any depth-0 `AS (ident, ident+)` multi-column aliases (which sqlparser
    /// cannot parse) into sentinel single-identifier aliases. After lowering,
    /// a post-pass splices the sentinel-aliased projections into their
    /// generator-specific expansions (e.g. `explode(m) AS (k, v)` becomes
    /// `map_explode_key(m) AS k, map_explode_val(m) AS v`).
    pub fn parse(sql: &str) -> Result<CommonAst, EmissionError> {
        use sqlparser::parser::Parser;

        // Step 1: token-level rewrite of multi-column aliases.
        let (rewritten_sql, alias_lists) = multi_alias::rewrite_multi_aliases(sql)?;
        let parse_input = if alias_lists.is_empty() {
            sql
        } else {
            rewritten_sql.as_str()
        };

        let dialect = SparkDialect;
        // A sqlparser failure is a boundary error by DEFAULT — the input never
        // reached `CommonAst`, so `ProtoShape` (input τ can't ingest), not `Op`
        // (emission arm not implemented). `classify_parse_failure` upgrades the
        // provably-malformed subset to a Spark-emulated PARSE_SYNTAX_ERROR.
        let mut stmts = Parser::parse_sql(&dialect, parse_input)
            .map_err(|e| classify_parse_failure(parse_input, &e))?;
        if stmts.len() != 1 {
            bail_boundary_proto!(
                "sql::multi_statement",
                format!("expected exactly one SQL statement, got {}", stmts.len()),
            );
        }
        let mut ast = v2_lowering::lower_statement(stmts.remove(0))?;

        // Step 2: post-lowering splice of sentinel-aliased projections.
        if !alias_lists.is_empty() {
            multi_alias::splice_multi_aliases(&mut ast, &alias_lists)?;
        }

        Ok(ast)
    }

    /// Parse a raw Spark SQL string into a [`SqlStatement`].
    ///
    /// Unlike [`parse`] (which only accepts `SELECT` queries), this entry
    /// point also recognises DDL statements (`CREATE TEMP VIEW …`).
    /// Non-temporary `CREATE VIEW`, `CREATE TABLE`, and every other
    /// statement kind surface as Thunderduck-boundary errors.
    ///
    /// Used by the `SqlCommand` dispatch path in `connect-server::service`
    /// so that `spark.sql("CREATE TEMP VIEW v AS SELECT …")` can be
    /// eagerly executed.
    pub fn parse_statement(sql: &str) -> Result<crate::transpiler_v2::SqlStatement, EmissionError> {
        use crate::transpiler_v2::SqlStatement;
        use sqlparser::parser::Parser;

        // Step 1: token-level rewrite of multi-column aliases (shared
        // with the SELECT-only path).
        let (rewritten_sql, alias_lists) = multi_alias::rewrite_multi_aliases(sql)?;
        let parse_input = if alias_lists.is_empty() {
            sql
        } else {
            rewritten_sql.as_str()
        };

        let dialect = SparkDialect;
        let mut stmts = Parser::parse_sql(&dialect, parse_input)
            .map_err(|e| classify_parse_failure(parse_input, &e))?;
        if stmts.len() != 1 {
            bail_boundary_proto!(
                "sql::multi_statement",
                format!("expected exactly one SQL statement, got {}", stmts.len()),
            );
        }
        let mut result = v2_lowering::lower_statement_or_ddl(stmts.remove(0))?;

        // Step 2: post-lowering splice of sentinel-aliased projections
        // (only applicable to Query variants).
        if !alias_lists.is_empty() {
            if let SqlStatement::Query(ref mut ast) = result {
                multi_alias::splice_multi_aliases(ast, &alias_lists)?;
            }
        }

        Ok(result)
    }

    /// Parse a SparkSQL expression FRAGMENT (e.g. `age + 1`, `upper(name)`)
    /// into a single [`crate::transpiler_v2::Expression`]. Used by the
    /// protobuf front-end for `Expression::ExpressionString` — Spark's
    /// `F.expr("...")` / `df.selectExpr("...")`. Implemented by wrapping the
    /// fragment as `SELECT (<expr>) AS __td_expr` and extracting the single
    /// projection.
    pub fn parse_expression(
        expr_sql: &str,
    ) -> Result<crate::transpiler_v2::Expression, EmissionError> {
        // Spark's `F.expr("<x>")` / `selectExpr("<x>")` allows the fragment
        // to carry its own alias (e.g. `age + 1 as age1`), so we must NOT
        // wrap with our own `AS`. Simply parse `SELECT <fragment>` and take
        // the single projection — the alias, if any, is preserved by the
        // parser as an `Expression::Alias`.
        //
        // Spark's generator-function multi-column alias
        // (`stack(...) AS (metric, value)`) cannot be represented in
        // sqlparser-rs's `SelectItem::ExprWithAlias { alias: Ident }`
        // (single-ident slot) and the parser hard-fails at the `(` after
        // `AS`. Pre-scan the token stream to strip a trailing
        // `AS ( ident, ident+ )` alias list and remember the names.
        let (stripped_sql, multi_aliases) = multi_alias::strip_trailing_multi_alias(expr_sql)?;
        let wrapped = format!("SELECT {stripped_sql}");
        let plan = Self::parse(&wrapped)?;
        use crate::transpiler_v2::ast::CommonOp;
        // The fragment path must return the raw expression: strip any
        // τ-synthesized SparkSQL default-name alias (e.g. `count(*)` →
        // `count(1)`) so `F.expr(...)`/`selectExpr(...)` see the bare shape;
        // the DataFrame layer owns naming there.
        //
        // If the caller supplied a multi-column alias list, wrap the parsed
        // expression in a synthetic `stack_multi_alias(<stack call>, "<a1>",
        // …, "<aK>")` FunctionCall so the analyzer's Project pre-pass
        // (`expand_stack_projections`) can fan it out into K per-column
        // projections. piv-006 is the only witness — non-`stack` inner
        // functions surface as a Thunderduck-boundary error inside the
        // analyzer pre-pass.
        let single = match plan.op {
            CommonOp::Project {
                mut projections, ..
            } if projections.len() == 1 => Ok(v2_lowering::strip_synthetic_default_name(
                projections.remove(0),
            )),
            // Pass 71: when the fragment is a bare aggregate call
            // (e.g. `try_sum(lng)`, `every(active)`), `lower_select`
            // routes it through `lower_aggregate_select` and produces
            // `Aggregate { input: SingleRow, grouping: [], aggregates:
            // [<expr>], .. }`. Extract the single aggregate expression
            // — from the caller's perspective this is a scalar
            // aggregate expression, semantically identical to what
            // Spark hands us via `F.sum(...)`.
            CommonOp::Aggregate {
                input,
                grouping,
                mut aggregates,
                ..
            } if grouping.is_empty()
                && aggregates.len() == 1
                && matches!(input.op, CommonOp::SingleRow) =>
            {
                Ok(v2_lowering::strip_synthetic_default_name(
                    aggregates.remove(0),
                ))
            }
            _ => bail_boundary_proto!(
                "ExpressionString::not_a_scalar",
                format!("expression fragment did not parse as a single scalar: {expr_sql}"),
            ),
        }?;
        match multi_aliases {
            None => Ok(single),
            Some(aliases) => wrap_stack_multi_alias(single, aliases, expr_sql),
        }
    }
}

/// Wrap a parsed generator-call `Expression` in a synthetic
/// `stack_multi_alias(<inner>, "<a1>", ..., "<aK>")` FunctionCall so
/// [`crate::transpiler_v2::analyzer`]'s Project pre-pass can splice the
/// K-alias list into K per-column projections.
///
/// Only accepts an inner `stack` call (piv-006 scope) — other generator
/// functions (`posexplode`, `explode(map)`, `inline`, `json_tuple`) with a
/// multi-alias on the `F.expr()` SQL fragment path surface as a
/// Thunderduck-boundary error and remain a follow-up.
///
/// Delegates to [`multi_alias::build_stack_multi_alias`] for the actual
/// construction (single source of truth shared with the full-SQL path).
fn wrap_stack_multi_alias(
    inner: crate::transpiler_v2::Expression,
    aliases: Vec<String>,
    expr_sql: &str,
) -> Result<crate::transpiler_v2::Expression, EmissionError> {
    use crate::transpiler_v2::expression::Expression;

    // N5: `inner` came through `parse_expression` → `lower_function`, so the
    // name is canonical lowercase — exact compare.
    let is_stack = matches!(
        &inner,
        Expression::FunctionCall(fc) if fc.name == "stack"
    );
    if !is_stack {
        bail_boundary_proto!(
            "ExpressionString::multi_alias_non_stack",
            format!(
                "multi-column alias `AS ( ... )` on a non-`stack` generator is not \
                 implemented in τ's SparkSQL path: {expr_sql}"
            ),
        );
    }
    Ok(multi_alias::build_stack_multi_alias(inner, &aliases))
}

#[cfg(test)]
mod tests {
    //! Unit tests for `SparkSqlParserV2::parse_expression` — the
    //! entry point used by `F.expr("...")` / `df.selectExpr("...")`
    //! via `ExpressionString`.

    use super::SparkSqlParserV2;
    use crate::transpiler_v2::{generate, BaseTypes, Expression};

    /// ADR-022 classification of `sqlparser` failures — the
    /// `classify_parse_failure` contract, pinned end-to-end through
    /// `parse_statement`.
    ///
    /// These tests double as the canary for a `sqlparser` bump: signal (3)
    /// matches on the parser's error *wording*, so if that wording changes the
    /// upgrade silently stops firing. A failure here means re-derive the
    /// prefixes, not delete the test.
    mod parse_failure_classification {
        use super::SparkSqlParserV2;
        use crate::transpiler_v2::EmissionError;

        #[track_caller]
        fn assert_spark_emulated_syntax_error(sql: &str) {
            match SparkSqlParserV2::parse_statement(sql) {
                Err(EmissionError::SparkEmulated { class, .. }) => {
                    assert_eq!(
                        class,
                        Some("PARSE_SYNTAX_ERROR"),
                        "malformed SQL must carry Spark's own class: {sql}"
                    );
                }
                other => {
                    panic!("expected SparkEmulated PARSE_SYNTAX_ERROR for `{sql}`, got {other:?}")
                }
            }
        }

        #[track_caller]
        fn assert_boundary_error(sql: &str) {
            match SparkSqlParserV2::parse_statement(sql) {
                Err(EmissionError::Unsupported { name, .. }) => {
                    assert_eq!(name, "sql::parse_error", "for `{sql}`");
                }
                other => panic!("expected a boundary error for `{sql}`, got {other:?}"),
            }
        }

        // ── Signal 1: lexical failure ───────────────────────────────────────
        #[test]
        fn unterminated_string_is_spark_emulated() {
            // A tokenizer error. No grammar gap can make a literal terminate.
            assert_spark_emulated_syntax_error("SELECT 'abc FROM emp");
        }

        // ── Signal 2: unbalanced delimiters ─────────────────────────────────
        #[test]
        fn unclosed_paren_is_spark_emulated() {
            assert_spark_emulated_syntax_error("SELECT (1 FROM emp");
        }

        #[test]
        fn unopened_paren_is_spark_emulated() {
            assert_spark_emulated_syntax_error("SELECT id FROM emp )))");
        }

        #[test]
        fn parens_inside_string_literals_do_not_count_as_unbalanced() {
            // Balance is counted over TOKENS, so a lone paren inside a literal
            // must not trip signal 2. This query is valid, so it must parse.
            SparkSqlParserV2::parse_statement("SELECT '(' AS p FROM emp")
                .expect("a paren inside a string literal is not a delimiter");
        }

        // ── Signal 3: an EMPTY slot where an expression is required ─────────
        #[test]
        fn expression_required_but_input_ended_is_spark_emulated() {
            assert_spark_emulated_syntax_error("SELECT * FROM emp GROUP BY");
        }

        #[test]
        fn expression_required_but_slot_empty_is_spark_emulated() {
            assert_spark_emulated_syntax_error("SELECT * FROM emp UNPIVOT (v FOR k IN ())");
        }

        // ── The NEGATIVE half: these must stay Thunderduck-boundary ─────────
        // `Expected: end of statement, found: X` is produced by BOTH malformed
        // input and genuine τ grammar gaps, so it must never be upgraded.
        // Getting this wrong tells users their valid Spark is invalid, which is
        // strictly worse than admitting τ cannot ingest it.

        #[test]
        fn hiveql_transform_stays_boundary() {
            // Spark ACCEPTS this; sqlparser does not. A τ gap, not bad SQL.
            assert_boundary_error("SELECT TRANSFORM(id, name) USING 'cat' AS (x, y) FROM emp");
        }

        #[test]
        fn create_table_using_stays_boundary() {
            // Spark ACCEPTS this too.
            assert_boundary_error("CREATE TABLE t2 (a INT) USING parquet");
        }

        #[test]
        fn time_travel_stays_boundary() {
            // Spark parses it and fails in ANALYSIS with
            // UNSUPPORTED_FEATURE.TIME_TRAVEL — never PARSE_SYNTAX_ERROR. This
            // is the guardrail witnessed by corpus case parseerr-101.
            assert_boundary_error("SELECT * FROM emp VERSION AS OF 1");
        }

        #[test]
        fn keyword_typo_stays_boundary_because_it_is_indistinguishable() {
            // `SELECT * FRM emp` IS malformed, but its error shape
            // (`Expected: end of statement, found: FRM`) is byte-identical in
            // form to the three τ gaps above. Upgrading it would upgrade them
            // too. Deliberately left as a boundary error and tracked by the
            // deferred witness parseerr-001 — see
            // docs/future_work/adr-022-strict-rejection-enforcement.md.
            assert_boundary_error("SELECT * FRM emp");
        }
    }

    /// Assert the parsed fragment resolves to a `FunctionCall` for `name`.
    fn assert_parses_as_function(expr_sql: &str, name: &str) {
        let parsed = SparkSqlParserV2::parse_expression(expr_sql)
            .unwrap_or_else(|e| panic!("parse_expression({expr_sql:?}) failed: {e:?}"));
        match parsed {
            Expression::FunctionCall(ref fc) => {
                assert!(
                    fc.name.eq_ignore_ascii_case(name),
                    "expected function {name:?}, got {:?} for {expr_sql:?}",
                    fc.name,
                );
            }
            other => panic!("expected FunctionCall({name}), got {other:?} for {expr_sql:?}"),
        }
    }

    #[test]
    fn parse_expression_scalar_arithmetic() {
        // Baseline: existing behavior — a non-aggregate scalar expression
        // continues to lower via `CommonOp::Project`.
        let parsed =
            SparkSqlParserV2::parse_expression("age + 1").expect("scalar arithmetic must parse");
        assert!(matches!(parsed, Expression::Binary(_)));
    }

    #[test]
    fn parse_expression_scalar_function_upper() {
        assert_parses_as_function("upper(name)", "upper");
    }

    #[test]
    fn direct_sql_uses_emission_not_session_macros() {
        // Literal-only witnesses cover direct SQL parser → analyzer → emission.
        for (sql_text, expected) in [
            ("SELECT endswith('abc', 'c')", "ends_with"),
            (
                "SELECT arrays_zip(array(1, 2), array(3, 4))",
                "list_transform",
            ),
            ("SELECT conv('10', 10, 2)", "bin("),
        ] {
            let plan = SparkSqlParserV2::parse(sql_text).expect("parse");
            let emitted = generate(&plan, &BaseTypes::empty()).expect("analyze and emit");
            assert!(
                emitted.contains(expected),
                "expected `{expected}` in emitted SQL for {sql_text:?}, got: {emitted}"
            );
        }
    }

    // ── Pass 71: bare aggregate fragments ────────────────────────────────

    #[test]
    fn parse_expression_bare_try_sum() {
        // `F.expr("try_sum(lng)")` — Spark 4.x overflow-safe SUM.
        assert_parses_as_function("try_sum(lng)", "try_sum");
    }

    #[test]
    fn parse_expression_bare_try_avg() {
        // `F.expr("try_avg(a)")` — Spark 4.x overflow-safe AVG.
        assert_parses_as_function("try_avg(a)", "try_avg");
    }

    #[test]
    fn parse_expression_bare_every() {
        // `F.expr("every(active)")` — Spark boolean-all aggregate,
        // alias of `bool_and`.
        assert_parses_as_function("every(active)", "every");
    }

    #[test]
    fn parse_expression_bare_any_aggregate() {
        // `F.expr("any(active)")` — Spark boolean-any aggregate,
        // alias of `bool_or`. Confirms the sibling of `every` also
        // rides the same code path.
        assert_parses_as_function("any(active)", "any");
    }

    #[test]
    fn parse_expression_bare_any_value() {
        // `F.expr("any_value(name)")` — Spark's arbitrary-representative
        // aggregate. Same shape as the other bare aggregates.
        assert_parses_as_function("any_value(name)", "any_value");
    }

    #[test]
    fn parse_expression_bare_array_agg() {
        // `F.expr("array_agg(name)")` — Spark 4.x alias of `collect_list`.
        assert_parses_as_function("array_agg(name)", "array_agg");
    }

    #[test]
    fn parse_expression_bare_count_star() {
        // `F.expr("count(*)")` — the classic bare aggregate; make sure
        // the new `Aggregate` arm didn't regress the common case.
        let parsed = SparkSqlParserV2::parse_expression("count(*)").expect("count(*) must parse");
        match parsed {
            Expression::FunctionCall(ref fc) => {
                assert!(fc.name.eq_ignore_ascii_case("count"));
            }
            other => panic!("expected FunctionCall(count), got {other:?}"),
        }
    }

    #[test]
    fn parse_expression_bare_aggregate_with_alias() {
        // `F.expr("try_sum(lng) AS ts")` — an aliased bare aggregate
        // must still resolve. The alias is preserved by the SQL parser
        // as an `Expression::Alias` wrapping the aggregate call.
        let parsed = SparkSqlParserV2::parse_expression("try_sum(lng) AS ts")
            .expect("aliased bare aggregate must parse");
        match parsed {
            Expression::Alias(ref a) => {
                assert_eq!(a.alias, "ts");
                assert!(matches!(*a.expr, Expression::FunctionCall(_)));
            }
            other => panic!("expected Alias wrapping FunctionCall, got {other:?}"),
        }
    }

    // ── piv-006: `stack(...) AS (a, b, ...)` multi-column alias ────────────

    #[test]
    fn parse_expression_stack_multi_alias_lowers_to_wrapper() {
        // piv-006 witness: `F.expr("stack(2, 'age', CAST(age AS DOUBLE),
        // 'salary', salary) as (metric, value)")`. Before the fix,
        // sqlparser-rs 0.61 hard-fails at the `(` after `AS`. After the fix,
        // the multi-alias stripper strips the trailing alias list and the
        // parser wraps the parsed `stack(...)` call in a synthetic
        // `stack_multi_alias(<stack>, "metric", "value")` FunctionCall the
        // analyzer's Project pre-pass fans out into two per-column
        // projections.
        use crate::transpiler_v2::expression::{FunctionCall, Literal, LiteralValue};
        let parsed = SparkSqlParserV2::parse_expression(
            "stack(2, 'age', CAST(age AS DOUBLE), 'salary', salary) as (metric, value)",
        )
        .expect("stack multi-alias must lower without EmissionError::Unsupported");
        let FunctionCall {
            name,
            args,
            distinct,
        } = match parsed {
            Expression::FunctionCall(fc) => fc,
            other => panic!("expected FunctionCall(stack_multi_alias), got {other:?}"),
        };
        assert_eq!(name, "stack_multi_alias");
        assert!(!distinct);
        // args[0] is the inner stack call; args[1..] are the string-literal
        // alias slots.
        assert_eq!(args.len(), 3);
        match &args[0] {
            Expression::FunctionCall(fc) => {
                assert!(
                    fc.name.eq_ignore_ascii_case("stack"),
                    "inner call name must be `stack`, got {:?}",
                    fc.name
                );
            }
            other => panic!("inner arg must be a FunctionCall(stack), got {other:?}"),
        }
        let mut alias_slots: Vec<&str> = Vec::new();
        for a in &args[1..] {
            match a {
                Expression::Literal(Literal {
                    value: LiteralValue::String(s),
                    ..
                }) => alias_slots.push(s.as_str()),
                other => panic!("expected string-literal alias slot, got {other:?}"),
            }
        }
        assert_eq!(alias_slots, vec!["metric", "value"]);
    }

    // ── Full-SQL multi-alias (cx-011, pv-006) ────────────────────────────────

    #[test]
    fn parse_cx011_explode_map_multi_alias_produces_key_val_pair() {
        // cx-011: `SELECT id, explode(attrs) AS (k, v) FROM emp`
        // Must produce Project with projections:
        //   [UnresolvedColumn("id"),
        //    Alias(FunctionCall("map_explode_key", [UnresolvedColumn("attrs")]), "k"),
        //    Alias(FunctionCall("map_explode_val", [UnresolvedColumn("attrs")]), "v")]
        use crate::transpiler_v2::ast::CommonOp;
        use crate::transpiler_v2::expression::{AliasExpression, FunctionCall};

        let ast = SparkSqlParserV2::parse("SELECT id, explode(attrs) AS (k, v) FROM emp")
            .expect("cx-011 SQL must parse");

        let projections = match ast.op {
            CommonOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        assert_eq!(
            projections.len(),
            3,
            "expected 3 projections (id + key + val), got {}",
            projections.len()
        );

        // projections[0]: id column (bare UnresolvedColumn — no alias on
        // a bare column reference in a SELECT list without AS).
        match &projections[0] {
            Expression::UnresolvedColumn(uc) => {
                assert_eq!(uc.name, "id");
            }
            // The lowering may wrap bare columns in Alias(col, "id") for
            // SparkSQL default naming — accept both shapes.
            Expression::Alias(a) => {
                assert_eq!(a.alias, "id");
            }
            other => panic!("expected UnresolvedColumn or Alias for id, got {other:?}"),
        }

        // projections[1]: Alias(map_explode_key(attrs), "k")
        match &projections[1] {
            Expression::Alias(AliasExpression { expr, alias }) => {
                assert_eq!(alias, "k");
                match expr.as_ref() {
                    Expression::FunctionCall(FunctionCall { name, args, .. }) => {
                        assert_eq!(name, "map_explode_key");
                        assert_eq!(args.len(), 1);
                    }
                    other => panic!("expected FunctionCall(map_explode_key), got {other:?}"),
                }
            }
            other => panic!("expected Alias(map_explode_key, 'k'), got {other:?}"),
        }

        // projections[2]: Alias(map_explode_val(attrs), "v")
        match &projections[2] {
            Expression::Alias(AliasExpression { expr, alias }) => {
                assert_eq!(alias, "v");
                match expr.as_ref() {
                    Expression::FunctionCall(FunctionCall { name, args, .. }) => {
                        assert_eq!(name, "map_explode_val");
                        assert_eq!(args.len(), 1);
                    }
                    other => panic!("expected FunctionCall(map_explode_val), got {other:?}"),
                }
            }
            other => panic!("expected Alias(map_explode_val, 'v'), got {other:?}"),
        }
    }

    #[test]
    fn parse_pv006_stack_multi_alias_produces_wrapper() {
        // pv-006: `SELECT id, stack(2, 'age', age, 'salary', salary) AS (metric, value) FROM emp`
        // Must produce Project with projections:
        //   [Alias("id"), FunctionCall("stack_multi_alias", [stack(...), "metric", "value"])]
        // (proves the stack dispatch arm is distinct from explode)
        use crate::transpiler_v2::ast::CommonOp;
        use crate::transpiler_v2::expression::{FunctionCall, Literal, LiteralValue};

        let ast = SparkSqlParserV2::parse(
            "SELECT id, stack(2, 'age', age, 'salary', salary) AS (metric, value) FROM emp",
        )
        .expect("pv-006 SQL must parse");

        let projections = match ast.op {
            CommonOp::Project { projections, .. } => projections,
            other => panic!("expected Project, got {other:?}"),
        };
        assert_eq!(projections.len(), 2);

        // projections[1]: stack_multi_alias(stack(...), "metric", "value")
        match &projections[1] {
            Expression::FunctionCall(FunctionCall { name, args, .. }) => {
                assert_eq!(name, "stack_multi_alias");
                assert_eq!(args.len(), 3);
                match &args[0] {
                    Expression::FunctionCall(fc) => {
                        assert!(fc.name.eq_ignore_ascii_case("stack"));
                    }
                    other => panic!("expected inner stack call, got {other:?}"),
                }
                for (i, expected) in ["metric", "value"].iter().enumerate() {
                    match &args[i + 1] {
                        Expression::Literal(Literal {
                            value: LiteralValue::String(s),
                            ..
                        }) => assert_eq!(s, expected),
                        other => panic!("expected string literal '{expected}', got {other:?}"),
                    }
                }
            }
            other => panic!("expected FunctionCall(stack_multi_alias), got {other:?}"),
        }
    }

    #[test]
    fn parse_posexplode_multi_alias_is_boundary_error() {
        // posexplode with 2 aliases on the full-SQL path must be a boundary error
        // (not yet implemented for the SQL path — only explode and stack are).
        use crate::transpiler_v2::error::UnsupportedKind;
        let result = SparkSqlParserV2::parse("SELECT posexplode(arr) AS (p, v) FROM t");
        match result {
            Err(crate::transpiler_v2::EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                ref name,
                ..
            }) => {
                assert!(
                    name.contains("posexplode"),
                    "boundary error name must mention posexplode, got: {name}"
                );
            }
            other => panic!("expected boundary error for posexplode multi-alias, got {other:?}"),
        }
    }

    #[test]
    fn parse_explode_with_three_aliases_is_boundary_error() {
        // explode with 3 aliases (arity mismatch) must be a boundary error.
        use crate::transpiler_v2::error::UnsupportedKind;
        let result = SparkSqlParserV2::parse("SELECT explode(m) AS (a, b, c) FROM t");
        match result {
            Err(crate::transpiler_v2::EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                ref name,
                ..
            }) => {
                assert!(
                    name.contains("explode"),
                    "boundary error name must mention explode, got: {name}"
                );
            }
            other => panic!("expected boundary error for explode 3-alias, got {other:?}"),
        }
    }

    #[test]
    fn parse_unconsumed_sentinel_is_boundary_error() {
        // Deliberately construct a scenario where the sentinel survives into
        // a non-Project node. We use a CTE whose body is a subquery — but
        // actually, the simplest way to trigger this is to have the rewrite
        // produce a sentinel that lands in a non-Project op. Since all SELECT
        // statements produce Projects, a truly unconsumed sentinel is hard to
        // trigger via normal SQL. Instead, test the splice_multi_aliases
        // function directly with a tree that has no Project.
        use crate::transpiler_v2::ast::{CommonAst, CommonOp};
        let mut ast = CommonAst::new(CommonOp::SingleRow);
        let alias_lists = vec![vec!["k".to_owned(), "v".to_owned()]];
        let result = super::multi_alias::splice_multi_aliases(&mut ast, &alias_lists);
        match result {
            Err(crate::transpiler_v2::EmissionError::Unsupported {
                kind: crate::transpiler_v2::error::UnsupportedKind::ProtoShape,
                ref name,
                ..
            }) => {
                assert!(
                    name.contains("unconsumed_sentinel"),
                    "error name must contain 'unconsumed_sentinel', got: {name}"
                );
            }
            other => panic!("expected unconsumed-sentinel boundary error, got {other:?}"),
        }
    }

    // ── Convergence: SQL-path explode pair == DataFrame-path shape ───────────

    #[test]
    fn sql_path_explode_pair_matches_dataframe_path_shape() {
        // The SQL path's spliced explode pair (from splice_multi_aliases) must
        // be structurally identical to what try_convert_posexplode_multi_alias
        // (DataFrame converter) builds. We construct both shapes manually and
        // compare.
        use crate::transpiler_v2::expression::{AliasExpression, FunctionCall};

        // SQL-path shape: build via build_map_explode_pair.
        let arg = Expression::FunctionCall(FunctionCall {
            name: "explode".to_owned(),
            args: vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "attrs".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )],
            distinct: false,
        });
        // The SQL path uses the single argument to explode, not the explode
        // call itself. Extract it.
        let inner_arg = arg.clone();
        let sql_pair = super::multi_alias::build_map_explode_pair(inner_arg, "k", "v");

        // DataFrame-path shape: manual construction matching
        // try_convert_posexplode_multi_alias (v2_relation_converter.rs:1290-1309).
        let df_a = Expression::Alias(AliasExpression {
            expr: Box::new(Expression::FunctionCall(FunctionCall {
                name: "map_explode_key".to_owned(),
                args: vec![arg.clone()],
                distinct: false,
            })),
            alias: "k".to_owned(),
        });
        let df_b = Expression::Alias(AliasExpression {
            expr: Box::new(Expression::FunctionCall(FunctionCall {
                name: "map_explode_val".to_owned(),
                args: vec![arg],
                distinct: false,
            })),
            alias: "v".to_owned(),
        });

        assert_eq!(sql_pair.len(), 2);
        assert_eq!(
            sql_pair[0], df_a,
            "SQL-path key projection must match DataFrame-path shape"
        );
        assert_eq!(
            sql_pair[1], df_b,
            "SQL-path val projection must match DataFrame-path shape"
        );
    }

    // ── parse_statement tests ───────────────────────────────────────────

    mod parse_statement_tests {
        use super::*;
        use crate::transpiler_v2::error::UnsupportedKind;
        use crate::transpiler_v2::statement::{DdlStatement, SqlStatement};
        use crate::transpiler_v2::EmissionError;

        #[test]
        fn plain_select_returns_query() {
            let result = SparkSqlParserV2::parse_statement("SELECT 1 AS x")
                .expect("plain SELECT must parse");
            assert!(
                matches!(result, SqlStatement::Query(_)),
                "expected SqlStatement::Query, got {result:?}"
            );
        }

        #[test]
        fn create_temporary_view_returns_ddl() {
            let result =
                SparkSqlParserV2::parse_statement("CREATE TEMPORARY VIEW v AS SELECT 1 AS x")
                    .expect("CREATE TEMPORARY VIEW must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::CreateTempView {
                    name, or_replace, ..
                }) => {
                    assert_eq!(name, "v");
                    assert!(!or_replace);
                }
                other => panic!("expected CreateTempView, got {other:?}"),
            }
        }

        #[test]
        fn create_temp_view_shorthand() {
            let result = SparkSqlParserV2::parse_statement("CREATE TEMP VIEW v AS SELECT 1 AS x")
                .expect("CREATE TEMP VIEW must parse");
            assert!(
                matches!(
                    result,
                    SqlStatement::Ddl(DdlStatement::CreateTempView { .. })
                ),
                "expected CreateTempView, got {result:?}"
            );
        }

        #[test]
        fn create_or_replace_temporary_view_returns_ddl() {
            let result = SparkSqlParserV2::parse_statement(
                "CREATE OR REPLACE TEMPORARY VIEW v AS SELECT 1 AS x",
            )
            .expect("CREATE OR REPLACE TEMPORARY VIEW must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::CreateTempView {
                    name, or_replace, ..
                }) => {
                    assert_eq!(name, "v");
                    assert!(or_replace);
                }
                other => panic!("expected CreateTempView with or_replace, got {other:?}"),
            }
        }

        #[test]
        fn temp_view_if_not_exists_rejected_as_spark_parse_error() {
            // Spark 4.1.1 raises ParseException:
            // "It is not allowed to define a TEMPORARY view with IF NOT EXISTS."
            let err = SparkSqlParserV2::parse_statement(
                "CREATE TEMPORARY VIEW IF NOT EXISTS v AS SELECT 1 AS x",
            )
            .expect_err("IF NOT EXISTS on temp view must be rejected");
            match err {
                EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name,
                    reason,
                } => {
                    assert_eq!(name, "sql::parse_error");
                    assert!(
                        reason.contains("TEMPORARY view with IF NOT EXISTS"),
                        "expected Spark-parity message, got: {reason}"
                    );
                }
                other => panic!("expected sql::parse_error, got {other:?}"),
            }
        }

        #[test]
        fn or_replace_with_if_not_exists_rejected_as_spark_parse_error() {
            // Spark 4.1.1 raises ParseException:
            // "CREATE VIEW with both IF NOT EXISTS and REPLACE is not allowed."
            let err = SparkSqlParserV2::parse_statement(
                "CREATE OR REPLACE TEMPORARY VIEW IF NOT EXISTS v AS SELECT 1 AS x",
            )
            .expect_err("OR REPLACE + IF NOT EXISTS must be rejected");
            match err {
                EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name,
                    reason,
                } => {
                    assert_eq!(name, "sql::parse_error");
                    assert!(
                        reason.contains("IF NOT EXISTS and REPLACE"),
                        "expected Spark-parity message, got: {reason}"
                    );
                }
                other => panic!("expected sql::parse_error, got {other:?}"),
            }
        }

        #[test]
        fn or_replace_with_if_not_exists_on_persistent_view_also_rejected() {
            // Spark rejects OR REPLACE + IF NOT EXISTS on ALL views (not
            // just temporary). τ mirrors this before the boundary error.
            let err = SparkSqlParserV2::parse_statement(
                "CREATE OR REPLACE VIEW IF NOT EXISTS v AS SELECT 1 AS x",
            )
            .expect_err("OR REPLACE + IF NOT EXISTS on persistent view must be rejected");
            match err {
                EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name,
                    reason,
                } => {
                    assert_eq!(name, "sql::parse_error");
                    assert!(
                        reason.contains("IF NOT EXISTS and REPLACE"),
                        "expected Spark-parity message, got: {reason}"
                    );
                }
                other => panic!("expected sql::parse_error, got {other:?}"),
            }
        }

        #[test]
        fn persistent_create_view_parses_to_create_view_ddl() {
            let result = SparkSqlParserV2::parse_statement("CREATE VIEW v AS SELECT 1 AS x")
                .expect("persistent CREATE VIEW must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::CreateView {
                    name, or_replace, ..
                }) => {
                    assert_eq!(name, "v");
                    assert!(!or_replace);
                }
                other => panic!("expected CreateView, got {other:?}"),
            }
        }

        #[test]
        fn unsupported_column_list_bails_loudly() {
            let err = SparkSqlParserV2::parse_statement("CREATE TEMP VIEW v (a, b) AS SELECT 1, 2")
                .expect_err("column list must bail");
            match err {
                EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name,
                    ..
                } => {
                    assert_eq!(name, "sql::create_view::column_list");
                }
                other => panic!("expected column_list boundary, got {other:?}"),
            }
        }

        // ── CREATE TABLE tests ──────────────────────────────────────────

        #[test]
        fn create_table_parses() {
            let result = SparkSqlParserV2::parse_statement("CREATE TABLE t (id INT, name STRING)")
                .expect("CREATE TABLE must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::CreateTable {
                    name,
                    if_not_exists,
                    columns,
                }) => {
                    assert_eq!(name, "t");
                    assert!(!if_not_exists);
                    assert_eq!(columns.fields.len(), 2);
                    assert_eq!(columns.fields[0].name, "id");
                    assert_eq!(columns.fields[1].name, "name");
                }
                other => panic!("expected CreateTable, got {other:?}"),
            }
        }

        #[test]
        fn create_table_if_not_exists() {
            let result = SparkSqlParserV2::parse_statement("CREATE TABLE IF NOT EXISTS t (id INT)")
                .expect("CREATE TABLE IF NOT EXISTS must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::CreateTable { if_not_exists, .. }) => {
                    assert!(if_not_exists);
                }
                other => panic!("expected CreateTable, got {other:?}"),
            }
        }

        #[test]
        fn create_temporary_table_bails() {
            let err = SparkSqlParserV2::parse_statement("CREATE TEMPORARY TABLE t (id INT)")
                .expect_err("CREATE TEMPORARY TABLE must bail");
            match err {
                EmissionError::Unsupported { name, .. } => {
                    assert_eq!(name, "sql::create_table::temporary");
                }
                other => panic!("expected temporary bail, got {other:?}"),
            }
        }

        #[test]
        fn create_table_ctas_bails() {
            let err = SparkSqlParserV2::parse_statement("CREATE TABLE t AS SELECT 1 AS id")
                .expect_err("CTAS must bail");
            match err {
                EmissionError::Unsupported { name, .. } => {
                    assert_eq!(name, "sql::create_table::ctas");
                }
                other => panic!("expected CTAS bail, got {other:?}"),
            }
        }

        // ── DROP TABLE / VIEW tests ───────────────────────────────────────

        #[test]
        fn drop_table_parses() {
            let result =
                SparkSqlParserV2::parse_statement("DROP TABLE t").expect("DROP TABLE must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::DropTable { name, if_exists }) => {
                    assert_eq!(name, "t");
                    assert!(!if_exists);
                }
                other => panic!("expected DropTable, got {other:?}"),
            }
        }

        #[test]
        fn drop_table_if_exists() {
            let result = SparkSqlParserV2::parse_statement("DROP TABLE IF EXISTS t")
                .expect("DROP TABLE IF EXISTS must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::DropTable { if_exists, .. }) => {
                    assert!(if_exists);
                }
                other => panic!("expected DropTable, got {other:?}"),
            }
        }

        #[test]
        fn drop_view_parses() {
            let result =
                SparkSqlParserV2::parse_statement("DROP VIEW v").expect("DROP VIEW must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::DropView { name, if_exists }) => {
                    assert_eq!(name, "v");
                    assert!(!if_exists);
                }
                other => panic!("expected DropView, got {other:?}"),
            }
        }

        #[test]
        fn drop_view_if_exists() {
            let result = SparkSqlParserV2::parse_statement("DROP VIEW IF EXISTS v")
                .expect("DROP VIEW IF EXISTS must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::DropView { if_exists, .. }) => {
                    assert!(if_exists);
                }
                other => panic!("expected DropView, got {other:?}"),
            }
        }

        // ── INSERT tests ──────────────────────────────────────────────────

        #[test]
        fn insert_values_parses() {
            let result =
                SparkSqlParserV2::parse_statement("INSERT INTO t VALUES (1, 'a'), (2, 'b')")
                    .expect("INSERT VALUES must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::InsertValues { table, rows }) => {
                    assert_eq!(table, "t");
                    assert_eq!(rows.len(), 2);
                    assert_eq!(rows[0].len(), 2);
                }
                other => panic!("expected InsertValues, got {other:?}"),
            }
        }

        #[test]
        fn insert_select_parses() {
            let result = SparkSqlParserV2::parse_statement("INSERT INTO dst SELECT * FROM src")
                .expect("INSERT SELECT must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::InsertSelect { table, .. }) => {
                    assert_eq!(table, "dst");
                }
                other => panic!("expected InsertSelect, got {other:?}"),
            }
        }

        #[test]
        fn insert_column_list_bails() {
            let err = SparkSqlParserV2::parse_statement("INSERT INTO t (a, b) VALUES (1, 2)")
                .expect_err("INSERT with column list must bail");
            match err {
                EmissionError::Unsupported { name, .. } => {
                    assert_eq!(name, "sql::insert::column_list");
                }
                other => panic!("expected column_list bail, got {other:?}"),
            }
        }

        // ── TRUNCATE TABLE tests ──────────────────────────────────────────

        #[test]
        fn truncate_table_parses() {
            let result = SparkSqlParserV2::parse_statement("TRUNCATE TABLE t")
                .expect("TRUNCATE TABLE must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::TruncateTable { name }) => {
                    assert_eq!(name, "t");
                }
                other => panic!("expected TruncateTable, got {other:?}"),
            }
        }

        // ── CREATE VIEW (non-temp) tests ──────────────────────────────────

        #[test]
        fn persistent_create_view_parses() {
            let result = SparkSqlParserV2::parse_statement("CREATE VIEW v AS SELECT 1 AS x")
                .expect("CREATE VIEW must parse");
            match result {
                SqlStatement::Ddl(DdlStatement::CreateView {
                    name, or_replace, ..
                }) => {
                    assert_eq!(name, "v");
                    assert!(!or_replace);
                }
                other => panic!("expected CreateView, got {other:?}"),
            }
        }

        #[test]
        fn existing_parse_unchanged_for_select() {
            // The SELECT-only `parse()` still works and rejects DDL.
            let ok = SparkSqlParserV2::parse("SELECT 1 AS x");
            assert!(ok.is_ok(), "parse() must still accept SELECT");

            let err = SparkSqlParserV2::parse("CREATE TEMP VIEW v AS SELECT 1");
            assert!(err.is_err(), "parse() must still reject DDL");
        }
    }
}
