# Build & Commands Reference

> **Scope: applies to both legacy and (future) v2 code.** Since 2026-07-02, legacy is the sole active path (the morph-track v2 implementation was discarded at tag `v2-morph-track-end`; the `--transpiler` CLI flag was removed). The v2 restart begins at Slice A per `tasks/v2-adr-readiness-map.md`; when Slice A lands, dispatch relocates to the protobuf boundary per ADR-021 and this document will gain a v2-side subsection.

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

Note: the `--transpiler {legacy,v2}` CLI flag and `THUNDERDUCK_TRANSPILER` env var were removed in the 2026-07-02 v2 delete. Legacy is the sole active path; the env var, if set, logs a runtime warning and is ignored. Slice A of the v2 restart will re-introduce dispatch selection at the protobuf boundary per ADR-021.

## Change-and-Test Workflow

```bash
pkill -f thunderduck-connect-server 2>/dev/null
cargo build --release
cd tests/integration && python3 -m pytest \
  "differential/test_differential_v2.py::TestTPCH_AllQueries_Differential[7]" -v --tb=long
pkill -f thunderduck-connect-server 2>/dev/null
```
