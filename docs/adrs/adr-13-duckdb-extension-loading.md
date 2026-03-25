# ADR-13: DuckDB Extension Loading

**Decision: Embed platform-specific extension binaries in the Rust binary; extract to a temp file and `LOAD` at runtime**

```rust
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const EXTENSION: &[u8] = include_bytes!("../extensions/linux_amd64/thdck_spark_funcs.duckdb_extension");
```

The extension is the `thdck_spark_funcs` DuckDB extension from the `thunderduck-duckdb-extension` repository (v1.5.0 branch). It is a C/C++ DuckDB extension — platform-independent from the Rust host's perspective, compiled separately and bundled as bytes.

Platforms supported: `linux_amd64`, `linux_arm64`, `osx_amd64`, `osx_arm64`.

If no extension is bundled for the current platform, the server starts in relaxed mode with a log warning.

**Critical**: The extension binary DuckDB version must exactly match the `duckdb` crate's linked library version (1.5.0). DuckDB enforces this at `LOAD` time.

---

← [Back to Architecture Overview](../architecture.md)
