# Dependencies & Configuration

> **Scope: τ (the only production path per ADR-022).** External dependencies (the `thdck_spark_funcs` extension, protobuf, Arrow, Tokio) plus value-level types (`DataType`/`StructType`/`StructField`) are τ's substrate. τ owns its `Expression` enum and its `TypeInferenceEngine` (INV10). This file is authoritative on shared externals; for τ's substrate-independence commitments see [ADR-021](../adrs/adr-021-tau-substrate.md) and [Cross-Validation](../adrs/cross-validation.md).

## Spark Compatibility Extension

Spark parity is the only emission target. The `thdck_spark_funcs` extension is mandatory and bundled into every build (see [ADR-020](../adrs/adr-020-strict-only-target.md)).

```bash
cargo build --release
```

### Version Pinning

The `.duckdb_extension` binary's embedded DuckDB version must exactly match the linked DuckDB library version. Currently pinned to:

- Extension release: rebuilt from this branch (pulls the `v1.5.5` binaries)
- `duckdb` crate: crates.io `1.10505.0` API bindings
- linked DuckDB library: official `v1.5.5` static release archive

All 4 platform binaries (`linux_amd64`, `linux_arm64`, `osx_amd64`, `osx_arm64`) of the adopted release are vendored — checked into git plain (uncompressed) under `extensions/vendored/` (`MANIFEST.toml` + one `.duckdb_extension` per platform), exactly one version at a time. `build.rs` picks the binary matching `TARGET` at build time and embeds it via `include_bytes!()`; extension embedding needs no network access. `scripts/dev/duckdb-build-cache.sh` downloads the official static DuckDB archive before a build, verifies its pinned SHA-256 checksum, merges its component archives, and caches the result outside `target/`. Cargo does not download DuckDB during its build step. Adopting a new release (only on `duckdb` crate bumps) is done via `scripts/dev/adopt-extension-release.sh <release-tag> <duckdb-version>` (legacy release-download mode) or `scripts/dev/adopt-extension-release.sh --from-local <dir>` (adopts locally- or CI-built binaries, regenerating the manifest's `[source]` block with in-tree provenance instead of a release tag), either of which also regenerates the manifest's per-artifact sha256/size and supports a `--verify` mode. The extension is loaded at every session's startup; failure to load is a hard error.

**Source is now in-tree.** The extension's C++ source (formerly an external repository, `nubank/thunderduck-duckdb-extension`, slated for archival — gates and commands in `docs/context/extension-archival-checklist.md`) lives at [`extension/`](../../extension/), with `extension/duckdb` and `extension/extension-ci-tools` as pinned git submodules — see `extension/BUILD_PINS.toml` for the authoritative pin. Local dev builds (host platform only, single pinned DuckDB version — no multiversion builds) go through `scripts/dev/build-extension.sh` (`--init` to fetch the submodules, `--smoke` to also run the extension's own test suite plus a swap-in proof against `thunderduck-core`'s `extension_loader` tests via `THUNDERDUCK_EXT_PATH`). Producing new vendored binaries for all 4 shipped platforms is a `workflow_dispatch`-only CI job, `.github/workflows/extension-release.yml`, which opens a PR rather than pushing directly.

### `thdck_spark_funcs` Extension Functions

The extension implements Spark-precise numerical semantics:

| Function | Replaces | Behavior |
|----------|----------|----------|
| `spark_hash(c1, …, cN)` | Spark `hash()` | Murmur3-32, signed INT, seed 42 |
| `spark_xxhash64(c1, …, cN)` | Spark `xxhash64()` | xxHash64, signed BIGINT, seed 42 |
| `spark_decimal_div(a, b)` | Decimal `/` | `ROUND_HALF_UP` |
| `spark_sum(col)` | `SUM` | Spark-compatible return types |
| `spark_avg(col)` | `AVG` | Spark-compatible return types |
| `spark_skewness(col)` | `skewness()` | Population skewness (Spark semantics) |

Full details: [extension-loading.md](../adrs/runtime/extension-loading.md) (see [`docs/adrs/README.md`](../adrs/README.md) for the ADR index).

## Spark Connect Configuration

- **Protobuf**: `tonic` + `prost`; protos compiled at build time from `protos/`.
- **Arrow**: zero-copy IPC streaming — `arrow` crate shares the same dependency tree as `duckdb-rs` so no version mismatch is possible.
- **Column regexes**: `java_regex` mirrors JVM `Pattern` for Spark-compatible `colRegex` expansion without starting a JVM.
- **Async runtime**: `tokio` multi-thread scheduler for gRPC; session work runs on a dedicated OS thread (see [architecture.md](architecture.md) — DuckDB threading model).
