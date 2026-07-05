//! PySpark parity helpers shared across τ's emission substrate and the
//! `connect-server` outbound Arrow schema stamp.
//!
//! **INV10-safe.** Value-level `Vec<String>` in/out — no τ types cross this
//! boundary, so both the τ emission substrate and the non-τ arrow schema
//! stamp can consume it verbatim.

use std::collections::HashMap;

/// Dedup a list of names identically to
/// `pyspark.sql.pandas.types._dedup_names`.
///
/// Names that appear more than once are suffixed with `_{i}` where `i`
/// counts from 0 in the order the name appears. Names that appear once
/// are unchanged.
///
/// # Example
///
/// ```ignore
/// assert_eq!(
///     dedup_names(&["tags", "tags", "id"]),
///     vec!["tags_0", "tags_1", "id"],
/// );
/// ```
///
/// # Callers
///
/// - τ emission (`transpiler_v2::emission::render_data_type`): dedup the
///   field names inside a `CAST(x AS STRUCT(...))` payload so DuckDB's
///   binder does not refuse the type.
/// - `connect-server::arrow_schema_stamp`: dedup the outbound Arrow batch
///   field names so the wire schema matches τ's `resolved_schema`
///   bit-for-bit.
///
/// The two paths MUST use the same dedup rule or the substrate names and
/// the stamp target names drift apart.
pub fn dedup_names(names: &[&str]) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in names {
        *counts.entry(*n).or_insert(0) += 1;
    }
    let mut running: HashMap<&str, usize> = HashMap::new();
    names
        .iter()
        .map(|n| {
            if counts.get(*n).copied().unwrap_or(0) > 1 {
                let i = running.entry(*n).or_insert(0);
                let out = format!("{n}_{i}");
                *i += 1;
                out
            } else {
                (*n).to_owned()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_names_examples() {
        // Documented example from the PySpark parity source.
        assert_eq!(
            dedup_names(&["tags", "tags", "id"]),
            vec!["tags_0", "tags_1", "id"]
        );

        // Names unique across the list are passed through unchanged.
        assert_eq!(dedup_names(&["a", "b", "c"]), vec!["a", "b", "c"]);

        // Empty input.
        let empty: Vec<String> = dedup_names(&[]);
        assert!(empty.is_empty());

        // A single repeated name gets `_0`, `_1`, ... in appearance order.
        assert_eq!(dedup_names(&["x", "x", "x"]), vec!["x_0", "x_1", "x_2"]);

        // Case sensitivity: PySpark dedup is case-sensitive.
        assert_eq!(dedup_names(&["A", "a", "A"]), vec!["A_0", "a", "A_1"]);
    }
}
