//! τ's supported-function roster for Spark Connect catalog operations.
//!
//! INV10-safe registry view with no runtime imports.

use super::function_registry;

/// Returns `true` if `name` is in τ's supported-function registry
/// (case-insensitive).
pub fn is_supported_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    function_registry::lookup(&lower).is_some()
}

/// Returns τ's sorted supported-function names.
pub fn supported_function_names() -> impl Iterator<Item = &'static str> {
    function_registry::function_names()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_functions_are_sorted_and_deduped() {
        let names: Vec<_> = supported_function_names().collect();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn probed_names_present() {
        let must_have = [
            "abs",
            "sum",
            "count",
            "max",
            "min",
            "avg",
            "concat",
            "length",
            "upper",
            "lower",
            "ceil",
            "floor",
            "round",
            "sqrt",
            "coalesce",
            "row_number",
        ];
        for name in must_have {
            assert!(is_supported_function(name), "`{name}` should be registered");
        }
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(is_supported_function("ABS"));
        assert!(is_supported_function("Concat"));
        assert!(is_supported_function("SUM"));
    }

    #[test]
    fn unknown_names_are_absent() {
        assert!(!is_supported_function("nonexistent_function_xyz"));
        assert!(!is_supported_function(""));
        assert!(!is_supported_function("definitely_not_a_spark_func"));
    }
}
