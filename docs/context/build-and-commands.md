# Build & Commands Reference

> **Scope: τ (the only production path per ADR-022).**

## Build

```bash
# Full build (debug). Local dev uses the prebuilt libduckdb from
# scripts/dev/dev-cache-setup.sh; fresh clones / CI need --features bundled.
cargo build

# Release build (for integration tests). Downloads + embeds the thdck_spark_funcs
# extension on first run.
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
./tests/scripts/v2-progress.sh
./tests/scripts/kill-test-servers.sh 2>/dev/null
```
