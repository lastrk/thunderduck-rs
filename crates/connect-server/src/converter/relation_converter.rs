//! Residual Spark JSON-schema parser.
//!
//! The legacy `RelationConverter` (proto → `LogicalPlan`) and its helpers were
//! removed once the τ path (`V2RelationConverter`) became the only production
//! converter (ADR-022). The sole survivor is [`parse_json_schema`], which the
//! v2 path still calls to decode a `createDataFrame` payload's declared schema
//! (`{"type":"struct","fields":[…]}`). Full deletion of the legacy converter
//! module is tracked by Slice K; this file is what remains reachable.

use thunderduck_core::types::{DataType, StructField, StructType};

use crate::converter::type_converter::parse_type_str;

/// Parse Spark JSON schema format: {"type":"struct","fields":[...]}
pub(super) fn parse_json_schema(json: &str) -> crate::error::Result<StructType> {
    // Find "fields":[ and extract the array content
    let fields_key = match json.find("\"fields\"") {
        Some(p) => p,
        None => return Ok(StructType::new(vec![])),
    };
    let after_key = &json[fields_key + 8..]; // skip `"fields"`
    let bracket_pos = match after_key.find('[') {
        Some(p) => p,
        None => return Ok(StructType::new(vec![])),
    };
    let array_content_start = fields_key + 8 + bracket_pos + 1;

    // Find the matching ] at the same depth
    let array_content = extract_json_array_content(&json[array_content_start..]);

    // Split array content into individual field objects `{...}`
    let field_jsons = split_json_objects(array_content);

    let mut fields = Vec::new();
    for obj in field_jsons {
        let obj = obj.trim();
        if obj.is_empty() {
            continue;
        }
        // Depth-aware lookups — a struct-typed field's JSON body contains
        // nested `"name"` and `"nullable"` keys inside `type.fields[…]`.
        // PySpark's serializer happens to emit keys in alphabetical order so
        // depth-blind lookup returned the OUTER value in practice, but any
        // future client (or a struct-of-struct with a client that orders
        // `type` before `nullable`) would silently pick up an INNER field's
        // `nullable` / `name` as the outer value. Use the depth-aware
        // helpers introduced by pass 88 so this can't happen.
        let name = top_level_string_value(obj, "name").unwrap_or_default();
        let nullable = top_level_bool_value(obj, "nullable").unwrap_or(true);
        // "type" can be a quoted string or a nested object.
        let dt = json_type_value(obj);
        if nullable {
            fields.push(StructField::nullable(name, dt));
        } else {
            fields.push(StructField::not_null(name, dt));
        }
    }
    Ok(StructType::new(fields))
}

/// Return the content inside the first `[...]` at depth 0.
fn extract_json_array_content(s: &str) -> &str {
    let mut depth = 1i32;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return &s[..i];
                }
            }
            _ => {}
        }
    }
    s
}

/// Split a JSON array body into individual top-level `{...}` strings.
fn split_json_objects(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, c) in s.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s_pos) = start {
                        result.push(&s[s_pos..=i]);
                        start = None;
                    }
                }
            }
            _ => {}
        }
    }
    result
}

/// Extract the DataType from the "type" field of a Spark JSON field object.
/// The type can be a simple string ("integer") or a nested object ({"type":"array",...}).
/// Delegates to [`parse_json_type_field`] which also handles nested-object
/// recursion so `array<struct<…>>` etc. round-trip correctly.
fn json_type_value(obj: &str) -> DataType {
    parse_json_type_field(obj, "type")
}

/// Parse a nested Spark JSON type object like {"type":"array","elementType":"integer",...}.
/// Nested type-valued keys (`elementType`, `keyType`, `valueType`, `fields`) recurse.
///
/// Depth-aware — top-level keys are located by scanning the outermost object
/// and skipping over nested `{…}`, `[…]`, and string literals. Naive
/// substring lookup would collide with nested `"type"` keys (e.g. inside
/// `fields`) and return the wrong value.
fn parse_json_type_object(obj: &str) -> DataType {
    let type_name = top_level_string_value(obj, "type").unwrap_or_default();
    match type_name.as_str() {
        "array" => {
            let elem = parse_json_type_field(obj, "elementType");
            let contains_null = top_level_bool_value(obj, "containsNull").unwrap_or(true);
            DataType::Array(Box::new(elem), contains_null)
        }
        "map" => {
            let key_dt = parse_json_type_field(obj, "keyType");
            let val_dt = parse_json_type_field(obj, "valueType");
            let value_nullable = top_level_bool_value(obj, "valueContainsNull").unwrap_or(true);
            DataType::Map {
                key: Box::new(key_dt),
                value: Box::new(val_dt),
                value_nullable,
            }
        }
        "struct" => match parse_json_schema(obj) {
            Ok(st) => DataType::Struct(st),
            Err(_) => DataType::Unresolved,
        },
        _ => DataType::Unresolved,
    }
}

/// Extract the value of `key` (`elementType`, `keyType`, `valueType`, or `type`)
/// as a `DataType`. Handles both quoted-string values (`"long"`) and nested
/// object values (`{"type":"struct","fields":[…]}`). Depth-aware so nested
/// keys with the same name don't collide.
fn parse_json_type_field(obj: &str, key: &str) -> DataType {
    match top_level_value_slice(obj, key) {
        Some(v) => {
            let v = v.trim_start();
            if v.starts_with('"') {
                let inner = &v[1..];
                let end = inner.find('"').unwrap_or(inner.len());
                parse_type_str(&inner[..end])
            } else if v.starts_with('{') {
                let obj_str = extract_json_braced_object(v);
                parse_json_type_object(obj_str)
            } else {
                DataType::Unresolved
            }
        }
        None => DataType::Unresolved,
    }
}

/// Return the value slice (everything after `:` up to the next top-level `,`
/// or the closing `}`) for the top-level `key` of a JSON object. Skips
/// nested `{…}`, `[…]`, and string literals so `"type"` inside `fields`
/// does not shadow the outer `"type"` key.
fn top_level_value_slice<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\"", key);
    // Find the object body (everything after the leading `{`).
    let body_start = obj.find('{')? + 1;
    let body = &obj[body_start..];
    // Scan body character-by-character at depth 0.
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    let bytes = body.as_bytes();
    while i < bytes.len() {
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        let c = bytes[i] as char;
        if in_string {
            match c {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        if depth == 0 {
            // At depth 0 of the object body, check if this position starts
            // with our target key.
            if body[i..].starts_with(&needle) {
                let after_key = &body[i + needle.len()..];
                let after_colon =
                    after_key.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
                return Some(after_colon);
            }
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Depth-aware version of a JSON string-value lookup — returns the
/// quoted-string value of a top-level key, ignoring same-named keys inside
/// nested objects.
fn top_level_string_value(obj: &str, key: &str) -> Option<String> {
    let v = top_level_value_slice(obj, key)?;
    let v = v.trim_start();
    if !v.starts_with('"') {
        return None;
    }
    let inner = &v[1..];
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

/// Depth-aware version of a JSON bool-value lookup.
fn top_level_bool_value(obj: &str, key: &str) -> Option<bool> {
    let v = top_level_value_slice(obj, key)?;
    let v = v.trim_start();
    if v.starts_with("true") {
        Some(true)
    } else if v.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Return the substring from the leading `{` through its matching `}`.
/// String literals in the JSON are skipped so `{"a":"}"}` matches correctly.
fn extract_json_braced_object(s: &str) -> &str {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match c {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[..=i];
                }
            }
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_json_schema — depth-aware nullable / name lookups (rev-fix 1) ──
    //
    // Regression for the review's High #1: a struct-typed field's JSON body
    // carries nested `"name"` / `"nullable"` keys inside `type.fields[…]`;
    // depth-blind lookup returned an INNER field's value if it came first
    // in the source ordering. Pass 88 activated the JSON schema preference
    // path in `convert_local_relation`, exposing this pre-existing bug on
    // any `StructField("parent", StructType([StructField(..., False)]), True)`
    // shape. The test constructs that exact shape (using key ordering that
    // WOULD have tripped the depth-blind version — nested `nullable=false`
    // appearing before the outer `nullable=true` in the source string) and
    // asserts the outer nullability wins.

    #[test]
    fn parse_json_schema_reads_outer_nullable_not_inner_when_nested_field_is_non_null() {
        // Handcrafted JSON with the nested `"nullable":false` positioned so
        // depth-blind `.find()` would (incorrectly) see it BEFORE the outer
        // `"nullable":true`. The `type` key is placed before the outer
        // `nullable` key to force this ordering.
        let json = r#"{
          "type":"struct",
          "fields":[
            {
              "name":"parent",
              "type":{
                "fields":[
                  {"name":"child","type":"integer","nullable":false,"metadata":{}}
                ],
                "type":"struct"
              },
              "nullable":true,
              "metadata":{}
            }
          ]
        }"#;
        let st = parse_json_schema(json).expect("parse must succeed");
        assert_eq!(st.fields.len(), 1);
        let parent = &st.fields[0];
        assert_eq!(parent.name, "parent");
        assert!(
            parent.nullable,
            "outer `parent` field must inherit its OWN `nullable=true`, \
             not the nested `child` field's `nullable=false`",
        );
        let inner = match &parent.data_type {
            thunderduck_core::types::DataType::Struct(s) => s,
            other => panic!("expected Struct type for parent, got {other:?}"),
        };
        assert_eq!(inner.fields.len(), 1);
        assert_eq!(inner.fields[0].name, "child");
        assert!(
            !inner.fields[0].nullable,
            "inner `child` field must retain its declared `nullable=false`",
        );
    }

    /// Companion: PySpark's alphabetised key ordering (what the corpus
    /// actually exercises) still parses correctly — pin the no-regression
    /// contract for the happy path.
    #[test]
    fn parse_json_schema_pyspark_alphabetised_key_order_round_trips() {
        // Matches `_schema.json()` output for
        // `Struct<parent: Struct<child: int nullable=false> nullable=true>`.
        let json = r#"{"fields":[{"metadata":{},"name":"parent","nullable":true,"type":{"fields":[{"metadata":{},"name":"child","nullable":false,"type":"integer"}],"type":"struct"}}],"type":"struct"}"#;
        let st = parse_json_schema(json).expect("parse must succeed");
        assert_eq!(st.fields.len(), 1);
        assert!(st.fields[0].nullable, "outer parent must be nullable");
        let inner = match &st.fields[0].data_type {
            thunderduck_core::types::DataType::Struct(s) => s,
            other => panic!("expected Struct, got {other:?}"),
        };
        assert!(
            !inner.fields[0].nullable,
            "inner child must be non-nullable",
        );
    }
}
