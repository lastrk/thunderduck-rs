use crate::error::Result;
use crate::error::ThunderduckError;

static EXTENSION_BYTES: &[u8] = include_bytes!(env!("EXTENSION_BIN_PATH"));

/// Load the bundled `thdck_spark_funcs` extension into `conn`.
///
/// The extension is downloaded at build time (`build.rs`) and embedded in the
/// binary; loading is mandatory and a hard error on failure.
pub fn load(conn: &duckdb::Connection) -> Result<()> {
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
    Ok(())
}
