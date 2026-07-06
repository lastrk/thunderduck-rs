use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Result;
use crate::error::ThunderduckError;

static EXTENSION_BYTES: &[u8] = include_bytes!(env!("EXTENSION_BIN_PATH"));

/// Monotonic per-process counter so concurrent `load()` calls materialise the
/// extension to distinct temp files (see `path` below).
static LOAD_SEQ: AtomicU64 = AtomicU64::new(0);

/// Load the bundled `thdck_spark_funcs` extension into `conn`.
///
/// The extension is downloaded at build time (`build.rs`) and embedded in the
/// binary; loading is mandatory and a hard error on failure.
pub fn load(conn: &duckdb::Connection) -> Result<()> {
    // Unique per-process, per-call *directory*, keeping the canonical filename:
    // `SessionManager` can create sessions concurrently (two `get_or_create`
    // calls on different tokio tasks), and each session's `load()`
    // writes-then-removes this file. A shared fixed path let one session's
    // `remove_file` delete the bytes out from under another session's `LOAD`,
    // surfacing as a spurious "not a DuckDB extension" error. The filename
    // itself must stay `thdck_spark_funcs.duckdb_extension` because DuckDB
    // derives the `_duckdb_cpp_init` entrypoint symbol from the file stem, so
    // only the enclosing directory is made unique.
    let seq = LOAD_SEQ.fetch_add(1, Ordering::Relaxed);
    let subdir = std::env::temp_dir().join(format!("thdck-ext-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&subdir).map_err(|e| {
        ThunderduckError::DuckDb(format!("failed to create extension temp dir: {e}"))
    })?;
    let path = subdir.join("thdck_spark_funcs.duckdb_extension");
    std::fs::write(&path, EXTENSION_BYTES)
        .map_err(|e| ThunderduckError::DuckDb(format!("failed to write extension to temp: {e}")))?;

    let path_str = path
        .to_str()
        .ok_or_else(|| ThunderduckError::DuckDb("extension temp path is not valid UTF-8".into()))?;

    conn.execute_batch(&format!("LOAD '{path_str}';"))
        .map_err(|e| ThunderduckError::DuckDb(format!("failed to load extension: {e}")))?;

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&subdir);
    Ok(())
}
