# Thunderduck ADRs — Index & Agent-Context Router

This directory is the entry point to Thunderduck's architecture decisions. It is organized so a
**designer or reviewer agent can be pointed at exactly the right slice** without reading everything.

**Subject:** Spark Connect → DuckDB SQL transliterator (`τ`), its front-ends, its analyzer, and its test architecture.
**Status of set:** Proposed — for ratification before / alongside reimplementation.
**Reference Spark version:** 4.1.1.

The active ADRs are ordered as a logical narrative: ADR-000 establishes the product premise that
selects the whole approach; ADR-001–002 state what τ is and what it delegates; ADR-003–004 define
the intermediate representation and how both front-ends populate it; ADR-005–006 define the
analyzer that resolves and types it; ADR-007–010 define how it is transformed and emitted;
ADR-011–013 cover commands, the catalog, and external/lakehouse reads; ADR-014–016 are the testing
architecture the rest makes possible; ADR-017–019 add the per-format write paths and end-to-end I/O
contract; ADR-020 pins the emission target; ADR-021 pins the substrate boundary; ADR-022 pins the
runtime position; ADR-024 defines resolved attribute identity; ADR-025 defines ANSI interval field
spans; and ADR-026 defines Spark Connect plan-ID resolution.

Review an ADR on its own using its refinement hooks. For a change spanning decisions, also load
[`cross-validation.md`](cross-validation.md) to check dependencies, tensions, assumptions, and
invariants.

τ (Spark → DuckDB SQL) is the only production transpiler per ADR-022. Records live in four places:

- **Individual `adr-*.md` files listed below** — the **authoritative active τ spine**.
- **[`runtime/`](runtime/)** — current decisions for the serving/execution substrate.
- **[`retired/`](retired/)** — superseded τ records, retained only as design history.
- **[`legacy-transpiler/`](legacy-transpiler/)** — SUPERSEDED. Describes the retired v1 transpiler (Rust modules deleted 2026-07-05). Historical reference only.

---

## Precedence (read this first)

1. On any conflict about the **transpiler** (parsing, analysis, type/nullability inference, SQL emission, extension functions, commands, lakehouse I/O), the **rearchitect ADRs win**. They are authoritative.
2. `runtime/` decisions (gRPC, async, DuckDB bindings, threading, sessions, Arrow streaming, extension loading, errors, crate layout) are **current** and orthogonal to the transpiler.
3. `retired/` decisions are SUPERSEDED — load only when the active ADR links to historical rationale.
4. `legacy-transpiler/` decisions are SUPERSEDED — historical reference only. Do not use as guidance for new work.
5. Two older ADRs were fully superseded and **removed** — see [Superseded & removed](#superseded--removed). Git history retains them.

---

## Agent router — what to inject for which task

| If the agent is… | Inject |
|---|---|
| **Designing / implementing τ** (parse → analyze → emit) | The specific active ADRs routed below + [`cross-validation.md`](cross-validation.md) for changes that cross decision boundaries |
| **Reviewing τ work** | [`cross-validation.md`](cross-validation.md) (tensions T1–T6, LB1–LB9, INV1–INV10) + the specific ADRs the change cites |
| **Adding / changing a Spark function or expression emission** | [ADR-009](adr-009-emission-table.md) + [ADR-010](adr-010-extension-functions.md) + [ADR-005](adr-005-type-nullability-inference.md) |
| **Working on the SparkSQL parser front-end** | [ADR-004](adr-004-common-ast-frontends.md) |
| **Changing the analyzer, type inference, or nullability** | [ADR-005](adr-005-type-nullability-inference.md) + [ADR-006](adr-006-analyzer-passes.md) + [ADR-015](adr-015-differential-oracle.md) |
| **Changing resolved columns, qualifiers, joins, or missing-column recovery** | [ADR-024](adr-024-resolved-attribute-identity.md) + [ADR-006](adr-006-analyzer-passes.md) |
| **Changing Connect `plan_id`, DataFrame columns, stars, or regexes** | [ADR-026](adr-026-connect-plan-id-lookup.md) + [ADR-024](adr-024-resolved-attribute-identity.md) |
| **Changing interval types, lowering, inference, or Arrow representation** | [ADR-025](adr-025-ansi-interval-field-spans.md) + [ADR-005](adr-005-type-nullability-inference.md) + [ADR-016](adr-016-version-and-ansi-pins.md) |
| **Changing commands or catalog/session-visible state** | [ADR-011](adr-011-command-path.md) + [ADR-012](adr-012-catalog-overlay.md) |
| **Changing differential tests or compatibility claims** | [ADR-014](adr-014-testing-decision-spaces.md) + [ADR-015](adr-015-differential-oracle.md) + [ADR-016](adr-016-version-and-ansi-pins.md) |
| **Working on serving / runtime / threading / streaming / sessions** | [`runtime/`](runtime/) (see [table](#runtime--serving-substrate-runtime)) |
| **Lakehouse read / write (Delta, Iceberg, Unity Catalog)** | [ADR-012](adr-012-catalog-overlay.md), [ADR-013](adr-013-external-lakehouse-reads.md), [ADR-017](adr-017-delta-append-writes.md), [ADR-018](adr-018-iceberg-writes.md), [ADR-019](adr-019-lakehouse-io-contract.md) |

> Numbering note: `ADR-0NN` identifies the rearchitect ADR family, active or
> retired. Legacy/runtime records use topic filenames instead.

---

## τ spine at a glance

Authoritative decisions; one line each. Follow the ADR link for its full text, dependencies, and refinement hooks.

| ADR | Decision |
|---|---|
| [**ADR-000**](adr-000-positioning.md) | Positioning: single-node, vertically-scaled, instant-start, **no-JVM** DuckDB-backed Spark Connect server. Selects "reimplement a minimal Rust slice" over embedding Catalyst. |
| [**ADR-001**](adr-001-transliterator-not-optimizer.md) | `τ` is a **transliterator, not an optimizer** — no cost-driven rewrites; only expressibility-forced transforms, result-irrelevant cosmetic reductions, and enumerated carve-outs. |
| [**ADR-002**](adr-002-emit-level-delegation.md) | **Emit-level delegation**: ordinary structural binding goes to DuckDB; τ owns type/nullability plus ADR-026's bounded Connect plan-ID exception. |
| [**ADR-003**](adr-003-common-ast.md) | IR is a proto-inspired **common AST**, extended incrementally (add a node only when SQL needs it and it isn't composable), not a full Catalyst `LogicalPlan`. |
| [**ADR-004**](adr-004-common-ast-frontends.md) | **Both front-ends lower to the common AST**; relation-vs-command decided by parse-tree root. Raw `spark.sql(...)` is parsed (not string-rewritten). Dispatch at the protobuf boundary. |
| [**ADR-005**](adr-005-type-nullability-inference.md) | thunderduck **owns Spark type & nullability inference** over the common AST (the divergent slice): a coercion lattice + nullability derivation. |
| [**ADR-006**](adr-006-analyzer-passes.md) | The analyzer is a **bounded sequence of coordinated passes**, mostly bottom-up, with named non-upward traversals including ADR-026 plan-ID lookup. |
| [**ADR-007**](adr-007-translation-layers.md) | `τ` layers **A (annotate) / B (tree-rewrite) / C (escape hatch)**; B is retained but minimal (expressibility-forced + SQL desugarings + carve-outs). |
| [**ADR-008**](adr-008-correlated-subqueries.md) | **Correlated subqueries emitted directly** as DuckDB correlated subqueries — no rewrite to lateral. |
| [**ADR-009**](adr-009-emission-table.md) | **Closed, inspectable dispatch**: structural AST emission is handwritten and exhaustive; callable semantics use one interpreted `FunctionSpec` registry with five closed implementation routes. |
| [**ADR-010**](adr-010-extension-functions.md) | **Extension functions** (in the C++ `thunderduck-duckdb-extension` project, now in-tree at [`extension/`](../../extension/)) are a *minimal gap-filler* for value/return-type divergences DuckDB can't match natively. |
| [**ADR-011**](adr-011-command-path.md) | The Spark Connect **`Command` arm** is in scope as a separate `emit_command` path; its oracle is **catalog/table state**, not result rows. |
| [**ADR-012**](adr-012-catalog-overlay.md) | A **narrow catalog overlay** carries Spark types of base relations (+ access provenance/format); commands write it, resolution reads it. |
| [**ADR-013**](adr-013-external-lakehouse-reads.md) | **External / lakehouse reads** (Hive-Parquet, Delta, Iceberg, Unity Catalog) delegate to DuckDB storage extensions; **read-only** this iteration. |
| [**ADR-014**](adr-014-testing-decision-spaces.md) | **Two decision spaces** (translation, resolution) with independent coverage; three failure-attribution buckets (resolver / translator / DuckDB-excluded). |
| [**ADR-015**](adr-015-differential-oracle.md) | **Differential oracle** vs reference Spark (serialize once, send identical bytes to both); variation-suppression is test-side; inference validated in isolation via **AnalyzePlan**. |
| [**ADR-016**](adr-016-version-and-ansi-pins.md) | **Pinned reference version**: Spark 4.1.1; DuckDB ≥ v1.5.3 where Iceberg writes are used. Coverage claims are version-scoped. |
| [**ADR-017**](adr-017-delta-append-writes.md) | **Delta writes** = append into a pre-existing *attached* table only; DELETE/MERGE/overwrite/create are typed rejections pending DuckDB support. |
| [**ADR-018**](adr-018-iceberg-writes.md) | **Iceberg writes** target Databricks UC-managed Iceberg via the attached REST catalog: CTAS / INSERT / DELETE / MERGE (single-table, merge-on-read). |
| [**ADR-019**](adr-019-lakehouse-io-contract.md) | **Lakehouse I/O contract**: read inputs in their native format, write results as Iceberg, both via UC — each format on its DuckDB-strong side; no cross-format single-table access. |
| [**ADR-020**](adr-020-strict-only-target.md) | **Strict-only**: the `thdck_spark_funcs` extension is mandatory; "relaxed mode" is eliminated. One emission target. |
| [**ADR-021**](adr-021-tau-substrate.md) | **τ substrate independence**: dispatch at the protobuf boundary; τ-native `Expression` and `TypeInferenceEngine`. Only value-level types shared. |
| [**ADR-022**](adr-022-only-path-error-categories.md) | **τ is the only path**: no fallback, no dispatch flag, no alternate implementation; errors follow ADR-022's active category contract. |
| [**ADR-024**](adr-024-resolved-attribute-identity.md) | **Resolved attribute identity**: `ResolvedSchema` stores stable `ExprId` and qualifier lineage; references bind to attributes rather than positions alone. |
| [**ADR-025**](adr-025-ansi-interval-field-spans.md) | **ANSI interval field spans** are durable `DataType` structure and round-trip through analysis, Connect schemas, and interval lowering. |
| [**ADR-026**](adr-026-connect-plan-id-lookup.md) | **Spark Connect plan-ID lookup**: preserve per-node IDs and mirror Catalyst's top-down search plus ancestor-`ExprId` filtering; no join ID sets. |
| [**§CV**](cross-validation.md) | Cross-Validation: layered structure, dependency matrix, tensions T1–T6, load-bearing assumptions, invariants, ratification order. |
| [**OQ-1**](resolved-and-open-questions.md#oq-1--raw-sql-sparksql-handling--resolved-by-adr-004) | Raw-SQL handling — **resolved** by ADR-004 (parse to common AST). |
| [**OQ-2**](resolved-and-open-questions.md#oq-2--external--lakehouse-table-write-paths--partially-addressed-remainder-deferred-per-format) | External write paths — **partially addressed** (Delta append ADR-017, UC Iceberg ADR-018); remainder deferred per-format. |

---

## Cross-cutting invariants (INV1–INV10)

The reviewer's checklist — any refinement must preserve all of these (full text in [`cross-validation.md`](cross-validation.md#cv5--cross-cutting-invariants)).

| # | Invariant |
|---|---|
| **INV1** | Both engines receive **byte-identical input** (serialize-once-send-twice). |
| **INV2** | Every `τ` decision is **node-local** (post-A) or a labeled **C escape hatch** — never a hidden closure in the table. |
| **INV3** | The live function registry is the single callable-name authority; structural AST emission remains exhaustive handwritten dispatch. Neither path imports the deleted v1 modules. |
| **INV4** | **Inference is validated in isolation** (AnalyzePlan green) before translation failures are read as translation bugs. |
| **INV5** | thunderduck **knows the schema everywhere**; type tracking and plan-ID lookup consume the resolved attributes even where emission delegates structure. |
| **INV6** | Every `Extension(...)` target in the table **exists and is loaded** in the C++ extension. |
| **INV7** | No per-run front-end equality check; both lower to the same operator variants, while source-only metadata such as Connect `plan_id` may differ. |
| **INV8** | External-table access is **always delegated** to a DuckDB storage extension (no home-grown reader/log/catalog). |
| **INV9** | A **writable** external relation must have **attached-catalog provenance**; path-scan provenance is read-only. |
| **INV10** | **τ substrate independence**: no runtime import of legacy `LogicalPlan` / `Expression` / `TypeInferenceEngine` (input-side barrier; complements INV3). |

## Load-bearing assumptions (LB1–LB9)

Assumptions whose failure cascades; each is empirically checkable (full text in [`cross-validation.md`](cross-validation.md#cv4--load-bearing-assumptions)).

| # | Assumption |
|---|---|
| **LB1** | The owned divergent slice is **{type inference, nullability, Connect plan-ID lookup}**. |
| **LB2** | DuckDB **correctly executes valid SQL**. |
| **LB3** | DuckDB's ordinary SQL **binding, star, and scope** match what's wanted; Connect plan-ID lookup is explicitly owned. |
| **LB4** | `τ` never needs an optimization to be **correct** (only for performance). |
| **LB5** | A/B/C + the C++ extension functions are **expressive enough** for every *supported* expression. |
| **LB6** | The **single-node ceiling** is sufficient for target workloads. |
| **LB7** | DuckDB storage extensions **read** external/lakehouse tables faithfully to Spark. |
| **LB8** | DuckDB extensions **write** external/lakehouse tables faithfully to Spark. |
| **LB9** | An independent Spark-semantics implementation validated by the oracle beats delegation on coupling at equal correctness. |

---

## Runtime & serving substrate ([`runtime/`](runtime/))

Current decisions applying to the τ runtime.

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

## Legacy transpiler ([`legacy-transpiler/`](legacy-transpiler/))

SUPERSEDED. Historical reference only — describes the retired v1 transpiler stack whose Rust modules were deleted 2026-07-05.

## Retired τ ADRs ([`retired/`](retired/))

Superseded records excluded from the active spine and normal agent context:

| ADR | Replaced by | Note |
|---|---|---|
| [ADR-023](retired/adr-023-ordinal-reference-resolution.md) | ADR-024 and ADR-026 | Attribute binding moved to stored `ExprId`; its plan-ID scope model was incorrect. |

## Superseded & removed

Removed from the active set on 2026-07-02; recoverable from git history at the paths shown.

| Former ADR | Replaced by | Note |
|---|---|---|
| `adr-10-sparksql-raw-sql-path.md` — `preprocess_spark_sql` 13-phase text rewrite | Rearchitect **ADR-004** (parse to common AST; string-bashing explicitly tried-and-rejected) | Was already superseded by the parser strategy; the text-rewrite pass has been removed. |
| `adr-15-compatibility-modes.md` — Strict / Relaxed / Auto `CompatMode` | Rearchitect **ADR-020** (strict-only; extension mandatory; "relaxed" eliminated) | Single emission target; `--relaxed` / `THUNDERDUCK_COMPAT_MODE` no longer recognized. |

---

## Related docs

- [`retired/`](retired/) — superseded τ decisions, loaded only for historical rationale.
- [`../context/`](../context/) — condensed agent reference (architecture, build commands, coding standards, dependencies, testing).
- [`../dev-journal-toc.md`](../dev-journal-toc.md) — chronological development history.
- [`../../CLAUDE.md`](../../CLAUDE.md) — project rules and development cheatsheet.
