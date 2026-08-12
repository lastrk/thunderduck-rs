# Thunderduck (Rust)

[![Cargo Build](https://img.shields.io/badge/cargo-1.97.1-blue.svg)](https://doc.rust-lang.org/cargo/)
[![Rust](https://img.shields.io/badge/rust-1.97.1-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)

> **Alpha software.** Thunderduck is under active Spark-compatibility development. Use it for the supported batch surface, and treat an `UNIMPLEMENTED` response as an intentional Thunderduck boundary rather than Spark compatibility.

Thunderduck is a single-node [Spark Connect](https://spark.apache.org/docs/latest/spark-connect-overview.html) server backed by DuckDB. It translates supported Spark DataFrame and Spark SQL plans into DuckDB SQL, then returns Arrow results over the Spark Connect protocol.

It is not a complete Apache Spark replacement: it has no distributed execution or Structured Streaming, and many catalog, administration, extension, ML, Pandas, and persistence APIs remain outside its supported surface. The authoritative compatibility contract is indexed in the [architecture decisions](docs/adrs/README.md). The intended behavior for a Spark-valid but unsupported input is an explicit Thunderduck boundary; the [Spark parity report](SPARK_PARITY_REPORT.md) records the current supported surface and known gaps in that contract.

## What works today

- A Spark Connect batch-query surface validated against Apache Spark 4.1.1, including projection, filtering, joins, aggregation, windows, set operations, CTEs, scalar and correlated SQL subqueries, complex types, higher-order functions, and structured generators.
- Spark SQL queries and a bounded DDL/DML set: views, simple tables, inserts, and truncation. See the supported statement IR in [statement.rs](crates/core/src/transpiler_v2/statement.rs).
- PySpark Connect 4.1.1 is the primary tested client.
- Local Arrow relations and Arrow result batches.
- Parquet, CSV/text, JSON, and single-directory Delta reads. ORC and Iceberg reads are currently unsupported.
- Two bounded path writes: Delta append to an existing Delta table and Parquet overwrite.
- A mandatory bundled `thdck_spark_funcs` DuckDB extension for Spark-specific type, decimal, hash, and aggregate semantics.

The live function registry contains 352 supported public function spellings, each with explicit scalar, aggregate, generator, special, or frontend-lowered handling. A spelling is not a promise that every Spark overload or option is available; unsupported shapes return a boundary instead of falling through to DuckDB.

## Deliberate boundaries

Thunderduck currently rejects, among other things:

- Structured Streaming and distributed/shuffle execution.
- RDD and low-level Spark APIs.
- Cache/persist/checkpoint and most session/catalog administration.
- UDF/UDAF/UDTF registration, Pandas/group-map operations, ML, and general table-valued functions.
- Most write modes, table saves, and WriteOperationV2. Write layout options are currently accepted but not applied.
- Explain-plan analysis, storage-level analysis, artifacts, operation reattachment, and session cloning.

Some Spark SQL syntax is also intentionally bounded where τ cannot yet model its semantics. The server distinguishes these gaps from Spark-invalid input: Spark-invalid input receives a Spark-emulated error where implemented; Spark-valid but unsupported input should receive `UNIMPLEMENTED`. See the parity report for known SQL-lowering cases that still need that guard.

## Quick start

### Prerequisites

- Rust 1.97.1 with Cargo (pinned by [rust-toolchain.toml](rust-toolchain.toml)).
- Python and PySpark 4.1.1 only when running the differential suite.

`protoc` is vendored by the build; it is not a host prerequisite.

### Build and run

```bash
git clone https://github.com/nubank/thunderduck-rs.git
cd thunderduck-rs

# Fresh clones / CI compile DuckDB from source.
cargo build --release --features bundled

# Start the Spark Connect server on 0.0.0.0:15002.
./target/release/thunderduck-connect-server
```

For repeat development inside the devcontainer, `scripts/dev/dev-cache-setup.sh` prepares a shared prebuilt DuckDB library; then ordinary `cargo build` and `cargo test` work without `--features bundled`. See [scripts/dev/README.md](scripts/dev/README.md).

Connect with PySpark:

```python
from pyspark.sql import SparkSession

spark = (
    SparkSession.builder
    .remote("sc://localhost:15002")
    .getOrCreate()
)

df = spark.read.parquet("my_data.parquet")
rows = df.groupBy("category").sum("amount").collect()
print(rows)
```

The server does not implement the Connect `ShowString` relation, so use ordinary actions such as `collect()` rather than `DataFrame.show()`.

## Architecture

Both front ends share one production translation path, τ:

```
Spark Connect protobuf ─┐
                        ├─> CommonAst ─> analyzer ─> TypedAst ─> DuckDB SQL
Spark SQL text ─────────┘                                      │
                                                               ▼
                                                        DuckDB + Arrow
```

- `crates/core/src/transpiler_v2/` owns the common AST, Spark-aware analysis, function registry, and SQL emission.
- `crates/core/src/parser_v2/` parses Spark SQL into that same AST.
- `crates/connect-server/src/converter/v2_relation_converter.rs` converts Spark Connect protobuf into that AST.
- `crates/core/src/runtime/` owns each DuckDB connection on its dedicated session thread.
- `crates/connect-server/` implements the tonic Spark Connect service and Arrow wire bridge.

The concise architecture reference is [docs/context/architecture.md](docs/context/architecture.md). The [individual architecture decisions](docs/adrs/README.md) are authoritative when documentation disagrees.

### Spark compatibility extension

`thdck_spark_funcs` is embedded in every server build and loaded for every session. It closes semantic gaps that DuckDB SQL alone cannot express, including Spark-compatible hashing, decimal division, and selected aggregate return types. The extension source is in [extension/](extension/); the matching platform binaries are tracked under [extensions/vendored/](extensions/vendored/).

## Testing

The differential oracle compares τ against Apache Spark 4.1.1. The DataFrame and Spark SQL corpora include TPC-H and TPC-DS cases alongside focused compatibility witnesses. The corpora are the regression gates, not a claim of complete Spark API coverage.

```bash
# Rust quality gates
cargo fmt --check
cargo clippy -- -D warnings
cargo test

# Build the server used by the differential runner.
cargo build --release

# τ's DataFrame and Spark SQL conformance corpora.
./tests/scripts/run-differential-tests.sh core
./tests/scripts/run-differential-tests.sh sql_v2

# All differential tests.
./tests/scripts/run-differential-tests.sh all
```

See [docs/context/testing.md](docs/context/testing.md) for live-oracle recording, worktree isolation, Spark setup, and the current corpus inventory.

## Contributing

1. Read [AGENTS.md](AGENTS.md) and the relevant documents under [docs/context/](docs/context/).
2. Keep unsupported Spark-valid shapes as typed Thunderduck boundaries; do not silently weaken them to DuckDB behavior.
3. Add a focused unit or differential witness for behavioral changes.
4. Run format, lint, unit, and applicable differential gates before opening a pull request.

## License

This project is licensed under Apache License 2.0.

## Acknowledgments

- [DuckDB](https://duckdb.org/)
- [Apache Arrow](https://arrow.apache.org/)
- [Apache Spark](https://spark.apache.org/)
- [sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs)
- [tonic](https://github.com/hyperium/tonic)
