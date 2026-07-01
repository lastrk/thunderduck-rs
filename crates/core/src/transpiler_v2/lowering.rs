//! `LogicalPlan → CommonAst` adapter (§Layer 2 of Slice C.1 architecture plan).
//!
//! Every one of the 29 [`crate::logical::LogicalPlan`] variants gets a
//! `match` arm. Variants covered by Slice B's [`crate::transpiler_v2::ast::CommonOp`]
//! surface map 1:1; everything else produces
//! [`crate::transpiler_v2::ast::CommonOp::Punt`] with a stable `kind` /
//! `reason`, which the analyzer rejects with
//! [`crate::transpiler_v2::analyzer::AnalyzerError::PuntedOperator`].
//! `service.rs`'s dispatch wrapper treats a punt as fallback-eligible and
//! falls back to the legacy path — do NOT raise here.

use crate::logical as legacy;
use crate::transpiler_v2::ast::{
    Aggregate, AggregateCall, AliasedRelation, CommonAst, CommonOp, Distinct, DropColumns, Except,
    Filter, Intersect, Join, JoinKind, Limit, LocalRelation, Project, RangeRelation, Sort,
    TableScan, Tail, Union, WithColumns,
};

/// Errors from lowering. A [`CommonOp::Punt`] is *not* an error — a
/// Punt is a valid `CommonOp`. Errors here are only structural (a legacy
/// aggregate expression the adapter cannot represent, etc.).
#[derive(thiserror::Error, Debug)]
pub enum LoweringError {
    /// A `LogicalPlan` expression list contained an unrepresentable shape.
    #[error("lowering: expression `{expr_kind}` is not adaptable in {parent_kind}: {reason}")]
    UnadaptableExpression {
        /// Diagnostic name of the parent operator.
        parent_kind: &'static str,
        /// Diagnostic name of the expression kind that failed.
        expr_kind: &'static str,
        /// Human-readable reason.
        reason: &'static str,
    },
}

/// Lower a legacy [`legacy::LogicalPlan`] into a [`CommonAst`].
///
/// See §Layer 2 of the architecture plan for the full 29-variant mapping.
/// Every variant not in Slice B's [`CommonOp`] surface produces
/// [`CommonOp::Punt`] with a stable `kind` and `reason` string.
pub fn lower(plan: &legacy::LogicalPlan) -> Result<CommonAst, LoweringError> {
    Ok(CommonAst {
        root: lower_op(plan)?,
    })
}

fn lower_op(plan: &legacy::LogicalPlan) -> Result<CommonOp, LoweringError> {
    use legacy::LogicalPlan as LP;
    match plan {
        LP::Project(p) => Ok(CommonOp::Project(Project {
            input: Box::new(lower_op(&p.input)?),
            projections: p.projections.clone(),
        })),
        LP::Filter(f) => Ok(CommonOp::Filter(Filter {
            input: Box::new(lower_op(&f.input)?),
            predicate: f.condition.clone(),
        })),
        LP::Aggregate(a) => lower_aggregate(a),
        LP::Join(j) => lower_join(j),
        LP::Sort(s) => Ok(CommonOp::Sort(Sort {
            input: Box::new(lower_op(&s.input)?),
            order: s.order.clone(),
            limit: s.limit.clone(),
            offset: s.offset.clone(),
        })),
        LP::Limit(l) => Ok(CommonOp::Limit(Limit {
            input: Box::new(lower_op(&l.input)?),
            n: l.limit.clone(),
        })),
        LP::Tail(t) => Ok(CommonOp::Tail(Tail {
            input: Box::new(lower_op(&t.input)?),
            n: t.limit.clone(),
        })),
        LP::Union(u) => Ok(CommonOp::Union(Union {
            left: Box::new(lower_op(&u.left)?),
            right: Box::new(lower_op(&u.right)?),
            all: u.all,
            // OQ1 (Open Question §Open questions in the architecture plan):
            // `legacy::Union` does not currently carry a `by_name` marker.
            // Slice C.1 defaults to `false`; a follow-up threading the
            // proto field through `PlanConverter` will flip this for
            // `unionByName` cases.
            by_name: false,
        })),
        LP::Except(e) => Ok(CommonOp::Except(Except {
            left: Box::new(lower_op(&e.left)?),
            right: Box::new(lower_op(&e.right)?),
            all: e.all,
        })),
        LP::Intersect(i) => Ok(CommonOp::Intersect(Intersect {
            left: Box::new(lower_op(&i.left)?),
            right: Box::new(lower_op(&i.right)?),
            all: i.all,
        })),
        LP::Distinct(d) => Ok(CommonOp::Distinct(Distinct {
            input: Box::new(lower_op(&d.input)?),
            on: d.columns.clone(),
        })),
        LP::TableScan(t) => Ok(CommonOp::TableScan(TableScan {
            name: t.table.clone(),
            schema: t.schema.clone(),
        })),
        LP::LocalRelation(l) => Ok(CommonOp::LocalRelation(LocalRelation {
            schema: l.schema.clone(),
        })),
        LP::LocalDataRelation(_) => Ok(CommonOp::Punt {
            kind: "LocalDataRelation",
            reason: "data-carrying local relation not supported in v2 substrate yet — legacy path handles Phase 2 arrow payload",
        }),
        LP::InMemoryRelation(r) => Ok(CommonOp::TableScan(TableScan {
            name: r.view_name.clone(),
            schema: r.schema.clone(),
        })),
        LP::RangeRelation(r) => Ok(CommonOp::RangeRelation(RangeRelation {
            start: r.start,
            end: r.end,
            step: r.step,
        })),
        LP::WithColumns(w) => Ok(CommonOp::WithColumns(WithColumns {
            input: Box::new(lower_op(&w.input)?),
            columns: w.columns.clone(),
        })),
        LP::AliasedRelation(a) => Ok(CommonOp::AliasedRelation(AliasedRelation {
            input: Box::new(lower_op(&a.input)?),
            alias: a.alias.clone(),
            column_aliases: a.column_aliases.clone(),
        })),
        LP::DropColumns(d) => Ok(CommonOp::DropColumns(DropColumns {
            input: Box::new(lower_op(&d.input)?),
            names: d.column_names.clone(),
        })),
        // ── Punted variants ──────────────────────────────────────────────────
        // Every variant below is either a runtime-data-dependent operator
        // (Pivot needs pivot values), a stat/EDA sink (Describe/Summary),
        // a DDL/DML surface (DdlStatement), or a legacy-cosmetic shape
        // (SingleRow → `SELECT` without FROM). All punt; the analyzer
        // returns `PuntedOperator`; `service.rs` decides fallback.
        LP::Sample(_) => Ok(CommonOp::Punt {
            kind: "Sample",
            reason: "Sample requires runtime hashing — deferred to a later slice",
        }),
        LP::SqlRelation(_) => Ok(CommonOp::Punt {
            kind: "SqlRelation",
            reason: "raw-SQL sub-relations are not on the Slice B common-AST surface",
        }),
        LP::WithCte(_) => Ok(CommonOp::Punt {
            kind: "WithCte",
            reason: "CTEs are not on the Slice B common-AST surface",
        }),
        LP::DdlStatement(_) => Ok(CommonOp::Punt {
            kind: "DdlStatement",
            reason: "DDL/DML statements are handled by the command path, not τ",
        }),
        LP::ToDataFrame(_) => Ok(CommonOp::Punt {
            kind: "ToDataFrame",
            reason: "toDF column-rename is deferred to a later slice",
        }),
        LP::SingleRow(_) => Ok(CommonOp::Punt {
            kind: "SingleRow",
            reason: "SELECT-without-FROM is a legacy cosmetic shape; deferred",
        }),
        LP::ShowString(_) => Ok(CommonOp::Punt {
            kind: "ShowString",
            reason: "ShowString is client-side formatting; not τ's concern",
        }),
        LP::NADrop(_) => Ok(CommonOp::Punt {
            kind: "NADrop",
            reason: "na.drop() is deferred to a later slice",
        }),
        LP::NAFill(_) => Ok(CommonOp::Punt {
            kind: "NAFill",
            reason: "na.fill() is deferred to a later slice",
        }),
        LP::NAReplace(_) => Ok(CommonOp::Punt {
            kind: "NAReplace",
            reason: "na.replace() is deferred to a later slice",
        }),
        LP::Unpivot(_) => Ok(CommonOp::Punt {
            kind: "Unpivot",
            reason: "Unpivot/melt is deferred to a later slice",
        }),
        LP::Pivot(_) => Ok(CommonOp::Punt {
            kind: "Pivot",
            reason: "runtime-data-dependent schema; deferred to a later slice",
        }),
        LP::StatCov(_) => Ok(CommonOp::Punt {
            kind: "StatCov",
            reason: "stat.cov is deferred to a later slice",
        }),
        LP::StatCorr(_) => Ok(CommonOp::Punt {
            kind: "StatCorr",
            reason: "stat.corr is deferred to a later slice",
        }),
        LP::ApproxQuantile(_) => Ok(CommonOp::Punt {
            kind: "ApproxQuantile",
            reason: "approxQuantile has a bespoke execution path",
        }),
        LP::StatCrosstab(_) => Ok(CommonOp::Punt {
            kind: "StatCrosstab",
            reason: "stat.crosstab is deferred to a later slice",
        }),
        LP::StatFreqItems(_) => Ok(CommonOp::Punt {
            kind: "StatFreqItems",
            reason: "stat.freqItems is deferred to a later slice",
        }),
        LP::StatSampleBy(_) => Ok(CommonOp::Punt {
            kind: "StatSampleBy",
            reason: "stat.sampleBy is deferred to a later slice",
        }),
        LP::Describe(_) => Ok(CommonOp::Punt {
            kind: "Describe",
            reason: "describe() is deferred to a later slice",
        }),
        LP::Summary(_) => Ok(CommonOp::Punt {
            kind: "Summary",
            reason: "summary() is deferred to a later slice",
        }),
    }
}

fn lower_aggregate(a: &legacy::Aggregate) -> Result<CommonOp, LoweringError> {
    // Slice B's `AggregateCall` mirrors legacy `AggregateExpr` field-for-field;
    // the SelectEntry-based interleaving is a legacy-only concept, so we drop
    // it and rely on the legacy execution path when it's non-trivially set.
    if !a.select_order.is_empty() {
        return Ok(CommonOp::Punt {
            kind: "AggregateSelectOrder",
            reason: "SelectEntry-based aggregate ordering not in Slice B AST",
        });
    }
    let aggregates: Vec<AggregateCall> = a
        .aggregates
        .iter()
        .map(|e| AggregateCall {
            func: e.func.clone(),
            is_distinct: e.is_distinct,
            filter: e.filter.clone(),
        })
        .collect();
    Ok(CommonOp::Aggregate(Aggregate {
        input: Box::new(lower_op(&a.input)?),
        grouping: a.grouping.clone(),
        aggregates,
        having: a.having.clone(),
        grouping_sets: a.grouping_sets.clone(),
    }))
}

fn lower_join(j: &legacy::Join) -> Result<CommonOp, LoweringError> {
    let join_type = match &j.join_type {
        legacy::JoinType::Inner => JoinKind::Inner,
        legacy::JoinType::Left => JoinKind::Left,
        legacy::JoinType::Right => JoinKind::Right,
        legacy::JoinType::Full => JoinKind::Full,
        legacy::JoinType::Cross => JoinKind::Cross,
        legacy::JoinType::LeftSemi => JoinKind::LeftSemi,
        legacy::JoinType::LeftAnti => JoinKind::LeftAnti,
    };
    // Legacy `Join` also carries plan_id-qualification aliases and a per-
    // side plan_id list; C.1's `CommonOp::Join` does not carry those (the
    // analyzer resolves join column qualification structurally). If a
    // plan opts into legacy's plan-id aliasing we punt and let legacy
    // handle it.
    if j.left_alias.is_some() || j.right_alias.is_some() {
        return Ok(CommonOp::Punt {
            kind: "Join",
            reason: "plan-id-qualified joins are legacy-only",
        });
    }
    Ok(CommonOp::Join(Join {
        left: Box::new(lower_op(&j.left)?),
        right: Box::new(lower_op(&j.right)?),
        join_type,
        on: j.condition.clone(),
        using: j.using_columns.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logical::{LogicalPlan, RangeRelation as LegacyRange};

    #[test]
    fn lowers_range_relation() {
        let plan = LogicalPlan::RangeRelation(LegacyRange {
            start: 0,
            end: 5,
            step: 1,
            num_partitions: None,
        });
        let ast = lower(&plan).expect("range must lower");
        assert!(matches!(ast.root, CommonOp::RangeRelation(_)));
    }

    #[test]
    fn punts_pivot() {
        let plan = LogicalPlan::Pivot(crate::logical::Pivot {
            input: Box::new(LogicalPlan::SingleRow(crate::logical::SingleRowRelation)),
            grouping: vec![],
            pivot_col: crate::expression::Literal::int(0),
            pivot_values: vec![],
            aggregates: vec![],
        });
        let ast = lower(&plan).expect("Pivot must lower to a Punt op");
        assert!(matches!(ast.root, CommonOp::Punt { kind: "Pivot", .. }));
    }

    #[test]
    fn punts_single_row() {
        let plan = LogicalPlan::SingleRow(crate::logical::SingleRowRelation);
        let ast = lower(&plan).expect("SingleRow must lower to a Punt op");
        assert!(matches!(
            ast.root,
            CommonOp::Punt {
                kind: "SingleRow",
                ..
            }
        ));
    }

    #[test]
    fn lowers_project_and_filter_chain() {
        // Filter(Project(TableScan))
        let scan = LogicalPlan::TableScan(crate::logical::TableScan {
            table: "nums".to_string(),
            alias: None,
            schema: crate::types::StructType::empty(),
        });
        let proj =
            LogicalPlan::project(scan, vec![crate::expression::ColumnReference::untyped("a")]);
        let filt = LogicalPlan::filter(proj, crate::expression::Literal::boolean(true));
        let ast = lower(&filt).expect("filter+project must lower");
        assert!(matches!(ast.root, CommonOp::Filter(_)));
    }
}
