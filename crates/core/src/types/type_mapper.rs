use super::DataType;

/// Maps Spark `DataType` ↔ DuckDB SQL type strings.
pub struct TypeMapper;

impl TypeMapper {
    /// Convert a Spark `DataType` to the DuckDB SQL type string used in CAST/DDL.
    pub fn to_duckdb(dt: &DataType) -> std::string::String {
        match dt {
            DataType::Boolean => "BOOLEAN".into(),
            DataType::Byte => "TINYINT".into(),
            DataType::Short => "SMALLINT".into(),
            DataType::Integer => "INTEGER".into(),
            DataType::Long => "BIGINT".into(),
            DataType::Float => "FLOAT".into(),
            DataType::Double => "DOUBLE".into(),
            DataType::Decimal { precision, scale } => format!("DECIMAL({precision},{scale})"),
            DataType::String => "VARCHAR".into(),
            DataType::Binary => "BLOB".into(),
            DataType::Date => "DATE".into(),
            DataType::Timestamp => "TIMESTAMP WITH TIME ZONE".into(),
            DataType::TimestampNtz => "TIMESTAMP".into(),
            DataType::YearMonthInterval => "INTERVAL".into(),
            DataType::DayTimeInterval => "INTERVAL".into(),
            DataType::Null => "NULL".into(),
            DataType::Unresolved => "VARCHAR".into(),
            DataType::Array(elem) => format!("{}[]", Self::to_duckdb(elem)),
            DataType::Map { key, value, .. } => {
                format!("MAP({}, {})", Self::to_duckdb(key), Self::to_duckdb(value))
            }
            DataType::Struct(_) => "STRUCT".into(),
        }
    }

    /// Parse a DuckDB type string into a Spark `DataType`.
    /// Handles common aliases (INT, TEXT, REAL, BOOL, etc.).
    pub fn from_duckdb(s: &str) -> DataType {
        let upper = s.trim().to_uppercase();

        // Decimal: DECIMAL(p,s) or NUMERIC(p,s)
        if upper.starts_with("DECIMAL(") || upper.starts_with("NUMERIC(") {
            if let Some((p, sc)) = Self::parse_decimal_params(&upper) {
                return DataType::Decimal { precision: p, scale: sc };
            }
        }

        // Array: TYPE[] or ARRAY(TYPE) or LIST(TYPE)
        if upper.ends_with("[]") {
            let inner = &s[..s.len() - 2];
            return DataType::Array(Box::new(Self::from_duckdb(inner)));
        }
        if upper.starts_with("LIST(") || upper.starts_with("ARRAY(") {
            let inner = Self::extract_single_type_arg(&upper);
            return DataType::Array(Box::new(Self::from_duckdb(inner)));
        }

        // Map: MAP(KeyType, ValueType)
        if upper.starts_with("MAP(") {
            if let Some((k, v)) = Self::parse_map_params(&upper) {
                return DataType::Map {
                    key: Box::new(Self::from_duckdb(k)),
                    value: Box::new(Self::from_duckdb(v)),
                    value_nullable: true,
                };
            }
        }

        match upper.as_str() {
            "BOOLEAN" | "BOOL" => DataType::Boolean,
            "TINYINT" | "INT1" => DataType::Byte,
            "SMALLINT" | "INT2" | "SHORT" => DataType::Short,
            "INTEGER" | "INT" | "INT4" | "SIGNED" => DataType::Integer,
            "BIGINT" | "INT8" | "LONG" => DataType::Long,
            "HUGEINT" | "INT16" => DataType::Long, // lossy but correct mapping
            "FLOAT" | "FLOAT4" | "REAL" => DataType::Float,
            "DOUBLE" | "FLOAT8" | "DOUBLE PRECISION" => DataType::Double,
            "DECIMAL" | "NUMERIC" => DataType::Decimal { precision: 38, scale: 18 },
            "VARCHAR" | "TEXT" | "STRING" | "CHAR" | "CHARACTER VARYING" => DataType::String,
            "BLOB" | "BYTEA" | "BINARY" | "VARBINARY" => DataType::Binary,
            "DATE" => DataType::Date,
            "TIMESTAMP" | "DATETIME" | "TIMESTAMP WITHOUT TIME ZONE" => DataType::TimestampNtz,
            "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => DataType::Timestamp,
            "INTERVAL" => DataType::DayTimeInterval,
            _ => DataType::Unresolved,
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn parse_decimal_params(upper: &str) -> Option<(u8, u8)> {
        // "DECIMAL(10,2)" → Some((10,2))
        let inner = upper
            .trim_start_matches("DECIMAL(")
            .trim_start_matches("NUMERIC(")
            .trim_end_matches(')');
        let mut parts = inner.splitn(2, ',');
        let p = parts.next()?.trim().parse::<u8>().ok()?;
        let s = parts.next()?.trim().parse::<u8>().ok()?;
        Some((p, s))
    }

    fn extract_single_type_arg(upper: &str) -> &str {
        // "LIST(INTEGER)" → "INTEGER"
        let start = upper.find('(').map(|i| i + 1).unwrap_or(0);
        let end = upper.rfind(')').unwrap_or(upper.len());
        &upper[start..end]
    }

    fn parse_map_params(upper: &str) -> Option<(&str, &str)> {
        // "MAP(VARCHAR, INTEGER)" → ("VARCHAR", "INTEGER")
        let inner = upper.trim_start_matches("MAP(").trim_end_matches(')');
        let comma = Self::find_top_level_comma(inner)?;
        Some((inner[..comma].trim(), inner[comma + 1..].trim()))
    }

    fn find_top_level_comma(s: &str) -> Option<usize> {
        let mut depth = 0i32;
        for (i, c) in s.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => return Some(i),
                _ => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_scalars() {
        for dt in [
            DataType::Boolean,
            DataType::Byte,
            DataType::Short,
            DataType::Integer,
            DataType::Long,
            DataType::Float,
            DataType::Double,
            DataType::String,
            DataType::Binary,
            DataType::Date,
        ] {
            let s = TypeMapper::to_duckdb(&dt);
            let back = TypeMapper::from_duckdb(&s);
            assert_eq!(back, dt, "round-trip failed for {dt}");
        }
    }

    #[test]
    fn decimal() {
        assert_eq!(TypeMapper::to_duckdb(&DataType::Decimal { precision: 18, scale: 4 }), "DECIMAL(18,4)");
        assert_eq!(TypeMapper::from_duckdb("DECIMAL(18,4)"), DataType::Decimal { precision: 18, scale: 4 });
    }

    #[test]
    fn array() {
        let dt = DataType::Array(Box::new(DataType::Integer));
        let s = TypeMapper::to_duckdb(&dt);
        assert_eq!(s, "INTEGER[]");
        assert_eq!(TypeMapper::from_duckdb("INTEGER[]"), dt);
    }

    #[test]
    fn duckdb_aliases() {
        assert_eq!(TypeMapper::from_duckdb("INT"), DataType::Integer);
        assert_eq!(TypeMapper::from_duckdb("TEXT"), DataType::String);
        assert_eq!(TypeMapper::from_duckdb("REAL"), DataType::Float);
        assert_eq!(TypeMapper::from_duckdb("HUGEINT"), DataType::Long);
    }
}
