use std::collections::HashMap;
use std::sync::LazyLock;

use crate::types::DataType;

/// Compatibility mode — affects which function implementations are selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatMode {
    /// Use vanilla DuckDB functions (~85% Spark parity).
    Relaxed,
    /// Use thdck_spark_funcs extension for exact Spark semantics (~100% parity).
    Strict,
}

impl Default for CompatMode {
    fn default() -> Self {
        CompatMode::Relaxed
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

type CustomFn = fn(args: &[&str], mode: CompatMode) -> String;

static REGISTRY: LazyLock<FunctionRegistry> = LazyLock::new(FunctionRegistry::build);

pub struct FunctionRegistry {
    /// Simple 1:1 name mappings.
    direct: HashMap<&'static str, &'static str>,
    /// Complex mappings with custom translation logic.
    custom: HashMap<&'static str, CustomFn>,
    /// Names that need a DuckDB SQL macro defined at session startup.
    macros: Vec<(&'static str, &'static str)>,
}

impl FunctionRegistry {
    /// Translate a Spark function name + serialised arg SQL strings to a
    /// DuckDB SQL fragment.
    ///
    /// **IMPORTANT**: `args` must already be SQL-rendered strings
    /// (i.e., each `arg.to_sql(gen)` result), not raw values.
    pub fn translate(spark_name: &str, args: &[&str], mode: CompatMode) -> String {
        let r = &*REGISTRY;
        let lower = spark_name.to_lowercase();

        // 1. Custom translator takes priority
        if let Some(f) = r.custom.get(lower.as_str()) {
            return f(args, mode);
        }

        // 2. Direct mapping
        if let Some(duckdb_name) = r.direct.get(lower.as_str()) {
            return format!("{}({})", duckdb_name, args.join(", "));
        }

        // 3. Pass-through: emit as-is (function name unchanged)
        format!("{}({})", spark_name, args.join(", "))
    }

    /// Translate a Spark function name to DuckDB SQL, dispatching polymorphic
    /// functions based on the inferred argument types.
    ///
    /// When `arg_types` is non-empty and the first argument type is known
    /// (i.e. not `DataType::Unresolved`), this method selects the correct
    /// DuckDB equivalent for overloaded Spark functions like `reverse`,
    /// `size`, and `sort_array`. All other functions fall through to
    /// [`translate`][Self::translate].
    pub fn translate_typed(
        spark_name: &str,
        args: &[&str],
        arg_types: &[DataType],
        mode: CompatMode,
    ) -> String {
        let first_type = arg_types.first().unwrap_or(&DataType::Unresolved);

        let arg0 = args.first().copied().unwrap_or("");
        match spark_name.to_lowercase().as_str() {
            "reverse" => {
                if matches!(first_type, DataType::Array(_)) {
                    return format!("LIST_REVERSE({arg0})");
                }
            }
            "size" => {
                if matches!(first_type, DataType::String) {
                    return format!("LENGTH({arg0})");
                }
            }
            "sort_array" => {
                if matches!(first_type, DataType::Array(_)) {
                    return format!("LIST_SORT({arg0})");
                }
            }
            _ => {}
        }

        Self::translate(spark_name, args, mode)
    }

    /// Check if a function name is explicitly mapped (direct or custom).
    pub fn is_mapped(spark_name: &str) -> bool {
        let r = &*REGISTRY;
        let lower = spark_name.to_lowercase();
        r.direct.contains_key(lower.as_str()) || r.custom.contains_key(lower.as_str())
    }

    /// Return the list of SQL macros that must be registered at session startup.
    pub fn session_macros() -> &'static [(&'static str, &'static str)] {
        &REGISTRY.macros
    }

    // ── Builder ───────────────────────────────────────────────────────────────

    fn build() -> Self {
        let mut direct: HashMap<&'static str, &'static str> = HashMap::new();
        let mut custom: HashMap<&'static str, CustomFn> = HashMap::new();

        // ── String functions ──────────────────────────────────────────────────
        let string_direct: &[(&str, &str)] = &[
            ("upper", "UPPER"),
            ("lower", "LOWER"),
            ("length", "LENGTH"),
            ("char_length", "LENGTH"),
            ("character_length", "LENGTH"),
            ("octet_length", "OCTET_LENGTH"),
            ("bit_length", "BIT_LENGTH"),
            ("trim", "TRIM"),
            ("ltrim", "LTRIM"),
            ("rtrim", "RTRIM"),
            ("lpad", "LPAD"),
            ("rpad", "RPAD"),
            ("repeat", "REPEAT"),
            ("reverse", "REVERSE"),
            ("concat", "CONCAT"),
            ("concat_ws", "CONCAT_WS"),
            ("replace", "REPLACE"),
            ("translate", "TRANSLATE"),
            ("ascii", "ASCII"),
            ("chr", "CHR"),
            ("char", "CHR"),
            ("hex", "HEX"),
            ("unhex", "UNHEX"),
            ("base64", "BASE64"),
            ("unbase64", "DECODE"),
            ("encode", "ENCODE"),
            ("decode", "DECODE"),
            ("left", "LEFT"),
            ("right", "RIGHT"),
            ("md5", "MD5"),
            ("sha", "SHA256"),
            ("sha1", "SHA256"),
            ("sha2", "SHA256"),
            ("levenshtein", "LEVENSHTEIN"),
            ("url_decode", "URL_DECODE"),
            ("url_encode", "URL_ENCODE"),
            ("uuid", "GEN_RANDOM_UUID"),
            ("printf", "PRINTF"),
            ("format_string", "PRINTF"),
            ("sentences", "REGEXP_SPLIT_TO_ARRAY"),
            ("luhn_check", "LUHN_CHECK"),
        ];
        for (s, d) in string_direct {
            direct.insert(s, d);
        }

        // ── Math functions ─────────────────────────────────────────────────────
        let math_direct: &[(&str, &str)] = &[
            ("abs", "ABS"),
            ("ceil", "CEIL"),
            ("ceiling", "CEIL"),
            ("floor", "FLOOR"),
            ("round", "ROUND"),
            ("sqrt", "SQRT"),
            ("cbrt", "CBRT"),
            ("exp", "EXP"),
            ("pow", "POW"),
            ("power", "POW"),
            ("ln", "LN"),
            ("log2", "LOG2"),
            ("log10", "LOG10"),
            ("sin", "SIN"),
            ("cos", "COS"),
            ("tan", "TAN"),
            ("asin", "ASIN"),
            ("acos", "ACOS"),
            ("atan", "ATAN"),
            ("atan2", "ATAN2"),
            ("sinh", "SINH"),
            ("cosh", "COSH"),
            ("tanh", "TANH"),
            ("asinh", "ASINH"),
            ("acosh", "ACOSH"),
            ("atanh", "ATANH"),
            ("degrees", "DEGREES"),
            ("radians", "RADIANS"),
            ("sign", "SIGN"),
            ("signum", "SIGN"),
            ("hypot", "HYPOT"),
            ("greatest", "GREATEST"),
            ("least", "LEAST"),
            ("pi", "PI"),
            ("e", "E"),
            ("factorial", "FACTORIAL"),
            ("expm1", "EXPM1"),
            ("log1p", "LOG1P"),
            ("bround", "ROUND"),
            ("width_bucket", "WIDTH_BUCKET"),
        ];
        for (s, d) in math_direct {
            direct.insert(s, d);
        }

        // ── Date/Time functions ────────────────────────────────────────────────
        let date_direct: &[(&str, &str)] = &[
            ("year", "YEAR"),
            ("month", "MONTH"),
            ("day", "DAY"),
            ("dayofmonth", "DAY"),
            ("dayofyear", "DAYOFYEAR"),
            ("weekofyear", "WEEKOFYEAR"),
            ("quarter", "QUARTER"),
            ("hour", "HOUR"),
            ("minute", "MINUTE"),
            ("second", "SECOND"),
            ("last_day", "LAST_DAY"),
            ("date_trunc", "DATE_TRUNC"),
            ("now", "NOW"),
            ("current_timestamp", "CURRENT_TIMESTAMP"),
            ("current_date", "CURRENT_DATE"),
            ("curdate", "CURRENT_DATE"),
            ("make_date", "MAKE_DATE"),
            ("make_timestamp", "MAKE_TIMESTAMP"),
            ("to_days", "DATEDIFF"),
            ("trunc", "DATE_TRUNC"),
        ];
        for (s, d) in date_direct {
            direct.insert(s, d);
        }

        // ── Aggregate functions ────────────────────────────────────────────────
        let agg_direct: &[(&str, &str)] = &[
            ("min", "MIN"),
            ("max", "MAX"),
            ("avg", "AVG"),
            ("mean", "AVG"),
            ("stddev", "STDDEV_SAMP"),
            ("stddev_samp", "STDDEV_SAMP"),
            ("std", "STDDEV_SAMP"),
            ("stddev_pop", "STDDEV_POP"),
            ("variance", "VAR_SAMP"),
            ("var_samp", "VAR_SAMP"),
            ("var_pop", "VAR_POP"),
            ("kurtosis", "KURTOSIS"),
            ("skewness", "SKEWNESS"),
            ("approx_count_distinct", "APPROX_COUNT_DISTINCT"),
            ("percentile_approx", "PERCENTILE_CONT"),
            ("approx_percentile", "PERCENTILE_CONT"),
            ("first", "FIRST"),
            ("last", "LAST"),
            ("bool_and", "BOOL_AND"),
            ("every", "BOOL_AND"),
            ("bool_or", "BOOL_OR"),
            ("any_value", "ANY_VALUE"),
            ("bit_and", "BIT_AND"),
            ("bit_or", "BIT_OR"),
            ("bit_xor", "BIT_XOR"),
        ];
        for (s, d) in agg_direct {
            direct.insert(s, d);
        }

        // ── Window functions ────────────────────────────────────────────────────
        let window_direct: &[(&str, &str)] = &[
            ("row_number", "ROW_NUMBER"),
            ("rank", "RANK"),
            ("dense_rank", "DENSE_RANK"),
            ("percent_rank", "PERCENT_RANK"),
            ("cume_dist", "CUME_DIST"),
            ("ntile", "NTILE"),
            ("lag", "LAG"),
            ("lead", "LEAD"),
            ("first_value", "FIRST_VALUE"),
            ("last_value", "LAST_VALUE"),
            ("nth_value", "NTH_VALUE"),
        ];
        for (s, d) in window_direct {
            direct.insert(s, d);
        }

        // ── Array / List functions ──────────────────────────────────────────────
        // array_compact and array_union require lambdas — handled in custom below.
        let array_direct: &[(&str, &str)] = &[
            ("array_contains", "LIST_CONTAINS"),
            ("array_distinct", "LIST_DISTINCT"),
            ("array_sort", "LIST_SORT"),
            ("sort_array", "LIST_SORT"),
            ("array_max", "LIST_MAX"),
            ("array_min", "LIST_MIN"),
            ("array_reverse", "LIST_REVERSE"),
            ("flatten", "FLATTEN"),
            ("array_intersect", "LIST_INTERSECT"),
            ("array_except", "LIST_EXCEPT"),
            ("arrays_overlap", "LIST_HAS_ANY"),
            ("array_prepend", "LIST_PREPEND"),
            ("array_append", "LIST_APPEND"),
            ("map_keys", "MAP_KEYS"),
            ("map_values", "MAP_VALUES"),
            ("map_entries", "MAP_ENTRIES"),
            ("map_from_arrays", "MAP_FROM_ARRAYS"),
            ("map_concat", "MAP_CONCAT"),
        ];
        for (s, d) in array_direct {
            direct.insert(s, d);
        }

        // ── Conditional ────────────────────────────────────────────────────────
        // greatest/least are already in math_direct; not duplicated here.
        let cond_direct: &[(&str, &str)] = &[
            ("coalesce", "COALESCE"),
            ("nullif", "NULLIF"),
            ("ifnull", "IFNULL"),
            ("nvl", "COALESCE"),
        ];
        for (s, d) in cond_direct {
            direct.insert(s, d);
        }

        // ── JSON ────────────────────────────────────────────────────────────────
        let json_direct: &[(&str, &str)] = &[
            ("to_json", "TO_JSON"),
            ("json_array_length", "JSON_ARRAY_LENGTH"),
            ("json_object_keys", "JSON_KEYS"),
        ];
        for (s, d) in json_direct {
            direct.insert(s, d);
        }

        // ── Misc ────────────────────────────────────────────────────────────────
        let misc_direct: &[(&str, &str)] = &[
            ("rand", "RANDOM"),
            ("random", "RANDOM"),
            ("hash", "HASH"),
            ("xxhash64", "HASH"),
            ("typeof", "TYPEOF"),
            ("current_user", "CURRENT_USER"),
            ("current_schema", "CURRENT_SCHEMA"),
            ("current_database", "CURRENT_DATABASE"),
            ("version", "VERSION"),
            ("monotonically_increasing_id", "ROWID"),
            ("isnull", "ISNULL"),
            ("isnotnull", "ISNOTNULL"),
            ("isnan", "ISNAN"),
            ("isinf", "ISINF"),
            ("nanvl", "NANVL"),
        ];
        for (s, d) in misc_direct {
            direct.insert(s, d);
        }

        // ── Custom translators ─────────────────────────────────────────────────

        // count(*) and count(distinct x)
        custom.insert("count", |args, _mode| {
            if args.is_empty() || args[0] == "*" {
                "COUNT(*)".to_string()
            } else {
                format!("COUNT({})", args.join(", "))
            }
        });

        // sum — emit CAST(SUM(x) AS BIGINT) for integer types (avoid HUGEINT)
        // Note: type-aware routing is done in the generator; here we just emit the call
        custom.insert("sum", |args, _mode| {
            format!("SUM({})", args.join(", "))
        });

        // count_distinct → COUNT(DISTINCT ...)
        custom.insert("count_distinct", |args, _mode| {
            format!("COUNT(DISTINCT {})", args.join(", "))
        });

        // sum_distinct → SUM(DISTINCT ...)
        custom.insert("sum_distinct", |args, _mode| {
            format!("SUM(DISTINCT {})", args.join(", "))
        });

        // collect_list / collect_set
        custom.insert("collect_list", |args, _mode| format!("LIST({})", args[0]));
        custom.insert("collect_set", |args, _mode| {
            format!("LIST_DISTINCT(LIST({}))", args[0])
        });

        // percentile(col, pct) → PERCENTILE_CONT(pct) WITHIN GROUP (ORDER BY col)
        custom.insert("percentile", |args, _mode| {
            if args.len() >= 2 {
                format!("PERCENTILE_CONT({}) WITHIN GROUP (ORDER BY {})", args[1], args[0])
            } else {
                format!("PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY {})", args.first().copied().unwrap_or(""))
            }
        });

        // substring(str, pos) or substring(str, pos, len) → SUBSTR
        custom.insert("substring", |args, _mode| format!("SUBSTR({})", args.join(", ")));
        custom.insert("substr", |args, _mode| format!("SUBSTR({})", args.join(", ")));

        // locate(substr, str[, pos]) → INSTR with arg swap, or STRPOS
        custom.insert("locate", |args, _mode| {
            match args.len() {
                0 | 1 => "0".to_string(),
                2 => format!("INSTR({}, {})", args[1], args[0]),
                _ => format!("INSTR({}, {}, {})", args[1], args[0], args[2]),
            }
        });

        // instr(str, substr) → INSTR(str, substr) — same signature, direct
        custom.insert("instr", |args, _mode| format!("INSTR({})", args.join(", ")));

        // regexp_replace(str, pattern, replacement) → REGEXP_REPLACE with 'g' flag
        custom.insert("regexp_replace", |args, _mode| {
            match args.len() {
                3 => format!("REGEXP_REPLACE({}, {}, {}, 'g')", args[0], args[1], args[2]),
                _ => format!("REGEXP_REPLACE({})", args.join(", ")),
            }
        });

        // regexp_extract(str, pattern, idx) → REGEXP_EXTRACT
        custom.insert("regexp_extract", |args, _mode| {
            match args.len() {
                2 => format!("REGEXP_EXTRACT({}, {})", args[0], args[1]),
                3 => format!("REGEXP_EXTRACT({}, {}, {})", args[0], args[1], args[2]),
                _ => format!("REGEXP_EXTRACT({})", args.join(", ")),
            }
        });

        // split(str, pattern[, limit]) → STR_SPLIT_REGEX
        custom.insert("split", |args, _mode| {
            match args.len() {
                2 => format!("STR_SPLIT_REGEX({}, {})", args[0], args[1]),
                3 => format!("STR_SPLIT_REGEX({}, {})[1:{}]", args[0], args[1], args[2]),
                _ => format!("STR_SPLIT_REGEX({})", args.join(", ")),
            }
        });

        // overlay(str placing repl from start for len)
        custom.insert("overlay", |args, _mode| {
            match args.len() {
                3 => format!("OVERLAY({} PLACING {} FROM {})", args[0], args[1], args[2]),
                4 => format!("OVERLAY({} PLACING {} FROM {} FOR {})", args[0], args[1], args[2], args[3]),
                _ => format!("OVERLAY({})", args.join(", ")),
            }
        });

        // space(n) → REPEAT(' ', n)
        custom.insert("space", |args, _mode| {
            format!("REPEAT(' ', {})", args.first().copied().unwrap_or("0"))
        });

        // format_number(x, d) → PRINTF('%.{d}f', x) — approximation
        custom.insert("format_number", |args, _mode| {
            if args.len() >= 2 {
                format!("PRINTF('%.' || {} || 'f', {})", args[1], args[0])
            } else {
                format!("PRINTF('%f', {})", args.first().copied().unwrap_or(""))
            }
        });

        // log(base, x) or log(x) → LOG(x) or LOG(base, x)
        custom.insert("log", |args, _mode| {
            match args.len() {
                1 => format!("LN({})", args[0]),
                2 => format!("LOG({}, {})", args[0], args[1]),
                _ => "LN(1)".to_string(),
            }
        });

        // rand(seed) → SETSEED then RANDOM, or just RANDOM()
        custom.insert("rand", |args, _mode| {
            if args.is_empty() {
                "RANDOM()".to_string()
            } else {
                format!("(SETSEED({}) + RANDOM())", args[0])
            }
        });

        // randn([seed]) → random normal — use box-muller approximation
        custom.insert("randn", |args, _mode| {
            if args.is_empty() {
                "SQRT(-2 * LN(RANDOM())) * COS(2 * PI() * RANDOM())".to_string()
            } else {
                format!("(SETSEED({}) + SQRT(-2 * LN(RANDOM())) * COS(2 * PI() * RANDOM()))", args[0])
            }
        });

        // pmod(x, y) → ((x % y) + y) % y
        custom.insert("pmod", |args, _mode| {
            if args.len() >= 2 {
                format!("(({} % {}) + {}) % {}", args[0], args[1], args[1], args[1])
            } else {
                "0".to_string()
            }
        });

        // mod(x, y) → x % y
        custom.insert("mod", |args, _mode| {
            if args.len() >= 2 {
                format!("({} % {})", args[0], args[1])
            } else {
                "0".to_string()
            }
        });

        // date_add(date, days) → date + INTERVAL days DAY
        custom.insert("date_add", |args, _mode| {
            if args.len() >= 2 {
                format!("({} + INTERVAL ({}) DAY)", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("").to_string()
            }
        });

        // date_sub(date, days) → date - INTERVAL days DAY
        custom.insert("date_sub", |args, _mode| {
            if args.len() >= 2 {
                format!("({} - INTERVAL ({}) DAY)", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("").to_string()
            }
        });

        // datediff(end, start) → DATE_DIFF('day', start, end)
        custom.insert("datediff", |args, _mode| {
            if args.len() >= 2 {
                format!("DATE_DIFF('day', CAST({} AS DATE), CAST({} AS DATE))", args[1], args[0])
            } else {
                "0".to_string()
            }
        });

        // add_months(date, n) → date + INTERVAL n MONTH
        custom.insert("add_months", |args, _mode| {
            if args.len() >= 2 {
                format!("({} + INTERVAL ({}) MONTH)", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("").to_string()
            }
        });

        // months_between(end, start) → DATEDIFF in months (fractional)
        custom.insert("months_between", |args, _mode| {
            if args.len() >= 2 {
                format!(
                    "(DATE_DIFF('day', CAST({} AS DATE), CAST({} AS DATE)) / 30.0)",
                    args[1], args[0]
                )
            } else {
                "0".to_string()
            }
        });

        // to_date(str[, fmt]) → STRPTIME or CAST
        custom.insert("to_date", |args, _mode| {
            match args.len() {
                1 => format!("CAST({} AS DATE)", args[0]),
                2 => {
                    let fmt = convert_spark_date_format(args[1]);
                    format!("STRPTIME({}, '{}')", args[0], fmt)
                }
                _ => format!("CAST({} AS DATE)", args.first().copied().unwrap_or("")),
            }
        });

        // to_timestamp(str[, fmt]) → STRPTIME or CAST
        custom.insert("to_timestamp", |args, _mode| {
            match args.len() {
                1 => format!("CAST({} AS TIMESTAMP)", args[0]),
                2 => {
                    let fmt = convert_spark_date_format(args[1]);
                    format!("STRPTIME({}, '{}')", args[0], fmt)
                }
                _ => format!("CAST({} AS TIMESTAMP)", args.first().copied().unwrap_or("")),
            }
        });

        // unix_timestamp([str[, fmt]]) → EPOCH / EPOCH_MS
        custom.insert("unix_timestamp", |args, _mode| {
            match args.len() {
                0 => "EPOCH(NOW())".to_string(),
                1 => format!("EPOCH(CAST({} AS TIMESTAMP))", args[0]),
                2 => {
                    let fmt = convert_spark_date_format(args[1]);
                    format!("EPOCH(STRPTIME({}, '{}'))", args[0], fmt)
                }
                _ => "EPOCH(NOW())".to_string(),
            }
        });

        // from_unixtime(epoch[, fmt]) → strftime
        custom.insert("from_unixtime", |args, _mode| {
            if args.is_empty() {
                return "''".to_string();
            }
            let ts = format!("TO_TIMESTAMP({})", args[0]);
            if args.len() >= 2 {
                let fmt = convert_spark_date_format(args[1]);
                format!("STRFTIME({}, '{}')", ts, fmt)
            } else {
                format!("STRFTIME({}, '%Y-%m-%d %H:%M:%S')", ts)
            }
        });

        // date_format(date, fmt) → STRFTIME
        custom.insert("date_format", |args, _mode| {
            if args.len() >= 2 {
                let fmt = convert_spark_date_format(args[1]);
                format!("STRFTIME(CAST({} AS TIMESTAMP), '{}')", args[0], fmt)
            } else {
                args.first().copied().unwrap_or("").to_string()
            }
        });

        // dayofweek(date): Spark returns 1=Sun..7=Sat, DuckDB DAYOFWEEK is 0=Sun..6=Sat
        custom.insert("dayofweek", |args, _mode| {
            format!("(DAYOFWEEK({}) + 1)", args.first().copied().unwrap_or(""))
        });

        // weekday(date): Spark 0=Mon..6=Sun, DuckDB 0=Mon..6=Sun — same!
        custom.insert("weekday", |args, _mode| {
            format!("WEEKDAY({})", args.first().copied().unwrap_or(""))
        });

        // next_day(date, dayOfWeek)
        custom.insert("next_day", |args, _mode| {
            if args.len() >= 2 {
                format!("NEXT_DAY({}, {})", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("NULL").to_string()
            }
        });

        // size(array_or_map) → LEN
        custom.insert("size", |args, _mode| {
            format!("LEN({})", args.first().copied().unwrap_or(""))
        });
        custom.insert("array_size", |args, _mode| {
            format!("LEN({})", args.first().copied().unwrap_or(""))
        });
        custom.insert("cardinality", |args, _mode| {
            format!("CARDINALITY({})", args.first().copied().unwrap_or(""))
        });

        // element_at(arr, idx): Spark 1-indexed, negative from end
        // DuckDB list[idx] is 1-indexed, so direct
        custom.insert("element_at", |args, _mode| {
            if args.len() >= 2 {
                format!("{}[{}]", args[0], args[1])
            } else {
                "NULL".to_string()
            }
        });

        // slice(arr, start, length): DuckDB LIST_SLICE(arr, start, start+length-1)
        custom.insert("slice", |args, _mode| {
            if args.len() >= 3 {
                format!("LIST_SLICE({}, {}, {} + {} - 1)", args[0], args[1], args[1], args[2])
            } else if args.len() == 2 {
                format!("LIST_SLICE({}, {})", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("NULL").to_string()
            }
        });

        // array_position(arr, elem): DuckDB LIST_POSITION (1-indexed), Spark 1-indexed
        custom.insert("array_position", |args, _mode| {
            if args.len() >= 2 {
                format!("LIST_POSITION({}, {})", args[0], args[1])
            } else {
                "0".to_string()
            }
        });

        // array_remove(arr, elem) → LIST_FILTER(arr, x -> x <> elem)
        custom.insert("array_remove", |args, _mode| {
            if args.len() >= 2 {
                format!("LIST_FILTER({}, x -> x <> {})", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("NULL").to_string()
            }
        });

        // array_compact(arr) → LIST_FILTER(arr, x -> x IS NOT NULL)
        custom.insert("array_compact", |args, _mode| {
            format!("LIST_FILTER({}, x -> x IS NOT NULL)", args.first().copied().unwrap_or(""))
        });

        // array_union(a, b) → LIST_DISTINCT(LIST_CONCAT(a, b))
        custom.insert("array_union", |args, _mode| {
            if args.len() >= 2 {
                format!("LIST_DISTINCT(LIST_CONCAT({}, {}))", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("NULL").to_string()
            }
        });

        // concat for arrays → LIST_CONCAT
        custom.insert("array_concat", |args, _mode| {
            format!("LIST_CONCAT({})", args.join(", "))
        });

        // array_join(arr, delimiter[, nullReplacement]) → ARRAY_TO_STRING
        custom.insert("array_join", |args, _mode| {
            match args.len() {
                2 => format!("ARRAY_TO_STRING({}, {})", args[0], args[1]),
                3 => format!("ARRAY_TO_STRING(LIST_TRANSFORM({}, x -> COALESCE(CAST(x AS VARCHAR), {})), {})", args[0], args[2], args[1]),
                _ => format!("ARRAY_TO_STRING({}, ',')", args.first().copied().unwrap_or("")),
            }
        });

        // transform(arr, lambda_expr) → LIST_TRANSFORM
        custom.insert("transform", |args, _mode| {
            format!("LIST_TRANSFORM({})", args.join(", "))
        });

        // filter(arr, lambda_expr) → LIST_FILTER
        custom.insert("filter", |args, _mode| {
            format!("LIST_FILTER({})", args.join(", "))
        });

        // aggregate(arr, zero, merge[, finish]) → LIST_AGGREGATE equivalent
        custom.insert("aggregate", |args, _mode| {
            // DuckDB doesn't have a direct equivalent; use list_aggregate for simple cases
            format!("LIST_AGGREGATE({}, 'sum')", args.first().copied().unwrap_or(""))
        });

        // exists(arr, lambda) → LIST_ANY_VALUE workaround
        custom.insert("exists", |args, _mode| {
            format!("(LIST_FILTER({}) <> [])", args.join(", "))
        });

        // forall(arr, pred) → list_bool_and(list_transform(arr, pred))
        // args[1] is already-rendered lambda SQL, e.g. "x -> x > 0"
        custom.insert("forall", |args, _mode| {
            if args.len() >= 2 {
                format!("list_bool_and(list_transform({}, {}))", args[0], args[1])
            } else {
                "FALSE".to_string()
            }
        });

        // explode(arr) → UNNEST
        custom.insert("explode", |args, _mode| {
            format!("UNNEST({})", args.first().copied().unwrap_or(""))
        });
        custom.insert("explode_outer", |args, _mode| {
            format!("UNNEST({})", args.first().copied().unwrap_or(""))
        });
        custom.insert("posexplode", |args, _mode| {
            format!("UNNEST({})", args.first().copied().unwrap_or(""))
        });
        custom.insert("posexplode_outer", |args, _mode| {
            format!("UNNEST({})", args.first().copied().unwrap_or(""))
        });
        custom.insert("inline", |args, _mode| {
            format!("UNNEST({})", args.first().copied().unwrap_or(""))
        });

        // create_map / map: DuckDB MAP syntax is different
        custom.insert("create_map", |args, _mode| {
            // Spark: create_map(k1, v1, k2, v2, ...) → MAP {k1: v1, k2: v2, ...}
            if args.len() % 2 == 0 {
                let pairs: Vec<String> = args
                    .chunks(2)
                    .map(|c| format!("{}: {}", c[0], c[1]))
                    .collect();
                format!("MAP {{{}}}", pairs.join(", "))
            } else {
                "MAP {}".to_string()
            }
        });
        custom.insert("map", |args, _mode| {
            if args.is_empty() {
                return "MAP {}".to_string();
            }
            if args.len() % 2 == 0 {
                let pairs: Vec<String> = args
                    .chunks(2)
                    .map(|c| format!("{}: {}", c[0], c[1]))
                    .collect();
                format!("MAP {{{}}}", pairs.join(", "))
            } else {
                "MAP {}".to_string()
            }
        });

        // map_filter, map_transform_values — DuckDB has these
        custom.insert("map_filter", |args, _mode| {
            format!("MAP_FILTER({})", args.join(", "))
        });
        custom.insert("map_transform_values", |args, _mode| {
            format!("MAP_APPLY({})", args.join(", "))
        });

        // nvl2(expr, ifNotNull, ifNull) → CASE WHEN expr IS NOT NULL THEN ifNotNull ELSE ifNull END
        custom.insert("nvl2", |args, _mode| {
            if args.len() >= 3 {
                format!("CASE WHEN {} IS NOT NULL THEN {} ELSE {} END", args[0], args[1], args[2])
            } else {
                "NULL".to_string()
            }
        });

        // if(cond, thenVal, elseVal) → CASE WHEN
        custom.insert("if", |args, _mode| {
            if args.len() >= 3 {
                format!("CASE WHEN {} THEN {} ELSE {} END", args[0], args[1], args[2])
            } else if args.len() == 2 {
                format!("CASE WHEN {} THEN {} END", args[0], args[1])
            } else {
                "NULL".to_string()
            }
        });
        custom.insert("iff", |args, _mode| {
            if args.len() >= 3 {
                format!("IF({}, {}, {})", args[0], args[1], args[2])
            } else {
                "NULL".to_string()
            }
        });

        // get_json_object(json, '$.field') → JSON_EXTRACT_STRING
        custom.insert("get_json_object", |args, _mode| {
            if args.len() >= 2 {
                format!("JSON_EXTRACT_STRING({}, {})", args[0], args[1])
            } else {
                args.first().copied().unwrap_or("NULL").to_string()
            }
        });

        // schema_of_json(json) → returns a schema string
        custom.insert("schema_of_json", |_args, _mode| {
            "'string'".to_string() // simplified — exact type depends on data
        });

        // from_json(col, schema) — DuckDB equivalent varies; simplified
        custom.insert("from_json", |args, _mode| {
            format!("JSON({})", args.first().copied().unwrap_or(""))
        });

        // json_tuple(json, field1, field2, ...) → multiple JSON_EXTRACT_STRING
        custom.insert("json_tuple", |args, _mode| {
            if args.is_empty() {
                return "NULL".to_string();
            }
            let json = args[0];
            let fields: Vec<String> = args[1..]
                .iter()
                .map(|f| format!("JSON_EXTRACT_STRING({json}, '$.{f}')"))
                .collect();
            fields.join(", ")
        });

        // named_struct(name1, val1, ...) → STRUCT_PACK or literal struct
        custom.insert("named_struct", |args, _mode| {
            if args.len() % 2 == 0 {
                let fields: Vec<String> = args
                    .chunks(2)
                    .map(|c| format!("{} := {}", c[0].trim_matches('\''), c[1]))
                    .collect();
                format!("STRUCT_PACK({})", fields.join(", "))
            } else {
                "NULL".to_string()
            }
        });

        custom.insert("struct", |args, _mode| {
            format!("ROW({})", args.join(", "))
        });

        // to_number / to_char — format as string
        custom.insert("to_number", |args, _mode| {
            if args.len() >= 2 {
                format!("CAST({} AS DECIMAL)", args[0])
            } else {
                format!("CAST({} AS DECIMAL)", args.first().copied().unwrap_or(""))
            }
        });
        custom.insert("to_char", |args, _mode| {
            if args.len() >= 1 {
                format!("CAST({} AS VARCHAR)", args[0])
            } else {
                "''".to_string()
            }
        });

        // assert_true
        custom.insert("assert_true", |args, _mode| {
            format!("CASE WHEN {} THEN TRUE ELSE ERROR('assertion failed') END", args.first().copied().unwrap_or("FALSE"))
        });

        // raise_error
        custom.insert("raise_error", |args, _mode| {
            format!("ERROR({})", args.first().copied().unwrap_or("'error'"))
        });

        // crc32 → CRC32
        custom.insert("crc32", |args, _mode| {
            format!("CRC32({})", args.first().copied().unwrap_or(""))
        });

        // substring_index(str, delim, count)
        custom.insert("substring_index", |args, _mode| {
            if args.len() >= 3 {
                // Spark: positive count from left, negative from right
                format!(
                    "CASE WHEN {} >= 0 THEN ARRAY_TO_STRING(STR_SPLIT({}, {})[1:{}], {}) ELSE ARRAY_TO_STRING(STR_SPLIT({}, {})[-({}):], {}) END",
                    args[2], args[0], args[1], args[2], args[1],
                    args[0], args[1], args[2], args[1]
                )
            } else {
                args.first().copied().unwrap_or("").to_string()
            }
        });

        // conv(num, fromBase, toBase) — DuckDB doesn't have this natively
        custom.insert("conv", |args, _mode| {
            if args.len() >= 3 {
                // Only support common cases (from 10 or 16 to 16 or 10)
                format!("CONV({}, {}, {})", args[0], args[1], args[2])
            } else {
                args.first().copied().unwrap_or("NULL").to_string()
            }
        });

        // initcap — registered as a DuckDB macro
        custom.insert("initcap", |args, _mode| {
            format!("INITCAP({})", args.first().copied().unwrap_or(""))
        });

        // soundex — complex Spark algorithm, approximate with DuckDB macro
        custom.insert("soundex", |args, _mode| {
            format!("SOUNDEX({})", args.first().copied().unwrap_or(""))
        });

        // shiftleft / shiftright
        custom.insert("shiftleft", |args, _mode| {
            if args.len() >= 2 { format!("({} << {})", args[0], args[1]) } else { "0".to_string() }
        });
        custom.insert("shiftright", |args, _mode| {
            if args.len() >= 2 { format!("({} >> {})", args[0], args[1]) } else { "0".to_string() }
        });
        custom.insert("shiftrightunsigned", |args, _mode| {
            if args.len() >= 2 { format!("({} >> {})", args[0], args[1]) } else { "0".to_string() }
        });

        // bit_count(x) → BIT_COUNT
        custom.insert("bit_count", |args, _mode| {
            format!("BIT_COUNT({})", args.first().copied().unwrap_or("0"))
        });
        // bit_get / getbit
        custom.insert("bit_get", |args, _mode| {
            if args.len() >= 2 { format!("GET_BIT({}, {})", args[0], args[1]) } else { "0".to_string() }
        });
        custom.insert("getbit", |args, _mode| {
            if args.len() >= 2 { format!("GET_BIT({}, {})", args[0], args[1]) } else { "0".to_string() }
        });

        // make_interval for year_month / day_time intervals
        custom.insert("make_ym_interval", |args, _mode| {
            if args.len() >= 2 {
                format!("(INTERVAL ({}) YEAR + INTERVAL ({}) MONTH)", args[0], args[1])
            } else if args.len() == 1 {
                format!("INTERVAL ({}) YEAR", args[0])
            } else {
                "INTERVAL 0 YEAR".to_string()
            }
        });

        // window-function modes (strict mode routing placeholder)
        custom.insert("round", |args, mode| match mode {
            CompatMode::Strict => {
                // In strict mode, use the extension's half-up round
                format!("thdck_round({})", args.join(", "))
            }
            CompatMode::Relaxed => format!("ROUND({})", args.join(", ")),
        });

        // Strict-mode avg for decimal
        custom.insert("avg", |args, mode| match mode {
            CompatMode::Strict => format!("thdck_avg({})", args.join(", ")),
            CompatMode::Relaxed => format!("AVG({})", args.join(", ")),
        });

        // Macros registered at session startup
        let macros = vec![
            // initcap is already in DuckDB, no macro needed
        ];

        FunctionRegistry { direct, custom, macros }
    }
}

// ── Date format conversion (Spark SimpleDateFormat → strftime) ─────────────────

/// Convert a Spark SimpleDateFormat pattern string to a DuckDB strftime format.
///
/// This handles the most common patterns. Exotic patterns fall back to a literal
/// copy of the format string (which may produce incorrect output but won't panic).
pub fn convert_spark_date_format(spark_fmt: &str) -> String {
    // Strip surrounding quotes if present (the arg is an SQL string literal)
    let raw = spark_fmt.trim_matches('\'');

    let mut result = String::with_capacity(raw.len() * 2);
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\'' {
            // Literal text quoted with single quotes in SimpleDateFormat
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc == '\'' {
                    break;
                }
                result.push(nc);
            }
            continue;
        }
        match c {
            'y' | 'Y' => {
                // Count consecutive y
                let mut count = 1usize;
                while chars.peek() == Some(&c) { chars.next(); count += 1; }
                if count >= 4 { result.push_str("%Y"); } else { result.push_str("%y"); }
            }
            'M' => {
                let mut count = 1usize;
                while chars.peek() == Some(&'M') { chars.next(); count += 1; }
                match count {
                    1 | 2 => result.push_str("%m"),
                    3 => result.push_str("%b"),
                    _ => result.push_str("%B"),
                }
            }
            'd' => {
                while chars.peek() == Some(&'d') { chars.next(); }
                result.push_str("%d");
            }
            'H' => { while chars.peek() == Some(&'H') { chars.next(); } result.push_str("%H"); }
            'h' => { while chars.peek() == Some(&'h') { chars.next(); } result.push_str("%I"); }
            'm' => { while chars.peek() == Some(&'m') { chars.next(); } result.push_str("%M"); }
            's' => { while chars.peek() == Some(&'s') { chars.next(); } result.push_str("%S"); }
            'S' => { while chars.peek() == Some(&'S') { chars.next(); } result.push_str("%.f"); }
            'a' => result.push_str("%p"),
            'D' => result.push_str("%j"),
            'E' | 'e' => result.push_str("%A"),
            'w' => result.push_str("%W"),
            'z' => result.push_str("%Z"),
            'Z' => result.push_str("%z"),
            '-' | '/' | ':' | ' ' | '.' | ',' => result.push(c),
            _ => result.push(c),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_mapping() {
        let sql = FunctionRegistry::translate("upper", &["x"], CompatMode::Relaxed);
        assert_eq!(sql, "UPPER(x)");
    }

    #[test]
    fn count_star() {
        let sql = FunctionRegistry::translate("count", &["*"], CompatMode::Relaxed);
        assert_eq!(sql, "COUNT(*)");
    }

    #[test]
    fn count_distinct() {
        let sql = FunctionRegistry::translate("count_distinct", &["col"], CompatMode::Relaxed);
        assert_eq!(sql, "COUNT(DISTINCT col)");
    }

    #[test]
    fn locate_arg_swap() {
        let sql = FunctionRegistry::translate("locate", &["substr", "str"], CompatMode::Relaxed);
        assert_eq!(sql, "INSTR(str, substr)");
    }

    #[test]
    fn datediff_arg_order() {
        let sql = FunctionRegistry::translate("datediff", &["end_dt", "start_dt"], CompatMode::Relaxed);
        assert!(sql.contains("start_dt") && sql.contains("end_dt"));
    }

    #[test]
    fn dayofweek_offset() {
        let sql = FunctionRegistry::translate("dayofweek", &["d"], CompatMode::Relaxed);
        assert_eq!(sql, "(DAYOFWEEK(d) + 1)");
    }

    #[test]
    fn regexp_replace_global_flag() {
        let sql = FunctionRegistry::translate("regexp_replace", &["str", "pat", "rep"], CompatMode::Relaxed);
        assert!(sql.contains("'g'"), "expected global replace flag in: {sql}");
    }

    #[test]
    fn round_strict_mode() {
        let relaxed = FunctionRegistry::translate("round", &["x", "2"], CompatMode::Relaxed);
        let strict = FunctionRegistry::translate("round", &["x", "2"], CompatMode::Strict);
        assert_eq!(relaxed, "ROUND(x, 2)");
        assert!(strict.contains("thdck_round"), "expected extension fn in: {strict}");
    }

    #[test]
    fn unknown_passthrough() {
        let sql = FunctionRegistry::translate("my_custom_udf", &["a", "b"], CompatMode::Relaxed);
        assert_eq!(sql, "my_custom_udf(a, b)");
    }

    // ── Bug regression tests ───────────────────────────────────────────────────

    /// Bug: forall translator interpolates "" as the lambda body, producing
    /// `NOT ((x))` with an empty function name — completely broken SQL.
    #[test]
    fn forall_lambda_body_preserved() {
        let sql = FunctionRegistry::translate("forall", &["arr", "x -> x > 0"], CompatMode::Relaxed);
        // The broken implementation produces "NOT ((x))" (empty string interpolated)
        assert!(
            !sql.contains("NOT ((x))"),
            "forall must not emit broken empty-function pattern: {sql}"
        );
        assert!(sql.contains("arr"), "array arg must appear: {sql}");
        assert!(sql.contains("x > 0"), "lambda body must appear: {sql}");
    }

    /// Bug: greatest/least appear in both math_direct and cond_direct; the
    /// second insert silently overwrites, making the first a wasted allocation.
    /// Verify the function still resolves correctly after dedup.
    #[test]
    fn greatest_least_still_mapped() {
        let g = FunctionRegistry::translate("greatest", &["a", "b", "c"], CompatMode::Relaxed);
        let l = FunctionRegistry::translate("least", &["a", "b"], CompatMode::Relaxed);
        assert_eq!(g, "GREATEST(a, b, c)");
        assert_eq!(l, "LEAST(a, b)");
    }

    /// Bug: array_compact and array_union appear in array_direct with incorrect
    /// DuckDB mappings but are overridden by custom translators. The direct
    /// entries are unreachable dead weight.
    /// Verify correct (custom) translation is used.
    #[test]
    fn array_compact_uses_list_filter() {
        let sql = FunctionRegistry::translate("array_compact", &["arr"], CompatMode::Relaxed);
        // Must use LIST_FILTER with null-check lambda, NOT bare LIST_FILTER (direct entry)
        assert!(
            sql.contains("IS NOT NULL"),
            "array_compact must filter nulls via lambda: {sql}"
        );
    }

    #[test]
    fn spark_date_format_conversion() {
        assert_eq!(convert_spark_date_format("'yyyy-MM-dd'"), "%Y-%m-%d");
        assert_eq!(convert_spark_date_format("'yyyy-MM-dd HH:mm:ss'"), "%Y-%m-%d %H:%M:%S");
    }

    // ── Polymorphic dispatch tests ─────────────────────────────────────────────

    /// reverse(array_col) → LIST_REVERSE when first arg is Array type.
    #[test]
    fn reverse_array_dispatches_to_list_reverse() {
        let arg_types = [DataType::Array(Box::new(DataType::Integer))];
        let sql = FunctionRegistry::translate_typed(
            "reverse",
            &["arr"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "LIST_REVERSE(arr)");
    }

    /// reverse(str_col) → REVERSE (string variant) when first arg is String.
    #[test]
    fn reverse_string_dispatches_to_reverse() {
        let arg_types = [DataType::String];
        let sql = FunctionRegistry::translate_typed(
            "reverse",
            &["s"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "REVERSE(s)");
    }

    /// reverse(unresolved) → REVERSE (fall-through to existing direct mapping).
    #[test]
    fn reverse_unresolved_falls_through_to_reverse() {
        let arg_types = [DataType::Unresolved];
        let sql = FunctionRegistry::translate_typed(
            "reverse",
            &["x"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "REVERSE(x)");
    }

    /// size(array_col) → LEN when first arg is Array type.
    #[test]
    fn size_array_dispatches_to_len() {
        let arg_types = [DataType::Array(Box::new(DataType::Long))];
        let sql = FunctionRegistry::translate_typed(
            "size",
            &["arr"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "LEN(arr)");
    }

    /// size(map_col) → LEN when first arg is Map type.
    #[test]
    fn size_map_dispatches_to_len() {
        let arg_types = [DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Integer),
            value_nullable: true,
        }];
        let sql = FunctionRegistry::translate_typed(
            "size",
            &["m"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "LEN(m)");
    }

    /// size(str_col) → LENGTH when first arg is String.
    #[test]
    fn size_string_dispatches_to_length() {
        let arg_types = [DataType::String];
        let sql = FunctionRegistry::translate_typed(
            "size",
            &["s"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "LENGTH(s)");
    }

    /// sort_array(array_col) → LIST_SORT when first arg is Array type.
    #[test]
    fn sort_array_array_dispatches_to_list_sort() {
        let arg_types = [DataType::Array(Box::new(DataType::String))];
        let sql = FunctionRegistry::translate_typed(
            "sort_array",
            &["arr"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "LIST_SORT(arr)");
    }

    /// sort_array(unresolved) → LIST_SORT (fall-through to existing direct mapping).
    #[test]
    fn sort_array_unresolved_falls_through_to_list_sort() {
        let arg_types = [DataType::Unresolved];
        let sql = FunctionRegistry::translate_typed(
            "sort_array",
            &["x"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "LIST_SORT(x)");
    }

    /// Non-polymorphic functions delegate to translate unchanged.
    #[test]
    fn translate_typed_delegates_non_polymorphic_functions() {
        let arg_types = [DataType::String];
        let sql = FunctionRegistry::translate_typed(
            "upper",
            &["col"],
            &arg_types,
            CompatMode::Relaxed,
        );
        assert_eq!(sql, "UPPER(col)");
    }
}
