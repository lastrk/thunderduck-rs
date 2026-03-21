use super::{DataType, StructType};

/// Centralized Spark-compatible type inference.
///
/// All type promotion and return-type rules live here so that the expression
/// system and SQL generator stay consistent with Spark semantics.
pub struct TypeInferenceEngine;

impl TypeInferenceEngine {
    // ── Column resolution ─────────────────────────────────────────────────────

    /// Look up the type of `name` in `schema` (case-insensitive).
    /// Returns `DataType::Unresolved` if the column is not found.
    pub fn column_type(name: &str, schema: &StructType) -> DataType {
        schema.field_by_name(name).map(|f| f.data_type.clone()).unwrap_or(DataType::Unresolved)
    }

    /// Look up the nullability of `name` in `schema` (case-insensitive).
    /// Returns `true` (nullable) if not found — safe default.
    pub fn column_nullable(name: &str, schema: &StructType) -> bool {
        schema.field_by_name(name).map(|f| f.nullable).unwrap_or(true)
    }

    // ── Numeric promotion ──────────────────────────────────────────────────────

    /// Promote two numeric types following Spark's rules:
    /// Byte < Short < Integer < Long < Float < Double.
    /// Decimal is unified separately.
    pub fn promote_numeric(left: &DataType, right: &DataType) -> DataType {
        use DataType::*;
        match (left, right) {
            // Unresolved propagates — we don't know the type yet
            (Unresolved, _) | (_, Unresolved) => Unresolved,

            // Same type
            (a, b) if a == b => a.clone(),

            // Decimal × Decimal → unified decimal
            (Decimal { precision: p1, scale: s1 }, Decimal { precision: p2, scale: s2 }) => {
                Self::unify_decimal(*p1, *s1, *p2, *s2)
            }

            // Any numeric + Double → Double
            (Double, _) | (_, Double) => Double,

            // Float + integral → Double (Spark behaviour)
            (Float, b) | (b, Float) if b.is_integral() => Double,
            (Float, Float) => Float,

            // Decimal + integral → Decimal (widened)
            (Decimal { precision, scale }, b) | (b, Decimal { precision, scale })
                if b.is_integral() =>
            {
                let other_dec = Self::integral_to_decimal(b);
                if let Decimal { precision: p2, scale: s2 } = other_dec {
                    Self::unify_decimal(*precision, *scale, p2, s2)
                } else {
                    Decimal { precision: *precision, scale: *scale }
                }
            }

            // Long + lower → Long
            (Long, _) | (_, Long) => Long,

            // Integer + lower → Integer
            (Integer, _) | (_, Integer) => Integer,

            // Short + lower → Short
            (Short, _) | (_, Short) => Short,

            // Byte + Byte
            (Byte, Byte) => Byte,

            _ => Double,
        }
    }

    /// Spark's decimal addition/subtraction result type.
    pub fn decimal_add_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale = s1.max(s2);
        let int_digits = (p1 as i16 - s1 as i16).max(p2 as i16 - s2 as i16);
        let precision = ((int_digits + scale as i16 + 1).min(38)) as u8;
        DataType::Decimal { precision, scale }
    }

    /// Spark's decimal multiplication result type.
    pub fn decimal_mul_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale = (s1 + s2).min(38);
        let precision = ((p1 as u16 + p2 as u16).min(38)) as u8;
        DataType::Decimal { precision, scale }
    }

    /// Spark's decimal division result type.
    pub fn decimal_div_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale_raw = 6i16.max(s1 as i16 + p2 as i16 + 1);
        let precision_raw = p1 as i16 - s1 as i16 + s2 as i16 + scale_raw;
        let (precision, scale) = Self::adjust_precision_scale(precision_raw, scale_raw);
        DataType::Decimal { precision, scale }
    }

    // ── Aggregate return types ─────────────────────────────────────────────────

    /// Return type of an aggregate function given its argument type.
    /// Follows Spark's semantics exactly.
    pub fn aggregate_return_type(name: &str, arg_type: &DataType) -> DataType {
        use DataType::*;
        match name.to_lowercase().as_str() {
            // COUNT always returns Long
            "count" | "count_distinct" => Long,

            // SUM: integer types → Long, float → Double, decimal → wider decimal
            "sum" | "sum_distinct" => match arg_type {
                Byte | Short | Integer | Long => Long,
                Float => Double,
                Double => Double,
                Decimal { precision, scale } => {
                    let p = (*precision as u16 + 10).min(38) as u8;
                    Decimal { precision: p, scale: *scale }
                }
                _ => arg_type.clone(),
            },

            // AVG: integer types → Double, decimal → wider decimal
            "avg" | "mean" => match arg_type {
                Byte | Short | Integer | Long => Double,
                Float | Double => Double,
                Decimal { precision, scale } => {
                    let p = (*precision as u16 + 4).min(38) as u8;
                    let s = (*scale + 4).min(38);
                    Decimal { precision: p, scale: s }
                }
                _ => arg_type.clone(),
            },

            // MIN/MAX: same type as argument
            "min" | "max" | "first" | "last" | "first_value" | "last_value" => arg_type.clone(),

            // STDDEV / VARIANCE → Double
            "stddev" | "stddev_samp" | "std" | "stddev_pop" | "variance" | "var_samp"
            | "var_pop" | "skewness" | "kurtosis" => Double,

            // Percentile → Double
            "percentile" | "percentile_approx" | "approx_percentile" => Double,

            // collect_list / collect_set → Array
            "collect_list" => Array(Box::new(arg_type.clone())),
            "collect_set" => Array(Box::new(arg_type.clone())),

            // approx_count_distinct → Long
            "approx_count_distinct" | "count_approx_distinct" => Long,

            // bit aggregates → same type
            "bit_and" | "bit_or" | "bit_xor" => arg_type.clone(),

            // bool aggregates → Boolean
            "bool_and" | "every" | "bool_or" | "any_value" => Boolean,

            _ => arg_type.clone(),
        }
    }

    /// Is this aggregate function always non-nullable? (COUNT is.)
    pub fn aggregate_is_non_nullable(name: &str) -> bool {
        matches!(name.to_lowercase().as_str(), "count" | "count_distinct")
    }

    // ── Window function return types ───────────────────────────────────────────

    pub fn window_return_type(name: &str, arg_type: Option<&DataType>) -> DataType {
        match name.to_lowercase().as_str() {
            // Ranking functions → Integer
            "row_number" | "rank" | "dense_rank" | "ntile" => DataType::Integer,

            // Float ranking
            "percent_rank" | "cume_dist" => DataType::Double,

            // Analytic: same as argument, or Long if none
            "lag" | "lead" | "first_value" | "last_value" | "nth_value" => {
                arg_type.cloned().unwrap_or(DataType::Long)
            }

            _ => arg_type.cloned().unwrap_or(DataType::Unresolved),
        }
    }

    /// Is this window function non-nullable (ranking functions are).
    pub fn window_is_non_nullable(name: &str) -> bool {
        matches!(
            name.to_lowercase().as_str(),
            "row_number" | "rank" | "dense_rank" | "ntile" | "percent_rank" | "cume_dist"
        )
    }

    // ── Function return types ─────────────────────────────────────────────────

    /// Infer the return type of a scalar/table function.
    pub fn function_return_type(name: &str, arg_types: &[DataType]) -> DataType {
        use DataType::*;
        let name_lower = name.to_lowercase();

        match name_lower.as_str() {
            // ── String functions ──────────────────────────────────────────────
            "upper" | "lower" | "trim" | "ltrim" | "rtrim" | "lpad" | "rpad" | "concat"
            | "concat_ws" | "substring" | "substr" | "replace" | "regexp_replace"
            | "translate" | "repeat" | "reverse" | "space" | "soundex" | "hex" | "unhex"
            | "base64" | "unbase64" | "encode" | "decode" | "overlay" | "initcap"
            | "format_string" | "printf" | "from_unixtime" | "date_format" | "to_char"
            | "to_number" | "format_number" | "left" | "right" | "uuid" | "md5"
            | "sha" | "sha1" | "sha2" | "crc32" | "ascii" | "chr" | "char"
            | "conv" | "sentences" | "url_decode" | "url_encode" | "string" => String,

            // ── String → Integer ─────────────────────────────────────────────
            "length" | "char_length" | "character_length" | "octet_length" | "bit_length"
            | "levenshtein" | "locate" | "position" | "instr" | "strpos" => Integer,

            // ── String → Long ────────────────────────────────────────────────
            "unix_timestamp" | "to_unix_timestamp" => Long,

            // ── Boolean functions ─────────────────────────────────────────────
            "isnull" | "isnotnull" | "isnan" | "isinf" | "like" | "rlike" | "contains"
            | "startswith" | "starts_with" | "endswith" | "ends_with" | "array_contains"
            | "arrays_overlap" | "map_contains_key" | "in" => Boolean,

            // ── Math → Double ─────────────────────────────────────────────────
            "sqrt" | "exp" | "log" | "ln" | "log2" | "log10" | "sin" | "cos" | "tan"
            | "asin" | "acos" | "atan" | "atan2" | "sinh" | "cosh" | "tanh" | "asinh"
            | "acosh" | "atanh" | "degrees" | "radians" | "months_between" | "pow"
            | "power" | "hypot" | "expm1" | "log1p" => Double,

            // ── Math same-type (abs, ceil, floor, round, etc.) ───────────────
            "abs" | "negative" | "positive" => {
                arg_types.first().cloned().unwrap_or(Double)
            }
            "ceil" | "ceiling" | "floor" => match arg_types.first() {
                Some(Decimal { .. }) | Some(Double) | Some(Float) => Long,
                Some(t) => t.clone(),
                None => Long,
            },
            "round" | "bround" => arg_types.first().cloned().unwrap_or(Double),

            // ── Math → Integer ────────────────────────────────────────────────
            "sign" | "signum" | "pmod" | "mod" | "int" | "factorial" => Integer,

            // ── Math → Long ───────────────────────────────────────────────────
            "shiftleft" | "shiftright" | "shiftrightunsigned" | "bit_count" | "bit_get"
            | "getbit" => Long,

            // ── Date → Date ───────────────────────────────────────────────────
            "to_date" | "date_add" | "date_sub" | "add_months" | "last_day" | "next_day"
            | "make_date" | "trunc" | "date_trunc" => Date,

            // ── Date → Timestamp ──────────────────────────────────────────────
            "to_timestamp" | "to_timestamp_ntz" | "make_timestamp" | "date_trunc_ts"
            | "now" | "current_timestamp" => Timestamp,

            "current_date" | "curdate" => Date,

            // ── Date → Integer ────────────────────────────────────────────────
            "year" | "month" | "day" | "dayofmonth" | "dayofweek" | "dayofyear"
            | "weekofyear" | "quarter" | "hour" | "minute" | "second"
            | "extract" | "datediff" | "days" | "months" | "years" => Integer,

            // ── Array functions ────────────────────────────────────────────────
            "array" | "make_array" => {
                let elem = arg_types.first().cloned().unwrap_or(Unresolved);
                Array(Box::new(elem))
            }
            "array_distinct" | "array_sort" | "sort_array" | "array_reverse"
            | "array_compact" | "flatten" | "slice" => {
                arg_types.first().cloned().unwrap_or(Array(Box::new(Unresolved)))
            }
            "array_union" | "array_intersect" | "array_except" | "array_concat" => {
                arg_types.first().cloned().unwrap_or(Array(Box::new(Unresolved)))
            }
            "transform" => {
                // transform(array, x -> expr): return type is Array of whatever lambda returns
                // arg_types[1] is the lambda return type when available
                let elem = arg_types.get(1).cloned().unwrap_or(Unresolved);
                Array(Box::new(elem))
            }
            "filter" | "array_filter" => {
                arg_types.first().cloned().unwrap_or(Array(Box::new(Unresolved)))
            }
            "array_join" => String,
            "array_max" | "array_min" => match arg_types.first() {
                Some(Array(elem)) => *elem.clone(),
                Some(t) => t.clone(),
                None => Unresolved,
            },
            "size" | "array_size" | "cardinality" | "map_size" => Integer,
            "array_position" => Long,
            "element_at" => match arg_types.first() {
                Some(Array(elem)) => *elem.clone(),
                Some(Map { value, .. }) => *value.clone(),
                _ => Unresolved,
            },
            "explode" | "explode_outer" | "posexplode" | "posexplode_outer"
            | "inline" | "inline_outer" => match arg_types.first() {
                Some(Array(elem)) => *elem.clone(),
                _ => Unresolved,
            },

            // ── Map functions ─────────────────────────────────────────────────
            "map" | "create_map" | "map_from_arrays" | "map_from_entries" => {
                let k = arg_types.first().cloned().unwrap_or(Unresolved);
                let v = arg_types.get(1).cloned().unwrap_or(Unresolved);
                Map { key: Box::new(k), value: Box::new(v), value_nullable: true }
            }
            "map_keys" => {
                let k = match arg_types.first() {
                    Some(Map { key, .. }) => *key.clone(),
                    _ => Unresolved,
                };
                Array(Box::new(k))
            }
            "map_values" => {
                let v = match arg_types.first() {
                    Some(Map { value, .. }) => *value.clone(),
                    _ => Unresolved,
                };
                Array(Box::new(v))
            }
            "map_concat" => arg_types.first().cloned().unwrap_or(Unresolved),
            "map_entries" => Unresolved, // struct array

            // ── Struct ────────────────────────────────────────────────────────
            "struct" | "named_struct" | "to_csv" => String,

            // ── Hash / fingerprint ────────────────────────────────────────────
            "hash" | "xxhash64" | "murmur3" => Integer,

            // ── Null / conditional ────────────────────────────────────────────
            "coalesce" | "nvl" | "ifnull" => {
                arg_types.first().cloned().unwrap_or(Unresolved)
            }
            "nullif" => arg_types.first().cloned().unwrap_or(Unresolved),
            "if" | "iff" | "nvl2" => arg_types.get(1).cloned().unwrap_or(Unresolved),
            "nanvl" => arg_types.first().cloned().unwrap_or(Double),

            // ── JSON ─────────────────────────────────────────────────────────
            "get_json_object" | "json_extract_scalar" | "json_extract_string" => String,
            "from_json" => Unresolved, // struct; caller should provide schema
            "to_json" => String,
            "json_array_length" | "json_object_length" => Integer,
            "json_object_keys" | "schema_of_json" => String,

            // ── Misc ──────────────────────────────────────────────────────────
            "rand" | "random" | "randn" => Double,
            "monotonically_increasing_id" => Long,
            "spark_partition_id" => Integer,
            "input_file_name" | "input_file_block_start" | "input_file_block_length" => String,
            "current_user" | "user" | "current_schema" | "current_database" => String,
            "typeof" | "version" => String,
            "assert_true" => Boolean,
            "raise_error" => String,

            // Fallback: return first arg type or Unresolved
            _ => arg_types.first().cloned().unwrap_or(Unresolved),
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn unify_decimal(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale = s1.max(s2);
        let int_digits = (p1 as i16 - s1 as i16).max(p2 as i16 - s2 as i16);
        let precision = ((int_digits + scale as i16).min(38)) as u8;
        DataType::Decimal { precision, scale }
    }

    fn integral_to_decimal(dt: &DataType) -> DataType {
        match dt {
            DataType::Byte => DataType::Decimal { precision: 3, scale: 0 },
            DataType::Short => DataType::Decimal { precision: 5, scale: 0 },
            DataType::Integer => DataType::Decimal { precision: 10, scale: 0 },
            DataType::Long => DataType::Decimal { precision: 20, scale: 0 },
            other => other.clone(),
        }
    }

    fn adjust_precision_scale(raw_precision: i16, raw_scale: i16) -> (u8, u8) {
        if raw_precision <= 38 {
            (raw_precision as u8, raw_scale as u8)
        } else {
            let int_digits = raw_precision - raw_scale;
            let min_scale = raw_scale.min(6);
            let scale = (38 - int_digits).max(min_scale);
            let precision = (int_digits + scale).min(38);
            (precision as u8, scale as u8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_promotion() {
        assert_eq!(TypeInferenceEngine::promote_numeric(&DataType::Integer, &DataType::Long), DataType::Long);
        assert_eq!(TypeInferenceEngine::promote_numeric(&DataType::Integer, &DataType::Double), DataType::Double);
        assert_eq!(TypeInferenceEngine::promote_numeric(&DataType::Float, &DataType::Long), DataType::Double);
        assert_eq!(TypeInferenceEngine::promote_numeric(&DataType::Long, &DataType::Long), DataType::Long);
    }

    #[test]
    fn aggregate_count_is_long_non_nullable() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("count", &DataType::Integer),
            DataType::Long
        );
        assert!(TypeInferenceEngine::aggregate_is_non_nullable("count"));
        assert!(!TypeInferenceEngine::aggregate_is_non_nullable("sum"));
    }

    #[test]
    fn sum_integer_to_long() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("sum", &DataType::Integer),
            DataType::Long
        );
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("sum", &DataType::Byte),
            DataType::Long
        );
    }

    #[test]
    fn avg_integer_to_double() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("avg", &DataType::Long),
            DataType::Double
        );
    }

    #[test]
    fn window_ranking_to_integer() {
        assert_eq!(TypeInferenceEngine::window_return_type("row_number", None), DataType::Integer);
        assert_eq!(TypeInferenceEngine::window_return_type("rank", None), DataType::Integer);
        assert!(TypeInferenceEngine::window_is_non_nullable("row_number"));
    }

    #[test]
    fn column_lookup() {
        use crate::types::{StructField, StructType};
        let schema = StructType::new(vec![
            StructField::nullable("id", DataType::Long),
            StructField::not_null("code", DataType::String),
        ]);
        assert_eq!(TypeInferenceEngine::column_type("id", &schema), DataType::Long);
        assert_eq!(TypeInferenceEngine::column_type("ID", &schema), DataType::Long);
        assert_eq!(TypeInferenceEngine::column_nullable("code", &schema), false);
        assert_eq!(TypeInferenceEngine::column_type("missing", &schema), DataType::Unresolved);
    }
}
