use thunderduck_core::types::{DataType, StructField, StructType};

use crate::error::Result;
use crate::proto::spark::connect as proto;

/// Convert a proto DataType to the core DataType.
pub fn proto_to_data_type(dt: &proto::DataType) -> Result<DataType> {
    use proto::data_type::Kind;
    match &dt.kind {
        None => Ok(DataType::Unresolved),
        Some(Kind::Null(_)) => Ok(DataType::Null),
        Some(Kind::Boolean(_)) => Ok(DataType::Boolean),
        Some(Kind::Byte(_)) => Ok(DataType::Byte),
        Some(Kind::Short(_)) => Ok(DataType::Short),
        Some(Kind::Integer(_)) => Ok(DataType::Integer),
        Some(Kind::Long(_)) => Ok(DataType::Long),
        Some(Kind::Float(_)) => Ok(DataType::Float),
        Some(Kind::Double(_)) => Ok(DataType::Double),
        Some(Kind::String(_)) => Ok(DataType::String),
        Some(Kind::Char(_)) => Ok(DataType::String),
        Some(Kind::VarChar(_)) => Ok(DataType::String),
        Some(Kind::Binary(_)) => Ok(DataType::Binary),
        Some(Kind::Date(_)) => Ok(DataType::Date),
        Some(Kind::Timestamp(_)) => Ok(DataType::Timestamp),
        Some(Kind::TimestampNtz(_)) => Ok(DataType::TimestampNtz),
        Some(Kind::YearMonthInterval(_)) => Ok(DataType::YearMonthInterval),
        Some(Kind::DayTimeInterval(_)) => Ok(DataType::DayTimeInterval),
        Some(Kind::CalendarInterval(_)) => Ok(DataType::YearMonthInterval), // best-effort
        Some(Kind::Decimal(d)) => {
            let precision = d.precision.unwrap_or(38) as u8;
            let scale = d.scale.unwrap_or(18) as u8;
            Ok(DataType::Decimal { precision, scale })
        }
        Some(Kind::Array(a)) => {
            let element_type = a.element_type.as_deref()
                .map(proto_to_data_type)
                .transpose()?
                .unwrap_or(DataType::Unresolved);
            Ok(DataType::Array(Box::new(element_type)))
        }
        Some(Kind::Map(m)) => {
            let key_type = m.key_type.as_deref()
                .map(proto_to_data_type)
                .transpose()?
                .unwrap_or(DataType::Unresolved);
            let value_type = m.value_type.as_deref()
                .map(proto_to_data_type)
                .transpose()?
                .unwrap_or(DataType::Unresolved);
            Ok(DataType::Map {
                key: Box::new(key_type),
                value: Box::new(value_type),
                value_nullable: m.value_contains_null,
            })
        }
        Some(Kind::Struct(s)) => Ok(DataType::Struct(proto_struct_to_struct_type(s)?)),
        Some(Kind::Udt(_)) => Ok(DataType::Unresolved),
        Some(Kind::Unparsed(_)) => Ok(DataType::Unresolved),
        Some(Kind::Variant(_)) => Ok(DataType::Unresolved),
        Some(Kind::Geometry(_)) => Ok(DataType::Unresolved),
        Some(Kind::Geography(_)) => Ok(DataType::Unresolved),
        Some(Kind::Time(_)) => Ok(DataType::Unresolved),
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
        DataType::Decimal { precision, scale } => {
            Kind::Decimal(proto::data_type::Decimal {
                precision: Some(*precision as i32),
                scale: Some(*scale as i32),
                type_variation_reference: 0,
            })
        }
        DataType::Array(elem) => Kind::Array(Box::new(proto::data_type::Array {
            element_type: Some(Box::new(data_type_to_proto(elem))),
            contains_null: true,
            type_variation_reference: 0,
        })),
        DataType::Map { key, value, value_nullable } => Kind::Map(Box::new(proto::data_type::Map {
            key_type: Some(Box::new(data_type_to_proto(key))),
            value_type: Some(Box::new(data_type_to_proto(value))),
            value_contains_null: *value_nullable,
            type_variation_reference: 0,
        })),
        DataType::Struct(s) => Kind::Struct(proto::data_type::Struct {
            fields: s.fields.iter().map(struct_field_to_proto).collect(),
            type_variation_reference: 0,
        }),
        DataType::Unresolved => Kind::String(proto::data_type::String::default()),
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

/// Convert proto struct fields to a `StructType` (used by both `proto_to_data_type` and callers).
pub fn proto_struct_to_struct_type(
    s: &proto::data_type::Struct,
) -> Result<StructType> {
    let fields: Result<Vec<StructField>> = s
        .fields
        .iter()
        .map(|f| {
            let dt = f.data_type.as_ref()
                .map(proto_to_data_type)
                .transpose()?
                .unwrap_or(DataType::Unresolved);
            Ok(if f.nullable {
                StructField::nullable(f.name.clone(), dt)
            } else {
                StructField::not_null(f.name.clone(), dt)
            })
        })
        .collect();
    Ok(StructType::new(fields?))
}
