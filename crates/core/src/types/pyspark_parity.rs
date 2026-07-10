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

/// The SUBSTRATE name-plane uniquify: rename entries so the
/// result is GUARANTEED unique within the list (collision-safe), deterministic,
/// and stable in appearance order. Unlike [`dedup_names`] (which reproduces
/// PySpark's wire `_dedup_names` convention and is NOT collision-safe, e.g.
/// `["a","a","a_0"] -> ["a_0","a_1","a_0"]`), this is for INTERNAL emitted-SQL
/// names only (subquery SELECT-list aliases) and must never
/// touch `resolved_schema` or the outbound wire stamp (ADR-005 dup-name parity).
/// An already-unique input is returned unchanged. On collision the suffix
/// counter keeps advancing past any name already present in the (original or
/// so-far-emitted) set, mirroring Calcite's `SqlValidatorUtil.uniquify`.
pub(crate) fn uniquify(names: &[&str]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(names.len());
    for name in names {
        let mut candidate = (*name).to_owned();
        let mut i = 0usize;
        while seen.contains(&candidate) {
            i += 1;
            candidate = format!("{name}_{i}");
        }
        seen.insert(candidate.clone());
        result.push(candidate);
    }
    result
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

    #[test]
    fn uniquify_guarantees_unique_output() {
        // The exact adversarial case where `dedup_names` fails:
        // dedup_names(["a","a","a_0"]) == ["a_0","a_1","a_0"] (duplicate "a_0").
        let out = uniquify(&["a", "a", "a_0"]);
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert_eq!(
            unique.len(),
            out.len(),
            "uniquify produced duplicates: {out:?}"
        );
    }

    #[test]
    fn uniquify_passthrough_when_already_unique() {
        assert_eq!(uniquify(&["a", "b", "c"]), vec!["a", "b", "c"]);
    }

    #[test]
    fn uniquify_deterministic_and_ordered() {
        let out = uniquify(&["x", "x", "x"]);
        assert_eq!(out.len(), 3);
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert_eq!(
            unique.len(),
            3,
            "expected a stable distinct triple: {out:?}"
        );
        // Deterministic: repeated calls yield the same result.
        assert_eq!(uniquify(&["x", "x", "x"]), out);
    }

    #[test]
    fn uniquify_empty_input() {
        let empty: Vec<String> = uniquify(&[]);
        assert!(empty.is_empty());
    }

    #[test]
    fn dedup_names_is_not_collision_safe_contrast() {
        // Documents WHY `uniquify` exists as a separate function: `dedup_names`
        // reproduces PySpark's wire convention, which is not collision-safe.
        let out = dedup_names(&["a", "a", "a_0"]);
        assert_eq!(out, vec!["a_0", "a_1", "a_0"]);
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert!(
            unique.len() < out.len(),
            "expected dedup_names to have a collision on this input"
        );
    }
}
