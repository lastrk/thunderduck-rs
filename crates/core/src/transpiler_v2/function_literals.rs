use crate::types::{DataType, StructType};

pub(crate) fn parse_number_format(fmt: &str) -> Option<(u8, u8)> {
    let mut pre = 0u32;
    let mut post = 0u32;
    let mut seen_dot = false;
    for ch in fmt.trim().chars() {
        match ch {
            '9' | '0' => {
                if seen_dot {
                    post += 1;
                } else {
                    pre += 1;
                }
            }
            '.' if !seen_dot => seen_dot = true,
            ',' if !seen_dot => {}
            _ => return None,
        }
    }
    let precision = pre + post;
    (precision > 0 && precision <= 38).then_some((precision as u8, post as u8))
}

pub(crate) fn parse_from_json_schema(ddl: &str) -> Option<StructType> {
    crate::types::spark_ddl::parse_spark_schema(ddl)
}

pub(crate) fn parse_from_csv_schema(ddl: &str) -> Option<StructType> {
    let schema = crate::types::spark_ddl::parse_spark_schema(ddl)?;
    let nested = schema.fields.iter().any(|field| {
        matches!(
            field.data_type,
            DataType::Struct(_) | DataType::Array(_, _) | DataType::Map { .. }
        )
    });
    (!nested).then_some(schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_format_accepts_digit_templates() {
        assert_eq!(parse_number_format("999.99"), Some((5, 2)));
        assert_eq!(parse_number_format("9999"), Some((4, 0)));
        assert_eq!(parse_number_format("0.00"), Some((3, 2)));
        assert_eq!(parse_number_format("9,999.99"), Some((6, 2)));
        assert_eq!(parse_number_format("S999.99"), None);
        assert_eq!(parse_number_format(""), None);
    }

    #[test]
    fn csv_schema_is_flat() {
        let schema = parse_from_csv_schema("qty INT, label STRING, price DOUBLE").unwrap();
        assert_eq!(schema.fields.len(), 3);
        assert_eq!(schema.fields[0].data_type, DataType::Integer);
        assert_eq!(schema.fields[1].data_type, DataType::String);
        assert_eq!(schema.fields[2].data_type, DataType::Double);
        assert!(parse_from_csv_schema("a STRUCT<b:INT>").is_none());
        assert!(parse_from_csv_schema("a ARRAY<INT>").is_none());
    }

    #[test]
    fn json_schema_preserves_nested_structs() {
        let schema = parse_from_json_schema("a INT, c STRUCT<d:BOOLEAN>").unwrap();
        assert_eq!(schema.fields[0].data_type, DataType::Integer);
        let DataType::Struct(nested) = &schema.fields[1].data_type else {
            panic!("expected nested struct")
        };
        assert_eq!(nested.fields[0].data_type, DataType::Boolean);
    }
}
