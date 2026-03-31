//! SQL generator: translates `LogicalPlan` and `Expression` trees into DuckDB SQL.
//!
//! # Critical invariant
//! Always call `to_sql()` / `generate()`. Never use `Display` or `Debug` impls
//! to produce SQL strings sent to DuckDB.

use crate::error::Result;
use crate::expression::{
    AliasExpression, ArrayLiteralExpression, BetweenExpression, BinaryExpression, BinaryOp,
    CaseWhenExpression, CastExpression, Expression, ExistsSubquery,
    ExtractValueExpression, FrameBoundary, FrameUnit, FunctionCall, InListExpression, InSubquery,
    IntervalExpression, IsDistinctFromExpression, LambdaExpression, LikeExpression, LiteralValue,
    MapLiteralExpression, RowConstructorExpression, ScalarSubquery, SortOrder,
    StructLiteralExpression, UnaryExpression, UnaryOp, UnresolvedColumn,
    WindowFunction,
};
use crate::functions::{CompatMode, FunctionRegistry};
use crate::logical::{
    spark_column_name,
    Aggregate, AliasedRelation, Distinct, DropColumns, Except, Filter, GroupingSets,
    InMemoryRelation, Intersect, Join, Limit, LocalDataRelation, LocalRelation, LogicalPlan,
    NADrop, NADropHow, NAFill, NAReplace, Pivot, Project, RangeRelation, Sample, SelectEntry,
    ApproxQuantile, Describe, ShowString, SingleRowRelation, Sort, SqlRelation, StatCorr, StatCov,
    StatCrosstab, StatFreqItems, StatSampleBy,
    Summary, TableScan, Tail, ToDataFrame, Union, Unpivot, WithColumns, WithCte,
};
use crate::types::{DataType, StructType, TypeInferenceEngine, TypeMapper};

// ── Public API ─────────────────────────────────────────────────────────────────

/// Generates DuckDB-compatible SQL from a `LogicalPlan` tree.
pub struct SqlGenerator {
    mode: CompatMode,
    /// Schema of the current input relation, used for polymorphic function dispatch.
    schema: StructType,
}

impl SqlGenerator {
    pub fn new(mode: CompatMode) -> Self {
        Self { mode, schema: StructType::empty() }
    }

    pub fn relaxed() -> Self {
        Self::new(CompatMode::Relaxed)
    }

    pub fn strict() -> Self {
        Self::new(CompatMode::Strict)
    }

    /// Return a new generator with the given schema for type-aware dispatch.
    fn with_schema(&self, schema: StructType) -> Self {
        Self { mode: self.mode, schema }
    }

    /// Generate a complete SQL statement from the plan.
    pub fn generate(&self, plan: &LogicalPlan) -> Result<String> {
        let sql = self.gen_plan(plan)?;
        if std::env::var("TD_DEBUG_SQL").is_ok() {
            eprintln!("=== TD_DEBUG_SQL ===\n{sql}\n===================");
        }
        Ok(sql)
    }
}

// ── Plan generation ────────────────────────────────────────────────────────────

impl SqlGenerator {
    fn gen_plan(&self, plan: &LogicalPlan) -> Result<String> {
        match plan {
            LogicalPlan::Project(p) => self.gen_project(p),
            LogicalPlan::Filter(f) => self.gen_filter(f),
            LogicalPlan::Aggregate(a) => self.gen_aggregate(a),
            // A bare Join as the top-level plan emits a FROM-clause fragment.
            // For USING joins, SELECT key first then * EXCLUDE key (Spark puts key column first).
            LogicalPlan::Join(j) => {
                let fragment = self.gen_join(j)?;
                if !j.using_columns.is_empty() {
                    let select = gen_using_join_select(&j.using_columns);
                    Ok(format!("{select}\nFROM {fragment}"))
                } else {
                    Ok(format!("SELECT *\nFROM {fragment}"))
                }
            }
            LogicalPlan::Sort(s) => self.gen_sort(s),
            LogicalPlan::Limit(l) => self.gen_limit(l),
            LogicalPlan::Tail(t) => self.gen_tail(t),
            LogicalPlan::Union(u) => self.gen_union(u),
            LogicalPlan::Except(e) => self.gen_except(e),
            LogicalPlan::Intersect(i) => self.gen_intersect(i),
            LogicalPlan::Distinct(d) => self.gen_distinct(d),
            LogicalPlan::Sample(s) => self.gen_sample(s),
            LogicalPlan::TableScan(ts) => self.gen_table_scan(ts),
            LogicalPlan::SqlRelation(sr) => self.gen_sql_relation(sr),
            LogicalPlan::LocalRelation(lr) => self.gen_local_relation(lr),
            LogicalPlan::LocalDataRelation(ldr) => self.gen_local_data_relation(ldr),
            LogicalPlan::RangeRelation(rr) => self.gen_range_relation(rr),
            LogicalPlan::InMemoryRelation(imr) => self.gen_in_memory_relation(imr),
            LogicalPlan::WithCte(w) => self.gen_with_cte(w),
            LogicalPlan::WithColumns(wc) => self.gen_with_columns(wc),
            LogicalPlan::AliasedRelation(ar) => {
                // gen_aliased_relation produces a FROM-clause fragment (inner AS alias).
                // At top level we need a complete SELECT statement.
                let fragment = self.gen_aliased_relation(ar)?;
                Ok(format!("SELECT *\nFROM {fragment}"))
            }
            LogicalPlan::RawDdlStatement(r) => Ok(r.sql.clone()),
            LogicalPlan::ToDataFrame(t) => self.gen_to_dataframe(t),
            LogicalPlan::SingleRow(sr) => self.gen_single_row(sr),
            LogicalPlan::DropColumns(d) => self.gen_drop_columns(d),
            LogicalPlan::ShowString(s) => self.gen_show_string(s),
            LogicalPlan::NADrop(n) => self.gen_na_drop(n),
            LogicalPlan::NAFill(n) => self.gen_na_fill(n),
            LogicalPlan::NAReplace(n) => self.gen_na_replace(n),
            LogicalPlan::Unpivot(u) => self.gen_unpivot(u),
            LogicalPlan::Pivot(p) => self.gen_pivot(p),
            LogicalPlan::StatCov(s) => self.gen_stat_cov(s),
            LogicalPlan::StatCorr(s) => self.gen_stat_corr(s),
            LogicalPlan::ApproxQuantile(aq) => self.gen_approx_quantile(aq),
            LogicalPlan::StatCrosstab(s) => self.gen_stat_crosstab(s),
            LogicalPlan::StatFreqItems(s) => self.gen_stat_freq_items(s),
            LogicalPlan::StatSampleBy(s) => self.gen_stat_sample_by(s),
            LogicalPlan::Describe(d) => self.gen_describe(d),
            LogicalPlan::Summary(s) => self.gen_summary(s),
        }
    }

    fn gen_project(&self, p: &Project) -> Result<String> {
        // Use input schema for polymorphic function dispatch (e.g. reverse on arrays vs strings).
        let input_schema = p.input.infer_schema();
        let typed_gen = self.with_schema(input_schema);
        let cols = typed_gen.gen_projection_list(&p.projections)?;
        // SingleRow means no FROM clause (e.g. SELECT 1, SELECT now())
        if matches!(p.input.as_ref(), LogicalPlan::SingleRow(_)) {
            return Ok(format!("SELECT {cols}"));
        }
        // Peel any Filter nodes so join table aliases remain in scope in the outer SELECT.
        // Without this, Filter(Join(n1, n2)) gets wrapped in a subquery and n1/n2 become
        // inaccessible to the projection's column references.
        // Use typed_gen for WHERE conditions so schema-based dispatch (e.g. size on MAP) works.
        let (base, conditions) = Self::extract_filters(&p.input);
        let from = self.gen_from(base)?;
        let mut sql = format!("SELECT {cols}\nFROM {from}");
        if !conditions.is_empty() {
            let where_parts = conditions.iter()
                .map(|c| Ok(format!("({})", typed_gen.gen_expr(c)?)))
                .collect::<Result<Vec<_>>>()?;
            sql.push_str(&format!("\nWHERE {}", where_parts.join("\nAND ")));
        }
        Ok(sql)
    }

    fn gen_filter(&self, f: &Filter) -> Result<String> {
        // Peel any stacked inner filters so join table aliases remain in scope.
        // Without this, Filter(outer, Filter(inner, Join(l1, ...))) wraps the inner filter in a
        // subquery, making the l1 alias invisible to the outer condition.
        let (base, inner_conditions) = Self::extract_filters(&f.input);
        let from = self.gen_from(base)?;
        let input_schema = base.infer_schema();
        let typed_gen = self.with_schema(input_schema);
        // Current filter + any peeled inner filters — each wrapped in parens to avoid precedence issues.
        let all_conditions = std::iter::once(&f.condition)
            .chain(inner_conditions.into_iter())
            .map(|c| Ok(format!("({})", typed_gen.gen_expr(c)?)))
            .collect::<Result<Vec<_>>>()?;
        Ok(format!("SELECT *\nFROM {from}\nWHERE {}", all_conditions.join("\nAND ")))
    }

    /// Render a single aggregate expression: apply type casts, inject DISTINCT, append FILTER.
    fn render_agg_expr(
        &self,
        ae: &crate::logical::AggregateExpr,
        input_schema: &StructType,
    ) -> Result<String> {
        let func = apply_agg_type_casts(&ae.func, input_schema);
        let mut s = self.gen_expr(&func)?;
        if ae.is_distinct {
            s = inject_distinct(s);
        }
        if let Some(filter) = &ae.filter {
            s = format!("{s} FILTER (WHERE {})", self.gen_expr(filter)?);
        }
        // Always add an explicit Spark-compatible alias for unaliased aggregates.
        // Without this, DuckDB uses its own naming conventions (e.g., count_star() for COUNT(*),
        // or "CAST(sum(x) AS BIGINT)" when we wrap with CAST), which don't match Spark output.
        if !matches!(&ae.func, Expression::Alias(_)) {
            let spark_name = spark_column_name(&ae.func);
            let escaped = spark_name.replace('"', "\"\"");
            s = format!("{s} AS \"{escaped}\"");
        }
        Ok(s)
    }

    fn gen_aggregate(&self, a: &Aggregate) -> Result<String> {
        // Infer input schema once for type-aware aggregate cast wrapping.
        let input_schema = a.input.infer_schema();

        // Build SELECT list
        let select_list = if a.select_order.is_empty() {
            // Default: grouping columns first, then aggregates
            let mut parts = Vec::new();
            for g in &a.grouping {
                parts.push(self.gen_expr(g)?);
            }
            for ae in &a.aggregates {
                parts.push(self.render_agg_expr(ae, &input_schema)?);
            }
            parts.join(", ")
        } else {
            let mut parts = Vec::new();
            // If no grouping columns appear in select_order (DataFrame shorthand where groupBy
            // keys are not sent in aggregate_expressions), prepend them.
            // SQL path sets GroupingNotSelected to suppress this prepend when GROUP BY keys
            // are intentionally excluded from the SQL SELECT list.
            let has_grouping_in_order = a.select_order.iter()
                .any(|e| matches!(e, SelectEntry::GroupingExpr(_) | SelectEntry::GroupingNotSelected));
            if !has_grouping_in_order {
                for g in &a.grouping {
                    parts.push(self.gen_expr(g)?);
                }
            }
            for entry in &a.select_order {
                match entry {
                    SelectEntry::GroupingExpr(g) => parts.push(self.gen_expr(g)?),
                    SelectEntry::AggregateExpr(idx) => {
                        parts.push(self.render_agg_expr(&a.aggregates[*idx], &input_schema)?);
                    }
                    SelectEntry::GroupingNotSelected => {
                        // Sentinel: GROUP BY key not in SQL SELECT — suppress auto-prepend, render nothing.
                    }
                }
            }
            parts.join(", ")
        };

        // Peel pre-aggregation filters so join table aliases remain in scope.
        let (base_input, pre_filters) = Self::extract_filters(&a.input);
        let from = self.gen_from(base_input)?;
        let mut sql = format!("SELECT {select_list}\nFROM {from}");

        // Pre-aggregation WHERE (from peeled filters) — use input schema for dispatch.
        if !pre_filters.is_empty() {
            let filter_gen = self.with_schema(input_schema.clone());
            let where_parts = pre_filters.iter()
                .map(|c| Ok(format!("({})", filter_gen.gen_expr(c)?)))
                .collect::<Result<Vec<_>>>()?;
            sql.push_str(&format!("\nWHERE {}", where_parts.join("\nAND ")));
        }

        // GROUP BY
        if !a.grouping.is_empty() || a.grouping_sets.is_some() {
            if let Some(gs) = &a.grouping_sets {
                sql.push_str(&format!("\nGROUP BY {}", self.gen_grouping_sets(gs)?));
            } else {
                let gb = a.grouping.iter()
                    .map(|g| {
                        // Strip outer alias — GROUP BY must use bare expressions, not `expr AS alias`
                        let inner = if let Expression::Alias(a) = g { &*a.expr } else { g };
                        self.gen_expr(inner)
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join(", ");
                sql.push_str(&format!("\nGROUP BY {gb}"));
            }
        }

        // HAVING
        if let Some(having) = &a.having {
            let h = self.gen_expr(having)?;
            sql.push_str(&format!("\nHAVING {h}"));
        }

        Ok(sql)
    }

    fn gen_grouping_sets(&self, gs: &GroupingSets) -> Result<String> {
        match gs {
            GroupingSets::Rollup(sets) => {
                let inner = self.gen_grouping_set_list(sets)?;
                Ok(format!("ROLLUP({inner})"))
            }
            GroupingSets::Cube(sets) => {
                let inner = self.gen_grouping_set_list(sets)?;
                Ok(format!("CUBE({inner})"))
            }
            GroupingSets::GroupingSets(sets) => {
                let inner = sets.iter()
                    .map(|set| {
                        let exprs = set.iter()
                            .map(|e| self.gen_expr(e))
                            .collect::<Result<Vec<_>>>()?
                            .join(", ");
                        Ok(format!("({exprs})"))
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join(", ");
                Ok(format!("GROUPING SETS({inner})"))
            }
        }
    }

    fn gen_grouping_set_list(&self, sets: &[Vec<Expression>]) -> Result<String> {
        sets.iter()
            .map(|set| {
                let exprs = set.iter()
                    .map(|e| self.gen_expr(e))
                    .collect::<Result<Vec<_>>>()?
                    .join(", ");
                Ok(exprs)
            })
            .collect::<Result<Vec<_>>>()
            .map(|v| v.join(", "))
    }

    fn gen_join(&self, j: &Join) -> Result<String> {
        // When plan_id-based column qualification is active, wrap each side in a named subquery
        // so DuckDB can unambiguously resolve same-named columns from different sides.
        //
        // Special case: when the RIGHT side is an AliasedRelation with a user-facing alias
        // (not a __td_jr__ internal alias), we can use a "natural flat join" pattern:
        // - Use the AliasedRelation's own alias directly (keeps it accessible in outer WHERE)
        // - Don't wrap the left side in a subquery either (expose all nested aliases)
        // - Rewrite ON condition: substitute __td_jr_X__ → natural_alias, strip __td_jl_X__
        //
        // This fixes queries like TPC-DS Q17/Q25/Q29 where date_dim.alias("d1") is joined
        // multiple times and filter conditions reference d1/d2/d3 in the outer WHERE clause.
        // Detect the natural flat join pattern:
        // right_alias is a __td_jr_X__ alias AND j.right is an AliasedRelation with a
        // user-facing alias (e.g. "d1"). In this case we can use the natural alias directly
        // to keep it accessible in outer WHERE/HAVING clauses.
        // Check whether the left side is also a user-aliased relation (e.g. self-join with
        // df.alias("a").join(df.alias("b"), ...)). If so, skip the natural flat join path:
        // stripping __td_jl_X__ qualifiers would create ambiguous column refs when both sides
        // share the same column names.
        let left_is_user_alias = if let Some(la) = &j.left_alias {
            if la.starts_with("__td_jl_") {
                matches!(j.left.as_ref(), LogicalPlan::AliasedRelation(ar) if !ar.alias.starts_with("__"))
            } else {
                false
            }
        } else {
            false
        };

        let right_natural_alias: Option<(String, String)> = if let Some(ra) = &j.right_alias {
            if ra.starts_with("__td_jr_") && !left_is_user_alias && !j.join_type.is_semi_or_anti() {
                match j.right.as_ref() {
                    LogicalPlan::AliasedRelation(ar) if !ar.alias.starts_with("__") => {
                        // Case 1: right is AliasedRelation with user-facing alias (e.g. "d1")
                        Some((ar.alias.clone(), ra.clone()))
                    }
                    right_plan => {
                        // Case 2: right is a plain table (TableScan, InMemoryRelation).
                        // Only use flat path if left side contains user-facing AliasedRelations
                        // that would be buried in a subquery (e.g. d1/d2/d3 in Q17).
                        if plan_contains_user_alias(&j.left) {
                            right_plan_natural_name(right_plan).map(|name| (name, ra.clone()))
                        } else {
                            None
                        }
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let (left, right, effective_condition) = if let Some((nat_alias, td_alias)) = right_natural_alias {
            // Natural flat join: use user-facing alias directly.
            // The right side is an AliasedRelation; generate it without additional wrapping.
            let right_sql = self.gen_from(&j.right)?; // "table AS natural_alias"
            // Generate left side as a flat FROM fragment (no subquery wrap).
            let left_sql = self.gen_from(&j.left)?;
            // Rewrite ON condition: replace __td_jr_X__ qualifier → natural_alias,
            // strip __td_jl_X__ qualifiers (left-side columns are unqualified in flat join).
            let effective_cond = j.condition.as_ref().map(|c| {
                rewrite_td_join_qualifiers(c.clone(), &td_alias, &nat_alias)
            });
            (left_sql, right_sql, effective_cond)
        } else if j.left_alias.is_some() || j.right_alias.is_some() {
            let la = j.left_alias.as_deref().unwrap_or("__td_jl__");
            let ra = j.right_alias.as_deref().unwrap_or("__td_jr__");
            // Use gen_from to get a valid FROM-clause fragment, then wrap in SELECT * so we
            // always produce a complete query that can be used as a named subquery.
            // gen_from handles all plan types correctly (wraps non-leaf plans in parens).
            let lsql = format!("SELECT *\nFROM {}", self.gen_from(&j.left)?);
            let rsql = format!("SELECT *\nFROM {}", self.gen_from(&j.right)?);
            let cond = j.condition.clone();
            (
                format!("(\n{lsql}\n) AS {}", quote_ident(la)),
                format!("(\n{rsql}\n) AS {}", quote_ident(ra)),
                cond,
            )
        } else {
            (self.gen_from(&j.left)?, self.gen_from(&j.right)?, j.condition.clone())
        };

        let kw = j.join_type.sql_keyword();

        if j.join_type.is_semi_or_anti() {
            // DuckDB SEMI/ANTI JOIN syntax — returns only left-side columns.
            // When the left side is a wrapped subquery (no plan_id aliases), table aliases
            // inside it (like "l1") are not accessible from the ON condition. Strip those
            // qualifiers so DuckDB can find the columns unqualified from the subquery output.
            let cond = match &effective_condition {
                Some(c) => {
                    if j.left_alias.is_none() {
                        let mut left_aliases = std::collections::HashSet::new();
                        collect_plan_aliases(&j.left, &mut left_aliases);
                        let effective_c;
                        let c_ref = if !left_aliases.is_empty() {
                            effective_c = strip_qualifiers_in_expr(c.clone(), &left_aliases);
                            &effective_c
                        } else {
                            c
                        };
                        if let Some(using_cols) = equijoin_to_using(c_ref) {
                            let cols = using_cols.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ");
                            return Ok(format!("{left}\n{kw} {right} USING ({cols})"));
                        }
                        format!(" ON {}", self.gen_expr(c_ref)?)
                    } else {
                        format!(" ON {}", self.gen_expr(c)?)
                    }
                }
                None if !j.using_columns.is_empty() => {
                    let cols = j.using_columns.iter()
                        .map(|c| quote_ident(c))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(" USING ({cols})")
                }
                None => String::new(),
            };
            return Ok(format!("{left}\n{kw} {right}{cond}"));
        }

        let join_clause = if !j.using_columns.is_empty() {
            let cols = j.using_columns.iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" USING ({cols})")
        } else if let Some(cond) = &effective_condition {
            // When plan_id aliases are active (including USING joins converted by converter),
            // use ON directly (columns already qualified).
            // Otherwise, normalise simple same-name equijoin conditions to USING to avoid
            // DuckDB "Ambiguous reference" errors.
            if j.left_alias.is_none() {
                if let Some(using_cols) = equijoin_to_using(cond) {
                    let cols = using_cols.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ");
                    return Ok(format!("{left}\n{kw} {right} USING ({cols})"));
                }
            }
            format!(" ON {}", self.gen_expr(cond)?)
        } else {
            String::new() // CROSS JOIN
        };

        Ok(format!("{left}\n{kw} {right}{join_clause}"))
    }

    fn gen_sort(&self, s: &Sort) -> Result<String> {
        // For ROLLUP/CUBE aggregates, Spark always sorts NULL grouping values first.
        let (base_input, _) = Self::extract_filters(&s.input);
        let is_rollup_or_cube = matches!(base_input, LogicalPlan::Aggregate(a)
            if matches!(a.grouping_sets.as_ref(),
                Some(GroupingSets::Rollup(_)) | Some(GroupingSets::Cube(_))));

        let mut sql = if !s.order.is_empty() {
            let order = s.order.iter()
                .map(|o| {
                    if is_rollup_or_cube {
                        use crate::expression::NullOrdering;
                        let mut o = o.clone();
                        o.null_ordering = NullOrdering::NullsFirst;
                        self.gen_sort_order(&o)
                    } else {
                        self.gen_sort_order(o)
                    }
                })
                .collect::<Result<Vec<_>>>()?
                .join(", ");

            // Choose between two ORDER BY strategies:
            //
            // 1. ROW_NUMBER wrapper (stable): ORDER BY exprs are all simple unqualified
            //    column refs → they pass through SELECT * unchanged, so ROW_NUMBER gives
            //    deterministic tie-breaking matching Spark's insertion-order stability.
            //
            // 2. Inline ORDER BY: complex exprs (aggregates, qualified names, window
            //    functions) that must stay in-scope of the inner plan's CTEs / GROUP BY.
            //    DuckDB cannot re-evaluate aggregate functions in an outer ORDER BY.
            let all_simple = s.order.iter().all(|o| {
                matches!(&o.expr,
                    Expression::ColumnReference(cr) if cr.qualifier.is_none()
                ) || matches!(&o.expr,
                    Expression::UnresolvedColumn(uc) if uc.qualifier.is_none()
                )
            });

            // For the ROW_NUMBER wrapper to work, every ORDER BY column must be visible
            // in the inner plan's OUTPUT schema (post-alias names). If a column was aliased
            // (e.g. SELECT i_brand_id AS brand_id) and ORDER BY uses the original name
            // (i_brand_id), the wrapper hides it. Check via infer_schema; if schema is
            // unavailable (empty) or a column is missing, fall through to inline ORDER BY.
            let use_stable_wrap = if all_simple {
                let inner_schema = s.input.infer_schema();
                !inner_schema.is_empty() && s.order.iter().all(|o| {
                    let col_name = match &o.expr {
                        Expression::ColumnReference(cr) => cr.name.as_str(),
                        Expression::UnresolvedColumn(uc) => uc.name.as_str(),
                        _ => return false,
                    };
                    inner_schema.fields.iter().any(|f| f.name == col_name)
                })
            } else {
                false
            };

            if use_stable_wrap {
                // Stable path: wrap inner FROM with ROW_NUMBER for tie-breaking.
                let from = self.gen_from(&s.input)?;
                format!(
                    "SELECT * EXCLUDE (\"__td_rn__\")\nFROM (\n  SELECT *, ROW_NUMBER() OVER () AS \"__td_rn__\"\n  FROM {from}\n) \"__td_stable__\"\nORDER BY {order}, \"__td_rn__\""
                )
            } else {
                // Inline path: append ORDER BY to the full plan SQL so that CTE aliases,
                // pre-alias column names, and aggregate expressions remain in scope.
                let inner_sql = self.gen_plan(&s.input)?;
                format!("{inner_sql}\nORDER BY {order}")
            }
        } else {
            let from = self.gen_from(&s.input)?;
            format!("SELECT *\nFROM {from}")
        };

        if let Some(limit) = &s.limit {
            sql.push_str(&format!("\nLIMIT {}", self.gen_expr(limit)?));
        } else if s.offset.is_some() {
            // DuckDB requires LIMIT before OFFSET; use ALL to avoid restricting rows.
            sql.push_str("\nLIMIT ALL");
        }
        if let Some(offset) = &s.offset {
            sql.push_str(&format!("\nOFFSET {}", self.gen_expr(offset)?));
        }

        Ok(sql)
    }

    fn gen_limit(&self, l: &Limit) -> Result<String> {
        let from = self.gen_from(&l.input)?;
        let n = self.gen_expr(&l.limit)?;
        Ok(format!("SELECT *\nFROM {from}\nLIMIT {n}"))
    }

    fn gen_tail(&self, t: &Tail) -> Result<String> {
        // Assign stable row numbers, take the last N in reverse, restore original order.
        // rowid is unreliable on subquery results; ROW_NUMBER() is the correct approach.
        let inner = self.gen_from(&t.input)?;
        let n = self.gen_expr(&t.limit)?;
        Ok(format!(
            "SELECT * EXCLUDE (\"__rn\") FROM (\n  \
             SELECT * FROM (\n    \
             SELECT *, ROW_NUMBER() OVER () AS \"__rn\" FROM {inner}\n  \
             ) ORDER BY \"__rn\" DESC LIMIT {n}\n\
             ) ORDER BY \"__rn\" ASC"
        ))
    }

    fn gen_union(&self, u: &Union) -> Result<String> {
        let left_schema = u.left.infer_schema();
        let right_schema = u.right.infer_schema();
        let left_sql = self.gen_plan(&u.left)?;
        let right_sql = self.gen_plan(&u.right)?;
        let kw = if u.all { "UNION ALL" } else { "UNION" };

        // Emit widening CASTs when left/right column types differ so DuckDB column
        // types match the Spark-promoted schema (e.g. INT + LONG → BIGINT on both sides).
        if !left_schema.is_empty()
            && !right_schema.is_empty()
            && left_schema.fields.len() == right_schema.fields.len()
        {
            let needs_cast = left_schema.fields.iter().zip(&right_schema.fields)
                .any(|(l, r)| l.data_type != r.data_type);

            if needs_cast {
                let target_types: Vec<DataType> = left_schema.fields.iter()
                    .zip(&right_schema.fields)
                    .map(|(l, r)| {
                        let promoted = TypeInferenceEngine::promote_numeric(&l.data_type, &r.data_type);
                        // For non-numeric pairs promote_numeric returns Double — keep left type.
                        if promoted == DataType::Double
                            && !l.data_type.is_numeric() && !r.data_type.is_numeric()
                        { l.data_type.clone() } else { promoted }
                    })
                    .collect();

                let left_cols = left_schema.fields.iter().zip(&target_types).map(|(f, t)| {
                    let q = quote_ident(&f.name);
                    if f.data_type == *t || *t == DataType::Unresolved { q.clone() }
                    else { format!("CAST({q} AS {}) AS {q}", TypeMapper::to_duckdb(t)) }
                }).collect::<Vec<_>>().join(", ");

                let right_cols = right_schema.fields.iter().zip(&target_types).map(|(f, t)| {
                    let q = quote_ident(&f.name);
                    if f.data_type == *t || *t == DataType::Unresolved { q.clone() }
                    else { format!("CAST({q} AS {}) AS {q}", TypeMapper::to_duckdb(t)) }
                }).collect::<Vec<_>>().join(", ");

                return Ok(format!(
                    "(SELECT {left_cols} FROM ({left_sql}) \"__ul__\")\n{kw}\n\
                     (SELECT {right_cols} FROM ({right_sql}) \"__ur__\")"
                ));
            }
        }

        Ok(format!("({left_sql})\n{kw}\n({right_sql})"))
    }

    fn gen_except(&self, e: &Except) -> Result<String> {
        let left = self.gen_plan(&e.left)?;
        let right = self.gen_plan(&e.right)?;
        let kw = if e.all { "EXCEPT ALL" } else { "EXCEPT" };
        Ok(format!("({left})\n{kw}\n({right})"))
    }

    fn gen_intersect(&self, i: &Intersect) -> Result<String> {
        let left = self.gen_plan(&i.left)?;
        let right = self.gen_plan(&i.right)?;
        let kw = if i.all { "INTERSECT ALL" } else { "INTERSECT" };
        Ok(format!("({left})\n{kw}\n({right})"))
    }

    fn gen_distinct(&self, d: &Distinct) -> Result<String> {
        let from = self.gen_from(&d.input)?;
        if d.columns.is_empty() {
            return Ok(format!("SELECT DISTINCT *\nFROM {from}"));
        }
        // dropDuplicates(cols) — keep one row per distinct group of specified columns.
        // Use ROW_NUMBER() OVER (PARTITION BY cols) to pick one row per group.
        let partition = d.columns.iter()
            .map(|c| self.gen_expr(c))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        Ok(format!(
            "SELECT * EXCLUDE (\"__td_dd_rn__\")\nFROM (\n  SELECT *, ROW_NUMBER() OVER (PARTITION BY {partition}) AS \"__td_dd_rn__\"\n  FROM {from}\n)\nWHERE \"__td_dd_rn__\" = 1"
        ))
    }

    fn gen_sample(&self, s: &Sample) -> Result<String> {
        if s.with_replacement {
            return Err(crate::error::ThunderduckError::Unsupported(
                "df.sample(withReplacement=True) is not supported; \
                 DuckDB has no row-level sampling with replacement"
                    .into(),
            ));
        }
        let from = self.gen_from(&s.input)?;
        let pct = s.fraction * 100.0;
        // DuckDB: TABLESAMPLE BERNOULLI(pct PERCENT) REPEATABLE(seed)
        let seed_clause = match s.seed {
            Some(seed) => format!(" REPEATABLE({seed})"),
            None => String::new(),
        };
        Ok(format!("SELECT * FROM {from} TABLESAMPLE BERNOULLI({pct:.4} PERCENT){seed_clause}"))
    }

    fn gen_table_scan(&self, ts: &TableScan) -> Result<String> {
        let tbl = quote_ident(&ts.table);
        match &ts.alias {
            Some(a) => Ok(format!("{tbl} AS {}", quote_ident(a))),
            None => Ok(tbl),
        }
    }

    fn gen_sql_relation(&self, sr: &SqlRelation) -> Result<String> {
        // DDL/DML statements (CREATE, DROP, INSERT, UPDATE, DELETE, ALTER, TRUNCATE)
        // cannot be wrapped in parens — return them verbatim.
        //
        // DDL SqlRelations are produced by sql_converter.rs after full FunctionRegistry
        // translation and plan_to_sql conversion. They are already DuckDB-ready and must
        // NOT be run through preprocess_spark_sql again (doing so double-processes things
        // like MAP([keys], [vals]) → MAP([[keys]], [[vals]])).
        //
        // Non-DDL SqlRelations may carry raw Spark SQL from legacy paths and need preprocessing.
        let upper = sr.sql.trim_start().to_uppercase();
        let is_ddl = upper.starts_with("CREATE")
            || upper.starts_with("DROP")
            || upper.starts_with("INSERT")
            || upper.starts_with("UPDATE")
            || upper.starts_with("DELETE")
            || upper.starts_with("ALTER")
            || upper.starts_with("TRUNCATE")
            || upper.starts_with("SET");
        if is_ddl {
            Ok(sr.sql.clone())
        } else {
            Ok(format!("({})", preprocess_spark_sql(&sr.sql)))
        }
    }

    fn gen_local_relation(&self, lr: &LocalRelation) -> Result<String> {
        // Empty relation: generate VALUES(NULL, NULL, ...) with 0 rows
        // DuckDB: SELECT ... WHERE FALSE is simplest
        if lr.schema.fields.is_empty() {
            return Ok("(SELECT 1 WHERE FALSE)".to_string());
        }
        let cols = lr.schema.fields.iter()
            .map(|f| {
                let dt = TypeMapper::to_duckdb(&f.data_type);
                format!("CAST(NULL AS {dt}) AS {}", quote_ident(&f.name))
            })
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!("(SELECT {cols} WHERE FALSE)"))
    }

    fn gen_local_data_relation(&self, ldr: &LocalDataRelation) -> Result<String> {
        self.gen_local_relation(&LocalRelation { schema: ldr.schema.clone() })
    }

    fn gen_range_relation(&self, rr: &RangeRelation) -> Result<String> {
        // DuckDB ≥1.1.5: range(start, end, step) returns a table with column "range"
        // We alias it as `id` to match Spark's range relation schema.
        Ok(format!(
            "(SELECT \"range\" AS id FROM range({}, {}, {}))",
            rr.start, rr.end, rr.step
        ))
    }

    fn gen_in_memory_relation(&self, imr: &InMemoryRelation) -> Result<String> {
        Ok(quote_ident(&imr.view_name))
    }

    fn gen_with_cte(&self, w: &WithCte) -> Result<String> {
        let cte_parts = w.ctes.iter()
            .map(|(name, plan)| {
                let sql = self.gen_plan(plan)?;
                Ok(format!("{} AS (\n{}\n)", quote_ident(name), sql))
            })
            .collect::<Result<Vec<_>>>()?
            .join(",\n");
        let body = self.gen_plan(&w.input)?;
        Ok(format!("WITH {cte_parts}\n{body}"))
    }

    fn gen_with_columns(&self, wc: &WithColumns) -> Result<String> {
        // withColumnsRenamed: EXCLUDE old column, add it under new name
        //   SELECT * EXCLUDE ("old"), "old" AS "new" FROM input
        // withColumn (add/replace/rename): build SQL using DuckDB star modifiers.
        //   Pure rename:  SELECT * RENAME ("old" AS "new") FROM input  [preserves position]
        //   Add/replace:  SELECT * EXCLUDE ("col"), expr AS "col" FROM input  (appends)
        //   New column:   SELECT *, expr AS "col" FROM input
        let from = self.gen_from(&wc.input)?;

        let mut renames: Vec<String> = Vec::new();  // "old" AS "new" for * RENAME
        let mut adds: Vec<String> = Vec::new();     // expr AS col to append

        for (new_name, expr) in &wc.columns {
            let old_col_name: Option<&str> = match expr {
                Expression::UnresolvedColumn(uc) if uc.name != *new_name => Some(&uc.name),
                Expression::ColumnReference(cr) if cr.name != *new_name => Some(&cr.name),
                _ => None,
            };
            if let Some(old_name) = old_col_name {
                // Pure rename — use * RENAME to preserve column position
                renames.push(format!("{} AS {}", quote_ident(old_name), quote_ident(new_name)));
            } else {
                // Add new column or replace existing in-place.
                // We don't have schema info so can't know if col exists; use REPLACE which
                // keeps col in position if it exists and fails gracefully if not.
                // Fallback: just append (SELECT *, expr AS col).
                let e = self.gen_expr(expr)?;
                adds.push(format!("{e} AS {}", quote_ident(new_name)));
            }
        }

        // Build the SELECT list.
        // For expression-based columns we use COLUMNS(c -> c NOT IN ('col1','col2'))
        // instead of bare `*` so that an existing column is excluded before being
        // re-added with the new expression.  If the column doesn't exist the lambda
        // evaluates true for all columns (nothing excluded) — which is exactly the
        // "add new column" behaviour.  This gives correct add-OR-replace semantics
        // without needing to know the input schema at code-generation time.
        if adds.is_empty() {
            // Rename-only: use * RENAME which preserves column positions
            let star = if renames.is_empty() {
                "*".to_string()
            } else {
                format!("* RENAME ({})", renames.join(", "))
            };
            return Ok(format!("SELECT {star}\nFROM {from}"));
        }

        // Collect the names being added/replaced (for the exclusion lambda).
        let excluded_names: Vec<String> = wc.columns.iter()
            .filter(|(new_name, expr)| {
                // Only expression cols (not pure renames) go into `adds`
                !matches!(expr,
                    Expression::UnresolvedColumn(uc) if uc.name != *new_name
                ) && !matches!(expr,
                    Expression::ColumnReference(cr) if cr.name != *new_name
                )
            })
            .map(|(new_name, _)| format!("'{}'", new_name.replace('\'', "''")))
            .collect();

        let excl_lambda = if excluded_names.is_empty() {
            "c -> true".to_string()
        } else {
            format!("c -> c NOT IN ({})", excluded_names.join(", "))
        };

        let star_part = if renames.is_empty() {
            format!("COLUMNS({excl_lambda})")
        } else {
            format!("COLUMNS({excl_lambda}) RENAME ({})", renames.join(", "))
        };

        Ok(format!("SELECT {star_part}, {}\nFROM {from}", adds.join(", ")))
    }

    fn gen_aliased_relation(&self, ar: &AliasedRelation) -> Result<String> {
        let inner = self.gen_from(&ar.input)?;
        let alias = quote_ident(&ar.alias);
        if ar.column_aliases.is_empty() {
            Ok(format!("{inner} AS {alias}"))
        } else {
            let cols = ar.column_aliases.iter()
                .map(|c| quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(format!("{inner} AS {alias}({cols})"))
        }
    }

    fn gen_to_dataframe(&self, t: &ToDataFrame) -> Result<String> {
        let inner = self.gen_plan(&t.input)?;
        let schema = t.input.infer_schema();
        if schema.fields.is_empty() {
            // Schema unresolvable at generation time (e.g. SqlRelation, TableScan).
            // The converter (Phase 3) must attach a resolved schema before reaching here;
            // until then, pass through and let DuckDB resolve column names at runtime.
            return Ok(format!("SELECT *\nFROM ({inner})"));
        }
        let cols = schema.fields.iter()
            .zip(t.column_names.iter())
            .map(|(f, new_name)| format!("{} AS {}", quote_ident(&f.name), quote_ident(new_name)))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!("SELECT {cols}\nFROM ({inner})"))
    }

    fn gen_single_row(&self, _sr: &SingleRowRelation) -> Result<String> {
        // SingleRow is used as an input to Project (no FROM needed).
        // If somehow generated standalone, emit a no-op that produces one row.
        Ok("SELECT 1".to_string())
    }

    fn gen_drop_columns(&self, d: &DropColumns) -> Result<String> {
        let from = self.gen_from(&d.input)?;
        let excluded = d
            .column_names
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!("SELECT * EXCLUDE ({excluded})\nFROM {from}"))
    }

    fn gen_show_string(&self, s: &ShowString) -> Result<String> {
        // Phase 3 stub: just delegate to the input with an optional LIMIT.
        // PySpark formats the ASCII table client-side from the returned rows.
        let inner = self.gen_plan(&s.input)?;
        Ok(format!("SELECT *\nFROM ({inner})\nLIMIT {}", s.num_rows))
    }

    fn gen_na_drop(&self, n: &NADrop) -> Result<String> {
        let from = self.gen_from(&n.input)?;
        if n.cols.is_empty() {
            return Ok(format!("SELECT *\nFROM {from}"));
        }
        let cond = if let Some(thresh) = n.threshold {
            // Keep rows where the count of non-null values meets the threshold
            let sum_parts: Vec<String> = n
                .cols
                .iter()
                .map(|c| format!("CASE WHEN {} IS NOT NULL THEN 1 ELSE 0 END", quote_ident(c)))
                .collect();
            format!("({}) >= {}", sum_parts.join(" + "), thresh)
        } else {
            match n.how {
                NADropHow::Any => n
                    .cols
                    .iter()
                    .map(|c| format!("{} IS NOT NULL", quote_ident(c)))
                    .collect::<Vec<_>>()
                    .join(" AND "),
                NADropHow::All => {
                    let nulls = n
                        .cols
                        .iter()
                        .map(|c| format!("{} IS NULL", quote_ident(c)))
                        .collect::<Vec<_>>()
                        .join(" AND ");
                    format!("NOT ({nulls})")
                }
            }
        };
        Ok(format!("SELECT *\nFROM {from}\nWHERE {cond}"))
    }

    fn gen_na_fill(&self, n: &NAFill) -> Result<String> {
        let from = self.gen_from(&n.input)?;
        let mut select_parts: Vec<String> = Vec::with_capacity(n.all_columns.len());
        for col in &n.all_columns {
            let qname = quote_ident(col);
            // Linear scan: column counts are small (typically < 50) so no HashMap needed.
            if let Some((_, lit)) = n.values.iter().find(|(c, _)| c == col) {
                let lit_sql = self.gen_expr(&crate::expression::Expression::Literal(lit.clone()))?;
                select_parts.push(format!("COALESCE({qname}, {lit_sql}) AS {qname}"));
            } else {
                select_parts.push(qname);
            }
        }
        Ok(format!("SELECT {}\nFROM {from}", select_parts.join(", ")))
    }

    fn gen_na_replace(&self, n: &NAReplace) -> Result<String> {
        let from = self.gen_from(&n.input)?;
        // Group replacements by column
        let mut col_replacements: std::collections::HashMap<&str, Vec<(&crate::expression::Literal, &crate::expression::Literal)>> =
            std::collections::HashMap::with_capacity(n.replacements.len());
        for (col, from_val, to_val) in &n.replacements {
            col_replacements.entry(col.as_str()).or_default().push((from_val, to_val));
        }
        let mut select_parts: Vec<String> = Vec::with_capacity(n.all_columns.len());
        for col in &n.all_columns {
            let qname = quote_ident(col);
            if let Some(repls) = col_replacements.get(col.as_str()) {
                // Build CASE WHEN col = from THEN to ... ELSE col END
                let mut when_clauses: Vec<String> = Vec::with_capacity(repls.len());
                for (from_lit, to_lit) in repls {
                    let to_sql = self.gen_expr(&crate::expression::Expression::Literal((*to_lit).clone()))?;
                    let when = if matches!(from_lit.value, crate::expression::LiteralValue::Null) {
                        format!("WHEN {qname} IS NULL THEN {to_sql}")
                    } else {
                        let from_sql = self.gen_expr(&crate::expression::Expression::Literal((*from_lit).clone()))?;
                        format!("WHEN {qname} = {from_sql} THEN {to_sql}")
                    };
                    when_clauses.push(when);
                }
                select_parts.push(format!("CASE {} ELSE {qname} END AS {qname}", when_clauses.join(" ")));
            } else {
                select_parts.push(qname);
            }
        }
        Ok(format!("SELECT {}\nFROM {from}", select_parts.join(", ")))
    }

    fn gen_unpivot(&self, u: &Unpivot) -> Result<String> {
        let var_col = quote_ident(&u.variable_column_name);
        let val_col = quote_ident(&u.value_column_name);
        let include = if u.include_nulls { " INCLUDE NULLS" } else { "" };

        // Build the ON column list
        let value_cols = u.values.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ");

        // Pre-select only id + value columns so DuckDB doesn't include extra columns
        // (DuckDB keeps all non-ON columns as ids, but Spark only keeps the explicit ids).
        if !u.ids.is_empty() || !u.values.is_empty() {
            let select_cols: Vec<String> = u.ids.iter().chain(u.values.iter())
                .map(|c| quote_ident(c))
                .collect();
            let select_list = select_cols.join(", ");
            let from = self.gen_from(&u.input)?;
            Ok(format!(
                "UNPIVOT{include} (\nSELECT {select_list}\nFROM {from}\n)\nON {value_cols}\nINTO NAME {var_col} VALUE {val_col}"
            ))
        } else {
            let inner = self.gen_plan(&u.input)?;
            Ok(format!(
                "UNPIVOT{include} (\n{inner}\n)\nON {value_cols}\nINTO NAME {var_col} VALUE {val_col}"
            ))
        }
    }

    fn gen_pivot(&self, p: &Pivot) -> Result<String> {
        // Generate input: table scan by name, or subquery in parens.
        let inner = match p.input.as_ref() {
            LogicalPlan::TableScan(ts) => self.gen_table_scan(ts)?,
            other => format!("(\n{}\n)", self.gen_plan(other)?),
        };
        let pivot_col = self.gen_expr(&p.pivot_col)?;

        // IN clause (explicit pivot values)
        let in_clause = if !p.pivot_values.is_empty() {
            let vals = p.pivot_values.iter()
                .map(|v| self.gen_expr(v))
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            format!(" IN ({vals})")
        } else {
            String::new()
        };

        // USING clause (aggregate functions)
        let using = p.aggregates.iter()
            .map(|ae| self.gen_expr(&ae.func))
            .collect::<Result<Vec<_>>>()?
            .join(", ");

        // GROUP BY clause
        let group_by = if !p.grouping.is_empty() {
            let cols = p.grouping.iter()
                .map(|g| self.gen_expr(g))
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            format!("\nGROUP BY {cols}")
        } else {
            String::new()
        };

        Ok(format!(
            "SELECT * FROM (PIVOT {inner}\nON {pivot_col}{in_clause}\nUSING {using}{group_by}) \"__pivot__\""
        ))
    }

    fn gen_stat_cov(&self, s: &StatCov) -> Result<String> {
        let input = self.gen_from(&s.input)?;
        let col1 = format!("\"{}\"", s.col1.replace('"', "\"\""));
        let col2 = format!("\"{}\"", s.col2.replace('"', "\"\""));
        // Spark's stat.cov treats NULL as 0.0 in its streaming algorithm
        // (rows where one or both columns are null contribute with 0 for that column).
        // Match Spark by coalescing NULLs to 0.
        Ok(format!("SELECT COVAR_SAMP(COALESCE({col1}, 0.0::DOUBLE), COALESCE({col2}, 0.0::DOUBLE)) FROM {input}"))
    }

    fn gen_stat_corr(&self, s: &StatCorr) -> Result<String> {
        let input = self.gen_from(&s.input)?;
        let col1 = format!("\"{}\"", s.col1.replace('"', "\"\""));
        let col2 = format!("\"{}\"", s.col2.replace('"', "\"\""));
        // Spark's stat.corr also treats NULL as 0.0 (same streaming algorithm as cov).
        Ok(format!("SELECT CORR(COALESCE({col1}, 0.0::DOUBLE), COALESCE({col2}, 0.0::DOUBLE)) FROM {input}"))
    }

    fn gen_approx_quantile(&self, aq: &ApproxQuantile) -> Result<String> {
        let input = self.gen_from(&aq.input)?;
        // Build one SELECT per column, UNION ALL'd together.
        // We collect quantile scalars into a DuckDB list using a subquery + list() aggregate.
        // This guarantees a LIST<DOUBLE> return type (PySpark client expects list-per-column).
        let selects: Vec<String> = aq
            .cols
            .iter()
            .map(|col| {
                let quoted = format!("\"{}\"", col.replace('"', "\"\""));
                // Build UNION ALL of (ord, approx_quantile) pairs, then aggregate into a list.
                let union_arms: Vec<String> = aq
                    .probabilities
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        format!(
                            "SELECT {i} AS __ord, approx_quantile({quoted}, {p:.17}) AS __q FROM {input}"
                        )
                    })
                    .collect();
                let union_sql = union_arms.join("\nUNION ALL\n");
                format!(
                    "SELECT list(__q ORDER BY __ord) AS quantiles FROM ({union_sql}) __prob_rows__"
                )
            })
            .collect();
        Ok(selects.join("\nUNION ALL\n"))
    }

    fn gen_stat_crosstab(&self, s: &StatCrosstab) -> Result<String> {
        let from = self.gen_from(&s.input)?;
        let col1 = quote_ident(&s.col1);
        let col2 = quote_ident(&s.col2);
        let combined = quote_ident(&format!("{}_{}", s.col1, s.col2));
        Ok(format!(
            "SELECT c1 AS {combined}, * EXCLUDE (c1)\n\
             FROM (\n  \
               PIVOT (\n    \
                 SELECT CAST({col1} AS VARCHAR) AS c1, CAST({col2} AS VARCHAR) AS c2\n    \
                 FROM {from}\n  \
               ) ON c2 USING COUNT(*) GROUP BY c1\n\
             ) _crosstab\n\
             ORDER BY c1"
        ))
    }

    fn gen_stat_freq_items(&self, s: &StatFreqItems) -> Result<String> {
        let from = self.gen_from(&s.input)?;
        let support = s.support;
        let subqueries: Vec<String> = s.cols.iter().map(|col| {
            let qcol = quote_ident(col);
            let qalias = quote_ident(&format!("{}_freqItems", col));
            format!(
                "(SELECT LIST({qcol} ORDER BY {qcol}) FROM (\n  \
                   SELECT {qcol}, COUNT(*) AS cnt FROM _stat_input AS _inner\n  \
                   WHERE {qcol} IS NOT NULL GROUP BY {qcol}\n  \
                   HAVING COUNT(*) >= {support} * (SELECT COUNT(*) FROM _stat_input AS _total)\n\
                 ) AS _freq) AS {qalias}"
            )
        }).collect();
        Ok(format!(
            "WITH _stat_input AS (\nSELECT * FROM {from}\n)\nSELECT {}",
            subqueries.join(",\n")
        ))
    }

    fn gen_stat_sample_by(&self, s: &StatSampleBy) -> Result<String> {
        let from = self.gen_from(&s.input)?;
        let col_sql = self.gen_expr(&s.col_expr)?;

        if s.fractions.is_empty() {
            return Ok(format!("SELECT * FROM {from} AS _stat_input WHERE FALSE"));
        }

        let conditions: Result<Vec<String>> = s.fractions.iter().map(|(lit, frac)| {
            let lit_sql = self.gen_expr(&crate::expression::Expression::Literal(lit.clone()))?;
            Ok(format!("({col_sql} = {lit_sql} AND RANDOM() < {frac})"))
        }).collect();
        let where_body = conditions?.join(" OR ");

        let where_clause = if let Some(seed) = s.seed {
            let seed_f = (seed.rem_euclid(1_000_000) as f64) / 1_000_000.0;
            format!("(SELECT setseed({seed_f:.6})) IS NULL AND ({where_body})")
        } else {
            where_body
        };

        Ok(format!("SELECT * FROM {from} AS _stat_input WHERE {where_clause}"))
    }

    fn gen_describe(&self, d: &Describe) -> Result<String> {
        let input = self.gen_from(&d.input)?;
        let stats = ["count", "mean", "stddev", "min", "max"];
        let stats_owned: Vec<String> = stats.iter().map(|s| s.to_string()).collect();
        self.gen_stats_union(&d.cols, &stats_owned, &input)
    }

    fn gen_summary(&self, s: &Summary) -> Result<String> {
        let input = self.gen_from(&s.input)?;
        let default_stats: Vec<String> = ["count", "mean", "stddev", "min", "25%", "50%", "75%", "max"]
            .iter().map(|s| s.to_string()).collect();
        let stats = if s.statistics.is_empty() { &default_stats } else { &s.statistics };
        self.gen_stats_union(&s.cols, stats, &input)
    }

    /// Build a UNION ALL of one SELECT row per statistic.
    /// Uses a CTE so the input is scanned once and each arm can reference it.
    fn gen_stats_union(&self, cols: &[String], stats: &[String], input: &str) -> Result<String> {
        let rows: Vec<String> = stats
            .iter()
            .map(|stat| {
                let col_exprs: Vec<String> = cols
                    .iter()
                    .map(|col| {
                        let q = quote_col(col);
                        format!("{} AS {q}", stat_to_agg_expr(stat, &q))
                    })
                    .collect();
                let summary_lit = format!("'{}'", stat.replace('\'', "''"));
                let cols_sql = if col_exprs.is_empty() {
                    String::new()
                } else {
                    format!(", {}", col_exprs.join(", "))
                };
                format!("SELECT {summary_lit} AS summary{cols_sql} FROM __stats_input__")
            })
            .collect();
        Ok(format!(
            "WITH __stats_input__ AS {input}\n{}",
            rows.join("\nUNION ALL\n")
        ))
    }

    // ── FROM clause helpers ────────────────────────────────────────────────────

    /// Peel a stack of Filter nodes off the top of `plan`.
    /// Returns the underlying base plan and the collected filter conditions (outermost first).
    /// This allows callers to inline the WHERE clause so join table aliases remain in scope.
    fn extract_filters(plan: &LogicalPlan) -> (&LogicalPlan, Vec<&Expression>) {
        let mut conditions: Vec<&Expression> = Vec::new();
        let mut cur = plan;
        while let LogicalPlan::Filter(f) = cur {
            conditions.push(&f.condition);
            cur = &f.input;
        }
        (cur, conditions)
    }

    /// Generate a FROM-clause fragment: either bare table/subquery or wrapped subquery.
    fn gen_from(&self, plan: &LogicalPlan) -> Result<String> {
        match plan {
            // These are already valid FROM targets without wrapping
            LogicalPlan::TableScan(ts) => self.gen_table_scan(ts),
            LogicalPlan::SqlRelation(sr) => Ok(format!("({})", sr.sql)),
            LogicalPlan::InMemoryRelation(imr) => self.gen_in_memory_relation(imr),
            LogicalPlan::RangeRelation(rr) => self.gen_range_relation(rr),
            LogicalPlan::LocalRelation(lr) => self.gen_local_relation(lr),
            LogicalPlan::LocalDataRelation(ldr) => self.gen_local_data_relation(ldr),
            LogicalPlan::AliasedRelation(ar) => {
                // Inner might be a raw relation or a subquery.
                // Relations that already produce parenthesised SQL (gen_local_relation etc.)
                // must NOT go through the `other` arm which would double-wrap them.
                let inner = match ar.input.as_ref() {
                    LogicalPlan::TableScan(ts) => self.gen_table_scan(ts)?,
                    LogicalPlan::InMemoryRelation(imr) => self.gen_in_memory_relation(imr)?,
                    LogicalPlan::LocalRelation(lr) => self.gen_local_relation(lr)?,
                    LogicalPlan::LocalDataRelation(ldr) => self.gen_local_data_relation(ldr)?,
                    LogicalPlan::SqlRelation(sr) => self.gen_sql_relation(sr)?,
                    other => format!("(\n{}\n)", self.gen_plan(other)?),
                };
                let alias = quote_ident(&ar.alias);
                if ar.column_aliases.is_empty() {
                    Ok(format!("{inner} AS {alias}"))
                } else {
                    let cols = ar.column_aliases.iter()
                        .map(|c| quote_ident(c))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Ok(format!("{inner} AS {alias}({cols})"))
                }
            }
            LogicalPlan::Join(j) => {
                if !j.using_columns.is_empty() {
                    // USING join: wrap in a subquery with SELECT key, * EXCLUDE (key)
                    // so callers (Sort, Limit, Filter, etc.) see key column first (Spark convention).
                    let select = gen_using_join_select(&j.using_columns);
                    let chain = self.gen_join(j)?;
                    Ok(format!("(\n{select}\nFROM {chain}\n)"))
                } else {
                    self.gen_join(j)
                }
            }
            // All other plans become subqueries
            other => Ok(format!("(\n{}\n)", self.gen_plan(other)?)),
        }
    }

    // ── Projection list ────────────────────────────────────────────────────────

    fn gen_projection_list(&self, exprs: &[Expression]) -> Result<String> {
        if exprs.is_empty() {
            return Ok("*".to_string());
        }
        exprs.iter()
            .map(|e| {
                // Strict mode: wrap computed DECIMAL projections with explicit CAST so the
                // output column type matches Spark exactly (mirrors Java generateExpressionWithCast).
                if self.mode == CompatMode::Strict {
                    if let Some(sql) = self.try_strict_decimal_cast(e)? {
                        return Ok(sql);
                    }
                }
                let sql = self.gen_expr(e)?;
                // DuckDB normalises DECIMAL(p,s) to DECIMAL(p, s) (adds space) in auto-generated
                // column names. Spark uses no-space formatting. Add explicit Spark-compatible
                // aliases for any non-trivial expression that contains a CAST to DECIMAL so the
                // output column names match what Spark would produce.
                if !matches!(e,
                    Expression::Alias(_) | Expression::ColumnReference(_)
                    | Expression::UnresolvedColumn(_) | Expression::Star(_))
                    && expr_contains_decimal_cast(e)
                {
                    let alias = spark_column_name(e);
                    let escaped = alias.replace('"', "\"\"");
                    Ok(format!("{sql} AS \"{escaped}\""))
                } else {
                    Ok(sql)
                }
            })
            .collect::<Result<Vec<_>>>()
            .map(|v| v.join(", "))
    }

    /// In strict mode, wraps a computed projection expression with `CAST(... AS DECIMAL(p,s))`
    /// when the schema-resolved type is DECIMAL but the expression's intrinsic type is not.
    ///
    /// This matches the Java reference `generateExpressionWithCast` called from `visitWithColumns`.
    /// The mismatch arises on the DataFrame API path where column operands are `UnresolvedColumn`
    /// (type unknown until schema lookup), so arithmetic like `a / b` would otherwise produce
    /// DOUBLE in DuckDB even when `a` and `b` are DECIMAL columns.
    ///
    /// Returns `None` when no wrapping is needed (caller falls through to default logic).
    fn try_strict_decimal_cast(&self, e: &Expression) -> Result<Option<String>> {
        // Strip Alias wrapper to inspect the actual computed expression.
        let (inner_expr, alias_opt): (&Expression, Option<&str>) = match e {
            Expression::Alias(a) => (a.expr.as_ref(), Some(a.alias.as_str())),
            other => (other, None),
        };

        // Skip simple passthroughs — CAST is only meaningful for computed expressions.
        if matches!(
            inner_expr,
            Expression::ColumnReference(_)
                | Expression::UnresolvedColumn(_)
                | Expression::Cast(_)
                | Expression::Literal(_)
                | Expression::Star(_)
        ) {
            return Ok(None);
        }

        // Intrinsic type: what the expression resolves to without any schema context.
        // Schema type: what it resolves to once column types are known from the input plan.
        let intrinsic_type = inner_expr.data_type(&StructType::empty());
        let schema_type = inner_expr.data_type(&self.schema);

        if let DataType::Decimal { precision, scale } = schema_type {
            if !matches!(intrinsic_type, DataType::Decimal { .. }) {
                let inner_sql = self.gen_expr(inner_expr)?;
                let cast_sql = format!("CAST({inner_sql} AS DECIMAL({precision}, {scale}))");
                let result = match alias_opt {
                    Some(alias) => {
                        let escaped = alias.replace('"', "\"\"");
                        format!("{cast_sql} AS \"{escaped}\"")
                    }
                    None => {
                        // Bare computed expression — add Spark-compatible alias.
                        let alias = spark_column_name(e);
                        let escaped = alias.replace('"', "\"\"");
                        format!("{cast_sql} AS \"{escaped}\"")
                    }
                };
                return Ok(Some(result));
            }
        }

        Ok(None)
    }

    // ── Sort order ─────────────────────────────────────────────────────────────

    fn gen_sort_order(&self, o: &SortOrder) -> Result<String> {
        use crate::expression::{NullOrdering, SortDirection};
        let expr = self.gen_expr(&o.expr)?;
        let dir = match o.direction {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        };
        let nulls = match o.null_ordering {
            NullOrdering::NullsFirst => "NULLS FIRST",
            NullOrdering::NullsLast => "NULLS LAST",
        };
        Ok(format!("{expr} {dir} {nulls}"))
    }
}

// ── Expression generation ──────────────────────────────────────────────────────

impl SqlGenerator {
    /// Generate SQL for an expression. This is the main entry point.
    pub fn gen_expr(&self, expr: &Expression) -> Result<String> {
        match expr {
            Expression::Literal(l) => gen_literal(&l.value),
            Expression::ColumnReference(c) => {
                if let Some(q) = &c.qualifier {
                    Ok(format!("{}.{}", quote_ident(q), quote_ident(&c.name)))
                } else {
                    Ok(quote_ident(&c.name))
                }
            }
            Expression::UnresolvedColumn(u) => {
                if let Some(q) = &u.qualifier {
                    // Plan_id qualifiers (e.g. "__plan_id_10__") are internal join markers;
                    // they are replaced by real table aliases in qualify_join_condition.
                    // If one survives to here (non-join context), drop it and use col name only.
                    if q.starts_with("__plan_id_") && q.ends_with("__") {
                        Ok(quote_ident(&u.name))
                    } else if u.name.contains('.') {
                        // Multi-level struct access: "qualifier"."part1"."part2"
                        // e.g. col("person.address.city") → qualifier="person", name="address.city"
                        let parts: String = u.name.split('.').map(quote_ident).collect::<Vec<_>>().join(".");
                        Ok(format!("{}.{}", quote_ident(q), parts))
                    } else {
                        Ok(format!("{}.{}", quote_ident(q), quote_ident(&u.name)))
                    }
                } else if u.name.contains('.') {
                    // Unqualified multi-part reference (e.g. "a.b.c" without a leading qualifier)
                    Ok(u.name.split('.').map(quote_ident).collect::<Vec<_>>().join("."))
                } else {
                    Ok(quote_ident(&u.name))
                }
            }
            Expression::Binary(b) => self.gen_binary(b),
            Expression::Unary(u) => self.gen_unary(u),
            Expression::FunctionCall(f) => self.gen_function_call(f),
            Expression::Cast(c) => self.gen_cast(c),
            Expression::CaseWhen(cw) => self.gen_case_when(cw),
            Expression::Window(w) => self.gen_window(w),
            Expression::Alias(a) => self.gen_alias(a),
            Expression::Star(s) => match &s.qualifier {
                Some(q) => Ok(format!("{}.*", quote_ident(q))),
                None => Ok("*".to_string()),
            },
            Expression::InSubquery(i) => self.gen_in_subquery(i),
            Expression::ExistsSubquery(e) => self.gen_exists_subquery(e),
            Expression::ScalarSubquery(s) => self.gen_scalar_subquery(s),
            Expression::Lambda(l) => self.gen_lambda(l),
            Expression::LambdaVariable(lv) => Ok(quote_ident(&lv.name)),
            Expression::RawSql(r) => Ok(preprocess_spark_sql(&r.sql)),
            Expression::ArrayLiteral(a) => self.gen_array_literal(a),
            Expression::MapLiteral(m) => self.gen_map_literal(m),
            Expression::StructLiteral(s) => self.gen_struct_literal(s),
            Expression::Between(b) => self.gen_between(b),
            Expression::InList(il) => self.gen_in_list(il),
            // New expression variants
            Expression::Like(l) => self.gen_like(l),
            Expression::Interval(i) => self.gen_interval(i),
            Expression::IsDistinctFrom(idf) => self.gen_is_distinct_from(idf),
            Expression::ExtractValue(ev) => self.gen_extract_value(ev),
            Expression::RowConstructor(rc) => self.gen_row_constructor(rc),
            Expression::UpdateFields(uf) => self.gen_update_fields(uf),
        }
    }

    fn gen_binary(&self, b: &BinaryExpression) -> Result<String> {
        let left = self.gen_expr_paren(&b.left, b.op.precedence())?;
        let right = self.gen_expr_paren(&b.right, b.op.precedence())?;
        let sql = format!("{left} {} {right}", b.op.symbol());
        // DATE + INTERVAL → DuckDB promotes to TIMESTAMP; cast back to DATE to match Spark semantics
        if matches!(b.op, BinaryOp::Add | BinaryOp::Sub)
            && b.left.data_type(&self.schema) == DataType::Date
            && (matches!(&*b.right, Expression::Interval(_))
                || right.trim_start().to_ascii_uppercase().starts_with("INTERVAL"))
        {
            return Ok(format!("CAST({sql} AS DATE)"));
        }
        Ok(sql)
    }

    /// Generate an expression, wrapping in parens if its precedence is lower than `parent_prec`.
    fn gen_expr_paren(&self, expr: &Expression, parent_prec: u8) -> Result<String> {
        let s = self.gen_expr(expr)?;
        // Binary expressions may need parentheses
        if let Expression::Binary(b) = expr {
            if b.op.precedence() < parent_prec {
                return Ok(format!("({s})"));
            }
        }
        Ok(s)
    }

    fn gen_unary(&self, u: &UnaryExpression) -> Result<String> {
        let operand = self.gen_expr(&u.operand)?;
        match &u.op {
            UnaryOp::Not => Ok(format!("NOT ({operand})")),
            UnaryOp::Negate => Ok(format!("-({operand})")),
            UnaryOp::IsNull => Ok(format!("({operand}) IS NULL")),
            UnaryOp::IsNotNull => Ok(format!("({operand}) IS NOT NULL")),
            UnaryOp::IsNaN => Ok(format!("isnan({operand})")),
            UnaryOp::IsNotNaN => Ok(format!("NOT isnan({operand})")),
        }
    }

    fn gen_function_call(&self, f: &FunctionCall) -> Result<String> {
        // SQL operator pseudo-functions: Spark sends these as UnresolvedFunction but
        // DuckDB requires them as SQL operators (no scalar function of these names exists).
        match f.name.to_ascii_lowercase().as_str() {
            "like" if f.args.len() == 2 => {
                let left = self.gen_expr(&f.args[0])?;
                let right = self.gen_expr(&f.args[1])?;
                return Ok(format!("{left} LIKE {right}"));
            }
            "ilike" if f.args.len() == 2 => {
                let left = self.gen_expr(&f.args[0])?;
                let right = self.gen_expr(&f.args[1])?;
                return Ok(format!("{left} ILIKE {right}"));
            }
            "rlike" if f.args.len() == 2 => {
                let left = self.gen_expr(&f.args[0])?;
                let right = self.gen_expr(&f.args[1])?;
                return Ok(format!("regexp_matches({left}, {right})"));
            }
            "in" if f.args.len() >= 2 => {
                let expr = self.gen_expr(&f.args[0])?;
                let list = f.args[1..].iter()
                    .map(|a| self.gen_expr(a))
                    .collect::<Result<Vec<_>>>()?
                    .join(", ");
                return Ok(format!("{expr} IN ({list})"));
            }
            // grouping() returns INTEGER in DuckDB but TINYINT in Spark.
            "grouping" => {
                let args = f.args.iter()
                    .map(|a| self.gen_expr(a))
                    .collect::<Result<Vec<_>>>()?
                    .join(", ");
                return Ok(format!("CAST(grouping({args}) AS TINYINT)"));
            }
            // grouping_id() returns INTEGER in DuckDB but BIGINT in Spark.
            "grouping_id" => {
                let args = f.args.iter()
                    .map(|a| self.gen_expr(a))
                    .collect::<Result<Vec<_>>>()?
                    .join(", ");
                return Ok(format!("CAST(grouping_id({args}) AS BIGINT)"));
            }
            _ => {}
        }

        let arg_sqls: Vec<String> = f.args.iter()
            .map(|a| self.gen_expr(a))
            .collect::<Result<Vec<_>>>()?;

        let arg_refs: Vec<&str> = arg_sqls.iter().map(|s| s.as_str()).collect();

        // Infer argument types for polymorphic dispatch using the current input schema.
        // When schema is populated (e.g. inside a Project), column references resolve to
        // their actual types (e.g. arr1 → Array(Integer)), enabling correct dispatch for
        // functions like reverse(). When schema is empty, unresolved columns fall back to
        // DataType::Unresolved and translate_typed falls through to the non-polymorphic path.
        let arg_types: Vec<DataType> =
            f.args.iter().map(|a| a.data_type(&self.schema)).collect();

        // Route through FunctionRegistry for Spark→DuckDB translation
        let translated =
            FunctionRegistry::translate_typed(&f.name, &arg_refs, &arg_types, self.mode);

        if f.distinct {
            // DuckDB's COUNT(DISTINCT ...) only accepts a single expression.
            // For multi-column count distinct, wrap all columns in a struct so each
            // unique combination of values is treated as one distinct value.
            if f.name.eq_ignore_ascii_case("count") && arg_sqls.len() > 1 {
                let fields: String = arg_sqls.iter().enumerate()
                    .map(|(i, a)| format!("'f{i}': {a}"))
                    .collect::<Vec<_>>().join(", ");
                return Ok(format!("COUNT(DISTINCT {{{fields}}})"));
            }
            // Inject DISTINCT inside the outermost function call
            Ok(inject_distinct(translated))
        } else {
            Ok(translated)
        }
    }

    fn gen_cast(&self, c: &CastExpression) -> Result<String> {
        let expr = self.gen_expr(&c.expr)?;
        let ty = TypeMapper::to_duckdb(&c.to_type);
        // Spark truncates toward zero when casting float/double → integer.
        // DuckDB rounds, so we must wrap with TRUNCATE for float sources.
        let is_integer_target = matches!(
            c.to_type,
            DataType::Integer | DataType::Long | DataType::Short | DataType::Byte
        );
        let src_type = c.expr.data_type(&self.schema);
        let is_float_source = matches!(src_type, DataType::Double | DataType::Float);
        if c.try_cast {
            Ok(format!("TRY_CAST({expr} AS {ty})"))
        } else if is_integer_target && is_float_source {
            Ok(format!("CAST(trunc({expr}) AS {ty})"))
        } else {
            Ok(format!("CAST({expr} AS {ty})"))
        }
    }

    fn gen_case_when(&self, cw: &CaseWhenExpression) -> Result<String> {
        let mut s = "CASE".to_string();
        if let Some(base) = &cw.base {
            s.push(' ');
            s.push_str(&self.gen_expr(base)?);
        }
        for (cond, result) in &cw.branches {
            let c = self.gen_expr(cond)?;
            let r = self.gen_expr(result)?;
            s.push_str(&format!(" WHEN {c} THEN {r}"));
        }
        if let Some(else_e) = &cw.else_expr {
            s.push_str(&format!(" ELSE {}", self.gen_expr(else_e)?));
        }
        s.push_str(" END");
        Ok(s)
    }

    fn gen_window(&self, w: &WindowFunction) -> Result<String> {
        // Handle first/last/nth_value with ignore_nulls argument.
        // Spark: first(col, ignorenulls=True) → DuckDB: first_value(col IGNORE NULLS)
        let func = if let Expression::FunctionCall(f) = &*w.func {
            let lower = f.name.to_lowercase();
            let (duckdb_name, ignore_nulls_idx) = match lower.as_str() {
                "first" => ("first_value", Some(1usize)),
                "last" => ("last_value", Some(1usize)),
                "nth_value" => ("nth_value", Some(2usize)),
                _ => ("", None),
            };
            if let Some(nulls_idx) = ignore_nulls_idx {
                let ignore_nulls = f.args.get(nulls_idx)
                    .map(|a| matches!(a, Expression::Literal(l) if l.value == crate::expression::LiteralValue::Boolean(true)))
                    .unwrap_or(false);
                let col_args: Vec<String> = f.args.iter()
                    .take(nulls_idx)
                    .map(|a| self.gen_expr(a))
                    .collect::<Result<Vec<_>>>()?;
                let col_str = col_args.join(", ");
                if ignore_nulls {
                    format!("{duckdb_name}({col_str} IGNORE NULLS)")
                } else {
                    format!("{duckdb_name}({col_str})")
                }
            } else {
                self.gen_expr(&w.func)?
            }
        } else {
            self.gen_expr(&w.func)?
        };

        let mut over_parts = Vec::new();

        if !w.partition_by.is_empty() {
            let pb = w.partition_by.iter()
                .map(|e| self.gen_expr(e))
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            over_parts.push(format!("PARTITION BY {pb}"));
        }

        if !w.order_by.is_empty() {
            let ob = w.order_by.iter()
                .map(|o| self.gen_sort_order(o))
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            over_parts.push(format!("ORDER BY {ob}"));
        }

        if let Some(frame) = &w.frame {
            over_parts.push(self.gen_window_frame(frame)?);
        }

        let over = over_parts.join(" ");
        Ok(format!("{func} OVER ({over})"))
    }

    fn gen_window_frame(&self, frame: &crate::expression::WindowFrame) -> Result<String> {
        let unit = match frame.unit {
            FrameUnit::Rows => "ROWS",
            FrameUnit::Range => "RANGE",
        };
        let start = self.gen_frame_boundary(&frame.start)?;
        let end = self.gen_frame_boundary(&frame.end)?;
        Ok(format!("{unit} BETWEEN {start} AND {end}"))
    }

    fn gen_frame_boundary(&self, b: &FrameBoundary) -> Result<String> {
        match b {
            FrameBoundary::UnboundedPreceding => Ok("UNBOUNDED PRECEDING".to_string()),
            FrameBoundary::Preceding(e) => {
                let s = self.gen_expr(e)?;
                Ok(format!("{s} PRECEDING"))
            }
            FrameBoundary::CurrentRow => Ok("CURRENT ROW".to_string()),
            FrameBoundary::Following(e) => {
                let s = self.gen_expr(e)?;
                Ok(format!("{s} FOLLOWING"))
            }
            FrameBoundary::UnboundedFollowing => Ok("UNBOUNDED FOLLOWING".to_string()),
        }
    }

    fn gen_alias(&self, a: &AliasExpression) -> Result<String> {
        let expr = self.gen_expr(&a.expr)?;
        Ok(format!("{expr} AS {}", quote_ident(&a.alias)))
    }

    fn gen_in_subquery(&self, i: &InSubquery) -> Result<String> {
        let expr = self.gen_expr(&i.expr)?;
        let sub = self.gen_plan(&i.subquery)?;
        let not = if i.negated { "NOT " } else { "" };
        Ok(format!("{expr} {not}IN (\n{sub}\n)"))
    }

    fn gen_exists_subquery(&self, e: &ExistsSubquery) -> Result<String> {
        let sub = self.gen_plan(&e.subquery)?;
        let not = if e.negated { "NOT " } else { "" };
        Ok(format!("{not}EXISTS (\n{sub}\n)"))
    }

    fn gen_scalar_subquery(&self, s: &ScalarSubquery) -> Result<String> {
        let sub = self.gen_plan(&s.subquery)?;
        Ok(format!("(\n{sub}\n)"))
    }

    fn gen_lambda(&self, l: &LambdaExpression) -> Result<String> {
        let params = if l.params.len() == 1 {
            quote_ident(&l.params[0])
        } else {
            let ps = l.params.iter().map(|p| quote_ident(p)).collect::<Vec<_>>().join(", ");
            format!("({ps})")
        };
        let body = self.gen_expr(&l.body)?;
        Ok(format!("{params} -> {body}"))
    }

    fn gen_array_literal(&self, a: &ArrayLiteralExpression) -> Result<String> {
        let elems = a.elements.iter()
            .map(|e| self.gen_expr(e))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        Ok(format!("[{elems}]"))
    }

    fn gen_map_literal(&self, m: &MapLiteralExpression) -> Result<String> {
        // DuckDB MAP syntax: MAP {key1: val1, key2: val2}
        // or MAP(KEYS => [...], VALUES => [...])
        // Simplest: MAP(list_of_keys, list_of_values)
        let keys = m.keys.iter()
            .map(|k| self.gen_expr(k))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        let vals = m.values.iter()
            .map(|v| self.gen_expr(v))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        Ok(format!("MAP([{keys}], [{vals}])"))
    }

    fn gen_struct_literal(&self, s: &StructLiteralExpression) -> Result<String> {
        // DuckDB struct literal: {field1: expr1, field2: expr2}
        let fields = s.fields.iter()
            .map(|(name, expr)| {
                let e = self.gen_expr(expr)?;
                Ok(format!("{name}: {e}"))
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        Ok(format!("{{{fields}}}"))
    }

    fn gen_between(&self, b: &BetweenExpression) -> Result<String> {
        let expr = self.gen_expr(&b.expr)?;
        let low = self.gen_expr(&b.low)?;
        let high = self.gen_expr(&b.high)?;
        let not = if b.negated { "NOT " } else { "" };
        Ok(format!("{expr} {not}BETWEEN {low} AND {high}"))
    }

    fn gen_in_list(&self, il: &InListExpression) -> Result<String> {
        let expr = self.gen_expr(&il.expr)?;
        let list = il.list.iter()
            .map(|e| self.gen_expr(e))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        let not = if il.negated { "NOT " } else { "" };
        Ok(format!("{expr} {not}IN ({list})"))
    }

    /// Generate SQL for a LIKE / NOT LIKE / ILIKE / NOT ILIKE expression.
    ///
    /// DuckDB supports ILIKE natively for case-insensitive matching.
    fn gen_like(&self, l: &LikeExpression) -> Result<String> {
        let value = self.gen_expr(&l.value)?;
        let pattern = self.gen_expr(&l.pattern)?;
        let not = if l.negated { "NOT " } else { "" };
        let op = if l.case_insensitive { "ILIKE" } else { "LIKE" };
        Ok(format!("({value} {not}{op} {pattern})"))
    }

    /// Generate SQL for an interval literal.
    ///
    /// Ported from `IntervalExpression.toSQL()` in the Java reference.
    /// Handles year-month, day-time (microsecond decomposition), and calendar intervals.
    fn gen_interval(&self, i: &IntervalExpression) -> Result<String> {
        const MICROS_PER_SECOND: i64 = 1_000_000;
        const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
        const MICROS_PER_HOUR: i64 = 60 * MICROS_PER_MINUTE;
        const MICROS_PER_DAY: i64 = 24 * MICROS_PER_HOUR;

        // Determine which type of interval this is
        let has_months = i.months != 0;
        let has_days = i.days != 0;
        let has_micros = i.microseconds != 0;

        if has_months && !has_days && !has_micros {
            // Pure year-month interval
            return Ok(format!("INTERVAL '{}' MONTH", i.months));
        }

        if !has_months && has_days && !has_micros {
            // Pure day interval
            return Ok(format!("INTERVAL '{}' DAY", i.days));
        }

        if !has_months && !has_days {
            // Day-time interval: decompose microseconds
            let negative = i.microseconds < 0;
            let abs_micros = i.microseconds.unsigned_abs() as i64;

            let days_part = abs_micros / MICROS_PER_DAY;
            let remaining = abs_micros % MICROS_PER_DAY;
            let hours_part = remaining / MICROS_PER_HOUR;
            let remaining = remaining % MICROS_PER_HOUR;
            let minutes_part = remaining / MICROS_PER_MINUTE;
            let remaining = remaining % MICROS_PER_MINUTE;
            let seconds_part = remaining as f64 / MICROS_PER_SECOND as f64;

            let mut parts: Vec<String> = Vec::new();

            if days_part != 0 {
                let sign = if negative { "-" } else { "" };
                parts.push(format!("INTERVAL '{sign}{days_part}' DAY"));
            }
            if hours_part != 0 {
                // Sign goes on the first non-zero component only
                let sign = if negative && parts.is_empty() { "-" } else { "" };
                parts.push(format!("INTERVAL '{sign}{hours_part}' HOUR"));
            }
            if minutes_part != 0 {
                let sign = if negative && parts.is_empty() { "-" } else { "" };
                parts.push(format!("INTERVAL '{sign}{minutes_part}' MINUTE"));
            }
            if seconds_part != 0.0 || parts.is_empty() {
                let sign = if negative && parts.is_empty() { "-" } else { "" };
                parts.push(format!("INTERVAL '{sign}{seconds_part:.6}' SECOND"));
            }

            return Ok(parts.join(" + "));
        }

        // Calendar interval: months + days + microseconds
        let mut parts: Vec<String> = Vec::new();

        if has_months {
            parts.push(format!("INTERVAL '{}' MONTH", i.months));
        }
        if has_days {
            parts.push(format!("INTERVAL '{}' DAY", i.days));
        }
        if has_micros {
            let seconds = i.microseconds as f64 / MICROS_PER_SECOND as f64;
            parts.push(format!("INTERVAL '{seconds:.6}' SECOND"));
        }

        Ok(parts.join(" + "))
    }

    /// Generate SQL for IS [NOT] DISTINCT FROM.
    fn gen_is_distinct_from(&self, idf: &IsDistinctFromExpression) -> Result<String> {
        let left = self.gen_expr(&idf.left)?;
        let right = self.gen_expr(&idf.right)?;
        let not = if idf.negated { "NOT " } else { "" };
        Ok(format!("{left} IS {not}DISTINCT FROM {right}"))
    }

    /// Generate SQL for struct/array/map value extraction.
    ///
    /// Ported from `ExtractValueExpression.toSQL()` in the Java reference.
    /// String literal keys use `child['key']`; all other extractions use `child[expr]`.
    fn gen_extract_value(&self, ev: &ExtractValueExpression) -> Result<String> {
        let child = self.gen_expr(&ev.child)?;
        if let Expression::Literal(lit) = ev.extraction.as_ref() {
            match &lit.value {
                // String key: struct or map field access
                LiteralValue::String(s) => {
                    return Ok(format!("{child}['{}']", s.replace('\'', "''")));
                }
                // Numeric index: Spark is 0-based, DuckDB is 1-based.
                // Adjust by adding 1 for non-negative indices from the DataFrame API.
                LiteralValue::Int(n) => {
                    let idx = if *n >= 0 { n + 1 } else { *n };
                    return Ok(format!("{child}[{idx}]"));
                }
                LiteralValue::Long(n) => {
                    let idx = if *n >= 0 { n + 1 } else { *n };
                    return Ok(format!("{child}[{idx}]"));
                }
                _ => {}
            }
        }
        let extraction = self.gen_expr(&ev.extraction)?;
        Ok(format!("{child}[{extraction}]"))
    }

    /// Generate SQL for a row constructor: `(a, b, c)`.
    fn gen_row_constructor(&self, rc: &RowConstructorExpression) -> Result<String> {
        let fields = rc.fields.iter()
            .map(|e| self.gen_expr(e))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        Ok(format!("({fields})"))
    }

    /// Generate SQL for struct field add/update/drop (Spark `withField` / `dropFields`).
    /// - withField → `struct_insert(struct_expr, field_name := value)`
    /// - dropFields → `struct_pack(...)` (requires schema; unsupported without it)
    fn gen_update_fields(&self, uf: &crate::expression::UpdateFieldsExpression) -> Result<String> {
        let struct_sql = self.gen_expr(&uf.struct_expr)?;
        let field = quote_ident(&uf.field_name);
        match &uf.value {
            Some(val) => {
                let val_sql = self.gen_expr(val)?;
                // DuckDB: struct_insert(struct_expr, field_name := value)
                Ok(format!("struct_insert({struct_sql}, {field} := {val_sql})"))
            }
            None => {
                // dropFields: rebuild struct without the dropped field using struct_pack
                if let Some(fields) = &uf.struct_fields {
                    let kept: Vec<String> = fields
                        .iter()
                        .filter(|f| f.as_str() != uf.field_name.as_str())
                        .map(|f| {
                            let field_ident = quote_ident(f);
                            let key = format!("'{}'", f.replace('\'', "''"));
                            format!("{field_ident} := struct_extract({struct_sql}, {key})")
                        })
                        .collect();
                    Ok(format!("struct_pack({})", kept.join(", ")))
                } else {
                    Err(crate::error::ThunderduckError::Unsupported(
                        format!("dropFields('{}'): schema inference required", uf.field_name)
                    ))
                }
            }
        }
    }
}

// ── Describe/Summary helpers ───────────────────────────────────────────────────

fn quote_col(col: &str) -> String {
    format!("\"{}\"", col.replace('"', "\"\""))
}

/// Map a statistic name to the aggregate SQL expression for a single column.
///
/// Uses `TRY_CAST(col AS DOUBLE)` for numeric-only aggregates so that string
/// columns return NULL rather than erroring (matching Spark's behaviour).
/// Uses `PERCENTILE_DISC` (nearest-rank / discrete) to match Spark's
/// `approx_percentile` semantics.
fn stat_to_agg_expr(stat: &str, quoted_col: &str) -> String {
    match stat {
        "count" => format!("CAST(COUNT({quoted_col}) AS VARCHAR)"),
        "mean" => format!("CAST(AVG(TRY_CAST({quoted_col} AS DOUBLE)) AS VARCHAR)"),
        "stddev" => format!("CAST(STDDEV_SAMP(TRY_CAST({quoted_col} AS DOUBLE)) AS VARCHAR)"),
        "min" => format!("CAST(MIN({quoted_col}) AS VARCHAR)"),
        "max" => format!("CAST(MAX({quoted_col}) AS VARCHAR)"),
        "count_distinct" => format!("CAST(COUNT(DISTINCT {quoted_col}) AS VARCHAR)"),
        "approx_count_distinct" => format!("CAST(APPROX_COUNT_DISTINCT({quoted_col}) AS VARCHAR)"),
        s if s.ends_with('%') => {
            if let Ok(p) = s.trim_end_matches('%').parse::<f64>() {
                let frac = p / 100.0;
                // PERCENTILE_DISC matches Spark's approx_percentile nearest-rank semantics.
                // TRY_CAST so string columns return NULL instead of erroring.
                format!("CAST(PERCENTILE_DISC({frac:.17}) WITHIN GROUP (ORDER BY TRY_CAST({quoted_col} AS DOUBLE)) AS VARCHAR)")
            } else {
                "CAST(NULL AS VARCHAR)".to_string()
            }
        }
        _ => "CAST(NULL AS VARCHAR)".to_string(),
    }
}

// ── Literal generation ─────────────────────────────────────────────────────────

fn gen_literal(v: &LiteralValue) -> Result<String> {
    match v {
        LiteralValue::Null => Ok("NULL".to_string()),
        LiteralValue::Boolean(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_string()),
        LiteralValue::Byte(n) => Ok(format!("CAST({n} AS TINYINT)")),
        LiteralValue::Short(n) => Ok(format!("CAST({n} AS SMALLINT)")),
        LiteralValue::Int(n) => Ok(n.to_string()),
        LiteralValue::Long(n) => Ok(format!("{n}::BIGINT")),
        LiteralValue::Float(f) => {
            if f.is_infinite() {
                if *f > 0.0 {
                    Ok("'Infinity'::FLOAT".to_string())
                } else {
                    Ok("'-Infinity'::FLOAT".to_string())
                }
            } else if f.is_nan() {
                Ok("'NaN'::FLOAT".to_string())
            } else {
                Ok(format!("{f}::FLOAT"))
            }
        }
        LiteralValue::Double(d) => {
            if d.is_infinite() {
                if *d > 0.0 {
                    Ok("'Infinity'::DOUBLE".to_string())
                } else {
                    Ok("'-Infinity'::DOUBLE".to_string())
                }
            } else if d.is_nan() {
                Ok("'NaN'::DOUBLE".to_string())
            } else {
                Ok(format!("{d}::DOUBLE"))
            }
        }
        LiteralValue::Decimal(s) => Ok(format!("{s}::DECIMAL")),
        LiteralValue::String(s) => Ok(format!("'{}'", s.replace('\'', "''"))),
        LiteralValue::Date(days) => {
            // days since epoch → DuckDB date literal
            Ok(format!("(DATE '1970-01-01' + INTERVAL {days} DAY)"))
        }
        LiteralValue::Timestamp(micros) => {
            Ok(format!("(TIMESTAMPTZ '1970-01-01 00:00:00+00' + INTERVAL {micros} MICROSECOND)"))
        }
        LiteralValue::TimestampNtz(micros) => {
            Ok(format!("(TIMESTAMP '1970-01-01 00:00:00' + INTERVAL {micros} MICROSECOND)"))
        }
        LiteralValue::Binary(bytes) => {
            // Hex-encode binary
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            Ok(format!("decode('{hex}')"))
        }
    }
}

// ── Quoting and helpers ────────────────────────────────────────────────────────

/// Double-quote an identifier, escaping any embedded double-quotes.
pub fn quote_ident(name: &str) -> String {
    // If already a simple ASCII alphanumeric + underscore starting with letter/underscore
    // and not a reserved word, we could skip quoting, but for safety always quote.
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Wrap aggregate expressions with CASTs to match Spark's return types:
/// - `SUM(integer)` → `CAST(SUM(...) AS BIGINT)` (DuckDB returns HUGEINT)
/// - `SUM(decimal{p,s})` → `CAST(SUM(...) AS DECIMAL(min(p+10,38), s))`
/// - `AVG(integer)` → `CAST(AVG(...) AS DOUBLE)` (Spark promotes integer AVG to DOUBLE)
/// Returns true if the expression contains a `Cast` to a `Decimal` type anywhere in the tree.
/// Used by `gen_projection_list` to detect when DuckDB would normalise `DECIMAL(p,s)` to
/// `DECIMAL(p, s)` in auto-generated column names.
fn expr_contains_decimal_cast(expr: &Expression) -> bool {
    match expr {
        Expression::Cast(c) => matches!(c.to_type, DataType::Decimal { .. }),
        Expression::Binary(b) => {
            expr_contains_decimal_cast(&b.left) || expr_contains_decimal_cast(&b.right)
        }
        Expression::Unary(u) => expr_contains_decimal_cast(&u.operand),
        Expression::Alias(a) => expr_contains_decimal_cast(&a.expr),
        Expression::FunctionCall(f) => f.args.iter().any(expr_contains_decimal_cast),
        Expression::CaseWhen(cw) => {
            cw.branches.iter().any(|(when, then)| {
                expr_contains_decimal_cast(when) || expr_contains_decimal_cast(then)
            }) || cw.else_expr.as_ref().map_or(false, |e| expr_contains_decimal_cast(e))
        }
        _ => false,
    }
}

/// - `AVG(decimal{p,s})` → `CAST(AVG(...) AS DECIMAL(min(p+4,38), s+4))`
/// Passes through aliases transparently so the alias is preserved on the outer Cast.
fn apply_agg_type_casts(expr: &Expression, input_schema: &StructType) -> Expression {
    match expr {
        Expression::Alias(a) => {
            let inner = apply_agg_type_casts(&a.expr, input_schema);
            Expression::Alias(AliasExpression { expr: Box::new(inner), alias: a.alias.clone() })
        }
        Expression::FunctionCall(f)
            if f.name.eq_ignore_ascii_case("sum") || f.name.eq_ignore_ascii_case("sum_distinct") =>
        {
            if let Some(arg) = f.args.first() {
                let arg_type = arg.data_type(input_schema);
                match arg_type {
                    DataType::Byte | DataType::Short | DataType::Integer | DataType::Long => {
                        return Expression::Cast(CastExpression {
                            expr: Box::new(expr.clone()),
                            to_type: DataType::Long,
                            try_cast: false,
                        });
                    }
                    DataType::Decimal { precision, scale } => {
                        let new_p = ((precision as u16) + 10).min(38) as u8;
                        return Expression::Cast(CastExpression {
                            expr: Box::new(expr.clone()),
                            to_type: DataType::Decimal { precision: new_p, scale },
                            try_cast: false,
                        });
                    }
                    _ => {}
                }
            }
            expr.clone()
        }
        Expression::FunctionCall(f)
            if f.name.eq_ignore_ascii_case("avg") || f.name.eq_ignore_ascii_case("mean") =>
        {
            if let Some(arg) = f.args.first() {
                let arg_type = arg.data_type(input_schema);
                match arg_type {
                    DataType::Byte | DataType::Short | DataType::Integer | DataType::Long => {
                        return Expression::Cast(CastExpression {
                            expr: Box::new(expr.clone()),
                            to_type: DataType::Double,
                            try_cast: false,
                        });
                    }
                    DataType::Decimal { precision, scale } => {
                        let new_p = ((precision as u16) + 4).min(38) as u8;
                        let new_s = scale + 4;
                        return Expression::Cast(CastExpression {
                            expr: Box::new(expr.clone()),
                            to_type: DataType::Decimal { precision: new_p, scale: new_s },
                            try_cast: false,
                        });
                    }
                    _ => {}
                }
            }
            expr.clone()
        }
        _ => expr.clone(),
    }
}

fn inject_distinct(mut s: String) -> String {
    if let Some(pos) = s.find('(') {
        s.insert_str(pos + 1, "DISTINCT ");
    }
    s
}

// ── Precedence extension on BinaryOp ──────────────────────────────────────────

trait BinaryOpExt {
    fn precedence(&self) -> u8;
}

impl BinaryOpExt for BinaryOp {
    fn precedence(&self) -> u8 {
        match self {
            BinaryOp::Or => 1,
            BinaryOp::And => 2,
            BinaryOp::Eq | BinaryOp::NotEq
            | BinaryOp::Lt | BinaryOp::LtEq
            | BinaryOp::Gt | BinaryOp::GtEq => 3,
            BinaryOp::BitwiseOr => 4,
            BinaryOp::BitwiseXor => 5,
            BinaryOp::BitwiseAnd => 6,
            BinaryOp::Add | BinaryOp::Sub => 7,
            BinaryOp::Concat => 7,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 8,
        }
    }
}

// ── Spark SQL Preprocessing ────────────────────────────────────────────────────

/// Convert Spark-style backtick-quoted identifiers to DuckDB double-quoted identifiers.
/// e.g. `count(\`l_orderkey\`)` → `count("l_orderkey")`
/// Correctly skips single-quoted string literals and already double-quoted identifiers.
fn rewrite_backtick_identifiers(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            // Skip single-quoted string literals (don't convert backticks inside them)
            '\'' => {
                result.push('\'');
                loop {
                    match chars.next() {
                        None => break,
                        Some('\'') => {
                            result.push('\'');
                            // Handle escaped quote ''
                            if chars.peek() == Some(&'\'') {
                                chars.next();
                                result.push('\'');
                            } else {
                                break;
                            }
                        }
                        Some(c) => result.push(c),
                    }
                }
            }
            // Pass through already double-quoted identifiers unchanged
            '"' => {
                result.push('"');
                for c in chars.by_ref() {
                    result.push(c);
                    if c == '"' { break; }
                }
            }
            // Convert backtick identifier to double-quoted
            '`' => {
                result.push('"');
                for c in chars.by_ref() {
                    if c == '`' { break; }
                    if c == '"' {
                        result.push('"'); // escape embedded double-quote
                        result.push('"');
                    } else {
                        result.push(c);
                    }
                }
                result.push('"');
            }
            c => result.push(c),
        }
    }
    result
}

/// Preprocess Spark SQL to replace Spark-specific constructs with DuckDB equivalents.
/// Applied to raw SQL strings in SqlRelation before passing to DuckDB.
fn preprocess_spark_sql(sql: &str) -> String {
    // Phase 0: Convert backtick-quoted identifiers to double-quoted
    // DuckDB does not support MySQL-style backtick quoting; Spark SQL uses it freely.
    let sql = rewrite_backtick_identifiers(sql);
    // Phase 1: ARRAY( → LIST_VALUE(
    let mut sql = replace_spark_func(&sql, "ARRAY", "LIST_VALUE");
    // Phase 2: NAMED_STRUCT( → struct literal — loop until stable for nested structs
    loop {
        let new = rewrite_named_struct(&sql);
        if new == sql { break; }
        sql = new;
    }
    // Phase 3: MAP(k1, v1, k2, v2, ...) → MAP([k1, k2, ...], [v1, v2, ...])
    let sql = rewrite_spark_map_constructor(&sql);
    // Phase 4: simple 1:1 function name renames for raw Spark SQL
    let replacements: &[(&str, &str)] = &[
        // COLLECT_LIST/COLLECT_SET handled by DuckDB macro (null-filtering); no rename needed
        ("STARTSWITH",       "STARTS_WITH"),
        ("ENDSWITH",         "ENDS_WITH"),
        ("SIZE",             "LEN"),
        ("GET_JSON_OBJECT",  "JSON_EXTRACT_STRING"),
        ("JSON_OBJECT_KEYS", "json_keys"),
        ("TRANSFORM",        "LIST_TRANSFORM"),
        ("EXPLODE",          "UNNEST"),
        ("POSEXPLODE",       "UNNEST"),    // positional explode — DuckDB UNNEST handles both
    ];
    let mut sql = sql; // already a String from phase 3
    for (from, to) in replacements {
        sql = replace_spark_func(&sql, from, to);
    }
    // Phase 5: percentile(col, pct) → PERCENTILE_CONT(pct) WITHIN GROUP (ORDER BY col)
    let sql = rewrite_percentile(&sql);
    // Phase 6: overlay(col PLACING repl FROM pos [FOR len]) → LEFT/SUBSTRING concat
    let sql = rewrite_overlay_syntax(&sql);
    // Phase 7: Spark angle-bracket type syntax → DuckDB: ARRAY<TYPE> → TYPE[]
    let sql = rewrite_spark_type_syntax(&sql);
    // Phase 8: split(str, pat, n) with 3 args → CASE/STR_SPLIT_REGEX expression
    let sql = rewrite_split_with_limit(&sql);
    // Phase 9: DATE 'lit' + INTERVAL 'n' YEAR/MONTH → CAST(... AS DATE)
    // DuckDB promotes DATE+INTERVAL to TIMESTAMP; Spark keeps DATE type.
    let sql = rewrite_date_interval_to_date(&sql);
    // Phase 10: Spark HOF SQL functions → DuckDB equivalents
    // exists(arr, x -> cond) → (len(list_filter(arr, x -> cond)) > 0)
    let sql = rewrite_hof_func(&sql, "exists", |args| {
        if args.len() >= 2 {
            format!("(len(list_filter({}, {})) > 0)", args[0], args[1])
        } else if args.len() == 1 {
            // EXISTS(subquery) — reconstruct unchanged
            format!("exists({})", args[0])
        } else {
            "FALSE".to_string()
        }
    });
    // forall(arr, x -> cond) → (len(list_filter(arr, x -> cond)) = len(arr))
    let sql = rewrite_hof_func(&sql, "forall", |args| {
        if args.len() >= 2 {
            format!("(len(list_filter({}, {})) = len({}))", args[0], args[1], args[0])
        } else {
            "TRUE".to_string()
        }
    });
    // aggregate(arr, init, merge[, finish]) → list_reduce(list_concat([init], arr), merge)
    let sql = rewrite_hof_func(&sql, "aggregate", |args| {
        match args.len() {
            3 => format!("list_reduce(list_concat([{}], {}), {})", args[1], args[0], args[2]),
            4 => {
                let reduced = format!("list_reduce(list_concat([{}], {}), {})", args[1], args[0], args[2]);
                format!("list_transform([{reduced}], {})[1]", args[3])
            }
            _ => format!("list_reduce({})", args.first().map(|s| s.as_str()).unwrap_or("")),
        }
    });
    // Phase 11a: json_tuple(col, 'k1', 'k2') AS (a1, a2) →
    //   json_extract_string(col, '$.k1') AS a1, json_extract_string(col, '$.k2') AS a2
    let sql = rewrite_json_tuple(&sql);
    // Phase 11b: from_json(col, 'Spark DDL schema') → json_transform(col, '{"field": "TYPE"}')
    // DuckDB has its own from_json (= json_transform) but expects a JSON schema, not Spark DDL.
    let sql = rewrite_hof_func(&sql, "from_json", |args| {
        if args.len() < 2 {
            return format!("from_json({})", args.join(", "));
        }
        let col = &args[0];
        let schema_arg = args[1].trim();
        // The schema arg is a SQL string literal like 'name STRING, age INT'
        let ddl = if schema_arg.starts_with('\'') && schema_arg.ends_with('\'') && schema_arg.len() >= 2 {
            &schema_arg[1..schema_arg.len() - 1]
        } else {
            // Not a plain DDL string (e.g. already a JSON schema or complex expression)
            return format!("json_transform({col}, {schema_arg})");
        };
        let json_schema = spark_ddl_to_json_schema_inline(ddl);
        format!("json_transform({col}, '{json_schema}')")
    });
    sql
}

/// Convert a Spark schema (DDL string or Spark JSON format) to a DuckDB
/// `json_transform` schema JSON (e.g. `{"name": "VARCHAR", "age": "INTEGER"}`).
///
/// Handles two input formats:
/// 1. DDL: `name STRING, age INT`
/// 2. Spark StructType JSON: `{"type":"struct","fields":[{"name":"...", "type":"...", ...}]}`
fn spark_ddl_to_json_schema_inline(ddl: &str) -> String {
    let trimmed = ddl.trim();
    // Detect Spark JSON schema format (starts with '{' and contains "fields")
    if trimmed.starts_with('{') && trimmed.contains("\"fields\"") {
        return spark_json_schema_to_duckdb(trimmed);
    }
    // DDL format: "name STRING, age INT"
    ddl_string_to_duckdb_schema(ddl)
}

/// Parse a Spark DDL schema string to DuckDB json_transform schema.
fn ddl_string_to_duckdb_schema(ddl: &str) -> String {
    let fields: Vec<String> = ddl
        .split(',')
        .filter_map(|field_def| {
            let trimmed = field_def.trim();
            let mut parts = trimmed.splitn(2, char::is_whitespace);
            let name = parts.next()?.trim();
            let spark_type = parts.next().unwrap_or("STRING").trim().to_uppercase();
            if name.is_empty() {
                return None;
            }
            let duckdb_type = spark_type_str_to_duckdb(&spark_type);
            Some(format!("\"{name}\": \"{duckdb_type}\""))
        })
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// Parse Spark StructType JSON format to DuckDB json_transform schema.
/// Spark JSON: `{"type":"struct","fields":[{"name":"n","type":"string","nullable":true,...}]}`
/// DuckDB result: `{"n": "VARCHAR", ...}`
fn spark_json_schema_to_duckdb(json: &str) -> String {
    // Simple field extraction: find all "name":"..." and "type":"..." pairs
    let mut fields: Vec<String> = Vec::new();
    // Find the "fields": [...] array content
    if let Some(arr_start) = json.find("\"fields\"") {
        let after_fields = &json[arr_start + 8..]; // skip "fields"
        if let Some(bracket_pos) = after_fields.find('[') {
            let arr_content = &after_fields[bracket_pos + 1..];
            // Parse each field object: {"name":"...", "type":"...", ...}
            let mut depth = 0i32;
            let mut field_start = 0;
            let bytes = arr_content.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'{' => {
                        if depth == 0 { field_start = i + 1; }
                        depth += 1;
                    }
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            let field_obj = &arr_content[field_start..i];
                            if let Some(entry) = parse_spark_field_json(field_obj) {
                                fields.push(entry);
                            }
                        }
                    }
                    b']' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
        }
    }
    if fields.is_empty() {
        // Fallback: try DDL parse
        return ddl_string_to_duckdb_schema(json);
    }
    format!("{{{}}}", fields.join(", "))
}

/// Extract ("name", "type") from a Spark field JSON object string and return DuckDB schema entry.
fn parse_spark_field_json(field_obj: &str) -> Option<String> {
    let name = extract_json_string_field(field_obj, "name")?;
    let spark_type = extract_json_string_field(field_obj, "type").unwrap_or_else(|| "string".to_string());
    let duckdb_type = spark_type_str_to_duckdb(&spark_type.to_uppercase());
    Some(format!("\"{name}\": \"{duckdb_type}\""))
}

/// Extract the string value of a JSON key from a flat JSON object string.
fn extract_json_string_field(json: &str, key: &str) -> Option<String> {
    let search = format!("\"{}\":", key);
    let pos = json.find(&search)?;
    let after_colon = json[pos + search.len()..].trim_start();
    if after_colon.starts_with('"') {
        let content = &after_colon[1..];
        let end = content.find('"')?;
        Some(content[..end].to_string())
    } else {
        None
    }
}

fn spark_type_str_to_duckdb(spark_type: &str) -> &'static str {
    let base = spark_type.split('(').next().unwrap_or(spark_type).trim();
    match base {
        "STRING" | "VARCHAR" | "TEXT" | "CHAR" => "VARCHAR",
        "INT" | "INTEGER" | "INT4" => "INTEGER",
        "LONG" | "BIGINT" | "INT8" => "BIGINT",
        "SHORT" | "SMALLINT" | "INT2" => "SMALLINT",
        "BYTE" | "TINYINT" | "INT1" => "TINYINT",
        "DOUBLE" | "FLOAT8" => "DOUBLE",
        "FLOAT" | "REAL" | "FLOAT4" => "FLOAT",
        "BOOLEAN" | "BOOL" => "BOOLEAN",
        "DATE" => "DATE",
        "TIMESTAMP" | "TIMESTAMP_NTZ" => "TIMESTAMP",
        "DECIMAL" | "NUMERIC" => "DOUBLE",
        "BINARY" | "BYTES" => "BLOB",
        _ if spark_type.starts_with("ARRAY") => "JSON",
        _ => "VARCHAR",
    }
}

/// Replace a Spark function name with a DuckDB equivalent.
/// Respects word boundaries: only replaces when not preceded/followed by word chars.
fn replace_spark_func(sql: &str, from: &str, to: &str) -> String {
    let from_len = from.len();
    let bytes = sql.as_bytes();
    let mut result = String::with_capacity(sql.len());
    let mut i = 0;
    let mut slice_start = 0;
    while i < bytes.len() {
        if i + from_len <= bytes.len()
            && sql[i..i + from_len].eq_ignore_ascii_case(from)
        {
            let prev_is_word = i > 0 && {
                let c = bytes[i - 1];
                c.is_ascii_alphanumeric() || c == b'_'
            };
            let mut j = i + from_len;
            while j < bytes.len() && bytes[j] == b' ' { j += 1; }
            let next_is_paren = j < bytes.len() && bytes[j] == b'(';
            if !prev_is_word && next_is_paren {
                result.push_str(&sql[slice_start..i]);
                result.push_str(to);
                i += from_len;
                slice_start = i;
                continue;
            }
        }
        i += 1;
    }
    result.push_str(&sql[slice_start..]);
    result
}

/// Rewrite NAMED_STRUCT('key1', val1, 'key2', val2, ...) → {'key1': val1, 'key2': val2}
fn rewrite_named_struct(sql: &str) -> String {
    let marker = "named_struct(";
    let mut result = String::with_capacity(sql.len());
    let sql_lower = sql.to_lowercase();
    let mut start = 0;
    while let Some(pos) = sql_lower[start..].find(marker) {
        let abs_pos = start + pos;
        // Check word boundary before
        let prev_is_word = abs_pos > 0 && {
            let c = sql.as_bytes()[abs_pos - 1] as char;
            c.is_alphanumeric() || c == '_'
        };
        if prev_is_word {
            result.push_str(&sql[start..abs_pos + marker.len()]);
            start = abs_pos + marker.len();
            continue;
        }
        // Emit everything before this match
        result.push_str(&sql[start..abs_pos]);
        // Find the matching closing paren
        let args_start = abs_pos + marker.len(); // after '('
        let mut depth = 1usize;
        let mut k = args_start;
        let bytes = sql.as_bytes();
        while k < bytes.len() && depth > 0 {
            match bytes[k] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                b'\'' | b'"' => {
                    let quote = bytes[k];
                    k += 1;
                    while k < bytes.len() && bytes[k] != quote { k += 1; }
                }
                _ => {}
            }
            if depth > 0 { k += 1; }
        }
        // k now points at closing ')'
        let args_str = &sql[args_start..k];
        // Parse comma-separated args (respecting parens/quotes)
        let args = split_sql_args(args_str);
        if args.len() % 2 == 0 {
            // Build struct literal: {key: val, ...}
            let fields: Vec<String> = args.chunks(2).map(|pair| {
                let key = pair[0].trim().trim_matches('\'').trim_matches('"');
                format!("{}: {}", key, pair[1].trim())
            }).collect();
            result.push_str(&format!("{{{}}}", fields.join(", ")));
        } else {
            // Odd arg count — just pass through unchanged
            result.push_str(&format!("named_struct({})", args_str));
        }
        start = k + 1; // skip ')'
    }
    result.push_str(&sql[start..]);
    result
}

/// Rewrite 3-arg split(str, pat, limit) → a CASE expression using STR_SPLIT_REGEX.
/// Spark semantics: at most `limit` pieces; the last piece contains the remainder.
fn rewrite_split_with_limit(sql: &str) -> String {
    let marker = "split(";
    let mut result = String::with_capacity(sql.len());
    let sql_lower = sql.to_lowercase();
    let mut start = 0;

    while let Some(rel_pos) = sql_lower[start..].find(marker) {
        let abs_pos = start + rel_pos;
        // Skip if preceded by a word char (e.g. str_split, str_split_regex)
        let prev_is_word = abs_pos > 0 && {
            let c = sql.as_bytes()[abs_pos - 1] as char;
            c.is_alphanumeric() || c == '_'
        };
        if prev_is_word {
            result.push_str(&sql[start..abs_pos + marker.len()]);
            start = abs_pos + marker.len();
            continue;
        }

        result.push_str(&sql[start..abs_pos]);

        // Find matching closing paren
        let args_start = abs_pos + marker.len();
        let mut depth = 1usize;
        let mut k = args_start;
        let bytes = sql.as_bytes();
        while k < bytes.len() && depth > 0 {
            match bytes[k] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'\'' | b'"' => {
                    let quote = bytes[k];
                    k += 1;
                    while k < bytes.len() && bytes[k] != quote { k += 1; }
                }
                _ => {}
            }
            if depth > 0 { k += 1; }
        }
        let args_str = &sql[args_start..k];
        let args = split_sql_args(args_str);

        if args.len() == 3 {
            let s = args[0].trim();
            let p = args[1].trim();
            let n = args[2].trim();
            result.push_str(&format!(
                "CASE WHEN ARRAY_LENGTH(STR_SPLIT_REGEX({s}, {p})) <= {n} \
                 THEN STR_SPLIT_REGEX({s}, {p}) \
                 ELSE LIST_APPEND(STR_SPLIT_REGEX({s}, {p})[1:{n}-1], \
                      ARRAY_TO_STRING(STR_SPLIT_REGEX({s}, {p})[{n}:], {p})) \
                 END"
            ));
        } else if args.len() == 2 {
            // Convert 2-arg split to STR_SPLIT_REGEX (DuckDB's split uses literal sep, not regex)
            let s = args[0].trim();
            let p = args[1].trim();
            result.push_str(&format!("STR_SPLIT_REGEX({s}, {p})"));
        } else {
            result.push_str(&format!("split({})", args_str));
        }
        start = k + 1;
    }
    result.push_str(&sql[start..]);
    result
}

/// Check if a trimmed string looks like a SQL type name (not a value).
/// Used to distinguish MAP(K_TYPE, V_TYPE) from MAP(key_val, val_val).
fn is_sql_type_name(s: &str) -> bool {
    let upper = s.trim().to_uppercase();
    matches!(
        upper.as_str(),
        "VARCHAR" | "TEXT" | "STRING" | "INTEGER" | "INT" | "BIGINT" | "SMALLINT"
            | "TINYINT" | "FLOAT" | "DOUBLE" | "BOOLEAN" | "BOOL" | "DATE"
            | "TIMESTAMP" | "BLOB" | "BINARY" | "DECIMAL" | "NUMERIC" | "REAL"
            | "HUGEINT" | "UBIGINT" | "UINTEGER" | "USMALLINT" | "UTINYINT"
    ) || upper.starts_with("DECIMAL(")
        || upper.starts_with("NUMERIC(")
        || upper.ends_with("[]")
        || upper.starts_with("MAP(")
        || upper.starts_with("STRUCT")
        || upper.starts_with("ARRAY")
}

/// Rewrite Spark's MAP(k1, v1, k2, v2, ...) constructor → DuckDB MAP([k1, k2, ...], [v1, v2, ...]).
/// Leaves MAP(key_type, val_type) type references unchanged.
fn rewrite_spark_map_constructor(sql: &str) -> String {
    let marker = "map(";
    let mut result = String::with_capacity(sql.len());
    let sql_lower = sql.to_lowercase();
    let mut start = 0;

    while let Some(rel_pos) = sql_lower[start..].find(marker) {
        let abs_pos = start + rel_pos;
        // Only replace standalone MAP( (not preceded by word chars like map_from_arrays)
        let prev_is_word = abs_pos > 0 && {
            let c = sql.as_bytes()[abs_pos - 1] as char;
            c.is_alphanumeric() || c == '_'
        };
        if prev_is_word {
            result.push_str(&sql[start..abs_pos + marker.len()]);
            start = abs_pos + marker.len();
            continue;
        }

        result.push_str(&sql[start..abs_pos]);

        // Find matching closing paren
        let args_start = abs_pos + marker.len();
        let mut depth = 1usize;
        let mut k = args_start;
        let bytes = sql.as_bytes();
        while k < bytes.len() && depth > 0 {
            match bytes[k] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'\'' | b'"' => {
                    let quote = bytes[k];
                    k += 1;
                    while k < bytes.len() && bytes[k] != quote { k += 1; }
                }
                _ => {}
            }
            if depth > 0 { k += 1; }
        }
        let args_str = &sql[args_start..k];
        let args = split_sql_args(args_str);

        // Only rewrite if it looks like a constructor (even # args, not SQL type names)
        let is_type_ref = args.len() == 2
            && is_sql_type_name(args[0].trim())
            && is_sql_type_name(args[1].trim());

        if !is_type_ref && args.len() >= 2 && args.len() % 2 == 0 {
            let keys: Vec<&str> = args.iter().step_by(2).map(|s| s.trim()).collect();
            let vals: Vec<&str> = args.iter().skip(1).step_by(2).map(|s| s.trim()).collect();
            result.push_str(&format!("MAP([{}], [{}])", keys.join(", "), vals.join(", ")));
        } else {
            result.push_str(&format!("MAP({})", args_str));
        }
        start = k + 1;
    }
    result.push_str(&sql[start..]);
    result
}

/// Rewrite Spark angle-bracket type syntax to DuckDB type syntax.
/// ARRAY<TYPE> → TYPE[], MAP<K, V> → MAP(K, V) (DuckDB accepts this for types).
fn rewrite_spark_type_syntax(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let sql_lower = sql.to_lowercase();
    let mut start = 0;

    while let Some(rel_pos) = sql_lower[start..].find("array<") {
        let abs_pos = start + rel_pos;
        // Check word boundary before
        let prev_is_word = abs_pos > 0 && {
            let c = sql.as_bytes()[abs_pos - 1] as char;
            c.is_alphanumeric() || c == '_'
        };
        if prev_is_word {
            result.push_str(&sql[start..abs_pos + 6]);
            start = abs_pos + 6;
            continue;
        }

        result.push_str(&sql[start..abs_pos]);

        // Find matching closing >
        let inner_start = abs_pos + 6; // after "array<"
        let mut depth = 1usize;
        let mut k = inner_start;
        let bytes = sql.as_bytes();
        while k < bytes.len() && depth > 0 {
            match bytes[k] {
                b'<' => depth += 1,
                b'>' => depth -= 1,
                b'\'' | b'"' => {
                    let quote = bytes[k];
                    k += 1;
                    while k < bytes.len() && bytes[k] != quote { k += 1; }
                }
                _ => {}
            }
            if depth > 0 { k += 1; }
        }
        let inner = &sql[inner_start..k];
        // Recursively convert inner type and append []
        let inner_converted = rewrite_spark_type_syntax(inner);
        result.push_str(&format!("{}[]", inner_converted));
        start = k + 1; // skip '>'
    }
    result.push_str(&sql[start..]);
    result
}

/// Rewrite percentile(col, pct) → PERCENTILE_CONT(pct) WITHIN GROUP (ORDER BY col).
/// Handles word-boundary detection to avoid matching `percentile_approx` etc.
fn rewrite_percentile(sql: &str) -> String {
    let marker = "percentile(";
    let mut result = String::with_capacity(sql.len());
    let sql_lower = sql.to_lowercase();
    let mut start = 0;
    while let Some(rel_pos) = sql_lower[start..].find(marker) {
        let abs_pos = start + rel_pos;
        // Skip if preceded by a word char (e.g. percentile_approx, percentile_disc)
        let prev_is_word = abs_pos > 0 && {
            let c = sql.as_bytes()[abs_pos - 1] as char;
            c.is_alphanumeric() || c == '_'
        };
        if prev_is_word {
            result.push_str(&sql[start..abs_pos + marker.len()]);
            start = abs_pos + marker.len();
            continue;
        }
        result.push_str(&sql[start..abs_pos]);
        let args_start = abs_pos + marker.len();
        let mut depth = 1usize;
        let mut k = args_start;
        let bytes = sql.as_bytes();
        while k < bytes.len() && depth > 0 {
            match bytes[k] {
                b'(' => { depth += 1; k += 1; }
                b')' => { depth -= 1; if depth > 0 { k += 1; } }
                b'\'' | b'"' => {
                    let quote = bytes[k]; k += 1;
                    while k < bytes.len() && bytes[k] != quote { k += 1; }
                    if k < bytes.len() { k += 1; }
                }
                _ => { k += 1; }
            }
        }
        let args_str = &sql[args_start..k];
        let args = split_sql_args(args_str);
        if args.len() >= 2 {
            result.push_str(&format!(
                "PERCENTILE_CONT({}) WITHIN GROUP (ORDER BY {})",
                args[1].trim(),
                args[0].trim()
            ));
        } else {
            result.push_str(&format!("percentile({})", args_str));
        }
        start = k + 1;
    }
    result.push_str(&sql[start..]);
    result
}

/// Rewrite `overlay(str PLACING repl FROM pos [FOR len])` SQL standard syntax
/// to string concat form: `LEFT(str, pos-1) || repl || SUBSTRING(str, pos+len)`.
/// DuckDB 1.5 has no OVERLAY function.
fn rewrite_overlay_syntax(sql: &str) -> String {
    let marker = "overlay(";
    let mut result = String::with_capacity(sql.len());
    let sql_lower = sql.to_lowercase();
    let mut start = 0;

    while let Some(rel_pos) = sql_lower[start..].find(marker) {
        let abs_pos = start + rel_pos;
        // Check word boundary before
        let prev_is_word = abs_pos > 0 && {
            let c = sql.as_bytes()[abs_pos - 1] as char;
            c.is_alphanumeric() || c == '_'
        };
        let inner_start = abs_pos + marker.len();

        if prev_is_word {
            result.push_str(&sql[start..inner_start]);
            start = inner_start;
            continue;
        }

        // Find the matching closing paren
        let mut depth = 1usize;
        let mut k = inner_start;
        let bytes = sql.as_bytes();
        while k < bytes.len() && depth > 0 {
            match bytes[k] {
                b'(' => depth += 1,
                b')' => { depth -= 1; if depth == 0 { break; } }
                b'\'' | b'"' => {
                    let quote = bytes[k]; k += 1;
                    while k < bytes.len() && bytes[k] != quote { k += 1; }
                }
                _ => {}
            }
            k += 1;
        }

        let inner = &sql[inner_start..k]; // content between outer parens
        let inner_lower = inner.to_lowercase();

        // Check if this is the SQL-standard PLACING...FROM...FOR syntax
        if let Some(placing_pos) = inner_lower.find(" placing ") {
            let str_expr = inner[..placing_pos].trim();
            let after_placing = &inner[placing_pos + " placing ".len()..];
            let after_lower = after_placing.to_lowercase();

            if let Some(from_pos) = after_lower.find(" from ") {
                let repl_expr = after_placing[..from_pos].trim();
                let after_from = &after_placing[from_pos + " from ".len()..];
                let after_from_lower = after_from.to_lowercase();

                result.push_str(&sql[start..abs_pos]);
                if let Some(for_pos) = after_from_lower.find(" for ") {
                    let pos_expr = after_from[..for_pos].trim();
                    let len_expr = after_from[for_pos + " for ".len()..].trim();
                    result.push_str(&format!(
                        "LEFT({str_expr}, ({pos_expr}) - 1) || ({repl_expr}) || SUBSTRING({str_expr}, ({pos_expr}) + ({len_expr}))"
                    ));
                } else {
                    let pos_expr = after_from.trim();
                    result.push_str(&format!(
                        "LEFT({str_expr}, ({pos_expr}) - 1) || ({repl_expr}) || SUBSTRING({str_expr}, ({pos_expr}) + LENGTH({repl_expr}))"
                    ));
                }
                start = k + 1; // skip closing ')'
                continue;
            }
        }

        // Not SQL-standard syntax — pass through (may be regular function call args)
        result.push_str(&sql[start..inner_start]);
        start = inner_start;
    }

    result.push_str(&sql[start..]);
    result
}

/// Build `SELECT key1, key2, * EXCLUDE (key1, key2)` for a USING join.
///
/// DuckDB's USING deduplicates key columns but places them at their left-table position.
/// Spark always puts USING key columns first. This SELECT puts keys first without
/// requiring static schema knowledge.
fn gen_using_join_select(using_columns: &[String]) -> String {
    let keys: Vec<String> = using_columns.iter().map(|c| quote_ident(c)).collect();
    let exclude = if keys.len() == 1 {
        keys[0].clone()
    } else {
        format!("({})", keys.join(", "))
    };
    format!("SELECT {}, * EXCLUDE {}", keys.join(", "), exclude)
}

fn equijoin_to_using(expr: &Expression) -> Option<Vec<String>> {
    match expr {
        Expression::Binary(b) if b.op == BinaryOp::Eq => {
            let l = col_name_unqualified(&b.left)?;
            let r = col_name_unqualified(&b.right)?;
            if l == r { Some(vec![l]) } else { None }
        }
        Expression::Binary(b) if b.op == BinaryOp::And => {
            let mut l = equijoin_to_using(&b.left)?;
            let r = equijoin_to_using(&b.right)?;
            l.extend(r);
            Some(l)
        }
        _ => None,
    }
}

fn col_name_unqualified(expr: &Expression) -> Option<String> {
    match expr {
        Expression::UnresolvedColumn(u) => {
            // Columns with plan_id qualifiers are side-specific; don't use them for USING.
            if u.qualifier.as_ref().map_or(false, |q| q.starts_with("__plan_id_") && q.ends_with("__")) {
                return None;
            }
            Some(u.name.clone())
        }
        Expression::ColumnReference(c) => {
            if c.qualifier.is_some() { return None; }
            Some(c.name.clone())
        }
        _ => None,
    }
}

/// Recursively collect all `AliasedRelation` aliases from a plan tree.
/// Used to identify table qualifiers in SEMI/ANTI join conditions that refer
/// to relations inside a subquery (where the alias won't be directly accessible).
fn collect_plan_aliases(plan: &LogicalPlan, aliases: &mut std::collections::HashSet<String>) {
    match plan {
        LogicalPlan::AliasedRelation(ar) => { aliases.insert(ar.alias.clone()); }
        LogicalPlan::Filter(f) => collect_plan_aliases(&f.input, aliases),
        LogicalPlan::Join(j) => {
            collect_plan_aliases(&j.left, aliases);
            collect_plan_aliases(&j.right, aliases);
        }
        LogicalPlan::Project(p) => collect_plan_aliases(&p.input, aliases),
        LogicalPlan::Aggregate(a) => collect_plan_aliases(&a.input, aliases),
        LogicalPlan::Sort(s) => collect_plan_aliases(&s.input, aliases),
        LogicalPlan::Limit(l) => collect_plan_aliases(&l.input, aliases),
        LogicalPlan::Distinct(d) => collect_plan_aliases(&d.input, aliases),
        _ => {}
    }
}

/// Strip table qualifiers from `UnresolvedColumn` references where the qualifier
/// matches one of the given aliases. Used for SEMI/ANTI join conditions so that
/// columns from the (wrapped) left subquery are accessible without their qualifier.
fn strip_qualifiers_in_expr(
    expr: Expression,
    aliases: &std::collections::HashSet<String>,
) -> Expression {
    match expr {
        Expression::UnresolvedColumn(u) => {
            if u.qualifier.as_ref().map_or(false, |q| aliases.contains(q)) {
                Expression::UnresolvedColumn(UnresolvedColumn { name: u.name, qualifier: None })
            } else {
                Expression::UnresolvedColumn(u)
            }
        }
        Expression::Binary(b) => {
            let left = strip_qualifiers_in_expr(*b.left, aliases);
            let right = strip_qualifiers_in_expr(*b.right, aliases);
            Expression::Binary(BinaryExpression { op: b.op, left: Box::new(left), right: Box::new(right) })
        }
        Expression::Unary(u) => {
            let operand = strip_qualifiers_in_expr(*u.operand, aliases);
            Expression::Unary(UnaryExpression { op: u.op, operand: Box::new(operand) })
        }
        other => other,
    }
}

/// Returns the natural table/alias name for a simple leaf-like plan node (TableScan,
/// InMemoryRelation, AliasedRelation). Used in the flat join path to determine what alias
/// to use when rewriting __td_jr_X__ qualifiers in join ON conditions.
fn right_plan_natural_name(plan: &LogicalPlan) -> Option<String> {
    match plan {
        LogicalPlan::TableScan(ts) => {
            Some(ts.alias.clone().unwrap_or_else(|| ts.table.clone()))
        }
        LogicalPlan::InMemoryRelation(imr) => Some(imr.view_name.clone()),
        LogicalPlan::AliasedRelation(ar) => {
            if !ar.alias.starts_with("__") { Some(ar.alias.clone()) } else { None }
        }
        _ => None,
    }
}

/// Returns true if the plan tree contains any AliasedRelation with a user-facing alias
/// (i.e., not starting with "__"). Used to detect when the flat join path is needed
/// to keep those aliases accessible in outer WHERE/HAVING clauses.
fn plan_contains_user_alias(plan: &LogicalPlan) -> bool {
    match plan {
        LogicalPlan::AliasedRelation(ar) => !ar.alias.starts_with("__"),
        LogicalPlan::Join(j) => {
            plan_contains_user_alias(&j.left) || plan_contains_user_alias(&j.right)
        }
        LogicalPlan::Filter(f) => plan_contains_user_alias(&f.input),
        LogicalPlan::Project(p) => plan_contains_user_alias(&p.input),
        LogicalPlan::Aggregate(a) => plan_contains_user_alias(&a.input),
        LogicalPlan::Sort(s) => plan_contains_user_alias(&s.input),
        LogicalPlan::Limit(l) => plan_contains_user_alias(&l.input),
        LogicalPlan::Distinct(d) => plan_contains_user_alias(&d.input),
        _ => false,
    }
}

/// Rewrite qualifiers in a join ON condition for the "natural flat join" path.
///
/// When we use an AliasedRelation's natural alias (e.g. "d1") instead of the generated
/// __td_jr_X__ alias, we need to:
/// 1. Replace __td_jr_X__ qualifier → nat_alias (e.g. "d1")
/// 2. Strip __td_jl_X__ qualifiers (left-side columns are unqualified in flat join)
///
/// This is applied to the ON condition so DuckDB sees proper column references.
fn rewrite_td_join_qualifiers(
    expr: Expression,
    td_right_alias: &str,
    nat_alias: &str,
) -> Expression {
    match expr {
        Expression::UnresolvedColumn(u) => {
            match u.qualifier.as_deref() {
                Some(q) if q == td_right_alias => {
                    // Right-side column: use natural alias
                    Expression::UnresolvedColumn(UnresolvedColumn {
                        name: u.name,
                        qualifier: Some(nat_alias.to_string()),
                    })
                }
                Some(q) if q.starts_with("__td_jl_") => {
                    // Left-side column: strip qualifier (accessible unqualified in flat join)
                    Expression::UnresolvedColumn(UnresolvedColumn { name: u.name, qualifier: None })
                }
                _ => Expression::UnresolvedColumn(u),
            }
        }
        Expression::Binary(b) => {
            let left = rewrite_td_join_qualifiers(*b.left, td_right_alias, nat_alias);
            let right = rewrite_td_join_qualifiers(*b.right, td_right_alias, nat_alias);
            Expression::Binary(BinaryExpression { op: b.op, left: Box::new(left), right: Box::new(right) })
        }
        Expression::Unary(u) => {
            let operand = rewrite_td_join_qualifiers(*u.operand, td_right_alias, nat_alias);
            Expression::Unary(UnaryExpression { op: u.op, operand: Box::new(operand) })
        }
        other => other,
    }
}

/// Split a SQL argument list by commas, respecting nested parens and quotes.
///
/// Delegates to `extract_top_level_args` so the logic lives in exactly one place.
fn split_sql_args(s: &str) -> Vec<String> {
    extract_top_level_args(s.as_bytes(), 0).0
}

/// Rewrite `DATE 'lit' + INTERVAL 'n' YEAR/MONTH` → `CAST(... AS DATE)`.
/// DuckDB promotes DATE + year/month interval to TIMESTAMP; Spark preserves DATE type.
fn rewrite_date_interval_to_date(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    // Quick scan: skip if no DATE literal at all
    if !sql.to_uppercase().contains("DATE '") {
        return sql.to_string();
    }
    let upper: String = sql.to_ascii_uppercase();
    let ub = upper.as_bytes();
    let mut result = String::with_capacity(n + 40);
    let mut i = 0;

    while i < n {
        // Match DATE keyword at word boundary
        let at_date = i + 4 <= n
            && &ub[i..i + 4] == b"DATE"
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
            && (i + 4 >= n || !(bytes[i + 4].is_ascii_alphanumeric() || bytes[i + 4] == b'_'));

        if at_date {
            if let Some((full, end)) = try_date_plus_interval(bytes, ub.as_ref(), i) {
                result.push_str(&format!("CAST({full} AS DATE)"));
                i = end;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Try to match `DATE 'lit' OP INTERVAL 'n' YEAR/MONTH` at position `start`.
/// Returns `(full_expression_str, end_position)` on success.
fn try_date_plus_interval(bytes: &[u8], upper: &[u8], start: usize) -> Option<(String, usize)> {
    let n = bytes.len();
    let mut i = start + 4; // skip "DATE"
    while i < n && bytes[i] == b' ' { i += 1; }
    // Date literal: 'YYYY-MM-DD'
    if i >= n || bytes[i] != b'\'' { return None; }
    i += 1;
    while i < n && bytes[i] != b'\'' { i += 1; }
    if i >= n { return None; }
    i += 1; // past closing quote
    while i < n && bytes[i] == b' ' { i += 1; }
    // Operator: + or -
    if i >= n || (bytes[i] != b'+' && bytes[i] != b'-') { return None; }
    i += 1;
    while i < n && bytes[i] == b' ' { i += 1; }
    // INTERVAL keyword
    if i + 8 > n || &upper[i..i + 8] != b"INTERVAL" { return None; }
    i += 8;
    while i < n && bytes[i] == b' ' { i += 1; }
    // Interval value: 'n'
    if i >= n || bytes[i] != b'\'' { return None; }
    i += 1;
    while i < n && bytes[i] != b'\'' { i += 1; }
    if i >= n { return None; }
    i += 1; // past closing quote
    while i < n && bytes[i] == b' ' { i += 1; }
    // Unit keyword — only wrap for YEAR/MONTH/QUARTER (not DAY/HOUR/MINUTE/SECOND)
    let rem = &upper[i.min(n)..];
    let is_ym = rem.starts_with(b"YEAR") || rem.starts_with(b"MONTH") || rem.starts_with(b"QUARTER");
    if !is_ym { return None; }
    while i < n && bytes[i].is_ascii_alphabetic() { i += 1; }

    let expr = std::str::from_utf8(&bytes[start..i]).ok()?;
    Some((expr.to_string(), i))
}

/// Rewrite a Spark SQL HOF function call to its DuckDB equivalent.
/// Finds `func_name(` at word boundaries, extracts top-level arguments, and applies `rewrite`.
/// Rewrite `json_tuple(col, 'k1', 'k2') AS (a1, a2)` →
/// `json_extract_string(col, '$.k1') AS a1, json_extract_string(col, '$.k2') AS a2`
///
/// This handles the Spark SQL generator-function syntax where `json_tuple` produces
/// multiple columns and `AS (names)` provides column aliases.
pub(crate) fn rewrite_json_tuple(sql: &str) -> String {
    let needle = "json_tuple";
    let bytes = sql.as_bytes();
    let slen = bytes.len();
    let flen = needle.len();
    let mut result = String::with_capacity(sql.len() + 64);
    let mut i = 0;

    while i < slen {
        // Check for `json_tuple(`
        let matches = i + flen < slen
            && bytes[i + flen] == b'('
            && sql[i..i + flen].eq_ignore_ascii_case(needle)
            && (i == 0 || {
                let prev = bytes[i - 1];
                !(prev.is_ascii_alphanumeric() || prev == b'_')
            });

        if matches {
            let (args, end_paren) = extract_top_level_args(bytes, i + flen + 1);
            // After the closing ')' look for optional `AS (alias1, alias2, ...)`
            let after_call = &sql[end_paren + 1..];
            let after_trimmed = after_call.trim_start();
            // Check for `AS (`
            let (aliases, consumed_after) =
                if after_trimmed.to_ascii_uppercase().starts_with("AS") {
                    let after_as = &after_trimmed[2..];
                    let rest = after_as.trim_start();
                    if rest.starts_with('(') {
                        // Find the closing ')'
                        if let Some(close) = rest.find(')') {
                            let alias_str = &rest[1..close];
                            let aliases: Vec<String> = alias_str
                                .split(',')
                                .map(|a| a.trim().to_string())
                                .collect();
                            // Bytes consumed in after_call:
                            //   leading_spaces + "AS" + spaces_before_paren + "(alias_str)"
                            // close is the 0-based index of ')' in rest, so "(alias_str)" = close+1 bytes
                            let leading = after_call.len() - after_trimmed.len();
                            let spaces_before = after_as.len() - rest.len();
                            let consumed = leading + 2 + spaces_before + close + 1;
                            (aliases, consumed)
                        } else {
                            (vec![], 0)
                        }
                    } else {
                        (vec![], 0)
                    }
                } else {
                    (vec![], 0)
                };

            // Build the expanded expression
            if args.is_empty() {
                result.push_str("NULL");
            } else {
                let col = &args[0];
                let keys = &args[1..];
                let mut parts: Vec<String> = Vec::with_capacity(keys.len());
                for (ki, key) in keys.iter().enumerate() {
                    // key is like `'name'`; strip quotes for the path
                    let key_str = key.trim().trim_matches('\'');
                    let extract = format!("json_extract_string({col}, '$.{key_str}')");
                    if let Some(alias) = aliases.get(ki) {
                        if !alias.is_empty() {
                            parts.push(format!("{extract} AS {alias}"));
                            continue;
                        }
                    }
                    parts.push(extract);
                }
                result.push_str(&parts.join(", "));
            }

            // Advance past the call and the AS (...) clause
            i = end_paren + 1 + consumed_after;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

fn rewrite_hof_func(sql: &str, func_name: &str, rewrite: impl Fn(&[String]) -> String) -> String {
    let flen = func_name.len();
    let bytes = sql.as_bytes();
    let slen = bytes.len();
    let mut result = String::with_capacity(sql.len() + 64);
    let mut i = 0;

    while i < slen {
        // Check word boundary: not preceded by word char, followed by func_name (case-insensitive) + '('
        let name_matches = i + flen < slen
            && bytes[i + flen] == b'('
            && sql[i..i + flen].eq_ignore_ascii_case(func_name)
            && (i == 0 || {
                let prev = bytes[i - 1];
                !(prev.is_ascii_alphanumeric() || prev == b'_')
            });

        if name_matches {
            let (args, end_pos) = extract_top_level_args(bytes, i + flen + 1);
            result.push_str(&rewrite(&args));
            i = end_pos + 1;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// Extract top-level comma-separated arguments from a function call starting at `start`
/// (the position right after the opening `(`).
/// Returns `(args, index_of_closing_paren)`.
///
/// Uses slice-based accumulation: no character-by-character push, one `.to_string()` per arg.
fn extract_top_level_args(bytes: &[u8], start: usize) -> (Vec<String>, usize) {
    // All callers pass bytes from a valid UTF-8 SQL string.
    let s = std::str::from_utf8(bytes).unwrap_or("");
    let mut args: Vec<String> = Vec::new();
    let mut depth: i32 = 0;
    let mut i = start;
    let mut arg_start = start;

    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => { depth += 1; i += 1; }
            b')' if depth == 0 => {
                let trimmed = s[arg_start..i].trim();
                if !trimmed.is_empty() || !args.is_empty() {
                    args.push(trimmed.to_string());
                }
                return (args, i);
            }
            b')' | b']' | b'}' => { depth -= 1; i += 1; }
            b'\'' | b'"' | b'`' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != q { i += 1; }
                if i < bytes.len() { i += 1; } // skip closing quote
            }
            b',' if depth == 0 => {
                args.push(s[arg_start..i].trim().to_string());
                i += 1;
                arg_start = i;
            }
            _ => { i += 1; }
        }
    }
    // Unterminated — push whatever remains
    let trimmed = s[arg_start..i.min(s.len())].trim();
    if !trimmed.is_empty() {
        args.push(trimmed.to_string());
    }
    (args, i)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::{
        AliasExpression, BinaryExpression, ColumnReference, ExtractValueExpression,
        IntervalExpression, IsDistinctFromExpression, LikeExpression, Literal,
        RowConstructorExpression,
    };
    use crate::logical::{
        Filter, Limit, LogicalPlan, Project, SingleRowRelation, Sort, TableScan, Union,
    };

    fn gen() -> SqlGenerator {
        SqlGenerator::relaxed()
    }

    fn table(name: &str) -> LogicalPlan {
        LogicalPlan::TableScan(TableScan { table: name.to_string(), alias: None })
    }

    fn col(name: &str) -> Expression {
        ColumnReference::untyped(name)
    }

    fn int(n: i32) -> Expression {
        Literal::int(n)
    }

    #[test]
    fn simple_table_scan() {
        let plan = table("orders");
        let sql = gen().generate(&plan).unwrap();
        assert_eq!(sql, r#""orders""#);
    }

    #[test]
    fn project_with_alias() {
        let plan = LogicalPlan::Project(Project {
            input: Box::new(table("t")),
            projections: vec![
                Expression::Alias(AliasExpression {
                    expr: Box::new(col("a")),
                    alias: "x".to_string(),
                }),
            ],
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(sql.contains("\"a\" AS \"x\""), "got: {sql}");
    }

    #[test]
    fn filter_with_condition() {
        let plan = LogicalPlan::Filter(Filter {
            input: Box::new(table("t")),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(col("age")),
                right: Box::new(int(18)),
            }),
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(sql.contains("WHERE"), "got: {sql}");
        assert!(sql.contains("\"age\" > 18"), "got: {sql}");
    }

    #[test]
    fn sort_asc() {
        let plan = LogicalPlan::Sort(Sort {
            input: Box::new(table("t")),
            order: vec![SortOrder::asc(col("name"))],
            limit: None,
            offset: None,
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(sql.contains("ORDER BY"), "got: {sql}");
        assert!(sql.contains("ASC"), "got: {sql}");
    }

    #[test]
    fn limit_clause() {
        let plan = LogicalPlan::Limit(Limit {
            input: Box::new(table("t")),
            limit: int(10),
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(sql.contains("LIMIT 10"), "got: {sql}");
    }

    #[test]
    fn literal_string_escaping() {
        let expr = Literal::string("it's fine");
        let sql = gen().gen_expr(&expr).unwrap();
        assert_eq!(sql, "'it''s fine'");
    }

    #[test]
    fn quote_ident_special() {
        assert_eq!(quote_ident("my table"), r#""my table""#);
        assert_eq!(quote_ident(r#"a"b"#), r#""a""b""#);
    }

    #[test]
    fn case_when() {
        let expr = Expression::CaseWhen(CaseWhenExpression {
            base: None,
            branches: vec![(
                Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(col("x")),
                    right: Box::new(int(1)),
                }),
                Literal::string("one"),
            )],
            else_expr: Some(Box::new(Literal::string("other"))),
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert!(sql.starts_with("CASE WHEN"), "got: {sql}");
        assert!(sql.contains("ELSE"), "got: {sql}");
        assert!(sql.ends_with("END"), "got: {sql}");
    }

    #[test]
    fn union_all() {
        let plan = LogicalPlan::Union(Union {
            left: Box::new(table("a")),
            right: Box::new(table("b")),
            all: true,
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(sql.contains("UNION ALL"), "got: {sql}");
    }

    #[test]
    fn range_relation() {
        use crate::logical::RangeRelation;
        let plan = LogicalPlan::RangeRelation(RangeRelation {
            start: 0, end: 10, step: 1, num_partitions: None,
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(sql.contains("range(0, 10, 1)"), "got: {sql}");
        assert!(sql.contains("id"), "got: {sql}");
    }

    // ── Bug regression tests ───────────────────────────────────────────────────

    /// Bug: gen_tail uses `rowid` which is not a stable row-order index on
    /// arbitrary subqueries. Fix: use ROW_NUMBER() OVER ().
    #[test]
    fn tail_no_rowid() {
        use crate::logical::Tail;
        let plan = LogicalPlan::Tail(Tail {
            input: Box::new(table("orders")),
            limit: int(5),
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(
            !sql.to_lowercase().contains("rowid"),
            "tail SQL must not use rowid (unreliable on subqueries): {sql}"
        );
        assert!(
            sql.to_lowercase().contains("row_number"),
            "tail SQL must use row_number() for stable ordering: {sql}"
        );
    }

    /// Bug: gen_to_dataframe falls back to `col0`, `col1`, … references when
    /// input schema cannot be inferred (e.g. SqlRelation). Those column names
    /// do not exist in the query and produce invalid SQL.
    #[test]
    fn to_dataframe_empty_schema_no_phantom_cols() {
        use crate::logical::{SqlRelation, ToDataFrame};
        let plan = LogicalPlan::ToDataFrame(ToDataFrame {
            input: Box::new(LogicalPlan::SqlRelation(SqlRelation {
                sql: "SELECT 1 AS a, 2 AS b".to_string(),
                schema: crate::types::StructType::empty(),
            })),
            column_names: vec!["x".to_string(), "y".to_string()],
        });
        let sql = gen().generate(&plan).unwrap();
        assert!(
            !sql.contains("col0"),
            "fallback must not emit phantom col0 reference: {sql}"
        );
    }

    // ── New expression tests ───────────────────────────────────────────────────

    #[test]
    fn like_generates_correct_sql() {
        let expr = Expression::Like(LikeExpression {
            value: Box::new(col("name")),
            pattern: Box::new(Literal::string("%smith%")),
            negated: false,
            case_insensitive: false,
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert_eq!(sql, r#"("name" LIKE '%smith%')"#, "got: {sql}");
    }

    #[test]
    fn like_negated_and_ilike() {
        let not_like = Expression::Like(LikeExpression {
            value: Box::new(col("name")),
            pattern: Box::new(Literal::string("A%")),
            negated: true,
            case_insensitive: false,
        });
        let sql = gen().gen_expr(&not_like).unwrap();
        assert!(sql.contains("NOT LIKE"), "expected NOT LIKE, got: {sql}");

        let ilike = Expression::Like(LikeExpression {
            value: Box::new(col("name")),
            pattern: Box::new(Literal::string("%SMITH%")),
            negated: false,
            case_insensitive: true,
        });
        let sql = gen().gen_expr(&ilike).unwrap();
        assert!(sql.contains("ILIKE"), "expected ILIKE, got: {sql}");
        assert!(!sql.contains("NOT"), "ILIKE should not contain NOT: {sql}");

        let not_ilike = Expression::Like(LikeExpression {
            value: Box::new(col("name")),
            pattern: Box::new(Literal::string("%SMITH%")),
            negated: true,
            case_insensitive: true,
        });
        let sql = gen().gen_expr(&not_ilike).unwrap();
        assert!(sql.contains("NOT ILIKE"), "expected NOT ILIKE, got: {sql}");
    }

    #[test]
    fn interval_year_month() {
        let expr = Expression::Interval(IntervalExpression {
            months: 3,
            days: 0,
            microseconds: 0,
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert_eq!(sql, "INTERVAL '3' MONTH", "got: {sql}");
    }

    #[test]
    fn interval_day_time_decomposition() {
        // 1 day + 2 hours in microseconds
        const MICROS_PER_HOUR: i64 = 60 * 60 * 1_000_000;
        const MICROS_PER_DAY: i64 = 24 * MICROS_PER_HOUR;
        let micros = MICROS_PER_DAY + 2 * MICROS_PER_HOUR;
        let expr = Expression::Interval(IntervalExpression {
            months: 0,
            days: 0,
            microseconds: micros,
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert!(sql.contains("DAY"), "expected DAY in: {sql}");
        assert!(sql.contains("HOUR"), "expected HOUR in: {sql}");
    }

    #[test]
    fn interval_zero_microseconds() {
        // Zero microseconds in day-time form should still produce a valid interval
        let expr = Expression::Interval(IntervalExpression {
            months: 0,
            days: 0,
            microseconds: 0,
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert!(sql.contains("INTERVAL"), "expected INTERVAL in: {sql}");
        assert!(sql.contains("SECOND"), "expected SECOND fallback in: {sql}");
    }

    #[test]
    fn is_distinct_from() {
        let expr = Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(col("a")),
            right: Box::new(Literal::null()),
            negated: false,
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert!(sql.contains("IS DISTINCT FROM"), "got: {sql}");
        assert!(!sql.contains("NOT"), "should not have NOT: {sql}");

        let negated = Expression::IsDistinctFrom(IsDistinctFromExpression {
            left: Box::new(col("a")),
            right: Box::new(col("b")),
            negated: true,
        });
        let sql = gen().gen_expr(&negated).unwrap();
        assert!(sql.contains("IS NOT DISTINCT FROM"), "got: {sql}");
    }

    #[test]
    fn extract_value_string_key() {
        let expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(col("person")),
            extraction: Box::new(Literal::string("name")),
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert_eq!(sql, r#""person"['name']"#, "got: {sql}");
    }

    #[test]
    fn extract_value_numeric_index() {
        let expr = Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(col("arr")),
            extraction: Box::new(int(1)),
        });
        let sql = gen().gen_expr(&expr).unwrap();
        // Spark uses 0-based array indexing; DuckDB uses 1-based.
        // So Spark index 1 → DuckDB index 2.
        assert_eq!(sql, r#""arr"[2]"#, "got: {sql}");
    }

    #[test]
    fn row_constructor_generates_tuple() {
        let expr = Expression::RowConstructor(RowConstructorExpression {
            fields: vec![int(1), int(2), int(3)],
        });
        let sql = gen().gen_expr(&expr).unwrap();
        assert_eq!(sql, "(1, 2, 3)", "got: {sql}");
    }

    #[test]
    fn single_row_relation_no_from_clause() {
        // SELECT 1 → SELECT 1  (no FROM)
        let plan = LogicalPlan::Project(Project {
            input: Box::new(LogicalPlan::SingleRow(SingleRowRelation)),
            projections: vec![int(1)],
        });
        let sql = gen().generate(&plan).unwrap();
        assert_eq!(sql, "SELECT 1", "expected no FROM clause, got: {sql}");
    }

    #[test]
    fn single_row_relation_standalone() {
        // SingleRow standalone produces a single-row result
        let plan = LogicalPlan::SingleRow(SingleRowRelation);
        let sql = gen().generate(&plan).unwrap();
        assert!(sql.contains("SELECT"), "got: {sql}");
    }

    #[test]
    fn backtick_identifiers_rewritten_to_double_quote() {
        // Spark SQL uses backtick quoting; DuckDB requires double quotes.
        assert_eq!(
            rewrite_backtick_identifiers("SELECT `l_orderkey` FROM lineitem"),
            r#"SELECT "l_orderkey" FROM lineitem"#,
        );
        // Multiple identifiers in one expression
        assert_eq!(
            rewrite_backtick_identifiers("count(`l_orderkey`), count(`l_partkey`)"),
            r#"count("l_orderkey"), count("l_partkey")"#,
        );
        // Backticks inside string literals must NOT be converted
        assert_eq!(
            rewrite_backtick_identifiers("SELECT 'hello `world`' AS greeting"),
            "SELECT 'hello `world`' AS greeting",
        );
        // Already double-quoted identifiers pass through unchanged
        assert_eq!(
            rewrite_backtick_identifiers(r#"SELECT "col" FROM t"#),
            r#"SELECT "col" FROM t"#,
        );
    }

}
