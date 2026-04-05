use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use duckdb::arrow::datatypes::{Field, Schema};
use duckdb::arrow::record_batch::RecordBatch;
use tokio::sync::{mpsc, oneshot};

use crate::error::{Result, ThunderduckError};
use crate::functions::CompatMode;
use crate::runtime::compat_mode::{self, RuntimeCompatMode};
use crate::runtime::config::{HardwareProfile, StreamingConfig};
use crate::types::StructType;

/// A single item streamed from the DuckDB session thread during query execution.
#[derive(Debug)]
pub enum StreamBatch {
    /// An Arrow batch of results.
    Batch(RecordBatch),
    /// All batches sent successfully.
    Complete,
    /// An error occurred during execution.
    Error(String),
}

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
    /// Execute DDL (no result rows expected).
    ExecDdl {
        sql: String,
        resp: oneshot::Sender<SessionResult>,
    },
    /// Create a temp view and cache the Spark-declared schema.
    CreateViewWithSchema {
        name: String,
        sql: String,
        schema: StructType,
        resp: oneshot::Sender<SessionResult>,
    },
    /// Cache a Spark-declared schema for a temp view without re-creating it.
    /// Used when a view is created via SQL DDL (e.g. `CREATE TEMP VIEW AS SELECT ...`)
    /// and we want to preserve correct nullable metadata that DuckDB loses.
    CacheViewSchema {
        name: String,
        schema: StructType,
    },
    /// Retrieve a cached Spark schema for a temp view.
    GetViewSchema {
        name: String,
        resp: oneshot::Sender<Option<StructType>>,
    },
    /// Infer schema by preparing the SQL without executing it.
    SchemaOf {
        sql: String,
        resp: oneshot::Sender<SessionResult>,
    },
    /// Execute a query and stream results batch-by-batch via an mpsc channel.
    ExecuteStreaming {
        sql: String,
        spark_names: Option<Vec<String>>,
        batch_tx: mpsc::Sender<StreamBatch>,
    },
    Shutdown,
}

pub(crate) enum SessionResult {
    Batches(Vec<RecordBatch>),
    Schema(duckdb::arrow::datatypes::SchemaRef),
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
    resolved_mode: CompatMode,
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
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<CompatMode>>(1);

        let session_id = session_id.to_string();

        std::thread::Builder::new()
            .name(format!("duckdb-session-{session_id}"))
            .spawn(move || {
                let config = match duckdb::Config::default()
                    .with("allow_unsigned_extensions", "true")
                {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ready_tx.send(Err(ThunderduckError::DuckDb(format!(
                            "failed to build DuckDB config: {e}"
                        ))));
                        return;
                    }
                };
                let conn = match duckdb::Connection::open_in_memory_with_flags(config) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ready_tx.send(Err(ThunderduckError::DuckDb(e.to_string())));
                        return;
                    }
                };

                // Apply hardware profile settings.
                let hw = HardwareProfile::detect();
                let detected_tz = detect_timezone();
                let timezone = if detected_tz.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-+: ".contains(&b)) {
                    detected_tz
                } else {
                    "UTC".to_owned()
                }.replace('\'', "''");
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
                // These bridge Spark function names that differ from DuckDB equivalents.
                let macro_sql = "
-- DuckDB has INITCAP built-in; no macro needed (removed broken regex macro)
CREATE OR REPLACE MACRO size(x) AS len(x);
CREATE OR REPLACE MACRO startswith(s, prefix) AS starts_with(s, prefix);
CREATE OR REPLACE MACRO endswith(s, suffix) AS ends_with(s, suffix);
CREATE OR REPLACE MACRO get_json_object(j, p) AS json_extract_string(j, p);
CREATE OR REPLACE MACRO array_remove(arr, elem) AS
    list_filter(arr, x -> x IS DISTINCT FROM elem);
CREATE OR REPLACE MACRO array_compact(arr) AS
    list_filter(arr, x -> x IS NOT NULL);
CREATE OR REPLACE MACRO sequence(s, e, step := 1) AS generate_series(s, e, step);
-- cardinality(arr_or_map) → len works for both
CREATE OR REPLACE MACRO cardinality(x) AS len(x);
-- array_prepend(arr, elem) → DuckDB list_prepend has reversed arg order
CREATE OR REPLACE MACRO array_prepend(arr, elem) AS list_prepend(elem, arr);
-- btrim(str[, trimStr]) → TRIM BOTH
CREATE OR REPLACE MACRO btrim(s, t := NULL) AS
    CASE WHEN t IS NULL THEN TRIM(s) ELSE TRIM(BOTH t FROM s) END;
-- octet_length(str) → DuckDB octet_length only accepts BLOB; BIT_LENGTH works on VARCHAR
CREATE OR REPLACE MACRO octet_length(s) AS (BIT_LENGTH(s) / 8);
-- encode(str, charset) → binary representation (UTF-8 assumed)
CREATE OR REPLACE MACRO encode(s, charset := 'UTF-8') AS CAST(s AS BLOB);
-- decode(bytes, charset) → string (UTF-8 assumed)
CREATE OR REPLACE MACRO decode(b, charset := 'UTF-8') AS CAST(b AS VARCHAR);
-- isnull and nanvl are handled in the Rust function registry (isnull is a reserved word in DuckDB)
-- _spark_reverse(x): polymorphic — LIST_REVERSE for arrays, REVERSE for strings
-- Using underscore prefix to avoid shadowing DuckDB's built-in REVERSE
CREATE OR REPLACE MACRO _spark_reverse(x) AS
    CASE WHEN TYPEOF(x) LIKE '%]' THEN LIST_REVERSE(x) ELSE REVERSE(x) END;
-- array_except(a, b): first-occurrence elements in a not in b (order-preserving, deduplicated)
CREATE OR REPLACE MACRO array_except(a, b) AS
    list_filter(a, (v, i) -> list_position(a, v) = i AND NOT list_contains(b, v));
-- array_distinct(a): order-preserving deduplication
CREATE OR REPLACE MACRO array_distinct(a) AS
    list_filter(a, (v, i) -> list_position(a, v) = i);
-- array_union(a, b): concat then order-preserving dedup
CREATE OR REPLACE MACRO array_union(a, b) AS
    list_filter(list_concat(a, b), (v, i) -> list_position(list_concat(a, b), v) = i);
-- initcap(s): capitalize first letter of each space-separated word (DuckDB 1.5 lacks built-in INITCAP)
CREATE OR REPLACE MACRO initcap(s) AS
    array_to_string(
        list_transform(
            string_split(lower(s), ' '),
            w -> CASE WHEN len(w) = 0 THEN '' ELSE upper(left(w, 1)) || right(w, len(w) - 1) END
        ),
        ' '
    );
-- Spark bitwise / numeric functions not in DuckDB
CREATE OR REPLACE MACRO shiftleft(x, n) AS (x << n);
CREATE OR REPLACE MACRO shiftright(x, n) AS (x >> n);
CREATE OR REPLACE MACRO shiftrightunsigned(x, n) AS (x >> n);
CREATE OR REPLACE MACRO negative(x) AS (-x);
CREATE OR REPLACE MACRO positive(x) AS (x);
-- bit_get(x, pos): returns bit value (0 or 1) at position pos (0=LSB)
CREATE OR REPLACE MACRO bit_get(x, pos) AS ((x::BIGINT >> pos) & 1)::INT;
-- dayname/monthname: Spark returns 3-letter abbreviations; DuckDB built-ins return full names
CREATE OR REPLACE MACRO dayname(d) AS strftime('%a', d);
CREATE OR REPLACE MACRO monthname(d) AS strftime('%b', d);
-- forall: not implementable as a DuckDB macro (lambda params not supported); handled in Rust registry
-- Aggregate-compatible macros: collect_list / collect_set (used in spark.sql() path)
CREATE OR REPLACE MACRO collect_list(x) AS LIST(x) FILTER (WHERE x IS NOT NULL);
CREATE OR REPLACE MACRO collect_set(x) AS LIST(DISTINCT x);
-- substring_index(str, delim, cnt): first/last cnt delim-separated tokens
CREATE OR REPLACE MACRO substring_index(str, delim, cnt) AS
    CASE WHEN cnt > 0 THEN
        list_aggr(str_split(str, delim)[:cnt], 'string_agg', delim)
    WHEN cnt < 0 THEN
        list_aggr(str_split(str, delim)[cnt:], 'string_agg', delim)
    ELSE '' END;
-- format_number(x, d): format number with thousands separator and d decimal places
CREATE OR REPLACE MACRO format_number(x, d) AS printf('%,.' || CAST(d AS VARCHAR) || 'f', x);
-- to_char(x, fmt): format date/timestamp using Spark (Java) format strings
CREATE OR REPLACE MACRO to_char(x, fmt) AS
    strftime(
        replace(replace(replace(replace(replace(replace(replace(replace(
            fmt,
            'yyyy', '%Y'), 'YYYY', '%Y'),
            'MM', '%m'),
            'dd', '%d'), 'DD', '%d'),
            'HH', '%H'),
            'mm', '%M'),
            'ss', '%S'),
        x);
-- next_day(date, day_name): next occurrence of named weekday after date
CREATE OR REPLACE MACRO next_day(d, day_name) AS
    CAST(d AS DATE) + CAST(
        CASE lower(left(day_name, 3))
            WHEN 'sun' THEN ((0 - DAYOFWEEK(d) + 6) % 7) + 1
            WHEN 'mon' THEN ((1 - DAYOFWEEK(d) + 6) % 7) + 1
            WHEN 'tue' THEN ((2 - DAYOFWEEK(d) + 6) % 7) + 1
            WHEN 'wed' THEN ((3 - DAYOFWEEK(d) + 6) % 7) + 1
            WHEN 'thu' THEN ((4 - DAYOFWEEK(d) + 6) % 7) + 1
            WHEN 'fri' THEN ((5 - DAYOFWEEK(d) + 6) % 7) + 1
            WHEN 'sat' THEN ((6 - DAYOFWEEK(d) + 6) % 7) + 1
            ELSE 0
        END AS INTEGER) * INTERVAL 1 DAY;
-- _spark_size(x): returns size for arrays (LEN) or maps (LEN(MAP_KEYS(x)))
-- Used as fallback when type is unknown at code-gen time.
-- Note: this macro cannot work for maps because DuckDB macros type-check both CASE branches.
-- In practice, typed dispatch in translate_typed handles maps before reaching here.
CREATE OR REPLACE MACRO _spark_size(x) AS LEN(x);
-- map_from_arrays(keys, vals): Spark alias for DuckDB MAP(keys, vals)
CREATE OR REPLACE MACRO map_from_arrays(k, v) AS MAP(k, v);
-- map_from_entries(arr_of_structs): MAP from array of {key, value} structs
CREATE OR REPLACE MACRO map_from_entries(arr) AS MAP(list_transform(arr, s -> s.key), list_transform(arr, s -> s.value));
-- map_concat is already built-in to DuckDB 1.5; no macro needed
-- arrays_zip(a, b): zip two arrays into array of structs
CREATE OR REPLACE MACRO arrays_zip(a, b) AS list_zip(a, b);
-- pmod(x, y): positive modulo (Spark semantics)
CREATE OR REPLACE MACRO pmod(x, y) AS (((x % y) + y) % y);
-- rint(x): round to nearest even (Spark rounds to nearest integer)
CREATE OR REPLACE MACRO rint(x) AS round(x);
-- log1p(x): log(1+x)
CREATE OR REPLACE MACRO log1p(x) AS ln(1.0 + x);
-- log2(x): log base 2
CREATE OR REPLACE MACRO log2(x) AS log(x) / log(2.0);
-- cot(x): cotangent
CREATE OR REPLACE MACRO cot(x) AS cos(x) / sin(x);
-- degrees(x), radians(x): angle conversions
CREATE OR REPLACE MACRO degrees(x) AS x * 180.0 / pi();
CREATE OR REPLACE MACRO radians(x) AS x * pi() / 180.0;
-- unhex(s): Spark unhex returns BINARY; FROM_HEX returns BLOB in DuckDB
CREATE OR REPLACE MACRO unhex(s) AS FROM_HEX(s);
-- conv(n, from_base, to_base): number base conversion (simplified 10→16/16→10)
CREATE OR REPLACE MACRO conv(n, from_base, to_base) AS
    CASE
        WHEN from_base = 10 AND to_base = 16 THEN UPPER(HEX(CAST(n AS BIGINT)))
        WHEN from_base = 16 AND to_base = 10 THEN CAST(('0x' || CAST(n AS VARCHAR))::BIGINT AS VARCHAR)
        ELSE CAST(n AS VARCHAR)
    END;
-- soundex: Spark-compatible phonetic encoding
-- Algorithm: uppercase → remove H/W (pos 2+) → encode per code table →
--   dedup adjacent same codes → take first char + non-zero codes → pad/truncate to 4
CREATE OR REPLACE MACRO soundex(s) AS (
    left(
        left(upper(s), 1) || replace(substr(
            replace(replace(replace(replace(replace(replace(replace(
                translate(
                    left(upper(s), 1) || regexp_replace(substr(upper(s), 2), '[HW]', '', 'g'),
                    'AEIOUYHWBFPVCGJKQSXZDTLMNR',
                    '00000000111122222222334556'
                ),
                '00','0'), '11','1'), '22','2'), '33','3'), '44','4'), '55','5'), '66','6'
            ), 2), '0', ''
        ) || '000',
        4
    )
);

-- width_bucket(v, min, max, n): assign value to bucket
CREATE OR REPLACE MACRO width_bucket(v, mn, mx, n) AS
    GREATEST(0, LEAST(n + 1, FLOOR(n * (v - mn) / (mx - mn)) + 1)::INT);
";
                if let Err(e) = conn.execute_batch(macro_sql) {
                    let _ = ready_tx.send(Err(ThunderduckError::DuckDb(format!(
                        "macro registration failed: {e}"
                    ))));
                    return;
                }

                // Resolve compat mode (loads extension if requested).
                let resolved_mode = match compat_mode::resolve(mode, &conn) {
                    Ok(m) => m,
                    Err(e) => { let _ = ready_tx.send(Err(e)); return; }
                };

                // Signal ready with the resolved mode.
                let _ = ready_tx.send(Ok(resolved_mode));

                // Enter command loop.
                session_loop(conn, cmd_rx);
            })
            .map_err(|e| ThunderduckError::DuckDb(format!("failed to spawn session thread: {e}")))?;

        let resolved_mode = ready_rx
            .recv()
            .map_err(|_| ThunderduckError::DuckDb("session thread exited before ready".into()))??;

        Ok(DuckDbSession { cmd_tx, resolved_mode })
    }

    /// Return the resolved compat mode for this session (Strict or Relaxed).
    pub fn mode(&self) -> CompatMode {
        self.resolved_mode
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
            SessionResult::Schema(_) => unreachable!("Execute never returns Schema"),
        }
    }

    /// Execute a DDL statement (no rows expected back).
    pub async fn exec_ddl(&self, sql: &str) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::ExecDdl { sql: sql.to_string(), resp: resp_tx })
            .await
            .map_err(|_| ThunderduckError::DuckDb("session channel closed".into()))?;
        match resp_rx
            .await
            .map_err(|_| ThunderduckError::DuckDb("session thread died".into()))?
        {
            SessionResult::Ok => Ok(()),
            SessionResult::Error(e) => Err(e),
            _ => unreachable!("ExecDdl never returns batches or schema"),
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
            SessionResult::Batches(_) | SessionResult::Schema(_) => {
                unreachable!("CreateView never returns batches or schema")
            }
        }
    }

    /// Infer the Arrow schema of a SQL query by preparing (not executing) it.
    pub async fn schema_of(&self, sql: &str) -> Result<duckdb::arrow::datatypes::SchemaRef> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SchemaOf {
                sql: sql.to_string(),
                resp: resp_tx,
            })
            .await
            .map_err(|_| ThunderduckError::DuckDb("session channel closed".into()))?;

        match resp_rx
            .await
            .map_err(|_| ThunderduckError::DuckDb("session thread died".into()))?
        {
            SessionResult::Schema(schema) => Ok(schema),
            SessionResult::Error(e) => Err(e),
            _ => unreachable!("SchemaOf never returns batches or Ok"),
        }
    }

    /// Create a temp view and cache its Spark-declared schema.
    ///
    /// The cached schema preserves nullable flags from the original
    /// `createDataFrame(data, schema)` call, which DuckDB's `CREATE VIEW` loses.
    pub async fn create_temp_view_with_schema(
        &self,
        name: &str,
        sql: &str,
        schema: StructType,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::CreateViewWithSchema {
                name: name.to_string(),
                sql: sql.to_string(),
                schema,
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
            _ => unreachable!("CreateViewWithSchema never returns batches or schema"),
        }
    }

    /// Cache a Spark-accurate schema for a temp view created via SQL DDL.
    ///
    /// DuckDB views lose NOT NULL metadata for struct fields and literal
    /// expressions. This method stores the plan-inferred schema so that
    /// subsequent `get_view_schema` calls return correct nullability.
    pub async fn cache_view_schema(&self, name: &str, schema: StructType) {
        let _ = self.cmd_tx
            .send(SessionCommand::CacheViewSchema {
                name: name.to_string(),
                schema,
            })
            .await;
    }

    /// Execute a SQL query, streaming results batch-by-batch.
    ///
    /// Returns a receiver that yields `StreamBatch` items. The session thread
    /// iterates DuckDB's Arrow result lazily, applying column renames from
    /// `spark_names` if provided. Backpressure is achieved via the bounded
    /// channel: the session thread blocks when the buffer is full.
    pub async fn execute_streaming(
        &self,
        sql: &str,
        spark_names: Option<Vec<String>>,
        buffer: usize,
    ) -> Result<mpsc::Receiver<StreamBatch>> {
        let (tx, rx) = mpsc::channel(buffer);
        self.cmd_tx
            .send(SessionCommand::ExecuteStreaming {
                sql: sql.to_string(),
                spark_names,
                batch_tx: tx,
            })
            .await
            .map_err(|_| ThunderduckError::DuckDb("session channel closed".into()))?;
        Ok(rx)
    }

    /// Retrieve the cached Spark schema for a temp view.
    ///
    /// Returns `None` if no cached schema exists (e.g. the view was created
    /// via raw DDL rather than `createOrReplaceTempView`).
    pub async fn get_view_schema(&self, name: &str) -> Option<StructType> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::GetViewSchema {
                name: name.to_string(),
                resp: resp_tx,
            })
            .await
            .ok()?;
        resp_rx.await.ok().flatten()
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
    let mut view_schemas: HashMap<String, StructType> = HashMap::new();

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
            SessionCommand::ExecDdl { sql, resp } => {
                let result = conn
                    .execute_batch(&sql)
                    .map_err(|e| ThunderduckError::DuckDb(e.to_string()));
                let msg = match result {
                    Ok(()) => SessionResult::Ok,
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
            SessionCommand::CreateViewWithSchema { name, sql, schema, resp } => {
                let ddl = format!(
                    "CREATE OR REPLACE TEMP VIEW \"{}\" AS {}",
                    name.replace('"', "\"\""),
                    sql
                );
                let result = conn
                    .execute_batch(&ddl)
                    .map_err(|e| ThunderduckError::DuckDb(e.to_string()));
                let msg = match result {
                    Ok(()) => {
                        view_schemas.insert(name.to_lowercase(), schema);
                        SessionResult::Ok
                    }
                    Err(e) => SessionResult::Error(e),
                };
                let _ = resp.send(msg);
            }
            SessionCommand::CacheViewSchema { name, schema } => {
                view_schemas.insert(name.to_lowercase(), schema);
            }
            SessionCommand::GetViewSchema { name, resp } => {
                let schema = view_schemas.get(&name.to_lowercase()).cloned();
                let _ = resp.send(schema);
            }
            SessionCommand::ExecuteStreaming { sql, spark_names, batch_tx } => {
                let result = (|| -> std::result::Result<(), duckdb::Error> {
                    let mut stmt = conn.prepare(&sql)?;
                    let arrow = stmt.query_arrow(duckdb::params![])?;
                    let duckdb_schema: Arc<Schema> = arrow.get_schema();

                    // Build rename schema if needed (computed once).
                    let rename_schema = spark_names.as_ref().and_then(|names| {
                        let duck_fields = duckdb_schema.fields();
                        if names.len() == duck_fields.len() {
                            let needs_rename = names
                                .iter()
                                .zip(duck_fields.iter())
                                .any(|(n, f)| n.as_str() != f.name());
                            if needs_rename {
                                let fields: Vec<Field> = duck_fields
                                    .iter()
                                    .zip(names.iter())
                                    .map(|(f, name)| {
                                        Field::new(
                                            name.as_str(),
                                            f.data_type().clone(),
                                            f.is_nullable(),
                                        )
                                    })
                                    .collect();
                                Some(Arc::new(Schema::new(fields)))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    });

                    // Save schema for the empty-result case.
                    let schema = if let Some(ref s) = rename_schema {
                        Arc::clone(s)
                    } else {
                        duckdb_schema
                    };

                    let mut sent_any = false;
                    for batch in arrow {
                        sent_any = true;
                        let batch = if let Some(ref rs) = rename_schema {
                            RecordBatch::try_new(Arc::clone(rs), batch.columns().to_vec())
                                .unwrap_or(batch)
                        } else {
                            batch
                        };
                        if batch_tx.blocking_send(StreamBatch::Batch(batch)).is_err() {
                            // Receiver dropped — client cancelled.
                            return Ok(());
                        }
                    }

                    // If no batches were produced, send an empty schema-only batch
                    // so PySpark can build a table from the schema.
                    if !sent_any {
                        let empty = RecordBatch::new_empty(schema);
                        if batch_tx.blocking_send(StreamBatch::Batch(empty)).is_err() {
                            return Ok(());
                        }
                    }

                    Ok(())
                })();

                match result {
                    Ok(()) => {
                        let _ = batch_tx.blocking_send(StreamBatch::Complete);
                    }
                    Err(e) => {
                        let _ = batch_tx.blocking_send(StreamBatch::Error(e.to_string()));
                    }
                }
            }
            SessionCommand::SchemaOf { sql, resp } => {
                // Infer the output schema without reading any rows.
                //
                // For SELECT/WITH/VALUES/(subquery) statements we avoid the old
                // `SELECT * FROM ({sql}) __probe__ LIMIT 0` wrapping.  That subquery
                // wrapping causes DuckDB to deduplicate duplicate column names
                // (appending `_1`, `_2`, etc.), but Spark allows and preserves them.
                //
                // Instead we strip any trailing `LIMIT <n>` from the SQL and replace
                // it with `LIMIT 0`, keeping the query flat so duplicate column names
                // survive into the Arrow schema.
                //
                // For bare table references (e.g. `"my_table"`) we keep the
                // `SELECT * FROM {sql} LIMIT 0` form since a bare name is not valid
                // standalone SQL.
                let trimmed = sql.trim_start();
                let is_complete_statement = trimmed.starts_with('(')
                    || (trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("SELECT"))
                    || (trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("WITH"))
                    || (trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("VALUES"));
                let probe = if is_complete_statement {
                    // Strip trailing LIMIT clause (if any) and replace with LIMIT 0.
                    // The regex-like approach: find last occurrence of LIMIT <digits>
                    // at the end of the SQL (after trimming whitespace).
                    let stripped = sql.trim_end();
                    // Try to find and replace a trailing LIMIT <n>
                    if let Some(pos) = find_trailing_limit(stripped) {
                        format!("{} LIMIT 0", &stripped[..pos])
                    } else {
                        format!("{stripped} LIMIT 0")
                    }
                } else {
                    format!("SELECT * FROM {sql} LIMIT 0")
                };
                let msg = match conn.prepare(&probe) {
                    Ok(mut stmt) => match stmt.query_arrow(duckdb::params![]) {
                        Ok(arrow) => SessionResult::Schema(arrow.get_schema()),
                        Err(e) => SessionResult::Error(ThunderduckError::DuckDb(e.to_string())),
                    },
                    Err(e) => SessionResult::Error(ThunderduckError::DuckDb(e.to_string())),
                };
                let _ = resp.send(msg);
            }
            SessionCommand::Shutdown => break,
        }
    }
    // conn drops here — DuckDB connection closed.
}

/// Find the byte position just before a trailing `LIMIT <digits>` clause in SQL.
///
/// Returns `Some(pos)` where `sql[..pos]` is everything before the LIMIT keyword,
/// or `None` if no trailing LIMIT is found.
///
/// This intentionally only matches the very end of the string to avoid stripping
/// LIMIT clauses inside subqueries.
fn find_trailing_limit(sql: &str) -> Option<usize> {
    // Walk backwards: skip trailing whitespace, then digits, then whitespace, then "LIMIT" (case-insensitive).
    let bytes = sql.as_bytes();
    let mut i = bytes.len();

    // Skip trailing whitespace
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // Must end with digits
    let end = i;
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == end {
        return None; // no digits at end
    }
    // Skip whitespace between LIMIT and digits
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    // Check for "LIMIT" keyword (case-insensitive)
    if i >= 5 && bytes[i - 5..i].eq_ignore_ascii_case(b"LIMIT") {
        Some(i - 5)
    } else {
        None
    }
}

/// Run a SQL string against `conn` and return collected Arrow batches.
///
/// SELECT / WITH / VALUES → `query_arrow`
/// DDL / DML             → `execute_batch` (returns empty batch list)
fn run_query(conn: &duckdb::Connection, sql: &str) -> Result<Vec<RecordBatch>> {
    let trimmed = sql.trim_start();
    let is_query = trimmed.starts_with('(')
        || (trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("SELECT"))
        || (trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("WITH"))
        || (trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("VALUES"))
        || (trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("FROM"))
        || (trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("UNPIVOT"));

    if is_query {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| ThunderduckError::DuckDb(e.to_string()))?;
        let arrow_stream = stmt
            .query_arrow(duckdb::params![])
            .map_err(|e| ThunderduckError::DuckDb(e.to_string()))?;
        // Save schema before consuming the stream; needed when there are 0 rows.
        let schema = arrow_stream.get_schema();
        let batches: Vec<RecordBatch> = arrow_stream.collect();
        if batches.is_empty() {
            // Return a schema-only empty batch so PySpark can build a table from
            // the schema even when 0 rows are returned.
            Ok(vec![RecordBatch::new_empty(schema)])
        } else {
            Ok(batches)
        }
    } else {
        conn.execute_batch(sql)
            .map_err(|e| ThunderduckError::DuckDb(e.to_string()))?;
        Ok(vec![])
    }
}
