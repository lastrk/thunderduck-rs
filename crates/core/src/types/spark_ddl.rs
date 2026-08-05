//! Single Spark-DDL type-string parser.
//!
//! Pass-2 simplification: Spark type-string → [`DataType`] parsing used to be
//! implemented twice with different grammars — connect-server's
//! `parse_type_str` (lowercase tokens, decimal / array / intervals / null,
//! NOT NULL stripping, unknown → `Unresolved`) and emission's
//! `spark_ddl_type_to_core_data_type` (uppercase tokens, STRUCT / ARRAY,
//! unknown → `None`). This module is the union of BOTH grammars behind two
//! entry points that differ only in unknown-token handling: strict (unknown →
//! `None`, emission behavior) and [`parse_spark_type_lenient`] (unknown →
//! [`DataType::Unresolved`], connect-server behavior). Only the lenient *type*
//! entry point and the schema entry point ([`parse_spark_schema`], which routes
//! through strict parsing internally) are public; strict type parsing is
//! reachable through `parse_type(s, false)`.
//!
//! The union is strictly additive over each legacy parser: every input either
//! legacy parser accepted parses here to the SAME type; each entry point
//! additionally accepts what only the *other* legacy grammar covered.
//!
//! Value-level code only: this module must not import `transpiler_v2` or
//! `runtime` (INV10-adjacent layering — `types/` sits below τ).

use super::{DataType, StructField, StructType};

/// Parse a Spark type string leniently: unknown input returns
/// [`DataType::Unresolved`] (the legacy `parse_type_str` contract).
///
/// Grammar = the union of both legacy parsers. Relative to the legacy lenient
/// parser, acceptance is widened strictly additively by the union:
/// `struct<name:type,...>` now parses (it previously fell through to
/// `Unresolved`), `blob` maps to Binary, and a bare `null` token now parses
/// to [`DataType::Null`] (the legacy suffix-stripping consumed it before the
/// token match could see it). Nothing the legacy lenient parser accepted
/// parses differently.
pub fn parse_spark_type_lenient(s: &str) -> DataType {
    // The lenient mode of `parse_type` is total (every fallthrough lands on
    // `Unresolved`); `unwrap_or` is belt-and-braces, not a reachable path.
    parse_type(s, true).unwrap_or(DataType::Unresolved)
}

/// Parse a Spark DDL *schema* string strictly — either a bare field list
/// (`"a INT, b ARRAY<STRING>"`) or a single `struct<...>` type
/// (`"struct<a:INT,b:STRING>"`) — into a [`StructType`]. Returns `None` when
/// the DDL cannot be translated. All fields are marked nullable (a trailing
/// `NOT NULL` qualifier is accepted but, matching the legacy parsers, does
/// not flip nullability).
pub fn parse_spark_schema(ddl: &str) -> Option<StructType> {
    let t = ddl.trim();
    if starts_with_ci(t, "struct<") {
        return match parse_type(t, false)? {
            DataType::Struct(st) => Some(st),
            _ => None,
        };
    }
    parse_fields(t, false)
}

/// Core recursive parser. `lenient` controls unknown-token handling only:
/// `true` → `Some(Unresolved)`, `false` → `None`. In lenient mode every
/// fallthrough degrades to `Unresolved` (never `None`), matching the legacy
/// `parse_type_str` which was total.
fn parse_type(s: &str, lenient: bool) -> Option<DataType> {
    let t = s.trim();
    // Bare `null` / `void` first: the NOT NULL / NULL qualifier stripping
    // below would otherwise consume a bare `null` token entirely (the legacy
    // lenient parser had exactly that quirk — bare `null` was unreachable).
    if t.eq_ignore_ascii_case("null") || t.eq_ignore_ascii_case("void") {
        return Some(DataType::Null);
    }
    let t = strip_null_qualifiers(t);

    // struct<name:type, ...> (from the legacy strict grammar).
    if starts_with_ci(t, "struct<") {
        if let Some(inner) = t["struct<".len()..].strip_suffix('>') {
            if let Some(st) = parse_fields(inner, lenient) {
                return Some(DataType::Struct(st));
            }
        }
        // Malformed struct: strict rejects; lenient falls through to the
        // token match, which cannot match `struct<...` → Unresolved.
        if !lenient {
            return None;
        }
    }

    // array<element_type> (both legacy grammars; contains_null = true).
    if starts_with_ci(t, "array<") {
        if let Some(inner) = t["array<".len()..].strip_suffix('>') {
            if let Some(elem) = parse_type(inner, lenient) {
                return Some(DataType::Array(Box::new(elem), true));
            }
            if !lenient {
                return None;
            }
        }
        // Malformed array falls through: token match fails in both modes,
        // yielding None (strict) / Unresolved (lenient) — legacy behavior.
    }

    // decimal / decimal(p) / decimal(p,s) (from the legacy lenient grammar,
    // defaults preserved verbatim: missing/unparseable precision → 38,
    // missing/unparseable scale → 18).
    if starts_with_ci(t, "decimal") {
        return Some(parse_decimal(&t["decimal".len()..]));
    }

    let token = t.to_ascii_lowercase();
    let dt = match token.as_str() {
        "boolean" | "bool" => DataType::Boolean,
        "tinyint" | "byte" | "int8" => DataType::Byte,
        "smallint" | "short" | "int16" => DataType::Short,
        "int" | "integer" | "int32" => DataType::Integer,
        "bigint" | "long" | "int64" => DataType::Long,
        "float" | "real" | "float32" => DataType::Float,
        "double" | "float64" => DataType::Double,
        "string" | "str" | "varchar" | "char" | "text" => DataType::String,
        "binary" | "bytes" | "blob" => DataType::Binary,
        "date" => DataType::Date,
        "timestamp" | "timestamp_ltz" => DataType::Timestamp,
        "timestamp_ntz" => DataType::TimestampNtz,
        "interval year to month" | "yearmonthinterval" => DataType::YearMonthInterval,
        "interval day to second" | "daytimeinterval" => DataType::DayTimeInterval,
        "interval" => DataType::Interval,
        "null" | "void" => DataType::Null,
        _ => {
            return if lenient {
                Some(DataType::Unresolved)
            } else {
                None
            }
        }
    };
    Some(dt)
}

/// Parse the remainder after a leading `decimal` prefix, replicating the
/// legacy `parse_type_str` semantics exactly: well-formed `(p,s)` / `(p)`
/// parse their numbers with fallback defaults (precision 38, scale 18);
/// anything else (bare `decimal`, malformed parens, trailing junk) yields
/// `decimal(38,18)`.
fn parse_decimal(rest: &str) -> DataType {
    if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        let mut parts = inner.split(',');
        let precision = parts
            .next()
            .and_then(|p| p.trim().parse::<u8>().ok())
            .unwrap_or(38);
        let scale = parts
            .next()
            .and_then(|p| p.trim().parse::<u8>().ok())
            .unwrap_or(18);
        return DataType::Decimal { precision, scale };
    }
    DataType::Decimal {
        precision: 38,
        scale: 18,
    }
}

/// Parse a comma-separated `name TYPE` / `name:TYPE` field list into a
/// [`StructType`]. Structural failures (unbalanced brackets, a field with no
/// name/type separator) return `None` in both modes; in lenient mode only the
/// per-field *type* degrades to `Unresolved`.
fn parse_fields(ddl: &str, lenient: bool) -> Option<StructType> {
    let fields = split_top_level_fields(ddl)?;
    let mut out: Vec<StructField> = Vec::with_capacity(fields.len());
    for field in &fields {
        let (name, ty) = split_field_name_type(field)?;
        let dt = parse_type(ty.trim(), lenient)?;
        out.push(StructField::new(name.trim().to_owned(), dt, true));
    }
    Some(StructType::new(out))
}

/// Split a comma-separated field list, honoring nested `<...>` and `(...)`
/// so `STRUCT<a:INT, b:DOUBLE>` is treated as one field.
fn split_top_level_fields(s: &str) -> Option<Vec<String>> {
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '<' | '(' => {
                depth += 1;
                cur.push(ch);
            }
            '>' | ')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                cur.push(ch);
            }
            ',' if depth == 0 => {
                if !cur.trim().is_empty() {
                    parts.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if depth != 0 {
        return None;
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    Some(parts)
}

/// Split `name TYPE` (space-separated, top-level DDL) or `name:TYPE`
/// (colon-separated, used inside `STRUCT<...>`) into `(name, type_str)`.
/// Honors nested `<...>` and `(...)` so a `:` inside `STRUCT<f:INT>` is
/// not mistaken for the outer separator.
fn split_field_name_type(field: &str) -> Option<(&str, &str)> {
    let trimmed = field.trim();
    let mut depth = 0i32;
    let mut sep_idx: Option<usize> = None;
    let mut sep_len = 1usize;
    for (i, ch) in trimmed.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            ':' if depth == 0 => {
                sep_idx = Some(i);
                sep_len = 1;
                break;
            }
            c if depth == 0 && c.is_whitespace() => {
                if sep_idx.is_none() {
                    sep_idx = Some(i);
                    sep_len = c.len_utf8();
                    // Don't `break` on whitespace — we still prefer a
                    // colon if one appears later at depth 0.
                }
            }
            _ => {}
        }
    }
    let idx = sep_idx?;
    let (n, t) = trimmed.split_at(idx);
    Some((n, &t[sep_len..]))
}

/// Strip trailing `NOT NULL` / `NULL` qualifiers, replicating the legacy
/// `trim_end_matches("not null").trim_end_matches("null").trim()` (repeated
/// suffix stripping, no whitespace normalization between strips) but
/// case-insensitively and without lowercasing the input — struct field names
/// must keep their casing.
fn strip_null_qualifiers(s: &str) -> &str {
    let mut cur = s;
    while let Some(rest) = strip_suffix_ci(cur, "not null") {
        cur = rest;
    }
    while let Some(rest) = strip_suffix_ci(cur, "null") {
        cur = rest;
    }
    cur.trim()
}

/// Case-insensitive (ASCII) `strip_suffix`.
fn strip_suffix_ci<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    if s.len() < suffix.len() || !s.is_char_boundary(s.len() - suffix.len()) {
        return None;
    }
    let (head, tail) = s.split_at(s.len() - suffix.len());
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

/// Case-insensitive (ASCII) `starts_with`.
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict(s: &str) -> Option<DataType> {
        parse_type(s, false)
    }

    fn lenient(s: &str) -> DataType {
        parse_spark_type_lenient(s)
    }

    // ── Primitive token union ────────────────────────────────────────────

    #[test]
    fn primitives_parse_case_insensitively_in_both_modes() {
        let cases: &[(&str, DataType)] = &[
            ("boolean", DataType::Boolean),
            ("BOOL", DataType::Boolean),
            ("tinyint", DataType::Byte),
            ("Byte", DataType::Byte),
            ("int8", DataType::Byte),
            ("smallint", DataType::Short),
            ("short", DataType::Short),
            ("int16", DataType::Short),
            ("int", DataType::Integer),
            ("INTEGER", DataType::Integer),
            ("int32", DataType::Integer),
            ("bigint", DataType::Long),
            ("long", DataType::Long),
            ("int64", DataType::Long),
            ("float", DataType::Float),
            ("real", DataType::Float),
            ("float32", DataType::Float),
            ("double", DataType::Double),
            ("float64", DataType::Double),
            ("string", DataType::String),
            ("str", DataType::String),
            ("varchar", DataType::String),
            ("char", DataType::String),
            ("text", DataType::String),
            ("binary", DataType::Binary),
            ("bytes", DataType::Binary),
            ("BLOB", DataType::Binary),
            ("date", DataType::Date),
            ("timestamp", DataType::Timestamp),
            ("TIMESTAMP_LTZ", DataType::Timestamp),
            ("timestamp_ntz", DataType::TimestampNtz),
            ("interval year to month", DataType::YearMonthInterval),
            ("yearmonthinterval", DataType::YearMonthInterval),
            ("INTERVAL DAY TO SECOND", DataType::DayTimeInterval),
            ("daytimeinterval", DataType::DayTimeInterval),
            ("interval", DataType::Interval),
            ("void", DataType::Null),
            ("null", DataType::Null),
        ];
        for (input, expected) in cases {
            assert_eq!(strict(input).as_ref(), Some(expected), "strict {input:?}");
            assert_eq!(&lenient(input), expected, "lenient {input:?}");
        }
    }

    #[test]
    fn unknown_token_strict_none_lenient_unresolved() {
        assert_eq!(strict("garbage"), None);
        assert_eq!(lenient("garbage"), DataType::Unresolved);
    }

    // ── decimal (legacy parse_type_str defaults preserved verbatim) ──────

    #[test]
    fn decimal_forms_and_legacy_defaults() {
        assert_eq!(
            strict("decimal(10,2)"),
            Some(DataType::Decimal {
                precision: 10,
                scale: 2
            })
        );
        assert_eq!(
            lenient("DECIMAL(10, 2)"),
            DataType::Decimal {
                precision: 10,
                scale: 2
            }
        );
        // decimal(p) → legacy default scale 18.
        assert_eq!(
            lenient("decimal(10)"),
            DataType::Decimal {
                precision: 10,
                scale: 18
            }
        );
        // Bare / malformed decimal → legacy default (38,18).
        for input in ["decimal", "decimal(abc)", "decimal(10,2) extra"] {
            assert_eq!(
                lenient(input),
                DataType::Decimal {
                    precision: 38,
                    scale: 18
                },
                "lenient {input:?}"
            );
        }
    }

    // ── NOT NULL / NULL qualifier stripping ──────────────────────────────

    #[test]
    fn not_null_qualifier_is_stripped_in_both_modes() {
        assert_eq!(strict("int not null"), Some(DataType::Integer));
        assert_eq!(strict("BIGINT NOT NULL"), Some(DataType::Long));
        assert_eq!(lenient("bigint null"), DataType::Long);
        assert_eq!(lenient("array<int> not null").to_string(), "array<integer>");
    }

    // ── array ─────────────────────────────────────────────────────────────

    #[test]
    fn array_parses_with_nullable_elements() {
        assert_eq!(
            strict("array<string>"),
            Some(DataType::Array(Box::new(DataType::String), true))
        );
        assert_eq!(
            strict("ARRAY<ARRAY<INT>>"),
            Some(DataType::Array(
                Box::new(DataType::Array(Box::new(DataType::Integer), true)),
                true
            ))
        );
    }

    #[test]
    fn array_unknown_element_lenient_unresolved_strict_none() {
        // Legacy lenient behavior: element degrades to Unresolved, the array
        // itself still parses.
        assert_eq!(
            lenient("array<garbage>"),
            DataType::Array(Box::new(DataType::Unresolved), true)
        );
        assert_eq!(strict("array<garbage>"), None);
    }

    #[test]
    fn malformed_array_strict_none_lenient_unresolved() {
        assert_eq!(strict("array<int"), None);
        assert_eq!(lenient("array<int"), DataType::Unresolved);
    }

    // ── struct ────────────────────────────────────────────────────────────

    #[test]
    fn struct_parses_in_both_modes() {
        let expected = DataType::Struct(StructType::new(vec![
            StructField::nullable("id", DataType::Long),
            StructField::nullable("name", DataType::String),
        ]));
        assert_eq!(
            strict("struct<id:bigint,name:string>"),
            Some(expected.clone())
        );
        // Widening over legacy lenient parse_type_str (previously Unresolved).
        assert_eq!(lenient("struct<id: bigint, name: string>"), expected);
    }

    #[test]
    fn struct_field_names_preserve_case_even_under_array() {
        let dt = strict("ARRAY<STRUCT<myField:INT>>").expect("must parse");
        let DataType::Array(elem, _) = dt else {
            panic!("expected array, got {dt:?}");
        };
        let DataType::Struct(st) = *elem else {
            panic!("expected struct element, got {elem:?}");
        };
        assert_eq!(st.fields[0].name, "myField");
    }

    #[test]
    fn malformed_struct_strict_none_lenient_unresolved() {
        assert_eq!(strict("struct<a>"), None);
        assert_eq!(lenient("struct<a>"), DataType::Unresolved);
    }

    // ── schema-level helper ──────────────────────────────────────────────

    #[test]
    fn parse_spark_schema_accepts_bare_field_list() {
        let st =
            parse_spark_schema("a INT, b ARRAY<STRING>, c STRUCT<d:BOOLEAN>").expect("must parse");
        assert_eq!(st.fields.len(), 3);
        assert_eq!(st.fields[0].name, "a");
        assert_eq!(st.fields[0].data_type, DataType::Integer);
        assert_eq!(
            st.fields[1].data_type,
            DataType::Array(Box::new(DataType::String), true)
        );
        assert_eq!(
            st.fields[2].data_type,
            DataType::Struct(StructType::new(vec![StructField::nullable(
                "d",
                DataType::Boolean
            )]))
        );
        assert!(st.fields.iter().all(|f| f.nullable));
    }

    #[test]
    fn parse_spark_schema_accepts_struct_wrapper_form() {
        let st = parse_spark_schema("struct<id:bigint,name:string>").expect("must parse");
        assert_eq!(st.fields.len(), 2);
        assert_eq!(st.fields[0].name, "id");
        assert_eq!(st.fields[0].data_type, DataType::Long);
        assert_eq!(st.fields[1].name, "name");
        assert_eq!(st.fields[1].data_type, DataType::String);
    }

    #[test]
    fn parse_spark_schema_rejects_untranslatable_ddl() {
        assert_eq!(parse_spark_schema("a WIDGET"), None);
        assert_eq!(parse_spark_schema("int"), None); // scalar type, not a schema
        assert_eq!(parse_spark_schema("struct<a>"), None);
    }

    #[test]
    fn parse_spark_schema_empty_field_list_is_empty_struct() {
        // Matches the historical emission walker: empty DDL → empty struct.
        assert_eq!(parse_spark_schema(""), Some(StructType::empty()));
    }
}
