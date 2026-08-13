# Integrating thdck_spark_funcs with Thunderduck

This document describes how the `thdck_spark_funcs` DuckDB extension is
embedded into and loaded by **thunderduck-rs**, the Rust port of Thunderduck.
The extension itself now lives in-tree at `extension/` (this directory) in the
thunderduck-rs repository — see `extension/README.md`'s Provenance section for
how it got here. This document is the Rust-host integration contract, the
counterpart of the legacy JVM/JDBC bundling scheme it replaces.

## Prerequisites

- The extension is built as a `.duckdb_extension` loadable binary (4 platform
  binaries: `linux_amd64`, `linux_arm64`, `osx_amd64`, `osx_arm64`).
- The DuckDB version the binary was compiled against **must match exactly**
  the DuckDB version embedded in the `duckdb` Rust crate thunderduck-rs
  depends on (`Cargo.toml`). DuckDB enforces a strict ABI/version check at
  `LOAD` time and refuses to load a mismatched extension.

## Version Alignment

Currently pinned to DuckDB `v1.5.5` (`extension/BUILD_PINS.toml` is the
authoritative pin; see also `docs/context/dependencies.md` → Version Pinning
and `docs/context/gotchas.md` #6 in thunderduck-rs). The `duckdb` crate
version `1.10505.0` encodes this: crate minor `10505` decodes to DuckDB
`v1.5.5` (`1` + `05` + `05`).

If the DuckDB version is updated, three things move together:
- the `extension/duckdb` submodule (`git checkout v<new>`),
- `extension/BUILD_PINS.toml`'s `[duckdb]` block, and
- the `duckdb` crate version in thunderduck-rs's `Cargo.toml`.

`scripts/dev/build-extension.sh` (thunderduck-rs) asserts this three-way lock
before building and fails loudly on a mismatch.

## Two Embedding Phases

thunderduck-rs distinguishes a **build-time embed** (what ships in the
compiled server binary) from a **runtime override** (a local-dev convenience)
— they are different phases, do not confuse them:

1. **Build-time embed (production path).** thunderduck-rs vendors all 4
   platform binaries of exactly one adopted release under
   `extensions/vendored/` (repo root, checked into git — see
   `scripts/dev/adopt-extension-release.sh`). `crates/core/build.rs` selects
   the binary matching Cargo's `TARGET` and copies it to `OUT_DIR`;
   `crates/core/src/runtime/extension_loader.rs` embeds those bytes via
   `include_bytes!(env!("EXTENSION_BIN_PATH"))` at compile time. No network
   access is needed to build thunderduck-rs.
2. **Build-time override (extension development).** Setting
   `THUNDERDUCK_EXT_PATH=/path/to/thdck_spark_funcs.duckdb_extension` when
   running `cargo build` makes `build.rs` copy *that* binary into `OUT_DIR`
   instead of the vendored set, bypassing vendoring entirely. This is how an
   extension developer iterating in `extension/` swaps in a freshly built,
   unreleased binary without going through the vendoring/adoption flow. See
   `scripts/dev/build-extension.sh --smoke`, which builds the extension from
   this directory and re-runs thunderduck-rs's extension-loader test suite
   against the fresh binary via this exact env var.

Note the naming symmetry with (and difference from) the unrelated
`THUNDERDUCK_DELTA_EXT_PATH` runtime env var: that one `LOAD`s a *second,
additional* extension (`duckdb-delta`) at session startup; `THUNDERDUCK_EXT_PATH`
replaces the embedded `thdck_spark_funcs` bytes at *compile* time.

## Runtime Loading

At every session's startup, `extension_loader::load()` (thunderduck-rs,
`crates/core/src/runtime/extension_loader.rs`) writes the embedded bytes to a
unique temp file and issues `LOAD '<path>'` against the session's DuckDB
connection — no `allow_unsigned_extensions` connection property is required
because the loader points `LOAD` directly at a plain file path rather than
installing it into DuckDB's extension registry. Loading is **mandatory**:
failure is a hard error that aborts session creation, since every Spark plan
τ emits assumes `spark_*` functions are present.

## Usage

Once loaded, any SQL thunderduck-rs's transpiler (τ) emits can reference the
extension's functions directly:

```sql
SELECT spark_decimal_div(col_a, col_b) FROM my_table;
```

No explicit `INSTALL` or `LOAD` is needed anywhere in τ's emitted SQL — the
loader above runs once per session before any user query executes.

## Constraints

- **Version lock**: the extension binary, the `extension/duckdb` submodule
  tag, and the `duckdb` crate version in thunderduck-rs's `Cargo.toml` must
  all encode the same DuckDB version. A mismatch is a hard `LOAD` error at
  session startup.
- **Platform lock**: the extension is compiled per OS/CPU architecture.
  thunderduck-rs vendors all 4 shipped platforms
  (`linux_amd64`/`linux_arm64`/`osx_amd64`/`osx_arm64`); `build.rs` picks the
  one matching Cargo's `TARGET` and panics if none matches.
- **Per-session**: `LOAD` applies to the DuckDB connection it runs against;
  every new session created by thunderduck-rs's `SessionManager` loads the
  extension independently (see `extension_loader::load`).
- **Release provenance**: producing new vendored binaries is a
  `workflow_dispatch`-only CI job (`.github/workflows/extension-release.yml`,
  thunderduck-rs) that builds this directory's source for the single pinned
  DuckDB version and opens a PR checking the 4 binaries into
  `extensions/vendored/` — see that workflow and
  `docs/context/extension-archival-checklist.md`.
