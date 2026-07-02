# Crate Structure

> **Status: current — runtime/serving substrate.** An existing decision that applies to *both* transpiler paths (legacy and v2); not superseded by the rearchitecture. ADR index: [`../README.md`](../README.md) · v2 spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

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
│   │   │   ├── runtime/        # DuckDB session, Arrow streaming, extension loading
│   │   │   └── transpiler_v2/  # v2 rearchitecture path (behind --transpiler v2): CommonAst, analyzer, emission table
│   │   └── Cargo.toml
│   └── connect-server/         # gRPC server binary
│       ├── src/
│       │   ├── main.rs
│       │   ├── service/        # tonic SparkConnectService implementation
│       │   ├── session/        # SessionManager
│       │   └── converter/      # Protobuf → plan: RelationConverter/ExpressionConverter (legacy);
│       │                       #   v2_relation_converter.rs → v2 CommonAst directly (ADR-021)
│       ├── build.rs            # tonic_build proto compilation
│       ├── proto/              # Spark Connect .proto files (copied from reference)
│       └── Cargo.toml
├── extensions/ext6/            # thdck_spark_funcs binary — downloaded + cached by build.rs (gitignored)
└── tests/
    └── integration/            # Python differential test suite (PySpark ↔ Thunderduck)
```

The `core` crate has no dependency on `tonic` or any network I/O library — it is independently testable with pure Rust unit tests.

The `transpiler_v2/` module holds the rearchitected path. Per [rearchitect ADR-021](../../thunderduck-rearchitect-ADRs.md) it is *substrate-independent*: it shares only value-level types (`DataType` / `StructType` / `StructField`) with the legacy modules, carries its own `Expression` enum and `TypeInferenceEngine`, and is selected at the protobuf boundary by `--transpiler v2` (default is the legacy path).

---

← [Back to ADR Index](../README.md)
