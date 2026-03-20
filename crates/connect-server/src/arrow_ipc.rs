use arrow::record_batch::RecordBatch;
use arrow_ipc::writer::StreamWriter;

use crate::error::{ConnectError, Result};
use crate::proto::spark::connect::execute_plan_response::ArrowBatch;

/// Serialize a slice of `RecordBatch`es into proto `ArrowBatch` messages.
///
/// Each batch (including 0-row schema-only batches) is written as an independent
/// Arrow IPC stream so that the client can decode them individually.
/// A 0-row batch carries the schema, which PySpark requires to build a table.
pub fn record_batches_to_arrow_batches(batches: &[RecordBatch]) -> Result<Vec<ArrowBatch>> {
    let mut out = Vec::new();
    for batch in batches {
        let mut buf = Vec::new();
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
            row_count: batch.num_rows() as i64,
            data: buf,
            ..Default::default()
        });
    }
    Ok(out)
}
