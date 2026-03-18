use crate::error::Result;
use crate::error::ThunderduckError;

// Real platform binaries are dropped in at Phase 6 via include_bytes!.
// Until then every platform gets an empty slice and load() returns Ok(false).
const EXTENSION_BYTES: &[u8] = &[];

/// Attempt to load the bundled `thdck_spark_funcs` extension into `conn`.
///
/// Returns `Ok(true)` if the extension was loaded, `Ok(false)` if no binary
/// exists for this platform (server starts in relaxed mode), or an error if
/// loading failed after a binary was found.
pub fn load(conn: &duckdb::Connection) -> Result<bool> {
    if EXTENSION_BYTES.is_empty() {
        return Ok(false);
    }

    let dir = std::env::temp_dir();
    let path = dir.join("thdck_spark_funcs.duckdb_extension");
    std::fs::write(&path, EXTENSION_BYTES)
        .map_err(|e| ThunderduckError::DuckDb(format!("failed to write extension to temp: {e}")))?;

    let path_str = path
        .to_str()
        .ok_or_else(|| ThunderduckError::DuckDb("extension temp path is not valid UTF-8".into()))?;

    conn.execute_batch(&format!("LOAD '{path_str}';"))
        .map_err(|e| ThunderduckError::DuckDb(format!("failed to load extension: {e}")))?;

    let _ = std::fs::remove_file(&path);
    Ok(true)
}
