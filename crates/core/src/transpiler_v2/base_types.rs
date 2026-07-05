//! τ's `BaseTypes` overlay — the per-path lookup table for empty-schema
//! `TableScan` operands.
//!
//! **INV10:** this file imports ONLY value-level types from `crate::types`
//! (`StructType`) plus intra-τ modules. No `crate::runtime`,
//! `crate::types::TypeInferenceEngine`. Catalog lookup is **injected** as a
//! closure — the dispatch site (Slice A.3) constructs the closure that
//! bridges `DuckDbSession` into this overlay.
//!
//! **Short-circuit invariant (checklist §5.5):** `build_from_plan()` walks
//! the plan once via [`plan_has_empty_scan()`] and returns [`BaseTypes::empty()`]
//! without invoking the catalog closure when no empty-schema `TableScan`
//! exists. See the `build_from_plan_short_circuits_when_no_empty_scan` test.

use std::collections::HashMap;

use super::ast::{CommonAst, CommonOp, PivotGrouping};
use super::expression::{Expression, SubqueryPlan};
use crate::types::StructType;

/// A per-path overlay recording the resolved schemas of every empty-schema
/// `TableScan` that appears in a plan.
///
/// The overlay is intentionally cheap to construct when the plan has no
/// empty scans (the common case for file-based reads, which carry their
/// schema inline). See [`Self::build_from_plan`] for the short-circuit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BaseTypes {
    entries: HashMap<String, StructType>,
}

impl BaseTypes {
    /// Construct an empty overlay.
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Build an overlay by walking `plan` and resolving every empty-schema
    /// `TableScan` via `catalog_lookup`.
    ///
    /// **Short-circuit (checklist §5.5):** if `plan_has_empty_scan(plan)` is
    /// `false`, returns [`Self::empty()`] without invoking `catalog_lookup`
    /// even once — verified by the
    /// `build_from_plan_short_circuits_when_no_empty_scan` test.
    ///
    /// `catalog_lookup` is an injected closure — this keeps INV10 discipline
    /// (no `crate::runtime` import inside `transpiler_v2/`). The dispatch
    /// site is responsible for bridging the actual catalog.
    pub fn build_from_plan<F>(plan: &CommonAst, mut catalog_lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<StructType>,
    {
        if !plan_has_empty_scan(plan) {
            return Self::empty();
        }
        let mut tables: Vec<String> = Vec::new();
        collect_empty_scan_tables(plan, &mut tables);
        let mut entries = HashMap::new();
        for table in tables {
            if entries.contains_key(&table) {
                continue;
            }
            if let Some(schema) = catalog_lookup(&table) {
                entries.insert(table, schema);
            }
        }
        Self { entries }
    }

    /// Look up the resolved schema for `table`. Returns `None` when the
    /// overlay has no entry (either because the plan had no empty scan for
    /// this table or because the catalog closure returned `None`).
    pub fn lookup(&self, table: &str) -> Option<&StructType> {
        self.entries.get(table)
    }

    /// `true` iff no overlay entries were populated.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Walk `plan` and return `true` iff any `TableScan` has an empty implicit
/// schema requirement — the substrate distinguishes populated schemas from
/// empty ones at Slice B; A.2 treats every `TableScan` as "empty" (i.e. the
/// analyzer will need a catalog lookup) because the substrate does not yet
/// attach resolved schema to plan nodes.
///
/// Slice A.2 semantic: every `TableScan` is empty (the analyzer resolves
/// schemas). This is intentional — the overlay populates once per unique
/// table, and unpopulated tables carry no analyzer info, so a false positive
/// costs at most one closure invocation per unique table name.
pub fn plan_has_empty_scan(plan: &CommonAst) -> bool {
    // Seam D: a table referenced *only* inside a subquery (e.g. `dept` in
    // `... WHERE EXISTS (SELECT 1 FROM dept)`) must still trigger catalog
    // pre-fetch. Descend into this node's expressions first so the
    // short-circuit does not skip a plan whose only scans live in a subquery.
    if !node_expr_scan_tables(&plan.op).is_empty() {
        return true;
    }
    match &plan.op {
        CommonOp::TableScan { .. } => true,
        CommonOp::Project { input, .. }
        | CommonOp::Filter { input, .. }
        | CommonOp::Sort { input, .. }
        | CommonOp::Limit { input, .. }
        | CommonOp::Aggregate { input, .. }
        | CommonOp::WithColumns { input, .. }
        | CommonOp::DropColumns { input, .. }
        | CommonOp::AliasedRelation { input, .. }
        | CommonOp::WithColumnsRenamed { input, .. }
        | CommonOp::ToDf { input, .. }
        | CommonOp::Deduplicate { input, .. }
        | CommonOp::NaFill { input, .. }
        | CommonOp::NaDrop { input, .. }
        | CommonOp::NaReplace { input, .. }
        | CommonOp::Unpivot { input, .. }
        | CommonOp::Pivot { input, .. }
        | CommonOp::Describe { input, .. }
        | CommonOp::Summary { input, .. }
        | CommonOp::FreqItems { input, .. }
        | CommonOp::Crosstab { input, .. }
        | CommonOp::Sample { input, .. }
        | CommonOp::SampleBy { input, .. } => plan_has_empty_scan(input),
        CommonOp::Join { left, right, .. } => {
            plan_has_empty_scan(left) || plan_has_empty_scan(right)
        }
        CommonOp::SetOp { children, .. } => children.iter().any(plan_has_empty_scan),
        // Leaves other than TableScan carry their schema inline or resolve
        // via other channels — never empty for the purposes of this overlay.
        CommonOp::SingleRow
        | CommonOp::Values { .. }
        | CommonOp::LocalRelation { .. }
        | CommonOp::FileScan { .. }
        | CommonOp::TableFunction { .. }
        | CommonOp::Unnest { .. } => false,
    }
}

/// Enumerate every empty-scan `TableScan` table name in `plan`, in tree order
/// (may contain duplicates). Public so the request handler can pre-fetch
/// session-catalog schemas before building the sync overlay closure.
pub fn empty_scan_tables(plan: &CommonAst) -> Vec<String> {
    let mut out = Vec::new();
    collect_empty_scan_tables(plan, &mut out);
    out
}

/// Walk `plan` and push every `TableScan.table` name into `out`.
fn collect_empty_scan_tables(plan: &CommonAst, out: &mut Vec<String>) {
    // Seam D: collect tables referenced inside this node's subquery-bearing
    // expressions (their inner plans are `Unanalyzed` at collection time).
    out.extend(node_expr_scan_tables(&plan.op));
    match &plan.op {
        CommonOp::TableScan { table, .. } => {
            out.push(table.clone());
        }
        CommonOp::Project { input, .. }
        | CommonOp::Filter { input, .. }
        | CommonOp::Sort { input, .. }
        | CommonOp::Limit { input, .. }
        | CommonOp::Aggregate { input, .. }
        | CommonOp::WithColumns { input, .. }
        | CommonOp::DropColumns { input, .. }
        | CommonOp::AliasedRelation { input, .. }
        | CommonOp::WithColumnsRenamed { input, .. }
        | CommonOp::ToDf { input, .. }
        | CommonOp::Deduplicate { input, .. }
        | CommonOp::NaFill { input, .. }
        | CommonOp::NaDrop { input, .. }
        | CommonOp::NaReplace { input, .. }
        | CommonOp::Unpivot { input, .. }
        | CommonOp::Pivot { input, .. }
        | CommonOp::Describe { input, .. }
        | CommonOp::Summary { input, .. }
        | CommonOp::FreqItems { input, .. }
        | CommonOp::Crosstab { input, .. }
        | CommonOp::Sample { input, .. }
        | CommonOp::SampleBy { input, .. } => collect_empty_scan_tables(input, out),
        CommonOp::Join { left, right, .. } => {
            collect_empty_scan_tables(left, out);
            collect_empty_scan_tables(right, out);
        }
        CommonOp::SetOp { children, .. } => {
            for child in children {
                collect_empty_scan_tables(child, out);
            }
        }
        CommonOp::SingleRow
        | CommonOp::Values { .. }
        | CommonOp::LocalRelation { .. }
        | CommonOp::FileScan { .. }
        | CommonOp::TableFunction { .. }
        | CommonOp::Unnest { .. } => {}
    }
}

// ── Seam D: subquery-aware expression descent ────────────────────────────────

/// Collect every empty-scan table referenced inside the subquery-bearing
/// expressions carried directly by `op` (not its plan inputs — those are
/// handled by the operator walkers above).
fn node_expr_scan_tables(op: &CommonOp) -> Vec<String> {
    let mut out = Vec::new();
    for_each_node_expr(op, &mut |e| collect_scan_tables_in_expr(e, &mut out));
    out
}

/// Invoke `f` on every expression carried *directly* by `op`. Operators whose
/// only expressions are guaranteed literal (`LocalRelation`) or absent
/// (schema-only / string-list operators) fall through — none can host a
/// subquery.
fn for_each_node_expr(op: &CommonOp, f: &mut dyn FnMut(&Expression)) {
    match op {
        CommonOp::Project { projections, .. } => {
            for e in projections {
                f(e);
            }
        }
        CommonOp::Filter { condition, .. } => f(condition),
        CommonOp::Aggregate {
            grouping,
            aggregates,
            ..
        } => {
            for e in grouping.iter().chain(aggregates) {
                f(e);
            }
        }
        CommonOp::Sort { order, .. } => {
            for o in order {
                f(o.expr.as_ref());
            }
        }
        CommonOp::Join {
            condition: Some(c), ..
        } => f(c),
        CommonOp::Values { rows, .. } => {
            for e in rows.iter().flatten() {
                f(e);
            }
        }
        CommonOp::TableFunction { args, .. } => {
            for e in args {
                f(e);
            }
        }
        CommonOp::Unnest { expr, .. } => f(expr),
        CommonOp::NaFill { values, .. } => {
            for e in values {
                f(e);
            }
        }
        CommonOp::NaReplace { replacements, .. } => {
            for (a, b) in replacements {
                f(a);
                f(b);
            }
        }
        CommonOp::Pivot {
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
            ..
        } => {
            // Only explicit grouping carries expressions to walk; an implicit
            // (SQL PIVOT) grouping is derived from the schema by the analyzer.
            let grouping_exprs: &[Expression] = match grouping {
                PivotGrouping::Explicit(g) => g,
                PivotGrouping::Implicit => &[],
            };
            for e in grouping_exprs
                .iter()
                .chain(std::iter::once(pivot_column))
                .chain(pivot_values)
                .chain(aggregates)
            {
                f(e);
            }
        }
        CommonOp::WithColumns { assignments, .. } => {
            for (_, e) in assignments {
                f(e);
            }
        }
        CommonOp::SampleBy { col, .. } => f(col),
        // A join with no `ON` condition carries no direct expression.
        CommonOp::Join {
            condition: None, ..
        } => {}
        // Remaining variants carry no expression that can host a subquery:
        // their expressions are absent (schema-only / string-list operators)
        // or guaranteed literal (`LocalRelation` rows). Listed explicitly —
        // not `_` — so a future expression-bearing `CommonOp` variant fails
        // to compile here until its expressions are wired into the walk
        // (a subquery-only table in such a node would otherwise silently miss
        // base-type pre-fetch).
        CommonOp::Limit { .. }
        | CommonOp::SingleRow
        | CommonOp::TableScan { .. }
        | CommonOp::LocalRelation { .. }
        | CommonOp::FileScan { .. }
        | CommonOp::SetOp { .. }
        | CommonOp::NaDrop { .. }
        | CommonOp::Unpivot { .. }
        | CommonOp::Describe { .. }
        | CommonOp::Summary { .. }
        | CommonOp::FreqItems { .. }
        | CommonOp::Crosstab { .. }
        | CommonOp::Deduplicate { .. }
        | CommonOp::AliasedRelation { .. }
        | CommonOp::ToDf { .. }
        | CommonOp::WithColumnsRenamed { .. }
        | CommonOp::DropColumns { .. }
        | CommonOp::Sample { .. } => {}
    }
}

/// Walk expression `e` and, for every subquery variant (always `Unanalyzed`
/// at base-types collection time), collect its inner plan's empty-scan tables.
/// Recurses through every composite expression so a subquery nested inside a
/// binary op / CASE / function call is still reached.
fn collect_scan_tables_in_expr(e: &Expression, out: &mut Vec<String>) {
    match e {
        Expression::ScalarSubquery(s) => collect_subquery_plan_tables(&s.subquery, out),
        Expression::InSubquery(i) => {
            collect_scan_tables_in_expr(&i.expr, out);
            collect_subquery_plan_tables(&i.subquery, out);
        }
        Expression::ExistsSubquery(x) => collect_subquery_plan_tables(&x.subquery, out),
        Expression::Binary(b) => {
            collect_scan_tables_in_expr(&b.left, out);
            collect_scan_tables_in_expr(&b.right, out);
        }
        Expression::Unary(u) => collect_scan_tables_in_expr(&u.operand, out),
        Expression::FunctionCall(fc) => {
            fc.args
                .iter()
                .for_each(|a| collect_scan_tables_in_expr(a, out));
        }
        Expression::Cast(c) => collect_scan_tables_in_expr(&c.expr, out),
        Expression::CaseWhen(cw) => {
            for (w, t) in &cw.branches {
                collect_scan_tables_in_expr(w, out);
                collect_scan_tables_in_expr(t, out);
            }
            if let Some(else_expr) = &cw.else_expr {
                collect_scan_tables_in_expr(else_expr, out);
            }
        }
        Expression::Window(w) => {
            collect_scan_tables_in_expr(&w.func, out);
            w.partition_by
                .iter()
                .for_each(|e| collect_scan_tables_in_expr(e, out));
            w.order_by
                .iter()
                .for_each(|o| collect_scan_tables_in_expr(o.expr.as_ref(), out));
        }
        Expression::Alias(a) => collect_scan_tables_in_expr(&a.expr, out),
        Expression::Between(b) => {
            collect_scan_tables_in_expr(&b.expr, out);
            collect_scan_tables_in_expr(&b.low, out);
            collect_scan_tables_in_expr(&b.high, out);
        }
        Expression::InList(i) => {
            collect_scan_tables_in_expr(&i.expr, out);
            i.list
                .iter()
                .for_each(|e| collect_scan_tables_in_expr(e, out));
        }
        Expression::Like(l) => {
            collect_scan_tables_in_expr(&l.value, out);
            collect_scan_tables_in_expr(&l.pattern, out);
        }
        Expression::IsDistinctFrom(d) => {
            collect_scan_tables_in_expr(&d.left, out);
            collect_scan_tables_in_expr(&d.right, out);
        }
        Expression::ExtractValue(ev) => {
            collect_scan_tables_in_expr(&ev.child, out);
            collect_scan_tables_in_expr(&ev.extraction, out);
        }
        Expression::ArrayLiteral(a) => a
            .elements
            .iter()
            .for_each(|e| collect_scan_tables_in_expr(e, out)),
        Expression::MapLiteral(m) => {
            for (k, v) in &m.entries {
                collect_scan_tables_in_expr(k, out);
                collect_scan_tables_in_expr(v, out);
            }
        }
        Expression::StructLiteral(s) => s
            .fields
            .iter()
            .for_each(|(_, e)| collect_scan_tables_in_expr(e, out)),
        Expression::RowConstructor(rc) => rc
            .elements
            .iter()
            .for_each(|e| collect_scan_tables_in_expr(e, out)),
        Expression::UpdateFields(u) => {
            collect_scan_tables_in_expr(&u.struct_expr, out);
            for (_, upd) in &u.updates {
                if let Some(expr) = upd {
                    collect_scan_tables_in_expr(expr, out);
                }
            }
        }
        Expression::Lambda(l) => collect_scan_tables_in_expr(&l.body, out),
        // Leaves and no-sub-expression variants.
        Expression::Literal(_)
        | Expression::ColumnReference(_)
        | Expression::UnresolvedColumn(_)
        | Expression::UnresolvedRegex(_)
        | Expression::Star(_)
        | Expression::LambdaVariable(_)
        | Expression::RawSql(_)
        | Expression::Interval(_) => {}
    }
}

/// Collect empty-scan tables from a subquery's inner plan. At base-types
/// collection time the plan is always `Unanalyzed`; an `Analyzed` plan (would
/// only appear if collection ran post-analysis) needs no further pre-fetch.
fn collect_subquery_plan_tables(plan: &SubqueryPlan, out: &mut Vec<String>) {
    match plan {
        SubqueryPlan::Unanalyzed(inner) => collect_empty_scan_tables(inner, out),
        SubqueryPlan::Analyzed(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::super::expression::{Literal, LiteralValue};
    use super::*;
    use crate::transpiler_v2::ast::{CommonAst, CommonOp, JoinType};
    use crate::types::{DataType, StructField};

    fn table_scan(name: &str) -> CommonAst {
        CommonAst::new(CommonOp::TableScan {
            table: name.to_owned(),
            alias: None,
        })
    }

    fn single_int_literal() -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Int(1),
            data_type: DataType::Integer,
        })
    }

    // ExprAlias for test brevity.
    use super::super::expression::Expression;

    #[test]
    fn base_types_empty_lookup_returns_none() {
        let bt = BaseTypes::empty();
        assert!(bt.is_empty());
        assert!(bt.lookup("orders").is_none());
    }

    #[test]
    fn plan_has_empty_scan_true_for_bare_table_scan_with_empty_schema() {
        assert!(plan_has_empty_scan(&table_scan("orders")));
    }

    #[test]
    fn plan_has_empty_scan_false_for_project_over_populated_scan() {
        // §5.5 short-circuit anchor: a plan whose leaves are non-TableScan
        // (here: SingleRow) reports `false`.
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![single_int_literal()],
        });
        assert!(!plan_has_empty_scan(&plan));
    }

    #[test]
    fn plan_has_empty_scan_true_recursively_through_project_filter_sort_limit_aggregate() {
        let leaf = table_scan("orders");
        let with_project = CommonAst::new(CommonOp::Project {
            input: Box::new(leaf.clone()),
            projections: vec![single_int_literal()],
        });
        assert!(plan_has_empty_scan(&with_project));

        let with_filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(leaf.clone()),
            condition: single_int_literal(),
        });
        assert!(plan_has_empty_scan(&with_filter));

        let with_sort = CommonAst::new(CommonOp::Sort {
            input: Box::new(leaf.clone()),
            order: vec![],
            limit: None,
            offset: None,
        });
        assert!(plan_has_empty_scan(&with_sort));

        let with_limit = CommonAst::new(CommonOp::Limit {
            input: Box::new(leaf.clone()),
            limit: 10,
            offset: None,
        });
        assert!(plan_has_empty_scan(&with_limit));

        let with_agg = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(leaf),
            grouping: vec![],
            aggregates: vec![],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
        });
        assert!(plan_has_empty_scan(&with_agg));
    }

    #[test]
    fn plan_has_empty_scan_true_across_join_sides() {
        // Right side has a table scan, left does not — still true.
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::SingleRow)),
            right: Box::new(table_scan("orders")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        assert!(plan_has_empty_scan(&plan));
    }

    #[test]
    fn build_from_plan_short_circuits_when_no_empty_scan() {
        // Anchor for §5.5: closure MUST NOT be invoked when no TableScan.
        let calls = Cell::new(0u32);
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![single_int_literal()],
        });
        let bt = BaseTypes::build_from_plan(&plan, |_name| {
            calls.set(calls.get() + 1);
            None
        });
        assert_eq!(calls.get(), 0);
        assert!(bt.is_empty());
    }

    #[test]
    fn build_from_plan_calls_catalog_lookup_once_per_empty_scan_table() {
        let calls = Cell::new(0u32);
        // Same table twice → dedup after first insert.
        let scan1 = table_scan("orders");
        let scan2 = table_scan("orders");
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(scan1),
            right: Box::new(scan2),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let bt = BaseTypes::build_from_plan(&plan, |_name| {
            calls.set(calls.get() + 1);
            Some(schema.clone())
        });
        // Closure invoked once — cached on the second occurrence of "orders".
        assert_eq!(calls.get(), 1);
        assert!(!bt.is_empty());
        assert_eq!(bt.lookup("orders"), Some(&schema));
    }

    #[test]
    fn base_types_lookup_case_sensitive_matches_struct_type_semantics() {
        // BaseTypes uses HashMap<String, StructType> — case-sensitive by
        // construction. Callers that need case-insensitive matching must
        // canonicalize before insertion.
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let plan = table_scan("Orders");
        let bt = BaseTypes::build_from_plan(&plan, |name| {
            if name == "Orders" {
                Some(schema.clone())
            } else {
                None
            }
        });
        assert_eq!(bt.lookup("Orders"), Some(&schema));
        assert!(bt.lookup("orders").is_none());
    }

    // ── Pass 106 — Seam D: subquery-nested tables are collected ──────────

    #[test]
    fn collect_descends_into_in_subquery_over_dept() {
        use super::super::expression::InSubquery;
        // SELECT * FROM emp WHERE dept_id IN (SELECT dept_id FROM dept)
        let inner = table_scan("dept");
        let plan = CommonAst::new(CommonOp::Filter {
            input: Box::new(table_scan("emp")),
            condition: Expression::InSubquery(InSubquery {
                expr: Box::new(single_int_literal()),
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                negated: false,
            }),
        });
        let tables = empty_scan_tables(&plan);
        assert!(tables.contains(&"emp".to_owned()), "outer table collected");
        assert!(
            tables.contains(&"dept".to_owned()),
            "subquery-only table collected (Seam D)"
        );
    }

    #[test]
    fn plan_has_empty_scan_true_when_only_scan_is_in_subquery() {
        use super::super::expression::ScalarSubquery;
        // SELECT (SELECT id FROM dept) — outer has no FROM (SingleRow).
        let inner = table_scan("dept");
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        assert!(plan_has_empty_scan(&plan));
        assert_eq!(empty_scan_tables(&plan), vec!["dept".to_owned()]);
    }
}
