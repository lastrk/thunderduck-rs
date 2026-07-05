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
- **No hardcoded secrets.** Use environment variables via `dotenvy`.

## Preferred Crates

| Concern | Crate |
|---------|-------|
| Async runtime | `tokio` |
| HTTP server | `axum` + `tower` |
| HTTP client | `reqwest` |
| Serialization | `serde` + `serde_json` |
| CLI | `clap` (derive) |
| Logging | `tracing` |
| Database | `sqlx` |

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
