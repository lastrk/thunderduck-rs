# DuckDB Extension Loading

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). ADR index: [`../README.md`](../README.md) · τ spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: Bundle the `thdck_spark_funcs` binary into the Rust binary at build time; extract to a temp file and `LOAD` at every session's startup**

```rust
static EXTENSION_BYTES: &[u8] = include_bytes!(env!("EXTENSION_BIN_PATH"));
```

All 4 platform binaries of the adopted `ext6` release are vendored — checked into git plain under `extensions/vendored/` (`MANIFEST.toml` + one `.duckdb_extension` per platform), exactly one version at a time, adopted via `scripts/dev/adopt-extension-release.sh`. `build.rs` picks the platform-appropriate binary at build time — no network access required. The extension is **mandatory** — failure to load is a hard startup error (see [rearchitect ADR-020](../../thunderduck-rearchitect-ADRs.md)). The extension is the `thdck_spark_funcs` DuckDB extension — a C/C++ extension, platform-independent from the Rust host's perspective, compiled separately and bundled as bytes.

**Source is now in-tree.** The extension's C++ source (previously an external repository, `nubank/thunderduck-duckdb-extension`) has been absorbed into this repository at [`extension/`](../../../extension/), with `extension/duckdb` and `extension/extension-ci-tools` as pinned git submodules (see `extension/BUILD_PINS.toml`) — see `extension/README.md`'s Provenance section for the import history and `docs/context/extension-archival-checklist.md` for the archival status of the origin repo. Local dev builds go through `scripts/dev/build-extension.sh`; producing new vendored binaries for `extensions/vendored/` is a `workflow_dispatch`-only CI job, `.github/workflows/extension-release.yml`.

Platforms supported: `linux_amd64`, `linux_arm64`, `osx_amd64`, `osx_arm64`. Unsupported host platforms are unsupported builds; `build.rs` panics with a clear message.

**Critical**: The extension binary DuckDB version must exactly match the `duckdb` crate's linked library version. DuckDB enforces this at `LOAD` time.

---

← [Back to ADR Index](../README.md)
