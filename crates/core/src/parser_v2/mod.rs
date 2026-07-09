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
use crate::transpiler_v2::EmissionError;
use dialect::SparkDialect;

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
        use crate::transpiler_v2::error::UnsupportedKind;
        use sqlparser::parser::Parser;

        // Step 1: token-level rewrite of multi-column aliases.
        let (rewritten_sql, alias_lists) = multi_alias::rewrite_multi_aliases(sql)?;
        let parse_input = if alias_lists.is_empty() {
            sql
        } else {
            rewritten_sql.as_str()
        };

        let dialect = SparkDialect;
        // τ fix pass (review M2): sqlparser errors are boundary
        // failures — the input never reached `CommonAst`, so the correct
        // category is `ProtoShape` (input τ can't ingest), not `Op`
        // (emission arm not implemented).
        let mut stmts =
            Parser::parse_sql(&dialect, parse_input).map_err(|e| EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name: "sql::parse_error".to_owned(),
                reason: e.to_string(),
            })?;
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

    let is_stack = matches!(
        &inner,
        Expression::FunctionCall(fc) if fc.name.eq_ignore_ascii_case("stack")
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
    use crate::transpiler_v2::Expression;

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
}
