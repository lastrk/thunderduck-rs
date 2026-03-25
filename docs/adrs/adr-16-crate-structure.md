# ADR-16: Crate Structure

**Decision: Cargo workspace with two crates**

```
thunderduck-rs/
├── Cargo.toml                  # workspace
├── crates/
│   ├── core/                   # Pure translation engine — no gRPC dependency
│   │   ├── src/
│   │   │   ├── logical/        # LogicalPlan enum + node structs
│   │   │   ├── expression/     # Expression enum + node structs
│   │   │   ├── types/          # DataType, StructType, TypeInferenceEngine
│   │   │   ├── generator/      # SqlGenerator
│   │   │   ├── functions/      # FunctionRegistry
│   │   │   └── runtime/        # DuckDB session, Arrow streaming, extension loading
│   │   └── Cargo.toml
│   └── connect-server/         # gRPC server binary
│       ├── src/
│       │   ├── main.rs
│       │   ├── service/        # tonic SparkConnectService implementation
│       │   ├── session/        # SessionManager
│       │   └── converter/      # Protobuf → LogicalPlan (RelationConverter, ExpressionConverter)
│       ├── build.rs            # tonic_build proto compilation
│       ├── proto/              # Spark Connect .proto files (copied from reference)
│       └── Cargo.toml
├── extensions/                 # Pre-built DuckDB extension binaries
│   ├── linux_amd64/thdck_spark_funcs.duckdb_extension
│   ├── linux_arm64/thdck_spark_funcs.duckdb_extension
│   ├── osx_amd64/thdck_spark_funcs.duckdb_extension
│   └── osx_arm64/thdck_spark_funcs.duckdb_extension
└── tests/
    └── integration/            # Python differential test suite (PySpark ↔ Thunderduck)
```

The `core` crate has no dependency on `tonic` or any network I/O library — it is independently testable with pure Rust unit tests.

---

← [Back to Architecture Overview](../architecture.md)
