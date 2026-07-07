use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use duckdb::arrow::datatypes::{Field, Schema};
use duckdb::arrow::record_batch::RecordBatch;
use duckdb::core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId};
use duckdb::ffi::duckdb_string_t;
use duckdb::types::DuckString;
use duckdb::vscalar::{ScalarFunctionSignature, VScalar};
use duckdb::vtab::arrow::WritableVector;
use tokio::sync::{mpsc, oneshot};

use crate::error::{Result, ThunderduckError};
use crate::runtime::config::{HardwareProfile, StreamingConfig};
use crate::runtime::extension_loader;
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

// ── S3 credential chain ────────────────────────────────────────────────────────

/// Configure DuckDB for S3 access using the credential_chain provider when the
/// `THUNDERDUCK_S3_CREDENTIAL_CHAIN` environment variable is set to `"true"`.
///
/// The credential chain resolves AWS credentials from environment variables,
/// config files, and instance metadata — including IRSA (IAM Roles for Service
/// Accounts) on EKS via `AWS_ROLE_ARN` + `AWS_WEB_IDENTITY_TOKEN_FILE`. This
/// avoids the need for a wrapper entrypoint that calls STS and exports
/// temporary credentials.
///
/// Failures are non-fatal: the server logs a warning and continues without S3
/// access if `httpfs` / `aws` are unavailable. Workloads that don't touch S3
/// must still start.
/// Redirect DuckDB's extension install directory when
/// `THUNDERDUCK_DUCKDB_EXTENSION_DIR` is set.
///
/// DuckDB `INSTALL` writes to the shared per-user `~/.duckdb/extensions` cache
/// by default. When several git worktrees run tests concurrently on one machine
/// they would race on that shared cache. The test harness points this env var
/// at a per-worktree directory so each worktree owns its own extension cache.
/// Unset → DuckDB's default (`~/.duckdb`), so plain `cargo test` is unaffected.
/// (The mandatory `thdck_spark_funcs` extension is loaded from a per-process
/// temp path via `extension_loader` and does not consult this setting; only
/// `INSTALL` — e.g. the opt-in S3 `httpfs`/`aws` — does.)
fn configure_extension_directory(conn: &duckdb::Connection) {
    let Ok(dir) = std::env::var("THUNDERDUCK_DUCKDB_EXTENSION_DIR") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    let stmt = format!("SET extension_directory = '{}'", dir.replace('\'', "''"));
    if let Err(e) = conn.execute_batch(&stmt) {
        tracing::warn!("failed to set extension_directory to `{dir}`: {e}");
    } else {
        tracing::debug!("DuckDB extension_directory set to `{dir}`");
    }
}

fn configure_s3_credential_chain(conn: &duckdb::Connection, enabled: Option<String>) {
    let Some(value) = enabled else { return };
    if !value.eq_ignore_ascii_case("true") {
        return;
    }

    let setup = [
        "INSTALL httpfs",
        "LOAD httpfs",
        "INSTALL aws",
        "LOAD aws",
        "CREATE SECRET (TYPE S3, PROVIDER credential_chain)",
    ];

    for sql in setup {
        if let Err(e) = conn.execute_batch(sql) {
            tracing::warn!(
                "S3 credential_chain setup step `{sql}` failed: {e}. S3 reads may not work."
            );
            return;
        }
        tracing::debug!("S3 credential chain: {sql}");
    }
    tracing::info!("S3 credential_chain configured — AWS credentials resolved automatically");
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
    /// Check whether a view exists in the DuckDB catalog.
    ViewExists {
        view_name: String,
        resp: oneshot::Sender<bool>,
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

// ── json_strip_nulls UDF ───────────────────────────────────────────────────────

/// DuckDB scalar UDF that drops object keys whose value is JSON `null`,
/// recursively, matching Spark's `to_json` default
/// (`SQLConf.JSON_GENERATOR_IGNORE_NULL_FIELDS=true`).
///
/// DuckDB v1.5.1 has no native `json_strip_nulls`. τ wraps every `to_json(x)`
/// emission with `json_strip_nulls(to_json(x))` (see
/// `transpiler_v2/emission.rs`), and this UDF is registered at session
/// startup. Correctness contract:
///
/// - **Object**: entries whose value is JSON `null` are dropped at every
///   nesting depth (`{}` is preserved when all keys strip away).
/// - **Array**: `null` elements are kept as-is — Spark's `ignoreNullFields`
///   applies only to STRUCT fields, never to array elements.
/// - **Primitives** (string, number, boolean, `null`, JSON literal): returned
///   unchanged.
/// - **NULL row**: propagates as NULL.
/// - **Malformed JSON input**: returned unchanged (defensive; upstream is
///   always DuckDB's `to_json` output, so this branch is unreachable in
///   practice).
///
/// Field order is preserved via `serde_json`'s `preserve_order` feature
/// (backing map is `IndexMap`, not `BTreeMap`), so the output field order
/// matches DuckDB's `to_json(struct_pack(...))` insertion order — which in
/// turn matches Spark's struct field order. Pass 89: `json-005`.
struct JsonStripNulls;

impl VScalar for JsonStripNulls {
    type State = ();

    fn invoke(
        _: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let input_vec = input.flat_vector(0);
        // SAFETY: DuckDB guarantees the VARCHAR column's storage is
        // `duckdb_string_t` and the batch carries exactly `len` rows.
        let values = unsafe { input_vec.as_slice_with_len::<duckdb_string_t>(len) };
        let mut out_vec = output.flat_vector();
        for i in 0..len {
            if input_vec.row_is_null(i as u64) {
                out_vec.set_null(i);
                continue;
            }
            // `DuckString::as_str` borrows through the mutable `string_t`
            // descriptor; copy the descriptor to a local so we do not alias
            // the input slice.
            let mut str_t = values[i];
            let borrowed = DuckString::new(&mut str_t).as_str();
            let raw: &str = borrowed.as_ref();
            let stripped = match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(v) => strip_nulls(v).to_string(),
                // Defensive: upstream always emits DuckDB `to_json`, which is
                // valid JSON. If we ever receive something else, return it
                // untouched rather than fail the whole query.
                Err(_) => raw.to_owned(),
            };
            out_vec.insert(i, stripped.as_str());
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeHandle::from(LogicalTypeId::Varchar)],
            LogicalTypeHandle::from(LogicalTypeId::Varchar),
        )]
    }
}

/// Recursive JSON `null`-value key stripper. Public within the crate so the
/// UDF's semantics can be pinned by unit tests without spinning up DuckDB.
pub(crate) fn strip_nulls(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, val) in map {
                if val.is_null() {
                    continue;
                }
                out.insert(k, strip_nulls(val));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(strip_nulls).collect())
        }
        other => other,
    }
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
    /// applied, `thdck_spark_funcs` extension loaded).
    pub fn spawn(session_id: &str, _config: &StreamingConfig) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>(32);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<()>>(1);

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
-- json_strip_nulls(j) is registered by the Rust UDF below; see the
-- `JsonStripNulls` VScalar impl. Spark to_json defaults to
-- ignoreNullFields=true, and DuckDB v1.5.1 has no native equivalent. Pass 89.
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
CREATE OR REPLACE MACRO collect_set(x) AS LIST(DISTINCT x) FILTER (WHERE x IS NOT NULL);
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
-- array_size(arr): Spark returns NULL for NULL input; DuckDB LEN already
-- returns NULL for NULL. Spark's return type is Integer; the projection-slot
-- cast in `spark_return_cast` narrows LEN's BIGINT to INTEGER at the top of
-- the SELECT list. Corpus: arr2-004.
CREATE OR REPLACE MACRO array_size(arr) AS LEN(arr);
-- array_insert(arr, pos, val): Spark 1-based positional insert. For positive
-- `pos` this is `concat(prefix, [val], suffix)` where prefix = arr[1..pos-1]
-- and suffix = arr[pos..]. NULL array propagates NULL. This macro covers the
-- corpus witness (positive pos, in-range); Spark's full spec also supports
-- negative indices and out-of-range padding — not yet exercised by the corpus.
-- Corpus: arr2-002.
CREATE OR REPLACE MACRO array_insert(arr, pos, val) AS
    CASE WHEN arr IS NULL THEN NULL
         ELSE list_concat(list_slice(arr, 1, pos - 1), list_value(val), list_slice(arr, pos, len(arr)))
    END;
-- str_to_map(str, pair_delim, kv_delim): parse `k1<kv>v1<pair>k2<kv>v2` into
-- MAP(VARCHAR, VARCHAR). NULL input propagates. Corpus: map2-002.
CREATE OR REPLACE MACRO str_to_map(s, pair_delim, kv_delim) AS
    CASE WHEN s IS NULL THEN NULL
         ELSE map_from_entries(
             list_transform(
                 string_split(s, pair_delim),
                 pair -> {
                     'key':   split_part(pair, kv_delim, 1),
                     'value': split_part(pair, kv_delim, 2)
                 }
             )
         )
    END;
";
                if let Err(e) = conn.execute_batch(macro_sql) {
                    let _ = ready_tx.send(Err(ThunderduckError::DuckDb(format!(
                        "macro registration failed: {e}"
                    ))));
                    return;
                }
                // Register the `json_strip_nulls(VARCHAR) -> VARCHAR` scalar
                // UDF that powers Spark's `to_json` default
                // (`ignoreNullFields=true`). DuckDB v1.5.1's JSON extension
                // has no native equivalent; τ wraps every `to_json(x)`
                // emission with `json_strip_nulls(to_json(x))`. Pass 89:
                // `json-005`.
                if let Err(e) =
                    conn.register_scalar_function::<JsonStripNulls>("json_strip_nulls")
                {
                    let _ = ready_tx.send(Err(ThunderduckError::DuckDb(format!(
                        "json_strip_nulls UDF registration failed: {e}"
                    ))));
                    return;
                }
                // `spark_crc32` lives in its own `execute_batch` so the unit
                // tests can register just this fragment on a plain
                // `duckdb::Connection` without pulling in the mandatory
                // `thdck_spark_funcs` extension (extension load requires the
                // release-build binary; the macros are pure SQL and
                // self-contained). Corpus: hash-001.
                if let Err(e) = conn.execute_batch(SPARK_CRC32_MACRO_SQL) {
                    let _ = ready_tx.send(Err(ThunderduckError::DuckDb(format!(
                        "spark_crc32 macro registration failed: {e}"
                    ))));
                    return;
                }

                // Load the mandatory thdck_spark_funcs extension.
                if let Err(e) = extension_loader::load(&conn) {
                    let _ = ready_tx.send(Err(e));
                    return;
                }

                // Per-worktree extension cache (avoids cross-worktree races on
                // the shared ~/.duckdb when INSTALL runs). Must precede any
                // INSTALL below.
                configure_extension_directory(&conn);

                // Opt-in S3 credential_chain (IRSA-friendly auth on EKS).
                configure_s3_credential_chain(
                    &conn,
                    std::env::var("THUNDERDUCK_S3_CREDENTIAL_CHAIN").ok(),
                );

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
}

/// SQL fragment that registers the `_spark_crc32_table()` lookup-table macro
/// and the `spark_crc32(BLOB)` macro. Bit-exact `java.util.zip.CRC32`
/// emulation. Kept as a module-level constant so unit tests can register the
/// same SQL on a plain `duckdb::Connection` without spawning a full
/// `DuckDbSession` (which requires the `thdck_spark_funcs` extension binary
/// at runtime).
///
/// Corpus: `hash-001`. See `crates/core/src/transpiler_v2/emission.rs`
/// dispatch arm `"crc32" => "spark_crc32"` for the emission side.
pub(crate) const SPARK_CRC32_MACRO_SQL: &str = "
-- CRC-32-IEEE lookup table (poly 0xedb88320). 256 UINTEGER entries.
-- Zero-arg macro so callers reference by name; DuckDB inlines the list.
-- Values are decimal (DuckDB's SQL parser rejects `0x...` hex literals
-- inside a list initializer).
CREATE OR REPLACE MACRO _spark_crc32_table() AS [
    0::UINTEGER, 1996959894::UINTEGER, 3993919788::UINTEGER, 2567524794::UINTEGER,
    124634137::UINTEGER, 1886057615::UINTEGER, 3915621685::UINTEGER, 2657392035::UINTEGER,
    249268274::UINTEGER, 2044508324::UINTEGER, 3772115230::UINTEGER, 2547177864::UINTEGER,
    162941995::UINTEGER, 2125561021::UINTEGER, 3887607047::UINTEGER, 2428444049::UINTEGER,
    498536548::UINTEGER, 1789927666::UINTEGER, 4089016648::UINTEGER, 2227061214::UINTEGER,
    450548861::UINTEGER, 1843258603::UINTEGER, 4107580753::UINTEGER, 2211677639::UINTEGER,
    325883990::UINTEGER, 1684777152::UINTEGER, 4251122042::UINTEGER, 2321926636::UINTEGER,
    335633487::UINTEGER, 1661365465::UINTEGER, 4195302755::UINTEGER, 2366115317::UINTEGER,
    997073096::UINTEGER, 1281953886::UINTEGER, 3579855332::UINTEGER, 2724688242::UINTEGER,
    1006888145::UINTEGER, 1258607687::UINTEGER, 3524101629::UINTEGER, 2768942443::UINTEGER,
    901097722::UINTEGER, 1119000684::UINTEGER, 3686517206::UINTEGER, 2898065728::UINTEGER,
    853044451::UINTEGER, 1172266101::UINTEGER, 3705015759::UINTEGER, 2882616665::UINTEGER,
    651767980::UINTEGER, 1373503546::UINTEGER, 3369554304::UINTEGER, 3218104598::UINTEGER,
    565507253::UINTEGER, 1454621731::UINTEGER, 3485111705::UINTEGER, 3099436303::UINTEGER,
    671266974::UINTEGER, 1594198024::UINTEGER, 3322730930::UINTEGER, 2970347812::UINTEGER,
    795835527::UINTEGER, 1483230225::UINTEGER, 3244367275::UINTEGER, 3060149565::UINTEGER,
    1994146192::UINTEGER, 31158534::UINTEGER, 2563907772::UINTEGER, 4023717930::UINTEGER,
    1907459465::UINTEGER, 112637215::UINTEGER, 2680153253::UINTEGER, 3904427059::UINTEGER,
    2013776290::UINTEGER, 251722036::UINTEGER, 2517215374::UINTEGER, 3775830040::UINTEGER,
    2137656763::UINTEGER, 141376813::UINTEGER, 2439277719::UINTEGER, 3865271297::UINTEGER,
    1802195444::UINTEGER, 476864866::UINTEGER, 2238001368::UINTEGER, 4066508878::UINTEGER,
    1812370925::UINTEGER, 453092731::UINTEGER, 2181625025::UINTEGER, 4111451223::UINTEGER,
    1706088902::UINTEGER, 314042704::UINTEGER, 2344532202::UINTEGER, 4240017532::UINTEGER,
    1658658271::UINTEGER, 366619977::UINTEGER, 2362670323::UINTEGER, 4224994405::UINTEGER,
    1303535960::UINTEGER, 984961486::UINTEGER, 2747007092::UINTEGER, 3569037538::UINTEGER,
    1256170817::UINTEGER, 1037604311::UINTEGER, 2765210733::UINTEGER, 3554079995::UINTEGER,
    1131014506::UINTEGER, 879679996::UINTEGER, 2909243462::UINTEGER, 3663771856::UINTEGER,
    1141124467::UINTEGER, 855842277::UINTEGER, 2852801631::UINTEGER, 3708648649::UINTEGER,
    1342533948::UINTEGER, 654459306::UINTEGER, 3188396048::UINTEGER, 3373015174::UINTEGER,
    1466479909::UINTEGER, 544179635::UINTEGER, 3110523913::UINTEGER, 3462522015::UINTEGER,
    1591671054::UINTEGER, 702138776::UINTEGER, 2966460450::UINTEGER, 3352799412::UINTEGER,
    1504918807::UINTEGER, 783551873::UINTEGER, 3082640443::UINTEGER, 3233442989::UINTEGER,
    3988292384::UINTEGER, 2596254646::UINTEGER, 62317068::UINTEGER, 1957810842::UINTEGER,
    3939845945::UINTEGER, 2647816111::UINTEGER, 81470997::UINTEGER, 1943803523::UINTEGER,
    3814918930::UINTEGER, 2489596804::UINTEGER, 225274430::UINTEGER, 2053790376::UINTEGER,
    3826175755::UINTEGER, 2466906013::UINTEGER, 167816743::UINTEGER, 2097651377::UINTEGER,
    4027552580::UINTEGER, 2265490386::UINTEGER, 503444072::UINTEGER, 1762050814::UINTEGER,
    4150417245::UINTEGER, 2154129355::UINTEGER, 426522225::UINTEGER, 1852507879::UINTEGER,
    4275313526::UINTEGER, 2312317920::UINTEGER, 282753626::UINTEGER, 1742555852::UINTEGER,
    4189708143::UINTEGER, 2394877945::UINTEGER, 397917763::UINTEGER, 1622183637::UINTEGER,
    3604390888::UINTEGER, 2714866558::UINTEGER, 953729732::UINTEGER, 1340076626::UINTEGER,
    3518719985::UINTEGER, 2797360999::UINTEGER, 1068828381::UINTEGER, 1219638859::UINTEGER,
    3624741850::UINTEGER, 2936675148::UINTEGER, 906185462::UINTEGER, 1090812512::UINTEGER,
    3747672003::UINTEGER, 2825379669::UINTEGER, 829329135::UINTEGER, 1181335161::UINTEGER,
    3412177804::UINTEGER, 3160834842::UINTEGER, 628085408::UINTEGER, 1382605366::UINTEGER,
    3423369109::UINTEGER, 3138078467::UINTEGER, 570562233::UINTEGER, 1426400815::UINTEGER,
    3317316542::UINTEGER, 2998733608::UINTEGER, 733239954::UINTEGER, 1555261956::UINTEGER,
    3268935591::UINTEGER, 3050360625::UINTEGER, 752459403::UINTEGER, 1541320221::UINTEGER,
    2607071920::UINTEGER, 3965973030::UINTEGER, 1969922972::UINTEGER, 40735498::UINTEGER,
    2617837225::UINTEGER, 3943577151::UINTEGER, 1913087877::UINTEGER, 83908371::UINTEGER,
    2512341634::UINTEGER, 3803740692::UINTEGER, 2075208622::UINTEGER, 213261112::UINTEGER,
    2463272603::UINTEGER, 3855990285::UINTEGER, 2094854071::UINTEGER, 198958881::UINTEGER,
    2262029012::UINTEGER, 4057260610::UINTEGER, 1759359992::UINTEGER, 534414190::UINTEGER,
    2176718541::UINTEGER, 4139329115::UINTEGER, 1873836001::UINTEGER, 414664567::UINTEGER,
    2282248934::UINTEGER, 4279200368::UINTEGER, 1711684554::UINTEGER, 285281116::UINTEGER,
    2405801727::UINTEGER, 4167216745::UINTEGER, 1634467795::UINTEGER, 376229701::UINTEGER,
    2685067896::UINTEGER, 3608007406::UINTEGER, 1308918612::UINTEGER, 956543938::UINTEGER,
    2808555105::UINTEGER, 3495958263::UINTEGER, 1231636301::UINTEGER, 1047427035::UINTEGER,
    2932959818::UINTEGER, 3654703836::UINTEGER, 1088359270::UINTEGER, 936918000::UINTEGER,
    2847714899::UINTEGER, 3736837829::UINTEGER, 1202900863::UINTEGER, 817233897::UINTEGER,
    3183342108::UINTEGER, 3401237130::UINTEGER, 1404277552::UINTEGER, 615818150::UINTEGER,
    3134207493::UINTEGER, 3453421203::UINTEGER, 1423857449::UINTEGER, 601450431::UINTEGER,
    3009837614::UINTEGER, 3294710456::UINTEGER, 1567103746::UINTEGER, 711928724::UINTEGER,
    3020668471::UINTEGER, 3272380065::UINTEGER, 1510334235::UINTEGER, 755167117::UINTEGER
];
-- spark_crc32(b): Spark-compatible CRC-32-IEEE (java.util.zip.CRC32).
-- Signed BIGINT (Long) return, always non-negative (CRC-32 fits in 32 bits).
-- NULL input propagates. Algorithm: reflected input/output, poly 0xedb88320,
-- init/final XOR 0xFFFFFFFF (= 4294967295). Uses the 2-arg
-- `list_reduce(list, lambda)` form with `list_prepend(init, bytes)` so the
-- initial CRC register is folded in as the first list element (mirrors the
-- pattern already used for `aggregate` / `reduce` at emission.rs:2668).
CREATE OR REPLACE MACRO spark_crc32(b) AS
    CASE WHEN b IS NULL THEN NULL
         ELSE xor(
                  list_reduce(
                      list_prepend(
                          4294967295::UINTEGER,
                          -- Byte extraction: DuckDB lacks `get_byte(BLOB, i)`,
                          -- and our own `octet_length` macro shadows the
                          -- built-in with a VARCHAR-only definition (see the
                          -- macro at the top of `SPARK_MACRO_SQL`). Compute
                          -- byte count from `length(hex(b)) / 2` — `hex(b)`
                          -- returns exactly 2 chars per byte. Extract each
                          -- byte via `'0x' || <hh>` → INTEGER → UINTEGER.
                          -- Verified: `hex(b'test') = '74657374'`,
                          -- `CAST('0x74' AS INTEGER) = 116`.
                          list_transform(
                              range(0, (length(hex(b)) / 2)::INTEGER),
                              i -> CAST(
                                  ('0x' || substr(hex(b), i::INTEGER * 2 + 1, 2))
                                  AS INTEGER
                              )::UINTEGER
                          )
                      ),
                      (crc, byte) -> xor(
                          crc >> 8,
                          _spark_crc32_table()[
                              ((xor(crc, byte) & 255::UINTEGER)::INTEGER) + 1
                          ]
                      )
                  ),
                  4294967295::UINTEGER
              )::BIGINT
    END;
";

impl DuckDbSession {
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
            // ADR-006: re-clothe a DuckDB engine throw carrying a τ-emitted
            // Spark error-class token as a Spark-emulated runtime error.
            SessionResult::Error(e) => Err(e.reclassified_spark_runtime()),
            SessionResult::Schema(_) => unreachable!("Execute never returns Schema"),
        }
    }

    /// Execute a DDL statement (no rows expected back).
    pub async fn exec_ddl(&self, sql: &str) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::ExecDdl {
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
        let _ = self
            .cmd_tx
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

    /// Check whether a view exists in the DuckDB catalog.
    pub async fn view_exists(&self, name: &str) -> bool {
        let (resp_tx, resp_rx) = oneshot::channel();
        let sent = self
            .cmd_tx
            .send(SessionCommand::ViewExists {
                view_name: name.to_string(),
                resp: resp_tx,
            })
            .await;
        if sent.is_err() {
            return false;
        }
        resp_rx.await.unwrap_or(false)
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
            SessionCommand::CreateViewWithSchema {
                name,
                sql,
                schema,
                resp,
            } => {
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
            SessionCommand::ViewExists { view_name, resp } => {
                let exists = conn
                    .prepare("SELECT 1 FROM duckdb_views() WHERE view_name = ?")
                    .and_then(|mut stmt| {
                        let mut rows = stmt.query(duckdb::params![view_name])?;
                        Ok(rows.next()?.is_some())
                    })
                    .unwrap_or(false);
                let _ = resp.send(exists);
            }
            SessionCommand::ExecuteStreaming {
                sql,
                spark_names,
                batch_tx,
            } => {
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

#[cfg(test)]
mod tests {
    use super::configure_s3_credential_chain;
    use super::SPARK_CRC32_MACRO_SQL;

    fn fresh_conn() -> duckdb::Connection {
        duckdb::Connection::open_in_memory().expect("in-memory connection")
    }

    /// Open a plain in-memory DuckDB connection and register the CRC-32
    /// macros. Deliberately avoids `DuckDbSession::spawn` so the test does
    /// not depend on the `thdck_spark_funcs` extension binary (which is
    /// version-locked at build time). The macros are pure SQL — they only
    /// reference stdlib DuckDB primitives (`xor`, `list_reduce`,
    /// `list_prepend`, `list_transform`, `range`, `length`, `get_byte`).
    fn conn_with_crc32() -> duckdb::Connection {
        let conn = fresh_conn();
        conn.execute_batch(SPARK_CRC32_MACRO_SQL)
            .expect("register spark_crc32 macros");
        conn
    }

    /// Query a single BIGINT value.
    fn query_i64(conn: &duckdb::Connection, sql: &str) -> i64 {
        conn.query_row::<i64, _, _>(sql, [], |row| row.get(0))
            .expect("query_row failed")
    }

    /// ADR-006 Piece B1 contract: a runtime error thrown *during result
    /// materialisation* — DuckDB's `error()` fired by a row-dependent `CASE`,
    /// exactly how τ's ANSI divide/mod guard works — MUST surface as
    /// `Err(ThunderduckError::DuckDb(..))` carrying the emitted token, not be
    /// swallowed by `arrow_stream.collect()`.
    #[test]
    fn runtime_error_during_iteration_surfaces_as_err() {
        let conn = fresh_conn();
        // Two rows; the `a = 0` row triggers error() mid-stream.
        let sql = "SELECT CASE WHEN a = 0 THEN error('[DIVIDE_BY_ZERO] boom') ELSE a END \
                   FROM (VALUES (1), (0)) t(a)";
        let err = super::run_query(&conn, sql)
            .expect_err("row-dependent error() must surface as Err, not be swallowed");
        assert!(
            err.to_string().contains("DIVIDE_BY_ZERO"),
            "runtime error must carry the emitted token; got: {err}"
        );
    }

    /// hash-001 primary oracle: `spark_crc32` session-macro matches
    /// `java.util.zip.CRC32` bit-exactly. Verified against Python's
    /// `binascii.crc32(b'test') == 3632233996` and the Spark
    /// `ExpressionDescription` example `crc32('Spark') == 1557323817`.
    #[test]
    fn spark_crc32_matches_java_util_zip_crc32() {
        let conn = conn_with_crc32();
        assert_eq!(
            query_i64(&conn, "SELECT spark_crc32(CAST('test' AS BLOB))"),
            3_632_233_996,
            "spark_crc32(b'test') must equal java.util.zip.CRC32 of the same bytes",
        );
        assert_eq!(
            query_i64(&conn, "SELECT spark_crc32(CAST('Spark' AS BLOB))"),
            1_557_323_817,
            "spark_crc32(b'Spark') must match Spark's documented example",
        );
    }

    /// Empty BLOB: Spark's `crc32(cast('' as binary))` is 0.
    #[test]
    fn spark_crc32_empty_blob_is_zero() {
        let conn = conn_with_crc32();
        assert_eq!(
            query_i64(&conn, "SELECT spark_crc32(CAST('' AS BLOB))"),
            0,
            "spark_crc32(empty) must be 0",
        );
    }

    /// NULL-in / NULL-out contract for `spark_crc32`.
    #[test]
    fn spark_crc32_null_input_yields_null() {
        let conn = conn_with_crc32();
        let is_null: bool = conn
            .query_row(
                "SELECT spark_crc32(CAST(NULL AS BLOB)) IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("query_row failed");
        assert!(is_null, "spark_crc32(NULL) must be NULL");
    }

    /// Unset env var is a no-op — must not touch the connection.
    #[test]
    fn s3_credential_chain_disabled_when_none() {
        let conn = fresh_conn();
        configure_s3_credential_chain(&conn, None);
    }

    // ── Q3 (interval-transcode plan) — DuckDB's MonthDayNano layout ─────
    //
    // The interval-column Arrow transcoder in `crates/connect-server` maps
    // DuckDB's `Interval(MonthDayNano)` output to Spark's
    // `Duration(Microsecond)` wire encoding for `DayTimeInterval` columns.
    // The formula
    //     total_micros = days * 86_400_000_000 + nanoseconds / 1_000
    // is correct ONLY if DuckDB folds sub-day components into `nanoseconds`
    // and does NOT emit non-zero `months` for a pure DayTime result.
    // This test pins DuckDB's observed layout so a future upstream change
    // that shifts hours→days (or emits months for pure DayTime) trips loud
    // in unit tests, not in the corpus.

    /// `INTERVAL 1 DAY + INTERVAL 2 HOUR` → { months: 0, days: 1, nanos: 7.2e12 }.
    #[test]
    fn duckdb_month_day_nano_pure_day_time_layout() {
        use duckdb::arrow::array::IntervalMonthDayNanoArray;
        use duckdb::arrow::datatypes::{DataType as ArrowDt, IntervalUnit};

        let conn = fresh_conn();
        let mut stmt = conn
            .prepare("SELECT INTERVAL 1 DAY + INTERVAL 2 HOUR AS iv")
            .expect("prepare");
        let batches: Vec<duckdb::arrow::record_batch::RecordBatch> =
            stmt.query_arrow([]).expect("query").collect();
        assert_eq!(batches.len(), 1, "one batch expected");
        let batch = &batches[0];
        assert_eq!(
            batch.schema().field(0).data_type(),
            &ArrowDt::Interval(IntervalUnit::MonthDayNano),
            "DuckDB must emit Interval(MonthDayNano) for the INTERVAL type",
        );
        let arr = batch
            .column(0)
            .as_any()
            .downcast_ref::<IntervalMonthDayNanoArray>()
            .expect("MonthDayNano array");
        assert_eq!(arr.len(), 1);
        let v = arr.value(0);
        assert_eq!(v.months, 0, "pure DayTime must not accumulate into months");
        assert_eq!(v.days, 1, "1 day component preserved in `days`");
        assert_eq!(
            v.nanoseconds, 7_200_000_000_000,
            "2 hours = 7.2e12 nanos folded into `nanoseconds` (NOT into days)",
        );
    }

    /// `INTERVAL 90 DAYS` (Spark's `make_dt_interval(1, 2, 30, 0)` value —
    /// well, not exactly; that value is 1 d + 2h + 30 m — but corpus intv-004
    /// literally uses `INTERVAL 90 DAYS`, and this pins that DuckDB does NOT
    /// fold days into months at rate 30.
    #[test]
    fn duckdb_month_day_nano_ninety_days_stays_as_days() {
        use duckdb::arrow::array::IntervalMonthDayNanoArray;

        let conn = fresh_conn();
        let mut stmt = conn
            .prepare("SELECT INTERVAL 90 DAYS AS iv")
            .expect("prepare");
        let batches: Vec<duckdb::arrow::record_batch::RecordBatch> =
            stmt.query_arrow([]).expect("query").collect();
        let arr = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<IntervalMonthDayNanoArray>()
            .expect("MonthDayNano array");
        let v = arr.value(0);
        assert_eq!(
            v.months, 0,
            "90 days must NOT be folded into months (30d rate)"
        );
        assert_eq!(v.days, 90);
        assert_eq!(v.nanoseconds, 0);
    }

    /// `TIMESTAMP - TIMESTAMP` — corpus intv-005. DuckDB emits the difference
    /// as `Interval(MonthDayNano)` with `months = 0`. The transcoder maps this
    /// to `Duration(Microsecond)`; days/nanos come through unchanged.
    #[test]
    fn duckdb_month_day_nano_timestamp_diff_layout() {
        use duckdb::arrow::array::IntervalMonthDayNanoArray;

        let conn = fresh_conn();
        // 1 day, 2 hours, 3 minutes, 4 seconds, .5 seconds -> mixed sub-day.
        let mut stmt = conn
            .prepare(
                "SELECT TIMESTAMP '2024-01-02 02:03:04.5' - TIMESTAMP '2024-01-01 00:00:00' AS d",
            )
            .expect("prepare");
        let batches: Vec<duckdb::arrow::record_batch::RecordBatch> =
            stmt.query_arrow([]).expect("query").collect();
        let arr = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<IntervalMonthDayNanoArray>()
            .expect("MonthDayNano array");
        let v = arr.value(0);
        assert_eq!(v.months, 0);
        assert_eq!(v.days, 1);
        // 2h 3m 4.5s = (2*3600 + 3*60 + 4)*1e9 + 5e8 = 7_384_500_000_000 ns
        assert_eq!(v.nanoseconds, 7_384_500_000_000);
    }

    /// Empty string is treated as disabled.
    #[test]
    fn s3_credential_chain_disabled_when_empty() {
        let conn = fresh_conn();
        configure_s3_credential_chain(&conn, Some(String::new()));
    }

    /// Anything but case-insensitive "true" is disabled.
    #[test]
    fn s3_credential_chain_disabled_when_false() {
        let conn = fresh_conn();
        configure_s3_credential_chain(&conn, Some("false".into()));
        configure_s3_credential_chain(&conn, Some("0".into()));
        configure_s3_credential_chain(&conn, Some("yes".into()));
    }

    /// Recognised "true" values gracefully degrade when extensions / network
    /// are unavailable — the function logs a warning but must not panic.
    #[test]
    fn s3_credential_chain_enabled_does_not_panic() {
        let conn = fresh_conn();
        configure_s3_credential_chain(&conn, Some("true".into()));
        configure_s3_credential_chain(&conn, Some("TRUE".into()));
        configure_s3_credential_chain(&conn, Some("True".into()));
    }

    // ── json_strip_nulls UDF semantics (Pass 89, json-005) ─────────────

    use super::strip_nulls;

    fn strip_str(s: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(s).expect("test input must be valid JSON");
        strip_nulls(v).to_string()
    }

    /// Object entries with JSON-null values are dropped; the shape and the
    /// insertion order of the surviving entries are preserved (courtesy of
    /// serde_json `preserve_order`).
    #[test]
    fn json_strip_nulls_drops_null_valued_keys_and_preserves_order() {
        assert_eq!(strip_str(r#"{"a":1,"b":null,"c":2}"#), r#"{"a":1,"c":2}"#,);
        assert_eq!(strip_str(r#"{"a":null,"b":1}"#), r#"{"b":1}"#);
        assert_eq!(strip_str(r#"{"a":1,"b":null}"#), r#"{"a":1}"#);
    }

    /// A struct whose every field is null becomes `{}` — Spark keeps the
    /// empty-object container (`ignoreNullFields` drops entries with JSON
    /// null values, never containers). Corpus witness: json-005 Heidi row.
    #[test]
    fn json_strip_nulls_keeps_empty_object_when_all_keys_are_null() {
        assert_eq!(strip_str(r#"{"a":null,"b":null}"#), r#"{}"#);
        assert_eq!(
            strip_str(r#"{"outer":{"a":null,"b":null}}"#),
            r#"{"outer":{}}"#,
        );
    }

    /// Recursion covers every nesting depth in one call — nested struct in
    /// struct, struct in map, etc. All null-valued keys strip regardless of
    /// depth.
    #[test]
    fn json_strip_nulls_recurses_through_nested_objects() {
        assert_eq!(
            strip_str(r#"{"a":{"b":{"c":null,"d":1}}}"#),
            r#"{"a":{"b":{"d":1}}}"#,
        );
    }

    /// Array `null` elements are preserved — Spark's `ignoreNullFields`
    /// applies only to STRUCT / object fields, never to array elements.
    #[test]
    fn json_strip_nulls_keeps_null_array_elements() {
        assert_eq!(strip_str(r#"[1,null,2]"#), r#"[1,null,2]"#);
        assert_eq!(
            strip_str(r#"{"xs":[1,null,{"a":null,"b":2}]}"#),
            r#"{"xs":[1,null,{"b":2}]}"#,
        );
    }

    /// Regression against the earlier regex-based prototype: a string value
    /// that contains an escaped `"` followed by `:null` MUST NOT be
    /// corrupted. `serde_json`'s tokenizer treats the escape correctly, so
    /// the raw string content is preserved intact. Pass 89 review-fix pin.
    #[test]
    fn json_strip_nulls_preserves_string_values_with_embedded_quote_and_null_token() {
        // The JSON source literal is `{"raw":"foo\":null,bar","b":1}`. The
        // decoded string value is `foo":null,bar`. After stripping (no
        // top-level null-valued keys), the round-trip re-encodes the string
        // with the same escape sequence.
        let src = r#"{"raw":"foo\":null,bar","b":1}"#;
        assert_eq!(strip_str(src), src);
    }

    /// Primitives at the root pass through unchanged; the UDF only special-
    /// cases object entries and array descent.
    #[test]
    fn json_strip_nulls_returns_primitives_unchanged() {
        assert_eq!(strip_str("null"), "null");
        assert_eq!(strip_str("42"), "42");
        assert_eq!(strip_str(r#""hello""#), r#""hello""#);
        assert_eq!(strip_str("true"), "true");
    }
}
