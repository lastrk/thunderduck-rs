use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;

use crate::error::{ConnectError, Result};
use crate::proto::spark::connect::execute_plan_response::ArrowBatch;

/// Serialize a single `RecordBatch` into a proto `ArrowBatch` message.
///
/// Each batch is written as an independent Arrow IPC stream so that the
/// client can decode it individually.
pub fn record_batch_to_arrow_batch(batch: &RecordBatch) -> Result<ArrowBatch> {
    let estimated_size = 128usize.saturating_add(
        batch
            .num_rows()
            .saturating_mul(batch.num_columns())
            .saturating_mul(64),
    );
    let mut buf = Vec::with_capacity(estimated_size);
    {
        let mut writer = StreamWriter::try_new(&mut buf, &batch.schema())
            .map_err(|e| ConnectError::Arrow(format!("IPC writer init: {e}")))?;
        writer
            .write(batch)
            .map_err(|e| ConnectError::Arrow(format!("IPC write: {e}")))?;
        writer
            .finish()
            .map_err(|e| ConnectError::Arrow(format!("IPC finish: {e}")))?;
    }
    Ok(ArrowBatch {
        row_count: i64::try_from(batch.num_rows()).unwrap_or(i64::MAX),
        data: buf,
        ..Default::default()
    })
}

/// Serialize a slice of `RecordBatch`es into proto `ArrowBatch` messages.
///
/// Each batch (including 0-row schema-only batches) is written as an independent
/// Arrow IPC stream so that the client can decode them individually.
/// A 0-row batch carries the schema, which PySpark requires to build a table.
pub fn record_batches_to_arrow_batches(batches: &[RecordBatch]) -> Result<Vec<ArrowBatch>> {
    let mut out = Vec::with_capacity(batches.len());
    for batch in batches {
        let estimated_size = 128usize.saturating_add(
            batch
                .num_rows()
                .saturating_mul(batch.num_columns())
                .saturating_mul(64),
        );
        let mut buf = Vec::with_capacity(estimated_size);
        {
            let mut writer = StreamWriter::try_new(&mut buf, &batch.schema())
                .map_err(|e| ConnectError::Arrow(format!("IPC writer init: {e}")))?;
            writer
                .write(batch)
                .map_err(|e| ConnectError::Arrow(format!("IPC write: {e}")))?;
            writer
                .finish()
                .map_err(|e| ConnectError::Arrow(format!("IPC finish: {e}")))?;
        }
        out.push(ArrowBatch {
            row_count: i64::try_from(batch.num_rows()).unwrap_or(i64::MAX),
            data: buf,
            ..Default::default()
        });
    }
    Ok(out)
}
