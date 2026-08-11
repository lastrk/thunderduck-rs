use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Result;
use crate::error::ThunderduckError;

static EXTENSION_BYTES: &[u8] = include_bytes!(env!("EXTENSION_BIN_PATH"));

/// Monotonic per-process counter so concurrent `load()` calls materialise the
/// extension to distinct temp files (see `path` below).
static LOAD_SEQ: AtomicU64 = AtomicU64::new(0);

/// Load the bundled `thdck_spark_funcs` extension into `conn`.
///
/// The extension binary is vendored (checked into git under
/// `extensions/vendored/`, see `scripts/dev/adopt-extension-release.sh`) and
/// embedded at build time (`build.rs`) via `include_bytes!`; loading is
/// mandatory and a hard error on failure.
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

    load_dev_delta_extension(conn)?;

    Ok(())
}

/// Env var pointing at a locally-built `duckdb-delta` extension to `LOAD` after
/// the mandatory extension. Part of the cross-repo Delta dev loop; see
/// `docs/context/delta-cross-repo-dev-loop.md`.
const DELTA_EXT_PATH_ENV: &str = "THUNDERDUCK_DELTA_EXT_PATH";

/// Dev-only: if `THUNDERDUCK_DELTA_EXT_PATH` is set, `LOAD` that extension too.
///
/// This lets the cross-repo dev loop swap in a freshly built `duckdb-delta`
/// (against a custom `delta-kernel-rs`) with only a server restart — no
/// thunderduck recompile. Unset ⇒ no-op (production is unaffected). Set but
/// unloadable ⇒ hard error, because a developer who pointed at an extension
/// expects it to load and must be told when it does not.
fn load_dev_delta_extension(conn: &duckdb::Connection) -> Result<()> {
    load_optional_extension(conn, std::env::var_os(DELTA_EXT_PATH_ENV).as_deref())
}

/// Core of [`load_dev_delta_extension`], split out so it can be unit-tested with
/// an explicit path instead of a process-global env var (which would race under
/// the parallel test runner).
fn load_optional_extension(
    conn: &duckdb::Connection,
    path: Option<&std::ffi::OsStr>,
) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };

    let path_str = path.to_str().ok_or_else(|| {
        ThunderduckError::DuckDb(format!("{DELTA_EXT_PATH_ENV} is not valid UTF-8"))
    })?;

    conn.execute_batch(&format!("LOAD '{path_str}';"))
        .map_err(|e| {
            ThunderduckError::DuckDb(format!(
                "failed to load delta extension from {DELTA_EXT_PATH_ENV}={path_str}: {e}"
            ))
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler_v2::function_registry::ExtensionFunction;

    fn conn() -> duckdb::Connection {
        let config = duckdb::Config::default()
            .with("allow_unsigned_extensions", "true")
            .expect("config");
        duckdb::Connection::open_in_memory_with_flags(config).expect("open in-memory duckdb")
    }

    #[test]
    fn no_path_is_a_noop() {
        assert!(load_optional_extension(&conn(), None).is_ok());
    }

    #[test]
    fn every_emitted_extension_target_exists() {
        let conn = conn();
        load(&conn).expect("load thdck_spark_funcs");

        for target in ExtensionFunction::ALL {
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM duckdb_functions() WHERE function_name = ?",
                    [target.as_str()],
                    |row| row.get(0),
                )
                .expect("inspect loaded extension functions");
            assert!(count > 0, "missing extension target `{}`", target.as_str());
        }
    }

    /// Pins the shipped `spark_avg` aggregate's native DECIMAL result type,
    /// grouped and windowed. `emission.rs`'s `render_decimal_avg` wraps this
    /// call in an outer `CAST(... AS DECIMAL(pa,sa))` regardless (idempotent
    /// if this probe's observed native type already matches Spark's declared
    /// `(pa,sa)`), so a change in the extension's native type cannot silently
    /// alter the emitted decimal path.
    #[test]
    fn spark_avg_decimal_probe() {
        let conn = conn();
        load(&conn).expect("load thdck_spark_funcs");

        let grouped_type: String = conn
            .query_row(
                "SELECT typeof(spark_avg(x)) FROM (VALUES \
                 (CAST('123.45' AS DECIMAL(9,2))), \
                 (CAST('67.89' AS DECIMAL(9,2)))) AS t(x)",
                [],
                |r| r.get(0),
            )
            .expect("spark_avg(DECIMAL(9,2)) should execute");
        eprintln!("PROBE P1.1 grouped spark_avg(DECIMAL(9,2)) native type: {grouped_type}");
        // Lock the EXACT type, not just the DECIMAL family: emission's
        // `render_decimal_avg` casts to Spark's AvgLike type DECIMAL(13,6) for a
        // (9,2) arg, and its no-rounding-seam correctness relies on spark_avg
        // ALREADY returning exactly (13,6) so that outer CAST is a no-op. If a
        // future extension build widened the native scale, a silent HALF_UP
        // rounding step would appear — this exact-match assert catches that.
        assert_eq!(
            grouped_type, "DECIMAL(13,6)",
            "spark_avg(DECIMAL(9,2)) native type must equal Spark's AvgLike type \
             so the emission-side CAST stays a no-op; got: {grouped_type}"
        );

        let windowed_type: String = conn
            .query_row(
                "SELECT typeof(spark_avg(x) OVER (PARTITION BY k)) FROM (VALUES \
                 (1, CAST('123.45' AS DECIMAL(9,2))), \
                 (1, CAST('67.89' AS DECIMAL(9,2)))) AS t(k, x) LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("windowed spark_avg(DECIMAL(9,2)) should execute");
        eprintln!("PROBE P1.2 windowed spark_avg(DECIMAL(9,2)) native type: {windowed_type}");
        assert_eq!(
            windowed_type, "DECIMAL(13,6)",
            "windowed spark_avg(DECIMAL(9,2)) native type must equal Spark's AvgLike \
             type so the emission-side CAST stays a no-op; got: {windowed_type}"
        );
    }

    #[test]
    fn missing_extension_file_is_a_hard_error() {
        let err = load_optional_extension(
            &conn(),
            Some(std::ffi::OsStr::new("/nonexistent/delta.duckdb_extension")),
        )
        .expect_err("loading a nonexistent extension must fail");
        assert!(
            err.to_string().contains(DELTA_EXT_PATH_ENV),
            "error should name the env var, got: {err}"
        );
    }

    /// End-to-end gate for the cross-repo Delta dev loop: proves a locally-built
    /// `duckdb-delta` extension loads into thunderduck's *own* linked libduckdb
    /// (same v1.5.4 ABI) and that `delta_scan` runs. Exercises the real
    /// [`load`] path, so `THUNDERDUCK_DELTA_EXT_PATH` drives the dev hook.
    ///
    /// Ignored by default (needs the built extension + a Delta table). Run it
    /// from the dev loop, e.g.:
    /// ```text
    /// THUNDERDUCK_DELTA_EXT_PATH=.../delta.duckdb_extension \
    /// THUNDERDUCK_DELTA_TEST_TABLE=.delta-kernel-rs/kernel/tests/data/table-without-dv-small \
    ///   cargo test -p thunderduck-core -- --ignored delta_extension_loads_and_scans --nocapture
    /// ```
    #[test]
    #[ignore = "cross-repo dev loop: requires THUNDERDUCK_DELTA_EXT_PATH + THUNDERDUCK_DELTA_TEST_TABLE"]
    fn delta_extension_loads_and_scans() {
        assert!(
            std::env::var_os(DELTA_EXT_PATH_ENV).is_some(),
            "set {DELTA_EXT_PATH_ENV} to the built delta.duckdb_extension"
        );
        let table = std::env::var("THUNDERDUCK_DELTA_TEST_TABLE")
            .expect("set THUNDERDUCK_DELTA_TEST_TABLE to a Delta table directory");

        let conn = conn();
        // Full mandatory-load path: embeds thdck_spark_funcs AND, via the env
        // var above, LOADs the local delta extension through load_dev_delta_extension.
        load(&conn).expect("load thdck_spark_funcs + local delta extension");

        let rows: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM delta_scan('{table}')"),
                [],
                |r| r.get(0),
            )
            .expect("delta_scan should execute against the fixture table");
        assert!(
            rows > 0,
            "expected a non-empty Delta fixture, got {rows} rows"
        );
    }
}
