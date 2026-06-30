# ADR-13: DuckDB Extension Loading

**Decision: Bundle the `thdck_spark_funcs` binary into the Rust binary at build time; extract to a temp file and `LOAD` at every session's startup**

```rust
static EXTENSION_BYTES: &[u8] = include_bytes!(env!("EXTENSION_BIN_PATH"));
```

`build.rs` downloads the platform-appropriate binary from the [`ext4` release](https://github.com/nubank/thunderduck-duckdb-extension/releases/tag/ext4) and caches it under `extensions/ext4/`. The extension is **mandatory** — failure to load is a hard startup error (see [rearchitect ADR-020](../thunderduck-rearchitect-ADRs.md)). The extension is the `thdck_spark_funcs` DuckDB extension from the `thunderduck-duckdb-extension` repository — a C/C++ extension, platform-independent from the Rust host's perspective, compiled separately and bundled as bytes.

Platforms supported: `linux_amd64`, `linux_arm64`, `osx_amd64`, `osx_arm64`. Unsupported host platforms are unsupported builds; `build.rs` panics with a clear message.

**Critical**: The extension binary DuckDB version must exactly match the `duckdb` crate's linked library version. DuckDB enforces this at `LOAD` time.

---

← [Back to Architecture Overview](../architecture.md)
