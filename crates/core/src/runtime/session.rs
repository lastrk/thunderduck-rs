use std::sync::mpsc as std_mpsc;

use duckdb::arrow::record_batch::RecordBatch;
use tokio::sync::{mpsc, oneshot};

use crate::error::{Result, ThunderduckError};
use crate::runtime::compat_mode::{self, RuntimeCompatMode};
use crate::runtime::config::{HardwareProfile, StreamingConfig};

// ── Timezone detection ─────────────────────────────────────────────────────────

/// Detect the system local timezone string.
///
/// Resolution order:
/// 1. `TZ` environment variable
/// 2. `/etc/timezone` file (Linux)
/// 3. Fall back to `"UTC"`
fn detect_timezone() -> String {
    // 1. TZ env var
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim().to_string();
        if !tz.is_empty() {
            return tz;
        }
    }

    // 2. /etc/timezone (Linux/Debian-based)
    #[cfg(target_os = "linux")]
    if let Ok(contents) = std::fs::read_to_string("/etc/timezone") {
        let tz = contents.trim().trim_start_matches('/').to_string();
        if !tz.is_empty() {
            return tz;
        }
    }

    // 3. Fall back to UTC
    "UTC".to_string()
}

// ── Channel types ──────────────────────────────────────────────────────────────

pub(crate) enum SessionCommand {
    Execute {
        sql: String,
        resp: oneshot::Sender<SessionResult>,
    },
    CreateView {
        name: String,
        sql: String,
        resp: oneshot::Sender<SessionResult>,
    },
    Shutdown,
}

pub(crate) enum SessionResult {
    Batches(Vec<RecordBatch>),
    Ok,
    Error(ThunderduckError),
}

// ── DuckDbSession ──────────────────────────────────────────────────────────────

/// An async handle to a DuckDB session running on a dedicated OS thread.
///
/// `duckdb::Connection` is `!Send`, so it lives entirely on the session thread.
/// All communication goes through `tokio::sync::mpsc` + `oneshot` channels.
pub struct DuckDbSession {
    cmd_tx: mpsc::Sender<SessionCommand>,
}

impl DuckDbSession {
    /// Spawn a session thread with an in-memory DuckDB database and return a handle.
    ///
    /// This function blocks until the thread is ready (connection opened, settings
    /// applied, extension loaded if requested).
    pub fn spawn(
        session_id: &str,
        mode: RuntimeCompatMode,
        _config: &StreamingConfig,
    ) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>(32);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<()>>(1);

        let session_id = session_id.to_string();

        std::thread::Builder::new()
            .name(format!("duckdb-session-{session_id}"))
            .spawn(move || {
                let conn = match duckdb::Connection::open_in_memory() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ready_tx.send(Err(ThunderduckError::DuckDb(e.to_string())));
                        return;
                    }
                };

                // Apply hardware profile settings.
                let hw = HardwareProfile::detect();
                let timezone = detect_timezone();
                let init_sql = format!(
                    "SET threads = {threads};\
                     SET memory_limit = '{mem}GB';\
                     SET default_null_order = 'NULLS FIRST';\
                     SET TimeZone = '{tz}';\
                     SET enable_progress_bar = false;\
                     SET preserve_insertion_order = true;",
                    threads = hw.cpu_threads,
                    mem = hw.memory_limit_gb,
                    tz = timezone,
                );
                if let Err(e) = conn.execute_batch(&init_sql) {
                    let _ = ready_tx
                        .send(Err(ThunderduckError::DuckDb(format!("session init failed: {e}"))));
                    return;
                }

                // Enable jemalloc background threads on Linux with 8+ cores.
                // This allows background threads to handle memory purging without
                // blocking foreground operations.
                #[cfg(target_os = "linux")]
                if hw.cpu_threads >= 8 {
                    if let Err(e) = conn.execute_batch("SET allocator_background_threads = true;") {
                        let _ = ready_tx.send(Err(ThunderduckError::DuckDb(format!(
                            "jemalloc background threads init failed: {e}"
                        ))));
                        return;
                    }
                }

                // Register Spark-compatible SQL macros.
                // initcap: capitalize first letter of each whitespace-delimited word.
                // Spark treats only whitespace as word boundaries (not punctuation).
                let macro_sql = concat!(
                    "CREATE OR REPLACE MACRO initcap(s) AS ",
                    "regexp_replace(lower(s), '(^|\\s)(\\S)', '\\1' || upper('\\2'), 'g')",
                );
                if let Err(e) = conn.execute_batch(macro_sql) {
                    let _ = ready_tx.send(Err(ThunderduckError::DuckDb(format!(
                        "macro registration failed: {e}"
                    ))));
                    return;
                }

                // Resolve compat mode (loads extension if requested).
                if let Err(e) = compat_mode::resolve(mode, &conn) {
                    let _ = ready_tx.send(Err(e));
                    return;
                }

                // Signal ready.
                let _ = ready_tx.send(Ok(()));

                // Enter command loop.
                session_loop(conn, cmd_rx);
            })
            .map_err(|e| ThunderduckError::DuckDb(format!("failed to spawn session thread: {e}")))?;

        ready_rx
            .recv()
            .map_err(|_| ThunderduckError::DuckDb("session thread exited before ready".into()))??;

        Ok(DuckDbSession { cmd_tx })
    }

    /// Execute a SQL statement and collect all result Arrow batches.
    pub async fn execute(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::Execute {
                sql: sql.to_string(),
                resp: resp_tx,
            })
            .await
            .map_err(|_| ThunderduckError::DuckDb("session channel closed".into()))?;

        match resp_rx
            .await
            .map_err(|_| ThunderduckError::DuckDb("session thread died".into()))?
        {
            SessionResult::Batches(batches) => Ok(batches),
            SessionResult::Ok => Ok(vec![]),
            SessionResult::Error(e) => Err(e),
        }
    }

    /// Register a temporary view in this session.
    ///
    /// The view is created as `CREATE OR REPLACE TEMP VIEW <name> AS <sql>`.
    pub async fn create_temp_view(&self, name: &str, sql: &str) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::CreateView {
                name: name.to_string(),
                sql: sql.to_string(),
                resp: resp_tx,
            })
            .await
            .map_err(|_| ThunderduckError::DuckDb("session channel closed".into()))?;

        match resp_rx
            .await
            .map_err(|_| ThunderduckError::DuckDb("session thread died".into()))?
        {
            SessionResult::Ok => Ok(()),
            SessionResult::Error(e) => Err(e),
            SessionResult::Batches(_) => unreachable!("CreateView never returns batches"),
        }
    }
}

impl Drop for DuckDbSession {
    fn drop(&mut self) {
        // Best-effort shutdown — ignore send errors (thread may already be gone).
        let _ = self.cmd_tx.try_send(SessionCommand::Shutdown);
    }
}

// ── Session thread ─────────────────────────────────────────────────────────────

fn session_loop(conn: duckdb::Connection, mut rx: mpsc::Receiver<SessionCommand>) {
    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            SessionCommand::Execute { sql, resp } => {
                let result = run_query(&conn, &sql);
                let msg = match result {
                    Ok(batches) => SessionResult::Batches(batches),
                    Err(e) => SessionResult::Error(e),
                };
                let _ = resp.send(msg);
            }
            SessionCommand::CreateView { name, sql, resp } => {
                let ddl = format!(
                    "CREATE OR REPLACE TEMP VIEW \"{}\" AS {}",
                    name.replace('"', "\"\""),
                    sql
                );
                let result = conn
                    .execute_batch(&ddl)
                    .map_err(|e| ThunderduckError::DuckDb(e.to_string()));
                let msg = match result {
                    Ok(()) => SessionResult::Ok,
                    Err(e) => SessionResult::Error(e),
                };
                let _ = resp.send(msg);
            }
            SessionCommand::Shutdown => break,
        }
    }
    // conn drops here — DuckDB connection closed.
}

/// Run a SQL string against `conn` and return collected Arrow batches.
///
/// SELECT / WITH / VALUES → `query_arrow`
/// DDL / DML             → `execute_batch` (returns empty batch list)
fn run_query(conn: &duckdb::Connection, sql: &str) -> Result<Vec<RecordBatch>> {
    let upper = sql.trim_start().to_uppercase();
    let is_query = upper.starts_with("SELECT")
        || upper.starts_with("WITH")
        || upper.starts_with("VALUES")
        || upper.starts_with("FROM");

    if is_query {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| ThunderduckError::DuckDb(e.to_string()))?;
        let arrow_stream = stmt
            .query_arrow(duckdb::params![])
            .map_err(|e| ThunderduckError::DuckDb(e.to_string()))?;
        Ok(arrow_stream.collect())
    } else {
        conn.execute_batch(sql)
            .map_err(|e| ThunderduckError::DuckDb(e.to_string()))?;
        Ok(vec![])
    }
}
