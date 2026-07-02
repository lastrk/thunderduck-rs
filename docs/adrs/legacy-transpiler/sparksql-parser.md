# SparkSQL Parser Strategy

> **Status: current — SparkSQL parser front-end; complements (not superseded by) the rearchitecture.** Rearchitect ADR-004 mandates parsing SparkSQL into the common AST; this ADR records the parser *technology* that implements that front-end (sqlparser-rs + a custom `SparkDialect`, Tier 1; `chumsky`, Tier 2). ADR index: [`../README.md`](../README.md) · v2 spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

> **Context:** the interim `preprocess_spark_sql` text-rewrite pass this strategy replaced has since been removed from the codebase. Raw SQL now flows through a parser front-end (and, in v2, lowers into the common AST per rearchitect ADR-004) rather than through string substitution.

**Decision: sqlparser-rs with a custom `SparkDialect` (Tier 1); chumsky as the upgrade path if sqlparser-rs coverage proves insufficient (Tier 2). Coverage is demand-driven — the parser is extended feature-by-feature as real workloads require it, not upfront.**

### Context

The interim `spark.sql()` path sent the raw SQL string through `preprocess_spark_sql`
— a 13-phase chain of text substitutions — before handing it to DuckDB. That approach passed the
differential suite of its day but has a hard structural ceiling:

- `LATERAL VIEW [OUTER] EXPLODE / POSEXPLODE / JSON_TUPLE` — cannot be text-substituted
- `DISTRIBUTE BY` / `CLUSTER BY` / `SORT BY` — Hive sort directives with no DuckDB analogue
- `TRANSFORM (cols) USING script` — Hive streaming transformation
- `INSERT OVERWRITE ... PARTITION (k=v)` — partition-aware DML
- Multi-insert: `FROM t INSERT INTO t1 ... INSERT INTO t2 ...`
- Each new unsupported construct requires a new fragile special case; string manipulation of SQL
  is inherently brittle on nested expressions, quoted identifiers, and comments

A proper parser replaces the text-passthrough path with the same clean pipeline already used by
the DataFrame (protobuf) path. This is the same architecture as Apache Spark itself: Spark's
`SqlBase.g4` generates a concrete syntax tree (CST), `AstBuilder.scala` visits it to produce an
unresolved `LogicalPlan`, and analysis/optimisation passes resolve it. Thunderduck's equivalent:

```
spark.sql() SQL string
    ↓  SparkSqlParser::parse()              ← new: produces a typed AST
typed AST (sqlparser-rs Statement, ANTLR CST, or chumsky custom AST)
    ↓  SqlConverter::convert()              ← new: mirrors existing RelationConverter
Thunderduck LogicalPlan + Expression        ← same types as the DataFrame path
    ↓  SqlGenerator::generate()             ← existing, unchanged
DuckDB SQL string
```

The `LogicalPlan` and `Expression` enums, `TypeInferenceEngine`, and `SqlGenerator` are all
reused without modification. Only the entry point from SQL string to `LogicalPlan` is new.

When the parser is active, unrecognised constructs return a hard `Status::unimplemented` error —
no silent fallback to the preprocessing pass. This forces complete coverage before each construct
is declared supported.

### Decision Drivers

1. **Demand-driven coverage**: the goal is not 100% grammar compliance upfront — the parser is
   extended construct-by-construct as real workloads require it. Complete parity is a long-term
   direction, not a precondition for shipping.
2. **Correctness over completeness**: a declared list of supported constructs with hard errors is
   better than silent mis-execution of unsupported ones
3. **Reuse existing machinery**: `LogicalPlan`, `Expression`, `SqlGenerator`, and `TypeInferenceEngine`
   are already correct — the parser just needs to produce input for them
4. **Incremental delivery**: the parser can be deployed for a growing subset of Spark SQL without
   a big-bang migration
5. **No new unsafe code**: the existing codebase has zero `unsafe`; the parser must stay safe

### Options Examined

| Option | Approach | Coverage approach | Effort | Verdict |
|--------|----------|------------------|--------|---------|
| **A** | Extend `preprocess_spark_sql` | Incremental, but hits a hard structural ceiling at ~80% | Low per gap | **Reject as sole strategy** — correct up to a hard ceiling, then unmaintainable |
| **B** | `antlr4rust` (pure Rust ANTLR4 runtime) + `SqlBase.g4` | Theoretically complete but per-construct wiring still required | High — Java action code must be ported; unsafe runtime | **Reject** — community-maintained, contains `unsafe` code, no official ANTLR4 backing, no known production deployment |
| **C** | ANTLR4 official C++ runtime via Rust FFI + `SqlBase.g4` | Same grammar as Spark, but demand-driven coverage still applies | Very high — C++ shim layer, 30–60 min builds, parse-tree lifetime model | **Reject** — FFI complexity and build cost are only justified by 100% grammar parity upfront, which is not a goal; see detail below |
| **D** | `pest` PEG parser | Incremental; grammar must be fully rewritten | Very high — full grammar rewrite in a different formalism | **Reject** — PEG ≠ LL(*); left-recursion in SQL must be manually refactored; no production Spark SQL parser built on pest |
| **E** | `nom` / `winnow` combinators | Incremental | High | **Reject** — monomorphised combinator types produce million-character type signatures at SQL scale; LakeSail evaluated and rejected this path |
| **F** | `sqlparser-rs` + `DatabricksDialect` | Incremental; covers ~70–80% with existing dialect | Low to integrate | **Viable bridge** — DatabricksDialect covers lambdas, backtick identifiers, struct/map literals, `GROUPING SETS`; still missing `LATERAL VIEW`, `DISTRIBUTE BY`, `TABLESAMPLE` |
| **G** | `sqlparser-rs` + custom `SparkDialect` | Incremental; each construct added when needed | Medium | **Recommended Tier 1** |
| **H** | `chumsky` hand-written parser | Incremental; full `SqlBase.g4` transliteration possible | High | **Recommended Tier 2** |

#### Option C — ANTLR4 official C++ runtime via Rust FFI (detail)

ANTLR4 has an official, first-party C++ runtime maintained within the main ANTLR4 repository
(`runtime/Cpp/`, latest 4.13.2, August 2024). It is production-quality: **ClickHouse** vendored
it (git-subtree at `ClickHouse/antlr4-runtime`) for SQL parsing at high throughput. It supports
the same visitor and listener patterns as the Java runtime. Running `antlr4 -Dlanguage=Cpp
-visitor SqlBase.g4` generates a `SqlBaseParser.{h,cpp}` and accompanying visitor interfaces
from the exact same grammar that Spark uses.

**Integration into Rust**: The `cxx` crate provides a safe Rust↔C++ bridge but has a critical
limitation — it **cannot implement C++ virtual methods from Rust**. To walk the ANTLR parse tree
in Rust you must implement a `BaseVisitor` (an abstract C++ class). This requires a C++ shim
layer that implements the visitor in C++, holds a Rust function-pointer table, and dispatches
into Rust for each grammar rule. `autocxx` (Google) adds `derive`-based bindings but also lacks
native support for inheriting from pure-virtual C++ classes. The `bindgen` route requires a plain
C wrapper around the entire C++ API. All three paths involve writing and maintaining non-trivial
C++ shim code.

**`@lexer::members` action code**: `SqlBase.g4` (and the split `SqlBaseLexer.g4`) contains Java
code in `@lexer::members` that is load-bearing lexer logic — `isValidDecimal()`,
`isShiftRightOperator()` (resolves `>>` ambiguity in `MAP<STRING, ARRAY<INT>>`),
`complex_type_level_counter`, and `has_unclosed_bracketed_comment`. These must be ported from
Java syntax to C++ before the grammar compiles for the C++ target. The C++ target supports
`@lexer::declarations` / `@lexer::definitions` action hooks for exactly this; the porting is
structurally possible but requires careful correctness verification.

**Build cost**: The generated `SqlBaseParser.cpp` will be on the order of 100,000–200,000 lines
of C++. Compile times: 30–60 minutes for a clean build of just the generated files. The ANTLR4
DFA tables are computed lazily at runtime; the first parse of a given grammar rule takes seconds
(up to 8s reported for large SQL grammars) and warms up over subsequent queries.

**Parse tree lifetime**: ANTLR C++ parse trees are raw `ParseTree*` pointers owned by the
parser's internal arena, valid only while the parser object lives. Modelling this correctly in
Rust requires either opaque wrapper types with explicit lifetime coupling to the parser, or
eagerly copying the tree to a Rust-owned structure before the parser is dropped.

**No prior art**: There is no known production project that calls the ANTLR4 C++ runtime from
Rust. No `antlr4-sys` crate exists on crates.io. This is entirely greenfield FFI work.

**Why rejected**: The sole material advantage of Option C over chumsky (Option H) is that the
ANTLR4 C++ runtime executes the exact same grammar as Spark, giving 100% syntax-level parity by
construction — with no per-construct porting effort beyond the initial shim. Under a demand-driven
coverage model that advantage disappears: each new construct still needs a hand-written
`SqlConverter` case regardless of which parser is used, so the coverage rate is limited by
`SqlConverter` development, not parser breadth. The 30–60 minute build cost, C++ shim layer,
and lack of any prior Rust deployment are therefore not justified. Option H (chumsky) delivers
the same eventual coverage trajectory with lower integration cost.

#### Option G — sqlparser-rs with a custom SparkDialect (Tier 1 detail)  <!-- recommended -->

`sqlparser` (crates.io, v0.61+) is maintained under the Apache DataFusion umbrella at
`apache/datafusion-sqlparser-rs`. Architecture: hand-written recursive descent + Pratt expression
parser. Dialect customisation via ~50 overridable methods on a `Dialect` trait; for constructs
requiring fundamentally different parse logic, `parse_statement` can be overridden to return a
`Statement::Extension` carrying a custom payload.

The `SparkDialect` scope covers constructs in the open-source `SqlBase.g4` grammar that are
absent from the existing built-in dialects:

| Construct | Mechanism |
|-----------|-----------|
| Backtick-quoted identifiers | `is_identifier_start` / `is_identifier_part` override |
| `LATERAL VIEW [OUTER] func() tbl AS (col, ...)` | `parse_statement` intercept → `Statement::Extension` |
| `DISTRIBUTE BY` / `CLUSTER BY` / `SORT BY` | New `Statement` variants (upstream contribution) |
| `PIVOT (agg FOR col IN (...))` / `UNPIVOT` | Existing sqlparser support; dialect flag to enable |
| `TABLESAMPLE (BUCKET n OUT OF m ON col)` | `parse_table_factor` override |
| Lambda syntax `x -> expr` in HOFs | `supports_lambda_functions` flag (DatabricksDialect precedent) |
| Spark `INTERVAL 'n' UNIT` and `INTERVAL expr TO expr` | `parse_interval` override |
| Named struct: `STRUCT(val AS name, ...)` | Expression parser extension |
| `INSERT OVERWRITE ... PARTITION (k=v)` | `parse_insert` override |

`SqlConverter` (`crates/core/src/parser/sql_converter.rs`) converts the sqlparser `Statement` +
`Expr` AST into the Thunderduck `LogicalPlan` + `Expression` tree, following exactly the same
visitor pattern as the existing `RelationConverter`. This is the bulk of the work.

New files:
```
crates/core/src/parser/
    mod.rs             # SparkSqlParser::parse(sql) → Result<LogicalPlan>
    dialect.rs         # SparkDialect: Dialect impl
    sql_converter.rs   # SqlConverter: Statement → LogicalPlan
```

Cargo change: add `sqlparser = "0.61"` to workspace and to `[dependencies]` of `core`.

Upstream contribution strategy: implement `SparkDialect` in Thunderduck first, then contribute
non-proprietary primitives to `apache/datafusion-sqlparser-rs`.

**Trade-offs**: some constructs may require upstream AST additions (requiring PRs); coverage is
incremental. sqlparser-rs is actively maintained with Apache governance — a stable, low-risk
long-term dependency.

#### Option H — chumsky-based hand-written parser (Tier 2 detail)  <!-- upgrade path -->

`chumsky` (`zesterer/chumsky`, v0.9 / v1.0-alpha) is a Rust parser combinator library built
specifically for language parsing, with first-class error recovery (multiple diagnostics from a
single pass), a `pratt()` combinator for operator precedence, and a zero-copy API. It is the
approach chosen by LakeSail for Sail — the only other production Rust Spark-compatible SQL
engine as of 2026.

Sail's experience: custom ~300-line lexer, chumsky for grammar rules, procedural macros
(`#[derive(TreeParser)]`) to reduce boilerplate. Prototype in ~1 week; production quality
required considerably more investment.

Advantages over sqlparser-rs: total control over the AST, rich error recovery for user-facing
messages, no upstream approval for new SparkSQL constructs, `SqlBase.g4` can be transliterated
rule-by-rule. Disadvantages: substantially more upfront investment; chumsky API is at v0.9 /
1.0-alpha (some breakage between versions); the `SqlConverter` interface must be rebuilt from
scratch (or adapted from Tier 1's implementation).

**Upgrade trigger**: if ≥3 critical SparkSQL constructs in `SqlBase.g4` cannot be added to
sqlparser-rs without forking the crate, migrate to chumsky. The `SqlConverter` interface
established in Tier 1 remains unchanged — only the parser and its AST are replaced.

Reference: Sail source at `github.com/lakehq/sail` (open source, Apache-2.0).

### Decision

Adopt the two-tier strategy:

**Tier 1** — `sqlparser-rs` + custom `SparkDialect` + `SqlConverter`, implemented incrementally.
Differential tests serve as acceptance criteria: a construct is "supported" when its differential
test passes with a hard error for everything else.

**Tier 2** — `chumsky`-based full parser (Option H), triggered only if the sqlparser-rs coverage
plateau is reached. The `SqlConverter` interface from Tier 1 is reused; only the parser and AST
change.

**Rejected**: ANTLR4 C++ FFI (Option C) — FFI complexity and 30–60 min build cost are only
justified by 100% grammar parity upfront, which is not a goal under demand-driven coverage;
`antlr4rust` (immature, `unsafe`, no official ANTLR4 Rust target); `pest` (wrong formalism for
LL(*) SQL); `nom`/`winnow` (type complexity at SQL grammar scale).

---

← [Back to ADR Index](../README.md)
