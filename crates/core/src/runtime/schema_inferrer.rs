use duckdb::arrow::datatypes::DataType as ArrowDataType;

use crate::runtime::session::DuckDbSession;
use crate::types::{DataType, StructField, StructType};

/// Infers the output schema of a SQL query by executing it with `LIMIT 0`.
///
/// This avoids reading any data — DuckDB returns an empty batch with the full schema.
pub struct SchemaInferrer<'a> {
    session: &'a DuckDbSession,
}

impl<'a> SchemaInferrer<'a> {
    pub fn new(session: &'a DuckDbSession) -> Self {
        Self { session }
    }

    /// Infer the schema of the given SQL query.
    pub async fn infer_sql(&self, sql: &str) -> crate::error::Result<StructType> {
        // schema_of wraps the SQL in LIMIT 0 internally
        let schema = self.session.schema_of(sql).await?;
        Ok(arrow_schema_to_struct_type(&schema))
    }
}

/// Convert an Arrow schema to a `StructType`.
pub fn arrow_schema_to_struct_type(schema: &duckdb::arrow::datatypes::Schema) -> StructType {
    let fields = schema
        .fields()
        .iter()
        .map(|f| {
            let dt = arrow_type_to_core(f.data_type());
            if f.is_nullable() {
                StructField::nullable(f.name().clone(), dt)
            } else {
                StructField::not_null(f.name().clone(), dt)
            }
        })
        .collect();
    StructType::new(fields)
}

fn arrow_type_to_core(dt: &ArrowDataType) -> DataType {
    match dt {
        ArrowDataType::Null => DataType::Null,
        ArrowDataType::Boolean => DataType::Boolean,
        ArrowDataType::Int8 => DataType::Byte,
        ArrowDataType::Int16 => DataType::Short,
        ArrowDataType::Int32 => DataType::Integer,
        ArrowDataType::Int64 => DataType::Long,
        ArrowDataType::Float32 => DataType::Float,
        ArrowDataType::Float64 => DataType::Double,
        ArrowDataType::Utf8 | ArrowDataType::LargeUtf8 => DataType::String,
        ArrowDataType::Binary | ArrowDataType::LargeBinary => DataType::Binary,
        ArrowDataType::Date32 | ArrowDataType::Date64 => DataType::Date,
        ArrowDataType::Timestamp(_, _) => DataType::Timestamp,
        ArrowDataType::Decimal128(p, s) => DataType::Decimal {
            precision: *p,
            scale: (*s).max(0) as u8,
        },
        ArrowDataType::List(field) | ArrowDataType::LargeList(field) => {
            DataType::Array(Box::new(arrow_type_to_core(field.data_type())))
        }
        ArrowDataType::Map(field, _) => {
            // Arrow Map field is a Struct { key, value }
            if let ArrowDataType::Struct(fields) = field.data_type() {
                let key = fields.iter().find(|f| f.name() == "key")
                    .map(|f| arrow_type_to_core(f.data_type()))
                    .unwrap_or(DataType::Unresolved);
                let value = fields.iter().find(|f| f.name() == "value")
                    .map(|f| arrow_type_to_core(f.data_type()))
                    .unwrap_or(DataType::Unresolved);
                let value_nullable = fields.iter().find(|f| f.name() == "value")
                    .map(|f| f.is_nullable())
                    .unwrap_or(true);
                DataType::Map {
                    key: Box::new(key),
                    value: Box::new(value),
                    value_nullable,
                }
            } else {
                DataType::Unresolved
            }
        }
        ArrowDataType::Struct(fields) => {
            let struct_fields = fields
                .iter()
                .map(|f| {
                    let dt = arrow_type_to_core(f.data_type());
                    if f.is_nullable() {
                        StructField::nullable(f.name().clone(), dt)
                    } else {
                        StructField::not_null(f.name().clone(), dt)
                    }
                })
                .collect();
            DataType::Struct(StructType::new(struct_fields))
        }
        _ => DataType::Unresolved,
    }
}
