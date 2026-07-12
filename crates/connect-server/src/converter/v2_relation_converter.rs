//! τ's Spark Connect protobuf → [`CommonAst`] converter.
//!
//! **INV10:** this file imports value-level types from
//! `thunderduck_core::types` (`DataType`, `StructField`, `StructType`) and
//! substrate types from `thunderduck_core::transpiler_v2::*`. NO
//! `thunderduck_core::{logical,expression,generator,functions,parser,runtime}`
//! and NO `thunderduck_core::types::TypeInferenceEngine`. See
//! `inv10_no_disallowed_imports_from_transpiler_v2`.
//!
//! **Anti-SQL anchor (§2.1):** `arrow_val_to_literal()` uses exhaustive
//! typed dispatch on the Arrow value's data type and NEVER emits an
//! `Ok("NULL")` catch-all. Unhandled Arrow types return
//! [`EmissionError::Unsupported`] with `kind: ProtoShape`. This is mechanically enforced
//! by `arrow_val_no_catch_all_ok_null_source_grep`.
//!
//! **No `Sql` opaque variant (§2.2):** `RelType::Sql` surfaces as
//! [`EmissionError::Unsupported`] (`kind: ProtoShape`) — the SparkSQL path belongs to
//! `parser_v2`.
//!
//! **First-class plan_id (§2.3):** `CommonOp::Join` carries
//! `left_plan_ids: Vec<i64>` / `right_plan_ids: Vec<i64>`; every
//! `UnresolvedColumn` produced by the expression converter carries
//! `plan_id: Option<i64>` sourced from `UnresolvedAttribute::plan_id`.

use std::collections::HashSet;

use arrow::array::{
    Array, BinaryArray, BooleanArray, Date32Array, Decimal128Array, DurationMicrosecondArray,
    Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array,
    IntervalMonthDayNanoArray, IntervalYearMonthArray, LargeBinaryArray, LargeStringArray,
    ListArray, MapArray, StringArray, StructArray, TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType as ArrowDT, Field, IntervalUnit, TimeUnit};
use arrow_ipc::reader::StreamReader;
use thunderduck_core::bail_boundary_proto;
use thunderduck_core::transpiler_v2::ast::{
    AggregateProjection, CommonAst, CommonOp, FileFormat, JoinType, PivotGrouping, UnpivotIds,
};
use thunderduck_core::transpiler_v2::error::UnsupportedKind;
use thunderduck_core::transpiler_v2::expression::{
    decimal_value_precision_scale, AliasExpression, ArrayLiteralExpression, BinaryExpression,
    BinaryOp, CaseWhenExpression, CastExpression, Expression, ExtractValueExpression, FunctionCall,
    InListExpression, LambdaExpression, LambdaVariableExpression, Literal, LiteralValue,
    MapLiteralExpression, NullOrdering, RawSqlExpression, SortDirection, SortOrder, StarExpression,
    StructLiteralExpression, UnaryExpression, UnaryOp, UnresolvedColumn, UnresolvedRegexExpression,
    UpdateFieldsExpression, WindowFunction,
};
use thunderduck_core::transpiler_v2::macros::ProtoFieldExt;
use thunderduck_core::transpiler_v2::EmissionError;
use thunderduck_core::types::{DataType, StructField, StructType};

use crate::converter::type_converter::{parse_type_str, proto_to_data_type};
use crate::proto::spark::connect as proto;

/// Normalize a decimal literal's `(precision, scale)` to what Spark's
/// `LiteralValueProtoConverter.decodeDecimal` would compute on the server
/// side. PySpark sends a `Decimal` proto carrying the raw value string plus
/// (optionally) the wire-supplied `precision`/`scale`; Spark reconciles the
/// two by taking the **maximum** of the value-derived shape and the
/// wire-supplied shape, then clamping to `DecimalType.MAX_PRECISION = 38`.
///
/// Algorithm (mirrors Spark line-for-line):
/// 1. Parse `value` → `(vp, vs)` where `vs` is the fractional-digit count
///    and `vp = max(int_digits_excluding_leading_zeros + vs, vs, 1)` — this
///    matches `Decimal.set(BigDecimal)` bumping `_precision` up to
///    `max(bigDecimal.precision, bigDecimal.scale)`.
/// 2. `p_wire = server_precision.unwrap_or(vp)`.
/// 3. `s_wire = server_scale.unwrap_or(vs)`.
/// 4. `p_out = min(38, max(vp, p_wire))` — DecimalType.MAX_PRECISION clamp.
/// 5. `s_out = max(vs, s_wire)`.
/// 6. If `s_out > p_out` bump `p_out = min(s_out, 38)` to preserve the
///    `precision >= scale` invariant on malformed wire input.
///
/// Sources: Apache Spark 4.1.1
/// `sql/connect/common/src/main/scala/org/apache/spark/sql/connect/common/LiteralValueProtoConverter.scala:555-571`
/// and `sql/api/src/main/scala/org/apache/spark/sql/types/Decimal.scala:138-151`.
///
/// Corpus anchor: `cond-004` — Spark widens
/// `coalesce(Decimal(10,2), lit(Decimal("0.00")))` to `Decimal(10,2)`.
/// PySpark sends `(value="0.00", precision=10, scale=0)`; value-derived is
/// `(2, 2)`; the max-of-value-and-wire rule yields `(10, 2)`, which unifies
/// with `Decimal(10,2)` as-is.
fn normalize_decimal_literal(
    value: &str,
    server_precision: Option<u8>,
    server_scale: Option<u8>,
) -> (u8, u8) {
    // Step 1: value-derived (vp, vs) — shared, unclamped computation in
    // `transpiler_v2::expression::decimal_value_precision_scale`.
    let (vp, vs) = decimal_value_precision_scale(value);

    // Steps 2-3: wire fields default to the value-derived shape.
    let p_wire = server_precision.unwrap_or(vp);
    let s_wire = server_scale.unwrap_or(vs);

    // Steps 4-5: max-of-value-and-wire, clamped to MAX_PRECISION = 38.
    let mut p_out = vp.max(p_wire).min(38);
    let s_out = vs.max(s_wire);

    // Step 6: preserve `precision >= scale` on malformed wire input.
    if s_out > p_out {
        p_out = s_out.min(38);
    }
    (p_out, s_out)
}

// ── Public API ─────────────────────────────────────────────────────────────

/// τ's protobuf → [`CommonAst`] converter.
pub struct V2RelationConverter {
    expr: V2ExpressionConverter,
}

impl Default for V2RelationConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl V2RelationConverter {
    /// Construct a fresh converter.
    pub fn new() -> Self {
        Self {
            expr: V2ExpressionConverter::new(),
        }
    }

    /// Convert a proto [`proto::Relation`] into a [`CommonAst`].
    pub fn convert(&mut self, relation: &proto::Relation) -> Result<CommonAst, EmissionError> {
        use proto::relation::RelType;
        let rel_type = relation.rel_type.as_ref().require_proto(
            "Relation::rel_type::None",
            "proto Relation carries no rel_type",
        )?;
        match rel_type {
            RelType::Project(p) => self.convert_project(p),
            RelType::Filter(f) => self.convert_filter(f),
            RelType::Sort(s) => self.convert_sort(s),
            RelType::Limit(l) => self.convert_limit(l),
            RelType::Offset(o) => self.convert_offset(o),
            RelType::Aggregate(a) => self.convert_aggregate(a),
            RelType::Read(r) => self.convert_read(r),
            RelType::LocalRelation(lr) => self.convert_local_relation(lr),
            RelType::Join(j) => self.convert_join(j),
            RelType::WithColumns(wc) => self.convert_with_columns(wc),
            RelType::Drop(d) => self.convert_drop(d),
            RelType::SetOp(so) => self.convert_set_op(so),
            RelType::SubqueryAlias(sa) => self.convert_subquery_alias(sa),
            RelType::WithColumnsRenamed(wcr) => self.convert_with_columns_renamed(wcr),
            RelType::ToDf(td) => {
                let input = self.convert_input(td.input.as_deref(), "ToDf")?;
                Ok(CommonAst::new(CommonOp::ToDf {
                    input: Box::new(input),
                    column_names: td.column_names.clone(),
                }))
            }
            RelType::Deduplicate(d) => self.convert_deduplicate(d),
            RelType::FillNa(f) => self.convert_fill_na(f),
            RelType::DropNa(d) => self.convert_drop_na(d),
            RelType::Replace(r) => self.convert_replace(r),
            RelType::Unpivot(u) => self.convert_unpivot(u),
            RelType::Describe(d) => self.convert_describe(d),
            RelType::Summary(s) => self.convert_summary(s),
            RelType::FreqItems(f) => self.convert_freq_items(f),
            RelType::Cov(c) => self.convert_cov(c),
            RelType::Corr(c) => self.convert_corr(c),
            RelType::ApproxQuantile(a) => self.convert_approx_quantile(a),
            RelType::Crosstab(c) => self.convert_crosstab(c),
            RelType::Sample(s) => self.convert_sample(s),
            RelType::SampleBy(s) => self.convert_sample_by(s),
            RelType::Range(r) => self.convert_range(r),
            // Cosmetic ops per Spark 4 semantics — semantically no-op.
            // Thunderduck ignores them and continues with the input relation
            // (ADR-001 "result-irrelevant cosmetic" carve-out).
            RelType::Hint(h) => self.convert_input(h.input.as_deref(), "Hint"),
            RelType::RepartitionByExpression(r) => {
                self.convert_input(r.input.as_deref(), "RepartitionByExpression")
            }
            RelType::Repartition(r) => self.convert_input(r.input.as_deref(), "Repartition"),
            RelType::ToSchema(ts) => self.convert_to_schema(ts),
            RelType::Sql(_) => bail_boundary_proto!(
                "RelType::Sql",
                "SQL text is owned by parser_v2, not V2RelationConverter",
            ),
            RelType::Catalog(_) => bail_boundary_proto!(
                "RelType::Catalog",
                "catalog operations deferred to future τ work",
            ),
            other => bail_boundary_proto!(
                format!("RelType::{}", rel_type_kind(other)),
                "relation shape not covered by V2RelationConverter",
            ),
        }
    }

    fn convert_input(
        &mut self,
        input: Option<&proto::Relation>,
        ctx: &str,
    ) -> Result<CommonAst, EmissionError> {
        match input {
            Some(rel) => self.convert(rel),
            None => bail_boundary_proto!(
                format!("{ctx}::missing_input"),
                format!("{ctx} has no input relation"),
            ),
        }
    }

    fn convert_fill_na(&mut self, f: &proto::NaFill) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(f.input.as_deref(), "NaFill")?;
        let values = f
            .values
            .iter()
            .map(|l| self.expr.convert_literal(l))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CommonAst::new(CommonOp::NaFill {
            input: Box::new(input),
            cols: f.cols.clone(),
            values,
        }))
    }

    fn convert_drop_na(&mut self, d: &proto::NaDrop) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(d.input.as_deref(), "NaDrop")?;
        Ok(CommonAst::new(CommonOp::NaDrop {
            input: Box::new(input),
            cols: d.cols.clone(),
            min_non_nulls: d.min_non_nulls,
        }))
    }

    fn convert_replace(&mut self, r: &proto::NaReplace) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(r.input.as_deref(), "NaReplace")?;
        let mut replacements: Vec<(Expression, Expression)> =
            Vec::with_capacity(r.replacements.len());
        for rep in &r.replacements {
            let old_lit = rep.old_value.as_ref().require_proto(
                "NaReplace::old_value::None",
                "NaReplace replacement missing old_value",
            )?;
            let new_lit = rep.new_value.as_ref().require_proto(
                "NaReplace::new_value::None",
                "NaReplace replacement missing new_value",
            )?;
            replacements.push((
                self.expr.convert_literal(old_lit)?,
                self.expr.convert_literal(new_lit)?,
            ));
        }
        Ok(CommonAst::new(CommonOp::NaReplace {
            input: Box::new(input),
            cols: r.cols.clone(),
            replacements,
        }))
    }

    fn convert_unpivot(&mut self, u: &proto::Unpivot) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(u.input.as_deref(), "Unpivot")?;

        // Extract id column names — the τ AST stores column names. Anything
        // richer than a bare `UnresolvedAttribute` is a Thunderduck-boundary
        // shape.
        let mut ids: Vec<String> = Vec::with_capacity(u.ids.len());
        for e in &u.ids {
            ids.push(extract_column_name(e).require_proto(
                "Unpivot::id::non_attribute",
                "Unpivot id columns must be bare column references",
            )?);
        }

        // Extract value column names; None ⇒ analyzer expands to all non-id
        // input columns per Spark's default.
        let values: Vec<String> = match u.values.as_ref() {
            Some(v) => {
                let mut out = Vec::with_capacity(v.values.len());
                for e in &v.values {
                    out.push(extract_column_name(e).require_proto(
                        "Unpivot::value::non_attribute",
                        "Unpivot value columns must be bare column references",
                    )?);
                }
                out
            }
            None => Vec::new(),
        };

        Ok(CommonAst::new(CommonOp::Unpivot {
            input: Box::new(input),
            ids: UnpivotIds::Explicit(ids),
            values,
            variable_column_name: u.variable_column_name.clone(),
            value_column_name: u.value_column_name.clone(),
        }))
    }

    fn convert_describe(&mut self, d: &proto::StatDescribe) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(d.input.as_deref(), "Describe")?;
        Ok(CommonAst::new(CommonOp::Describe {
            input: Box::new(input),
            cols: d.cols.clone(),
        }))
    }

    fn convert_summary(&mut self, s: &proto::StatSummary) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(s.input.as_deref(), "Summary")?;
        Ok(CommonAst::new(CommonOp::Summary {
            input: Box::new(input),
            statistics: s.statistics.clone(),
        }))
    }

    /// Convert `proto::StatFreqItems`. The proto's `support` is optional; the
    /// PySpark client default is `0.01` (per
    /// `pyspark/sql/dataframe.py::freqItems`) which τ substitutes when the
    /// field is None.
    fn convert_freq_items(&mut self, f: &proto::StatFreqItems) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(f.input.as_deref(), "FreqItems")?;
        let support = f.support.unwrap_or(0.01);
        Ok(CommonAst::new(CommonOp::FreqItems {
            input: Box::new(input),
            cols: f.cols.clone(),
            support,
        }))
    }

    /// Convert `proto::Sample`. Proto `deterministic_order` (physical hint) is
    /// dropped at τ conversion.
    fn convert_sample(&mut self, s: &proto::Sample) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(s.input.as_deref(), "Sample")?;
        Ok(CommonAst::new(CommonOp::Sample {
            input: Box::new(input),
            lower_bound: s.lower_bound,
            upper_bound: s.upper_bound,
            with_replacement: s.with_replacement.unwrap_or(false),
            seed: s.seed,
        }))
    }

    /// Convert `proto::StatSampleBy`. Each stratum must decode as an
    /// `Expression::Literal`; anything else is a loud proto-shape error
    /// (defensive per the connect-server val() lesson in CLAUDE.md gotcha #9).
    fn convert_sample_by(&mut self, s: &proto::StatSampleBy) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(s.input.as_deref(), "SampleBy")?;
        let col_proto = s
            .col
            .as_ref()
            .require_proto("SampleBy::col::None", "StatSampleBy missing col")?;
        let col = self.expr.convert(col_proto)?;
        let mut fractions: Vec<(Literal, f64)> = Vec::with_capacity(s.fractions.len());
        for frac in &s.fractions {
            let stratum_proto = frac.stratum.as_ref().require_proto(
                "SampleBy::Fraction::stratum::None",
                "SampleBy fraction missing stratum literal",
            )?;
            let lit_expr = self.expr.convert_literal(stratum_proto)?;
            let Expression::Literal(lit) = lit_expr else {
                bail_boundary_proto!(
                    "SampleBy::Fraction::stratum::non-literal",
                    "stratum did not decode as Expression::Literal",
                );
            };
            fractions.push((lit, frac.fraction));
        }
        Ok(CommonAst::new(CommonOp::SampleBy {
            input: Box::new(input),
            col,
            fractions,
            seed: s.seed,
        }))
    }

    /// Convert `proto::Range` into a `CommonOp::TableFunction { name: "range" }`.
    ///
    /// Proto fields: `start` (optional, default 0), `end` (required), `step`
    /// (required, client-side default 1). `num_partitions` is a distribution
    /// hint irrelevant to a single-node engine and is silently dropped (same
    /// "cosmetic carve-out" precedent as the `Repartition` / `Hint` arms).
    ///
    /// The resulting `TableFunction` node carries three `Expression::Literal`
    /// `Long` args in `(start, end, step)` order — the same shape the SQL
    /// front-end produces for `SELECT * FROM range(...)`.
    fn convert_range(&mut self, r: &proto::Range) -> Result<CommonAst, EmissionError> {
        let long_lit = |v: i64| -> Expression { lit(LiteralValue::Long(v), DataType::Long) };
        let start = long_lit(r.start.unwrap_or(0));
        let end = long_lit(r.end);
        let step = long_lit(r.step);
        // num_partitions is silently ignored (single-node engine).
        Ok(CommonAst::new(CommonOp::TableFunction {
            name: "range".to_owned(),
            args: vec![start, end, step],
            with_ordinality: false,
        }))
    }

    /// Convert `proto::StatCrosstab`. The analyzer rejects this variant as a
    /// Thunderduck-boundary punt (`Crosstab[dynamic-values]`) — output
    /// columns are `DISTINCT(col2)` which is unknowable at plan time.
    fn convert_crosstab(&mut self, c: &proto::StatCrosstab) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(c.input.as_deref(), "Crosstab")?;
        Ok(CommonAst::new(CommonOp::Crosstab {
            input: Box::new(input),
            col1: c.col1.clone(),
            col2: c.col2.clone(),
        }))
    }

    /// Convert `proto::StatCov` (`df.stat.cov(col1, col2)`).
    ///
    /// Desugars to a global `Aggregate` (no grouping) whose single aggregate
    /// expression is `covar_samp(COALESCE(col1, 0), COALESCE(col2, 0))`.
    /// Both DuckDB and Spark expose `covar_samp` natively; τ's type inference
    /// already registers the name (returns `Double`, always nullable).
    ///
    /// **NULL semantics (Spark parity):** Spark's `StatFunctions.calculateCovImpl`
    /// calls `ds.na.fill(0L)` before aggregating — NULLs in numeric columns
    /// are replaced with zero, so rows with a NULL in one column still
    /// participate in the covariance (contributing zero for the missing value).
    /// DuckDB's bare `covar_samp` skips NULL pairs entirely, which produces a
    /// different result when NULLs are present.  Wrapping each column ref in
    /// `COALESCE(col, 0)` aligns DuckDB with Spark.
    ///
    /// PySpark wire contract: single row, single column; the client extracts
    /// `table[0][0].as_py()` — column name is immaterial.
    fn convert_cov(&mut self, c: &proto::StatCov) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(c.input.as_deref(), "StatCov")?;
        let col1 = coalesce_zero(&c.col1);
        let col2 = coalesce_zero(&c.col2);
        let agg_expr = Expression::FunctionCall(FunctionCall {
            name: "covar_samp".to_owned(),
            args: vec![col1, col2],
            distinct: false,
        });
        Ok(CommonAst::new(CommonOp::Aggregate {
            input: Box::new(input),
            grouping: vec![],
            aggregates: vec![agg_expr],
            projection: AggregateProjection::Grouped,
            grouping_kind: thunderduck_core::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        }))
    }

    /// Convert `proto::StatCorr` (`df.stat.corr(col1, col2)`).
    ///
    /// Desugars to a global `Aggregate` with `corr(col1, col2)`.  Spark only
    /// supports the Pearson correlation coefficient; the PySpark client
    /// validates this client-side, but the proto carries an optional `method`
    /// field — τ rejects non-Pearson methods as a Spark-emulated error (they
    /// would also fail Spark-side).
    ///
    /// PySpark wire contract: single row, single column (`table[0][0].as_py()`).
    fn convert_corr(&mut self, c: &proto::StatCorr) -> Result<CommonAst, EmissionError> {
        // Validate method if present.
        if let Some(ref method) = c.method {
            if !method.eq_ignore_ascii_case("pearson") {
                return Err(EmissionError::Unsupported {
                    kind: UnsupportedKind::ProtoShape,
                    name: "StatCorr::method".to_owned(),
                    reason: format!(
                        "only the Pearson correlation coefficient is supported, got '{method}'"
                    ),
                });
            }
        }
        let input = self.convert_input(c.input.as_deref(), "StatCorr")?;
        let col1 = unresolved_col(&c.col1);
        let col2 = unresolved_col(&c.col2);
        let agg_expr = Expression::FunctionCall(FunctionCall {
            name: "corr".to_owned(),
            args: vec![col1, col2],
            distinct: false,
        });
        Ok(CommonAst::new(CommonOp::Aggregate {
            input: Box::new(input),
            grouping: vec![],
            aggregates: vec![agg_expr],
            projection: AggregateProjection::Grouped,
            grouping_kind: thunderduck_core::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        }))
    }

    /// Convert `proto::StatApproxQuantile` (`df.stat.approxQuantile(...)`).
    ///
    /// Spark uses a Greenwald-Khanna sketch whose accuracy depends on
    /// `relative_error`.  On small datasets (the corpus witnesses) the sketch
    /// returns exact order statistics (actual data elements).  τ maps to
    /// DuckDB's exact `quantile_disc` (discrete sample value) which matches
    /// the small-data behavior; `relative_error` is intentionally ignored
    /// because exact computation subsumes any accuracy parameter.
    ///
    /// PySpark wire contract: single row, single column of type
    /// `Array(Array(Double))`.  Outer array has one element per input column;
    /// inner array has one element per probability.  The client decodes as
    /// `[q.as_py() for q in table[0][0]]`.
    ///
    /// Desugars to a global `Aggregate` whose single aggregate expression is
    /// a nested `ArrayLiteral`:
    /// ```text
    /// [[percentile_approx(col1, p1), percentile_approx(col1, p2), ...],
    ///  [percentile_approx(col2, p1), percentile_approx(col2, p2), ...]]
    /// ```
    /// Each `percentile_approx` call flows through the existing
    /// `render_aggregate` emission arm which maps to `quantile_disc`.
    fn convert_approx_quantile(
        &mut self,
        a: &proto::StatApproxQuantile,
    ) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(a.input.as_deref(), "StatApproxQuantile")?;
        // Build per-column inner arrays: [percentile_approx(col, p1), ... , percentile_approx(col, pN)]
        let per_col_arrays: Vec<Expression> = a
            .cols
            .iter()
            .map(|col_name| {
                let elements: Vec<Expression> = a
                    .probabilities
                    .iter()
                    .map(|&prob| {
                        Expression::FunctionCall(FunctionCall {
                            name: "percentile_approx".to_owned(),
                            args: vec![
                                unresolved_col(col_name),
                                lit(LiteralValue::Double(prob), DataType::Double),
                            ],
                            distinct: false,
                        })
                    })
                    .collect();
                Expression::ArrayLiteral(ArrayLiteralExpression {
                    elements,
                    element_type: DataType::Double,
                })
            })
            .collect();
        // Outer array wrapping per-column arrays.
        let outer = Expression::ArrayLiteral(ArrayLiteralExpression {
            elements: per_col_arrays,
            element_type: DataType::Array(Box::new(DataType::Double), true),
        });
        Ok(CommonAst::new(CommonOp::Aggregate {
            input: Box::new(input),
            grouping: vec![],
            aggregates: vec![outer],
            projection: AggregateProjection::Grouped,
            grouping_kind: thunderduck_core::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        }))
    }

    /// Convert `proto::ToSchema` (Spark `df.to(schema)`).
    ///
    /// Desugars to `CommonOp::Project` whose projections are, per target field
    /// in TARGET ORDER: `Alias(Cast(UnresolvedColumn(name), target_type), name)`.
    ///
    /// The unconditional CAST is intentional — the converter is untyped; a
    /// same-type CAST is a no-op for value, wire type, and nullability
    /// (expression.rs:732 — non-try Cast preserves child nullability for
    /// non-string sources).
    ///
    /// # Deviations from Spark 4.1.1 `Dataset.to`
    ///
    /// The following behaviours are deliberately out of scope (all unexercised
    /// by the corpus tests):
    ///
    /// - **Missing nullable target column**: Spark null-fills columns present
    ///   in the target schema but absent in the source; τ lets the unresolved
    ///   column propagate to the analyzer, which surfaces a resolution error.
    /// - **Analysis-time diagnostics**: Spark raises
    ///   `NULLABLE_COLUMN_OR_FIELD`, `INVALID_COLUMN_OR_FIELD_DATA_TYPE`,
    ///   or `AMBIGUOUS_COLUMN_OR_FIELD` during analysis; τ does not emulate
    ///   these error classes.
    /// - **Nested struct/array/map reconciliation**: Spark recursively
    ///   reconciles nested complex types; τ applies a flat top-level CAST only.
    fn convert_to_schema(&mut self, ts: &proto::ToSchema) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(ts.input.as_deref(), "ToSchema")?;
        let schema_proto = ts
            .schema
            .as_ref()
            .require_proto("ToSchema::missing_schema", "ToSchema has no target schema")?;
        let dt = proto_to_data_type(schema_proto);
        let st = match dt {
            DataType::Struct(st) => st,
            _ => {
                bail_boundary_proto!("ToSchema::schema", "ToSchema schema is not a struct");
            }
        };
        let projections: Vec<Expression> = st
            .fields
            .iter()
            .map(|f| {
                let col = Expression::UnresolvedColumn(UnresolvedColumn {
                    name: f.name.clone(),
                    qualifier: None,
                    plan_id: None,
                });
                let cast = Expression::Cast(CastExpression {
                    expr: Box::new(col),
                    to_type: f.data_type.clone(),
                    try_cast: false,
                    implicit: false,
                });
                Expression::Alias(AliasExpression {
                    expr: Box::new(cast),
                    alias: f.name.clone(),
                })
            })
            .collect();
        Ok(CommonAst::new(CommonOp::Project {
            input: Box::new(input),
            projections,
        }))
    }

    fn convert_deduplicate(&mut self, d: &proto::Deduplicate) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(d.input.as_deref(), "Deduplicate")?;
        // `all_columns_as_keys=true` → dedupe on all columns (empty on_columns).
        // Otherwise use column_names.
        let on_columns = if d.all_columns_as_keys.unwrap_or(false) {
            Vec::new()
        } else {
            d.column_names.clone()
        };
        Ok(CommonAst::new(CommonOp::Deduplicate {
            input: Box::new(input),
            on_columns,
        }))
    }

    fn convert_subquery_alias(
        &mut self,
        sa: &proto::SubqueryAlias,
    ) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(sa.input.as_deref(), "SubqueryAlias")?;
        Ok(CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(input),
            alias: sa.alias.clone(),
        }))
    }

    fn convert_with_columns_renamed(
        &mut self,
        wcr: &proto::WithColumnsRenamed,
    ) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(wcr.input.as_deref(), "WithColumnsRenamed")?;
        // Proto 3.4+ uses `rename_columns_map` (repeated Rename with existing/new).
        let mut renames: Vec<(String, String)> = Vec::new();
        for r in &wcr.renames {
            renames.push((r.col_name.clone(), r.new_col_name.clone()));
        }
        Ok(CommonAst::new(CommonOp::WithColumnsRenamed {
            input: Box::new(input),
            renames,
        }))
    }

    fn convert_set_op(&mut self, so: &proto::SetOperation) -> Result<CommonAst, EmissionError> {
        use proto::set_operation::SetOpType;
        use thunderduck_core::transpiler_v2::analyzer::SetOpKind;
        let left = self.convert_input(so.left_input.as_deref(), "SetOp::left")?;
        let right = self.convert_input(so.right_input.as_deref(), "SetOp::right")?;
        let kind = match SetOpType::try_from(so.set_op_type).unwrap_or(SetOpType::Unspecified) {
            SetOpType::Union => SetOpKind::Union,
            SetOpType::Intersect => SetOpKind::Intersect,
            SetOpType::Except => SetOpKind::Except,
            SetOpType::Unspecified => {
                bail_boundary_proto!(
                    "SetOp::Unspecified",
                    "SetOp proto has SET_OP_TYPE_UNSPECIFIED",
                );
            }
        };
        Ok(CommonAst::new(CommonOp::SetOp {
            kind,
            all: so.is_all.unwrap_or(false),
            by_name: so.by_name.unwrap_or(false),
            allow_missing_columns: so.allow_missing_columns.unwrap_or(false),
            children: vec![left, right],
        }))
    }

    fn convert_drop(&mut self, d: &proto::Drop) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(d.input.as_deref(), "Drop")?;
        // Spark's `df.drop(col1, col2, ...)` may arrive via `column_names`
        // (raw strings, most common) or `columns` (Expression references).
        // For Column references we accept only bare `UnresolvedAttribute`
        // shapes; anything more elaborate is a Thunderduck-boundary.
        let mut drop_names: Vec<String> = d.column_names.clone();
        for col_expr in &d.columns {
            use proto::expression::ExprType;
            let expr_type = col_expr
                .expr_type
                .as_ref()
                .require_proto("Drop::column::None", "Drop.columns entry has no expr_type")?;
            match expr_type {
                ExprType::UnresolvedAttribute(a) => {
                    drop_names.push(a.unparsed_identifier.clone());
                }
                _ => {
                    bail_boundary_proto!(
                        "Drop::column::non_attribute",
                        "Drop.columns must be bare column references",
                    );
                }
            }
        }
        Ok(CommonAst::new(CommonOp::DropColumns {
            input: Box::new(input),
            drop_names,
        }))
    }

    fn convert_with_columns(
        &mut self,
        wc: &proto::WithColumns,
    ) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(wc.input.as_deref(), "WithColumns")?;
        let mut assignments: Vec<(String, thunderduck_core::transpiler_v2::Expression)> =
            Vec::with_capacity(wc.aliases.len());
        for alias in &wc.aliases {
            // Proto contract: exactly one name part for a scalar column.
            let name = single_name_part(
                &alias.name,
                "WithColumns::Alias::multi_name",
                "WithColumns aliases must carry exactly one name part",
            )?;
            let expr_proto = alias.expr.as_deref().require_proto(
                "WithColumns::Alias::missing_expr",
                "WithColumns alias has no expression",
            )?;
            let expr = self.expr.convert(expr_proto)?;
            assignments.push((name, expr));
        }
        Ok(CommonAst::new(CommonOp::WithColumns {
            input: Box::new(input),
            assignments,
        }))
    }

    fn convert_project(&mut self, p: &proto::Project) -> Result<CommonAst, EmissionError> {
        let input = match &p.input {
            Some(i) => self.convert(i)?,
            None => CommonAst::new(CommonOp::SingleRow),
        };
        // Spark's `F.posexplode(arr).alias("pos", "val")` arrives as a single
        // proto `Alias(name=[pos, val], expr=UnresolvedFunction("posexplode", [arr]))`.
        // A one-slot projection needs to yield two SELECT-list expressions
        // (position + value). Detect the multi-name posexplode Alias here and
        // expand into two synthetic FunctionCall projections that
        // `emission::render_function_call` renders as `generate_subscripts(arr, 1) - 1`
        // and `UNNEST(arr)` respectively. Corpus: arr-017.
        let mut projections: Vec<Expression> = Vec::with_capacity(p.expressions.len());
        for e in &p.expressions {
            if let Some(pair) = self.expr.try_convert_posexplode_multi_alias(e)? {
                let (pos_proj, val_proj) = pair;
                projections.push(pos_proj);
                projections.push(val_proj);
            } else {
                projections.push(self.expr.convert(e)?);
            }
        }
        Ok(CommonAst::new(CommonOp::Project {
            input: Box::new(input),
            projections,
        }))
    }

    fn convert_filter(&mut self, f: &proto::Filter) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(f.input.as_deref(), "Filter")?;
        let condition_proto = f
            .condition
            .as_ref()
            .require_proto("Filter::missing_condition", "Filter has no condition")?;
        let condition = self.expr.convert(condition_proto)?;
        Ok(CommonAst::new(CommonOp::Filter {
            input: Box::new(input),
            condition,
        }))
    }

    fn convert_sort(&mut self, s: &proto::Sort) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(s.input.as_deref(), "Sort")?;
        let order = s
            .order
            .iter()
            .map(|so| self.expr.convert_sort_order(so))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CommonAst::new(CommonOp::Sort {
            input: Box::new(input),
            order,
            limit: None,
            offset: None,
        }))
    }

    fn convert_limit(&mut self, l: &proto::Limit) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(l.input.as_deref(), "Limit")?;
        Ok(CommonAst::new(CommonOp::Limit {
            input: Box::new(input),
            limit: l.limit as i64,
            offset: None,
        }))
    }

    fn convert_offset(&mut self, o: &proto::Offset) -> Result<CommonAst, EmissionError> {
        let input = self.convert_input(o.input.as_deref(), "Offset")?;
        Ok(CommonAst::new(CommonOp::Sort {
            input: Box::new(input),
            order: vec![],
            limit: None,
            offset: Some(o.offset as i64),
        }))
    }

    fn convert_aggregate(&mut self, a: &proto::Aggregate) -> Result<CommonAst, EmissionError> {
        use proto::aggregate::GroupType;
        use thunderduck_core::transpiler_v2::ast::GroupingKind;
        // Pass 60: PIVOT is a first-class CommonOp — not an Aggregate.
        // Bail into the dedicated pivot conversion before the GroupType
        // discriminator collapses into GroupingKind (which has no Pivot arm).
        if a.group_type() == GroupType::Pivot {
            return self.convert_pivot(a);
        }
        let grouping_kind = match a.group_type() {
            GroupType::Unspecified | GroupType::Groupby => GroupingKind::GroupBy,
            GroupType::Rollup => GroupingKind::Rollup,
            GroupType::Cube => GroupingKind::Cube,
            GroupType::GroupingSets => GroupingKind::GroupingSets,
            GroupType::Pivot => unreachable!("handled by convert_pivot above"),
        };
        let input = self.convert_input(a.input.as_deref(), "Aggregate")?;
        let grouping = self.expr.convert_all(&a.grouping_expressions)?;
        let aggregates = self.expr.convert_all(&a.aggregate_expressions)?;
        Ok(CommonAst::new(CommonOp::Aggregate {
            input: Box::new(input),
            grouping,
            aggregates,
            // The DataFrame converter puts only the aggregate expressions
            // into `aggregates` (grouping columns are never mixed in) — the
            // output is `grouping ++ aggregates`, Spark's DataFrame
            // `.groupBy(...).agg(...)` semantics.
            projection: AggregateProjection::Grouped,
            grouping_kind,
            // DataFrame `groupingSets` path is not implemented in τ — leave the
            // per-set membership empty so emission surfaces the boundary error
            // (ADR-022). The SparkSQL front-end populates this instead.
            grouping_sets: Vec::new(),
            // DataFrame path models post-aggregation filtering as a separate
            // Filter over the Aggregate; HAVING is a SparkSQL-only concept.
            having: None,
        }))
    }

    /// Convert `Aggregate` protos whose `group_type` is `PIVOT` into a
    /// [`CommonOp::Pivot`]. Grouping expressions map 1:1; the pivot column
    /// and (optional) pivot value literals live in the proto `pivot` sub-msg.
    /// Aggregate expressions come from `aggregate_expressions`. Empty
    /// `pivot_values` is legal — signals "eager discovery" per Spark
    /// semantics; DuckDB PIVOT will auto-materialise distinct values at
    /// execution.
    fn convert_pivot(&mut self, a: &proto::Aggregate) -> Result<CommonAst, EmissionError> {
        let pivot_proto = a.pivot.as_ref().require_proto(
            "Aggregate::Pivot::missing_pivot",
            "PIVOT Aggregate has no `pivot` sub-message",
        )?;
        let pivot_col_proto = pivot_proto.col.as_ref().require_proto(
            "Aggregate::Pivot::missing_col",
            "PIVOT Aggregate::pivot has no `col`",
        )?;
        let input = self.convert_input(a.input.as_deref(), "Aggregate[Pivot]")?;
        let pivot_column = self.expr.convert(pivot_col_proto)?;
        let grouping = self.expr.convert_all(&a.grouping_expressions)?;
        let pivot_values = pivot_proto
            .values
            .iter()
            .map(|lit| self.expr.convert_literal(lit))
            .collect::<Result<Vec<_>, _>>()?;
        let aggregates = self.expr.convert_all(&a.aggregate_expressions)?;
        Ok(CommonAst::new(CommonOp::Pivot {
            input: Box::new(input),
            grouping: PivotGrouping::Explicit(grouping),
            pivot_column,
            pivot_values,
            aggregates,
        }))
    }

    fn convert_read(&mut self, r: &proto::Read) -> Result<CommonAst, EmissionError> {
        use proto::read::ReadType;
        match &r.read_type {
            Some(ReadType::NamedTable(nt)) => Ok(CommonAst::new(CommonOp::TableScan {
                table: nt.unparsed_identifier.clone(),
                alias: None,
            })),
            Some(ReadType::DataSource(ds)) => {
                if ds.paths.is_empty() {
                    bail_boundary_proto!(
                        "Read::DataSource::empty_paths",
                        "DataSource has no paths",
                    );
                }
                let format = classify_file_format(ds.format.as_deref(), &ds.paths[0])?;
                let schema = ds
                    .schema
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(parse_type_str_to_struct);
                let options: Vec<(String, String)> = ds
                    .options
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                Ok(CommonAst::new(CommonOp::FileScan {
                    format,
                    paths: ds.paths.clone(),
                    schema,
                    options,
                }))
            }
            None => bail_boundary_proto!("Read::missing_read_type", "Read has no read_type"),
        }
    }

    fn convert_local_relation(
        &self,
        lr: &proto::LocalRelation,
    ) -> Result<CommonAst, EmissionError> {
        let (arrow_schema, rows) = match &lr.data {
            Some(data) if !data.is_empty() => {
                let (s, r) = arrow_ipc_to_schema_and_rows(data)?;
                (Some(s), r)
            }
            _ => (None, vec![]),
        };
        // Prefer the Spark-visible JSON schema (from `_schema.json()`) over
        // the Arrow-derived schema. PySpark's client dedups struct field
        // names for the Arrow wire (`_deduplicate_field_names`) but sends the
        // ORIGINAL Spark schema in `lr.schema` — the JSON path preserves
        // duplicate field names (e.g. `arrays_zip("tags","tags")` →
        // `Struct<tags, tags>`), which the round-trip through
        // `sparkSession.createDataFrame(rows, df.schema)` needs to match
        // Spark's reference behaviour on arr-012. Fall back to the Arrow
        // schema only when no `lr.schema` is provided.
        let schema = match lr.schema.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(s) => parse_type_str_to_struct(s),
            None => arrow_schema.unwrap_or_else(StructType::empty),
        };
        Ok(CommonAst::new(CommonOp::LocalRelation { schema, rows }))
    }

    fn convert_join(&mut self, j: &proto::Join) -> Result<CommonAst, EmissionError> {
        use proto::join::JoinType as ProtoJoinType;
        let left_proto = j
            .left
            .as_deref()
            .require_proto("Join::missing_left", "Join has no left input")?;
        let right_proto = j
            .right
            .as_deref()
            .require_proto("Join::missing_right", "Join has no right input")?;
        let join_type = match j.join_type() {
            ProtoJoinType::Inner | ProtoJoinType::Unspecified => JoinType::Inner,
            ProtoJoinType::FullOuter => JoinType::Full,
            ProtoJoinType::LeftOuter => JoinType::Left,
            ProtoJoinType::RightOuter => JoinType::Right,
            ProtoJoinType::LeftAnti => JoinType::LeftAnti,
            ProtoJoinType::LeftSemi => JoinType::LeftSemi,
            ProtoJoinType::Cross => JoinType::Cross,
        };
        let mut left_ids: HashSet<i64> = HashSet::new();
        let mut right_ids: HashSet<i64> = HashSet::new();
        collect_relation_plan_ids(left_proto, &mut left_ids);
        collect_relation_plan_ids(right_proto, &mut right_ids);
        let condition = match &j.join_condition {
            Some(c) => Some(self.expr.convert(c)?),
            None => None,
        };
        let left = self.convert(left_proto)?;
        let right = self.convert(right_proto)?;
        let mut left_plan_ids: Vec<i64> = left_ids.into_iter().collect();
        let mut right_plan_ids: Vec<i64> = right_ids.into_iter().collect();
        // Deterministic ordering — HashSet iteration is unstable.
        left_plan_ids.sort_unstable();
        right_plan_ids.sort_unstable();
        Ok(CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type,
            condition,
            using_columns: j.using_columns.clone(),
            // Spark Connect's `Join.JoinType` proto has no NATURAL variant —
            // the DataFrame front-end can never produce a NATURAL join; only
            // the SparkSQL front-end (`v2_lowering.rs`) can set this true.
            natural: false,
            // Spark 4.x's `LateralJoin` proto relation is deferred to a
            // future pass — the DataFrame front-end has no equivalent syntax
            // for explicit `JOIN LATERAL (subquery)` yet.
            lateral: false,
            left_plan_ids,
            right_plan_ids,
        }))
    }
}

// ── Private expression converter ───────────────────────────────────────────

struct V2ExpressionConverter;

impl V2ExpressionConverter {
    fn new() -> Self {
        Self
    }

    /// Convert a slice of proto expressions, short-circuiting on the first
    /// conversion error.
    fn convert_all(
        &mut self,
        exprs: &[proto::Expression],
    ) -> Result<Vec<Expression>, EmissionError> {
        exprs.iter().map(|e| self.convert(e)).collect()
    }

    fn convert(&mut self, expr: &proto::Expression) -> Result<Expression, EmissionError> {
        use proto::expression::ExprType;
        let expr_type = expr
            .expr_type
            .as_ref()
            .require_proto("Expression::None", "proto Expression carries no expr_type")?;
        match expr_type {
            ExprType::Literal(lit) => self.convert_literal(lit),
            ExprType::UnresolvedAttribute(attr) => self.convert_unresolved_attribute(attr),
            ExprType::UnresolvedFunction(func) => self.convert_unresolved_function(func),
            ExprType::Alias(alias) => self.convert_alias(alias),
            ExprType::Cast(cast) => self.convert_cast(cast),
            ExprType::UnresolvedStar(star) => {
                // Spark's `select("address.*")` sends `unparsed_target = "address.*"`
                // (including the trailing `.*`). The analyzer wants just the
                // qualifier (`"address"`) — strip the star suffix. A bare `*`
                // arrives as `None`. Corpus witness: `struct-008`.
                let qualifier = star.unparsed_target.as_ref().map(|s| {
                    if let Some(base) = s.strip_suffix(".*") {
                        base.to_owned()
                    } else {
                        s.clone()
                    }
                });
                Ok(Expression::Star(StarExpression { qualifier }))
            }
            ExprType::LambdaFunction(lf) => {
                let func_proto = lf.function.as_deref().require_proto(
                    "LambdaFunction::function::None",
                    "LambdaFunction missing body",
                )?;
                let body = self.convert(func_proto)?;
                let params = lf
                    .arguments
                    .iter()
                    .map(|arg| {
                        single_name_part(
                            &arg.name_parts,
                            "LambdaFunction::arg::multi_part",
                            "lambda argument name must be a single part",
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Expression::Lambda(LambdaExpression {
                    params,
                    body: Box::new(body),
                }))
            }
            ExprType::UnresolvedNamedLambdaVariable(v) => {
                let name = single_name_part(
                    &v.name_parts,
                    "UnresolvedNamedLambdaVariable::multi_part",
                    "lambda variable name must be a single part",
                )?;
                Ok(Expression::LambdaVariable(LambdaVariableExpression {
                    name,
                }))
            }
            ExprType::Window(w) => self.convert_window(w),
            ExprType::UnresolvedExtractValue(uev) => {
                let child = uev.child.as_deref().require_proto(
                    "UnresolvedExtractValue::child::None",
                    "ExtractValue missing child expression",
                )?;
                let extraction = uev.extraction.as_deref().require_proto(
                    "UnresolvedExtractValue::extraction::None",
                    "ExtractValue missing extraction expression",
                )?;
                let child = self.convert(child)?;
                let extraction = self.convert(extraction)?;
                Ok(Expression::ExtractValue(ExtractValueExpression {
                    child: Box::new(child),
                    extraction: Box::new(extraction),
                }))
            }
            ExprType::ExpressionString(es) => {
                // Spark's `F.expr("<sql>")` / `df.selectExpr("<sql>")` — a
                // raw SparkSQL expression fragment. Route through τ's
                // SparkSQL parser so the analyzer can type-resolve the
                // resulting expression (RawSql passthrough would leak an
                // `Unresolved` DataType into `analyze_plan(Schema)` and
                // PySpark would reject the response with
                // `PySparkValueError: data type unparsed`).
                thunderduck_core::parser_v2::SparkSqlParserV2::parse_expression(&es.expression)
            }
            ExprType::UpdateFields(uf) => {
                // Spark Connect chains withField / dropFields as nested
                // `UpdateFields` protos — each carries one op and points at
                // its predecessor via `struct_expression`. Flatten into a
                // single [`UpdateFieldsExpression`] with an ordered
                // `updates: Vec<(String, Option<Expression>)>`.
                let (base_proto, ops) = flatten_update_fields(uf);
                let base_proto = base_proto.require_proto(
                    "UpdateFields::struct_expression::None",
                    "UpdateFields missing struct_expression",
                )?;
                let struct_expr = self.convert(base_proto)?;
                let mut updates: Vec<(String, Option<Expression>)> = Vec::with_capacity(ops.len());
                for (field_name, maybe_value) in ops {
                    let converted_value = match maybe_value {
                        Some(v) => Some(self.convert(v)?),
                        None => None,
                    };
                    updates.push((field_name, converted_value));
                }
                Ok(Expression::UpdateFields(UpdateFieldsExpression {
                    struct_expr: Box::new(struct_expr),
                    updates,
                }))
            }
            ExprType::UnresolvedRegex(ur) => Ok(convert_unresolved_regex(ur)),
            other => bail_boundary_proto!(
                format!("Expression::{}", expr_type_kind(other)),
                "expression shape not covered by V2ExpressionConverter",
            ),
        }
    }

    fn convert_sort_order(
        &mut self,
        so: &proto::expression::SortOrder,
    ) -> Result<SortOrder, EmissionError> {
        use proto::expression::sort_order::{NullOrdering as ProtoNO, SortDirection as ProtoSD};
        let child = so.child.as_deref().require_proto(
            "SortOrder::missing_child",
            "SortOrder has no child expression",
        )?;
        let expr = self.convert(child)?;
        // Mirrors Spark's `SparkConnectPlanner.scala::transformSortOrder`,
        // which decodes the proto with catch-all defaults (not a
        // derive-from-direction rule — that rule is `SortOrder.scala`'s
        // `defaultNullOrdering`, which only applies where Spark constructs a
        // `SortOrder` from SQL text without an explicit ordering; the SQL
        // front-end's `lower_order_by_expr` already matches that separately).
        // The proto decoder's unspecified-field defaults are Descending /
        // NullsLast, not Ascending / NullsFirst.
        let direction = match so.direction() {
            ProtoSD::Ascending => SortDirection::Ascending,
            _ => SortDirection::Descending,
        };
        let null_ordering = match so.null_ordering() {
            ProtoNO::SortNullsFirst => NullOrdering::NullsFirst,
            _ => NullOrdering::NullsLast,
        };
        Ok(SortOrder {
            expr: Box::new(expr),
            direction,
            null_ordering,
        })
    }

    /// Convert a proto `Window` expression: window function + PARTITION BY /
    /// ORDER BY specs + optional frame.
    fn convert_window(
        &mut self,
        w: &proto::expression::Window,
    ) -> Result<Expression, EmissionError> {
        let func_proto = w.window_function.as_deref().require_proto(
            "Window::window_function::None",
            "Window missing window_function",
        )?;
        let func = self.convert(func_proto)?;
        let partition_by = self.convert_all(&w.partition_spec)?;
        let order_by = w
            .order_spec
            .iter()
            .map(|so| self.convert_sort_order(so))
            .collect::<Result<Vec<_>, _>>()?;
        let frame = match w.frame_spec.as_deref() {
            Some(fs) => self.convert_window_frame(fs)?,
            None => None,
        };
        Ok(Expression::Window(WindowFunction {
            func: Box::new(func),
            partition_by,
            order_by,
            frame,
        }))
    }

    /// Parse a proto window frame spec. `FrameType::Undefined` carries no
    /// frame semantics — returns `Ok(None)` so the caller omits the frame.
    fn convert_window_frame(
        &mut self,
        fs: &proto::expression::window::WindowFrame,
    ) -> Result<Option<thunderduck_core::transpiler_v2::expression::WindowFrame>, EmissionError>
    {
        use proto::expression::window::window_frame as pwf;
        use thunderduck_core::transpiler_v2::expression::{
            FrameBoundary as VFB, FrameUnit as VFU, WindowFrame as VWF,
        };
        let unit =
            match pwf::FrameType::try_from(fs.frame_type).unwrap_or(pwf::FrameType::Undefined) {
                pwf::FrameType::Row => VFU::Rows,
                pwf::FrameType::Range => VFU::Range,
                pwf::FrameType::Undefined => return Ok(None),
            };
        let mut convert_boundary =
            |b_opt: Option<&pwf::FrameBoundary>, is_lower: bool| -> Result<VFB, EmissionError> {
                let b = b_opt.require_proto(
                    "Window::frame_spec::missing_boundary",
                    "frame boundary missing",
                )?;
                use pwf::frame_boundary::Boundary as PB;
                match b.boundary.as_ref() {
                    Some(PB::CurrentRow(_)) => Ok(VFB::CurrentRow),
                    Some(PB::Unbounded(_)) => Ok(if is_lower {
                        VFB::UnboundedPreceding
                    } else {
                        VFB::UnboundedFollowing
                    }),
                    Some(PB::Value(v)) => {
                        let expr = self.convert(v)?;
                        // Spark encodes offsets as signed
                        // numeric literals: negative = PRECEDING,
                        // positive = FOLLOWING. Match here.
                        if let Expression::Literal(l) = &expr {
                            use thunderduck_core::transpiler_v2::expression::LiteralValue as LV;
                            let sign: Option<i64> = match &l.value {
                                LV::Int(i) => Some(*i as i64),
                                LV::Long(i) => Some(*i),
                                _ => None,
                            };
                            if let Some(n) = sign {
                                let abs_expr = lit(LV::Long(n.abs()), DataType::Long);
                                return Ok(if n < 0 {
                                    VFB::Preceding(Box::new(abs_expr))
                                } else {
                                    VFB::Following(Box::new(abs_expr))
                                });
                            }
                        }
                        Ok(if is_lower {
                            VFB::Preceding(Box::new(expr))
                        } else {
                            VFB::Following(Box::new(expr))
                        })
                    }
                    None => bail_boundary_proto!(
                        "Window::frame_boundary::None",
                        "frame boundary carries no shape",
                    ),
                }
            };
        let lower = convert_boundary(fs.lower.as_deref(), true)?;
        let upper = convert_boundary(fs.upper.as_deref(), false)?;
        Ok(Some(VWF { unit, lower, upper }))
    }

    fn convert_literal(
        &self,
        proto_lit: &proto::expression::Literal,
    ) -> Result<Expression, EmissionError> {
        use proto::expression::literal::LiteralType;
        let lt = match &proto_lit.literal_type {
            Some(l) => l,
            None => return Ok(null_literal()),
        };
        Ok(match lt {
            LiteralType::Null(_) => null_literal(),
            LiteralType::Boolean(b) => lit(LiteralValue::Boolean(*b), DataType::Boolean),
            LiteralType::Byte(v) => lit(LiteralValue::Byte(*v as i8), DataType::Byte),
            LiteralType::Short(v) => lit(LiteralValue::Short(*v as i16), DataType::Short),
            LiteralType::Integer(v) => lit(LiteralValue::Int(*v), DataType::Integer),
            LiteralType::Long(v) => lit(LiteralValue::Long(*v), DataType::Long),
            LiteralType::Float(v) => lit(LiteralValue::Float(*v), DataType::Float),
            LiteralType::Double(v) => lit(LiteralValue::Double(*v), DataType::Double),
            LiteralType::String(s) => lit(LiteralValue::String(s.clone()), DataType::String),
            LiteralType::Binary(b) => lit(LiteralValue::Binary(b.clone()), DataType::Binary),
            LiteralType::Date(d) => lit(LiteralValue::Date(*d), DataType::Date),
            LiteralType::Timestamp(ts) => lit(LiteralValue::Timestamp(*ts), DataType::Timestamp),
            LiteralType::TimestampNtz(ts) => {
                lit(LiteralValue::TimestampNtz(*ts), DataType::TimestampNtz)
            }
            LiteralType::Decimal(d) => {
                let server_precision = d.precision.map(|p| p as u8);
                let server_scale = d.scale.map(|s| s as u8);
                let (precision, scale) =
                    normalize_decimal_literal(&d.value, server_precision, server_scale);
                lit(
                    LiteralValue::Decimal {
                        value: d.value.clone(),
                        precision,
                        scale,
                    },
                    DataType::Decimal { precision, scale },
                )
            }
            other => {
                bail_boundary_proto!(
                    format!("Literal::{}", literal_kind(other)),
                    "literal type not covered by V2ExpressionConverter",
                );
            }
        })
    }

    fn convert_unresolved_attribute(
        &self,
        attr: &proto::expression::UnresolvedAttribute,
    ) -> Result<Expression, EmissionError> {
        let name = &attr.unparsed_identifier;
        if name == "*" {
            return Ok(Expression::Star(StarExpression { qualifier: None }));
        }
        // §2.3: plan_id is first-class (Option<i64>), not string-encoded.
        if let Some(plan_id) = attr.plan_id {
            let col_name = name.split('.').next_back().unwrap_or(name).to_owned();
            return Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name: col_name,
                qualifier: None,
                plan_id: Some(plan_id),
            }));
        }
        let parts: Vec<&str> = name.splitn(2, '.').collect();
        if parts.len() == 2 {
            Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name: parts[1].to_owned(),
                qualifier: Some(parts[0].to_owned()),
                plan_id: None,
            }))
        } else {
            Ok(Expression::UnresolvedColumn(UnresolvedColumn {
                name: name.clone(),
                qualifier: None,
                plan_id: None,
            }))
        }
    }

    fn convert_unresolved_function(
        &mut self,
        func: &proto::expression::UnresolvedFunction,
    ) -> Result<Expression, EmissionError> {
        let args = self.convert_all(&func.arguments)?;
        if args.len() == 2 {
            let op = match func.function_name.as_str() {
                ">" => Some(BinaryOp::Gt),
                ">=" => Some(BinaryOp::GtEq),
                "<" => Some(BinaryOp::Lt),
                "<=" => Some(BinaryOp::LtEq),
                "=" | "==" => Some(BinaryOp::Eq),
                "!=" | "<>" => Some(BinaryOp::NotEq),
                "and" | "&&" => Some(BinaryOp::And),
                "or" | "||" => Some(BinaryOp::Or),
                "+" => Some(BinaryOp::Add),
                "-" => Some(BinaryOp::Sub),
                "*" => Some(BinaryOp::Mul),
                "/" => Some(BinaryOp::Div),
                "%" => Some(BinaryOp::Mod),
                _ => None,
            };
            if let Some(op) = op {
                let mut args = args;
                let right = args.remove(1);
                let left = args.remove(0);
                return Ok(Expression::Binary(BinaryExpression {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                }));
            }
        }
        if args.len() == 1 {
            let op = match func.function_name.as_str() {
                "not" | "!" => Some(UnaryOp::Not),
                "isnull" => Some(UnaryOp::IsNull),
                "isnotnull" => Some(UnaryOp::IsNotNull),
                _ => None,
            };
            if let Some(op) = op {
                let mut args = args;
                let operand = args.remove(0);
                return Ok(Expression::Unary(UnaryExpression {
                    op,
                    operand: Box::new(operand),
                }));
            }
        }
        // CASE WHEN emitted as function "when" with alternating pairs
        if func.function_name.eq_ignore_ascii_case("when") && !args.is_empty() {
            let mut branches: Vec<(Expression, Expression)> = Vec::new();
            let mut iter = args.into_iter();
            let mut else_expr: Option<Box<Expression>> = None;
            while let Some(cond) = iter.next() {
                match iter.next() {
                    Some(then) => branches.push((cond, then)),
                    None => else_expr = Some(Box::new(cond)),
                }
            }
            return Ok(Expression::CaseWhen(CaseWhenExpression {
                branches,
                else_expr,
            }));
        }
        // IN list emitted as function "in" with (expr, v1, v2, ...) arguments
        if func.function_name.eq_ignore_ascii_case("in") && args.len() >= 2 {
            let mut args = args;
            let expr = args.remove(0);
            return Ok(Expression::InList(InListExpression {
                expr: Box::new(expr),
                list: args,
                negated: false,
            }));
        }
        Ok(Expression::FunctionCall(FunctionCall {
            name: func.function_name.clone(),
            args,
            distinct: func.is_distinct,
        }))
    }

    fn convert_alias(
        &mut self,
        alias: &proto::expression::Alias,
    ) -> Result<Expression, EmissionError> {
        let inner = alias
            .expr
            .as_deref()
            .require_proto("Alias::missing_expr", "Alias has no inner expression")?;
        let expr = self.convert(inner)?;
        let name = alias
            .name
            .first()
            .cloned()
            .unwrap_or_else(|| "_col".to_owned());
        Ok(Expression::Alias(AliasExpression {
            expr: Box::new(expr),
            alias: name,
        }))
    }

    /// If `e` is `Alias(name=[a, b], expr=posexplode(arr))` (or
    /// `posexplode_outer`), return the two synthetic projections
    /// `Alias(posexplode_pos(arr), a)` and `Alias(posexplode_val(arr), b)`.
    /// Otherwise return `Ok(None)` — the caller falls back to normal
    /// single-expression conversion. Corpus: arr-017.
    ///
    /// Encapsulated here (in the expression converter) so `convert_project`
    /// can splice the two-projection expansion without knowing the proto shape.
    ///
    /// **`F.inline` / `F.inline_outer` boundary (Pass 90).** Unlike
    /// `posexplode`, inline/inline_outer are NOT split at the converter — the
    /// N-column widening depends on the resolved `Array<Struct<...>>` schema,
    /// which is only available inside the analyzer. Inline projections reach
    /// the analyzer as a plain `FunctionCall("inline"|"inline_outer", [arr])`
    /// and are expanded by `analyzer::expand_inline_projections` (τ's Project
    /// pre-pass, next to `expand_regex_projections`). Corpus: inl-001, inl-002.
    ///
    /// **`F.json_tuple` boundary (Pass 91).** Same shape: `json_tuple(json,
    /// k1, ..., kN)` reaches the analyzer as a plain `FunctionCall` and is
    /// expanded by `analyzer::expand_json_tuple_projections` into N synthetic
    /// `Alias(json_tuple_field(json, "<ki>"), "c<i>")` projections
    /// (positional `c0, c1, ...` names, matching Spark's
    /// `Generator.elementSchema`). Multi-alias `json_tuple(...).alias(a, b)`
    /// is a follow-up — corpus witness (json-002) uses positional names.
    /// Corpus: json-002.
    fn try_convert_posexplode_multi_alias(
        &mut self,
        e: &proto::Expression,
    ) -> Result<Option<(Expression, Expression)>, EmissionError> {
        use proto::expression::ExprType;
        let Some(ExprType::Alias(alias)) = e.expr_type.as_ref() else {
            return Ok(None);
        };
        // Only two-name aliases participate; single-name aliases route through
        // the normal `convert_alias` path.
        if alias.name.len() != 2 {
            return Ok(None);
        }
        let Some(inner) = alias.expr.as_deref() else {
            return Ok(None);
        };
        let Some(ExprType::UnresolvedFunction(func)) = inner.expr_type.as_ref() else {
            return Ok(None);
        };
        let name_lower = func.function_name.to_ascii_lowercase();
        // `posexplode(arr).alias(pos, val)` → two synthetic projections
        // `posexplode_pos(arr) AS pos` + `posexplode_val(arr) AS val`.
        //
        // `explode(map).alias(k, v)` → two synthetic projections
        // `map_explode_key(m) AS k` + `map_explode_val(m) AS v`. Spark's
        // `explode` on a MAP fans out one row per key/value pair; the
        // two-name Alias is Spark's signal that the operand is a MAP (arrays
        // reject two-name aliases). Emission converts the synthetic
        // `map_explode_key/val(m)` names to `UNNEST(map_keys/map_values(m))`.
        //
        // Corpus: arr-017 (posexplode), map-007 (map explode).
        let is_pos = matches!(name_lower.as_str(), "posexplode" | "posexplode_outer");
        let is_map_explode = matches!(name_lower.as_str(), "explode" | "explode_outer");
        if !is_pos && !is_map_explode {
            return Ok(None);
        }
        if func.arguments.len() != 1 {
            bail_boundary_proto!(
                format!("{}::arity", name_lower),
                format!(
                    "`{}` with a two-name Alias requires exactly 1 argument, got {}",
                    func.function_name,
                    func.arguments.len()
                ),
            );
        }
        let arg = self.convert(&func.arguments[0])?;
        let a_name = alias.name[0].clone();
        let b_name = alias.name[1].clone();
        let (a_fn_name, b_fn_name) = if is_pos {
            ("posexplode_pos", "posexplode_val")
        } else {
            ("map_explode_key", "map_explode_val")
        };
        let a_fn = Expression::FunctionCall(FunctionCall {
            name: a_fn_name.to_owned(),
            args: vec![arg.clone()],
            distinct: false,
        });
        let b_fn = Expression::FunctionCall(FunctionCall {
            name: b_fn_name.to_owned(),
            args: vec![arg],
            distinct: false,
        });
        Ok(Some((
            Expression::Alias(AliasExpression {
                expr: Box::new(a_fn),
                alias: a_name,
            }),
            Expression::Alias(AliasExpression {
                expr: Box::new(b_fn),
                alias: b_name,
            }),
        )))
    }

    fn convert_cast(
        &mut self,
        cast: &proto::expression::Cast,
    ) -> Result<Expression, EmissionError> {
        use proto::expression::cast::CastToType;
        let inner = cast
            .expr
            .as_deref()
            .require_proto("Cast::missing_expr", "Cast has no inner expression")?;
        let to_type = match &cast.cast_to_type {
            Some(CastToType::Type(dt)) => proto_to_data_type(dt),
            Some(CastToType::TypeStr(s)) => parse_type_str(s),
            None => {
                bail_boundary_proto!("Cast::missing_to_type", "Cast has no target type");
            }
        };
        let try_cast = matches!(cast.eval_mode(), proto::expression::cast::EvalMode::Try);
        Ok(Expression::Cast(CastExpression {
            expr: Box::new(self.convert(inner)?),
            to_type,
            try_cast,
            implicit: false,
        }))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Extract a column name from a proto `Expression` if it's a bare
/// `UnresolvedAttribute`. Returns `None` for anything more elaborate — the
/// caller decides whether to surface a Thunderduck-boundary error.
fn extract_column_name(expr: &proto::Expression) -> Option<String> {
    match expr.expr_type.as_ref()? {
        proto::expression::ExprType::UnresolvedAttribute(attr) => {
            Some(attr.unparsed_identifier.clone())
        }
        _ => None,
    }
}

/// Construct an [`Expression::UnresolvedColumn`] from a bare column name.
fn unresolved_col(name: &str) -> Expression {
    Expression::UnresolvedColumn(UnresolvedColumn {
        name: name.to_owned(),
        qualifier: None,
        plan_id: None,
    })
}

/// Wrap a column reference in `COALESCE(col, 0)` — fills NULLs with zero.
///
/// Used by [`V2RelationConverter::convert_cov`] to match Spark's
/// `StatFunctions.calculateCovImpl` semantics, which calls `na.fill(0L)`
/// before `covar_samp`.
fn coalesce_zero(col_name: &str) -> Expression {
    Expression::FunctionCall(FunctionCall {
        name: "coalesce".to_owned(),
        args: vec![
            unresolved_col(col_name),
            lit(LiteralValue::Long(0), DataType::Long),
        ],
        distinct: false,
    })
}

fn null_literal() -> Expression {
    Expression::Literal(Literal {
        value: LiteralValue::Null,
        data_type: DataType::Null,
    })
}

/// Construct an [`Expression::Literal`] from a value / data-type pair —
/// collapses the `Expression::Literal(Literal { value, data_type })`
/// constructor boilerplate at every typed literal dispatch arm.
fn lit(value: LiteralValue, data_type: DataType) -> Expression {
    Expression::Literal(Literal { value, data_type })
}

/// Require `parts` to carry exactly one name part, returning it owned.
/// Anything else is a `ProtoShape`-kinded [`EmissionError::Unsupported`]
/// with the caller's `name` / `reason` strings.
fn single_name_part(parts: &[String], name: &str, reason: &str) -> Result<String, EmissionError> {
    match parts {
        [n] => Ok(n.clone()),
        _ => Err(EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: name.to_owned(),
            reason: reason.to_owned(),
        }),
    }
}

/// Convert an Arrow schema [`Field`] into a τ [`StructField`], preserving
/// name and nullability.
pub(crate) fn arrow_field_to_struct_field(f: &Field) -> Result<StructField, EmissionError> {
    let dt = arrow_data_type_to_core(f.data_type())?;
    Ok(if f.is_nullable() {
        StructField::nullable(f.name().clone(), dt)
    } else {
        StructField::not_null(f.name().clone(), dt)
    })
}

/// Convert Spark Connect `ExprType::UnresolvedRegex` into τ's
/// `Expression::UnresolvedRegex`. Mirrors Java Thunderduck's
/// `RegexColumnExpression.stripBackticks`: if `col_name` begins with a single
/// `` ` `` AND ends with a single `` ` `` (and has room for both), the
/// backticks are stripped; otherwise `col_name` passes through verbatim.
///
/// The pattern MUST be a Rust `regex` crate expression once the analyzer's
/// `expand_regex_projections` pre-pass receives it — invalid patterns surface
/// as `AnalyzerError::Other` (Spark-emulated).
fn convert_unresolved_regex(ur: &proto::expression::UnresolvedRegex) -> Expression {
    let pattern = strip_regex_backticks(&ur.col_name);
    Expression::UnresolvedRegex(UnresolvedRegexExpression {
        pattern,
        plan_id: ur.plan_id,
    })
}

/// Strip a single leading `` ` `` AND a single trailing `` ` `` from `name`
/// when both are present and `name.len() >= 2`. All other inputs pass through
/// unchanged.
fn strip_regex_backticks(name: &str) -> String {
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes.first() == Some(&b'`') && bytes.last() == Some(&b'`') {
        name[1..name.len() - 1].to_owned()
    } else {
        name.to_owned()
    }
}

fn rel_type_kind(rt: &proto::relation::RelType) -> &'static str {
    use proto::relation::RelType::*;
    match rt {
        Project(_) => "Project",
        Filter(_) => "Filter",
        Aggregate(_) => "Aggregate",
        Sort(_) => "Sort",
        Limit(_) => "Limit",
        Offset(_) => "Offset",
        Read(_) => "Read",
        LocalRelation(_) => "LocalRelation",
        Join(_) => "Join",
        Sql(_) => "Sql",
        SetOp(_) => "SetOp",
        Sample(_) => "Sample",
        SubqueryAlias(_) => "SubqueryAlias",
        WithColumns(_) => "WithColumns",
        WithColumnsRenamed(_) => "WithColumnsRenamed",
        Deduplicate(_) => "Deduplicate",
        Drop(_) => "Drop",
        Range(_) => "Range",
        Tail(_) => "Tail",
        Repartition(_) => "Repartition",
        RepartitionByExpression(_) => "RepartitionByExpression",
        ShowString(_) => "ShowString",
        Hint(_) => "Hint",
        Unpivot(_) => "Unpivot",
        ToSchema(_) => "ToSchema",
        ToDf(_) => "ToDf",
        FillNa(_) => "FillNa",
        DropNa(_) => "DropNa",
        Replace(_) => "Replace",
        Summary(_) => "Summary",
        Describe(_) => "Describe",
        Cov(_) => "Cov",
        Corr(_) => "Corr",
        ApproxQuantile(_) => "ApproxQuantile",
        Crosstab(_) => "Crosstab",
        FreqItems(_) => "FreqItems",
        SampleBy(_) => "SampleBy",
        Catalog(_) => "Catalog",
        WithRelations(_) => "WithRelations",
        _ => "Other",
    }
}

fn expr_type_kind(et: &proto::expression::ExprType) -> &'static str {
    use proto::expression::ExprType::*;
    match et {
        Literal(_) => "Literal",
        UnresolvedAttribute(_) => "UnresolvedAttribute",
        UnresolvedFunction(_) => "UnresolvedFunction",
        ExpressionString(_) => "ExpressionString",
        UnresolvedStar(_) => "UnresolvedStar",
        Alias(_) => "Alias",
        Cast(_) => "Cast",
        Window(_) => "Window",
        LambdaFunction(_) => "LambdaFunction",
        UnresolvedNamedLambdaVariable(_) => "LambdaVariable",
        SortOrder(_) => "SortOrder",
        UpdateFields(_) => "UpdateFields",
        UnresolvedRegex(_) => "UnresolvedRegex",
        NamedArgumentExpression(_) => "NamedArgument",
        SubqueryExpression(_) => "SubqueryExpression",
        CallFunction(_) => "CallFunction",
        UnresolvedExtractValue(_) => "UnresolvedExtractValue",
        _ => "Other",
    }
}

fn literal_kind(lt: &proto::expression::literal::LiteralType) -> &'static str {
    use proto::expression::literal::LiteralType::*;
    match lt {
        Null(_) => "Null",
        Boolean(_) => "Boolean",
        Byte(_) => "Byte",
        Short(_) => "Short",
        Integer(_) => "Integer",
        Long(_) => "Long",
        Float(_) => "Float",
        Double(_) => "Double",
        String(_) => "String",
        Binary(_) => "Binary",
        Date(_) => "Date",
        Timestamp(_) => "Timestamp",
        TimestampNtz(_) => "TimestampNtz",
        Decimal(_) => "Decimal",
        YearMonthInterval(_) => "YearMonthInterval",
        DayTimeInterval(_) => "DayTimeInterval",
        CalendarInterval(_) => "CalendarInterval",
        Array(_) => "Array",
        Map(_) => "Map",
        Struct(_) => "Struct",
        SpecializedArray(_) => "SpecializedArray",
        Time(_) => "Time",
    }
}

/// Flatten a chain of nested `UpdateFields` protos into `(base, ops)`.
///
/// Spark Connect emits `df.col("s").withField("a", va).withField("b", vb)` as
/// `UpdateFields(field="b", value=Some(vb), struct=UpdateFields(field="a",
/// value=Some(va), struct=<col "s">))`. The outermost proto is the *most
/// recent* op; the innermost `struct_expression` that is NOT an `UpdateFields`
/// is the base struct. Returns ops in **application order** — the innermost
/// (oldest) op is index 0, the outermost (newest) is last.
fn flatten_update_fields(
    outer: &proto::expression::UpdateFields,
) -> (
    Option<&proto::Expression>,
    Vec<(String, Option<&proto::Expression>)>,
) {
    use proto::expression::ExprType;
    let mut ops_rev: Vec<(String, Option<&proto::Expression>)> = Vec::new();
    let mut cursor: &proto::expression::UpdateFields = outer;
    let base: Option<&proto::Expression> = loop {
        ops_rev.push((
            cursor.field_name.clone(),
            cursor.value_expression.as_deref(),
        ));
        match cursor.struct_expression.as_deref() {
            Some(next) => match &next.expr_type {
                Some(ExprType::UpdateFields(inner)) => {
                    cursor = inner;
                }
                _ => break Some(next),
            },
            None => break None,
        }
    };
    ops_rev.reverse();
    (base, ops_rev)
}

fn classify_file_format(
    format: Option<&str>,
    first_path: &str,
) -> Result<FileFormat, EmissionError> {
    let explicit = format.map(|s| s.to_ascii_lowercase());
    let lower = first_path.to_ascii_lowercase();
    let kind = match explicit.as_deref() {
        Some("parquet") => Some(FileFormat::Parquet),
        Some("csv") | Some("text") => Some(FileFormat::Csv),
        Some("json") => Some(FileFormat::Json),
        Some("orc") => Some(FileFormat::Orc),
        Some("delta") => Some(FileFormat::Delta),
        Some("") | None => None,
        Some(other) => {
            bail_boundary_proto!(
                format!("Read::DataSource::format::{other}"),
                "file format not supported by τ\'s SparkSQL parser",
            );
        }
    };
    if let Some(k) = kind {
        return Ok(k);
    }
    // Fall back to path extension.
    if lower.ends_with(".parquet") {
        Ok(FileFormat::Parquet)
    } else if lower.ends_with(".csv") || lower.ends_with(".tsv") {
        Ok(FileFormat::Csv)
    } else if lower.ends_with(".json") || lower.ends_with(".jsonl") || lower.ends_with(".ndjson") {
        Ok(FileFormat::Json)
    } else if lower.ends_with(".orc") {
        Ok(FileFormat::Orc)
    } else {
        Err(EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: "Read::DataSource::format::unknown".to_owned(),
            reason: format!(
                "cannot determine file format for `{first_path}` (no format and unknown extension)"
            ),
        })
    }
}

fn parse_type_str_to_struct(s: &str) -> StructType {
    // PySpark sends the LocalRelation `schema` field as a JSON-serialized
    // Spark type when the client calls `createDataFrame(rows, schema)` —
    // `_schema.json()` emits `{"type":"struct","fields":[…]}`. Delegate JSON
    // parsing to the shared helper in `relation_converter`. This path
    // preserves duplicate struct field names (`Struct<tags, tags>` from
    // `arrays_zip`), which the Arrow-IPC-derived schema lacks because
    // PySpark's client dedups struct field names before wire serialization.
    let trimmed = s.trim();
    if trimmed.starts_with('{') {
        // A `{`-leading schema is ALWAYS decoded as JSON — `parse_json_schema`
        // is total (invalid JSON or a missing `"fields"` array yields the
        // empty struct), so a JSON-shaped string never falls through to the
        // DDL parser below. Returning the (possibly empty) result verbatim
        // preserves that long-standing behavior.
        return super::relation_converter::parse_json_schema(trimmed);
    }
    // Fallback: Spark DDL schema parser — accepts both the `struct<...>`
    // wrapper form (`struct<id:bigint,name:string>`) and the bare field-list
    // form PySpark sends for `DataFrameReader.schema("id INT, name STRING")`.
    // Pass-2 widening: the legacy fallback routed through `parse_type_str`,
    // which had no struct arm — its `DataType::Struct` match was unreachable
    // and every DDL-string schema silently became `StructType::empty()`.
    // Untranslatable DDL still degrades to the empty struct (legacy shape).
    thunderduck_core::types::spark_ddl::parse_spark_schema(s).unwrap_or_else(StructType::empty)
}

/// Parse an Arrow IPC stream once, returning both the schema and the row
/// literals. Consolidating both extractions eliminates a duplicated Arrow
/// schema parse and `StreamReader` construction (perf OPT-1).
fn arrow_ipc_to_schema_and_rows(
    data: &[u8],
) -> Result<(StructType, Vec<Vec<Expression>>), EmissionError> {
    let cursor = std::io::Cursor::new(data);
    let reader = StreamReader::try_new(cursor, None).map_err(|e| EmissionError::Unsupported {
        kind: UnsupportedKind::ProtoShape,
        name: "LocalRelation::arrow_ipc".to_owned(),
        reason: format!("Arrow IPC parse error: {e}"),
    })?;
    let arrow_schema = reader.schema();
    let fields = arrow_schema
        .fields()
        .iter()
        .map(|f| arrow_field_to_struct_field(f))
        .collect::<Result<Vec<_>, EmissionError>>()?;
    let schema = StructType::new(fields);
    let batches: Vec<arrow::record_batch::RecordBatch> = reader
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| EmissionError::Unsupported {
            kind: UnsupportedKind::ProtoShape,
            name: "LocalRelation::arrow_ipc_collect".to_owned(),
            reason: format!("Arrow IPC collect error: {e}"),
        })?;
    // OPT-2: known row count → single bounded allocation.
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    let mut rows: Vec<Vec<Expression>> = Vec::with_capacity(total_rows);
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let mut cells: Vec<Expression> = Vec::with_capacity(batch.num_columns());
            for col in batch.columns() {
                cells.push(arrow_val_to_literal(col.as_ref(), row)?);
            }
            rows.push(cells);
        }
    }
    Ok((schema, rows))
}

pub(crate) fn arrow_data_type_to_core(dt: &ArrowDT) -> Result<DataType, EmissionError> {
    Ok(match dt {
        ArrowDT::Null => DataType::Null,
        ArrowDT::Boolean => DataType::Boolean,
        ArrowDT::Int8 => DataType::Byte,
        ArrowDT::Int16 => DataType::Short,
        ArrowDT::Int32 => DataType::Integer,
        ArrowDT::Int64 => DataType::Long,
        ArrowDT::Float32 => DataType::Float,
        ArrowDT::Float64 => DataType::Double,
        ArrowDT::Utf8 | ArrowDT::LargeUtf8 => DataType::String,
        ArrowDT::Binary | ArrowDT::LargeBinary => DataType::Binary,
        ArrowDT::Date32 => DataType::Date,
        ArrowDT::Timestamp(_, _) => DataType::Timestamp,
        ArrowDT::Decimal128(p, s) => DataType::Decimal {
            precision: *p,
            scale: *s as u8,
        },
        // Interval types — round-trip through `createDataFrame(rows, schema)`.
        // Spark 4.1 sends `DayTimeIntervalType` as Arrow `Duration(Microsecond)`,
        // `YearMonthIntervalType` as Arrow `Interval(YEAR_MONTH)`, and
        // `CalendarIntervalType` as Arrow `Interval(MonthDayNano)`. Corpus:
        // intv-001, intv-003, intv-005.
        ArrowDT::Duration(TimeUnit::Microsecond) => DataType::DayTimeInterval,
        ArrowDT::Interval(IntervalUnit::YearMonth) => DataType::YearMonthInterval,
        ArrowDT::Interval(IntervalUnit::MonthDayNano) => DataType::Interval,
        ArrowDT::List(f) | ArrowDT::LargeList(f) => DataType::Array(
            Box::new(arrow_data_type_to_core(f.data_type())?),
            f.is_nullable(),
        ),
        ArrowDT::Map(field, _) => {
            let ArrowDT::Struct(fields) = field.data_type() else {
                bail_boundary_proto!(
                    "arrow_schema::map_non_struct",
                    "Arrow Map entries must be Struct",
                );
            };
            let key_field = fields.iter().find(|f| f.name() == "key").require_proto(
                "arrow_schema::map_missing_key",
                "Arrow Map entries missing `key`",
            )?;
            let val_field = fields.iter().find(|f| f.name() == "value").require_proto(
                "arrow_schema::map_missing_value",
                "Arrow Map entries missing `value`",
            )?;
            DataType::Map {
                key: Box::new(arrow_data_type_to_core(key_field.data_type())?),
                value: Box::new(arrow_data_type_to_core(val_field.data_type())?),
                value_nullable: val_field.is_nullable(),
            }
        }
        ArrowDT::Struct(fields) => {
            let inner = fields
                .iter()
                .map(|f| arrow_field_to_struct_field(f))
                .collect::<Result<Vec<_>, EmissionError>>()?;
            DataType::Struct(StructType::new(inner))
        }
        other => {
            bail_boundary_proto!(
                format!("arrow_schema::{other:?}"),
                "Arrow schema data type not supported by τ\'s SparkSQL parser",
            );
        }
    })
}

/// Convert a single Arrow cell into an [`Expression::Literal`].
///
/// **§2.1 loud-fail contract:** every unhandled Arrow data type returns
/// [`EmissionError::Unsupported`] (`kind: ProtoShape`). There is NO `Ok("NULL")` or
/// `Ok(null_literal())` catch-all — silent NULL substitution would turn every
/// unhandled type into wrong-answer data corruption (see the DECIMAL bug
/// documented in `local_relation_to_values_sql`).
pub(crate) fn arrow_val_to_literal(
    array: &dyn Array,
    row: usize,
) -> Result<Expression, EmissionError> {
    if array.is_null(row) {
        return Ok(null_literal());
    }
    match array.data_type() {
        ArrowDT::Null => Ok(null_literal()),
        ArrowDT::Boolean => {
            let a = downcast::<BooleanArray>(array)?;
            Ok(lit(LiteralValue::Boolean(a.value(row)), DataType::Boolean))
        }
        ArrowDT::Int8 => {
            let a = downcast::<Int8Array>(array)?;
            Ok(lit(LiteralValue::Byte(a.value(row)), DataType::Byte))
        }
        ArrowDT::Int16 => {
            let a = downcast::<Int16Array>(array)?;
            Ok(lit(LiteralValue::Short(a.value(row)), DataType::Short))
        }
        ArrowDT::Int32 => {
            let a = downcast::<Int32Array>(array)?;
            Ok(lit(LiteralValue::Int(a.value(row)), DataType::Integer))
        }
        ArrowDT::Int64 => {
            let a = downcast::<Int64Array>(array)?;
            Ok(lit(LiteralValue::Long(a.value(row)), DataType::Long))
        }
        ArrowDT::Float32 => {
            let a = downcast::<Float32Array>(array)?;
            Ok(lit(LiteralValue::Float(a.value(row)), DataType::Float))
        }
        ArrowDT::Float64 => {
            let a = downcast::<Float64Array>(array)?;
            Ok(lit(LiteralValue::Double(a.value(row)), DataType::Double))
        }
        ArrowDT::Utf8 => {
            let a = downcast::<StringArray>(array)?;
            Ok(lit(
                LiteralValue::String(a.value(row).to_owned()),
                DataType::String,
            ))
        }
        ArrowDT::LargeUtf8 => {
            let a = downcast::<LargeStringArray>(array)?;
            Ok(lit(
                LiteralValue::String(a.value(row).to_owned()),
                DataType::String,
            ))
        }
        ArrowDT::Binary => {
            let a = downcast::<BinaryArray>(array)?;
            Ok(lit(
                LiteralValue::Binary(a.value(row).to_vec()),
                DataType::Binary,
            ))
        }
        ArrowDT::LargeBinary => {
            let a = downcast::<LargeBinaryArray>(array)?;
            Ok(lit(
                LiteralValue::Binary(a.value(row).to_vec()),
                DataType::Binary,
            ))
        }
        ArrowDT::Date32 => {
            let a = downcast::<Date32Array>(array)?;
            Ok(lit(LiteralValue::Date(a.value(row)), DataType::Date))
        }
        ArrowDT::Timestamp(_, tz) => {
            let a = downcast::<TimestampMicrosecondArray>(array)?;
            let micros = a.value(row);
            let (value, data_type) = if tz.is_some() {
                (LiteralValue::Timestamp(micros), DataType::Timestamp)
            } else {
                (LiteralValue::TimestampNtz(micros), DataType::TimestampNtz)
            };
            Ok(lit(value, data_type))
        }
        ArrowDT::Decimal128(p, s) => {
            let a = downcast::<Decimal128Array>(array)?;
            let unscaled = a.value(row);
            let value = format_decimal128(unscaled, *s);
            let precision = *p;
            let scale = *s as u8;
            Ok(lit(
                LiteralValue::Decimal {
                    value,
                    precision,
                    scale,
                },
                DataType::Decimal { precision, scale },
            ))
        }
        ArrowDT::List(_) | ArrowDT::LargeList(_) => {
            let a = downcast::<ListArray>(array)?;
            let inner = a.value(row);
            let mut elements: Vec<Expression> = Vec::with_capacity(inner.len());
            for i in 0..inner.len() {
                elements.push(arrow_val_to_literal(inner.as_ref(), i)?);
            }
            let element_type = arrow_data_type_to_core(inner.data_type())?;
            Ok(Expression::ArrayLiteral(ArrayLiteralExpression {
                elements,
                element_type,
            }))
        }
        ArrowDT::Map(_, _) => {
            let a = downcast::<MapArray>(array)?;
            let entries = a.value(row);
            let sa = entries
                .as_any()
                .downcast_ref::<StructArray>()
                .require_proto(
                    "arrow_value::map_entries",
                    "Arrow Map entries must be StructArray",
                )?;
            let keys = sa.column(0);
            let vals = sa.column(1);
            let mut entries_out: Vec<(Expression, Expression)> = Vec::with_capacity(keys.len());
            for i in 0..keys.len() {
                entries_out.push((
                    arrow_val_to_literal(keys.as_ref(), i)?,
                    arrow_val_to_literal(vals.as_ref(), i)?,
                ));
            }
            let key_type = arrow_data_type_to_core(keys.data_type())?;
            let value_type = arrow_data_type_to_core(vals.data_type())?;
            Ok(Expression::MapLiteral(MapLiteralExpression {
                entries: entries_out,
                key_type,
                value_type,
            }))
        }
        ArrowDT::Struct(fields) => {
            let a = downcast::<StructArray>(array)?;
            let mut out: Vec<(String, Expression)> = Vec::with_capacity(a.num_columns());
            for (i, f) in fields.iter().enumerate() {
                let col = a.column(i);
                out.push((f.name().clone(), arrow_val_to_literal(col.as_ref(), row)?));
            }
            Ok(Expression::StructLiteral(StructLiteralExpression {
                fields: out,
            }))
        }
        // ── Interval Arrow values ────────────────────────────────────────
        // `createDataFrame(rows_with_intervals, schema)` from PySpark re-sends
        // interval cells as their Arrow wire form. τ has no `LiteralValue`
        // variant for intervals; wrap the DuckDB `INTERVAL` literal SQL as
        // `RawSql` with a data-type hint so the analyzer stamps the correct
        // shape. Emission of `RawSql` is a verbatim string passthrough
        // (`emission.rs::render_expr`), which suits DuckDB's native INTERVAL
        // syntax exactly. Corpus: intv-001 / intv-003 / intv-005.
        ArrowDT::Duration(TimeUnit::Microsecond) => {
            let a = downcast::<DurationMicrosecondArray>(array)?;
            let micros: i64 = a.value(row);
            // DuckDB has no `make_interval`; use `to_microseconds(BIGINT)` which
            // returns an INTERVAL representing the given microseconds.
            let sql = format!("to_microseconds(CAST({micros} AS BIGINT))");
            Ok(Expression::RawSql(RawSqlExpression {
                sql,
                data_type: Some(DataType::DayTimeInterval),
                nullable: Some(false),
            }))
        }
        ArrowDT::Interval(IntervalUnit::YearMonth) => {
            let a = downcast::<IntervalYearMonthArray>(array)?;
            let total_months: i32 = a.value(row);
            let sql = format!("to_months(CAST({total_months} AS INTEGER))");
            Ok(Expression::RawSql(RawSqlExpression {
                sql,
                data_type: Some(DataType::YearMonthInterval),
                nullable: Some(false),
            }))
        }
        ArrowDT::Interval(IntervalUnit::MonthDayNano) => {
            let a = downcast::<IntervalMonthDayNanoArray>(array)?;
            let v = a.value(row);
            let micros = v.nanoseconds / 1_000;
            // Compose the three components — DuckDB INTERVAL arithmetic
            // preserves the MonthDayNano semantics DuckDB uses internally.
            let sql = format!(
                "(to_months(CAST({m} AS INTEGER)) + to_days(CAST({d} AS INTEGER)) + to_microseconds(CAST({us} AS BIGINT)))",
                m = v.months,
                d = v.days,
                us = micros,
            );
            Ok(Expression::RawSql(RawSqlExpression {
                sql,
                data_type: Some(DataType::Interval),
                nullable: Some(false),
            }))
        }
        // §2.1 loud-fail: NO Ok(null_literal()) catch-all here. Every
        // unhandled Arrow type surfaces as `UnsupportedProtoShape`.
        other => bail_boundary_proto!(
            format!("arrow_value::{other:?}"),
            "V2RelationConverter Arrow value dispatch has no arm for this type",
        ),
    }
}

fn downcast<T: Array + 'static>(array: &dyn Array) -> Result<&T, EmissionError> {
    array.as_any().downcast_ref::<T>().require_proto(
        &format!("arrow_downcast::{:?}", array.data_type()),
        &format!("Arrow array downcast failed for {:?}", array.data_type()),
    )
}

/// Format an unscaled Arrow Decimal128 value at the given scale as a
/// canonical decimal string.
fn format_decimal128(unscaled: i128, scale: i8) -> String {
    if scale <= 0 {
        // For non-positive scale, the value is an integer; append zeros.
        let zeros = (-scale) as usize;
        return format!("{unscaled}{}", "0".repeat(zeros));
    }
    let scale_usize = scale as usize;
    let (sign, abs_str) = if unscaled < 0 {
        ("-", (-unscaled).to_string())
    } else {
        ("", unscaled.to_string())
    };
    if abs_str.len() > scale_usize {
        let split = abs_str.len() - scale_usize;
        format!("{sign}{}.{}", &abs_str[..split], &abs_str[split..])
    } else {
        let pad = scale_usize - abs_str.len();
        format!("{sign}0.{}{abs_str}", "0".repeat(pad))
    }
}

/// Walk a proto [`proto::Relation`] tree and collect every
/// [`proto::RelationCommon::plan_id`] value.
fn collect_relation_plan_ids(rel: &proto::Relation, ids: &mut HashSet<i64>) {
    if let Some(common) = &rel.common {
        if let Some(id) = common.plan_id {
            ids.insert(id);
        }
    }
    use proto::relation::RelType;
    // Deliberately partial traversal — only these operator shapes are walked.
    let children: [Option<&proto::Relation>; 2] = match &rel.rel_type {
        Some(RelType::Filter(f)) => [f.input.as_deref(), None],
        Some(RelType::Project(p)) => [p.input.as_deref(), None],
        Some(RelType::Aggregate(a)) => [a.input.as_deref(), None],
        Some(RelType::Sort(s)) => [s.input.as_deref(), None],
        Some(RelType::Limit(l)) => [l.input.as_deref(), None],
        Some(RelType::Offset(o)) => [o.input.as_deref(), None],
        Some(RelType::Join(j)) => [j.left.as_deref(), j.right.as_deref()],
        _ => [None, None],
    };
    for child in children.into_iter().flatten() {
        collect_relation_plan_ids(child, ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Builder helpers (readability) ──────────────────────────────────────

    fn rel(rt: proto::relation::RelType) -> proto::Relation {
        proto::Relation {
            common: None,
            rel_type: Some(rt),
        }
    }

    fn rel_with_plan_id(rt: proto::relation::RelType, plan_id: i64) -> proto::Relation {
        proto::Relation {
            common: Some(proto::RelationCommon {
                plan_id: Some(plan_id),
                ..Default::default()
            }),
            rel_type: Some(rt),
        }
    }

    fn table_scan_rel(name: &str) -> proto::Relation {
        rel(proto::relation::RelType::Read(proto::Read {
            is_streaming: false,
            read_type: Some(proto::read::ReadType::NamedTable(proto::read::NamedTable {
                unparsed_identifier: name.to_owned(),
                options: Default::default(),
            })),
        }))
    }

    fn table_scan_rel_with_plan_id(name: &str, plan_id: i64) -> proto::Relation {
        rel_with_plan_id(
            proto::relation::RelType::Read(proto::Read {
                is_streaming: false,
                read_type: Some(proto::read::ReadType::NamedTable(proto::read::NamedTable {
                    unparsed_identifier: name.to_owned(),
                    options: Default::default(),
                })),
            }),
            plan_id,
        )
    }

    fn int_literal(v: i32) -> proto::Expression {
        proto::Expression {
            common: None,
            expr_type: Some(proto::expression::ExprType::Literal(
                proto::expression::Literal {
                    data_type: None,
                    literal_type: Some(proto::expression::literal::LiteralType::Integer(v)),
                },
            )),
        }
    }

    fn unresolved_attr(name: &str) -> proto::Expression {
        proto::Expression {
            common: None,
            expr_type: Some(proto::expression::ExprType::UnresolvedAttribute(
                proto::expression::UnresolvedAttribute {
                    unparsed_identifier: name.to_owned(),
                    plan_id: None,
                    is_metadata_column: None,
                },
            )),
        }
    }

    fn unresolved_attr_with_plan_id(name: &str, plan_id: i64) -> proto::Expression {
        proto::Expression {
            common: None,
            expr_type: Some(proto::expression::ExprType::UnresolvedAttribute(
                proto::expression::UnresolvedAttribute {
                    unparsed_identifier: name.to_owned(),
                    plan_id: Some(plan_id),
                    is_metadata_column: None,
                },
            )),
        }
    }

    // ── Invoke helpers ─────────────────────────────────────────────────────

    /// Convert a relation whose conversion must succeed.
    fn convert_ok(relation: &proto::Relation) -> CommonAst {
        V2RelationConverter::new()
            .convert(relation)
            .expect("conversion must succeed")
    }

    /// Convert a relation whose conversion must fail with
    /// `EmissionError::Unsupported { kind: ProtoShape }`; asserts the error
    /// kind and returns the shape name for further assertions.
    fn convert_proto_shape_err(relation: &proto::Relation) -> String {
        match V2RelationConverter::new().convert(relation).unwrap_err() {
            EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                name,
                ..
            } => name,
            other => panic!("expected UnsupportedProtoShape, got {other:?}"),
        }
    }

    // ── Round-trip tests ───────────────────────────────────────────────────

    #[test]
    fn convert_project_round_trip() {
        let input = table_scan_rel("t");
        let proj = rel(proto::relation::RelType::Project(Box::new(
            proto::Project {
                input: Some(Box::new(input)),
                expressions: vec![int_literal(1)],
            },
        )));
        let out = convert_ok(&proj);
        match out.op {
            CommonOp::Project {
                input, projections, ..
            } => {
                assert_eq!(projections.len(), 1);
                assert!(matches!(input.op, CommonOp::TableScan { .. }));
            }
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn convert_filter_round_trip() {
        let input = table_scan_rel("t");
        let f = rel(proto::relation::RelType::Filter(Box::new(proto::Filter {
            input: Some(Box::new(input)),
            condition: Some(int_literal(1)),
        })));
        let out = convert_ok(&f);
        assert!(matches!(out.op, CommonOp::Filter { .. }));
    }

    #[test]
    fn convert_sort_round_trip() {
        let input = table_scan_rel("t");
        let s = rel(proto::relation::RelType::Sort(Box::new(proto::Sort {
            input: Some(Box::new(input)),
            order: vec![proto::expression::SortOrder {
                child: Some(Box::new(unresolved_attr("id"))),
                direction: proto::expression::sort_order::SortDirection::Ascending as i32,
                null_ordering: proto::expression::sort_order::NullOrdering::SortNullsFirst as i32,
            }],
            is_global: None,
        })));
        let out = convert_ok(&s);
        match out.op {
            CommonOp::Sort {
                order,
                limit,
                offset,
                ..
            } => {
                assert_eq!(order.len(), 1);
                assert!(limit.is_none());
                assert!(offset.is_none());
            }
            _ => panic!("expected Sort"),
        }
    }

    /// `convert_sort_order`'s `Unspecified`-field defaults must mirror Spark's
    /// `SparkConnectPlanner.scala::transformSortOrder` proto decoder, not
    /// `SortOrder.scala`'s direction-derived default (that rule is the SQL
    /// front-end's `lower_order_by_expr`, a separate code path). Real PySpark
    /// clients always stamp both fields explicitly, so this is unit-test-only
    /// (unreachable via the DataFrame corpus).
    #[test]
    fn convert_sort_order_unspecified_defaults_match_spark_connect_planner() {
        use proto::expression::sort_order::{NullOrdering as ProtoNO, SortDirection as ProtoSD};

        let cases = [
            (
                ProtoSD::Unspecified,
                ProtoNO::SortNullsUnspecified,
                false,
                false,
            ),
            (
                ProtoSD::Ascending,
                ProtoNO::SortNullsUnspecified,
                true,
                false,
            ),
            (ProtoSD::Unspecified, ProtoNO::SortNullsFirst, false, true),
            (ProtoSD::Descending, ProtoNO::SortNullsLast, false, false),
        ];
        for (direction, null_ordering, want_ascending, want_nulls_first) in cases {
            let so = proto::expression::SortOrder {
                child: Some(Box::new(unresolved_attr("id"))),
                direction: direction as i32,
                null_ordering: null_ordering as i32,
            };
            let mut converter = V2ExpressionConverter::new();
            let out = converter
                .convert_sort_order(&so)
                .expect("sort order conversion must succeed");
            assert_eq!(
                out.direction == SortDirection::Ascending,
                want_ascending,
                "direction mismatch for {direction:?}/{null_ordering:?}"
            );
            assert_eq!(
                out.null_ordering == NullOrdering::NullsFirst,
                want_nulls_first,
                "null_ordering mismatch for {direction:?}/{null_ordering:?}"
            );
        }
    }

    #[test]
    fn convert_limit_round_trip() {
        let input = table_scan_rel("t");
        let l = rel(proto::relation::RelType::Limit(Box::new(proto::Limit {
            input: Some(Box::new(input)),
            limit: 10,
        })));
        let out = convert_ok(&l);
        match out.op {
            CommonOp::Limit { limit, .. } => assert_eq!(limit, 10),
            _ => panic!("expected Limit"),
        }
    }

    #[test]
    fn convert_offset_round_trip() {
        let input = table_scan_rel("t");
        let o = rel(proto::relation::RelType::Offset(Box::new(proto::Offset {
            input: Some(Box::new(input)),
            offset: 5,
        })));
        let out = convert_ok(&o);
        match out.op {
            CommonOp::Sort {
                order,
                limit,
                offset,
                ..
            } => {
                assert!(order.is_empty());
                assert!(limit.is_none());
                assert_eq!(offset, Some(5));
            }
            _ => panic!("expected Sort (bare offset)"),
        }
    }

    #[test]
    fn convert_aggregate_primitive_round_trip() {
        let input = table_scan_rel("t");
        let a = rel(proto::relation::RelType::Aggregate(Box::new(
            proto::Aggregate {
                input: Some(Box::new(input)),
                group_type: proto::aggregate::GroupType::Groupby as i32,
                grouping_expressions: vec![unresolved_attr("dept")],
                aggregate_expressions: vec![],
                pivot: None,
                grouping_sets: vec![],
            },
        )));
        let out = convert_ok(&a);
        match out.op {
            CommonOp::Aggregate { grouping, .. } => assert_eq!(grouping.len(), 1),
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn convert_aggregate_rollup_produces_rollup_kind() {
        let input = table_scan_rel("t");
        let a = rel(proto::relation::RelType::Aggregate(Box::new(
            proto::Aggregate {
                input: Some(Box::new(input)),
                group_type: proto::aggregate::GroupType::Rollup as i32,
                grouping_expressions: vec![],
                aggregate_expressions: vec![],
                pivot: None,
                grouping_sets: vec![],
            },
        )));
        let out = convert_ok(&a);
        match out.op {
            CommonOp::Aggregate { grouping_kind, .. } => assert_eq!(
                grouping_kind,
                thunderduck_core::transpiler_v2::ast::GroupingKind::Rollup
            ),
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn convert_aggregate_cube_produces_cube_kind() {
        let input = table_scan_rel("t");
        let a = rel(proto::relation::RelType::Aggregate(Box::new(
            proto::Aggregate {
                input: Some(Box::new(input)),
                group_type: proto::aggregate::GroupType::Cube as i32,
                grouping_expressions: vec![],
                aggregate_expressions: vec![],
                pivot: None,
                grouping_sets: vec![],
            },
        )));
        let out = convert_ok(&a);
        match out.op {
            CommonOp::Aggregate { grouping_kind, .. } => assert_eq!(
                grouping_kind,
                thunderduck_core::transpiler_v2::ast::GroupingKind::Cube
            ),
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn convert_aggregate_grouping_sets_produces_grouping_sets_kind() {
        let input = table_scan_rel("t");
        let a = rel(proto::relation::RelType::Aggregate(Box::new(
            proto::Aggregate {
                input: Some(Box::new(input)),
                group_type: proto::aggregate::GroupType::GroupingSets as i32,
                grouping_expressions: vec![],
                aggregate_expressions: vec![],
                pivot: None,
                grouping_sets: vec![],
            },
        )));
        let out = convert_ok(&a);
        match out.op {
            CommonOp::Aggregate { grouping_kind, .. } => assert_eq!(
                grouping_kind,
                thunderduck_core::transpiler_v2::ast::GroupingKind::GroupingSets
            ),
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn convert_aggregate_pivot_without_pivot_sub_message_rejects_loudly() {
        // Pass 60: PIVOT is now first-class, but a proto whose group_type is
        // PIVOT yet carries no `pivot` sub-message is malformed — reject
        // with a specific UnsupportedProtoShape rather than silently defaulting.
        let input = table_scan_rel("t");
        let a = rel(proto::relation::RelType::Aggregate(Box::new(
            proto::Aggregate {
                input: Some(Box::new(input)),
                group_type: proto::aggregate::GroupType::Pivot as i32,
                grouping_expressions: vec![],
                aggregate_expressions: vec![],
                pivot: None,
                grouping_sets: vec![],
            },
        )));
        assert_eq!(
            convert_proto_shape_err(&a),
            "Aggregate::Pivot::missing_pivot"
        );
    }

    #[test]
    fn convert_aggregate_pivot_with_explicit_values_round_trips_to_common_op_pivot() {
        // Pass 60 anchor for grp-004: PIVOT with explicit value literals maps
        // 1:1 into CommonOp::Pivot — grouping / pivot_column / pivot_values /
        // aggregates all preserved.
        let input = table_scan_rel("emp");
        let true_lit = proto::expression::Literal {
            data_type: None,
            literal_type: Some(proto::expression::literal::LiteralType::Boolean(true)),
        };
        let false_lit = proto::expression::Literal {
            data_type: None,
            literal_type: Some(proto::expression::literal::LiteralType::Boolean(false)),
        };
        let pivot_sub = proto::aggregate::Pivot {
            col: Some(unresolved_attr("active")),
            values: vec![true_lit, false_lit],
        };
        let a = rel(proto::relation::RelType::Aggregate(Box::new(
            proto::Aggregate {
                input: Some(Box::new(input)),
                group_type: proto::aggregate::GroupType::Pivot as i32,
                grouping_expressions: vec![unresolved_attr("dept_id")],
                aggregate_expressions: vec![int_literal(1)],
                pivot: Some(pivot_sub),
                grouping_sets: vec![],
            },
        )));
        let out = convert_ok(&a);
        match out.op {
            CommonOp::Pivot {
                grouping,
                pivot_values,
                aggregates,
                ..
            } => {
                match grouping {
                    PivotGrouping::Explicit(g) => assert_eq!(g.len(), 1),
                    PivotGrouping::Implicit => panic!("expected explicit grouping"),
                }
                assert_eq!(pivot_values.len(), 2);
                assert_eq!(aggregates.len(), 1);
            }
            _ => panic!("expected CommonOp::Pivot"),
        }
    }

    #[test]
    fn convert_aggregate_pivot_without_values_produces_empty_pivot_values() {
        // Pass 60 anchor for grp-005: PIVOT with empty values → analyzer /
        // emission handle "eager discovery" downstream.
        let input = table_scan_rel("emp");
        let pivot_sub = proto::aggregate::Pivot {
            col: Some(unresolved_attr("dept_id")),
            values: vec![],
        };
        let a = rel(proto::relation::RelType::Aggregate(Box::new(
            proto::Aggregate {
                input: Some(Box::new(input)),
                group_type: proto::aggregate::GroupType::Pivot as i32,
                grouping_expressions: vec![unresolved_attr("active")],
                aggregate_expressions: vec![int_literal(1)],
                pivot: Some(pivot_sub),
                grouping_sets: vec![],
            },
        )));
        let out = convert_ok(&a);
        match out.op {
            CommonOp::Pivot { pivot_values, .. } => assert!(pivot_values.is_empty()),
            _ => panic!("expected CommonOp::Pivot"),
        }
    }

    #[test]
    fn convert_read_named_table_round_trip() {
        let out = convert_ok(&table_scan_rel("orders"));
        match out.op {
            CommonOp::TableScan { table, .. } => assert_eq!(table, "orders"),
            _ => panic!("expected TableScan"),
        }
    }

    #[test]
    fn convert_read_data_source_round_trip() {
        let read = rel(proto::relation::RelType::Read(proto::Read {
            is_streaming: false,
            read_type: Some(proto::read::ReadType::DataSource(proto::read::DataSource {
                format: Some("parquet".into()),
                schema: None,
                options: Default::default(),
                paths: vec!["/tmp/t.parquet".into()],
                predicates: vec![],
            })),
        }));
        let out = convert_ok(&read);
        match out.op {
            CommonOp::FileScan { format, paths, .. } => {
                assert_eq!(format, FileFormat::Parquet);
                assert_eq!(paths, vec!["/tmp/t.parquet"]);
            }
            _ => panic!("expected FileScan"),
        }
    }

    #[test]
    fn convert_local_relation_schema_only_round_trip() {
        let lr = rel(proto::relation::RelType::LocalRelation(
            proto::LocalRelation {
                data: None,
                schema: Some("STRUCT<id: BIGINT>".into()),
            },
        ));
        let out = convert_ok(&lr);
        match out.op {
            CommonOp::LocalRelation { rows, .. } => assert!(rows.is_empty()),
            _ => panic!("expected LocalRelation"),
        }
    }

    // ── parse_type_str_to_struct — DDL fallback (pass-2 fix) ────────────────
    // The legacy fallback routed through `parse_type_str`, which had no
    // struct arm — DDL-string schemas silently became `StructType::empty()`.

    #[test]
    fn parse_type_str_to_struct_parses_struct_wrapper_ddl() {
        let st = super::parse_type_str_to_struct("struct<id:bigint,name:string>");
        assert_eq!(st.fields.len(), 2);
        assert_eq!(st.fields[0].name, "id");
        assert_eq!(st.fields[0].data_type, DataType::Long);
        assert_eq!(st.fields[1].name, "name");
        assert_eq!(st.fields[1].data_type, DataType::String);
    }

    #[test]
    fn parse_type_str_to_struct_parses_bare_field_list_ddl() {
        // The shape `DataFrameReader.schema("id INT, name STRING")` sends for
        // a Read::DataSource schema string.
        let st = super::parse_type_str_to_struct("id bigint, name string");
        assert_eq!(st.fields.len(), 2);
        assert_eq!(st.fields[0].name, "id");
        assert_eq!(st.fields[0].data_type, DataType::Long);
        assert_eq!(st.fields[1].name, "name");
        assert_eq!(st.fields[1].data_type, DataType::String);
    }

    #[test]
    fn parse_type_str_to_struct_untranslatable_input_degrades_to_empty() {
        // Legacy shape preserved: scalar type strings and garbage are not
        // schemas — they yield the empty struct, not a panic/error.
        assert_eq!(super::parse_type_str_to_struct("int"), StructType::empty());
        assert_eq!(
            super::parse_type_str_to_struct("not a schema at all!"),
            StructType::empty()
        );
    }

    #[test]
    fn convert_join_round_trip() {
        let left = table_scan_rel("a");
        let right = table_scan_rel("b");
        let j = rel(proto::relation::RelType::Join(Box::new(proto::Join {
            left: Some(Box::new(left)),
            right: Some(Box::new(right)),
            join_condition: None,
            join_type: proto::join::JoinType::Inner as i32,
            using_columns: vec![],
            join_data_type: None,
        })));
        let out = convert_ok(&j);
        assert!(matches!(out.op, CommonOp::Join { .. }));
    }

    #[test]
    fn convert_join_populates_left_plan_ids_and_right_plan_ids() {
        // §2.3 anchor.
        let left = table_scan_rel_with_plan_id("a", 100);
        let right = table_scan_rel_with_plan_id("b", 200);
        let j = rel(proto::relation::RelType::Join(Box::new(proto::Join {
            left: Some(Box::new(left)),
            right: Some(Box::new(right)),
            join_condition: None,
            join_type: proto::join::JoinType::Inner as i32,
            using_columns: vec![],
            join_data_type: None,
        })));
        let out = convert_ok(&j);
        match out.op {
            CommonOp::Join {
                left_plan_ids,
                right_plan_ids,
                ..
            } => {
                assert_eq!(left_plan_ids, vec![100i64]);
                assert_eq!(right_plan_ids, vec![200i64]);
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn convert_join_condition_unresolved_column_plan_id_first_class() {
        let left = table_scan_rel_with_plan_id("a", 100);
        let right = table_scan_rel_with_plan_id("b", 200);
        // Condition references a column with plan_id → should NOT string-encode.
        let cond = unresolved_attr_with_plan_id("id", 100);
        let j = rel(proto::relation::RelType::Join(Box::new(proto::Join {
            left: Some(Box::new(left)),
            right: Some(Box::new(right)),
            join_condition: Some(cond),
            join_type: proto::join::JoinType::Inner as i32,
            using_columns: vec![],
            join_data_type: None,
        })));
        let out = convert_ok(&j);
        let CommonOp::Join { condition, .. } = out.op else {
            panic!("expected Join");
        };
        let cond = condition.expect("join condition present");
        match cond {
            Expression::UnresolvedColumn(u) => {
                assert_eq!(u.plan_id, Some(100));
                assert!(
                    u.qualifier.is_none(),
                    "no string qualifier encoding — got {:?}",
                    u.qualifier
                );
            }
            other => panic!("expected UnresolvedColumn, got {other:?}"),
        }
    }

    #[test]
    fn convert_sql_relation_returns_unsupported_proto_shape() {
        // §2.2 anchor.
        let s = rel(proto::relation::RelType::Sql(proto::Sql {
            query: "SELECT 1".to_owned(),
            ..Default::default()
        }));
        assert_eq!(convert_proto_shape_err(&s), "RelType::Sql");
    }

    #[test]
    fn convert_catalog_returns_unsupported_proto_shape() {
        let c_rel = rel(proto::relation::RelType::Catalog(proto::Catalog {
            cat_type: None,
        }));
        convert_proto_shape_err(&c_rel);
    }

    // ── Arrow value dispatch tests ─────────────────────────────────────────

    #[test]
    fn arrow_val_to_literal_decimal128_produces_literal_decimal_not_null() {
        // §2.1 anchor: unscaled 10025 at scale=2 → 100.25, never NULL.
        use arrow::array::Decimal128Array;
        let arr = Decimal128Array::from(vec![Some(10025i128)])
            .with_precision_and_scale(5, 2)
            .expect("decimal");
        let expr = arrow_val_to_literal(&arr, 0).expect("decimal literal");
        match expr {
            Expression::Literal(Literal {
                value:
                    LiteralValue::Decimal {
                        value,
                        precision,
                        scale,
                    },
                data_type: DataType::Decimal { .. },
            }) => {
                assert_eq!(value, "100.25");
                assert_eq!(precision, 5);
                assert_eq!(scale, 2);
            }
            other => panic!("expected Decimal literal, got {other:?}"),
        }
    }

    #[test]
    fn arrow_val_to_literal_primitives_sweep() {
        use arrow::array::{BooleanArray, Int32Array, Int64Array, StringArray};
        let a = BooleanArray::from(vec![Some(true)]);
        assert!(matches!(
            arrow_val_to_literal(&a, 0).unwrap(),
            Expression::Literal(Literal {
                value: LiteralValue::Boolean(true),
                ..
            })
        ));
        let a = Int32Array::from(vec![Some(42)]);
        assert!(matches!(
            arrow_val_to_literal(&a, 0).unwrap(),
            Expression::Literal(Literal {
                value: LiteralValue::Int(42),
                ..
            })
        ));
        let a = Int64Array::from(vec![Some(-7i64)]);
        assert!(matches!(
            arrow_val_to_literal(&a, 0).unwrap(),
            Expression::Literal(Literal {
                value: LiteralValue::Long(-7),
                ..
            })
        ));
        let a = StringArray::from(vec![Some("hi")]);
        match arrow_val_to_literal(&a, 0).unwrap() {
            Expression::Literal(Literal {
                value: LiteralValue::String(s),
                ..
            }) => assert_eq!(s, "hi"),
            _ => panic!("expected string literal"),
        }
    }

    #[test]
    fn arrow_val_null_row_produces_literal_null() {
        use arrow::array::Int32Array;
        let a = Int32Array::from(vec![None::<i32>]);
        assert!(matches!(
            arrow_val_to_literal(&a, 0).unwrap(),
            Expression::Literal(Literal {
                value: LiteralValue::Null,
                ..
            })
        ));
    }

    #[test]
    fn arrow_val_to_literal_list_recursion() {
        use arrow::array::{Int32Array, ListArray};
        let values = Int32Array::from(vec![Some(1), Some(2), Some(3)]);
        let offsets =
            arrow::buffer::OffsetBuffer::new(arrow::buffer::ScalarBuffer::from(vec![0i32, 3i32]));
        let field = std::sync::Arc::new(arrow::datatypes::Field::new(
            "item",
            arrow::datatypes::DataType::Int32,
            true,
        ));
        let arr = ListArray::new(field, offsets, std::sync::Arc::new(values), None);
        let expr = arrow_val_to_literal(&arr, 0).expect("list literal");
        match expr {
            Expression::ArrayLiteral(a) => {
                assert_eq!(a.elements.len(), 3);
            }
            other => panic!("expected ArrayLiteral, got {other:?}"),
        }
    }

    #[test]
    fn arrow_val_decimal256_returns_unsupported_proto_shape() {
        // Decimal256 is intentionally missing from the dispatch table — loud fail.
        use arrow::array::Decimal256Array;
        use arrow::datatypes::i256;
        let arr = Decimal256Array::from(vec![Some(i256::from_i128(1_000_000i128))])
            .with_precision_and_scale(76, 2)
            .expect("decimal256");
        let err = arrow_val_to_literal(&arr, 0).unwrap_err();
        assert!(matches!(
            err,
            EmissionError::Unsupported {
                kind: UnsupportedKind::ProtoShape,
                ..
            }
        ));
    }

    /// §2.1 source-file grep: the converter MUST NOT carry an `Ok("NULL")`
    /// (or equivalent catch-all) line. Silent NULL substitution is the exact
    /// bug pattern this test blocks.
    #[test]
    fn arrow_val_no_catch_all_ok_null_source_grep() {
        // Resolve the source path via CARGO_MANIFEST_DIR since tests may run
        // from either the workspace root or the crate root depending on
        // invocation.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set under cargo test");
        let path =
            std::path::Path::new(&manifest_dir).join("src/converter/v2_relation_converter.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read own source file {}: {e}", path.display()));
        // Build the forbidden needle at runtime so this test's own source
        // (`r#"Ok(\"NULL\")"#`) does not match itself when the grep runs.
        // Concatenating four fragments keeps every fragment shorter than
        // the full needle.
        let needle: String = ["Ok(", "\"", "NULL", "\")"].concat();
        for (n, line) in src.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            // Skip this test's own body (contains the needle-construction
            // above and the assertion below — both would trip the grep).
            if trimmed.contains("let needle:") || trimmed.contains("!trimmed.contains(&needle)") {
                continue;
            }
            assert!(
                !trimmed.contains(&needle),
                "line {}: {trimmed}\nτ's V2 converter must never emit the forbidden Ok(NULL) \
                 catch-all — use EmissionError::Unsupported (kind: ProtoShape) instead.",
                n + 1
            );
        }
    }

    // ── Unpivot conversion (piv-004 / piv-005) ─────────────────────────────

    #[test]
    fn convert_unpivot_round_trip_maps_proto_fields_to_common_op() {
        // Anchor: piv-004 shape — ids=[id], values=[age, salary], names
        // "metric"/"value". The proto → AST mapping must preserve column
        // names exactly (`extract_column_name` accepts bare
        // UnresolvedAttribute only).
        let input = table_scan_rel("emp");
        let unpivot = rel(proto::relation::RelType::Unpivot(Box::new(
            proto::Unpivot {
                input: Some(Box::new(input)),
                ids: vec![unresolved_attr("id")],
                values: Some(proto::unpivot::Values {
                    values: vec![unresolved_attr("age"), unresolved_attr("salary")],
                }),
                variable_column_name: "metric".to_owned(),
                value_column_name: "value".to_owned(),
            },
        )));
        let out = convert_ok(&unpivot);
        match out.op {
            CommonOp::Unpivot {
                input,
                ids,
                values,
                variable_column_name,
                value_column_name,
            } => {
                assert!(matches!(input.op, CommonOp::TableScan { .. }));
                assert_eq!(ids, UnpivotIds::Explicit(vec!["id".to_owned()]));
                assert_eq!(values, vec!["age".to_owned(), "salary".to_owned()]);
                assert_eq!(variable_column_name, "metric");
                assert_eq!(value_column_name, "value");
            }
            _ => panic!("expected Unpivot"),
        }
    }

    #[test]
    fn convert_unpivot_absent_values_carries_empty_list_for_analyzer_expansion() {
        // When the proto omits the `values` field, Spark's default is "all
        // non-id columns". τ leaves `values` empty in the AST and expects
        // the analyzer to materialise the expansion.
        let input = table_scan_rel("emp");
        let unpivot = rel(proto::relation::RelType::Unpivot(Box::new(
            proto::Unpivot {
                input: Some(Box::new(input)),
                ids: vec![unresolved_attr("id")],
                values: None,
                variable_column_name: "metric".to_owned(),
                value_column_name: "value".to_owned(),
            },
        )));
        let out = convert_ok(&unpivot);
        match out.op {
            CommonOp::Unpivot { values, .. } => assert!(values.is_empty()),
            _ => panic!("expected Unpivot"),
        }
    }

    // ── Describe / Summary conversion (Pass 80) ───────────────────────────

    #[test]
    fn convert_describe_preserves_input_and_cols() {
        let input = table_scan_rel("emp");
        let describe = rel(proto::relation::RelType::Describe(Box::new(
            proto::StatDescribe {
                input: Some(Box::new(input)),
                cols: vec!["age".to_owned(), "salary".to_owned()],
            },
        )));
        let out = convert_ok(&describe);
        match out.op {
            CommonOp::Describe { input, cols } => {
                assert!(matches!(input.op, CommonOp::TableScan { .. }));
                assert_eq!(cols, vec!["age".to_owned(), "salary".to_owned()]);
            }
            _ => panic!("expected CommonOp::Describe"),
        }
    }

    #[test]
    fn convert_describe_missing_input_surfaces_unsupported_proto_shape() {
        let describe = rel(proto::relation::RelType::Describe(Box::new(
            proto::StatDescribe {
                input: None,
                cols: vec![],
            },
        )));
        convert_proto_shape_err(&describe);
    }

    #[test]
    fn convert_summary_preserves_input_and_statistics() {
        let input = table_scan_rel("emp");
        let summary = rel(proto::relation::RelType::Summary(Box::new(
            proto::StatSummary {
                input: Some(Box::new(input)),
                statistics: vec![
                    "count".to_owned(),
                    "min".to_owned(),
                    "25%".to_owned(),
                    "75%".to_owned(),
                    "max".to_owned(),
                ],
            },
        )));
        let out = convert_ok(&summary);
        match out.op {
            CommonOp::Summary { input, statistics } => {
                assert!(matches!(input.op, CommonOp::TableScan { .. }));
                assert_eq!(statistics.len(), 5);
                assert_eq!(statistics[2], "25%");
            }
            _ => panic!("expected CommonOp::Summary"),
        }
    }

    #[test]
    fn convert_summary_missing_input_surfaces_unsupported_proto_shape() {
        let summary = rel(proto::relation::RelType::Summary(Box::new(
            proto::StatSummary {
                input: None,
                statistics: vec![],
            },
        )));
        convert_proto_shape_err(&summary);
    }

    // ── FreqItems / Crosstab (Pass 82) ────────────────────────────────────

    #[test]
    fn convert_freq_items_defaults_support_to_pyspark_0_01_when_proto_none() {
        let input = table_scan_rel("emp");
        let fi = rel(proto::relation::RelType::FreqItems(Box::new(
            proto::StatFreqItems {
                input: Some(Box::new(input)),
                cols: vec!["dept_id".to_owned()],
                support: None,
            },
        )));
        let out = convert_ok(&fi);
        match out.op {
            CommonOp::FreqItems {
                input: _,
                cols,
                support,
            } => {
                assert_eq!(cols, vec!["dept_id".to_owned()]);
                assert!((support - 0.01).abs() < f64::EPSILON);
            }
            _ => panic!("expected CommonOp::FreqItems"),
        }
    }

    #[test]
    fn convert_freq_items_explicit_support_passes_through() {
        let input = table_scan_rel("emp");
        let fi = rel(proto::relation::RelType::FreqItems(Box::new(
            proto::StatFreqItems {
                input: Some(Box::new(input)),
                cols: vec!["a".to_owned(), "b".to_owned()],
                support: Some(0.42),
            },
        )));
        let out = convert_ok(&fi);
        match out.op {
            CommonOp::FreqItems { cols, support, .. } => {
                assert_eq!(cols, vec!["a".to_owned(), "b".to_owned()]);
                assert!((support - 0.42).abs() < f64::EPSILON);
            }
            _ => panic!("expected CommonOp::FreqItems"),
        }
    }

    #[test]
    fn convert_crosstab_carries_input_and_two_col_names() {
        let input = table_scan_rel("emp");
        let ct = rel(proto::relation::RelType::Crosstab(Box::new(
            proto::StatCrosstab {
                input: Some(Box::new(input)),
                col1: "dept_id".to_owned(),
                col2: "active".to_owned(),
            },
        )));
        let out = convert_ok(&ct);
        match out.op {
            CommonOp::Crosstab {
                input: _,
                col1,
                col2,
            } => {
                assert_eq!(col1, "dept_id");
                assert_eq!(col2, "active");
            }
            _ => panic!("expected CommonOp::Crosstab"),
        }
    }

    // ── StatCov / StatCorr / ApproxQuantile converters ─────────────────────

    #[test]
    fn convert_cov_desugars_to_global_aggregate_with_covar_samp() {
        let input = table_scan_rel("emp");
        let cov = rel(proto::relation::RelType::Cov(Box::new(proto::StatCov {
            input: Some(Box::new(input)),
            col1: "val1".to_owned(),
            col2: "val2".to_owned(),
        })));
        let out = convert_ok(&cov);
        match &out.op {
            CommonOp::Aggregate {
                grouping,
                aggregates,
                ..
            } => {
                assert!(grouping.is_empty(), "cov is a global aggregate");
                assert_eq!(aggregates.len(), 1);
                match &aggregates[0] {
                    Expression::FunctionCall(f) => {
                        assert_eq!(f.name, "covar_samp");
                        assert_eq!(f.args.len(), 2);
                        // Each arg should be coalesce(col, 0) per Spark's na.fill(0L).
                        for arg in &f.args {
                            match arg {
                                Expression::FunctionCall(inner) => {
                                    assert_eq!(inner.name, "coalesce");
                                }
                                other => panic!("expected coalesce FunctionCall, got {other:?}"),
                            }
                        }
                    }
                    other => panic!("expected FunctionCall, got {other:?}"),
                }
            }
            other => panic!("expected CommonOp::Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn convert_cov_missing_input_surfaces_unsupported_proto_shape() {
        let cov = rel(proto::relation::RelType::Cov(Box::new(proto::StatCov {
            input: None,
            col1: "a".to_owned(),
            col2: "b".to_owned(),
        })));
        convert_proto_shape_err(&cov);
    }

    #[test]
    fn convert_corr_desugars_to_global_aggregate_with_corr() {
        let input = table_scan_rel("emp");
        let corr = rel(proto::relation::RelType::Corr(Box::new(proto::StatCorr {
            input: Some(Box::new(input)),
            col1: "val1".to_owned(),
            col2: "val2".to_owned(),
            method: Some("pearson".to_owned()),
        })));
        let out = convert_ok(&corr);
        match &out.op {
            CommonOp::Aggregate {
                grouping,
                aggregates,
                ..
            } => {
                assert!(grouping.is_empty(), "corr is a global aggregate");
                assert_eq!(aggregates.len(), 1);
                match &aggregates[0] {
                    Expression::FunctionCall(f) => {
                        assert_eq!(f.name, "corr");
                        assert_eq!(f.args.len(), 2);
                    }
                    other => panic!("expected FunctionCall, got {other:?}"),
                }
            }
            other => panic!("expected CommonOp::Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn convert_corr_rejects_non_pearson_method() {
        let input = table_scan_rel("emp");
        let corr = rel(proto::relation::RelType::Corr(Box::new(proto::StatCorr {
            input: Some(Box::new(input)),
            col1: "a".to_owned(),
            col2: "b".to_owned(),
            method: Some("spearman".to_owned()),
        })));
        let err = V2RelationConverter::new().convert(&corr).unwrap_err();
        assert!(
            err.to_string().contains("Pearson"),
            "should mention Pearson: {err}"
        );
    }

    #[test]
    fn convert_corr_accepts_no_method_as_pearson() {
        let input = table_scan_rel("emp");
        let corr = rel(proto::relation::RelType::Corr(Box::new(proto::StatCorr {
            input: Some(Box::new(input)),
            col1: "a".to_owned(),
            col2: "b".to_owned(),
            method: None,
        })));
        let out = convert_ok(&corr);
        assert!(matches!(out.op, CommonOp::Aggregate { .. }));
    }

    #[test]
    fn convert_approx_quantile_single_col_multiple_probs() {
        let input = table_scan_rel("emp");
        let aq = rel(proto::relation::RelType::ApproxQuantile(Box::new(
            proto::StatApproxQuantile {
                input: Some(Box::new(input)),
                cols: vec!["val1".to_owned()],
                probabilities: vec![0.0, 0.5, 1.0],
                relative_error: 0.0,
            },
        )));
        let out = convert_ok(&aq);
        match &out.op {
            CommonOp::Aggregate {
                grouping,
                aggregates,
                ..
            } => {
                assert!(grouping.is_empty(), "approxQuantile is a global aggregate");
                assert_eq!(aggregates.len(), 1);
                // Outer array wraps one inner array (single column).
                match &aggregates[0] {
                    Expression::ArrayLiteral(outer) => {
                        assert_eq!(outer.elements.len(), 1, "one col → one inner array");
                        match &outer.elements[0] {
                            Expression::ArrayLiteral(inner) => {
                                assert_eq!(inner.elements.len(), 3, "3 probabilities");
                                for elem in &inner.elements {
                                    match elem {
                                        Expression::FunctionCall(f) => {
                                            assert_eq!(f.name, "percentile_approx");
                                        }
                                        other => panic!("expected FunctionCall, got {other:?}"),
                                    }
                                }
                            }
                            other => panic!("expected inner ArrayLiteral, got {other:?}"),
                        }
                    }
                    other => panic!("expected outer ArrayLiteral, got {other:?}"),
                }
            }
            other => panic!("expected CommonOp::Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn convert_approx_quantile_missing_input_surfaces_unsupported_proto_shape() {
        let aq = rel(proto::relation::RelType::ApproxQuantile(Box::new(
            proto::StatApproxQuantile {
                input: None,
                cols: vec!["a".to_owned()],
                probabilities: vec![0.5],
                relative_error: 0.0,
            },
        )));
        convert_proto_shape_err(&aq);
    }

    // ── Sample / SampleBy converters (Pass 83) ────────────────────────────

    #[test]
    fn convert_sample_maps_bounds_and_seed() {
        // samp-001 anchor — PySpark `df.sample(0.5, seed=11)` ships as
        // `Sample { lower_bound: 0.0, upper_bound: 0.5, with_replacement:
        // Some(false), seed: Some(11) }`.
        let input = table_scan_rel("emp");
        let s = rel(proto::relation::RelType::Sample(Box::new(proto::Sample {
            input: Some(Box::new(input)),
            lower_bound: 0.0,
            upper_bound: 0.5,
            with_replacement: Some(false),
            seed: Some(11),
            deterministic_order: false,
        })));
        let out = convert_ok(&s);
        match out.op {
            CommonOp::Sample {
                input,
                lower_bound,
                upper_bound,
                with_replacement,
                seed,
            } => {
                assert!(matches!(input.op, CommonOp::TableScan { .. }));
                assert!((lower_bound - 0.0).abs() < f64::EPSILON);
                assert!((upper_bound - 0.5).abs() < f64::EPSILON);
                assert!(!with_replacement);
                assert_eq!(seed, Some(11));
            }
            _ => panic!("expected CommonOp::Sample"),
        }
    }

    #[test]
    fn convert_sample_by_extracts_literal_strata() {
        // samp-002 anchor — three literal strata + fractions, seed=11.
        fn int_lit(v: i32) -> proto::expression::Literal {
            proto::expression::Literal {
                data_type: None,
                literal_type: Some(proto::expression::literal::LiteralType::Integer(v)),
            }
        }
        fn col(name: &str) -> proto::Expression {
            proto::Expression {
                common: None,
                expr_type: Some(proto::expression::ExprType::UnresolvedAttribute(
                    proto::expression::UnresolvedAttribute {
                        unparsed_identifier: name.to_owned(),
                        plan_id: None,
                        is_metadata_column: None,
                    },
                )),
            }
        }
        let input = table_scan_rel("emp");
        let s = rel(proto::relation::RelType::SampleBy(Box::new(
            proto::StatSampleBy {
                input: Some(Box::new(input)),
                col: Some(col("dept_id")),
                fractions: vec![
                    proto::stat_sample_by::Fraction {
                        stratum: Some(int_lit(10)),
                        fraction: 0.5,
                    },
                    proto::stat_sample_by::Fraction {
                        stratum: Some(int_lit(20)),
                        fraction: 0.5,
                    },
                    proto::stat_sample_by::Fraction {
                        stratum: Some(int_lit(30)),
                        fraction: 1.0,
                    },
                ],
                seed: Some(11),
            },
        )));
        let out = convert_ok(&s);
        match out.op {
            CommonOp::SampleBy {
                input,
                col,
                fractions,
                seed,
            } => {
                assert!(matches!(input.op, CommonOp::TableScan { .. }));
                assert!(matches!(col, Expression::UnresolvedColumn(_)));
                assert_eq!(fractions.len(), 3);
                assert!((fractions[2].1 - 1.0).abs() < f64::EPSILON);
                assert_eq!(seed, Some(11));
            }
            _ => panic!("expected CommonOp::SampleBy"),
        }
    }

    #[test]
    fn convert_summary_empty_statistics_preserved_for_analyzer_defaulting() {
        let input = table_scan_rel("emp");
        let summary = rel(proto::relation::RelType::Summary(Box::new(
            proto::StatSummary {
                input: Some(Box::new(input)),
                statistics: vec![],
            },
        )));
        let out = convert_ok(&summary);
        match out.op {
            CommonOp::Summary { statistics, .. } => {
                assert!(statistics.is_empty());
            }
            _ => panic!("expected CommonOp::Summary"),
        }
    }

    // ── normalize_decimal_literal (Spark parity, LiteralValueProtoConverter) ─

    /// cond-004 anchor: PySpark sends `(value="0.00", precision=10, scale=0)`.
    /// Value-derived is `(2, 2)`; max-of-value-and-wire yields `(10, 2)`,
    /// matching what Spark's `LiteralValueProtoConverter.decodeDecimal`
    /// computes.
    #[test]
    fn normalize_decimal_literal_cond004_anchor_pyspark_zero_dot_zero_zero() {
        assert_eq!(
            normalize_decimal_literal("0.00", Some(10), Some(0)),
            (10, 2)
        );
    }

    /// Wire precision/scale smaller than the value-derived shape: `max`
    /// selects the value-derived side (`(5, 2)` vs `(3, 1)` → `(5, 2)`).
    #[test]
    fn normalize_decimal_literal_wire_smaller_than_value_takes_value_side() {
        assert_eq!(
            normalize_decimal_literal("123.45", Some(3), Some(1)),
            (5, 2)
        );
    }

    /// Wire absent on both fields: falls back to the value-derived shape
    /// (`(5, 2)` for `"123.45"`).
    #[test]
    fn normalize_decimal_literal_wire_absent_uses_value_derived() {
        assert_eq!(normalize_decimal_literal("123.45", None, None), (5, 2));
    }

    /// Zero-value edge: `"0"` has no fractional digits and no non-zero
    /// integer digits; the `.max(1)` guard on `vp` yields `(1, 0)`.
    #[test]
    fn normalize_decimal_literal_zero_value_no_wire_yields_one_zero() {
        assert_eq!(normalize_decimal_literal("0", None, None), (1, 0));
    }

    /// Invariant safety: malformed wire (`p_wire < s_wire`) is clamped so
    /// `precision >= scale` still holds post-normalization.
    #[test]
    fn normalize_decimal_literal_malformed_wire_clamps_to_preserve_invariant() {
        assert_eq!(normalize_decimal_literal("0", Some(3), Some(5)), (5, 5));
    }

    // ── Pass 85 — UnresolvedRegex conversion ────────────────────────────────

    #[test]
    fn convert_unresolved_regex_strips_matching_backticks() {
        let ur = proto::expression::UnresolvedRegex {
            col_name: "`.*_id`".to_owned(),
            plan_id: None,
        };
        let out = super::convert_unresolved_regex(&ur);
        match out {
            Expression::UnresolvedRegex(r) => {
                assert_eq!(r.pattern, ".*_id");
                assert!(r.plan_id.is_none());
            }
            other => panic!("expected UnresolvedRegex, got {other:?}"),
        }
    }

    #[test]
    fn convert_unresolved_regex_preserves_plan_id() {
        let ur = proto::expression::UnresolvedRegex {
            col_name: "col_.*".to_owned(),
            plan_id: Some(1234),
        };
        let out = super::convert_unresolved_regex(&ur);
        match out {
            Expression::UnresolvedRegex(r) => {
                // Bare (no wrapping backticks) — passes through as-is.
                assert_eq!(r.pattern, "col_.*");
                assert_eq!(r.plan_id, Some(1234));
            }
            other => panic!("expected UnresolvedRegex, got {other:?}"),
        }
    }

    // ── Range ─────────────────────────────────────────────────────────────

    /// Build a `proto::Range` with the given fields.
    fn range_proto(
        start: Option<i64>,
        end: i64,
        step: i64,
        num_partitions: Option<i32>,
    ) -> proto::Relation {
        rel(proto::relation::RelType::Range(proto::Range {
            start,
            end,
            step,
            num_partitions,
        }))
    }

    /// Assert that a `CommonOp::TableFunction` carries `name = "range"` and
    /// return the args for further inspection.
    fn assert_range_table_fn(ast: &CommonAst) -> &[Expression] {
        match &ast.op {
            CommonOp::TableFunction {
                name,
                args,
                with_ordinality,
            } => {
                assert_eq!(name, "range");
                assert!(!with_ordinality);
                args
            }
            other => panic!("expected TableFunction(range), got {other:?}"),
        }
    }

    /// Extract the i64 value from an `Expression::Literal(Long(v))`.
    fn long_val(expr: &Expression) -> i64 {
        match expr {
            Expression::Literal(Literal {
                value: LiteralValue::Long(v),
                data_type: DataType::Long,
            }) => *v,
            other => panic!("expected Long literal, got {other:?}"),
        }
    }

    #[test]
    fn convert_range_default_start() {
        // spark.range(5) — start absent → default 0, step = 1
        let r = range_proto(None, 5, 1, None);
        let ast = convert_ok(&r);
        let args = assert_range_table_fn(&ast);
        assert_eq!(args.len(), 3);
        assert_eq!(long_val(&args[0]), 0); // start
        assert_eq!(long_val(&args[1]), 5); // end
        assert_eq!(long_val(&args[2]), 1); // step
    }

    #[test]
    fn convert_range_explicit_start_end_step() {
        // spark.range(2, 10, 3)
        let r = range_proto(Some(2), 10, 3, None);
        let ast = convert_ok(&r);
        let args = assert_range_table_fn(&ast);
        assert_eq!(args.len(), 3);
        assert_eq!(long_val(&args[0]), 2);
        assert_eq!(long_val(&args[1]), 10);
        assert_eq!(long_val(&args[2]), 3);
    }

    #[test]
    fn convert_range_num_partitions_ignored() {
        // spark.range(0, 100, 1, numPartitions=8) — partitions silently dropped
        let r = range_proto(Some(0), 100, 1, Some(8));
        let ast = convert_ok(&r);
        let args = assert_range_table_fn(&ast);
        assert_eq!(args.len(), 3);
        assert_eq!(long_val(&args[0]), 0);
        assert_eq!(long_val(&args[1]), 100);
        assert_eq!(long_val(&args[2]), 1);
    }

    // ── ToSchema ─────────────────────────────────────────────────────────────

    /// Build a proto `DataType` wrapping a `Struct` with the given fields.
    fn struct_data_type_proto(fields: Vec<(&str, proto::data_type::Kind)>) -> proto::DataType {
        let struct_fields = fields
            .into_iter()
            .map(|(name, kind)| proto::data_type::StructField {
                name: name.to_owned(),
                data_type: Some(proto::DataType { kind: Some(kind) }),
                nullable: true,
                metadata: None,
            })
            .collect();
        proto::DataType {
            kind: Some(proto::data_type::Kind::Struct(proto::data_type::Struct {
                fields: struct_fields,
                type_variation_reference: 0,
            })),
        }
    }

    #[test]
    fn convert_to_schema_reordered_struct_produces_project_with_aliased_casts() {
        // Target schema: (b: String, a: Integer) — reversed vs hypothetical
        // source (a, b). The projections must follow TARGET order.
        let schema = struct_data_type_proto(vec![
            ("b", proto::data_type::Kind::String(Default::default())),
            ("a", proto::data_type::Kind::Integer(Default::default())),
        ]);
        let r = rel(proto::relation::RelType::ToSchema(Box::new(
            proto::ToSchema {
                input: Some(Box::new(table_scan_rel("t"))),
                schema: Some(schema),
            },
        )));
        let ast = convert_ok(&r);
        let CommonOp::Project { projections, .. } = &ast.op else {
            panic!("expected Project, got {:?}", ast.op);
        };
        assert_eq!(projections.len(), 2);

        // First projection: CAST(b AS STRING) AS b
        let Expression::Alias(a0) = &projections[0] else {
            panic!("expected Alias, got {:?}", projections[0]);
        };
        assert_eq!(a0.alias, "b");
        let Expression::Cast(c0) = a0.expr.as_ref() else {
            panic!("expected Cast, got {:?}", a0.expr);
        };
        assert_eq!(c0.to_type, thunderduck_core::types::DataType::String);
        assert!(!c0.try_cast);
        let Expression::UnresolvedColumn(u0) = c0.expr.as_ref() else {
            panic!("expected UnresolvedColumn, got {:?}", c0.expr);
        };
        assert_eq!(u0.name, "b");

        // Second projection: CAST(a AS INTEGER) AS a
        let Expression::Alias(a1) = &projections[1] else {
            panic!("expected Alias, got {:?}", projections[1]);
        };
        assert_eq!(a1.alias, "a");
        let Expression::Cast(c1) = a1.expr.as_ref() else {
            panic!("expected Cast, got {:?}", a1.expr);
        };
        assert_eq!(c1.to_type, thunderduck_core::types::DataType::Integer);
        let Expression::UnresolvedColumn(u1) = c1.expr.as_ref() else {
            panic!("expected UnresolvedColumn, got {:?}", c1.expr);
        };
        assert_eq!(u1.name, "a");
    }

    #[test]
    fn convert_to_schema_non_struct_schema_produces_boundary_error() {
        // schema = Long (not a struct) → must fail.
        let r = rel(proto::relation::RelType::ToSchema(Box::new(
            proto::ToSchema {
                input: Some(Box::new(table_scan_rel("t"))),
                schema: Some(proto::DataType {
                    kind: Some(proto::data_type::Kind::Long(Default::default())),
                }),
            },
        )));
        let name = convert_proto_shape_err(&r);
        assert_eq!(name, "ToSchema::schema");
    }

    #[test]
    fn convert_to_schema_missing_input_produces_boundary_error() {
        let schema = struct_data_type_proto(vec![(
            "x",
            proto::data_type::Kind::Integer(Default::default()),
        )]);
        let r = rel(proto::relation::RelType::ToSchema(Box::new(
            proto::ToSchema {
                input: None,
                schema: Some(schema),
            },
        )));
        let name = convert_proto_shape_err(&r);
        assert_eq!(name, "ToSchema::missing_input");
    }
}
