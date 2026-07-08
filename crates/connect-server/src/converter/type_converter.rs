use thunderduck_core::types::{DataType, StructField, StructType};

use crate::proto::spark::connect as proto;

/// Convert a proto DataType to the core DataType.
///
/// Total: kinds τ does not model (UDT, Variant, Geometry, …) map to
/// [`DataType::Unresolved`] rather than erroring.
pub fn proto_to_data_type(dt: &proto::DataType) -> DataType {
    use proto::data_type::Kind;
    match &dt.kind {
        None => DataType::Unresolved,
        Some(Kind::Null(_)) => DataType::Null,
        Some(Kind::Boolean(_)) => DataType::Boolean,
        Some(Kind::Byte(_)) => DataType::Byte,
        Some(Kind::Short(_)) => DataType::Short,
        Some(Kind::Integer(_)) => DataType::Integer,
        Some(Kind::Long(_)) => DataType::Long,
        Some(Kind::Float(_)) => DataType::Float,
        Some(Kind::Double(_)) => DataType::Double,
        Some(Kind::String(_)) => DataType::String,
        Some(Kind::Char(_)) => DataType::String,
        Some(Kind::VarChar(_)) => DataType::String,
        Some(Kind::Binary(_)) => DataType::Binary,
        Some(Kind::Date(_)) => DataType::Date,
        Some(Kind::Timestamp(_)) => DataType::Timestamp,
        Some(Kind::TimestampNtz(_)) => DataType::TimestampNtz,
        Some(Kind::YearMonthInterval(_)) => DataType::YearMonthInterval,
        Some(Kind::DayTimeInterval(_)) => DataType::DayTimeInterval,
        Some(Kind::CalendarInterval(_)) => DataType::YearMonthInterval, // best-effort
        Some(Kind::Decimal(d)) => {
            let precision = d.precision.unwrap_or(38) as u8;
            let scale = d.scale.unwrap_or(18) as u8;
            DataType::Decimal { precision, scale }
        }
        Some(Kind::Array(a)) => {
            let element_type = a
                .element_type
                .as_deref()
                .map(proto_to_data_type)
                .unwrap_or(DataType::Unresolved);
            DataType::Array(Box::new(element_type), a.contains_null)
        }
        Some(Kind::Map(m)) => {
            let key_type = m
                .key_type
                .as_deref()
                .map(proto_to_data_type)
                .unwrap_or(DataType::Unresolved);
            let value_type = m
                .value_type
                .as_deref()
                .map(proto_to_data_type)
                .unwrap_or(DataType::Unresolved);
            DataType::Map {
                key: Box::new(key_type),
                value: Box::new(value_type),
                value_nullable: m.value_contains_null,
            }
        }
        Some(Kind::Struct(s)) => DataType::Struct(proto_struct_to_struct_type(s)),
        Some(Kind::Udt(_)) => DataType::Unresolved,
        Some(Kind::Unparsed(_)) => DataType::Unresolved,
        Some(Kind::Variant(_)) => DataType::Unresolved,
        Some(Kind::Geometry(_)) => DataType::Unresolved,
        Some(Kind::Geography(_)) => DataType::Unresolved,
        Some(Kind::Time(_)) => DataType::Unresolved,
    }
}

/// Convert a core DataType back to a proto DataType (for analyze_plan schema response).
pub fn data_type_to_proto(dt: &DataType) -> proto::DataType {
    use proto::data_type::Kind;

    let kind = match dt {
        DataType::Null => Kind::Null(proto::data_type::Null::default()),
        DataType::Boolean => Kind::Boolean(proto::data_type::Boolean::default()),
        DataType::Byte => Kind::Byte(proto::data_type::Byte::default()),
        DataType::Short => Kind::Short(proto::data_type::Short::default()),
        DataType::Integer => Kind::Integer(proto::data_type::Integer::default()),
        DataType::Long => Kind::Long(proto::data_type::Long::default()),
        DataType::Float => Kind::Float(proto::data_type::Float::default()),
        DataType::Double => Kind::Double(proto::data_type::Double::default()),
        DataType::String => Kind::String(proto::data_type::String::default()),
        DataType::Binary => Kind::Binary(proto::data_type::Binary::default()),
        DataType::Date => Kind::Date(proto::data_type::Date::default()),
        DataType::Timestamp => Kind::Timestamp(proto::data_type::Timestamp::default()),
        DataType::TimestampNtz => Kind::TimestampNtz(proto::data_type::TimestampNtz::default()),
        DataType::YearMonthInterval => {
            Kind::YearMonthInterval(proto::data_type::YearMonthInterval::default())
        }
        DataType::DayTimeInterval => {
            Kind::DayTimeInterval(proto::data_type::DayTimeInterval::default())
        }
        DataType::Interval => Kind::CalendarInterval(proto::data_type::CalendarInterval::default()),
        DataType::Decimal { precision, scale } => Kind::Decimal(proto::data_type::Decimal {
            precision: Some(*precision as i32),
            scale: Some(*scale as i32),
            type_variation_reference: 0,
        }),
        DataType::Array(elem, cn) => Kind::Array(Box::new(proto::data_type::Array {
            element_type: Some(Box::new(data_type_to_proto(elem))),
            contains_null: *cn,
            type_variation_reference: 0,
        })),
        DataType::Map {
            key,
            value,
            value_nullable,
        } => Kind::Map(Box::new(proto::data_type::Map {
            key_type: Some(Box::new(data_type_to_proto(key))),
            value_type: Some(Box::new(data_type_to_proto(value))),
            value_contains_null: *value_nullable,
            type_variation_reference: 0,
        })),
        DataType::Struct(s) => Kind::Struct(proto::data_type::Struct {
            fields: s.fields.iter().map(struct_field_to_proto).collect(),
            type_variation_reference: 0,
        }),
        DataType::Unresolved => Kind::Unparsed(proto::data_type::Unparsed {
            data_type_string: "unresolved".to_string(),
        }),
    };

    proto::DataType { kind: Some(kind) }
}

fn struct_field_to_proto(f: &StructField) -> proto::data_type::StructField {
    proto::data_type::StructField {
        name: f.name.clone(),
        data_type: Some(data_type_to_proto(&f.data_type)),
        nullable: f.nullable,
        metadata: None,
    }
}

/// Parse a Spark type string (e.g. "int", "bigint", "decimal(10,2)") into a DataType.
/// Used when Cast arrives as TypeStr rather than a structured proto DataType.
///
/// Thin wrapper over the shared union-grammar parser
/// (`thunderduck_core::types::spark_ddl` — pass-2 simplification consolidated
/// the two legacy type-string grammars there). Acceptance is strictly-additively
/// wider than the legacy local parser: `struct<name:type,...>` now parses
/// instead of yielding `Unresolved`, `blob` maps to Binary, and a bare `null`
/// token now parses to `DataType::Null`. Everything the legacy parser accepted
/// parses identically; unknown input still yields `DataType::Unresolved`.
pub fn parse_type_str(s: &str) -> DataType {
    thunderduck_core::types::spark_ddl::parse_spark_type_lenient(s)
}

/// Convert proto struct fields to a `StructType` (used by both `proto_to_data_type` and callers).
///
/// Total, like [`proto_to_data_type`]: a missing field type maps to
/// [`DataType::Unresolved`].
pub fn proto_struct_to_struct_type(s: &proto::data_type::Struct) -> StructType {
    let fields = s
        .fields
        .iter()
        .map(|f| {
            let dt = f
                .data_type
                .as_ref()
                .map(proto_to_data_type)
                .unwrap_or(DataType::Unresolved);
            if f.nullable {
                StructField::nullable(f.name.clone(), dt)
            } else {
                StructField::not_null(f.name.clone(), dt)
            }
        })
        .collect();
    StructType::new(fields)
}
