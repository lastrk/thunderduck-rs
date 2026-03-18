# Claude Code Project Rules

This file contains project-specific rules and guidelines for working with thunderduck-rs (the Rust port of Thunderduck).

## Project Vision

**Keep your Spark API, get single-node DuckDB performance.** Thunderduck is a drop-in Spark Connect server backed by DuckDB for workloads that don't need distributed compute. This is the Rust port: same Spark API compatibility, fast startup (~50ms vs ~10s JVM), and low memory overhead (~30MB vs ~500MB JVM baseline).

See [docs/architecture.md](docs/architecture.md) for all architectural decisions.

## Workflow Orchestration

### 1. Plan Mode Default
Enter plan mode for ANY non-trivial task (3+ steps or architectural decisions). If something goes sideways, STOP and re-plan immediately.

### 2. Subagent Strategy
Offload research, exploration, and parallel analysis to subagents. One task per subagent for focused execution.
- **Compile tasks**: Use a subagent for `cargo build`. Return success or the focused error message.
- **Test suites**: Use a subagent. Return: Total/Passed/Failed counts, list of failed tests with one-line error summaries, and the exact command used.

### 3. Self-Improvement Loop
After ANY correction from the user: update `tasks/lessons.md` with the pattern. Review lessons at session start.

### 4. Verification Before Done
Never mark a task complete without proving it works. Run tests, check logs, demonstrate correctness.

### 5. Demand Elegance (Balanced)
For non-trivial changes: pause and ask "is there a more elegant Rust way?" Skip for simple, obvious fixes.

### 6. Autonomous Bug Fixing
When given a bug report: just fix it. Point at logs, errors, failing tests, then resolve them.

## Task Management
**Plan First**: Write plan to `tasks/todo.md` with checkable items. **Track Progress**: Mark items complete as you go. **Capture Lessons**: Update `tasks/lessons.md` after corrections.

## Core Principles
**Simplicity First**: Make every change as simple as possible. Impact minimal code.
**No Laziness**: Find root causes. No temporary fixes. Senior Rust developer standards.
**Minimal Impact**: Changes should only touch what's necessary. Avoid introducing bugs.
**Idiomatic Rust**: Prefer enums over trait objects for closed sets. Prefer `match` over dynamic dispatch. Use `thiserror` for typed errors. No `unwrap()` in production paths.

## SQL Generation Architecture Principles

These are non-negotiable constraints governing all SQL generation and type handling:

1. **All SQL and expression snippets MUST be built from the typed AST.** No string manipulation on SQL text.
2. **Zero pre/post-processing of SQL strings.** All transformations happen on the AST.
3. **SparkSQL data flow**: Spark SQL string → sqlparser-rs parse tree → Thunderduck expression tree → `SqlGenerator::generate()` → SQL string for DuckDB.
4. **DataFrame data flow**: Spark Connect protobuf → Thunderduck expression tree → `SqlGenerator::generate()` → SQL string for DuckDB.
5. **Relaxed mode**: Best performance mapping to vanilla DuckDB constructs producing value-equivalent results (type equivalence not required).
6. **Strict mode**: Match Apache Spark exactly via (a) CASTs at top-level SELECT projection, or (b) DuckDB extension functions. No casts on intermediate values.
7. **Zero result copying**: Strict mode achieves 100% type matching at SQL generation time using extension functions + AS aliases. No Arrow vector copying or rewriting.
8. **`to_sql()` is for SQL generation only.** `Display` / `Debug` implementations are human-readable debug output only — never used to build SQL strings sent to DuckDB.

## Architecture Quick-Reference

See [docs/architecture.md](docs/architecture.md) for full decisions. Key summary:

### Crate Structure

```
crates/core/            # Pure translation engine (no gRPC)
  logical/              # LogicalPlan enum (29 variants, exhaustive match enforced)
  expression/           # Expression enum (21+ variants)
  types/                # DataType enum, StructType, TypeInferenceEngine
  generator/            # SqlGenerator (match-based visitor)
  functions/            # FunctionRegistry (500+ Spark→DuckDB mappings)
  parser/               # SparkSQL parser (sqlparser-rs + SparkDialect)
  runtime/              # DuckDB session, Arrow streaming, extension loading

crates/connect-server/  # gRPC binary (tonic)
  service/              # SparkConnectService (tonic)
  session/              # SessionManager (DashMap + per-session OS threads)
  converter/            # Protobuf → LogicalPlan (RelationConverter, ExpressionConverter)
```

### Key Types

| Layer | Type | Responsibility |
|-------|------|----------------|
| **Service** | `SparkConnectService` | tonic gRPC service: receives Spark Connect requests |
| **Session** | `SessionManager` | Manages sessions; each session owns a DuckDB Connection on a dedicated OS thread |
| **Converter** | `RelationConverter` | Spark Connect protobuf Relation → `LogicalPlan` |
| **Converter** | `ExpressionConverter` | Spark Connect protobuf Expression → `Expression` |
| **Parser** | `SparkSqlParser` | sqlparser-rs based Spark SQL parser (raw SQL path) |
| **Logical** | `LogicalPlan` (enum) | 29 variants — exhaustive match at compile time |
| **Expression** | `Expression` (enum) | 21+ variants — `to_sql()`, `data_type()`, `nullable()` |
| **Generator** | `SqlGenerator` | Traverses LogicalPlan tree, produces DuckDB SQL |
| **Runtime** | `DuckDbSession` | Owns `duckdb::Connection` on its dedicated OS thread |
| **Functions** | `FunctionRegistry` | Maps Spark function names → DuckDB equivalents |
| **Types** | `TypeInferenceEngine` | Resolves expression types following Spark semantics |

### CRITICAL: `to_sql()` vs `Display`

Expression rendering **MUST** use `to_sql()`, not `Display` / `Debug`. The `Display` implementation is for debug logging only. This is a recurring bug class in the Java reference; the Rust port must not repeat it.

### DuckDB Threading Model

`duckdb::Connection` is `!Send + !Sync`. Each session runs on a dedicated `std::thread`. The gRPC async handler communicates via `tokio::sync::mpsc` channels:

```
tokio task → mpsc::Sender<SessionCommand> → session thread (owns Connection)
session thread → oneshot::Sender<SessionResult> → tokio task → gRPC stream
```

### Dual SQL Generation Paths for Joins

When modifying join SQL generation, check BOTH paths:
- `visit_join()` — primary path, converts SEMI/ANTI to EXISTS subqueries
- `generate_flat_join_chain()` — optimised chain path, **breaks at SEMI/ANTI** (no EXISTS conversion)

Aggregate SQL generation uses a **single canonical path** through `visit_aggregate()`.

### Expression Hierarchy (key types)

```
Expression (enum)
  Literal               # constant values
  ColumnReference       # resolved column ref with type info and qualifier
  UnresolvedColumn      # unresolved (pre-resolution)
  Binary(BinaryExpression)   # left OP right
  Unary(UnaryExpression)     # OP operand
  FunctionCall          # func(args) — uses FunctionRegistry for translation
  Cast(CastExpression)  # CAST(expr AS type)
  CaseWhen              # CASE WHEN ... THEN ... END
  Window(WindowFunction)     # ROW_NUMBER, RANK, LAG, LEAD, etc.
  Alias(AliasExpression)     # expr AS alias
  Star                  # *
  InSubquery / ExistsSubquery / ScalarSubquery
  Lambda / LambdaVariable    # HOF lambdas (transform, filter, etc.)
  RawSql                # raw SQL passthrough from spark.expr()
  ArrayLiteral / MapLiteral / StructLiteral
```

## Known Gotchas

1. **`to_sql()` vs `Display`**: SQL rendering MUST use `to_sql()`. `Display` is debug-only.

2. **`duckdb::Connection` is `!Send`**: Never attempt to move a Connection across thread boundaries or hold it across `.await` points. Use the session thread model.

3. **Composite aggregate expressions**: When adding expression types that can appear inside aggregates, ensure `RelationConverter::convert_aggregate()` handles them. A default `_` arm silently drops unknown cases.

4. **Semi/Anti join in flat chains**: `generate_flat_join_chain()` does NOT convert semi/anti joins to EXISTS subqueries. Chain must break at semi/anti joins.

5. **DuckDB SEMI JOIN syntax**: DuckDB uses `SEMI JOIN` and `ANTI JOIN` (without `LEFT` prefix). `LEFT SEMI JOIN` is a parser error.

6. **Extension version pinning**: The `.duckdb_extension` binary DuckDB version must exactly match the `duckdb` crate version in `Cargo.toml`. Currently targeting DuckDB 1.5.0.

7. **HUGEINT overflow**: DuckDB `SUM()` of integer columns returns `HUGEINT` (i128). Spark returns `BIGINT` (i64). SQL generation must emit explicit `CAST(... AS BIGINT)` for integer SUM.

8. **Schema inference vs DESCRIBE**: Prefer `plan.infer_schema()` for schema analysis. Only fall back to issuing `DESCRIBE` queries to DuckDB when plan-level inference is impossible.

## Spark Parity Requirements

**Critical Rule**: Thunderduck must match Spark EXACTLY, not just produce equivalent results.

- **Return types**: If Spark returns DOUBLE, Thunderduck must return DOUBLE (not BIGINT)
- **Rounding conventions**: Must match Spark's rounding behavior (`ROUND_HALF_UP`)
- **Type coercion**: Implicit casts must follow Spark's rules
- **NULL handling**: Must match Spark's null propagation
- **COUNT**: Always non-nullable `Long`
- **SUM/AVG**: Nullability depends on argument nullability

Differential tests validate: same row count, same column names, **same column types**, same values (with epsilon for floats), same null handling, same sort order.

**Goal**: Drop-in replacement for Spark, not "Spark-like" behavior.

## Spark Compatibility Extension

Two modes: **Relaxed** (default, no extension, ~85% compat) and **Strict** (extension loaded, ~100% compat).

The `thdck_spark_funcs` DuckDB extension (from `thunderduck-duckdb-extension` repo, v1.5.0 branch) implements Spark-precise numerical semantics:
- `spark_decimal_div(a, b)` — decimal division with `ROUND_HALF_UP`
- `spark_sum(col)` — Spark-compatible SUM return types
- `spark_avg(col)` — Spark-compatible AVG return types

Extension binaries are bundled in `extensions/` and embedded in the binary via `include_bytes!()`. Platform-specific loading at startup.

> Full details: [docs/architecture.md#adr-13](docs/architecture.md)

## Documentation Structure

1. **Architecture** (`docs/architecture.md`) — all architectural decisions (ADRs)
2. **Implementation Plan** (`docs/implementation-plan.md`) — phased delivery plan
3. **Phase details** (`docs/phase*.md`) — detailed breakdown per phase
4. **Task tracking** (`tasks/`) — active work items and lessons learned

## Git Commit Workflow

**Critical Rule**: NEVER commit code without user review first. Show changes, wait for explicit approval, then commit.

## Development Cheatsheet

### Build

```bash
# Full build (debug)
cargo build

# Release build (for integration tests)
cargo build --release

# Build a single crate
cargo build -p thunderduck-core
cargo build -p thunderduck-connect-server

# Check (faster than build, no codegen)
cargo check
```

### Unit Tests

```bash
# All unit tests
cargo test

# Single module
cargo test -p thunderduck-core -- types::

# Single test
cargo test -p thunderduck-core -- generator::tests::test_project_to_sql

# With output
cargo test -- --nocapture
```

### Integration / Differential Tests

```bash
# Full differential test suite (all 41 test files)
./tests/scripts/run-differential-tests.sh all

# Quick check: TPC-H only
./tests/scripts/run-differential-tests.sh tpch

# Strict mode (requires extension)
THUNDERDUCK_COMPAT_MODE=strict ./tests/scripts/run-differential-tests.sh tpch

# Direct pytest (activate venv first)
cd tests/integration && python3 -m pytest differential/ -v --tb=short

# Single test
cd tests/integration && python3 -m pytest \
  "differential/test_differential_v2.py::TestTPCH_AllQueries_Differential[7]" -v --tb=long
```

### Server

```bash
# Start server (default port 15002)
./target/release/thunderduck-connect-server

# Custom port
./target/release/thunderduck-connect-server --port 15002

# Strict mode
./target/release/thunderduck-connect-server --strict

# Relaxed mode
./target/release/thunderduck-connect-server --relaxed

# Kill server
pkill -f thunderduck-connect-server
```

### Key Data & SQL Paths

| Resource | Path |
|----------|------|
| TPC-H parquet data | `tests/integration/tpch_sf001/*.parquet` |
| TPC-H SQL queries | `tests/integration/sql/tpch_queries/q{1-22}.sql` |
| TPC-DS SQL queries | `tests/integration/sql/tpcds_queries/q{1-99}.sql` |
| Test conftest | `tests/integration/conftest.py` |
| DataFrame diff util | `tests/integration/utils/dataframe_diff.py` |

**Last Updated**: 2026-03-18
