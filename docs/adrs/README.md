# Thunderduck ADRs — Index & Agent-Context Router

This directory is the entry point to Thunderduck's architecture decisions. It is organized so a
**designer or reviewer agent can be pointed at exactly the right slice** without reading everything.

τ (Spark → DuckDB SQL) is the only production transpiler per ADR-022. Records live in two places:

- **[`../thunderduck-rearchitect-ADRs.md`](../thunderduck-rearchitect-ADRs.md)** — the **authoritative τ spine** (ADR-000 → ADR-022 + Cross-Validation + Open Questions).
- **[`runtime/`](runtime/)** — current decisions for the serving/execution substrate.
- **[`legacy-transpiler/`](legacy-transpiler/)** — SUPERSEDED. Describes the retired v1 transpiler (Rust modules deleted 2026-07-05). Historical reference only.

---

## Precedence (read this first)

1. On any conflict about the **transpiler** (parsing, analysis, type/nullability inference, SQL emission, extension functions, commands, lakehouse I/O), the **rearchitect ADRs win**. They are authoritative.
2. `runtime/` decisions (gRPC, async, DuckDB bindings, threading, sessions, Arrow streaming, extension loading, errors, crate layout) are **current** and orthogonal to the transpiler.
3. `legacy-transpiler/` decisions are SUPERSEDED — historical reference only. Do not use as guidance for new work.
4. Two ADRs were fully superseded and **removed** — see [Superseded & removed](#superseded--removed). Git history retains them.

---

## Agent router — what to inject for which task

| If the agent is… | Inject |
|---|---|
| **Designing / implementing τ** (parse → analyze → emit) | The whole [rearchitect spine](../thunderduck-rearchitect-ADRs.md) + the [τ spine at a glance](#τ-spine-at-a-glance) below |
| **Reviewing τ work** | The rearchitect **§Cross-Validation** (tensions T1–T6, [LB1–LB9](#load-bearing-assumptions-lb1lb9), [INV1–INV10](#cross-cutting-invariants-inv1inv10)) + the specific ADRs the change cites |
| **Adding / changing a Spark function or expression emission** | Rearchitect **ADR-009** (emission table) + **ADR-010** (extension functions) + **ADR-005** (types/nullability) |
| **Working on the SparkSQL parser front-end** | Rearchitect **ADR-004** (both front-ends lower to the common AST) |
| **Working on serving / runtime / threading / streaming / sessions** | [`runtime/`](runtime/) (see [table](#runtime--serving-substrate-runtime)) |
| **Lakehouse read / write (Delta, Iceberg, Unity Catalog)** | Rearchitect **ADR-012** (catalog overlay), **ADR-013** (external reads), **ADR-017** (Delta writes), **ADR-018** (Iceberg writes), **ADR-019** (I/O contract) |

> Numbering note: `ADR-0NN` (zero-padded) always refers to the **rearchitect spine**. The preserved
> legacy/runtime records are referenced by **topic filename**, not by a number.

---

## τ spine at a glance

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
| **010** | **Extension functions** (in the C++ `thunderduck-duckdb-extension` project, now in-tree at [`extension/`](../../extension/)) are a *minimal gap-filler* for value/return-type divergences DuckDB can't match natively. |
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
| **021** | **τ substrate independence**: dispatch at the protobuf boundary; τ-native `Expression` and `TypeInferenceEngine`. Only value-level types shared. |
| **022** | **τ is the only path**: no fallback, no dispatch flag, no alternate implementation. Two error categories (Spark-emulated, Thunderduck-boundary). |
| **§CV** | Cross-Validation: layered structure, dependency matrix, tensions T1–T6, load-bearing assumptions, invariants, ratification order. |
| **OQ-1** | Raw-SQL handling — **resolved** by ADR-004 (parse to common AST). |
| **OQ-2** | External write paths — **partially addressed** (Delta append ADR-017, UC Iceberg ADR-018); remainder deferred per-format. |

---

## Cross-cutting invariants (INV1–INV10)

The reviewer's checklist — any refinement must preserve all of these (full text in §CV.5).

| # | Invariant |
|---|---|
| **INV1** | Both engines receive **byte-identical input** (serialize-once-send-twice). |
| **INV2** | Every `τ` decision is **node-local** (post-A) or a labeled **C escape hatch** — never a hidden closure in the table. |
| **INV3** | The **emission table is the single source of truth** for generation and coverage (emission-side contamination barrier; no imports from the deleted v1 modules). |
| **INV4** | **Inference is validated in isolation** (AnalyzePlan green) before translation failures are read as translation bugs. |
| **INV5** | thunderduck **knows the schema everywhere**, even where it emits delegated structure (keep the internal resolver/star-expander). |
| **INV6** | Every `Extension(...)` target in the table **exists and is loaded** in the C++ extension. |
| **INV7** | Both **τ front-ends** produce the **same common-AST node** for semantically equivalent inputs. |
| **INV8** | External-table access is **always delegated** to a DuckDB storage extension (no home-grown reader/log/catalog). |
| **INV9** | A **writable** external relation must have **attached-catalog provenance**; path-scan provenance is read-only. |
| **INV10** | **τ substrate independence**: no runtime import of legacy `LogicalPlan` / `Expression` / `TypeInferenceEngine` (input-side barrier; complements INV3). |

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

## Superseded & removed

Removed from the active set on 2026-07-02; recoverable from git history at the paths shown.

| Former ADR | Replaced by | Note |
|---|---|---|
| `adr-10-sparksql-raw-sql-path.md` — `preprocess_spark_sql` 13-phase text rewrite | Rearchitect **ADR-004** (parse to common AST; string-bashing explicitly tried-and-rejected) | Was already superseded by the parser strategy; the text-rewrite pass has been removed. |
| `adr-15-compatibility-modes.md` — Strict / Relaxed / Auto `CompatMode` | Rearchitect **ADR-020** (strict-only; extension mandatory; "relaxed" eliminated) | Single emission target; `--relaxed` / `THUNDERDUCK_COMPAT_MODE` no longer recognized. |

---

## Related docs

- [`../thunderduck-rearchitect-ADRs.md`](../thunderduck-rearchitect-ADRs.md) — the authoritative τ spine.
- [`../context/`](../context/) — condensed agent reference (architecture, build commands, coding standards, dependencies, testing).
- [`../dev-journal-toc.md`](../dev-journal-toc.md) — chronological development history.
- [`../../CLAUDE.md`](../../CLAUDE.md) — project rules and development cheatsheet.
