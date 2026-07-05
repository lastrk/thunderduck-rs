# Claude Code Project Rules

This file contains project-specific rules and guidelines for working with thunderduck-rs (the Rust port of Thunderduck).

## Project Vision

**Keep your Spark API, get single-node DuckDB performance.** Thunderduck is a drop-in Spark Connect server backed by DuckDB for workloads that don't need distributed compute. This is the Rust port: same Spark API compatibility, fast startup (~50ms vs ~10s JVM), and low memory overhead (~30MB vs ~500MB JVM baseline).

### Authoritative architecture

[**docs/thunderduck-rearchitect-ADRs.md**](docs/thunderduck-rearchitect-ADRs.md) (ADR-000 → ADR-022 + Cross-Validation) is the **authoritative** architecture source for the transpiler. It records the design of the Spark → DuckDB transliterator (`τ`): a common AST fed by both front-ends, an owned type/nullability analyzer over that AST, a declarative compiled emission table, extension functions as minimal gap-fillers, and a differential + AnalyzePlan test architecture.

Older ADRs under [docs/adrs/legacy-transpiler/](docs/adrs/legacy-transpiler/) are marked **SUPERSEDED** — they describe the retired v1 transpiler stack and are kept only for historical reference.

### τ is the only path (per ADR-022)

τ (the transpiler at `crates/core/src/transpiler_v2/`, `crates/connect-server/src/converter/v2_relation_converter.rs`, and `crates/core/src/parser_v2/`) is the only production path. Every Spark Connect request flows to τ; τ's output is the response. If τ has not implemented a given operator, τ produces a Thunderduck-boundary error (`Unsupported*`) directly to the caller. There is no fallback, no dispatch flag, no alternate implementation.

**Two error categories** (ADR-022): (1) **Spark-emulated errors** — inputs Spark itself would reject; τ matches Spark's error semantics. (2) **Thunderduck-boundary errors** — inputs Spark accepts but τ has not implemented; honest "not implemented in Thunderduck."

**Practical implications:**
- The DataFrame corpus (`tests/scripts/v2-progress.sh`, 324 cases) is the fitness function; TPC-H is temporarily red until τ covers its query surface.
- The SQL corpus (`differential/sql_corpus.py`, 262 `spark.sql` cases) is the fitness function for the τ SQL front-end — run it with `./tests/scripts/run-differential-tests.sh sql_v2` (or `tests/scripts/v2-sql-progress.sh` to record a progress row in `tests/integration/v2_sql_progress.md`).
- The v1 transpiler modules (`crates/core/src/{logical,expression,generator,functions,parser}/`) were deleted on 2026-07-05. INV3/INV10 in `crates/core/src/transpiler_v2/invariants.rs` mechanically enforce that τ does not import from those (now-absent) prefixes.

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
Never mark a task complete without proving it works. For any non-trivial change, run the following in order and require each to pass before moving on. Commands live in the [Development Cheatsheet](#development-cheatsheet); do not duplicate them here.

1. **Format** — `cargo fmt --check` must be clean.
2. **Lint** — `cargo clippy -- -D warnings` must be clean (zero warnings).
3. **Unit tests** — `cargo test` must pass across all crates.
4. **TPC-H differential (DEFERRED during the τ reimplementation per ADR-022)** — `./tests/scripts/run-differential-tests.sh tpch` is *not* a required gate right now. The DataFrame corpus (`tests/scripts/v2-progress.sh`, 324 cases) is the fitness function; TPC-H is temporarily red until τ covers its query surface. TPC-H rejoins this step as mandatory once τ covers its query surface.
5. **Full differential (when SQL-relevant)** — DEFERRED for the same reason as step 4. The `all` suite exercises TPC-H + TPC-DS through the non-τ path, which is not maintained. Use `./tests/scripts/v2-progress.sh` to measure τ progress on the DataFrame corpus.

A task is **not** done if any step is red. Do not commit, do not declare success, do not move on. If a step is intentionally skipped (e.g., docs-only change skips clippy, or TPC-H is deferred per ADR-022 above), state which step and why.

### 5. Demand Elegance (Balanced)
For non-trivial changes: pause and ask "is there a more elegant Rust way?" Skip for simple, obvious fixes.

### 6. Autonomous Bug Fixing
When given a bug report: just fix it. Point at logs, errors, failing tests, then resolve them.

## Quality Gate

This is the **agent-pipeline gate** — the checks the orchestrated agents in
`/new-feature` and `/fix-bug` must clear after every implementation and after
every review-fix pass. The differential test suites are **intentionally
excluded** from this gate: `core_v2` is the v2-transpiler progress signal,
currently expected to be partially red, measured separately via
`tests/scripts/v2-progress.sh` (or `cargo test -p thunderduck-connect-server
--test differential core_v2 -- --ignored`).

Run, in order, after every implementation and after every review fix:

1. **`cargo check -p <touched-crate>`** must succeed (no compile errors, no
   missing imports). For multi-crate changes, run once per crate touched.

2. **`cargo fmt --check`** on the files the agent created or modified must be
   clean. Use `git diff --name-only HEAD -- '*.rs' | xargs -r rustfmt --check`
   to scope to changed files, since the workspace baseline has pre-existing
   formatting drift this work does not own.

3. **`cargo test -p <touched-crate> --lib --tests`** must pass — all unit and
   lib tests for the crate the change touches. Examples:
   `cargo test -p thunderduck-core` for `crates/core/` work;
   `cargo test -p thunderduck-connect-server` for `crates/connect-server/`.

Clippy is **not** in the agent pipeline's gate because the workspace baseline
has pre-existing clippy errors in unrelated modules. The agent must not
introduce *new* clippy warnings on the files it touches — verify ad-hoc, but a
workspace `cargo clippy` run is not required.

For the broader human-driven verification (full clippy, full differential
suite when SQL generation is touched), see `### 4. Verification Before Done`
above.

## Code Search Tools

Two MCP-backed search tools are preinstalled in the devcontainer. They answer different kinds of questions — pick the right one:

- **codegraph** — structural queries over a parsed AST/symbol graph. Use when you have a symbol name or want to trace relationships.
  - "Where is `SqlGenerator` defined?" → `codegraph_search`
  - "What calls `visit_join`?" → `codegraph_callers`
  - "What does `convert_aggregate` call?" → `codegraph_callees`
  - "What would break if I change `LogicalPlan`?" → `codegraph_impact`
  - "Show me the signature / source of `to_sql`" → `codegraph_node`
  - "Give me focused context for an area" → `codegraph_context`
  - Exact, deterministic, AST-backed. Answers grep can't give (callers, callees, impact).

- **semble** — semantic / hybrid search over code chunks. Use when you don't know the symbol name yet, just intent.
  - "How does session lifecycle work?" → `semble.search`
  - "Where do we stream Arrow back to the client?" → `semble.search`
  - "Find code similar to this snippet" → `semble.find_related`
  - Fuzzy, intent-based. Good for unfamiliar areas; can also index remote git URLs.
  - **Pass the project root as `repo`** (the current working directory, e.g. `/workspace` in this devcontainer) — without it, semble errors with "No repo specified and no default index."

**Rule of thumb**: named symbol or relationship → codegraph; fuzzy intent or unfamiliar area → semble. If semble surfaces a candidate symbol, hand it to codegraph for the precise structural follow-up. Prefer either over raw grep for code questions.

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
5. **Spark parity is the only emission target.** τ matches Apache Spark exactly via (a) CASTs at top-level SELECT projection or (b) DuckDB extension functions; the `thdck_spark_funcs` extension is mandatory (rearchitect ADR-020).
6. **Zero result copying**: 100% type matching is achieved at SQL generation time using extension functions + AS aliases. No Arrow vector copying or rewriting.
7. **`to_sql()` is for SQL generation only.** `Display` / `Debug` implementations are human-readable debug output only — never used to build SQL strings sent to DuckDB.

## Architecture Quick-Reference

See [docs/thunderduck-rearchitect-ADRs.md](docs/thunderduck-rearchitect-ADRs.md) for full decisions. Key summary:

### Crate Structure

```
crates/core/                        # Pure translation engine (no gRPC)
  transpiler_v2/                    # τ: CommonAst, analyzer, emission, INV enforcement
    ast.rs                          # CommonAst + CommonOp (shared IR)
    analyzer.rs                     # Resolve + type + nullability over CommonAst → TypedAst
    emission.rs                     # TypedAst → DuckDB SQL
    expression.rs                   # τ Expression enum
    type_inference.rs               # Spark-parity type inference
    invariants.rs                   # INV1-10 mechanical enforcement (grep barriers, todo! gates)
  parser_v2/                        # SparkSQL parser (sqlparser-rs + SparkDialect) → CommonAst
  types/                            # DataType, StructField, StructType
  runtime/                          # DuckDB session, Arrow streaming, extension loading

crates/connect-server/              # gRPC binary (tonic)
  service.rs                        # SparkConnectService (tonic)
  session/                          # SessionManager (DashMap + per-session OS threads)
  converter/
    v2_relation_converter.rs        # Protobuf Relation → CommonAst
    relation_converter.rs           # parse_json_schema helper (LocalRelation schema decode)
    type_converter.rs               # DataType ↔ proto DataType
  arrow_schema_stamp.rs             # Arrow-schema re-stamp so wire schema == τ's resolved_schema
```

### Key Types

| Layer | Type | Responsibility |
|-------|------|----------------|
| **Service** | `SparkConnectService` | tonic gRPC service: receives Spark Connect requests |
| **Session** | `SessionManager` | Manages sessions; each session owns a DuckDB Connection on a dedicated OS thread |
| **Converter** | `V2RelationConverter` | Spark Connect protobuf Relation → `CommonAst` |
| **Converter** | `V2ExpressionConverter` | Spark Connect protobuf Expression → τ `Expression` |
| **Parser** | `SparkSqlParserV2` | sqlparser-rs based Spark SQL parser (raw SQL path) → `CommonAst` |
| **IR** | `CommonAst` / `CommonOp` (enum) | Shared IR — same tree fed by both front-ends |
| **Expression** | τ `Expression` (enum) | τ's Spark-parity expression types with `data_type()` / `nullable()` |
| **Analyzer** | `analyze()` | `CommonAst` + `BaseTypes` → `TypedAst { op, resolved_schema }` |
| **Emission** | `dispatch_op()` / `render_expr()` | Traverses `TypedAst`, produces DuckDB SQL |
| **Runtime** | `DuckDbSession` | Owns `duckdb::Connection` on its dedicated OS thread |
| **Types** | `TypeInferenceEngine` | Resolves expression types following Spark semantics |

### CRITICAL: `to_sql()` vs `Display`

Expression rendering **MUST** use dedicated emission functions (`render_expr`, `dispatch_op`), not `Display` / `Debug`. The `Display` implementation is for debug logging only. This is a recurring bug class; do not repeat it.

### DuckDB Threading Model

`duckdb::Connection` is `!Send + !Sync`. Each session runs on a dedicated `std::thread`. The gRPC async handler communicates via `tokio::sync::mpsc` channels:

```
tokio task → mpsc::Sender<SessionCommand> → session thread (owns Connection)
session thread → oneshot::Sender<SessionResult> → tokio task → gRPC stream
```

### Expression Hierarchy (key types — τ `Expression` enum)

```
Expression (enum)
  Literal               # constant values
  ColumnReference       # resolved column ref with type info and qualifier
  UnresolvedColumn      # unresolved (pre-resolution)
  Binary(BinaryExpression)   # left OP right
  Unary(UnaryExpression)     # OP operand
  FunctionCall          # func(args) — τ owns its Spark-parity translation table
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

3. **Composite aggregate expressions**: When adding expression types that can appear inside aggregates, ensure `V2RelationConverter::convert_aggregate()` handles them. A default `_` arm silently drops unknown cases.

4. **Semi/Anti join in flat chains**: τ emits native `SEMI JOIN` / `ANTI JOIN` in `render_join`. The flat-chain rendering branch must break at semi/anti boundaries — folding the chain across a semi/anti would change the tree shape and reorder filtering semantics.

5. **DuckDB SEMI JOIN syntax**: DuckDB uses `SEMI JOIN` and `ANTI JOIN` (without `LEFT` prefix). `LEFT SEMI JOIN` is a parser error.

6. **Extension version pinning**: The `.duckdb_extension` binary DuckDB version must exactly match the `duckdb` crate version in `Cargo.toml`. Currently pinned to the `ext6` release (multi-version: pulls the `v1.5.4` binaries to match the `duckdb` crate at `1.10504.0`).

7. **HUGEINT overflow**: DuckDB `SUM()` of integer columns returns `HUGEINT` (i128). Spark returns `BIGINT` (i64). SQL generation must emit explicit `CAST(... AS BIGINT)` for integer SUM.

8. **Schema inference vs DESCRIBE**: Prefer `plan.infer_schema()` for schema analysis. Only fall back to issuing `DESCRIBE` queries to DuckDB when plan-level inference is impossible.

9. **Loud-fail on unhandled Arrow types in `local_relation_to_values_sql`**: τ's LocalRelation converter has a loud-fail rule: no catch-all `Ok` fallbacks on typed dispatch. Silent `_ => Ok("NULL")` catch-alls used to map every unhandled Arrow type (including `Decimal128`) to SQL literal `NULL`, corrupting `createDataFrame` payloads. Adding a new Arrow-type payload requires a real match arm, not a silent NULL.

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

The `thdck_spark_funcs` DuckDB extension is **mandatory** and bundled into every build (rearchitect ADR-020 — "relaxed mode" has been eliminated). It implements Spark-precise numerical semantics:
- `spark_hash(c1, ..., cN)` — Spark `hash()` (Murmur3-32, signed INT, seed 42)
- `spark_xxhash64(c1, ..., cN)` — Spark `xxhash64()` (xxHash64, signed BIGINT, seed 42)
- `spark_decimal_div(a, b)` — decimal division with `ROUND_HALF_UP`
- `spark_sum(col)` — Spark-compatible SUM return types
- `spark_avg(col)` — Spark-compatible AVG return types
- `spark_skewness(col)` — population skewness (Spark semantics)

Source: release [`ext6`](https://github.com/nubank/thunderduck-duckdb-extension/releases/tag/ext6) (multi-version — currently pulls the `v1.5.4` binaries). On a fresh build, `build.rs` downloads the correct platform binary from GitHub releases and caches it under `extensions/ext6/` (gitignored). The binary is embedded via `include_bytes!()` and loaded at every session's startup; failure to load is a hard error.

> Full details: [rearchitect ADR-020](docs/thunderduck-rearchitect-ADRs.md).

## Documentation Structure

1. **Rearchitecture ADRs** (`docs/thunderduck-rearchitect-ADRs.md`) — **authoritative** architecture (ADR-000 → ADR-022 + Cross-Validation). Source of truth on any contradiction.
2. **Legacy ADRs** (`docs/adrs/legacy-transpiler/`) — SUPERSEDED, historical reference only. Describe the retired v1 transpiler.
3. **Dev journal** (`docs/dev-journal-toc.md`) — chronological development history; entries in `docs/dev_journal/`
4. **Agent context** (`docs/context/`) — condensed reference (architecture, build commands, coding standards, dependencies, gotchas, testing) for τ
5. **Dev cheatsheets** (`docs/dev-cheatsheets/`) — portable, project-agnostic technique libraries loaded by the language-specialized subagents (see §Agent Cheatsheets below)
6. **Task tracking** (`tasks/`) — active work items and lessons learned; retired plans under `tasks/archive/`

## Agent Cheatsheets

Language-specialized subagents (`rust-coder`, `rust-diagnostician`, `rust-reviewer`, `rust-perf`, `rust-architect`) load their persona from `.claude/agents/*.md` (kept small — memento headlines only) and their technique library from portable cheatsheets under `docs/dev-cheatsheets/`. Every subagent MUST read the linked cheatsheet AND this file before starting work; project rules here and in the ADRs override any generic idiom in the cheatsheets.

- Debugging methodology → [`docs/dev-cheatsheets/rust-debugging.md`](docs/dev-cheatsheets/rust-debugging.md)
- Implementation patterns → [`docs/dev-cheatsheets/rust-implementation.md`](docs/dev-cheatsheets/rust-implementation.md)
- Review checklist → [`docs/dev-cheatsheets/rust-review.md`](docs/dev-cheatsheets/rust-review.md)
- Performance analysis → [`docs/dev-cheatsheets/rust-perf.md`](docs/dev-cheatsheets/rust-perf.md)
- Architecture plan shape → [`docs/dev-cheatsheets/rust-architecture.md`](docs/dev-cheatsheets/rust-architecture.md)

The `docs-updater` subagent is language-agnostic and has no cheatsheet; its policy comes entirely from this file.

## Spark Specification Lookup

When a task involves Spark compatibility (type inference, nullability, decimal precision, function behavior, schema propagation, error semantics), consult **Apache Spark 4.1.1 in ANSI mode** (`spark.sql.ansi.enabled=true`, per ADR-016) as the authoritative specification:

- **Spark source is authoritative.** Use `WebSearch` / `WebFetch` on the `apache/spark` GitHub repo for the relevant source (`DecimalPrecision.scala`, `TypeCoercion.scala`, `HiveResult.scala`, `ArithmeticExpression.scala`, `UpdateFields.scala`, etc.). Spark's behavior in these files defines "correct" for τ.
- **`.reference/` is the Java Thunderduck implementation** (only if present in the working copy). When looking there for equivalent behaviour, note that its structure and function boundaries may differ from τ's — use it as a Spark-parity cross-check, not a template.
- **Spark parity wins over DuckDB-native ergonomics** (ADR-015). If DuckDB offers a shorter emission that changes Spark's observable behavior (return type, nullability, error class, precision, sort order), don't take the shortcut.
- **ANSI-mode error semantics matter.** Division / mod by zero, `element_at` OOB, cast overflow, and `to_number` format mismatches THROW in ANSI mode (see ADR-016 error-emulation contract). τ must re-wrap DuckDB engine throws with Spark's error class before crossing the wire — never surface an opaque DuckDB error string.

## Git Commit Workflow

**Critical Rule**: NEVER commit code without user review first. Show changes, wait for explicit approval, then commit.

## Development Cheatsheet

### Build

> **DuckDB linkage:** DuckDB is **non-bundled by default**. Pick one:
> - **Local dev:** run `scripts/dev/dev-cache-setup.sh` once (links a shared prebuilt libduckdb via
>   `DUCKDB_LIB_DIR`); then the plain commands below work — no DuckDB recompile.
> - **Fresh clone / CI:** add `--features bundled` to `build`/`test`/`check`/`clippy` to compile DuckDB
>   from source. Builds fail to link without one of these.

```bash
# Full build (debug) — local dev (prebuilt libduckdb via scripts/dev/)
cargo build
# Fresh clone / CI: compile DuckDB from source
cargo build --features bundled

# Release build (for integration tests) — always bundles the thdck_spark_funcs extension
cargo build --release                       # local dev (prebuilt libduckdb via scripts/dev/)
cargo build --release --features bundled    # fresh clone / CI (compile DuckDB from source)

# Build a single crate
cargo build -p thunderduck-core
cargo build -p thunderduck-connect-server

# Check (faster than build, no codegen)
cargo check
```

### Unit Tests

> Same DuckDB linkage rule as Build: local dev works as-is (prebuilt lib); on a fresh clone / CI add
> `--features bundled` (e.g. `cargo test --features bundled`).

```bash
# All unit tests
cargo test

# Single module
cargo test -p thunderduck-core -- types::

# Single test
cargo test -p thunderduck-core -- transpiler_v2::emission::tests::render_project

# With output
cargo test -- --nocapture
```

### Integration / Differential Tests

> **Spark IS INSTALLED** — vendored in the **main checkout** at `/workspace/.spark/spark-4.1.1`
> (with its venv at `/workspace/.venv`). The runner's default probe (`$HOME/spark/current`)
> misses it, and worktrees have no in-tree `.spark/`. From a worktree, export the paths first:
> ```bash
> export SPARK_HOME=/workspace/.spark/spark-4.1.1 THUNDERDUCK_VENV_DIR=/workspace/.venv
> ```
> Do **not** re-run `setup-differential-testing.sh` — Spark is already present.

```bash
# Full differential test suite (all 41 test files)
./tests/scripts/run-differential-tests.sh all

# Quick check: TPC-H only
./tests/scripts/run-differential-tests.sh tpch

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

**Last Updated**: 2026-05-24

# Project Guidelines

## Rust Standards

This project follows strict Rust conventions enforced by specialized agents.

### Quality Gate Commands
Run in this order; each must pass before the next is meaningful. See [Verification Before Done](#4-verification-before-done) for when each is mandatory.
- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`
- `./tests/scripts/v2-progress.sh` (DataFrame corpus — the τ fitness gate)

### Code Style & Invariants
- No `.unwrap()` in library code. `.expect()` only for proven invariants.
- All public items must have `///` doc comments.
- Error types use `thiserror` in library crates, `anyhow` in application crates.
- No hardcoded secrets. Use environment variables via `dotenvy`.

### Preferred Crates
- Async runtime: `tokio`
- HTTP server: `axum` + `tower`
- HTTP client: `reqwest`
- Serialization: `serde` + `serde_json`
- CLI: `clap` (derive)
- Logging: `tracing`
- Database: `sqlx`

### Code Style
- Accept the most general borrow: `&str` not `&String`, `&[T]` not `&Vec<T>`
- Iterator chains over manual loops when intent is clearer
- 4 parameters max per function; use config structs beyond that
- Return early to reduce nesting
- Derive `Debug` on everything; other derives only when semantically meaningful

## Agent Pipeline

This project uses a multi-agent development pipeline. Invoke with:
```
/rust-feature <describe the feature or requirement>
```

### Pipeline Stages
1. **Architect** (read-only, Opus) — explores codebase, produces architecture plan
2. **Coder** (read-write, Sonnet) — implements the plan, runs tests
3. **Reviewer** (read-only, Opus) — reviews for correctness, style, security
4. **Coder** (fix loop, max 3 iterations) — addresses Critical/High review findings
5. **Perf Reviewer** (read-only, Opus) — identifies optimization opportunities
6. **Perf Optimizer** (read-write, Sonnet) — implements approved optimizations
7. **Summary** — compiled for human review

### Agent Communication
Agents communicate through `.agent-output/` markdown files:
- `001-architecture-plan.md` — Architecture decisions and type skeletons
- `002-implementation-log.md` — Files changed, tests added, fix iterations
- `003-review-findings.md` — Review findings with severity and verdicts
- `004-perf-findings.md` — Performance analysis with hypotheses
- `005-summary.md` — Final human-readable summary

### Agent Boundaries
- **Read-only agents** (architect, reviewer, perf-reviewer): CANNOT modify source files
- **Read-write agents** (coder, perf-optimizer): CAN modify source files and run commands
- Agents must stay within their designated scope — no architecture changes from the coder, no code fixes from the reviewer
