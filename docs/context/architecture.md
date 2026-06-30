# Architecture Reference

## SQL Generation Architecture Principles

These are non-negotiable constraints governing all SQL generation and type handling:

1. **All SQL and expression snippets MUST be built from the typed AST.** No string manipulation on SQL text.
2. **Zero pre/post-processing of SQL strings.** All transformations happen on the AST.
3. **SparkSQL data flow**: Spark SQL string → sqlparser-rs parse tree → Thunderduck expression tree → `SqlGenerator::generate()` → SQL string for DuckDB.
4. **DataFrame data flow**: Spark Connect protobuf → Thunderduck expression tree → `SqlGenerator::generate()` → SQL string for DuckDB.
5. **Spark parity is the only emission target.** τ matches Apache Spark exactly via (a) CASTs at top-level SELECT projection or (b) DuckDB extension functions; the `thdck_spark_funcs` extension is mandatory (rearchitect ADR-020).
6. **Zero result copying**: 100% type matching is achieved at SQL generation time using extension functions + AS aliases. No Arrow vector copying or rewriting.
7. **`to_sql()` is for SQL generation only.** `Display` / `Debug` implementations are human-readable debug output only — never used to build SQL strings sent to DuckDB.

## Crate Structure

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

## Key Types

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

## CRITICAL: `to_sql()` vs `Display`

Expression rendering **MUST** use `to_sql()`, not `Display` / `Debug`. The `Display` implementation is for debug logging only. This is a recurring bug class in the Java reference; the Rust port must not repeat it.

## DuckDB Threading Model

`duckdb::Connection` is `!Send + !Sync`. Each session runs on a dedicated `std::thread`. The gRPC async handler communicates via `tokio::sync::mpsc` channels:

```
tokio task → mpsc::Sender<SessionCommand> → session thread (owns Connection)
session thread → oneshot::Sender<SessionResult> → tokio task → gRPC stream
```

Never attempt to move a `Connection` across thread boundaries or hold it across `.await` points.

## Dual SQL Generation Paths for Joins

When modifying join SQL generation, check **both** paths:

- `gen_join()` — primary path, emits native DuckDB `SEMI JOIN` / `ANTI JOIN` directly (no EXISTS-subquery conversion).
- Flat-chain rendering inside `gen_join()` (the natural-flat-join branch) — must break at SEMI/ANTI to preserve the tree shape; the chain cannot fold across a semi/anti boundary.

Aggregate SQL generation uses a **single canonical path** through `gen_aggregate()`.

## Expression Hierarchy (key variants)

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

## Raw SQL vs DataFrame Code Paths

Both paths go through full logical planning:

- **Raw SQL**: Spark SQL string → `SparkSqlParser` (sqlparser-rs + SparkDialect) → LogicalPlan → `SqlGenerator::generate()` → DuckDB SQL
- **DataFrame API**: Spark Connect protobuf → `RelationConverter` / `ExpressionConverter` → LogicalPlan → `SqlGenerator::generate()` → DuckDB SQL

Both paths have full type awareness via `plan.infer_schema()` and `TypeInferenceEngine`.

**Implication**: Type inference and SQL rewriting fixes affect **both** raw SQL and DataFrame queries.

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
