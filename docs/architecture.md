# Thunderduck Rust — Architecture Decisions

> **Superseded where it conflicts.** This document is the system overview for the **existing** implementation; its individual decisions now live as topic files under [`adrs/runtime/`](adrs/runtime/) and [`adrs/legacy-transpiler/`](adrs/legacy-transpiler/), indexed by [`adrs/README.md`](adrs/README.md). The authoritative architecture for the transpiler going forward is [`thunderduck-rearchitect-ADRs.md`](thunderduck-rearchitect-ADRs.md) (ADR-000 → ADR-021 + Cross-Validation). The decisions below remain valid reference for the current code, which continues to run behind the `--transpiler` dispatch flag while the rearchitected path is built alongside it; on any contradiction, the rearchitecture ADRs win.

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
│  + transpiler_v2 (v2 path, behind --transpiler flag)│
└─────────────────────┬───────────────────────────────┘
                      │  DuckDB SQL string
                      ▼
┌─────────────────────────────────────────────────────┐
│        DuckDB Execution + Arrow Streaming           │
│  duckdb-rs  →  Arrow RecordBatch (zero-copy)        │
│  thdck_spark_funcs extension (mandatory)            │
└─────────────────────────────────────────────────────┘
```

---

## Architecture Decision Records

The full ADR index and an agent-context router live in **[`adrs/README.md`](adrs/README.md)**. The existing-implementation decisions are grouped below; the authoritative v2 decisions are in [`thunderduck-rearchitect-ADRs.md`](thunderduck-rearchitect-ADRs.md).

### Runtime & serving substrate ([`adrs/runtime/`](adrs/runtime/))

Apply to both transpiler paths; not superseded by the rearchitecture.

| Decision | Summary |
|---|---|
| [gRPC Framework](adrs/runtime/grpc-framework.md) | `tonic` + `prost` |
| [Async Runtime](adrs/runtime/async-runtime.md) | `tokio` multi-thread scheduler |
| [DuckDB Bindings](adrs/runtime/duckdb-bindings.md) | `duckdb` crate (`arrow` feature); pinned to the `ext6` extension binary |
| [Arrow Library](adrs/runtime/arrow-library.md) | `arrow` crate (apache/arrow-rs); same dep as duckdb-rs |
| [DuckDB Threading Model](adrs/runtime/threading-model.md) | Dedicated OS thread per session; `mpsc` channel to async handler |
| [Session Management](adrs/runtime/session-management.md) | `DashMap<String, Arc<SessionHandle>>`; named in-memory DuckDB per session |
| [Crate Structure](adrs/runtime/crate-structure.md) | Workspace: `core` (translation) + `connect-server` (gRPC); `transpiler_v2` module |
| [Arrow ↔ DuckDB Zero-Copy](adrs/runtime/arrow-duckdb-zero-copy.md) | `query_arrow()` → Arrow IPC → tonic streaming; no copies on hot path |
| [DuckDB Extension Loading](adrs/runtime/extension-loading.md) | Bundle `thdck_spark_funcs` via `include_bytes!`; `LOAD` per session (mandatory) |
| [Error Handling](adrs/runtime/error-handling.md) | `thiserror` in `core`; `anyhow` in `connect-server`; maps to `tonic::Status` |

### Legacy transpiler ([`adrs/legacy-transpiler/`](adrs/legacy-transpiler/)) — runs behind `--transpiler legacy`

Superseded where it conflicts with the rearchitecture; both paths coexist.

| Decision | Summary | v2 successor |
|---|---|---|
| [Logical Plan](adrs/legacy-transpiler/logical-plan.md) | Rust `enum`, compiler-exhaustive `match` | ADR-003 / ADR-021 |
| [Expression System](adrs/legacy-transpiler/expression-system.md) | Rust `enum` (not `Box<dyn Trait>`) | ADR-003 / ADR-021 |
| [Type System](adrs/legacy-transpiler/type-system.md) | `DataType` enum + `TypeInferenceEngine` | ADR-005 / ADR-006 (`DataType` shared) |
| [SQL Generator](adrs/legacy-transpiler/sql-generator.md) | `match`-based dispatch; `gen_*`; dual join path | ADR-009 / ADR-007 |
| [Protobuf Plan Conversion](adrs/legacy-transpiler/plan-converter.md) | `RelationConverter` + `ExpressionConverter` | ADR-004 / ADR-021 |
| [Function Registry](adrs/legacy-transpiler/function-registry.md) | 500+ Spark→DuckDB mappings | ADR-009 / ADR-010 |
| [Correctness Rules](adrs/legacy-transpiler/correctness-rules.md) | 5 SQL-generation invariants (still current) | §CV INV1–INV10 |
| [Testing Strategy](adrs/legacy-transpiler/testing-strategy.md) | Unit + Python differential tests | ADR-014 / ADR-015 |
| [SparkSQL Parser Strategy](adrs/legacy-transpiler/sparksql-parser.md) | `sqlparser-rs` + `SparkDialect` (T1); `chumsky` (T2) | complements ADR-004 |

> **Removed (superseded):** the former *SparkSQL Raw SQL Path* (`preprocess_spark_sql` text rewrite → rearchitect ADR-004) and *Compatibility Modes* (Strict/Relaxed/Auto → rearchitect ADR-020). See [`adrs/README.md`](adrs/README.md#superseded--removed).

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
| SQL parser | ANTLR4 (Spark grammar) | `sqlparser-rs` + custom `SparkDialect`; raw SQL parsed to the common AST (rearchitect ADR-004) |
