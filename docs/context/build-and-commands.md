# Build & Commands Reference

## Build

```bash
# Full build (debug)
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

# Kill server
pkill -f thunderduck-connect-server
```

## Change-and-Test Workflow

```bash
pkill -f thunderduck-connect-server 2>/dev/null
cargo build --release
cd tests/integration && python3 -m pytest \
  "differential/test_differential_v2.py::TestTPCH_AllQueries_Differential[7]" -v --tb=long
pkill -f thunderduck-connect-server 2>/dev/null
```
