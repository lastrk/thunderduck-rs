# Thunderduck Rust — Architecture Decisions

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
│  SparkSqlParser (sqlparser-rs)                      │
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

## ADR-01: gRPC Framework

**Decision: `tonic` + `prost`**

`tonic` is the canonical async-native gRPC library for Rust. `prost` handles protobuf codegen. The Spark Connect `.proto` files are copied verbatim from the Java reference implementation (`connect-server/src/main/proto/`) and compiled at build time via `tonic_build` in `build.rs`.

No alternative was seriously considered — tonic is the ecosystem standard.

---

## ADR-02: Async Runtime

**Decision: `tokio` (multi-thread scheduler)**

All gRPC I/O, session lifecycle, and result streaming run on tokio. DuckDB operations (inherently blocking) are isolated on dedicated OS threads and communicated with via channels (see ADR-05).

---

## ADR-03: DuckDB Bindings

**Decision: `duckdb` crate with `arrow` feature; drop to `libduckdb-sys` C FFI only if incremental streaming control proves necessary**

The `duckdb` crate provides idiomatic Rust bindings. Its `arrow` feature exposes `Connection::query_arrow()` which drives DuckDB's native Arrow C Data Interface export — the zero-copy path.

**Version pinning**: The DuckDB crate version in `Cargo.toml` **must** exactly match the compiled `thdck_spark_funcs.duckdb_extension` binary version. **Target: DuckDB 1.5.0** (aligned with the `thunderduck-duckdb-extension` v1.5.0 branch).

If fine-grained streaming control (batch-size, back-pressure) cannot be achieved through the high-level API, we drop down to `libduckdb-sys` and call `duckdb_query_arrow_array()` directly.

---

## ADR-04: Arrow Library

**Decision: `arrow` crate (apache/arrow-rs)**

The `duckdb` crate already depends on `arrow-rs`. Using the same library means DuckDB's Arrow export flows directly into tonic response serialization without a conversion step.

- `arrow::record_batch::RecordBatch` — batch type throughout the pipeline
- `arrow_ipc::writer::StreamWriter` — serializes batches to Arrow IPC for gRPC wire encoding
- `arrow::ffi` — used if we drop to C FFI for DuckDB streaming

`arrow2` is not used: it is less actively maintained and incompatible with `duckdb-rs`.

---

## ADR-05: DuckDB Threading Model

**Decision: Dedicated OS thread per session with `tokio::sync::mpsc` channel communication**

`duckdb::Connection` is `!Send + !Sync`. It cannot be moved across thread boundaries or held across `.await` points in async code. The solution:

```
tokio async task (gRPC handler)
    │  sends QueryRequest via mpsc::Sender<SessionCommand>
    ▼
Session thread  (std::thread, owns Connection for its lifetime)
    │  executes query, collects Arrow batches
    │  sends results back via oneshot::Sender<SessionResult>
    ▼
tokio async task
    │  streams Arrow batches over gRPC response
```

Properties of this design:
- Each session's `Connection` is created on the session thread and never leaves it — fully safe.
- DuckDB uses its own internal thread pool for query parallelism; the session thread is just a dispatcher.
- Execution serialization per session (one query at a time) is the natural consequence of a single-receiver channel.
- Session teardown is clean: dropping the `mpsc::Sender` causes the session thread to exit its receive loop and drop the `Connection`.

**Rejected alternative**: `tokio::task::spawn_blocking` — `Connection` is `!Send`, so it cannot be moved into the closure.

---

## ADR-06: Logical Plan Representation

**Decision: Rust `enum` — one variant per plan node**

Rust enums are sealed by definition. A `match` on a non-exhaustive enum is a **compile error**. This is strictly stronger than Java's sealed classes which require explicit `permits` and can still fall through to a default case.

```rust
pub enum LogicalPlan {
    Project(Project),
    Filter(Filter),
    Aggregate(Aggregate),
    Join(Join),
    Sort(Sort),
    Limit(Limit),
    Tail(Tail),
    Union(Union),
    Except(Except),
    Intersect(Intersect),
    Distinct(Distinct),
    Sample(Sample),
    TableScan(TableScan),
    SqlRelation(SqlRelation),
    LocalRelation(LocalRelation),
    LocalDataRelation(LocalDataRelation),
    RangeRelation(RangeRelation),
    InMemoryRelation(InMemoryRelation),
    WithCte(WithCte),
    WithColumns(WithColumns),
    AliasedRelation(AliasedRelation),
    RawDdlStatement(RawDdlStatement),
    ToDataFrame(ToDataFrame),
}
```

Each variant wraps a struct carrying the node's fields. `SqlGenerator` is a set of `match` arms — adding a new variant without handling it is a compile error.

---

## ADR-07: Expression System

**Decision: Rust `enum` (not `Box<dyn Trait>`)**

The expression set is closed (all types known at compile time). Enum variants are zero-allocation, exhaustively matchable, and avoid vtable dispatch overhead.

```rust
pub enum Expression {
    Literal(Literal),
    ColumnReference(ColumnReference),
    UnresolvedColumn(UnresolvedColumn),
    Binary(BinaryExpression),
    Unary(UnaryExpression),
    FunctionCall(FunctionCall),
    Cast(CastExpression),
    CaseWhen(CaseWhenExpression),
    Window(WindowFunction),
    Alias(AliasExpression),
    Star,
    InSubquery(InSubquery),
    ExistsSubquery(ExistsSubquery),
    ScalarSubquery(ScalarSubquery),
    Lambda(LambdaExpression),
    LambdaVariable(LambdaVariableExpression),
    RawSql(RawSqlExpression),
    ArrayLiteral(ArrayLiteralExpression),
    MapLiteral(MapLiteralExpression),
    StructLiteral(StructLiteralExpression),
    Between(BetweenExpression),
}
```

Key methods (implemented via `match`):

| Method | Purpose |
|--------|---------|
| `to_sql(&self) -> String` | Generates SQL text for DuckDB. **Never** implement via `Display` or `Debug` — those are for humans. |
| `data_type(&self, schema: &StructType) -> DataType` | Type inference |
| `nullable(&self) -> bool` | Null propagation |

**Rejected alternative**: `Box<dyn ExpressionTrait>` — heap allocation per node, no exhaustiveness, no benefit for a closed set.

---

## ADR-08: Type System

**Decision: `DataType` enum mirroring Spark's type hierarchy**

```rust
pub enum DataType {
    Boolean,
    Byte,
    Short,
    Integer,
    Long,
    Float,
    Double,
    Decimal { precision: u8, scale: u8 },
    String,
    Binary,
    Date,
    Timestamp,
    TimestampNtz,
    YearMonthInterval,
    DayTimeInterval,
    Array(Box<DataType>),
    Map { key: Box<DataType>, value: Box<DataType>, value_nullable: bool },
    Struct(StructType),
    Null,
    Unresolved,
}

pub struct StructType {
    pub fields: Vec<StructField>,
}

pub struct StructField {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
}
```

`TypeInferenceEngine` centralises all type promotion rules (e.g., `Integer + Double → Double`, `SUM(Integer) → Long`, `COUNT → Long non-nullable`) following Spark semantics exactly.

---

## ADR-09: SQL Generator

**Decision: `SqlGenerator` struct with `match`-based dispatch**

```rust
pub struct SqlGenerator {
    alias_counter: u32,
    subquery_depth: u32,
}

impl SqlGenerator {
    pub fn generate(&mut self, plan: &LogicalPlan) -> String {
        match plan {
            LogicalPlan::Project(p)    => self.visit_project(p),
            LogicalPlan::Filter(f)     => self.visit_filter(f),
            LogicalPlan::Aggregate(a)  => self.visit_aggregate(a),
            LogicalPlan::Join(j)       => self.visit_join(j),
            // ... exhaustive — compiler enforces completeness
        }
    }
}
```

**Join dual-path rule (inherited from the Java reference)**:
- `visit_join()`: primary path, converts SEMI/ANTI to `EXISTS` subqueries.
- `generate_flat_join_chain()`: optimised flat chain path, **must break at SEMI/ANTI joins** (does not do EXISTS conversion).
- When modifying join SQL generation, **always check both paths**.

**Aggregate path**: single canonical path through `visit_aggregate()` — no dual-path issue.

---

## ADR-10: SparkSQL Parser (Raw SQL Path)

**Decision: `sqlparser-rs` as the foundation, with a custom `SparkDialect`; extend incrementally as gaps surface**

The DataFrame API path (protobuf → LogicalPlan) does **not** use a SQL parser. The parser is only needed for raw SQL strings passed via `spark.sql("...")`.

`sqlparser-rs` handles the majority of ANSI SQL. A custom `SparkDialect` will cover Spark-specific syntax: `TABLESAMPLE`, `LATERAL VIEW EXPLODE`, `DISTRIBUTE BY`, `CLUSTER BY`, `TRANSFORM`, lambda expressions in HOFs, etc.

Gaps are tracked as issues and filled as differential tests reveal them. A full port of Spark's ANTLR4 grammar (via `antlr4rust`) remains an option for later phases if sqlparser-rs gaps prove too numerous.

---

## ADR-11: Protobuf Plan Conversion

**Decision: Two-module converter mirroring the Java `RelationConverter` + `ExpressionConverter`**

- `relation_converter.rs` — converts prost-generated Spark Connect `Relation` to `LogicalPlan`
- `expression_converter.rs` — converts prost-generated `Expression` to our `Expression` enum
- `plan_converter.rs` — entry point, orchestrates both

Input: prost-generated types from Spark Connect protos.
Output: our typed `LogicalPlan` / `Expression` trees.

---

## ADR-12: Function Registry

**Decision: `LazyLock<FunctionRegistry>` with direct mappings and custom translators**

```rust
static FUNCTION_REGISTRY: LazyLock<FunctionRegistry> = LazyLock::new(FunctionRegistry::new);

pub struct FunctionRegistry {
    direct: HashMap<&'static str, &'static str>,
    custom: HashMap<&'static str, fn(&[&str]) -> String>,
}
```

500+ Spark → DuckDB function mappings ported from the Java reference. The registry is mode-aware: in strict mode, calls like `round()` and `avg()` on Decimals route through `thdck_spark_funcs` extension functions instead of vanilla DuckDB.

---

## ADR-13: DuckDB Extension Loading

**Decision: Embed platform-specific extension binaries in the Rust binary; extract to a temp file and `LOAD` at runtime**

```rust
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const EXTENSION: &[u8] = include_bytes!("../extensions/linux_amd64/thdck_spark_funcs.duckdb_extension");
```

The extension is the `thdck_spark_funcs` DuckDB extension from the `thunderduck-duckdb-extension` repository (v1.5.0 branch). It is a C/C++ DuckDB extension — platform-independent from the Rust host's perspective, compiled separately and bundled as bytes.

Platforms supported: `linux_amd64`, `linux_arm64`, `osx_amd64`, `osx_arm64`.

If no extension is bundled for the current platform, the server starts in relaxed mode with a log warning.

**Critical**: The extension binary DuckDB version must exactly match the `duckdb` crate's linked library version (1.5.0). DuckDB enforces this at `LOAD` time.

---

## ADR-14: Session Management

**Decision: `DashMap<String, Arc<SessionHandle>>` for concurrent access; one OS thread per session**

```rust
pub struct SessionManager {
    sessions: DashMap<String, Arc<SessionHandle>>,
}

pub struct SessionHandle {
    session_id: String,
    /// Channel to the session's dedicated OS thread
    cmd_tx: mpsc::Sender<SessionCommand>,
    /// Cached view schemas (written when temp views are created)
    view_schemas: Arc<RwLock<HashMap<String, StructType>>>,
}
```

Session isolation: each session creates a named in-memory DuckDB database (`duckdb:///:memory:<session_id_sanitised>`), ensuring temp views and state don't bleed between sessions.

Session replacement (idle session replaced by a new client with a different session ID) is handled by dropping the old `SessionHandle`, which closes the `mpsc::Sender`, causing the session thread to exit.

---

## ADR-15: Compatibility Modes

**Decision: Mirror the Java strict/relaxed/auto model**

```rust
pub enum CompatMode { Strict, Relaxed, Auto }
```

- **Relaxed** (default): vanilla DuckDB functions, ~85% Spark parity, no extension required.
- **Strict**: `thdck_spark_funcs` extension loaded, exact Spark numeric semantics, ~100% parity.
- **Auto**: strict if extension available, relaxed otherwise.

CLI flags: `--strict`, `--relaxed`. Environment variable: `THUNDERDUCK_COMPAT_MODE=strict|relaxed|auto`.

---

## ADR-16: Crate Structure

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
│   │   │   ├── parser/         # SparkSQL parser (sqlparser-rs based)
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

## ADR-17: Arrow ↔ DuckDB Zero-Copy Exchange

**The performance-critical path.**

```
DuckDB query execution
    ↓  Arrow C Data Interface (zero-copy)
arrow::record_batch::RecordBatch
    ↓  arrow_ipc::writer::StreamWriter
Arrow IPC bytes
    ↓  tonic streaming response
PySpark client
```

DuckDB exports Arrow natively via `Connection::query_arrow()` (duckdb-rs high-level API) or `duckdb_query_arrow_array()` (C FFI). The resulting `RecordBatch` objects are serialised to Arrow IPC format and streamed over gRPC. No data is copied between the DuckDB export and the wire.

Default batch size: 8192 rows. Configurable via `THUNDERDUCK_BATCH_SIZE` env var.

---

## ADR-18: Error Handling

**Decision: `thiserror` in `core`, `anyhow` in `connect-server`**

```rust
// core/src/error.rs
#[derive(thiserror::Error, Debug)]
pub enum ThunderduckError {
    #[error("SQL generation failed: {0}")]
    SqlGeneration(String),
    #[error("Type inference error: {0}")]
    TypeInference(String),
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
    #[error("DuckDB error: {0}")]
    DuckDb(#[from] duckdb::Error),
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("Parse error: {0}")]
    Parse(String),
}
```

`ThunderduckError` maps to `tonic::Status` in the gRPC service layer:
- `Unsupported` → `Status::unimplemented`
- `SqlGeneration` / `TypeInference` → `Status::internal`
- `DuckDb` → `Status::internal`

---

## ADR-19: SQL Generation Correctness Rules (Non-Negotiable)

These constraints are inherited from the Java reference and are architecture-level invariants:

1. **All SQL and expression snippets must be built from the typed AST.** No string manipulation on SQL text outside of `to_sql()` implementations.
2. **Zero pre/post-processing of SQL strings.** All transformations happen on the AST nodes.
3. **`to_sql()` is for SQL generation only.** `Display` / `Debug` implementations are for human-readable debug output — never used to build SQL strings sent to DuckDB.
4. **Sealed plan + expression enums enforce exhaustiveness.** All new node types must be handled in `SqlGenerator` — the compiler enforces this.
5. **Type inference is centralised in `TypeInferenceEngine`.** No ad-hoc type guessing scattered through converters.

---

## ADR-20: Testing Strategy

**Unit tests**: Rust `#[test]` in each module — type inference rules, SQL generation for each plan node and expression type, function registry mappings.

**Differential tests**: The Python pytest framework from the Java reference is imported unchanged into `tests/integration/`. The `server_manager.py` is adapted to launch the Rust binary (`target/release/thunderduck-connect-server`) instead of the Java JAR. All 746 differential tests (TPC-H, TPC-DS, joins, aggregations, window functions, etc.) run against the Rust server without modification.

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
| SQL parser | ANTLR4 (Spark grammar) | sqlparser-rs + SparkDialect |
