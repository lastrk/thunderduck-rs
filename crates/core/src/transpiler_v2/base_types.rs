//! τ's `BaseTypes` overlay — the per-path lookup table for empty-schema
//! `TableScan` operands.
//!
//! **INV10:** this file imports ONLY value-level types from `crate::types`
//! (`StructType`) plus intra-τ modules. No `crate::runtime`,
//! `crate::types::TypeInferenceEngine`. Catalog lookup is **injected** as a
//! closure — the dispatch site constructs the closure that
//! bridges `DuckDbSession` into this overlay.
//!
//! **Short-circuit invariant (checklist §5.5):** `build_from_plan()` walks
//! the plan once via [`empty_scan_tables()`] and never invokes the catalog
//! closure when no empty-schema `TableScan` exists. See the
//! `build_from_plan_short_circuits_when_no_empty_scan` test.

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
    /// **Short-circuit (checklist §5.5):** `catalog_lookup` is invoked at
    /// most once per unique table name collected by [`empty_scan_tables`] —
    /// a plan with no empty-schema `TableScan` never invokes it, verified by
    /// the `build_from_plan_short_circuits_when_no_empty_scan` test.
    ///
    /// `catalog_lookup` is an injected closure — this keeps INV10 discipline
    /// (no `crate::runtime` import inside `transpiler_v2/`). The dispatch
    /// site is responsible for bridging the actual catalog.
    pub fn build_from_plan<F>(plan: &CommonAst, mut catalog_lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<StructType>,
    {
        let mut entries = HashMap::new();
        for table in empty_scan_tables(plan) {
            if entries.contains_key(&table) {
                continue;
            }
            if let Some(schema) = catalog_lookup(&table) {
                entries.insert(table, schema);
            }
        }
        Self { entries }
    }

    /// Construct an overlay directly from pre-resolved `table → schema`
    /// entries. For callers (e.g. the connect-server request handler) that
    /// have already collected the plan's empty-scan tables via
    /// [`empty_scan_tables`] and resolved each schema through an async
    /// catalog — avoids [`Self::build_from_plan`]'s second plan walk.
    pub fn from_entries(entries: HashMap<String, StructType>) -> Self {
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

    /// Return a **new** overlay with an additional `(table, schema)` entry.
    ///
    /// If `table` already exists it is replaced (shadowing semantics — a CTE
    /// correctly shadows a catalog table of the same name per Spark). The
    /// receiver is `&self` + `Clone` — no mutation, no aliasing.
    pub fn with_entry(&self, table: &str, schema: StructType) -> Self {
        let mut entries = self.entries.clone();
        entries.insert(table.to_owned(), schema);
        Self { entries }
    }
}

/// Enumerate every empty-scan `TableScan` table name in `plan`, in tree order
/// (may contain duplicates). Public so the request handler can pre-fetch
/// session-catalog schemas and hand them to [`BaseTypes::from_entries`].
///
/// τ semantic: every `TableScan` is "empty" (the analyzer resolves schemas)
/// because the substrate does not yet attach resolved schema to plan nodes.
/// This is intentional — the overlay populates once per unique table, and
/// unpopulated tables carry no analyzer info, so a false positive costs at
/// most one closure invocation per unique table name.
pub fn empty_scan_tables(plan: &CommonAst) -> Vec<String> {
    let mut out = Vec::new();
    collect_empty_scan_tables(plan, &mut out);
    out
}

/// Walk `plan` and push every `TableScan.table` name into `out`.
fn collect_empty_scan_tables(plan: &CommonAst, out: &mut Vec<String>) {
    // Seam D: collect tables referenced inside this node's subquery-bearing
    // expressions (their inner plans are `Unanalyzed` at collection time) —
    // a table referenced *only* inside a subquery (e.g. `dept` in
    // `... WHERE EXISTS (SELECT 1 FROM dept)`) must still trigger catalog
    // pre-fetch.
    for_each_node_expr(&plan.op, &mut |e| collect_scan_tables_in_expr(e, out));
    if let CommonOp::TableScan { table, .. } = &plan.op {
        out.push(table.clone());
    }
    for child in plan.op.children() {
        collect_empty_scan_tables(child, out);
    }
}

// ── Seam D: subquery-aware expression descent ────────────────────────────────

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
            having,
            ..
        } => {
            for e in grouping.iter().chain(aggregates).chain(having.iter()) {
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
        CommonOp::LateralView { columns, .. } => {
            for (_, e) in columns {
                f(e);
            }
        }
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
        | CommonOp::Sample { .. }
        | CommonOp::RecursiveCte { .. } => {}
    }
}

/// Walk expression `e` and, for every subquery variant (always `Unanalyzed`
/// at base-types collection time), collect its inner plan's empty-scan tables.
/// Recurses through every composite expression via [`Expression::children`]
/// so a subquery nested inside a binary op / CASE / function call is still
/// reached.
///
/// MAINTENANCE CONTRACT: the subquery plans are opaque to
/// [`Expression::children`] (τ walker convention), so they need the custom
/// arms below. A future subquery-bearing variant added to `children()` as
/// opaque must be added here too — its subquery-only tables would otherwise
/// silently miss base-type pre-fetch.
fn collect_scan_tables_in_expr(e: &Expression, out: &mut Vec<String>) {
    match e {
        Expression::ScalarSubquery(s) => collect_subquery_plan_tables(&s.subquery, out),
        Expression::InSubquery(i) => {
            collect_scan_tables_in_expr(&i.expr, out);
            collect_subquery_plan_tables(&i.subquery, out);
        }
        Expression::ExistsSubquery(x) => collect_subquery_plan_tables(&x.subquery, out),
        _ => e
            .children()
            .for_each(|c| collect_scan_tables_in_expr(c, out)),
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
    fn empty_scan_tables_collects_bare_table_scan() {
        assert_eq!(
            empty_scan_tables(&table_scan("orders")),
            vec!["orders".to_owned()]
        );
    }

    #[test]
    fn empty_scan_tables_empty_for_project_over_populated_scan() {
        // §5.5 short-circuit anchor: a plan whose leaves are non-TableScan
        // (here: SingleRow) collects nothing.
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![single_int_literal()],
        });
        assert!(empty_scan_tables(&plan).is_empty());
    }

    #[test]
    fn empty_scan_tables_recurses_through_project_filter_sort_limit_aggregate() {
        let leaf = table_scan("orders");
        let with_project = CommonAst::new(CommonOp::Project {
            input: Box::new(leaf.clone()),
            projections: vec![single_int_literal()],
        });
        assert!(!empty_scan_tables(&with_project).is_empty());

        let with_filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(leaf.clone()),
            condition: single_int_literal(),
        });
        assert!(!empty_scan_tables(&with_filter).is_empty());

        let with_sort = CommonAst::new(CommonOp::Sort {
            input: Box::new(leaf.clone()),
            order: vec![],
            limit: None,
            offset: None,
        });
        assert!(!empty_scan_tables(&with_sort).is_empty());

        let with_limit = CommonAst::new(CommonOp::Limit {
            input: Box::new(leaf.clone()),
            limit: 10,
            offset: None,
        });
        assert!(!empty_scan_tables(&with_limit).is_empty());

        let with_agg = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(leaf),
            grouping: vec![],
            aggregates: vec![],
            projection: crate::transpiler_v2::ast::AggregateProjection::Folded,
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        assert!(!empty_scan_tables(&with_agg).is_empty());
    }

    #[test]
    fn empty_scan_tables_collects_across_join_sides() {
        // Right side has a table scan, left does not — still collected.
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::SingleRow)),
            right: Box::new(table_scan("orders")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        assert_eq!(empty_scan_tables(&plan), vec!["orders".to_owned()]);
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
            natural: false,
            lateral: false,
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
    fn empty_scan_tables_collects_when_only_scan_is_in_subquery() {
        use super::super::expression::ScalarSubquery;
        // SELECT (SELECT id FROM dept) — outer has no FROM (SingleRow).
        let inner = table_scan("dept");
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![Expression::ScalarSubquery(ScalarSubquery {
                subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
            })],
        });
        assert_eq!(empty_scan_tables(&plan), vec!["dept".to_owned()]);
    }
}
