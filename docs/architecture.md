# Thunderduck Rust — Architecture Decisions

> **Superseded where it conflicts.** This document records the ADRs (01–21) of the **existing** implementation. The authoritative architecture for the transpiler going forward is [`thunderduck-rearchitect-ADRs.md`](thunderduck-rearchitect-ADRs.md) (ADR-000 → ADR-019). The decisions below remain valid reference for the current code, which continues to run behind a dispatch flag while the rearchitected path is built alongside it; on any contradiction, the rearchitecture ADRs win.

**Thunderduck** is a Rust-native Spark Connect server that translates Spark DataFrame/SQL operations to DuckDB SQL and streams Arrow results back to clients. Goals: identical Spark API compatibility as the Java reference, plus fast startup and minimal memory footprint by eliminating the JVM.

---

## System Overview

```
PySpark / Spark Client
        │  (Spark Connect protobuf over gRPC / HTTP2)
        ▼
┌─────────────────────────────────────────────────────┐
│          connect-server crate (tonic gRPC)          │
│  SparkConnectService → SessionManager               │
│  RelationConverter + ExpressionConverter            │
└─────────────────────┬───────────────────────────────┘
                      │  Thunderduck LogicalPlan / Expression enums
                      ▼
┌─────────────────────────────────────────────────────┐
│                 core crate                          │
│  SqlGenerator  FunctionRegistry  TypeInferenceEngine│
│  preprocess_spark_sql (Spark→DuckDB dialect rewrite)│
└─────────────────────┬───────────────────────────────┘
                      │  DuckDB SQL string
                      ▼
┌─────────────────────────────────────────────────────┐
│        DuckDB Execution + Arrow Streaming           │
│  duckdb-rs  →  Arrow RecordBatch (zero-copy)        │
│  thdck_spark_funcs extension (strict mode)          │
└─────────────────────────────────────────────────────┘
```

---

## Architecture Decision Records

All architectural decisions are documented as individual ADRs. Each entry below links to the full decision document with context, options examined, and rationale.

| ADR | Title | Decision Summary | File |
|-----|-------|-----------------|------|
| ADR-01 | gRPC Framework | `tonic` + `prost` | [adr-01](adrs/adr-01-grpc-framework.md) |
| ADR-02 | Async Runtime | `tokio` multi-thread scheduler | [adr-02](adrs/adr-02-async-runtime.md) |
| ADR-03 | DuckDB Bindings | `duckdb` crate with `arrow` feature; version-pinned to 1.5.0 | [adr-03](adrs/adr-03-duckdb-bindings.md) |
| ADR-04 | Arrow Library | `arrow` crate (apache/arrow-rs); same dep as duckdb-rs | [adr-04](adrs/adr-04-arrow-library.md) |
| ADR-05 | DuckDB Threading Model | Dedicated OS thread per session; `mpsc` channel to async handler | [adr-05](adrs/adr-05-duckdb-threading-model.md) |
| ADR-06 | Logical Plan Representation | Rust `enum` — 36 variants, exhaustive `match` enforced at compile time | [adr-06](adrs/adr-06-logical-plan-representation.md) |
| ADR-07 | Expression System | Rust `enum` (not `Box<dyn Trait>`) — zero-allocation, exhaustively matchable | [adr-07](adrs/adr-07-expression-system.md) |
| ADR-08 | Type System | `DataType` enum mirroring Spark's type hierarchy; `TypeInferenceEngine` centralises promotions | [adr-08](adrs/adr-08-type-system.md) |
| ADR-09 | SQL Generator | `SqlGenerator` struct with `match`-based dispatch; `gen_*` naming; dual join path rule | [adr-09](adrs/adr-09-sql-generator.md) |
| ADR-10 | SparkSQL Raw SQL Path | ~~`preprocess_spark_sql` 13-phase text rewrite~~ — **superseded by ADR-21** | [adr-10](adrs/adr-10-sparksql-raw-sql-path.md) |
| ADR-11 | Protobuf Plan Conversion | Two-module converter: `RelationConverter` + `ExpressionConverter` | [adr-11](adrs/adr-11-protobuf-plan-conversion.md) |
| ADR-12 | Function Registry | `LazyLock<FunctionRegistry>` with 500+ Spark→DuckDB mappings; strict/relaxed routing | [adr-12](adrs/adr-12-function-registry.md) |
| ADR-13 | DuckDB Extension Loading | Embed platform binaries via `include_bytes!`; extract to temp file and `LOAD` at runtime | [adr-13](adrs/adr-13-duckdb-extension-loading.md) |
| ADR-14 | Session Management | `DashMap<String, Arc<SessionHandle>>`; named in-memory DuckDB databases per session | [adr-14](adrs/adr-14-session-management.md) |
| ADR-15 | Compatibility Modes | `CompatMode` enum: Strict / Relaxed / Auto; CLI flags and env var | [adr-15](adrs/adr-15-compatibility-modes.md) |
| ADR-16 | Crate Structure | Cargo workspace: `core` (pure translation) + `connect-server` (gRPC binary) | [adr-16](adrs/adr-16-crate-structure.md) |
| ADR-17 | Arrow ↔ DuckDB Zero-Copy Exchange | `query_arrow()` → Arrow IPC → tonic streaming; no data copies on hot path | [adr-17](adrs/adr-17-arrow-duckdb-zero-copy-exchange.md) |
| ADR-18 | Error Handling | `thiserror` in `core`; `anyhow` in `connect-server`; maps to `tonic::Status` | [adr-18](adrs/adr-18-error-handling.md) |
| ADR-19 | SQL Generation Correctness Rules | 5 non-negotiable invariants: AST-only SQL, no post-processing, `to_sql()` not Display, sealed enums, centralised type inference | [adr-19](adrs/adr-19-sql-generation-correctness-rules.md) |
| ADR-20 | Testing Strategy | Rust unit tests per module + Python differential tests (670 passing) against release binary | [adr-20](adrs/adr-20-testing-strategy.md) |
| ADR-21 | SparkSQL Parser Strategy | `sqlparser-rs` + custom `SparkDialect` (Tier 1); `chumsky` upgrade path (Tier 2); demand-driven coverage | [adr-21](adrs/adr-21-sparksql-parser-strategy.md) |

---

## Key Differences from the Java Reference

| Aspect | Java Reference | Rust Port |
|--------|---------------|-----------|
| Sealed types | `sealed class` + `permits` | `enum` — compiler-enforced exhaustiveness |
| Expression set | Interface + 30+ final classes | Single `enum` — zero allocation, no vtable |
| Threading | JVM threads, synchronized | OS threads + tokio; explicit `!Send` safety |
| Startup time | ~10s (JVM warmup) | ~50ms |
| Memory baseline | ~500MB (JVM heap + metaspace) | ~30MB |
| GC pauses | Yes (G1GC configured) | None |
| Arrow JVM flags | `--add-opens` required | Not needed |
| SQL parser | ANTLR4 (Spark grammar) | Preprocessing pass (see ADR-10); full parser deferred |
