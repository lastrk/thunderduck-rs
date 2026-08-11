//! τ's supported-function roster for Spark Connect catalog operations.
//!
//! This module provides Spark Connect catalog operations with the function
//! names τ supports.
//!
//! INV10-safe: pure data, no runtime imports.

use super::function_registry;

/// Unmigrated catalog spellings. Registry names must not appear here.
pub const LEGACY_FUNCTIONS: &[&str] = &[
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
    "array_repeat",
    "array_union",
    "arrays_overlap",
    "arrays_zip",
    "ascii",
    "asin",
    "atan",
    "atan2",
    "base64",
    "bin",
    "bit_count",
    "bit_length",
    "bitwise_not",
    "bround",
    "cardinality",
    "cast",
    "cbrt",
    "ceil",
    "ceiling",
    "char_length",
    "coalesce",
    "concat",
    "concat_ws",
    "contains",
    "conv",
    "cos",
    "cosh",
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
    "extract",
    "factorial",
    "filter",
    "find_in_set",
    "flatten",
    "floor",
    "forall",
    "format_string",
    "from_csv",
    "from_json",
    "from_unixtime",
    "from_utc_timestamp",
    "greatest",
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
    "lag",
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
    "md5",
    "minute",
    "mod",
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
    "slice",
    "sort_array",
    "split",
    "sqrt",
    "starts_with",
    "startswith",
    "struct",
    "substr",
    "substring",
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
    function_registry::lookup(&lower).is_some()
        || LEGACY_FUNCTIONS
            .binary_search_by(|probe| probe.cmp(&&*lower))
            .is_ok()
}

/// Returns a sorted iterator over τ's supported function names.
pub fn supported_function_names() -> impl Iterator<Item = &'static str> {
    let mut names: Vec<_> = LEGACY_FUNCTIONS
        .iter()
        .copied()
        .chain(function_registry::function_names())
        .collect();
    names.sort_unstable();
    names.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_functions_is_sorted_and_deduped() {
        let mut prev: Option<&str> = None;
        for &name in LEGACY_FUNCTIONS {
            if let Some(p) = prev {
                assert!(
                    p < name,
                    "LEGACY_FUNCTIONS is not sorted/deduped: \
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
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn legacy_and_registry_are_disjoint() {
        for name in function_registry::function_names() {
            assert!(
                LEGACY_FUNCTIONS.binary_search(&name).is_err(),
                "duplicate `{name}`"
            );
        }
    }
}
