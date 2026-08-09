//! τ's supported-function roster for Spark Connect catalog operations.
//!
//! This module provides a curated list of Spark function names that τ can
//! translate to DuckDB SQL.  The list is derived from the match arms in
//! [`emission::render_function_call`](super::emission) and
//! [`type_inference::TypeInferenceEngine::function_return_type`](super::type_inference).
//!
//! INV10-safe: pure data, no runtime imports.

/// Alphabetically sorted, deduplicated list of Spark function names that τ
/// supports.  Each entry is a lowercase canonical name matching the
/// case-insensitive lookup in `render_function_call` / `function_return_type`.
///
/// The list is intentionally conservative — it includes functions that both
/// the emission layer AND the type inference layer handle end-to-end.
/// Internal/synthetic names (e.g. `posexplode_pos`, `stack_col`,
/// `inline_field`) are excluded because they are not user-visible Spark
/// functions.
pub const SUPPORTED_FUNCTIONS: &[&str] = &[
    "abs",
    "acos",
    "add_months",
    "aggregate",
    "array",
    "array_append",
    "array_compact",
    "array_contains",
    "array_distinct",
    "array_except",
    "array_insert",
    "array_intersect",
    "array_join",
    "array_max",
    "array_min",
    "array_position",
    "array_prepend",
    "array_remove",
    "array_repeat",
    "array_union",
    "arrays_overlap",
    "arrays_zip",
    "ascii",
    "asin",
    "atan",
    "atan2",
    "avg",
    "base64",
    "bin",
    "bit_and",
    "bit_count",
    "bit_length",
    "bit_or",
    "bit_xor",
    "bitwise_not",
    "bool_and",
    "bool_or",
    "bround",
    "cardinality",
    "cast",
    "cbrt",
    "ceil",
    "ceiling",
    "char_length",
    "coalesce",
    "collect_list",
    "collect_set",
    "concat",
    "concat_ws",
    "contains",
    "conv",
    "corr",
    "cos",
    "cosh",
    "count",
    "count_distinct",
    "count_if",
    "covar_pop",
    "covar_samp",
    "crc32",
    "create_map",
    "current_date",
    "current_timestamp",
    "date_add",
    "date_format",
    "date_sub",
    "datediff",
    "day",
    "dayofmonth",
    "dayofweek",
    "dayofyear",
    "degrees",
    "element_at",
    "elt",
    "ends_with",
    "endswith",
    "exists",
    "exp",
    "explode",
    "explode_outer",
    "extract",
    "factorial",
    "filter",
    "find_in_set",
    "first",
    "first_value",
    "flatten",
    "floor",
    "forall",
    "format_string",
    "from_csv",
    "from_json",
    "from_unixtime",
    "from_utc_timestamp",
    "greatest",
    "grouping",
    "grouping_id",
    "hash",
    "hex",
    "hour",
    "hypot",
    "if",
    "ifnull",
    "ilike",
    "initcap",
    "instr",
    "isnan",
    "isnotnull",
    "isnull",
    "kurtosis",
    "lag",
    "last",
    "last_value",
    "lead",
    "least",
    "left",
    "length",
    "levenshtein",
    "like",
    "ln",
    "locate",
    "log",
    "log10",
    "log2",
    "lower",
    "lpad",
    "ltrim",
    "make_date",
    "make_dt_interval",
    "make_interval",
    "make_ym_interval",
    "map",
    "map_concat",
    "map_contains_key",
    "map_entries",
    "map_filter",
    "map_from_arrays",
    "map_from_entries",
    "map_keys",
    "map_values",
    "map_zip_with",
    "max",
    "md5",
    "mean",
    "median",
    "min",
    "minute",
    "mod",
    "mode",
    "month",
    "months_between",
    "named_struct",
    "nanvl",
    "negative",
    "not",
    "now",
    "nth_value",
    "nullif",
    "nvl",
    "nvl2",
    "overlay",
    "parse_url",
    "percentile",
    "percentile_approx",
    "pmod",
    "pow",
    "power",
    "quarter",
    "radians",
    "rand",
    "randn",
    "reduce",
    "regexp_extract",
    "regexp_like",
    "regexp_replace",
    "repeat",
    "replace",
    "reverse",
    "right",
    "rlike",
    "round",
    "rpad",
    "rtrim",
    "schema_of_json",
    "second",
    "sequence",
    "sha",
    "sha1",
    "sha2",
    "shiftleft",
    "shiftright",
    "shuffle",
    "sign",
    "signum",
    "sin",
    "sinh",
    "size",
    "skewness",
    "slice",
    "sort_array",
    "split",
    "sqrt",
    "starts_with",
    "startswith",
    "std",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "struct",
    "substr",
    "substring",
    "sum",
    "sum_distinct",
    "tan",
    "tanh",
    "timestampadd",
    "timestampdiff",
    "to_csv",
    "to_date",
    "to_json",
    "to_number",
    "to_timestamp",
    "to_utc_timestamp",
    "transform",
    "transform_keys",
    "transform_values",
    "translate",
    "trim",
    "trunc",
    "try_divide",
    "try_element_at",
    "try_to_number",
    "typeof",
    "unbase64",
    "unhex",
    "unix_timestamp",
    "upper",
    "url_decode",
    "url_encode",
    "var_pop",
    "var_samp",
    "variance",
    "week",
    "weekofyear",
    "window",
    "xxhash64",
    "year",
    "zip_with",
];

/// Returns `true` if `name` is in τ's supported-function roster
/// (case-insensitive).
///
/// The case-insensitive check is retained at this public boundary even though
/// production `FunctionCall` names are canonicalized earlier.
pub fn is_supported_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SUPPORTED_FUNCTIONS
        .binary_search_by(|probe| probe.cmp(&&*lower))
        .is_ok()
}

/// Returns a sorted iterator over τ's supported function names.
pub fn supported_function_names() -> impl Iterator<Item = &'static str> {
    SUPPORTED_FUNCTIONS.iter().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_functions_is_sorted_and_deduped() {
        let mut prev: Option<&str> = None;
        for &name in SUPPORTED_FUNCTIONS {
            if let Some(p) = prev {
                assert!(
                    p < name,
                    "SUPPORTED_FUNCTIONS is not sorted/deduped: \
                     `{p}` appears before `{name}`"
                );
            }
            prev = Some(name);
        }
    }

    #[test]
    fn probed_names_present() {
        let must_have = [
            "abs", "sum", "count", "max", "min", "avg", "concat", "length", "upper", "lower",
            "ceil", "floor", "round", "sqrt", "coalesce",
        ];
        for name in &must_have {
            assert!(
                is_supported_function(name),
                "`{name}` should be in the roster"
            );
        }
    }

    #[test]
    fn case_insensitive_lookup() {
        assert!(is_supported_function("ABS"));
        assert!(is_supported_function("Concat"));
        assert!(is_supported_function("SUM"));
    }

    #[test]
    fn garbage_absent() {
        assert!(!is_supported_function("nonexistent_function_xyz"));
        assert!(!is_supported_function(""));
        assert!(!is_supported_function("definitely_not_a_spark_func"));
    }

    #[test]
    fn supported_function_names_yields_sorted() {
        let names: Vec<&str> = supported_function_names().collect();
        assert_eq!(names.len(), SUPPORTED_FUNCTIONS.len());
        for (i, name) in names.iter().enumerate() {
            assert_eq!(*name, SUPPORTED_FUNCTIONS[i]);
        }
    }
}
