//! Per-batch Arrow interval-column transcoder.
//!
//! DuckDB emits every SQL `INTERVAL`-typed column as Arrow
//! `Interval(MonthDayNano)` (16 B per row: i32 months, i32 days, i64 nanos).
//! Spark 4.1's Arrow wire encoding uses a different Arrow type for each of
//! Spark's three interval semantic types:
//!
//! | Spark type              | Spark Arrow wire type          |
//! |-------------------------|--------------------------------|
//! | `DayTimeIntervalType`   | `Duration(MICROSECOND)` (i64)  |
//! | `CalendarIntervalType`  | `Interval(MonthDayNano)`       |
//! | `YearMonthIntervalType` | `Interval(YEAR_MONTH)` (i32)   |
//!
//! PySpark's `_from_arrow_type` (see `pyspark/sql/pandas/types.py`) only has
//! decoder arms for `Duration(MICROSECOND)`; it has NO `is_interval` arm and
//! throws `UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION` on either
//! `Interval(MonthDayNano)` or `Interval(YearMonth)`. When the server sends a
//! dedicated proto `Schema` frame first (see
//! `build_schema_response` in `service.rs`), the client uses
//! `proto_schema_to_pyspark_data_type` (which has arms for all three interval
//! kinds) and skips the `_from_arrow_schema` fallback — but the row-level
//! Arrow decoder still needs the correct wire type per column. For
//! `DayTimeInterval`, that means transcoding to `Duration(Microsecond)`
//! server-side; the `CalendarInterval` case is already the correct
//! `Interval(MonthDayNano)` layout that pyarrow accepts.
//!
//! For every pure-DayTime DuckDB result, `months == 0` and the entire
//! quantity lives in `days` + `nanoseconds`. `INTERVAL 90 DAYS` is emitted as
//! `{months: 0, days: 90, nanos: 0}` — DuckDB does NOT fold days into months
//! at any rate. Formula:
//!
//! ```text
//! total_micros = days as i64 * 86_400_000_000 + nanoseconds / 1_000
//! ```
//!
//! Spark's legal range is roughly ±106.7M days (±292K years), which fits in
//! i64 microseconds, so wrapping arithmetic is safe for legal values.

use std::sync::Arc;

use arrow::array::{Array, ArrayRef, IntervalMonthDayNanoArray, PrimitiveArray, RecordBatch};
use arrow::buffer::ScalarBuffer;
use arrow::datatypes::{DataType as ArrowDt, DurationMicrosecondType, IntervalUnit, TimeUnit};
use arrow::error::ArrowError;
use thiserror::Error;
use thunderduck_core::types::{DataType as TdckDt, StructType as TdckStruct};

use crate::error::ConnectError;

/// Per-column transcode target. Closed set (Spark has exactly three interval
/// semantic types) — enum, not trait object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntervalTarget {
    /// Non-interval column, or `DataType::Interval` (CalendarInterval —
    /// DuckDB already emits the target `Interval(MonthDayNano)`). Zero-cost
    /// passthrough: the column's `Arc<ArrayData>` is reused verbatim.
    NoOp,
    /// τ `DataType::DayTimeInterval` — transcode DuckDB `Interval(MonthDayNano)`
    /// to Spark's `Duration(Microsecond)`.
    DayTimeToDurationMicros,
    /// τ `DataType::YearMonthInterval` — reserved. Passed through as
    /// `Interval(MonthDayNano)` so the client raises the SAME
    /// `UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION` error class as Spark's
    /// reference (`.reference/`) does. When PySpark grows a decoder we can
    /// implement the actual transcode without changing the enum surface.
    YearMonthPassthrough,
}

/// A per-query plan built once from τ's `resolved_schema`; reused for every
/// batch. `all_noop = true` short-circuits `apply()` to a no-op — non-interval
/// queries pay zero column-scan cost.
#[derive(Debug, Clone)]
pub struct IntervalPlan {
    targets: Vec<IntervalTarget>,
    all_noop: bool,
}

impl IntervalPlan {
    /// Build the plan by walking `resolved_schema`'s top-level fields.
    /// Nested intervals (e.g. `Array<DayTimeInterval>`) are intentionally not
    /// walked; top-level interval columns are the wire boundary supported here.
    pub fn build(resolved_schema: &TdckStruct) -> Self {
        let mut all_noop = true;
        let targets: Vec<IntervalTarget> = resolved_schema
            .fields
            .iter()
            .map(|f| match &f.data_type {
                TdckDt::DayTimeInterval { .. } => {
                    all_noop = false;
                    IntervalTarget::DayTimeToDurationMicros
                }
                TdckDt::YearMonthInterval { .. } => {
                    all_noop = false;
                    IntervalTarget::YearMonthPassthrough
                }
                _ => IntervalTarget::NoOp,
            })
            .collect();
        Self { targets, all_noop }
    }

    /// Fast-path predicate — callers with `is_noop() == true` should skip
    /// `apply()` entirely and reuse the input batch's `Arc<ArrayData>`.
    #[inline]
    pub fn is_noop(&self) -> bool {
        self.all_noop
    }
}

/// Loud-fail contract. DuckDB is expected to emit `Interval(MonthDayNano)`
/// for every INTERVAL-typed column; a runtime mismatch is a Thunderduck-boundary
/// error per ADR-022, not a client-input problem.
#[derive(Debug, Error)]
pub enum TranscodeError {
    #[error(
        "arrow_interval_transcode: expected Interval(MonthDayNano) for column {col} \
         (τ resolved type = {tdck}); got {actual:?}"
    )]
    UnexpectedArrowType {
        col: usize,
        tdck: &'static str,
        actual: ArrowDt,
    },

    #[error("arrow_interval_transcode: RecordBatch reconstruction failed: {0}")]
    RebuildFailed(#[from] ArrowError),

    #[error(
        "arrow_interval_transcode: transcode plan / batch column-count mismatch: plan={plan}, batch={batch}"
    )]
    ColumnCountMismatch { plan: usize, batch: usize },
}

impl From<TranscodeError> for ConnectError {
    fn from(e: TranscodeError) -> Self {
        ConnectError::Arrow(e.to_string())
    }
}

/// Apply the plan to a batch, returning the transcoded column list only.
///
/// Non-target columns share the original `Arc<ArrayData>` (Arc::clone bumps
/// refcount; no data copy). Target columns allocate exactly one `Vec<i64>`
/// for the output values buffer. On the noop path the input batch's own
/// `columns()` slice is cloned into a fresh `Vec` — no per-column allocation.
///
/// The final wire `RecordBatch` is built by the caller from
/// `(stamped_schema, columns)`.
pub fn apply(batch: &RecordBatch, plan: &IntervalPlan) -> Result<Vec<ArrayRef>, TranscodeError> {
    if plan.is_noop() {
        return Ok(batch.columns().to_vec());
    }
    if plan.targets.len() != batch.num_columns() {
        return Err(TranscodeError::ColumnCountMismatch {
            plan: plan.targets.len(),
            batch: batch.num_columns(),
        });
    }

    let mut new_cols: Vec<ArrayRef> = Vec::with_capacity(batch.num_columns());
    for (i, col) in batch.columns().iter().enumerate() {
        match plan.targets[i] {
            IntervalTarget::NoOp | IntervalTarget::YearMonthPassthrough => {
                new_cols.push(Arc::clone(col));
            }
            IntervalTarget::DayTimeToDurationMicros => {
                new_cols.push(transcode_daytime_to_duration_us(col.as_ref(), i)?);
            }
        }
    }
    Ok(new_cols)
}

/// DuckDB `Interval(MonthDayNano)` → Spark `Duration(Microsecond)`.
///
/// One heap allocation for the output i64 buffer; the input MonthDayNano
/// buffer stays live behind an `Arc` in the source `ArrayData` (not touched
/// by this function). Nulls are shared by `Arc::clone` of the input's
/// `NullBuffer`.
fn transcode_daytime_to_duration_us(
    input: &dyn Array,
    col_index: usize,
) -> Result<ArrayRef, TranscodeError> {
    let src = input
        .as_any()
        .downcast_ref::<IntervalMonthDayNanoArray>()
        .ok_or_else(|| TranscodeError::UnexpectedArrowType {
            col: col_index,
            tdck: "DayTimeInterval",
            actual: input.data_type().clone(),
        })?;

    let n = src.len();
    let mut out: Vec<i64> = Vec::with_capacity(n);
    for v in src.values() {
        let micros = (v.days as i64)
            .wrapping_mul(86_400_000_000)
            .wrapping_add(v.nanoseconds / 1_000);
        out.push(micros);
    }

    let nulls = src.nulls().cloned();
    let arr = PrimitiveArray::<DurationMicrosecondType>::new(ScalarBuffer::from(out), nulls);
    Ok(Arc::new(arr) as ArrayRef)
}

/// True when the Arrow type matches what DuckDB is expected to emit for
/// every INTERVAL semantic (namely `Interval(MonthDayNano)`). Used by the
/// stamp module to accept both the pre- and post-transcode shapes.
pub(crate) fn is_arrow_interval_month_day_nano(dt: &ArrowDt) -> bool {
    matches!(dt, ArrowDt::Interval(IntervalUnit::MonthDayNano))
}

/// True when the Arrow type matches Spark's DayTimeInterval wire encoding
/// (`Duration(Microsecond)`).
pub(crate) fn is_arrow_duration_micros(dt: &ArrowDt) -> bool {
    matches!(dt, ArrowDt::Duration(TimeUnit::Microsecond))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, DurationMicrosecondArray, Int64Array};
    use arrow::datatypes::{Field, IntervalMonthDayNano, Schema};
    use thunderduck_core::types::{DataType as TdckDt, StructField as TdckField};

    fn tdck_struct(fields: Vec<(&str, TdckDt)>) -> TdckStruct {
        TdckStruct::new(
            fields
                .into_iter()
                .map(|(n, dt)| TdckField::nullable(n, dt))
                .collect(),
        )
    }

    /// `is_noop()` is `true` iff every field is non-interval OR `Interval`
    /// (CalendarInterval — DuckDB's native layout is already correct).
    #[test]
    fn plan_is_noop_for_non_interval_and_calendar_columns() {
        let plan = IntervalPlan::build(&tdck_struct(vec![
            ("i", TdckDt::Long),
            ("s", TdckDt::String),
            ("cv", TdckDt::Interval),
        ]));
        assert!(plan.is_noop(), "no DayTime/YearMonth column ⇒ is_noop");
    }

    #[test]
    fn plan_is_not_noop_when_daytime_present() {
        let plan = IntervalPlan::build(&tdck_struct(vec![
            ("i", TdckDt::Long),
            ("dt", TdckDt::day_time_full()),
        ]));
        assert!(!plan.is_noop());
    }

    /// Non-interval batch through a noop plan: same `Arc` — no data copy.
    #[test]
    fn apply_noop_returns_input_batch() {
        let schema = Arc::new(Schema::new(vec![Field::new("x", ArrowDt::Int64, true)]));
        let col: ArrayRef = Arc::new(Int64Array::from(vec![1, 2, 3]));
        let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::clone(&col)]).unwrap();
        let plan = IntervalPlan::build(&tdck_struct(vec![("x", TdckDt::Long)]));

        let out = apply(&batch, &plan).expect("apply(noop) must succeed");
        assert_eq!(out.len(), 1);
        assert!(
            Arc::ptr_eq(&col, &out[0]),
            "noop plan must reuse the input column Arc verbatim",
        );
    }

    /// 1 day → 86_400_000_000 microseconds; no sub-day.
    #[test]
    fn daytime_transcode_pure_days() {
        let src = IntervalMonthDayNanoArray::from(vec![IntervalMonthDayNano::new(0, 1, 0)]);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "dt",
            ArrowDt::Interval(IntervalUnit::MonthDayNano),
            true,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(src)]).unwrap();
        let plan = IntervalPlan::build(&tdck_struct(vec![("dt", TdckDt::day_time_full())]));

        let out = apply(&batch, &plan).expect("apply must succeed");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].data_type(),
            &ArrowDt::Duration(TimeUnit::Microsecond)
        );
        let arr = out[0]
            .as_any()
            .downcast_ref::<DurationMicrosecondArray>()
            .expect("Duration(us) array");
        assert_eq!(arr.value(0), 86_400_000_000);
    }

    /// 1 day 500_000_000 ns → 86_400_500_000 microseconds.
    #[test]
    fn daytime_transcode_days_and_nanos() {
        let src =
            IntervalMonthDayNanoArray::from(vec![IntervalMonthDayNano::new(0, 1, 500_000_000)]);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "dt",
            ArrowDt::Interval(IntervalUnit::MonthDayNano),
            true,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(src)]).unwrap();
        let plan = IntervalPlan::build(&tdck_struct(vec![("dt", TdckDt::day_time_full())]));

        let out = apply(&batch, &plan).expect("apply must succeed");
        let arr = out[0]
            .as_any()
            .downcast_ref::<DurationMicrosecondArray>()
            .expect("Duration(us) array");
        assert_eq!(arr.value(0), 86_400_500_000);
    }

    /// Nulls preserved through the transcode (validity buffer is Arc-shared).
    #[test]
    fn daytime_transcode_preserves_nulls() {
        let vals = vec![
            Some(IntervalMonthDayNano::new(0, 1, 0)),
            None,
            Some(IntervalMonthDayNano::new(0, 2, 0)),
        ];
        let src = IntervalMonthDayNanoArray::from(vals);
        assert!(src.nulls().is_some());
        let schema = Arc::new(Schema::new(vec![Field::new(
            "dt",
            ArrowDt::Interval(IntervalUnit::MonthDayNano),
            true,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(src)]).unwrap();
        let plan = IntervalPlan::build(&tdck_struct(vec![("dt", TdckDt::day_time_full())]));

        let out = apply(&batch, &plan).expect("apply must succeed");
        let arr = out[0]
            .as_any()
            .downcast_ref::<DurationMicrosecondArray>()
            .expect("Duration(us) array");
        assert_eq!(arr.len(), 3);
        assert!(arr.is_valid(0));
        assert!(!arr.is_valid(1), "middle row must be null");
        assert!(arr.is_valid(2));
        assert_eq!(arr.value(0), 86_400_000_000);
        assert_eq!(arr.value(2), 172_800_000_000);
    }

    /// Feeding a `Duration(us)` where the plan expects `Interval(MonthDayNano)`
    /// (i.e. DuckDB regressed) → `UnexpectedArrowType` error.
    #[test]
    fn unexpected_arrow_type_returns_error() {
        let bogus = DurationMicrosecondArray::new(ScalarBuffer::from(vec![1_000_000i64]), None);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "dt",
            ArrowDt::Duration(TimeUnit::Microsecond),
            true,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(bogus) as ArrayRef]).unwrap();
        let plan = IntervalPlan::build(&tdck_struct(vec![("dt", TdckDt::day_time_full())]));

        match apply(&batch, &plan) {
            Err(TranscodeError::UnexpectedArrowType { col: 0, tdck, .. }) => {
                assert_eq!(tdck, "DayTimeInterval");
            }
            other => panic!("expected UnexpectedArrowType at col 0, got {other:?}"),
        }
    }

    /// YearMonthInterval passes through unchanged (server-side); the client
    /// will raise UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION exactly like it
    /// does against the Spark reference (matches intv-002 error parity).
    #[test]
    fn yearmonth_column_passes_through_unchanged() {
        let src = IntervalMonthDayNanoArray::from(vec![IntervalMonthDayNano::new(27, 0, 0)]);
        let src_arc: ArrayRef = Arc::new(src);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "ymi",
            ArrowDt::Interval(IntervalUnit::MonthDayNano),
            true,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::clone(&src_arc)]).unwrap();
        let plan = IntervalPlan::build(&tdck_struct(vec![("ymi", TdckDt::year_month_full())]));

        let out = apply(&batch, &plan).expect("passthrough must succeed");
        assert_eq!(out.len(), 1);
        assert!(Arc::ptr_eq(&src_arc, &out[0]));
        assert_eq!(
            out[0].data_type(),
            &ArrowDt::Interval(IntervalUnit::MonthDayNano)
        );
    }

    /// The caller combines transcoded columns with the stamped wire schema in
    /// one `RecordBatch` construction.
    #[test]
    fn one_shot_batch_construction_from_cols_and_wire_schema() {
        use arrow::array::RecordBatchOptions;

        let src =
            IntervalMonthDayNanoArray::from(vec![IntervalMonthDayNano::new(0, 1, 500_000_000)]);
        let src_schema = Arc::new(Schema::new(vec![Field::new(
            "duck_name",
            ArrowDt::Interval(IntervalUnit::MonthDayNano),
            true,
        )]));
        let batch = RecordBatch::try_new(Arc::clone(&src_schema), vec![Arc::new(src)]).unwrap();
        let plan = IntervalPlan::build(&tdck_struct(vec![("dt", TdckDt::day_time_full())]));

        let cols = apply(&batch, &plan).expect("apply must succeed");
        assert_eq!(cols.len(), 1);
        assert_eq!(
            cols[0].data_type(),
            &ArrowDt::Duration(TimeUnit::Microsecond),
            "transcoded column carries the post-transcode Arrow type",
        );

        let wire_schema = Arc::new(Schema::new(vec![Field::new(
            "renamed_dt",
            ArrowDt::Duration(TimeUnit::Microsecond),
            true,
        )]));

        let opts = RecordBatchOptions::new()
            .with_match_field_names(false)
            .with_row_count(Some(batch.num_rows()));
        let out = RecordBatch::try_new_with_options(Arc::clone(&wire_schema), cols, &opts)
            .expect("one-shot RecordBatch::try_new_with_options must succeed");

        assert!(
            Arc::ptr_eq(&wire_schema, &out.schema()),
            "wire schema Arc identity must be preserved (no re-wrapping in the pipeline)",
        );
        assert_eq!(out.schema().field(0).name(), "renamed_dt");
        let arr = out
            .column(0)
            .as_any()
            .downcast_ref::<DurationMicrosecondArray>()
            .expect("Duration(us) column");
        assert_eq!(arr.value(0), 86_400_500_000);
    }
}
