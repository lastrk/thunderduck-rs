# Dependencies & Configuration

## Spark Compatibility Extension

Two modes:

- **Relaxed** (default, no extension, ~85% compat — value-equivalent results, type equivalence not required)
- **Strict** (extension loaded, ~100% compat — exact Spark types, rounding, NULL semantics)

```bash
# Build WITHOUT extension (relaxed mode, default)
cargo build --release

# Build WITH extension (strict mode — downloads binary on first run)
cargo build --release --features bundled-extension
```

### Version Pinning

The `.duckdb_extension` binary's embedded DuckDB version must exactly match the `duckdb` crate version in `Cargo.toml`. Currently pinned to:

- Extension release: `ext4` (multi-version — pulls the `v1.5.1` binaries)
- `duckdb` crate: `1.10501.0`

On first `--features bundled-extension` build, `build.rs` downloads the correct platform binary from the GitHub releases of `thunderduck-duckdb-extension` and caches it under `extensions/` (gitignored). The binary is embedded via `include_bytes!()` and loaded at startup in strict mode.

### `thdck_spark_funcs` Extension Functions

The strict-mode extension implements Spark-precise numerical semantics:

| Function | Replaces | Behavior |
|----------|----------|----------|
| `spark_hash(c1, …, cN)` | Spark `hash()` | Murmur3-32, signed INT, seed 42 |
| `spark_xxhash64(c1, …, cN)` | Spark `xxhash64()` | xxHash64, signed BIGINT, seed 42 |
| `spark_decimal_div(a, b)` | Decimal `/` | `ROUND_HALF_UP` |
| `spark_sum(col)` | `SUM` | Spark-compatible return types |
| `spark_avg(col)` | `AVG` | Spark-compatible return types |

Full details: [adr-13-duckdb-extension-loading.md](../adrs/adr-13-duckdb-extension-loading.md) (see `docs/architecture.md` for the ADR index).

## Spark Connect Configuration

- **Protobuf**: `tonic` + `prost`; protos compiled at build time from `protos/`.
- **Arrow**: zero-copy IPC streaming — `arrow` crate shares the same dependency tree as `duckdb-rs` so no version mismatch is possible.
- **Async runtime**: `tokio` multi-thread scheduler for gRPC; session work runs on a dedicated OS thread (see [architecture.md](architecture.md) — DuckDB threading model).
