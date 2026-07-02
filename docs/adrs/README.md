# Thunderduck ADRs — Index & Agent-Context Router

This directory is the entry point to Thunderduck's architecture decisions. It is organized so a
**designer or reviewer agent can be pointed at exactly the right slice** without reading everything.

The transpiler (`τ`: Spark → DuckDB SQL) is being rebuilt as a v2 path alongside the existing one.
The two sets of records reflect that:

- **[`../thunderduck-rearchitect-ADRs.md`](../thunderduck-rearchitect-ADRs.md)** — the **authoritative v2 spine** (ADR-000 → ADR-021 + Cross-Validation + Open Questions). It drives the v2 implementation.
- **[`runtime/`](runtime/)** — existing decisions for the serving/execution substrate. Apply to **both** paths; not superseded.
- **[`legacy-transpiler/`](legacy-transpiler/)** — the existing `τ` internals, running behind `--transpiler legacy` (the default) while v2 is grown behind `--transpiler v2`.

---

## Precedence (read this first)

1. On any conflict about the **transpiler** (parsing, analysis, type/nullability inference, SQL emission, extension functions, commands, lakehouse I/O), the **rearchitect ADRs win**. They are authoritative.
2. `runtime/` decisions (gRPC, async, DuckDB bindings, threading, sessions, Arrow streaming, extension loading, errors, crate layout) are **current** and orthogonal to the transpiler rebuild — the rearchitecture does not restate them.
3. `legacy-transpiler/` decisions are **valid reference for the code that still runs by default**, but are **superseded where they conflict** with the rearchitecture. The two paths **coexist**; do not delete the legacy path to make room for v2.
4. Two ADRs were fully superseded and **removed** — see [Superseded & removed](#superseded--removed). Git history retains them.

---

## Agent router — what to inject for which task

| If the agent is… | Inject |
|---|---|
| **Designing / implementing the v2 transpiler** (parse → analyze → emit) | The whole [rearchitect spine](../thunderduck-rearchitect-ADRs.md) + the [v2 spine at a glance](#v2-spine-at-a-glance) below + [`tasks/v2-adr-readiness-map.md`](../../tasks/v2-adr-readiness-map.md) (slice → ADR → corpus Case-IDs) + [`tasks/v2-restart-inheritance-checklist.md`](../../tasks/v2-restart-inheritance-checklist.md) |
| **Reviewing v2 work** | The rearchitect **§Cross-Validation** (tensions T1–T6, [LB1–LB9](#load-bearing-assumptions-lb1lb9), [INV1–INV10](#cross-cutting-invariants-inv1inv10)) + the specific ADRs the slice cites |
| **Adding / changing a Spark function or expression emission** | Rearchitect **ADR-009** (emission table) + **ADR-010** (extension functions) + **ADR-005** (types/nullability). Legacy analogs: [`legacy-transpiler/function-registry.md`](legacy-transpiler/function-registry.md), [`legacy-transpiler/sql-generator.md`](legacy-transpiler/sql-generator.md) |
| **Working on the SparkSQL parser front-end** | [`legacy-transpiler/sparksql-parser.md`](legacy-transpiler/sparksql-parser.md) (parser technology) + rearchitect **ADR-004** (both front-ends lower to the common AST) |
| **Working on serving / runtime / threading / streaming / sessions** | [`runtime/`](runtime/) (see [table](#runtime--serving-substrate-runtime)) |
| **Touching the legacy transpiler path** | [`legacy-transpiler/`](legacy-transpiler/) + the coexistence rule above |
| **Lakehouse read / write (Delta, Iceberg, Unity Catalog)** | Rearchitect **ADR-012** (catalog overlay), **ADR-013** (external reads), **ADR-017** (Delta writes), **ADR-018** (Iceberg writes), **ADR-019** (I/O contract) |

> Numbering note: `ADR-0NN` (zero-padded) always refers to the **rearchitect spine**. The preserved
> legacy/runtime records are referenced by **topic filename**, not by a number, to avoid the old
> `ADR-09`-means-two-things collision.

---

## v2 spine at a glance

Authoritative decisions; one line each. Full text (with `Depends on` / `Depended on by` and refinement hooks) in [`../thunderduck-rearchitect-ADRs.md`](../thunderduck-rearchitect-ADRs.md).

| ADR | Decision |
|---|---|
| **000** | Positioning: single-node, vertically-scaled, instant-start, **no-JVM** DuckDB-backed Spark Connect server. Selects "reimplement a minimal Rust slice" over embedding Catalyst. |
| **001** | `τ` is a **transliterator, not an optimizer** — no cost-driven rewrites; only expressibility-forced transforms, result-irrelevant cosmetic reductions, and enumerated carve-outs. |
| **002** | **Emit-level delegation**: own only the slice where Spark diverges from DuckDB — namely type inference + nullability. Delegate structural resolution to DuckDB's binder. |
| **003** | IR is a proto-inspired **common AST**, extended incrementally (add a node only when SQL needs it and it isn't composable), not a full Catalyst `LogicalPlan`. |
| **004** | **Both front-ends lower to the common AST**; relation-vs-command decided by parse-tree root. Raw `spark.sql(...)` is parsed (not string-rewritten). Dispatch at the protobuf boundary. |
| **005** | thunderduck **owns Spark type & nullability inference** over the common AST (the divergent slice): a coercion lattice + nullability derivation. |
| **006** | The analyzer is a **bounded sequence of coordinated passes** (mostly one bottom-up), not iterate-to-fixed-point. Explicit extra passes for set-op widening / correlation / aggregate scoping. |
| **007** | `τ` layers **A (annotate) / B (tree-rewrite) / C (escape hatch)**; B is retained but minimal (expressibility-forced + SQL desugarings + carve-outs). |
| **008** | **Correlated subqueries emitted directly** as DuckDB correlated subqueries — no rewrite to lateral. |
| **009** | The **emission table is declarative data**, keyed on `(op, operand types, mode, nullability)` — simultaneously the input grammar and the coverage denominator. **Compiled dispatch**. |
| **010** | **Extension functions** (in the C++ `thunderduck-duckdb-extension` project) are a *minimal gap-filler* for value/return-type divergences DuckDB can't match natively. |
| **011** | The Spark Connect **`Command` arm** is in scope as a separate `emit_command` path; its oracle is **catalog/table state**, not result rows. |
| **012** | A **narrow catalog overlay** carries Spark types of base relations (+ access provenance/format); commands write it, resolution reads it. |
| **013** | **External / lakehouse reads** (Hive-Parquet, Delta, Iceberg, Unity Catalog) delegate to DuckDB storage extensions; **read-only** this iteration. |
| **014** | **Two decision spaces** (translation, resolution) with independent coverage; three failure-attribution buckets (resolver / translator / DuckDB-excluded). |
| **015** | **Differential oracle** vs reference Spark (serialize once, send identical bytes to both); variation-suppression is test-side; inference validated in isolation via **AnalyzePlan**. |
| **016** | **Pinned reference version**: Spark 4.1.1; DuckDB ≥ v1.5.3 where Iceberg writes are used. Coverage claims are version-scoped. |
| **017** | **Delta writes** = append into a pre-existing *attached* table only; DELETE/MERGE/overwrite/create are typed rejections pending DuckDB support. |
| **018** | **Iceberg writes** target Databricks UC-managed Iceberg via the attached REST catalog: CTAS / INSERT / DELETE / MERGE (single-table, merge-on-read). |
| **019** | **Lakehouse I/O contract**: read inputs in their native format, write results as Iceberg, both via UC — each format on its DuckDB-strong side; no cross-format single-table access. |
| **020** | **Strict-only**: the `thdck_spark_funcs` extension is mandatory; "relaxed mode" is eliminated. One emission target. |
| **021** | **v2 substrate independence**: dispatch at the protobuf boundary; v2-native `Expression` and `TypeInferenceEngine`; no shared legacy `LogicalPlan` input. Only value-level types shared. |
| **§CV** | Cross-Validation: layered structure, dependency matrix, tensions T1–T6, load-bearing assumptions, invariants, ratification order, slice sub-split rules. |
| **OQ-1** | Raw-SQL handling — **resolved** by ADR-004 (parse to common AST). |
| **OQ-2** | External write paths — **partially addressed** (Delta append ADR-017, UC Iceberg ADR-018); remainder deferred per-format. |

---

## Cross-cutting invariants (INV1–INV10)

The reviewer's checklist — any refinement must preserve all of these (full text in §CV.5).

| # | Invariant |
|---|---|
| **INV1** | Both engines receive **byte-identical input** (serialize-once-send-twice). |
| **INV2** | Every `τ` decision is **node-local** (post-A) or a labeled **C escape hatch** — never a hidden closure in the table. |
| **INV3** | The **emission table is the single source of truth** for generation and coverage (emission-side contamination barrier). |
| **INV4** | **Inference is validated in isolation** (AnalyzePlan green) before translation failures are read as translation bugs. |
| **INV5** | thunderduck **knows the schema everywhere**, even where it emits delegated structure (keep the internal resolver/star-expander). |
| **INV6** | Every `Extension(...)` target in the table **exists and is loaded** in the C++ extension. |
| **INV7** | Both **v2 front-ends** produce the **same common-AST node** for semantically equivalent inputs. |
| **INV8** | External-table access is **always delegated** to a DuckDB storage extension (no home-grown reader/log/catalog). |
| **INV9** | A **writable** external relation must have **attached-catalog provenance**; path-scan provenance is read-only. |
| **INV10** | **v2 substrate independence**: no runtime import of legacy `LogicalPlan` / `Expression` / `TypeInferenceEngine` (input-side barrier; complements INV3). |

## Load-bearing assumptions (LB1–LB9)

Assumptions whose failure cascades; each is empirically checkable (full text in §CV.4).

| # | Assumption |
|---|---|
| **LB1** | The divergent slice is exactly **{type inference, nullability}**. |
| **LB2** | DuckDB **correctly executes valid SQL**. |
| **LB3** | DuckDB's **structural resolution** (binding, star, scope) matches what's wanted. |
| **LB4** | `τ` never needs an optimization to be **correct** (only for performance). |
| **LB5** | A/B/C + the C++ extension functions are **expressive enough** for every *supported* expression. |
| **LB6** | The **single-node ceiling** is sufficient for target workloads. |
| **LB7** | DuckDB storage extensions **read** external/lakehouse tables faithfully to Spark. |
| **LB8** | DuckDB extensions **write** external/lakehouse tables faithfully to Spark. |
| **LB9** | **Two independent** Spark-semantics implementations validated by the oracle beat one shared implementation on coupling at equal correctness. |

---

## Runtime & serving substrate ([`runtime/`](runtime/))

Current decisions; apply to both transpiler paths.

| File | Decision |
|---|---|
| [grpc-framework.md](runtime/grpc-framework.md) | `tonic` + `prost`; Spark Connect protos compiled at build time. |
| [async-runtime.md](runtime/async-runtime.md) | `tokio` multi-thread scheduler for gRPC I/O, session lifecycle, result streaming. |
| [duckdb-bindings.md](runtime/duckdb-bindings.md) | `duckdb` crate (`arrow` feature); version-pinned to the `ext6` extension binary. |
| [arrow-library.md](runtime/arrow-library.md) | `apache/arrow-rs` end-to-end (same dep as duckdb-rs); `arrow2` rejected. |
| [threading-model.md](runtime/threading-model.md) | Dedicated OS thread per session (`Connection` is `!Send`); `mpsc`/`oneshot` to the async layer. |
| [session-management.md](runtime/session-management.md) | `DashMap` of session handles; named in-memory DuckDB database per session. |
| [crate-structure.md](runtime/crate-structure.md) | Cargo workspace: `core` (translation) + `connect-server` (gRPC); `transpiler_v2` module. |
| [arrow-duckdb-zero-copy.md](runtime/arrow-duckdb-zero-copy.md) | `query_arrow()` → Arrow IPC → tonic streaming; no copies on the hot path. |
| [extension-loading.md](runtime/extension-loading.md) | `thdck_spark_funcs` embedded via `include_bytes!`, `LOAD`ed per session (mandatory). |
| [error-handling.md](runtime/error-handling.md) | `thiserror` in `core`, `anyhow` in `connect-server`; maps to `tonic::Status`. |

## Legacy transpiler ([`legacy-transpiler/`](legacy-transpiler/)) + legacy → v2 successor map

Existing `τ` internals (default path). Each row names the v2 decision that governs the same concern.

| File | Documents (legacy) | v2 successor |
|---|---|---|
| [logical-plan.md](legacy-transpiler/logical-plan.md) | Legacy `LogicalPlan` enum (compiler-exhaustive `match`) | ADR-003 (common AST); ADR-021 (v2-native plan via `V2RelationConverter`) |
| [expression-system.md](legacy-transpiler/expression-system.md) | Legacy `Expression` enum (`to_sql`/`data_type`/`nullable`) | ADR-003 (v2-native `Expression` payload); ADR-021 |
| [type-system.md](legacy-transpiler/type-system.md) | `DataType` enum + legacy `TypeInferenceEngine` | `DataType` shared (ADR-021 §4); inference → ADR-005 / ADR-006 (v2-native engine) |
| [sql-generator.md](legacy-transpiler/sql-generator.md) | Legacy match-based `SqlGenerator` (`gen_*`, dual join path) | ADR-009 (declarative emission table, compiled dispatch); ADR-007 (A/B/C) |
| [plan-converter.md](legacy-transpiler/plan-converter.md) | Legacy `PlanConverter` / `RelationConverter` / `ExpressionConverter` | ADR-004 + ADR-021 (`V2RelationConverter`, dispatch at the protobuf boundary) |
| [function-registry.md](legacy-transpiler/function-registry.md) | 500+ Spark → DuckDB function mappings | ADR-009 (emission table) + ADR-010 (extension functions) |
| [correctness-rules.md](legacy-transpiler/correctness-rules.md) | 5 SQL-generation invariants (**still current**, both paths) | Echoed by §CV **INV1–INV10** (esp. INV2, INV3) + ADR-001 |
| [testing-strategy.md](legacy-transpiler/testing-strategy.md) | Unit + Python differential harness | ADR-014 (two decision spaces) + ADR-015 (differential + AnalyzePlan oracle) |
| [sparksql-parser.md](legacy-transpiler/sparksql-parser.md) | Parser tech: `sqlparser-rs` + `SparkDialect` (T1); `chumsky` (T2) | **Complements** ADR-004 (which mandates parsing SparkSQL into the common AST) — *not superseded* |

## Superseded & removed

Removed from the active set on 2026-07-02; recoverable from git history at the paths shown.

| Former ADR | Replaced by | Note |
|---|---|---|
| `adr-10-sparksql-raw-sql-path.md` — `preprocess_spark_sql` 13-phase text rewrite | Rearchitect **ADR-004** (parse to common AST; string-bashing explicitly tried-and-rejected) | Was already superseded by the parser strategy; the text-rewrite pass has been removed. |
| `adr-15-compatibility-modes.md` — Strict / Relaxed / Auto `CompatMode` | Rearchitect **ADR-020** (strict-only; extension mandatory; "relaxed" eliminated) | Single emission target; `--relaxed` / `THUNDERDUCK_COMPAT_MODE` no longer recognized. |

---

## Related docs

- [`../thunderduck-rearchitect-ADRs.md`](../thunderduck-rearchitect-ADRs.md) — the authoritative v2 spine.
- [`../architecture.md`](../architecture.md) — existing-implementation system overview.
- [`../context/`](../context/) — condensed agent reference (architecture, build commands, coding standards, dependencies, gotchas, testing).
- [`../dev-journal-toc.md`](../dev-journal-toc.md) — chronological development history.
- [`../../tasks/v2-adr-readiness-map.md`](../../tasks/v2-adr-readiness-map.md) — slice → ADR → corpus Case-ID map for v2 implementation.
- [`../../CLAUDE.md`](../../CLAUDE.md) — project rules and development cheatsheet.
