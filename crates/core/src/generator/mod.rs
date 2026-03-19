//! SQL generator: translates `LogicalPlan` and `Expression` trees into DuckDB SQL.
//!
//! # Critical invariant
//! Always call `to_sql()` / `generate()`. Never use `Display` or `Debug` impls
//! to produce SQL strings sent to DuckDB.

use crate::error::Result;
use crate::expression::{
    AliasExpression, ArrayLiteralExpression, BetweenExpression, BinaryExpression, BinaryOp,
    CaseWhenExpression, CastExpression, Expression, ExistsSubquery, ExtractValueExpression,
    FrameBoundary, FrameUnit, FunctionCall, InListExpression, InSubquery, IntervalExpression,
    IsDistinctFromExpression, LambdaExpression, LikeExpression, LiteralValue,
    MapLiteralExpression, RowConstructorExpression, ScalarSubquery, SortOrder,
    StructLiteralExpression, UnaryExpression, UnaryOp, WindowFunction,
};
use crate::functions::{CompatMode, FunctionRegistry};
use crate::logical::{
    Aggregate, AliasedRelation, Distinct, DropColumns, Except, Filter, GroupingSets,
    InMemoryRelation, Intersect, Join, Limit, LocalDataRelation, LocalRelation, LogicalPlan,
    NADrop, NADropHow, NAFill, NAReplace, Project, RangeRelation, Sample, SelectEntry, ShowString,
    SingleRowRelation, Sort, SqlRelation, TableScan, Tail, ToDataFrame, Union, Unpivot,
    WithColumns, WithCte,
};
use crate::types::{DataType, StructType, TypeMapper};

// ── Public API ─────────────────────────────────────────────────────────────────

/// Generates DuckDB-compatible SQL from a `LogicalPlan` tree.
pub struct SqlGenerator {
    mode: CompatMode,
}

impl SqlGenerator {
    pub fn new(mode: CompatMode) -> Self {
        Self { mode }
    }

    pub fn relaxed() -> Self {
        Self::new(CompatMode::Relaxed)
    }

    pub fn strict() -> Self {
        Self::new(CompatMode::Strict)
    }

    /// Generate a complete SQL statement from the plan.
    pub fn generate(&self, plan: &LogicalPlan) -> Result<String> {
        self.gen_plan(plan)
    }
}

// ── Plan generation ────────────────────────────────────────────────────────────

impl SqlGenerator {
    fn gen_plan(&self, plan: &LogicalPlan) -> Result<String> {
        match plan {
            LogicalPlan::Project(p) => self.gen_project(p),
            LogicalPlan::Filter(f) => self.gen_filter(f),
            LogicalPlan::Aggregate(a) => self.gen_aggregate(a),
            LogicalPlan::Join(j) => self.gen_join(j),
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
            LogicalPlan::AliasedRelation(ar) => self.gen_aliased_relation(ar),
            LogicalPlan::RawDdlStatement(r) => Ok(r.sql.clone()),
            LogicalPlan::ToDataFrame(t) => self.gen_to_dataframe(t),
            LogicalPlan::SingleRow(sr) => self.gen_single_row(sr),
            LogicalPlan::DropColumns(d) => self.gen_drop_columns(d),
            LogicalPlan::ShowString(s) => self.gen_show_string(s),
            LogicalPlan::NADrop(n) => self.gen_na_drop(n),
            LogicalPlan::NAFill(n) => self.gen_na_fill(n),
            LogicalPlan::NAReplace(n) => self.gen_na_replace(n),
            LogicalPlan::Unpivot(u) => self.gen_unpivot(u),
        }
    }

    fn gen_project(&self, p: &Project) -> Result<String> {
        let cols = self.gen_projection_list(&p.projections)?;
        // SingleRow means no FROM clause (e.g. SELECT 1, SELECT now())
        if matches!(p.input.as_ref(), LogicalPlan::SingleRow(_)) {
            return Ok(format!("SELECT {cols}"));
        }
        let from = self.gen_from(&p.input)?;
        Ok(format!("SELECT {cols}\nFROM {from}"))
    }

    fn gen_filter(&self, f: &Filter) -> Result<String> {
        let from = self.gen_from(&f.input)?;
        let cond = self.gen_expr(&f.condition)?;
        Ok(format!("SELECT *\nFROM {from}\nWHERE {cond}"))
    }

    fn gen_aggregate(&self, a: &Aggregate) -> Result<String> {
        // Build SELECT list
        let select_list = if a.select_order.is_empty() {
            // Default: grouping columns first, then aggregates
            let mut parts = Vec::new();
            for g in &a.grouping {
                parts.push(self.gen_expr(g)?);
            }
            for ae in &a.aggregates {
                let mut s = self.gen_expr(&ae.func)?;
                if ae.is_distinct {
                    // Wrap with DISTINCT inside the aggregate
                    s = inject_distinct(s);
                }
                if let Some(filter) = &ae.filter {
                    let f = self.gen_expr(filter)?;
                    s = format!("{s} FILTER (WHERE {f})");
                }
                parts.push(s);
            }
            parts.join(", ")
        } else {
            let mut parts = Vec::new();
            for entry in &a.select_order {
                match entry {
                    SelectEntry::GroupingExpr(g) => parts.push(self.gen_expr(g)?),
                    SelectEntry::AggregateExpr(idx) => {
                        let ae = &a.aggregates[*idx];
                        let mut s = self.gen_expr(&ae.func)?;
                        if ae.is_distinct {
                            s = inject_distinct(s);
                        }
                        if let Some(filter) = &ae.filter {
                            let f = self.gen_expr(filter)?;
                            s = format!("{s} FILTER (WHERE {f})");
                        }
                        parts.push(s);
                    }
                }
            }
            parts.join(", ")
        };

        let from = self.gen_from(&a.input)?;
        let mut sql = format!("SELECT {select_list}\nFROM {from}");

        // GROUP BY
        if !a.grouping.is_empty() || a.grouping_sets.is_some() {
            if let Some(gs) = &a.grouping_sets {
                sql.push_str(&format!("\nGROUP BY {}", self.gen_grouping_sets(gs)?));
            } else {
                let gb = a.grouping.iter()
                    .map(|g| self.gen_expr(g))
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
        let left = self.gen_from(&j.left)?;
        let right = self.gen_from(&j.right)?;
        let kw = j.join_type.sql_keyword();

        if j.join_type.is_semi_or_anti() {
            // DuckDB SEMI/ANTI JOIN syntax
            let cond = match &j.condition {
                Some(c) => format!(" ON {}", self.gen_expr(c)?),
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
        } else if let Some(cond) = &j.condition {
            format!(" ON {}", self.gen_expr(cond)?)
        } else {
            String::new() // CROSS JOIN
        };

        Ok(format!("{left}\n{kw} {right}{join_clause}"))
    }

    fn gen_sort(&self, s: &Sort) -> Result<String> {
        let from = self.gen_from(&s.input)?;
        let order = s.order.iter()
            .map(|o| self.gen_sort_order(o))
            .collect::<Result<Vec<_>>>()?
            .join(", ");

        let mut sql = format!("SELECT *\nFROM {from}\nORDER BY {order}");

        if let Some(limit) = &s.limit {
            sql.push_str(&format!("\nLIMIT {}", self.gen_expr(limit)?));
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
        let left = self.gen_plan(&u.left)?;
        let right = self.gen_plan(&u.right)?;
        let kw = if u.all { "UNION ALL" } else { "UNION" };
        Ok(format!("({left})\n{kw}\n({right})"))
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
        Ok(format!("SELECT DISTINCT *\nFROM {from}"))
    }

    fn gen_sample(&self, s: &Sample) -> Result<String> {
        let from = self.gen_from(&s.input)?;
        let pct = s.fraction * 100.0;
        // DuckDB: TABLESAMPLE BERNOULLI(pct PERCENT) REPEATABLE(seed)
        let seed_clause = match s.seed {
            Some(seed) => format!(" REPEATABLE({seed})"),
            None => String::new(),
        };
        let method = if s.with_replacement { "SYSTEM" } else { "BERNOULLI" };
        Ok(format!("SELECT * FROM {from} TABLESAMPLE {method}({pct:.4} PERCENT){seed_clause}"))
    }

    fn gen_table_scan(&self, ts: &TableScan) -> Result<String> {
        let tbl = quote_ident(&ts.table);
        match &ts.alias {
            Some(a) => Ok(format!("{tbl} AS {}", quote_ident(a))),
            None => Ok(tbl),
        }
    }

    fn gen_sql_relation(&self, sr: &SqlRelation) -> Result<String> {
        Ok(format!("({})", sr.sql))
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
        // Phase 1: schema only, no data — produce empty relation
        if ldr.schema.fields.is_empty() {
            return Ok("(SELECT 1 WHERE FALSE)".to_string());
        }
        let cols = ldr.schema.fields.iter()
            .map(|f| {
                let dt = TypeMapper::to_duckdb(&f.data_type);
                format!("CAST(NULL AS {dt}) AS {}", quote_ident(&f.name))
            })
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!("(SELECT {cols} WHERE FALSE)"))
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
        // Translate to SELECT *, expr AS col, ... FROM input
        // For existing columns being replaced, they should appear in order.
        // Simplest: SELECT * REPLACE (expr AS col, ...) FROM input — DuckDB supports this.
        let from = self.gen_from(&wc.input)?;
        let replacements = wc.columns.iter()
            .map(|(name, expr)| {
                let e = self.gen_expr(expr)?;
                Ok(format!("{e} AS {}", quote_ident(name)))
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        Ok(format!("SELECT * REPLACE ({replacements})\nFROM {from}"))
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
        // Build a map of col_name → fill_literal for fast lookup
        let fill_map: std::collections::HashMap<&str, &crate::expression::Literal> =
            n.values.iter().map(|(col, lit)| (col.as_str(), lit)).collect();
        let select_parts: Vec<String> = n
            .all_columns
            .iter()
            .map(|col| {
                let qname = quote_ident(col);
                if let Some(lit) = fill_map.get(col.as_str()) {
                    let lit_sql = self.gen_expr(&crate::expression::Expression::Literal((*lit).clone())).unwrap_or_else(|_| "NULL".to_string());
                    format!("COALESCE({qname}, {lit_sql}) AS {qname}")
                } else {
                    qname
                }
            })
            .collect();
        Ok(format!("SELECT {}\nFROM {from}", select_parts.join(", ")))
    }

    fn gen_na_replace(&self, n: &NAReplace) -> Result<String> {
        let from = self.gen_from(&n.input)?;
        // Group replacements by column
        let mut col_replacements: std::collections::HashMap<&str, Vec<(&crate::expression::Literal, &crate::expression::Literal)>> = std::collections::HashMap::new();
        for (col, from_val, to_val) in &n.replacements {
            col_replacements.entry(col.as_str()).or_default().push((from_val, to_val));
        }
        let replacement_cols: std::collections::HashSet<&str> = col_replacements.keys().copied().collect();
        let select_parts: Vec<String> = n
            .all_columns
            .iter()
            .map(|col| {
                let qname = quote_ident(col);
                if let Some(repls) = col_replacements.get(col.as_str()) {
                    // Build CASE WHEN col = from THEN to ... ELSE col END
                    let when_clauses: Vec<String> = repls
                        .iter()
                        .map(|(from_lit, to_lit)| {
                            let from_sql = self.gen_expr(&crate::expression::Expression::Literal((*from_lit).clone())).unwrap_or_else(|_| "NULL".to_string());
                            let to_sql = self.gen_expr(&crate::expression::Expression::Literal((*to_lit).clone())).unwrap_or_else(|_| "NULL".to_string());
                            format!("WHEN {qname} = {from_sql} THEN {to_sql}")
                        })
                        .collect();
                    format!("CASE {} ELSE {qname} END AS {qname}", when_clauses.join(" "))
                } else {
                    qname
                }
            })
            .collect();
        let _ = replacement_cols; // suppress unused warning
        Ok(format!("SELECT {}\nFROM {from}", select_parts.join(", ")))
    }

    fn gen_unpivot(&self, u: &Unpivot) -> Result<String> {
        let inner = self.gen_plan(&u.input)?;
        let value_cols = u.values.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ");
        let var_col = quote_ident(&u.variable_column_name);
        let val_col = quote_ident(&u.value_column_name);
        let include = if u.include_nulls { " INCLUDE NULLS" } else { "" };
        Ok(format!(
            "UNPIVOT{include} (\n{inner}\n)\nON {value_cols}\nINTO NAME {var_col} VALUE {val_col}"
        ))
    }

    // ── FROM clause helpers ────────────────────────────────────────────────────

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
                // Inner might be a raw relation or a subquery
                let inner = match ar.input.as_ref() {
                    LogicalPlan::TableScan(ts) => self.gen_table_scan(ts)?,
                    LogicalPlan::InMemoryRelation(imr) => self.gen_in_memory_relation(imr)?,
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
            LogicalPlan::Join(j) => self.gen_join(j),
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
            .map(|e| self.gen_expr(e))
            .collect::<Result<Vec<_>>>()
            .map(|v| v.join(", "))
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
                    Ok(format!("{}.{}", quote_ident(q), quote_ident(&u.name)))
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
            Expression::RawSql(r) => Ok(r.sql.clone()),
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
        }
    }

    fn gen_binary(&self, b: &BinaryExpression) -> Result<String> {
        let left = self.gen_expr_paren(&b.left, b.op.precedence())?;
        let right = self.gen_expr_paren(&b.right, b.op.precedence())?;
        Ok(format!("{left} {} {right}", b.op.symbol()))
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
        let arg_sqls: Vec<String> = f.args.iter()
            .map(|a| self.gen_expr(a))
            .collect::<Result<Vec<_>>>()?;

        let arg_refs: Vec<&str> = arg_sqls.iter().map(|s| s.as_str()).collect();

        // Infer argument types for polymorphic dispatch.
        // We use an empty schema as a sentinel: ColumnReferences that carry
        // an embedded DataType (i.e. already resolved) will return their type
        // directly; unresolved references fall back to DataType::Unresolved,
        // which causes translate_typed to fall through to the non-polymorphic path.
        let empty_schema = StructType::empty();
        let arg_types: Vec<DataType> =
            f.args.iter().map(|a| a.data_type(&empty_schema)).collect();

        // Route through FunctionRegistry for Spark→DuckDB translation
        let translated =
            FunctionRegistry::translate_typed(&f.name, &arg_refs, &arg_types, self.mode);

        if f.distinct {
            // Inject DISTINCT inside the outermost function call
            Ok(inject_distinct(translated))
        } else {
            Ok(translated)
        }
    }

    fn gen_cast(&self, c: &CastExpression) -> Result<String> {
        let expr = self.gen_expr(&c.expr)?;
        let ty = TypeMapper::to_duckdb(&c.to_type);
        if c.try_cast {
            Ok(format!("TRY_CAST({expr} AS {ty})"))
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
        let func = self.gen_expr(&w.func)?;

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
        // Check if extraction is a string literal — use bracket-quoted form
        if let Expression::Literal(lit) = ev.extraction.as_ref() {
            if let LiteralValue::String(s) = &lit.value {
                return Ok(format!("{child}['{}']", s.replace('\'', "''")));
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

/// Inject DISTINCT into a function call SQL string.
/// e.g. `COUNT(x)` → `COUNT(DISTINCT x)`
fn inject_distinct(s: String) -> String {
    if let Some(pos) = s.find('(') {
        let (before, after) = s.split_at(pos + 1);
        format!("{before}DISTINCT {after}")
    } else {
        s
    }
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
        assert_eq!(sql, r#""arr"[1]"#, "got: {sql}");
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
}
