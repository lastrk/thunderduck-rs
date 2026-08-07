# Claude Code Project Rules

Project-specific **policy and process** for thunderduck-rs (the Rust port of Thunderduck). Factual reference — architecture, commands, gotchas, standards — lives under `docs/context/` and the ADRs; see the [Documentation Map](#documentation-map) at the bottom and pull the relevant doc in on demand.

## Project Vision

**Keep your Spark API, get single-node DuckDB performance.** Thunderduck is a drop-in Spark Connect server backed by DuckDB for workloads that don't need distributed compute. This is the Rust port: same Spark API compatibility, fast startup (~50ms vs ~10s JVM), and low memory overhead (~30MB vs ~500MB JVM baseline).

### Authoritative architecture

[**docs/thunderduck-rearchitect-ADRs.md**](docs/thunderduck-rearchitect-ADRs.md) (ADR-000 → ADR-022 + Cross-Validation) is the **authoritative** architecture source for the transpiler (`τ`) and the source of truth on any contradiction. The condensed, day-to-day reference is [`docs/context/architecture.md`](docs/context/architecture.md).

### τ is the only path (per ADR-022)

τ (the transpiler at `crates/core/src/transpiler_v2/`, `crates/connect-server/src/converter/v2_relation_converter.rs`, and `crates/core/src/parser_v2/`) is the only production path. Every Spark Connect request flows to τ; τ's output is the response. If τ has not implemented a given operator, it produces a Thunderduck-boundary error (`Unsupported*`) directly to the caller. There is no fallback, no dispatch flag, no alternate implementation.

**Three error categories** (ADR-022, third added by its Amendment 1 on 2026-08-07): (1) **Spark-emulated errors** — inputs Spark itself would reject; τ matches Spark's error semantics. (2) **Thunderduck-boundary errors** — inputs Spark accepts but τ has not implemented; honest "not implemented in Thunderduck." (3) **Strict rejections** — inputs Spark *accepts* that τ deliberately rejects as malformed under the standard Spark claims to follow. Every instance must be listed in ADR-022's strict-rejection register; an unregistered strict rejection is a bug, not policy. `sqlparser`'s grammar is the mechanism, never the authority — it both over-rejects valid Spark and over-accepts malformed SQL.

**Practical implications:**
- The two corpora are τ's fitness functions — the DataFrame corpus (405 cases) and the SQL corpus (408 cases). TPC-H/TPC-DS live *inside* the corpora as `tpch-*`/`tpcds-*` cases and are held to the same standard as every other case: a red TPC case is a defect to fix, not tolerable background signal. Commands and mechanics: [`docs/context/testing.md`](docs/context/testing.md).
- The v1 transpiler modules (`crates/core/src/{logical,expression,generator,functions,parser}/`) were deleted on 2026-07-05. INV3/INV10 in `crates/core/src/transpiler_v2/invariants.rs` mechanically enforce that τ does not import from those (now-absent) prefixes, nor from `crate::runtime`.

## Workflow Orchestration

### Plan Mode Default
Enter plan mode for ANY non-trivial task (3+ steps or architectural decisions). If something goes sideways, STOP and re-plan immediately.

### Self-Improvement Loop
After ANY correction from the user: update `tasks/lessons.md` with the pattern. Review lessons at session start.

### Verification Before Done
Never mark a task complete without proving it works. For any non-trivial change, run these in order and require each to pass before moving on (exact commands: [`docs/context/coding-standards.md`](docs/context/coding-standards.md), [`docs/context/build-and-commands.md`](docs/context/build-and-commands.md), [`docs/context/testing.md`](docs/context/testing.md)):

1. **Format** — `cargo fmt --check` clean.
2. **Lint** — `cargo clippy -- -D warnings` clean.
3. **Unit tests** — `cargo test` passes across all crates.
4. **Corpus differential** — the DataFrame and SQL corpora are the fitness gates; the hard requirement is **no previously-green case regresses**.

A task is **not** done if any step is red. Do not commit, do not declare success, do not move on. If a step is intentionally skipped (e.g., a docs-only change skips clippy), state which step and why.

### Demand Elegance (Balanced)
For non-trivial changes: pause and ask "is there a more elegant Rust way?" Skip for simple, obvious fixes.

## Core Principles
- **Simplicity First**: make every change as simple as possible; impact minimal code.
- **No Laziness**: find root causes. No temporary fixes. Senior Rust developer standards.
- **Minimal Impact**: touch only what's necessary; avoid introducing bugs.
- **Idiomatic Rust**: enums over trait objects for closed sets, `match` over dynamic dispatch, `thiserror` for typed errors, no `unwrap()` in library code. Full rules in [`docs/context/coding-standards.md`](docs/context/coding-standards.md).
- **Respect architectural underpinnings**:  see [`docs/context/architecture.md`](docs/context/architecture.md).

## Documentation Map

CLAUDE.md holds only policy and process. Everything factual lives in the docs below — pull the relevant one in when the task touches its area.

**Authoritative architecture**
- [`docs/thunderduck-rearchitect-ADRs.md`](docs/thunderduck-rearchitect-ADRs.md) — ADR-000 → ADR-022 + Cross-Validation. Source of truth for τ's design; consult on any architecture question or contradiction.
- [`docs/adrs/legacy-transpiler/`](docs/adrs/legacy-transpiler/) — SUPERSEDED v1 ADRs, historical reference only.

**Condensed reference — [`docs/context/`](docs/context/)** (pull in when the task touches the area):
- [`architecture.md`](docs/context/architecture.md) — crate layout, key types, data flow, DuckDB threading model, emission entry points, SQL-generation principles, Spark-parity requirements. **Read before changing the analyzer, emission, or a converter.**
- [`gotchas.md`](docs/context/gotchas.md) — 14 recurring bug classes + non-obvious constraints. **Read before touching emission, the LocalRelation converter, extension loading, the session thread, or the Arrow wire boundary.**
- [`build-and-commands.md`](docs/context/build-and-commands.md) — build / check / run / server commands and DuckDB linkage (bundled vs prebuilt).
- [`testing.md`](docs/context/testing.md) — unit + differential test commands, corpus mechanics, TPC clusters, per-worktree isolation, key data paths.
- [`coding-standards.md`](docs/context/coding-standards.md) — Rust hygiene gates, core stack, code style, commit rule.
- [`dependencies.md`](docs/context/dependencies.md) — the mandatory `thdck_spark_funcs` extension (in-tree C++ source at `extension/`, vendored binaries at `extensions/vendored/`, local builds via `scripts/dev/build-extension.sh`), version pins, Spark Connect config.
- [`code-search-tools.md`](docs/context/code-search-tools.md) — **READ THIS ALWAYS** for token-efficient, super-fast and accurate code exploration, code search, code lookup methods using tools other than grep.
- [`spark-parity-lookup.md`](docs/context/spark-parity-lookup.md) — how to consult Apache Spark 4.1.1 source as the authoritative parity spec.
- [`delta-cross-repo-dev-loop.md`](docs/context/delta-cross-repo-dev-loop.md) — Delta read/write dev loop across thunderduck ⇄ duckdb-delta ⇄ delta-kernel-rs.

**Other**
- [`extension/CLAUDE.md`](extension/CLAUDE.md) — the in-tree `thdck_spark_funcs` DuckDB extension's own rules (C++11 conventions, quality gate, Spark-parity contract). **Read before touching anything under `extension/`.** Archival of the origin repos: [`docs/context/extension-archival-checklist.md`](docs/context/extension-archival-checklist.md).
- [`docs/dev-journal-toc.md`](docs/dev-journal-toc.md) + [`docs/dev_journal/`](docs/dev_journal/) — chronological development history.
- [`docs/dev-cheatsheets/`](docs/dev-cheatsheets/) — portable Rust technique libraries (debugging, implementation, review, perf, architecture), loaded by the language-specialized subagents.
- [`tasks/`](tasks/) — active work items and `lessons.md`; retired plans under `tasks/archive/`.
