# Thunderduck (Rust)

[![Cargo Build](https://img.shields.io/badge/cargo-1.75+-blue.svg)](https://doc.rust-lang.org/cargo/)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)

> **Alpha Software**: Despite extensive test coverage, Thunderduck is currently alpha quality software and will undergo extensive testing with real-world workloads before production readiness.

**Thunderduck** is an embedded execution engine that translates Spark operations to DuckDB SQL, providing fast single-node query execution as a drop-in replacement for Apache Spark. This is the Rust port: same Spark API compatibility, ~50ms startup (vs ~10s JVM), and ~45MB baseline memory (vs ~500MB JVM). Achieves **100% pass rate** on the 835-test differential suite against Apache Spark 4.1.1.

### Key Features

- **Spark Connect Server** for remote client connectivity (PySpark 4.1.x, Scala Spark)
- **Faster than Spark local mode** via DuckDB's vectorized engine
- **Fast startup**: ~50ms cold start vs ~10s for the JVM reference implementation
- **Low memory**: ~45MB RSS at idle vs ~500MB for the JVM
- **Multi-architecture support**: x86_64 (Intel/AMD) and ARM64 (AWS Graviton, Apple Silicon)
- **Arrow-native data interchange** with DuckDB's vectorized engine
- **Format support**: Parquet, Delta Lake (PLANNED), Iceberg (PLANNED)
- **835 differential tests** against Spark 4.1.1 — TPC-H (100%), TPC-DS (100%), functions, joins, window, aggregations, lambdas, complex types
- **Exact Spark type parity** via the bundled `thdck_spark_funcs` DuckDB extension
- **Query plan introspection** via EXPLAIN statements

### Why Thunderduck?

Most Spark workloads [don't need distributed computing](https://motherduck.com/blog/big-data-is-dead/) — they'd run faster and cheaper on a single node. Thunderduck lets you keep your Spark code while replacing the execution engine with DuckDB's vectorized, SIMD-optimized columnar processing. Zero-copy Arrow interchange eliminates serialization overhead between layers.

The Rust port adds further gains: near-instant startup eliminates JVM warm-up time, and the low memory footprint means you can run more workloads on smaller instances.

### Platform Support

Thunderduck supports **x86_64** (Intel/AMD) and **ARM64** (AWS Graviton, Apple Silicon) architectures. DuckDB automatically applies SIMD optimizations per architecture.

## Quick Start

### Prerequisites

- **Rust** 1.75+ with Cargo (required)
- **Python** 3.11+ (required for differential tests)
- **protoc** (Protocol Buffers compiler, required for gRPC code generation)

### Build and Run

```bash
git clone https://github.com/lastrk/thunderduck-rs.git
cd thunderduck-rs

# Build the release binary (--features bundled compiles DuckDB from source)
cargo build --release --features bundled

# Start the Spark Connect server (default port 15002)
./target/release/thunderduck-connect-server
```

> **DuckDB linkage:** DuckDB is non-bundled by default, so a fresh clone builds with `--features bundled`
> (compiles DuckDB from source). Inside the devcontainer you can instead run
> `scripts/dev/dev-cache-setup.sh` once to link a shared prebuilt libduckdb — then `--features bundled`
> is no longer needed. See [`scripts/dev/README.md`](scripts/dev/README.md).

Connect with PySpark:

```python
from pyspark.sql import SparkSession

spark = SparkSession.builder \
    .remote("sc://localhost:15002") \
    .getOrCreate()

df = spark.read.parquet("my_data.parquet")
df.groupBy("category").agg({"amount": "sum"}).show()
```

## Architecture

Thunderduck uses a three-layer architecture:

```
┌─────────────────────────────────────────────────────────┐
│         Spark API Facade (DataFrame/Dataset)            │
│              Lazy Plan Construction                     │
└─────────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────┐
│                Translation Engine                       │
│   Logical Plan → DuckDB SQL Translation                 │
│   Expression Mapping, Type Conversion                   │
└─────────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────┐
│         DuckDB Execution Engine                         │
│   Vectorized Processing, SIMD Optimization              │
│   Arrow-Native Data Interchange                         │
└─────────────────────────────────────────────────────────┘
```

### Crate Structure

```
thunderduck-rs/
├── Cargo.toml                     # Workspace manifest
├── crates/
│   ├── core/                      # Pure translation engine (no gRPC)
│   │   ├── logical/               # LogicalPlan enum (29 variants, exhaustive match)
│   │   ├── expression/            # Expression enum (21+ variants)
│   │   ├── types/                 # DataType enum, StructType, TypeInferenceEngine
│   │   ├── generator/             # SqlGenerator (match-based visitor)
│   │   ├── functions/             # FunctionRegistry (500+ Spark→DuckDB mappings)
│   │   ├── parser/                # SparkSQL parser (sqlparser-rs + SparkDialect)
│   │   └── runtime/               # DuckDB session, Arrow streaming, extension loading
│   └── connect-server/            # gRPC binary (tonic)
│       ├── service/               # SparkConnectService (tonic gRPC handlers)
│       ├── session/               # SessionManager (DashMap + per-session OS threads)
│       └── converter/             # Protobuf → LogicalPlan (RelationConverter, ExpressionConverter)
├── extensions/vendored/            # thdck_spark_funcs binaries, all 4 platforms (checked into git;
│                                   #   embedded via include_bytes!; see MANIFEST.toml)
└── tests/
    ├── integration/               # Python differential tests
    │   ├── differential/          # Differential test suites (41 test files)
    │   └── sql/                   # TPC-H and TPC-DS SQL queries
    └── scripts/                   # Test runner scripts
```

### Core Components

- **Logical Plan** (`crates/core/logical/`): `LogicalPlan` enum with 29 variants — exhaustive `match` enforced at compile time
- **Expression System** (`crates/core/expression/`): `Expression` enum with 21+ variants; `to_sql()` for generation, `data_type()` for inference
- **SQL Generator** (`crates/core/generator/`): `SqlGenerator` — match-based visitor producing DuckDB SQL from the logical plan tree
- **Type Mapping** (`crates/core/types/`): `TypeInferenceEngine` resolves expression types following Spark semantics
- **Function Registry** (`crates/core/functions/`): 500+ Spark→DuckDB function mappings
- **SparkSQL Parser** (`crates/core/parser/`): sqlparser-rs with a custom `SparkDialect` for raw SQL queries
- **Runtime** (`crates/core/runtime/`): `DuckDbSession` owns a `duckdb::Connection` on a dedicated OS thread; Arrow streaming; extension loading
- **gRPC Server** (`crates/connect-server/`): tonic-based Spark Connect service with per-session thread model

**Note**: Thunderduck relies on **DuckDB's world-class query optimizer** rather than implementing custom optimization rules. DuckDB automatically performs filter pushdown, column pruning, join reordering, and many other optimizations.

### Threading Model

`duckdb::Connection` is `!Send + !Sync`. Each session runs on a dedicated `std::thread`. The gRPC async handler communicates via `tokio::sync::mpsc` channels:

```
tokio task → mpsc::Sender<SessionCommand> → session thread (owns Connection)
session thread → oneshot::Sender<SessionResult> → tokio task → gRPC stream
```

## Building from Source

### Prerequisites

- **Rust** 1.75+ (`rustup` recommended)
- **protoc** — Protocol Buffers compiler
- **curl** — required by `build.rs` to download the mandatory `thdck_spark_funcs` extension binary on first build

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install protoc (macOS)
brew install protobuf

# Install protoc (Ubuntu/Debian)
apt-get install -y protobuf-compiler
```

### Build

> DuckDB is non-bundled by default. The commands below need an external libduckdb: either run
> `scripts/dev/dev-cache-setup.sh` once (devcontainer — links a shared prebuilt lib), **or** append
> `--features bundled` to compile DuckDB from source (fresh clones / CI). `--features bundled` applies
> equally to `build`, `test`, `check`, and `clippy`.

```bash
# Full build (debug) — add `--features bundled` on a fresh clone / CI
cargo build

# Release build (required for integration/differential tests).
# Downloads and embeds the thdck_spark_funcs extension automatically.
cargo build --release

# Build a single crate
cargo build -p thunderduck-core
cargo build -p thunderduck-connect-server

# Check only (faster than build — no code generation)
cargo check
```

All 4 platform binaries of the adopted `thdck_spark_funcs` release (currently
the `ext6` set, `v1.5.4`, matching the `duckdb` crate at `1.10504.0`) are
vendored — checked into git plain under `extensions/vendored/` (see
`extensions/vendored/MANIFEST.toml`). `build.rs` picks the binary matching the
current platform at build time — no network access, no download. Adopting a
new release (only on `duckdb` crate bumps) is done via
`scripts/dev/adopt-extension-release.sh`. The extension is embedded directly
in the binary via `include_bytes!()` and loaded at every session's startup.

The extension's C++ source now lives in-tree at [`extension/`](extension/)
(imported from `nubank/thunderduck-duckdb-extension`, now archived — see
`extension/README.md`'s Provenance section and
`docs/context/extension-archival-checklist.md`). Local dev builds use
`scripts/dev/build-extension.sh`; producing new vendored binaries is a
`workflow_dispatch`-only CI job, `.github/workflows/extension-release.yml`.

### Start the Server

```bash
# Default
./target/release/thunderduck-connect-server

# Custom port
./target/release/thunderduck-connect-server --port 15002

# Kill the server (worktree-scoped — never touches other worktrees' servers)
./tests/scripts/kill-test-servers.sh
```

## Spark Compatibility Extension

Spark parity is the only emission target. The `thdck_spark_funcs` DuckDB extension is mandatory and bundled into every build (see [rearchitect ADR-020](docs/thunderduck-rearchitect-ADRs.md)). It implements Spark-precise numerical semantics:
- `spark_hash(c1, ..., cN)` — Spark `hash()` (Murmur3-32, signed INT, seed 42)
- `spark_xxhash64(c1, ..., cN)` — Spark `xxhash64()` (xxHash64, signed BIGINT, seed 42)
- `spark_decimal_div(a, b)` — decimal division with `ROUND_HALF_UP`
- `spark_sum(col)` — Spark-compatible SUM return types
- `spark_avg(col)` — Spark-compatible AVG return types
- `spark_skewness(col)` — population skewness (Spark's formula, no bias correction)

## Testing

Assumes the project is already built (see [Building from Source](#building-from-source)). **Always use a release build for differential tests** — test servers launch `./target/release/thunderduck-connect-server`.

### Unit Tests

> Same DuckDB linkage rule as Build: works as-is with the devcontainer prebuilt lib; on a fresh clone / CI
> add `--features bundled` (e.g. `cargo test --features bundled`).

```bash
# All unit tests
cargo test

# Single module
cargo test -p thunderduck-core -- types::

# Single test
cargo test -p thunderduck-core -- generator::tests::test_project_to_sql

# With stdout output
cargo test -- --nocapture
```

### Differential Tests

```bash
# Full suite (all 41 test files: TPC-H, TPC-DS, joins, window, aggregations, etc.)
./tests/scripts/run-differential-tests.sh all

# Quick check: TPC-H only
./tests/scripts/run-differential-tests.sh tpch
```

The run script handles virtualenv setup, server lifecycle, and cleanup automatically.

### Running Tests Directly with pytest

```bash
# Activate the virtualenv first
source tests/integration/.venv/bin/activate

# Full suite
cd tests/integration && python3 -m pytest differential/ -v --tb=short

# Single test file
cd tests/integration && python3 -m pytest differential/test_joins_differential.py -v --tb=long

# Single parameterized test (e.g., TPC-H Q7)
cd tests/integration && python3 -m pytest \
  "differential/test_differential_v2.py::TestTPCH_AllQueries_Differential[7]" -v --tb=long
```

### Key Test Data Paths

| Resource | Path |
|----------|------|
| TPC-H parquet data | `tests/integration/tpch_sf001/*.parquet` |
| TPC-H SQL queries | `tests/integration/sql/tpch_queries/q{1-22}.sql` |
| TPC-DS SQL queries | `tests/integration/sql/tpcds_queries/q{1-99}.sql` |
| Test conftest | `tests/integration/conftest.py` |
| DataFrame diff util | `tests/integration/utils/dataframe_diff.py` |

## Documentation

- **[Rearchitecture ADRs](docs/thunderduck-rearchitect-ADRs.md)**: Authoritative architecture for the transpiler redesign (ADR-000 → ADR-019)
- **[Architecture](docs/architecture.md)**: Architectural decisions for the existing implementation (ADRs 1–21)
- **[Dev Journal](docs/dev-journal-toc.md)**: Chronological development history

## Contributing

1. **Fork the repository** and create a feature branch
2. **Write tests** for new functionality
3. **Ensure unit tests pass**: `cargo test`
4. **Build release and run differential tests**: `cargo build --release && ./tests/scripts/run-differential-tests.sh all`
5. **Submit a pull request** with a clear description

## License

This project is licensed under the Apache License 2.0 - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- **DuckDB Team**: High-performance embedded database
- **Apache Arrow**: Zero-copy data interchange
- **Apache Spark**: API compatibility and testing reference
- **sqlparser-rs**: SQL parsing foundation for the SparkSQL parser
- **tonic**: gRPC framework for the Spark Connect server
