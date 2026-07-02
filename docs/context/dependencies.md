# Dependencies & Configuration

> **Scope: applies to both legacy and (future) v2 code.** External dependencies (the `thdck_spark_funcs` extension, protobuf, Arrow, Tokio) are shared substrate. Value-level types (`DataType`/`StructType`/`StructField`) are also shared per ADR-021. Under ADR-021, however, v2 will have its **own** `Expression` enum and its **own** `TypeInferenceEngine` — those are peer implementations to legacy's, validated by ADR-015's differential oracle, not delegating clients. This file is authoritative on shared externals; for v2's substrate-independence commitments see `docs/thunderduck-rearchitect-ADRs.md` §ADR-021 and INV10.

## Spark Compatibility Extension

Spark parity is the only emission target. The `thdck_spark_funcs` extension is mandatory and bundled into every build (see [rearchitect ADR-020](../thunderduck-rearchitect-ADRs.md)).

```bash
cargo build --release
```

### Version Pinning

The `.duckdb_extension` binary's embedded DuckDB version must exactly match the `duckdb` crate version in `Cargo.toml`. Currently pinned to:

- Extension release: `ext6` (multi-version — pulls the `v1.5.4` binaries)
- `duckdb` crate: `1.10504.0`

On the first build, `build.rs` downloads the correct platform binary from the GitHub releases of `thunderduck-duckdb-extension` and caches it under `extensions/ext6/` (gitignored). The binary is embedded via `include_bytes!()` and loaded at every session's startup; failure to load is a hard error.

### `thdck_spark_funcs` Extension Functions

The extension implements Spark-precise numerical semantics:

| Function | Replaces | Behavior |
|----------|----------|----------|
| `spark_hash(c1, …, cN)` | Spark `hash()` | Murmur3-32, signed INT, seed 42 |
| `spark_xxhash64(c1, …, cN)` | Spark `xxhash64()` | xxHash64, signed BIGINT, seed 42 |
| `spark_decimal_div(a, b)` | Decimal `/` | `ROUND_HALF_UP` |
| `spark_sum(col)` | `SUM` | Spark-compatible return types |
| `spark_avg(col)` | `AVG` | Spark-compatible return types |

Full details: [extension-loading.md](../adrs/runtime/extension-loading.md) (see [`docs/adrs/README.md`](../adrs/README.md) for the ADR index).

## Spark Connect Configuration

- **Protobuf**: `tonic` + `prost`; protos compiled at build time from `protos/`.
- **Arrow**: zero-copy IPC streaming — `arrow` crate shares the same dependency tree as `duckdb-rs` so no version mismatch is possible.
- **Async runtime**: `tokio` multi-thread scheduler for gRPC; session work runs on a dedicated OS thread (see [architecture.md](architecture.md) — DuckDB threading model).
