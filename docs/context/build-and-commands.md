# Build & Commands Reference

## Build

```bash
# Full build (debug)
cargo build

# Release build (for integration tests)
cargo build --release

# Release build WITH strict-mode extension (downloads binary on first run)
cargo build --release --features bundled-extension

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

# Strict mode (requires bundled-extension build)
./target/release/thunderduck-connect-server --strict

# Relaxed mode (default)
./target/release/thunderduck-connect-server --relaxed

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

For strict-mode iteration, rebuild with `--features bundled-extension` and prefix the pytest invocation with `THUNDERDUCK_COMPAT_MODE=strict`.
