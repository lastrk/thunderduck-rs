//! The SINGLE case-folding authority for USER-IDENTIFIER comparisons
//! (column / qualifier / alias names) across τ. Spark's resolver runs on the
//! JVM and compares identifiers with Java `String.equalsIgnoreCase`; this
//! module reproduces that as closely as Rust's `char` case-conversion API
//! allows, with one documented residual (see [`canon_char`]).
//!
//! Every user-identifier comparison or key-fold in the analyzer/emission
//! substrate should go through [`eq_fold`] (comparison) or [`fold_key`]
//! (`HashMap`/`HashSet` key-folding) — never a bespoke
//! `eq_ignore_ascii_case`/`to_lowercase`/`to_ascii_lowercase` call — so the
//! whole of τ agrees on what "the same name" means. Both are derived from
//! the SAME per-`char` primitive, [`canon_char`], so `eq_fold(a, b)` and
//! `fold_key(a) == fold_key(b)` can never disagree
//! (`eq_fold_and_fold_key_are_consistent_over_notorious_chars` pins this).
//!
//! Function/keyword/interval-unit/type-token folds are a DIFFERENT concern
//! (τ's own vocabulary, not a user-supplied identifier, and ASCII by
//! construction) and deliberately stay on plain `eq_ignore_ascii_case` /
//! `to_ascii_lowercase` at their call sites — this module is scoped to
//! identifiers a Spark caller wrote.

/// JDK `String.equalsIgnoreCase` folds each `char` via
/// `Character.toLowerCase(Character.toUpperCase(c))` (empirically verified
/// against JDK 21). This mirrors that exactly: uppercase, then lowercase,
/// each step kept only when it yields exactly ONE `char` — a step whose
/// Rust `to_uppercase()`/`to_lowercase()` expands to more than one `char`
/// (a Unicode `SpecialCasing` multi-character mapping, which Rust's `char`
/// API always exposes as an iterator, never a lookup back to a single
/// `char`) instead leaves that step's INPUT unchanged and continues.
///
/// This reproduces JDK 21's per-`char` `equalsIgnoreCase` fold exactly for
/// every case in the divergence table (`É`/`é`, `MÜLLER`/`müller`, `ß`/`ß`
/// vs `ß`/`ss`, Greek `σ`/`ς`/`Σ`, the Kelvin sign vs `k`) — see this
/// module's tests. The one known residual: `İ` (LATIN CAPITAL
/// LETTER I WITH DOT ABOVE, U+0130) vs `i` — Java's `equalsIgnoreCase`
/// matches them (both fold to `i` under JVM `Character.toLowerCase`'s
/// locale-independent 1:1 mapping), but Rust's `'İ'.to_lowercase()` yields
/// the TWO-char sequence `"i̇"` (`i` + COMBINING DOT ABOVE, per Unicode
/// `SpecialCasing.txt`) — a multi-char expansion, so this function's
/// fallback rule leaves `İ` unfolded and it does not match plain `i`. This
/// is an ADR-022 category-2 (Thunderduck-boundary) residual — bridging it
/// would need a hardcoded exception table Rust's `char` API has no room
/// for.
pub(crate) fn canon_char(c: char) -> char {
    let upper = single_char(c.to_uppercase()).unwrap_or(c);
    single_char(upper.to_lowercase()).unwrap_or(upper)
}

/// `Some(c)` iff the iterator yields EXACTLY one `char`; `None` for a
/// multi-char (or empty) expansion — [`canon_char`]'s per-step fallback
/// gate.
fn single_char<I: Iterator<Item = char>>(mut it: I) -> Option<char> {
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Case-insensitive equality for a user-supplied identifier pair, folded via
/// [`canon_char`]. ASCII fast path (`eq_ignore_ascii_case`, no allocation)
/// when BOTH sides are ASCII — provably consistent with the slow path,
/// since `canon_char` on an ASCII input always reduces to that code point's
/// ASCII-lowercase form (uppercase/lowercase of an ASCII letter is always a
/// single ASCII `char`).
pub(crate) fn eq_fold(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.chars().map(canon_char).eq(b.chars().map(canon_char))
}

/// The allocating, `HashMap`/`HashSet`-key-shaped sibling of [`eq_fold`]:
/// `fold_key(a) == fold_key(b)` iff `eq_fold(a, b)` (pinned by
/// `eq_fold_and_fold_key_are_consistent_over_notorious_chars`). ASCII fast
/// path (`to_ascii_lowercase`) for the same reason as [`eq_fold`]'s.
pub(crate) fn fold_key(name: &str) -> String {
    if name.is_ascii() {
        return name.to_ascii_lowercase();
    }
    name.chars().map(canon_char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground truth: JDK 21 `String.equalsIgnoreCase`, empirically measured.
    /// The `İ`/`i` row is the sole documented divergence (see
    /// [`canon_char`]'s doc) — every other row matches Java exactly.
    const DIVERGENCE_TABLE: &[(&str, &str, bool)] = &[
        ("É", "é", true),
        ("MÜLLER", "müller", true),
        ("İ", "i", false),
        ("ß", "ß", true),
        ("ß", "ss", false),
        ("σ", "ς", true),
        ("Σ", "ς", true),
        ("\u{212A}", "k", true), // KELVIN SIGN vs 'k'
    ];

    #[test]
    fn divergence_table_matches_design_verdicts() {
        for &(a, b, expected) in DIVERGENCE_TABLE {
            assert_eq!(
                eq_fold(a, b),
                expected,
                "eq_fold({a:?}, {b:?}) should be {expected}"
            );
            assert_eq!(
                fold_key(a) == fold_key(b),
                expected,
                "fold_key({a:?}) == fold_key({b:?}) should be {expected}"
            );
        }
    }

    /// `eq_fold` and `fold_key` can never disagree — both are derived from
    /// the same [`canon_char`] primitive. Exercised over the notorious
    /// chars from the divergence table plus their ASCII counterparts.
    #[test]
    fn eq_fold_and_fold_key_are_consistent_over_notorious_chars() {
        let chars = [
            'a', 'A', 'z', 'Z', 'é', 'É', 'ü', 'Ü', 'İ', 'i', 'I', 'ı', 'ß', 's', 'S', 'σ', 'ς',
            'Σ', '\u{212A}', 'k', 'K',
        ];
        for &a in &chars {
            for &b in &chars {
                let sa = a.to_string();
                let sb = b.to_string();
                assert_eq!(
                    eq_fold(&sa, &sb),
                    fold_key(&sa) == fold_key(&sb),
                    "eq_fold/fold_key disagree on {a:?} vs {b:?}"
                );
            }
        }
    }

    /// For pure-ASCII inputs, both functions degrade EXACTLY to their
    /// stdlib ASCII counterparts — the fast path is not an approximation.
    #[test]
    fn ascii_fast_path_agrees_with_stdlib_ascii_folds() {
        let samples = ["Hello", "WORLD", "MixedCase123", "", "the_quick_FOX"];
        for a in samples {
            for b in samples {
                assert_eq!(eq_fold(a, b), a.eq_ignore_ascii_case(b));
            }
            assert_eq!(fold_key(a), a.to_ascii_lowercase());
        }
    }
}
