//! Pre-rendering AST normalisation pass for the SQL generator.
//!
//! Applies structural rewrites to a `LogicalPlan` tree that simplify SQL
//! generation.  All transformations are semantics-preserving and
//! mode-independent (they do not depend on `CompatMode`).
//!
//! Currently implemented:
//! - **Filter-stack flattening**: collapses `Filter(Filter(...(base)))` into a
//!   single `Filter` with conditions ANDed together via
//!   `Expression::Binary(BinaryExpression { op: BinaryOp::And, .. })`.
//!
//! **Limitation**: Normalisation recurses into `LogicalPlan` children but does
//! not recurse into `Expression` fields. Subquery plans embedded in expressions
//! (`InSubquery`, `ExistsSubquery`, `ScalarSubquery`) are not normalised. In
//! practice stacked filters inside subquery expressions are extremely unlikely
//! and the rendering path handles them correctly (just less optimally).

use crate::expression::{BinaryExpression, BinaryOp, Expression};
use crate::logical::{
    Aggregate, AliasedRelation, ApproxQuantile, Describe, Distinct, DropColumns, Except, Filter,
    Intersect, Join, Limit, LogicalPlan, NADrop, NAFill, NAReplace, Pivot, Project, Sample,
    ShowString, Sort, StatCorr, StatCov, StatCrosstab, StatFreqItems, StatSampleBy, Summary, Tail,
    ToDataFrame, Union, Unpivot, WithColumns, WithCte,
};

/// Returns `true` if the plan tree contains at least one stacked `Filter(Filter(...))`.
/// Used to skip the full-tree clone when no normalisation is needed.
pub(crate) fn needs_normalization(plan: &LogicalPlan) -> bool {
    match plan {
        // Core check: Filter whose input is also a Filter.
        LogicalPlan::Filter(f) => {
            if matches!(&*f.input, LogicalPlan::Filter(_)) {
                return true;
            }
            needs_normalization(&f.input)
        }

        // -- Single-child variants --
        LogicalPlan::Project(p) => needs_normalization(&p.input),
        LogicalPlan::Aggregate(a) => needs_normalization(&a.input),
        LogicalPlan::Sort(s) => needs_normalization(&s.input),
        LogicalPlan::Limit(l) => needs_normalization(&l.input),
        LogicalPlan::Tail(t) => needs_normalization(&t.input),
        LogicalPlan::Distinct(d) => needs_normalization(&d.input),
        LogicalPlan::Sample(s) => needs_normalization(&s.input),
        LogicalPlan::WithColumns(wc) => needs_normalization(&wc.input),
        LogicalPlan::AliasedRelation(ar) => needs_normalization(&ar.input),
        LogicalPlan::ToDataFrame(t) => needs_normalization(&t.input),
        LogicalPlan::DropColumns(d) => needs_normalization(&d.input),
        LogicalPlan::ShowString(s) => needs_normalization(&s.input),
        LogicalPlan::NADrop(n) => needs_normalization(&n.input),
        LogicalPlan::NAFill(n) => needs_normalization(&n.input),
        LogicalPlan::NAReplace(n) => needs_normalization(&n.input),
        LogicalPlan::Unpivot(u) => needs_normalization(&u.input),
        LogicalPlan::Pivot(p) => needs_normalization(&p.input),
        LogicalPlan::StatCov(s) => needs_normalization(&s.input),
        LogicalPlan::StatCorr(s) => needs_normalization(&s.input),
        LogicalPlan::ApproxQuantile(aq) => needs_normalization(&aq.input),
        LogicalPlan::StatCrosstab(s) => needs_normalization(&s.input),
        LogicalPlan::StatFreqItems(s) => needs_normalization(&s.input),
        LogicalPlan::StatSampleBy(s) => needs_normalization(&s.input),
        LogicalPlan::Describe(d) => needs_normalization(&d.input),
        LogicalPlan::Summary(s) => needs_normalization(&s.input),

        // -- Two-child variants (short-circuit on first match) --
        LogicalPlan::Join(j) => needs_normalization(&j.left) || needs_normalization(&j.right),
        LogicalPlan::Union(u) => needs_normalization(&u.left) || needs_normalization(&u.right),
        LogicalPlan::Except(e) => needs_normalization(&e.left) || needs_normalization(&e.right),
        LogicalPlan::Intersect(i) => needs_normalization(&i.left) || needs_normalization(&i.right),

        // -- WithCte: input + CTE plans --
        LogicalPlan::WithCte(w) => {
            needs_normalization(&w.input)
                || w.ctes.iter().any(|(_, plan)| needs_normalization(plan))
        }

        // -- Leaf nodes: no children --
        LogicalPlan::TableScan(_)
        | LogicalPlan::SqlRelation(_)
        | LogicalPlan::LocalRelation(_)
        | LogicalPlan::LocalDataRelation(_)
        | LogicalPlan::RangeRelation(_)
        | LogicalPlan::InMemoryRelation(_)
        | LogicalPlan::DdlStatement(_)
        | LogicalPlan::SingleRow(_) => false,
    }
}

/// Normalise a `LogicalPlan` tree for rendering.
///
/// Applies structural rewrites that simplify SQL generation:
/// - Filter-stack flattening: collapses `Filter(Filter(base))` into a single
///   `Filter` with conditions ANDed together.
///
/// Takes a borrow and returns an owned plan.  Unchanged subtrees are cloned
/// when a parent node is rewritten.
///
/// Invariant: `normalize(plan).infer_schema() == plan.infer_schema()` for all
/// plans.
pub(crate) fn normalize(plan: &LogicalPlan) -> LogicalPlan {
    match plan {
        LogicalPlan::Filter(_) => normalize_filter_stack(plan),

        // -- Variants with a single `input` child --
        LogicalPlan::Project(p) => LogicalPlan::Project(Project {
            input: Box::new(normalize(&p.input)),
            projections: p.projections.clone(),
        }),
        LogicalPlan::Aggregate(a) => LogicalPlan::Aggregate(Aggregate {
            input: Box::new(normalize(&a.input)),
            grouping: a.grouping.clone(),
            aggregates: a.aggregates.clone(),
            having: a.having.clone(),
            grouping_sets: a.grouping_sets.clone(),
            select_order: a.select_order.clone(),
        }),
        LogicalPlan::Sort(s) => LogicalPlan::Sort(Sort {
            input: Box::new(normalize(&s.input)),
            order: s.order.clone(),
            limit: s.limit.clone(),
            offset: s.offset.clone(),
        }),
        LogicalPlan::Limit(l) => LogicalPlan::Limit(Limit {
            input: Box::new(normalize(&l.input)),
            limit: l.limit.clone(),
        }),
        LogicalPlan::Tail(t) => LogicalPlan::Tail(Tail {
            input: Box::new(normalize(&t.input)),
            limit: t.limit.clone(),
        }),
        LogicalPlan::Distinct(d) => LogicalPlan::Distinct(Distinct {
            input: Box::new(normalize(&d.input)),
            columns: d.columns.clone(),
        }),
        LogicalPlan::Sample(s) => LogicalPlan::Sample(Sample {
            input: Box::new(normalize(&s.input)),
            fraction: s.fraction,
            seed: s.seed,
            with_replacement: s.with_replacement,
        }),
        LogicalPlan::WithColumns(wc) => LogicalPlan::WithColumns(WithColumns {
            input: Box::new(normalize(&wc.input)),
            columns: wc.columns.clone(),
        }),
        LogicalPlan::AliasedRelation(ar) => LogicalPlan::AliasedRelation(AliasedRelation {
            input: Box::new(normalize(&ar.input)),
            alias: ar.alias.clone(),
            column_aliases: ar.column_aliases.clone(),
        }),
        LogicalPlan::ToDataFrame(t) => LogicalPlan::ToDataFrame(ToDataFrame {
            input: Box::new(normalize(&t.input)),
            column_names: t.column_names.clone(),
        }),
        LogicalPlan::DropColumns(d) => LogicalPlan::DropColumns(DropColumns {
            input: Box::new(normalize(&d.input)),
            column_names: d.column_names.clone(),
        }),
        LogicalPlan::ShowString(s) => LogicalPlan::ShowString(ShowString {
            input: Box::new(normalize(&s.input)),
            num_rows: s.num_rows,
            truncate: s.truncate,
            vertical: s.vertical,
        }),
        LogicalPlan::NADrop(n) => LogicalPlan::NADrop(NADrop {
            input: Box::new(normalize(&n.input)),
            how: n.how.clone(),
            threshold: n.threshold,
            cols: n.cols.clone(),
        }),
        LogicalPlan::NAFill(n) => LogicalPlan::NAFill(NAFill {
            input: Box::new(normalize(&n.input)),
            values: n.values.clone(),
            all_columns: n.all_columns.clone(),
        }),
        LogicalPlan::NAReplace(n) => LogicalPlan::NAReplace(NAReplace {
            input: Box::new(normalize(&n.input)),
            replacements: n.replacements.clone(),
            all_columns: n.all_columns.clone(),
        }),
        LogicalPlan::Unpivot(u) => LogicalPlan::Unpivot(Unpivot {
            input: Box::new(normalize(&u.input)),
            ids: u.ids.clone(),
            values: u.values.clone(),
            variable_column_name: u.variable_column_name.clone(),
            value_column_name: u.value_column_name.clone(),
            include_nulls: u.include_nulls,
        }),
        LogicalPlan::Pivot(p) => LogicalPlan::Pivot(Pivot {
            input: Box::new(normalize(&p.input)),
            grouping: p.grouping.clone(),
            pivot_col: p.pivot_col.clone(),
            pivot_values: p.pivot_values.clone(),
            aggregates: p.aggregates.clone(),
        }),
        LogicalPlan::StatCov(s) => LogicalPlan::StatCov(StatCov {
            input: Box::new(normalize(&s.input)),
            col1: s.col1.clone(),
            col2: s.col2.clone(),
        }),
        LogicalPlan::StatCorr(s) => LogicalPlan::StatCorr(StatCorr {
            input: Box::new(normalize(&s.input)),
            col1: s.col1.clone(),
            col2: s.col2.clone(),
            method: s.method.clone(),
        }),
        LogicalPlan::ApproxQuantile(aq) => LogicalPlan::ApproxQuantile(ApproxQuantile {
            input: Box::new(normalize(&aq.input)),
            cols: aq.cols.clone(),
            probabilities: aq.probabilities.clone(),
            relative_error: aq.relative_error,
        }),
        LogicalPlan::StatCrosstab(s) => LogicalPlan::StatCrosstab(StatCrosstab {
            input: Box::new(normalize(&s.input)),
            col1: s.col1.clone(),
            col2: s.col2.clone(),
        }),
        LogicalPlan::StatFreqItems(s) => LogicalPlan::StatFreqItems(StatFreqItems {
            input: Box::new(normalize(&s.input)),
            cols: s.cols.clone(),
            support: s.support,
        }),
        LogicalPlan::StatSampleBy(s) => LogicalPlan::StatSampleBy(StatSampleBy {
            input: Box::new(normalize(&s.input)),
            col_expr: s.col_expr.clone(),
            fractions: s.fractions.clone(),
            seed: s.seed,
        }),
        LogicalPlan::Describe(d) => LogicalPlan::Describe(Describe {
            input: Box::new(normalize(&d.input)),
            cols: d.cols.clone(),
        }),
        LogicalPlan::Summary(s) => LogicalPlan::Summary(Summary {
            input: Box::new(normalize(&s.input)),
            statistics: s.statistics.clone(),
            cols: s.cols.clone(),
        }),

        // -- Variants with two children --
        LogicalPlan::Join(j) => LogicalPlan::Join(Join {
            left: Box::new(normalize(&j.left)),
            right: Box::new(normalize(&j.right)),
            join_type: j.join_type.clone(),
            condition: j.condition.clone(),
            using_columns: j.using_columns.clone(),
            left_alias: j.left_alias.clone(),
            right_alias: j.right_alias.clone(),
            left_plan_ids: j.left_plan_ids.clone(),
            right_plan_ids: j.right_plan_ids.clone(),
        }),
        LogicalPlan::Union(u) => LogicalPlan::Union(Union {
            left: Box::new(normalize(&u.left)),
            right: Box::new(normalize(&u.right)),
            all: u.all,
        }),
        LogicalPlan::Except(e) => LogicalPlan::Except(Except {
            left: Box::new(normalize(&e.left)),
            right: Box::new(normalize(&e.right)),
            all: e.all,
        }),
        LogicalPlan::Intersect(i) => LogicalPlan::Intersect(Intersect {
            left: Box::new(normalize(&i.left)),
            right: Box::new(normalize(&i.right)),
            all: i.all,
        }),

        // -- WithCte: input + CTE plans --
        LogicalPlan::WithCte(w) => LogicalPlan::WithCte(WithCte {
            ctes: w
                .ctes
                .iter()
                .map(|(name, plan)| (name.clone(), Box::new(normalize(plan))))
                .collect(),
            input: Box::new(normalize(&w.input)),
        }),

        // -- Leaf nodes: no children to recurse into --
        LogicalPlan::TableScan(_)
        | LogicalPlan::SqlRelation(_)
        | LogicalPlan::LocalRelation(_)
        | LogicalPlan::LocalDataRelation(_)
        | LogicalPlan::RangeRelation(_)
        | LogicalPlan::InMemoryRelation(_)
        | LogicalPlan::DdlStatement(_)
        | LogicalPlan::SingleRow(_) => plan.clone(),
    }
}

/// Flatten a stack of `Filter` nodes into a single `Filter` with all
/// conditions ANDed together.
///
/// Conditions are collected innermost-first (the bottom-most filter's
/// condition becomes the leftmost operand in the AND chain) and combined
/// into a left-associative AND tree.
fn normalize_filter_stack(plan: &LogicalPlan) -> LogicalPlan {
    let mut conditions: Vec<Expression> = Vec::new();
    let mut cur = plan;
    while let LogicalPlan::Filter(f) = cur {
        conditions.push(f.condition.clone());
        cur = &f.input;
    }
    // Recurse into the non-Filter base.
    let base = normalize(cur);

    // Conditions were collected outermost-first. Reverse so innermost is first
    // (matching the original evaluation order).
    conditions.reverse();

    // Combine into a left-associative AND chain.
    let combined = conditions
        .into_iter()
        .reduce(|acc, cond| {
            Expression::Binary(BinaryExpression {
                op: BinaryOp::And,
                left: Box::new(acc),
                right: Box::new(cond),
            })
        })
        // Invariant: we only enter this function when `plan` is a Filter,
        // so `conditions` has at least one element.
        .expect("normalize_filter_stack called with at least one Filter");

    LogicalPlan::Filter(Filter {
        input: Box::new(base),
        condition: combined,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::{ColumnReference, Literal};
    use crate::logical::{SingleRowRelation, TableScan};

    fn table(name: &str) -> LogicalPlan {
        LogicalPlan::TableScan(TableScan {
            table: name.to_owned(),
            alias: None,
            schema: Default::default(),
        })
    }

    fn col(name: &str) -> Expression {
        ColumnReference::untyped(name)
    }

    fn filter(input: LogicalPlan, cond: Expression) -> LogicalPlan {
        LogicalPlan::Filter(Filter {
            input: Box::new(input),
            condition: cond,
        })
    }

    fn eq_expr(left: Expression, right: Expression) -> Expression {
        Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    fn and_expr(left: Expression, right: Expression) -> Expression {
        Expression::Binary(BinaryExpression {
            op: BinaryOp::And,
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    #[test]
    fn normalize_single_filter_unchanged() {
        let cond = eq_expr(col("x"), Literal::int(1));
        let plan = filter(table("t"), cond.clone());

        let result = normalize(&plan);

        // Single filter should remain as-is (with recursed base).
        match &result {
            LogicalPlan::Filter(f) => {
                assert_eq!(f.condition, cond);
                assert!(matches!(&*f.input, LogicalPlan::TableScan(_)));
            }
            other => panic!("expected Filter, got {other:?}"),
        }
    }

    #[test]
    fn normalize_stacked_filters_flattened_to_single() {
        // Filter(cond3, Filter(cond2, Filter(cond1, table)))
        let cond1 = eq_expr(col("a"), Literal::int(1));
        let cond2 = eq_expr(col("b"), Literal::int(2));
        let cond3 = eq_expr(col("c"), Literal::int(3));

        let plan = filter(
            filter(filter(table("t"), cond1.clone()), cond2.clone()),
            cond3.clone(),
        );

        let result = normalize(&plan);

        // Should be a single Filter with (cond1 AND cond2) AND cond3.
        match &result {
            LogicalPlan::Filter(f) => {
                assert!(matches!(&*f.input, LogicalPlan::TableScan(_)));
                // Left-associative: ((cond1 AND cond2) AND cond3)
                let expected = and_expr(and_expr(cond1, cond2), cond3);
                assert_eq!(f.condition, expected);
            }
            other => panic!("expected Filter, got {other:?}"),
        }
    }

    #[test]
    fn normalize_two_stacked_filters_flattened() {
        let cond1 = eq_expr(col("x"), Literal::int(10));
        let cond2 = eq_expr(col("y"), Literal::int(20));
        let plan = filter(filter(table("t"), cond1.clone()), cond2.clone());

        let result = normalize(&plan);

        match &result {
            LogicalPlan::Filter(f) => {
                assert!(matches!(&*f.input, LogicalPlan::TableScan(_)));
                let expected = and_expr(cond1, cond2);
                assert_eq!(f.condition, expected);
            }
            other => panic!("expected Filter, got {other:?}"),
        }
    }

    #[test]
    fn normalize_recurses_into_project_child() {
        // Project(Filter(Filter(table)))
        let cond1 = eq_expr(col("a"), Literal::int(1));
        let cond2 = eq_expr(col("b"), Literal::int(2));
        let inner = filter(filter(table("t"), cond1.clone()), cond2.clone());
        let plan = LogicalPlan::Project(Project {
            input: Box::new(inner),
            projections: vec![col("a")],
        });

        let result = normalize(&plan);

        match &result {
            LogicalPlan::Project(p) => {
                // The nested filter stack should be flattened.
                match &*p.input {
                    LogicalPlan::Filter(f) => {
                        assert!(matches!(&*f.input, LogicalPlan::TableScan(_)));
                        let expected = and_expr(cond1, cond2);
                        assert_eq!(f.condition, expected);
                    }
                    other => panic!("expected Filter in Project input, got {other:?}"),
                }
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn normalize_recurses_into_join_children() {
        let cond_l = eq_expr(col("a"), Literal::int(1));
        let cond_r = eq_expr(col("b"), Literal::int(2));
        let left = filter(filter(table("l"), cond_l.clone()), cond_l.clone());
        let right = filter(table("r"), cond_r.clone());

        let plan = LogicalPlan::Join(Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type: crate::logical::JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_alias: None,
            right_alias: None,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });

        let result = normalize(&plan);

        match &result {
            LogicalPlan::Join(j) => {
                // Left side: two filters flattened to one
                match &*j.left {
                    LogicalPlan::Filter(f) => {
                        assert!(matches!(&*f.input, LogicalPlan::TableScan(_)));
                        let expected = and_expr(cond_l.clone(), cond_l);
                        assert_eq!(f.condition, expected);
                    }
                    other => panic!("expected Filter on left, got {other:?}"),
                }
                // Right side: single filter unchanged
                match &*j.right {
                    LogicalPlan::Filter(f) => {
                        assert!(matches!(&*f.input, LogicalPlan::TableScan(_)));
                        assert_eq!(f.condition, cond_r);
                    }
                    other => panic!("expected Filter on right, got {other:?}"),
                }
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn normalize_leaf_node_unchanged() {
        let plan = table("t");
        let result = normalize(&plan);
        assert_eq!(result, plan);
    }

    #[test]
    fn normalize_single_row_unchanged() {
        let plan = LogicalPlan::SingleRow(SingleRowRelation);
        let result = normalize(&plan);
        assert_eq!(result, plan);
    }

    #[test]
    fn normalize_filter_over_join_flattened() {
        // Filter(cond, Join(left, right)) -- single filter over a join
        // This is the dominant pattern that caused the original extract_filters helper.
        let cond = eq_expr(col("x"), Literal::int(42));
        let join = LogicalPlan::Join(Join {
            left: Box::new(table("l")),
            right: Box::new(table("r")),
            join_type: crate::logical::JoinType::Inner,
            condition: Some(eq_expr(col("l.id"), col("r.id"))),
            using_columns: vec![],
            left_alias: None,
            right_alias: None,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = filter(join.clone(), cond.clone());

        let result = normalize(&plan);

        match &result {
            LogicalPlan::Filter(f) => {
                assert_eq!(f.condition, cond);
                assert!(matches!(&*f.input, LogicalPlan::Join(_)));
            }
            other => panic!("expected Filter, got {other:?}"),
        }
    }

    // ── needs_normalization tests ─────────────────────────────────────────────

    #[test]
    fn needs_normalization_leaf_returns_false() {
        assert!(!needs_normalization(&table("t")));
    }

    #[test]
    fn needs_normalization_single_filter_returns_false() {
        let plan = filter(table("t"), eq_expr(col("a"), Literal::int(1)));
        assert!(!needs_normalization(&plan));
    }

    #[test]
    fn needs_normalization_stacked_filters_returns_true() {
        let plan = filter(
            filter(table("t"), eq_expr(col("a"), Literal::int(1))),
            eq_expr(col("b"), Literal::int(2)),
        );
        assert!(needs_normalization(&plan));
    }

    #[test]
    fn needs_normalization_stacked_filters_nested_in_project_returns_true() {
        let inner = filter(
            filter(table("t"), eq_expr(col("a"), Literal::int(1))),
            eq_expr(col("b"), Literal::int(2)),
        );
        let plan = LogicalPlan::Project(Project {
            input: Box::new(inner),
            projections: vec![col("a")],
        });
        assert!(needs_normalization(&plan));
    }

    #[test]
    fn needs_normalization_no_stacked_filters_in_join_returns_false() {
        let plan = LogicalPlan::Join(Join {
            left: Box::new(filter(table("l"), eq_expr(col("a"), Literal::int(1)))),
            right: Box::new(table("r")),
            join_type: crate::logical::JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_alias: None,
            right_alias: None,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        assert!(!needs_normalization(&plan));
    }

    #[test]
    fn needs_normalization_stacked_filters_in_join_left_returns_true() {
        let stacked = filter(
            filter(table("l"), eq_expr(col("a"), Literal::int(1))),
            eq_expr(col("b"), Literal::int(2)),
        );
        let plan = LogicalPlan::Join(Join {
            left: Box::new(stacked),
            right: Box::new(table("r")),
            join_type: crate::logical::JoinType::Inner,
            condition: None,
            using_columns: vec![],
            left_alias: None,
            right_alias: None,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        assert!(needs_normalization(&plan));
    }

    #[test]
    fn needs_normalization_single_row_returns_false() {
        let plan = LogicalPlan::SingleRow(SingleRowRelation);
        assert!(!needs_normalization(&plan));
    }
}
