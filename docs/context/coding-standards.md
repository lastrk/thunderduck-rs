# Coding Standards

> **Scope: τ (the only production path per ADR-022).** These are language-level rules (Rust hygiene, error-type conventions, borrow patterns, commit workflow). Substrate-independence questions are covered by INV10 in `docs/thunderduck-rearchitect-ADRs.md`.

## Rust Standards

All code must pass the following gates before merge:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

### Quality Rules

- **No `.unwrap()` in library code.** Use `?`, `expect()` for proven invariants, or typed errors.
- **`.expect()` only for proven invariants** — the expect message must state the invariant.
- **All public items must have `///` doc comments.**
- **Error types**: `thiserror` in library crates, `anyhow` in application/binary crates.
- **No hardcoded secrets.** Use environment variables.

## Quality Gate

The checks a coder subagent must clear after every implementation and after every review-fix pass. Run in order; each must pass before the next is meaningful:

1. **`cargo check -p <touched-crate>`** — succeeds (no compile errors, no missing imports). Run once per crate touched.
2. **`cargo fmt --check`** on created/modified files — clean. Scope to changed files: `git diff --name-only HEAD -- '*.rs' | xargs -r rustfmt --check --edition 2021`. (The workspace baseline has pre-existing formatting drift this work does not own; plain `rustfmt --check` without `--edition 2021` also fails spuriously here.)
3. **`cargo test -p <touched-crate> --lib --tests`** — all unit/lib tests for the touched crate pass (e.g. `cargo test -p thunderduck-core` for `crates/core/` work, `cargo test -p thunderduck-connect-server` for `crates/connect-server/`). When a crate has multiple test binaries, the summary prints one `test result:` line per binary — read the sum, not just the last block.

Clippy is **not** in this gate: the workspace baseline has pre-existing clippy errors in unrelated modules. Do not introduce *new* clippy warnings on touched files (verify ad-hoc); a workspace `cargo clippy` run is not required here. The broader human verification (full clippy, full differential suite) lives in CLAUDE.md → Verification Before Done.

## Core Stack

The crates this project is actually built on — reach for these rather than
alternatives:

| Concern | Crate |
|---------|-------|
| gRPC / protobuf | `tonic` + `prost` |
| Async runtime | `tokio` (multi-thread scheduler) |
| Embedded engine | `duckdb` |
| Columnar data / IPC | `arrow` (shared dep tree with `duckdb-rs`) |
| SQL parsing | `sqlparser` (+ SparkDialect) |
| Serialization | `serde` + `serde_json` |
| CLI | `clap` (derive) |
| Logging | `tracing` |
| Errors | `thiserror` (library crates), `anyhow` (binary crates) |

## Code Style

- **Borrow the most general type**: `&str` not `&String`, `&[T]` not `&Vec<T>`.
- **Iterator chains over manual loops** when intent is clearer.
- **4 parameters max per function**; use config structs beyond that.
- **Return early** to reduce nesting.
- **Derive `Debug` on everything**; other derives only when semantically meaningful.
- **Prefer enums over trait objects** for closed sets. Prefer `match` over dynamic dispatch.

## Git Commit Workflow

**Critical Rule**: NEVER commit code without user review first. Show the diff, wait for explicit approval, then commit.

This applies to every commit, no exceptions. The cost of pausing is low; the cost of an unwanted commit on a shared branch is high.
