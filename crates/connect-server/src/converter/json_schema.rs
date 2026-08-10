//! Spark JSON-schema parser.
//!
//! [`parse_json_schema`] decodes a `createDataFrame` payload's declared schema
//! (`{"type":"struct","fields":[…]}`) — τ's `V2RelationConverter` uses it for
//! the LocalRelation path.

use serde_json::Value;
use thunderduck_core::types::{DataType, StructField, StructType};

use crate::converter::type_converter::parse_type_str;

/// Parse Spark JSON schema format: {"type":"struct","fields":[...]}
///
/// Lenient, total contract: JSON parse failure or a missing `"fields"` array
/// yields `StructType::empty()` — this function never fails.
pub(super) fn parse_json_schema(json: &str) -> StructType {
    match serde_json::from_str::<Value>(json) {
        Ok(v) => struct_from_value(&v),
        Err(_) => StructType::empty(),
    }
}

/// Decode a `{"type":"struct","fields":[…]}` JSON value into a [`StructType`].
/// A missing or non-array `"fields"` key yields an empty struct (lenient
/// contract); non-object entries in the array are skipped.
fn struct_from_value(value: &Value) -> StructType {
    let Some(fields) = value.get("fields").and_then(Value::as_array) else {
        return StructType::empty();
    };
    let fields = fields
        .iter()
        .filter(|f| f.is_object())
        .map(|f| {
            let name = f
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let nullable = f.get("nullable").and_then(Value::as_bool).unwrap_or(true);
            let dt = type_from_value(f.get("type"));
            if nullable {
                StructField::nullable(name, dt)
            } else {
                StructField::not_null(name, dt)
            }
        })
        .collect();
    StructType::new(fields)
}

/// Decode a type-valued JSON entry (`type`, `elementType`, `keyType`, or
/// `valueType`). Quoted-string values (`"long"`) delegate to
/// [`parse_type_str`]; object values recurse (array / map / struct); anything
/// else is [`DataType::Unresolved`].
fn type_from_value(v: Option<&Value>) -> DataType {
    match v {
        Some(Value::String(s)) => parse_type_str(s),
        Some(obj @ Value::Object(_)) => type_from_object(obj),
        _ => DataType::Unresolved,
    }
}

/// Decode a nested Spark JSON type object like
/// `{"type":"array","elementType":"integer","containsNull":true}`. Unknown
/// (or non-string) `"type"` discriminators map to [`DataType::Unresolved`].
fn type_from_object(obj: &Value) -> DataType {
    match obj.get("type").and_then(Value::as_str) {
        Some("array") => {
            let elem = type_from_value(obj.get("elementType"));
            let contains_null = obj
                .get("containsNull")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            DataType::Array(Box::new(elem), contains_null)
        }
        Some("map") => {
            let key = type_from_value(obj.get("keyType"));
            let value = type_from_value(obj.get("valueType"));
            let value_nullable = obj
                .get("valueContainsNull")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            DataType::Map {
                key: Box::new(key),
                value: Box::new(value),
                value_nullable,
            }
        }
        Some("struct") => DataType::Struct(struct_from_value(obj)),
        _ => DataType::Unresolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_schema_reads_outer_nullable_not_inner_when_nested_field_is_non_null() {
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
        let st = parse_json_schema(json);
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

    #[test]
    fn parse_json_schema_pyspark_alphabetised_key_order_round_trips() {
        let json = r#"{"fields":[{"metadata":{},"name":"parent","nullable":true,"type":{"fields":[{"metadata":{},"name":"child","nullable":false,"type":"integer"}],"type":"struct"}}],"type":"struct"}"#;
        let st = parse_json_schema(json);
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
