# Crate Structure

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). Active ADR index: [`../README.md`](../README.md).

**Decision: Cargo workspace with two crates**

```
thunderduck-rs/
├── Cargo.toml                  # workspace
├── crates/
│   ├── core/                   # Pure translation engine — no gRPC dependency
│   │   ├── src/
│   │   │   ├── transpiler_v2/  # τ: CommonAst, analyzer, emission, INV enforcement
│   │   │   ├── parser_v2/      # SparkSQL parser → CommonAst
│   │   │   ├── types/          # DataType, StructType, StructField
│   │   │   └── runtime/        # DuckDB session, Arrow streaming, extension loading
│   │   └── Cargo.toml
│   └── connect-server/         # gRPC server binary
│       ├── src/
│       │   ├── main.rs
│       │   ├── service.rs      # tonic SparkConnectService implementation
│       │   ├── session/        # SessionManager
│       │   ├── converter/      # Protobuf → CommonAst (V2RelationConverter, V2ExpressionConverter)
│       │   └── arrow_schema_stamp.rs   # Wire schema = τ's resolved_schema
│       ├── build.rs            # tonic_build proto compilation
│       ├── proto/              # Spark Connect .proto files (copied from reference)
│       └── Cargo.toml
├── extensions/vendored/         # thdck_spark_funcs binaries (all 4 platforms) — checked into git,
│                                #   adopted via scripts/dev/adopt-extension-release.sh
└── tests/
    └── integration/            # Python differential test suite (PySpark ↔ Thunderduck)
```

The `core` crate has no dependency on `tonic` or any network I/O library — it is independently testable with pure Rust unit tests.

Per [ADR-021](../adr-021-tau-substrate.md), τ is *substrate-independent*: it owns its `Expression` enum and `TypeInferenceEngine`, sharing only value-level types (`DataType` / `StructType` / `StructField`). Dispatch happens at the protobuf boundary per ADR-022 (τ is the only path — no fallback, no dispatch flag).

---

← [Back to ADR Index](../README.md)
