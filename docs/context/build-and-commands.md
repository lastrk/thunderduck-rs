# Build & Commands Reference

> **Scope: τ (the only production path per ADR-022).**

## Build

```bash
# Full build (debug). It downloads the pinned DuckDB release when needed.
cargo build

# Release build (for integration tests). Embeds the vendored thdck_spark_funcs
# extension (extensions/vendored/) for the current platform — no download.
cargo build --release

# Build a single crate
cargo build -p thunderduck-core
cargo build -p thunderduck-connect-server

# Check (faster than build — no codegen)
cargo check
```

## Server

```bash
# Start server (default port 15002)
./target/release/thunderduck-connect-server

# Custom port
./target/release/thunderduck-connect-server --port 15002

# Kill server (worktree-scoped; ownership-verified — never touches other worktrees)
./tests/scripts/kill-test-servers.sh
```

## Change-and-Test Workflow

```bash
./tests/scripts/kill-test-servers.sh 2>/dev/null
cargo build --release
./tests/scripts/differential-progress.sh   # full suite + progress row (fast iteration: run-differential-tests.sh <group>)
./tests/scripts/kill-test-servers.sh 2>/dev/null
```
