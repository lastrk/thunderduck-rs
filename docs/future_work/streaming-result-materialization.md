# Perf opportunity: query results are fully materialized inside DuckDB before "streaming"

**Status**: documented finding, not scheduled
**Date**: 2026-07-08 (analysis on `feat/v2-transpiler`)
**Area**: `crates/core/src/runtime/session.rs`, `crates/connect-server/src/service.rs`

> **⚠ Read this first: the true fix requires a patch to duckdb-rs.**
> Eliminating the one genuinely unnecessary materialization — switching the
> session thread from `Statement::query_arrow` to `Statement::stream_arrow` —
> cannot be done safely against duckdb-rs `1.10505.0` as published. The
> crate's streaming iterator (`ArrowStream` / `streaming_step`) maps a
> **mid-stream runtime error to `None`**, which is indistinguishable from a
> clean end-of-stream. τ's ANSI error emulation (ADR-006) relies on
> row-dependent `error()` throws (divide-by-zero guards, cast overflow, …)
> surfacing as errors on the wire; with a naive switch they would instead
> surface as a silently **truncated but "successful"** result followed by
> `ResultComplete`. duckdb-rs must first be patched (upstream PR or vendored
> fork) to expose the result's error state (`duckdb_result_error`) after the
> last chunk. Until then, the materialized path is the *correct* one.

## Current behavior

The response path is architecturally streaming end-to-end:

```
session thread (owns !Send Connection)
  → bounded mpsc (4 batches)            DuckDbSession::execute_streaming
  → stream::unfold(streaming_step)      service.rs — per-batch transcode/stamp/IPC
  → tonic ExecutePlanStream             gRPC flow control
```

But the head of that pipeline is a streaming façade. The
`SessionCommand::ExecuteStreaming` handler (`session.rs`, `session_loop`)
executes via:

```rust
let arrow = stmt.query_arrow(duckdb::params![])?;   // ← materializes
for batch in arrow { ... blocking_send(StreamBatch::Batch(batch)) ... }
```

In duckdb-rs `1.10505.0`, `query_arrow` → `RawStatement::execute()` →
`duckdb_execute_prepared_arrow`, DuckDB's **materialized-result** C API: the
query runs to completion and the full result set is buffered inside DuckDB
before the first chunk is handed to the iterator. The `for` loop only drains
2048-row chunks out of an already-complete result.

Consequences:

- **Peak memory is O(full result size)** inside DuckDB, regardless of the
  4-slot mpsc buffer. The buffer bounds only the Rust-side `RecordBatch`
  copies in flight.
- **Backpressure is ineffective where it matters.** A slow client does not
  slow the engine down; it only extends how long the fully materialized
  result is held.
- **Time-to-first-byte equals full query time**, even for plans a streaming
  pipeline could start emitting immediately (scans, filters, projections).

Everything *downstream* of this point is genuinely incremental and needs no
change: `streaming_step` holds exactly one batch at a time, the stamped wire
`Arc<Schema>` is built once and reused, the interval transcode rewrites
columns per batch with no intermediate `RecordBatch`, and Arrow-IPC encoding
is per-frame.

## The fix (blocked on duckdb-rs)

duckdb-rs already ships the right API: `Statement::stream_arrow(params,
schema)` → `duckdb_execute_prepared_streaming` + `duckdb_stream_fetch_chunk`,
which executes the pipeline incrementally as chunks are pulled. With it, the
bounded mpsc provides true end-to-end backpressure into the engine.

Two prerequisites:

1. **duckdb-rs patch — mid-stream error visibility (the blocker).**
   `RawStatement::streaming_step` returns `Option<StructArray>`; a runtime
   error during `duckdb_stream_fetch_chunk` yields `None`, identical to
   end-of-stream. The patch must let the caller distinguish the two (e.g.
   check `duckdb_result_error` on the held `duckdb_result` after the final
   chunk, or return `Result<Option<_>>`). The contract to preserve is pinned
   by `session.rs` test `runtime_error_during_iteration_surfaces_as_err`
   (ADR-006 Piece B1): a row-dependent engine throw MUST surface as an error,
   never as truncated success. The DataFrame corpus's ANSI error cases
   (divide-by-zero, `element_at` OOB, cast overflow) are the end-to-end
   witnesses.

2. **Schema for the streaming decoder.** `stream_arrow` requires a
   `SchemaRef` up front, and `stream_step` uses it for FFI decoding — a wrong
   schema mis-decodes data. Safe source: a cheap `LIMIT 0` probe before the
   real execution; the machinery already exists in the `SchemaOf` handler
   (including `find_trailing_limit` flattening so duplicate column names
   survive). τ's `resolved_schema` is **not** a safe source — it is the
   Spark-visible view, which is exactly why `arrow_schema_stamp` exists to
   reconcile it with DuckDB's actual output types.

Note the bounded expectation: even with true streaming, pipeline-breaking
operators (ORDER BY, hash aggregate, window) still materialize inside DuckDB —
but there it is governed by DuckDB's `memory_limit` and spill-to-disk, which
is the right place for it. The unconditional result buffer is what's
avoidable.

## Secondary observations

- **`run_query` double-buffers** (`session.rs`, the `SessionCommand::Execute`
  path): `arrow_stream.collect::<Vec<RecordBatch>>()` on top of the
  already-materialized DuckDB result → briefly ~2× the result in memory. Its
  only production caller today is the implicit-pivot/crosstab discovery
  pre-pass (`resolve_implicit_pivots` → small `SELECT DISTINCT`), so it is
  benign — but `DuckDbSession::execute()` is a footgun if anyone routes real
  result sets through it instead of `execute_streaming`. At minimum its doc
  comment should say so.

- **Frame coalescing companion win.** The session thread forwards DuckDB's
  native 2048-row chunks 1:1 into wire frames, so large results produce many
  small gRPC messages (Spark's own server coalesces to ~4 MB `ArrowBatch`es,
  `spark.connect.grpc.arrow.maxBatchSize`). Whoever touches this path for the
  streaming fix should coalesce batches on the session thread — a small
  bounded buffer for far fewer frames, without reintroducing unbounded
  materialization.

- **Input side (adjacent, out of scope):** `local_relation_to_values_sql`
  expands `createDataFrame` payloads into a `VALUES` SQL string — a size
  multiplier on ingest, though the proto message arrives fully materialized
  anyway.

## Verification sketch (when implemented)

- Unit: extend/keep `runtime_error_during_iteration_surfaces_as_err` semantics
  against the streaming path (error after N successful chunks must yield
  `StreamBatch::Error`, not `Complete`).
- Corpus: `tests/scripts/v2-progress.sh` must stay green, in particular the
  ANSI error-emulation cases.
- Memory: a `range()`-style wide scan with a slow client should show flat
  server RSS instead of O(result) growth; time-to-first-batch on a large scan
  should drop from full-query time to near-immediate.
