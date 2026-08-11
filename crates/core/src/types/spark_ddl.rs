//! Spark DDL type-string parsing.
//!
//! Strict parsing returns `None` for unknown or malformed types. Lenient
//! parsing maps unknown types to [`DataType::Unresolved`]. Schema parsing is
//! strict and accepts either a bare field list or a `struct<...>` wrapper.
//! The grammar is the additive union of the former strict and lenient parsers:
//! previously accepted inputs keep their meaning, while forms such as
//! `struct<...>`, `blob`, and bare `null` are shared across call sites.
//!
//! Value-level code only: this module must not import `transpiler_v2` or
//! `runtime` (INV10-adjacent layering — `types/` sits below τ).

use super::{DataType, DayTimeField, StructField, StructType, YearMonthField};

/// Parse a Spark type string leniently: unknown input returns
/// [`DataType::Unresolved`].
pub fn parse_spark_type_lenient(s: &str) -> DataType {
    // The lenient mode of `parse_type` is total (every fallthrough lands on
    // `Unresolved`); `unwrap_or` is belt-and-braces, not a reachable path.
    parse_type(s, true).unwrap_or(DataType::Unresolved)
}

/// Parse a Spark DDL *schema* string strictly — either a bare field list
/// (`"a INT, b ARRAY<STRING>"`) or a single `struct<...>` type
/// (`"struct<a:INT,b:STRING>"`) — into a [`StructType`]. Returns `None` when
/// the DDL cannot be translated. All fields are marked nullable (a trailing
/// `NOT NULL` qualifier is accepted but does not flip nullability).
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
/// fallthrough degrades to `Unresolved` (never `None`).
fn parse_type(s: &str, lenient: bool) -> Option<DataType> {
    let t = s.trim();
    // Check bare `null` / `void` before stripping type qualifiers.
    if t.eq_ignore_ascii_case("null") || t.eq_ignore_ascii_case("void") {
        return Some(DataType::Null);
    }
    let t = strip_null_qualifiers(t);

    // struct<name:type, ...>
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

    // array<element_type> (contains_null = true).
    if starts_with_ci(t, "array<") {
        if let Some(inner) = t["array<".len()..].strip_suffix('>') {
            if let Some(elem) = parse_type(inner, lenient) {
                return Some(DataType::Array(Box::new(elem), true));
            }
            if !lenient {
                return None;
            }
        }
        // Malformed arrays fall through to the mode-specific unknown result.
    }

    // decimal / decimal(p) / decimal(p,s), with defaults 38 and 18.
    if starts_with_ci(t, "decimal") {
        return Some(parse_decimal(&t["decimal".len()..]));
    }

    let token = t.to_ascii_lowercase();
    if let Some(interval) = parse_ansi_interval(&token) {
        return Some(interval);
    }
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
        "yearmonthinterval" => DataType::year_month_full(),
        "daytimeinterval" => DataType::day_time_full(),
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

fn parse_ansi_interval(token: &str) -> Option<DataType> {
    let fields = token.strip_prefix("interval ")?;
    let mut fields = fields.split(" to ");
    let start = fields.next()?;
    let end = fields.next().unwrap_or(start);
    if fields.next().is_some() {
        return None;
    }

    let year_month = |field| match field {
        "year" => Some(YearMonthField::Year),
        "month" => Some(YearMonthField::Month),
        _ => None,
    };
    if let (Some(start), Some(end)) = (year_month(start), year_month(end)) {
        return (start <= end).then_some(DataType::YearMonthInterval { start, end });
    }

    let day_time = |field| match field {
        "day" => Some(DayTimeField::Day),
        "hour" => Some(DayTimeField::Hour),
        "minute" => Some(DayTimeField::Minute),
        "second" => Some(DayTimeField::Second),
        _ => None,
    };
    let (start, end) = (day_time(start)?, day_time(end)?);
    (start <= end).then_some(DataType::DayTimeInterval { start, end })
}

/// Parse the remainder after a leading `decimal` prefix. Well-formed `(p,s)` /
/// `(p)` forms use fallback defaults (precision 38,
/// scale 18); bare or malformed forms yield `decimal(38,18)`.
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
            // Only the FIRST depth-0 whitespace is a candidate separator, and
            // we deliberately do NOT `break` — a colon appearing later at
            // depth 0 still wins. Once `sep_idx` is set the guard stops
            // matching, which falls through to the no-op arm exactly as the
            // previous inner `if` did.
            c if depth == 0 && c.is_whitespace() && sep_idx.is_none() => {
                sep_idx = Some(i);
                sep_len = c.len_utf8();
            }
            _ => {}
        }
    }
    let idx = sep_idx?;
    let (n, t) = trimmed.split_at(idx);
    Some((n, &t[sep_len..]))
}

/// Strip trailing `NOT NULL` / `NULL` qualifiers case-insensitively without
/// lowercasing the input, preserving struct field-name casing.
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
            ("interval year to month", DataType::year_month_full()),
            ("yearmonthinterval", DataType::year_month_full()),
            ("INTERVAL DAY TO SECOND", DataType::day_time_full()),
            ("daytimeinterval", DataType::day_time_full()),
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

    #[test]
    fn ansi_interval_spans_parse_exactly() {
        let cases = [
            (
                "interval month",
                DataType::YearMonthInterval {
                    start: YearMonthField::Month,
                    end: YearMonthField::Month,
                },
            ),
            (
                "INTERVAL HOUR TO SECOND",
                DataType::DayTimeInterval {
                    start: DayTimeField::Hour,
                    end: DayTimeField::Second,
                },
            ),
            (
                "interval day",
                DataType::DayTimeInterval {
                    start: DayTimeField::Day,
                    end: DayTimeField::Day,
                },
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(strict(input), Some(expected.clone()));
            assert_eq!(lenient(input), expected);
        }
        assert_eq!(strict("interval month to year"), None);
        assert_eq!(lenient("interval day to month"), DataType::Unresolved);
    }

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
        // decimal(p) defaults the scale to 18.
        assert_eq!(
            lenient("decimal(10)"),
            DataType::Decimal {
                precision: 10,
                scale: 18
            }
        );
        // Bare / malformed decimal defaults to (38,18).
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

    #[test]
    fn not_null_qualifier_is_stripped_in_both_modes() {
        assert_eq!(strict("int not null"), Some(DataType::Integer));
        assert_eq!(strict("BIGINT NOT NULL"), Some(DataType::Long));
        assert_eq!(lenient("bigint null"), DataType::Long);
        assert_eq!(lenient("array<int> not null").to_string(), "array<integer>");
    }

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
        // Lenient mode keeps the array shape while degrading its element type.
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
        assert_eq!(parse_spark_schema(""), Some(StructType::empty()));
    }
}
