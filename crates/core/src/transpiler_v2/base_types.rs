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

use super::ast::{CommonAst, CommonOp};
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
}
