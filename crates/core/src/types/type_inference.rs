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
    ///
    /// Supports dot-notation for struct fields: `"person.name"` resolves
    /// to the `name` field within the `person` struct column.
    pub fn column_type(name: &str, schema: &StructType) -> DataType {
        if let Some(f) = schema.field_by_name(name) {
            return f.data_type.clone();
        }
        // Dot-notation: "struct_col.field_name"
        if let Some(dot_pos) = name.find('.') {
            let struct_name = &name[..dot_pos];
            let field_name = &name[dot_pos + 1..];
            if let Some(f) = schema.field_by_name(struct_name) {
                if let DataType::Struct(st) = &f.data_type {
                    return st
                        .field_by_name(field_name)
                        .map(|ff| ff.data_type.clone())
                        .unwrap_or(DataType::Unresolved);
                }
            }
        }
        DataType::Unresolved
    }

    /// Look up the nullability of `name` in `schema` (case-insensitive).
    /// Returns `true` (nullable) if not found — safe default.
    ///
    /// Supports dot-notation for struct fields: `"person.name"` resolves
    /// to `person.nullable || person.name.nullable` (Spark semantics).
    pub fn column_nullable(name: &str, schema: &StructType) -> bool {
        if let Some(f) = schema.field_by_name(name) {
            return f.nullable;
        }
        // Dot-notation: "struct_col.field_name"
        if let Some(dot_pos) = name.find('.') {
            let struct_name = &name[..dot_pos];
            let field_name = &name[dot_pos + 1..];
            if let Some(f) = schema.field_by_name(struct_name) {
                if let DataType::Struct(st) = &f.data_type {
                    let field_nullable = st
                        .field_by_name(field_name)
                        .map(|ff| ff.nullable)
                        .unwrap_or(true);
                    return f.nullable || field_nullable;
                }
            }
        }
        true
    }

    /// Look up the type of a qualified column reference in `schema`.
    ///
    /// When `qualifier` matches a struct-typed column, resolves `name`
    /// as a field within that struct. Otherwise falls back to flat lookup.
    pub fn qualified_column_type(
        name: &str,
        qualifier: Option<&str>,
        schema: &StructType,
    ) -> DataType {
        if let Some(q) = qualifier {
            // Try qualifier as struct column name
            if let Some(f) = schema.field_by_name(q) {
                if let DataType::Struct(st) = &f.data_type {
                    if let Some(ff) = st.field_by_name(name) {
                        return ff.data_type.clone();
                    }
                    // Handle nested dot-notation: name="address.city" →
                    // traverse struct fields recursively.
                    if let Some(dt) = Self::resolve_nested_field_type(name, st) {
                        return dt;
                    }
                }
            }
        }
        // Fall back to flat lookup
        Self::column_type(name, schema)
    }

    /// Look up the nullability of a qualified column reference in `schema`.
    ///
    /// When `qualifier` matches a struct-typed column, returns
    /// `struct_col.nullable || field.nullable` (Spark semantics).
    pub fn qualified_column_nullable(
        name: &str,
        qualifier: Option<&str>,
        schema: &StructType,
    ) -> bool {
        if let Some(q) = qualifier {
            if let Some(f) = schema.field_by_name(q) {
                if let DataType::Struct(st) = &f.data_type {
                    if let Some(ff) = st.field_by_name(name) {
                        return f.nullable || ff.nullable;
                    }
                    // Handle nested dot-notation: name="address.city" →
                    // traverse struct fields recursively for nullability.
                    if let Some(nullable) = Self::resolve_nested_field_nullable(name, st) {
                        return f.nullable || nullable;
                    }
                }
            }
        }
        Self::column_nullable(name, schema)
    }

    // ── Nested struct field resolution ──────────────────────────────────────

    /// Resolve a dot-separated field path within a struct type.
    /// e.g. `"address.city"` in `Struct<address: Struct<street: String, city: String>>`
    /// returns `Some(String)`.
    fn resolve_nested_field_type(path: &str, st: &StructType) -> Option<DataType> {
        let dot_pos = path.find('.')?;
        let head = &path[..dot_pos];
        let tail = &path[dot_pos + 1..];
        let field = st.field_by_name(head)?;
        if let DataType::Struct(inner_st) = &field.data_type {
            if let Some(ff) = inner_st.field_by_name(tail) {
                Some(ff.data_type.clone())
            } else {
                // Recurse for deeper nesting
                Self::resolve_nested_field_type(tail, inner_st)
            }
        } else {
            None
        }
    }

    /// Resolve the nullability of a dot-separated field path within a struct.
    /// Returns `Some(combined_nullable)` where combined is the OR of all
    /// intermediate struct nullabilities along the path.
    fn resolve_nested_field_nullable(path: &str, st: &StructType) -> Option<bool> {
        let dot_pos = path.find('.')?;
        let head = &path[..dot_pos];
        let tail = &path[dot_pos + 1..];
        let field = st.field_by_name(head)?;
        if let DataType::Struct(inner_st) = &field.data_type {
            if let Some(ff) = inner_st.field_by_name(tail) {
                Some(field.nullable || ff.nullable)
            } else {
                // Recurse for deeper nesting
                let inner_nullable = Self::resolve_nested_field_nullable(tail, inner_st)?;
                Some(field.nullable || inner_nullable)
            }
        } else {
            None
        }
    }

    // ── Lambda schema augmentation ───────────────────────────────────────────

    /// Creates a schema augmented with lambda parameter bindings.
    /// Binds lambda params to the array element type so lambda body
    /// expressions can resolve via schema lookup.
    pub fn augment_schema_with_lambda_params(
        schema: &StructType,
        param_names: &[String],
        element_type: &DataType,
        element_nullable: bool,
    ) -> StructType {
        let mut fields: Vec<crate::types::StructField> = schema
            .fields
            .iter()
            .filter(|f| !param_names.iter().any(|p| f.name.eq_ignore_ascii_case(p)))
            .cloned()
            .collect();
        if let Some(name) = param_names.first() {
            fields.push(crate::types::StructField::new(
                name.clone(),
                element_type.clone(),
                element_nullable,
            ));
        }
        if param_names.len() > 1 {
            fields.push(crate::types::StructField::new(
                param_names[1].clone(),
                DataType::Integer,
                false,
            ));
        }
        StructType::new(fields)
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
            (
                Decimal {
                    precision: p1,
                    scale: s1,
                },
                Decimal {
                    precision: p2,
                    scale: s2,
                },
            ) => Self::unify_decimal(*p1, *s1, *p2, *s2),

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
                if let Decimal {
                    precision: p2,
                    scale: s2,
                } = other_dec
                {
                    Self::unify_decimal(*precision, *scale, p2, s2)
                } else {
                    Decimal {
                        precision: *precision,
                        scale: *scale,
                    }
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

    /// Spark-compatible type unification (TypeCoercion.findTightestCommonType).
    /// Used for CaseWhen branches and Union schemas.
    pub fn unify_types(a: &DataType, b: &DataType) -> DataType {
        use DataType::*;
        match (a, b) {
            (Unresolved, _) => b.clone(),
            (_, Unresolved) => a.clone(),
            (Null, _) => b.clone(),
            (_, Null) => a.clone(),
            (x, y) if x == y => a.clone(),
            (x, y) if x.is_numeric() && y.is_numeric() => Self::promote_numeric(a, b),
            (Boolean, y) if y.is_numeric() => y.clone(),
            (x, Boolean) if x.is_numeric() => x.clone(),
            (Date, Timestamp) | (Timestamp, Date) => Timestamp,
            (x, y) if x.is_interval() && y.is_interval() => Interval,
            _ => String,
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
        // Spark: precision = p1 + p2 + 1, scale = s1 + s2 (with adjust_precision_scale for overflow)
        let raw_precision = p1 as i16 + p2 as i16 + 1;
        let raw_scale = s1 as i16 + s2 as i16;
        let (precision, scale) = Self::adjust_precision_scale(raw_precision, raw_scale);
        DataType::Decimal { precision, scale }
    }

    /// Spark's decimal division result type.
    pub fn decimal_div_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale_raw = 6i16.max(s1 as i16 + p2 as i16 + 1);
        let precision_raw = p1 as i16 - s1 as i16 + s2 as i16 + scale_raw;
        let (precision, scale) = Self::adjust_precision_scale(precision_raw, scale_raw);
        DataType::Decimal { precision, scale }
    }

    /// Spark's decimal modulo result type.
    pub fn decimal_mod_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale = s1.max(s2);
        let int_digits = (p1 as i16 - s1 as i16).min(p2 as i16 - s2 as i16);
        let (precision, scale_out) =
            Self::adjust_precision_scale(int_digits + scale as i16, scale as i16);
        DataType::Decimal {
            precision,
            scale: scale_out,
        }
    }

    // ── Aggregate return types ─────────────────────────────────────────────────

    /// Return type of an aggregate function given its argument type.
    /// Follows Spark's semantics exactly.
    pub fn aggregate_return_type(name: &str, arg_type: &DataType) -> DataType {
        use DataType::*;
        match name.to_lowercase().as_str() {
            // COUNT always returns Long
            "count" | "count_distinct" | "count_if" => Long,

            // SUM: integer types → Long, float → Double, decimal → wider decimal
            "sum" | "sum_distinct" => match arg_type {
                Byte | Short | Integer | Long => Long,
                Float => Double,
                Double => Double,
                Decimal { precision, scale } => {
                    let p = (*precision as u16 + 10).min(38) as u8;
                    Decimal {
                        precision: p,
                        scale: *scale,
                    }
                }
                _ => arg_type.clone(),
            },

            // AVG: integer types → Double, decimal → wider decimal
            "avg" | "mean" => match arg_type {
                Byte | Short | Integer | Long => Double,
                Float | Double => Double,
                Decimal { precision, scale } => {
                    let p = (*precision as u16 + 4).min(38) as u8;
                    let s = (*scale + 4).min(18).min(p);
                    Decimal {
                        precision: p,
                        scale: s,
                    }
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
            "collect_list" => Array(Box::new(arg_type.clone()), false),
            "collect_set" => Array(Box::new(arg_type.clone()), false),

            // approx_count_distinct → Long
            "approx_count_distinct" | "count_approx_distinct" => Long,

            // bit aggregates → same type
            "bit_and" | "bit_or" | "bit_xor" => arg_type.clone(),

            // bool aggregates → Boolean
            "bool_and" | "every" | "bool_or" | "any_value" => Boolean,

            // Scalar functions that can appear as wrappers around aggregates.
            // Without these, agg_expr_to_field would use the default `arg_type.clone()`,
            // returning the array type of the wrapped aggregate instead of the correct scalar type.
            "size" | "cardinality" | "map_size" | "array_size" => Integer,

            // GROUPING / GROUPING_ID — bitmask indicators for cube/rollup subtotals
            "grouping" => Byte,
            "grouping_id" => Long,

            _ => arg_type.clone(),
        }
    }

    /// Is this aggregate function always non-nullable? (COUNT is.)
    pub fn aggregate_is_non_nullable(name: &str) -> bool {
        matches!(
            name.to_lowercase().as_str(),
            "count" | "count_distinct" | "count_if"
        )
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

            // Aggregate window functions: delegate to aggregate_return_type
            agg => {
                let resolved = arg_type.unwrap_or(&DataType::Unresolved);
                let dt = Self::aggregate_return_type(agg, resolved);
                if dt == *resolved && matches!(resolved, DataType::Unresolved) {
                    DataType::Unresolved
                } else {
                    dt
                }
            }
        }
    }

    /// Aggregate functions that always return NULL for an empty group.
    /// Distinct from COUNT, which returns 0 (non-nullable).
    pub fn aggregate_is_always_nullable(name: &str) -> bool {
        matches!(
            name.to_lowercase().as_str(),
            "sum"
                | "sum_distinct"
                | "avg"
                | "mean"
                | "min"
                | "max"
                | "first"
                | "last"
                | "first_value"
                | "last_value"
                | "any_value"
                | "stddev"
                | "stddev_samp"
                | "stddev_pop"
                | "variance"
                | "var_samp"
                | "var_pop"
                | "percentile"
                | "percentile_approx"
                | "approx_percentile"
                | "collect_list"
                | "collect_set"
                | "array_agg"
                | "kurtosis"
                | "skewness"
                | "corr"
                | "covar_pop"
                | "covar_samp"
                | "regr_avgx"
                | "regr_avgy"
                | "regr_count"
                | "regr_r2"
                | "regr_slope"
                | "regr_intercept"
                | "bit_and"
                | "bit_or"
                | "bit_xor"
                | "bool_and"
                | "bool_or"
                | "every"
                | "nth_value"
        )
    }

    /// Is this window function non-nullable (ranking functions and COUNT are).
    pub fn window_is_non_nullable(name: &str) -> bool {
        matches!(
            name.to_lowercase().as_str(),
            "row_number"
                | "rank"
                | "dense_rank"
                | "ntile"
                | "percent_rank"
                | "cume_dist"
                | "count"
                | "count_distinct"
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
            | "concat_ws" | "substring" | "substr" | "replace" | "regexp_replace" | "translate"
            | "repeat" | "space" | "soundex" | "hex" | "base64" | "unbase64" | "decode"
            | "overlay" | "initcap" | "format_string" | "printf" | "from_unixtime"
            | "date_format" | "to_char" | "to_number" | "format_number" | "left" | "right"
            | "uuid" | "md5" | "sha" | "sha1" | "sha2" | "crc32" | "ascii" | "chr" | "char"
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
            "sqrt" | "exp" | "log" | "ln" | "log2" | "log10" | "sin" | "cos" | "tan" | "asin"
            | "acos" | "atan" | "atan2" | "sinh" | "cosh" | "tanh" | "asinh" | "acosh"
            | "atanh" | "degrees" | "radians" | "months_between" | "pow" | "power" | "hypot"
            | "expm1" | "log1p" => Double,

            // ── Math same-type (abs, ceil, floor, round, etc.) ───────────────
            "abs" | "negative" | "positive" => arg_types.first().cloned().unwrap_or(Double),
            "ceil" | "ceiling" | "floor" => match arg_types.first() {
                Some(Decimal { .. }) | Some(Double) | Some(Float) => Long,
                Some(t) => t.clone(),
                None => Long,
            },
            "round" | "bround" => arg_types.first().cloned().unwrap_or(Double),

            // ── Math → Integer ────────────────────────────────────────────────
            "sign" | "signum" => Double,
            "pmod" | "mod" | "int" => Integer,
            "factorial" => Long,
            "bit_count" => Integer,
            "bit_get" | "getbit" => Byte,

            // ── Math → same type as first arg ────────────────────────────────
            "shiftleft" | "shiftright" | "shiftrightunsigned" => {
                arg_types.first().cloned().unwrap_or(Long)
            }

            // ── Binary ────────────────────────────────────────────────────────
            "unhex" | "decode_binary" | "encode" => Binary,

            // ── Integer → String ──────────────────────────────────────────────
            "bin" => String,

            // ── Date → String ─────────────────────────────────────────────────
            "dayname" | "monthname" => String,

            // ── Date → Date ───────────────────────────────────────────────────
            "to_date" | "date_add" | "date_sub" | "add_months" | "last_day" | "next_day"
            | "make_date" | "trunc" => Date,

            // ── Date → Timestamp ──────────────────────────────────────────────
            // date_trunc(fmt, expr): always returns Timestamp in Spark (unlike trunc which → Date)
            "date_trunc" | "to_timestamp" | "to_timestamp_ntz" | "make_timestamp"
            | "date_trunc_ts" | "now" | "current_timestamp" => Timestamp,

            "current_date" | "curdate" => Date,

            // ── Date → Integer ────────────────────────────────────────────────
            "year" | "month" | "day" | "dayofmonth" | "dayofweek" | "dayofyear" | "weekofyear"
            | "quarter" | "hour" | "minute" | "second" | "extract" | "datediff" | "days"
            | "months" | "years" => Integer,

            // ── Array functions ────────────────────────────────────────────────
            "array" | "make_array" => {
                let elem = arg_types.first().cloned().unwrap_or(Unresolved);
                Array(Box::new(elem), true)
            }
            "array_distinct" | "array_sort" | "sort_array" | "array_reverse" | "slice" => arg_types
                .first()
                .cloned()
                .unwrap_or(Array(Box::new(Unresolved), true)),
            // flatten unwraps one nesting level: array<array<T>> → array<T>
            "flatten" => match arg_types.first() {
                Some(Array(inner, _)) => match inner.as_ref() {
                    Array(elem, nullable) => Array(elem.clone(), *nullable),
                    _ => arg_types[0].clone(),
                },
                _ => Array(Box::new(Unresolved), true),
            },
            // reverse: polymorphic — array<T> → array<T>, string → string
            "reverse" => match arg_types.first() {
                Some(Array(_, _)) => arg_types[0].clone(),
                _ => String,
            },
            // array_append / array_prepend: Spark conservatively sets containsNull=true
            "array_append" | "array_prepend" | "append_element" | "prepend_element" => {
                match arg_types.first() {
                    Some(Array(elem, _)) => Array(elem.clone(), true),
                    _ => arg_types
                        .first()
                        .cloned()
                        .unwrap_or(Array(Box::new(Unresolved), true)),
                }
            }
            "array_compact" => {
                // compact removes nulls → elements are not null
                match arg_types.first() {
                    Some(Array(elem, _)) => Array(elem.clone(), false),
                    Some(t) => t.clone(),
                    None => Array(Box::new(Unresolved), false),
                }
            }
            "array_union" | "array_intersect" | "array_except" | "array_concat" => arg_types
                .first()
                .cloned()
                .unwrap_or(Array(Box::new(Unresolved), true)),
            "transform" | "list_transform" => {
                // transform(array, x -> expr): return type is Array of whatever lambda returns
                // arg_types[1] is the lambda return type when available
                let elem = arg_types.get(1).cloned().unwrap_or(Unresolved);
                Array(Box::new(elem), true)
            }
            "filter" | "array_filter" | "list_filter" => arg_types
                .first()
                .cloned()
                .unwrap_or(Array(Box::new(Unresolved), true)),
            // ── HOF predicates ────────────────────────────────────────────
            "exists" | "forall" | "list_bool_or" | "list_bool_and" => Boolean,
            "aggregate" | "reduce" | "list_reduce" => {
                arg_types.get(1).cloned().unwrap_or(Unresolved)
            }
            "split" => Array(Box::new(String), false),
            "sequence" => {
                let elem = arg_types.first().cloned().unwrap_or(Long);
                Array(Box::new(elem), false)
            }
            "array_join" => String,
            "array_max" | "array_min" => match arg_types.first() {
                Some(Array(elem, _)) => *elem.clone(),
                Some(t) => t.clone(),
                None => Unresolved,
            },
            "size" | "array_size" | "cardinality" | "map_size" => Integer,
            "array_position" => Long,
            "element_at" => match arg_types.first() {
                Some(Array(elem, _)) => *elem.clone(),
                Some(Map { value, .. }) => *value.clone(),
                _ => Unresolved,
            },
            "explode" | "explode_outer" | "posexplode" | "posexplode_outer" | "inline"
            | "inline_outer" => match arg_types.first() {
                Some(Array(elem, _)) => *elem.clone(),
                _ => Unresolved,
            },

            // ── Map functions ─────────────────────────────────────────────────
            "map" | "create_map" => {
                let k = arg_types.first().cloned().unwrap_or(Unresolved);
                let v = arg_types.get(1).cloned().unwrap_or(Unresolved);
                Map {
                    key: Box::new(k),
                    value: Box::new(v),
                    value_nullable: true,
                }
            }
            "map_from_arrays" => {
                // map_from_arrays(Array<K>, Array<V>) → Map<K, V>
                let k = match arg_types.first() {
                    Some(Array(elem, _)) => *elem.clone(),
                    _ => arg_types.first().cloned().unwrap_or(Unresolved),
                };
                let (v, value_nullable) = match arg_types.get(1) {
                    Some(Array(elem, contains_null)) => (*elem.clone(), *contains_null),
                    _ => (arg_types.get(1).cloned().unwrap_or(Unresolved), true),
                };
                Map {
                    key: Box::new(k),
                    value: Box::new(v),
                    value_nullable,
                }
            }
            "map_from_entries" => {
                let k = arg_types.first().cloned().unwrap_or(Unresolved);
                let v = arg_types.get(1).cloned().unwrap_or(Unresolved);
                Map {
                    key: Box::new(k),
                    value: Box::new(v),
                    value_nullable: true,
                }
            }
            "map_keys" => {
                let k = match arg_types.first() {
                    Some(Map { key, .. }) => *key.clone(),
                    _ => Unresolved,
                };
                // Spark returns containsNull=true for map_keys
                Array(Box::new(k), true)
            }
            "map_values" => {
                let v = match arg_types.first() {
                    Some(Map { value, .. }) => *value.clone(),
                    _ => Unresolved,
                };
                Array(Box::new(v), true)
            }
            "map_concat" => arg_types.first().cloned().unwrap_or(Unresolved),
            "map_entries" => match arg_types.first() {
                Some(Map {
                    key,
                    value,
                    value_nullable,
                }) => {
                    // Spark: ArrayType(StructType([key: K NOT NULL, value: V nullable]), containsNull=false)
                    use crate::types::StructField;
                    let entry_struct = DataType::Struct(crate::types::StructType::new(vec![
                        StructField::not_null("key", *key.clone()),
                        StructField::new("value", *value.clone(), *value_nullable),
                    ]));
                    Array(Box::new(entry_struct), false)
                }
                _ => Unresolved,
            },

            // ── Struct ────────────────────────────────────────────────────────
            "to_csv" => String,
            "struct" | "named_struct" => Unresolved,

            // ── Hash / fingerprint ────────────────────────────────────────────
            // Spark hash() and murmur3 return signed INT32; xxhash64 returns
            // signed INT64. The previous shared arm was wrong for xxhash64.
            "hash" | "murmur3" => Integer,
            "xxhash64" => Long,

            // ── Null / conditional ────────────────────────────────────────────
            "coalesce" | "nvl" | "ifnull" => {
                // If any arg is Unresolved, we cannot determine the unified type.
                if arg_types.iter().any(|t| matches!(t, Unresolved)) {
                    Unresolved
                } else {
                    arg_types
                        .iter()
                        .filter(|t| !matches!(t, Null))
                        .cloned()
                        .reduce(|acc, t| Self::unify_types(&acc, &t))
                        .unwrap_or_else(|| arg_types.first().cloned().unwrap_or(Unresolved))
                }
            }
            "nullif" => arg_types.first().cloned().unwrap_or(Unresolved),
            "if" | "iff" | "nvl2" => arg_types.get(1).cloned().unwrap_or(Unresolved),
            // when(cond1, val1, cond2, val2, ..., [else]): unify THEN + ELSE values
            // Even args = no else (c1,v1,c2,v2,...); odd args = has else at end (c1,v1,...,else)
            "when" => {
                let has_else = arg_types.len() % 2 == 1;
                let pair_count = arg_types.len() / 2; // number of (cond, value) pairs
                                                      // THEN values are at odd indices: 1, 3, 5, ...
                let then_types = (0..pair_count).map(|i| &arg_types[i * 2 + 1]);
                let else_type = if has_else { arg_types.last() } else { None };
                then_types
                    .chain(else_type)
                    .filter(|t| !matches!(t, Unresolved | Null))
                    .cloned()
                    .reduce(|acc, t| Self::unify_types(&acc, &t))
                    .unwrap_or(Unresolved)
            }
            "nanvl" => arg_types.first().cloned().unwrap_or(Double),

            // ── JSON ─────────────────────────────────────────────────────────
            "get_json_object" | "json_extract_scalar" | "json_extract_string" => String,
            "from_json" => Unresolved, // struct; caller should provide schema
            "to_json" => String,
            "json_array_length" | "json_object_length" => Integer,
            "json_object_keys" => Array(Box::new(String), true),
            "schema_of_json" => String,

            // ── Misc ──────────────────────────────────────────────────────────
            "rand" | "random" | "randn" => Double,
            "monotonically_increasing_id" => Long,
            "spark_partition_id" => Integer,
            "input_file_name" | "input_file_block_start" | "input_file_block_length" => String,
            "current_user" | "user" | "current_schema" | "current_database" => String,
            "typeof" | "version" => String,
            "assert_true" => Boolean,
            "raise_error" => String,

            // ── Aggregate functions used in scalar context (e.g. sort_array(collect_list(...))) ──
            "collect_list" | "array_agg" => match arg_types.first() {
                Some(Array(_, _)) => arg_types[0].clone(), // already an array
                Some(t) => Array(Box::new(t.clone()), false),
                None => Array(Box::new(Unresolved), false),
            },
            "collect_set" => match arg_types.first() {
                Some(Array(_, _)) => arg_types[0].clone(),
                Some(t) => Array(Box::new(t.clone()), false),
                None => Array(Box::new(Unresolved), false),
            },

            // ── Count variants ────────────────────────────────────────────────
            "count_if" => Long,

            // ── Grouping functions (ROLLUP / CUBE) ──────────────────────────
            "grouping" => Byte,
            "grouping_id" => Long,

            // Aggregate functions — delegate to aggregate_return_type for correct Spark types
            "sum"
            | "sum_distinct"
            | "avg"
            | "mean"
            | "count"
            | "count_distinct"
            | "min"
            | "max"
            | "first"
            | "last"
            | "first_value"
            | "last_value"
            | "stddev"
            | "stddev_samp"
            | "std"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
            | "skewness"
            | "kurtosis"
            | "approx_count_distinct"
            | "count_approx_distinct"
            | "bit_and"
            | "bit_or"
            | "bit_xor" => Self::aggregate_return_type(
                name_lower.as_str(),
                arg_types.first().unwrap_or(&Unresolved),
            ),

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

    pub fn integral_to_decimal(dt: &DataType) -> DataType {
        match dt {
            DataType::Byte => DataType::Decimal {
                precision: 3,
                scale: 0,
            },
            DataType::Short => DataType::Decimal {
                precision: 5,
                scale: 0,
            },
            DataType::Integer => DataType::Decimal {
                precision: 10,
                scale: 0,
            },
            DataType::Long => DataType::Decimal {
                precision: 20,
                scale: 0,
            },
            other => other.clone(),
        }
    }

    /// Compute the minimum number of decimal digits needed to represent a value.
    /// Mirrors Spark's `DecimalType.fromValue()` / `DecimalPrecision` catalyst rule
    /// which narrows integer literals to the smallest decimal precision for arithmetic.
    pub fn decimal_digits_for_value(v: i64) -> u8 {
        if v == 0 {
            return 1;
        }
        let abs = v.unsigned_abs();
        // floor(log10(abs)) + 1
        let digits = (abs as f64).log10().floor() as u8 + 1;
        digits.max(1)
    }

    fn adjust_precision_scale(raw_precision: i16, raw_scale: i16) -> (u8, u8) {
        if raw_precision <= 38 {
            (raw_precision as u8, raw_scale as u8)
        } else {
            let int_digits = raw_precision - raw_scale;
            let min_scale = raw_scale.min(6);
            let scale = ((38i16 - int_digits).max(min_scale)).max(0);
            let precision = (int_digits + scale).min(38).max(0);
            (precision as u8, scale as u8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_promotion() {
        assert_eq!(
            TypeInferenceEngine::promote_numeric(&DataType::Integer, &DataType::Long),
            DataType::Long
        );
        assert_eq!(
            TypeInferenceEngine::promote_numeric(&DataType::Integer, &DataType::Double),
            DataType::Double
        );
        assert_eq!(
            TypeInferenceEngine::promote_numeric(&DataType::Float, &DataType::Long),
            DataType::Double
        );
        assert_eq!(
            TypeInferenceEngine::promote_numeric(&DataType::Long, &DataType::Long),
            DataType::Long
        );
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

    /// Regression: `count_if(boolean_col)` must return Long, not Boolean.
    /// Prior to the fix the default `_ => arg_type.clone()` arm returned the
    /// argument type, which caused agg-020 (`count_if(F.col("active"))`) to
    /// advertise `Boolean` in the client schema while DuckDB returned HUGEINT.
    #[test]
    fn count_if_of_boolean_returns_long() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("count_if", &DataType::Boolean),
            DataType::Long
        );
    }

    /// Regression: `count_if(salary > 90000)` — the argument type is Boolean
    /// (the `>` comparison result), same as agg2-006. Must still return Long.
    #[test]
    fn count_if_of_boolean_expression_returns_long() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("count_if", &DataType::Boolean),
            DataType::Long
        );
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
        assert_eq!(
            TypeInferenceEngine::window_return_type("row_number", None),
            DataType::Integer
        );
        assert_eq!(
            TypeInferenceEngine::window_return_type("rank", None),
            DataType::Integer
        );
        assert!(TypeInferenceEngine::window_is_non_nullable("row_number"));
    }

    #[test]
    fn unify_types_cases() {
        use DataType::*;
        // Same type
        assert_eq!(
            TypeInferenceEngine::unify_types(&Integer, &Integer),
            Integer
        );
        assert_eq!(TypeInferenceEngine::unify_types(&String, &String), String);
        // Numeric promotion
        assert_eq!(TypeInferenceEngine::unify_types(&Integer, &Long), Long);
        assert_eq!(TypeInferenceEngine::unify_types(&Float, &Double), Double);
        // Null is bottom type
        assert_eq!(TypeInferenceEngine::unify_types(&Null, &Integer), Integer);
        assert_eq!(TypeInferenceEngine::unify_types(&Integer, &Null), Integer);
        assert_eq!(TypeInferenceEngine::unify_types(&Null, &String), String);
        // Unresolved propagation
        assert_eq!(TypeInferenceEngine::unify_types(&Unresolved, &Long), Long);
        assert_eq!(TypeInferenceEngine::unify_types(&Long, &Unresolved), Long);
        // Date + Timestamp
        assert_eq!(
            TypeInferenceEngine::unify_types(&Date, &Timestamp),
            Timestamp
        );
        assert_eq!(
            TypeInferenceEngine::unify_types(&Timestamp, &Date),
            Timestamp
        );
        // Incompatible non-numeric → String
        assert_eq!(TypeInferenceEngine::unify_types(&Boolean, &Date), String);
        // Boolean + numeric → numeric (Spark coercion)
        assert_eq!(
            TypeInferenceEngine::unify_types(&Boolean, &Integer),
            Integer
        );
        assert_eq!(TypeInferenceEngine::unify_types(&Long, &Boolean), Long);
    }

    #[test]
    fn decimal_div_type_cases() {
        use DataType::*;
        // Decimal(10,2) / Decimal(5,1) → scale=max(6,2+5+1)=8, prec=10-2+1+8=17
        assert_eq!(
            TypeInferenceEngine::decimal_div_type(10, 2, 5, 1),
            Decimal {
                precision: 17,
                scale: 8
            }
        );
        // Decimal(38,2) / Decimal(38,2) → overflow case
        let result = TypeInferenceEngine::decimal_div_type(38, 2, 38, 2);
        assert!(matches!(result, Decimal { precision: 38, .. }));
    }

    #[test]
    fn decimal_mod_type_cases() {
        use DataType::*;
        // Decimal(10,2) % Decimal(10,2) → scale=2, int=min(8,8)=8, prec=10
        assert_eq!(
            TypeInferenceEngine::decimal_mod_type(10, 2, 10, 2),
            Decimal {
                precision: 10,
                scale: 2
            }
        );
        // Decimal(10,2) % Decimal(5,1) → scale=2, int=min(8,4)=4, prec=6
        assert_eq!(
            TypeInferenceEngine::decimal_mod_type(10, 2, 5, 1),
            Decimal {
                precision: 6,
                scale: 2
            }
        );
    }

    #[test]
    fn avg_decimal_scale_cap() {
        use DataType::*;
        // AVG(Decimal(10,2)) → prec=14, scale=min(min(6,18),14)=6
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type(
                "avg",
                &Decimal {
                    precision: 10,
                    scale: 2
                }
            ),
            Decimal {
                precision: 14,
                scale: 6
            }
        );
        // AVG(Decimal(10,16)) → prec=14, scale=min(min(20,18),14)=14
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type(
                "avg",
                &Decimal {
                    precision: 10,
                    scale: 16
                }
            ),
            Decimal {
                precision: 14,
                scale: 14
            }
        );
    }

    #[test]
    fn column_lookup() {
        use crate::types::{StructField, StructType};
        let schema = StructType::new(vec![
            StructField::nullable("id", DataType::Long),
            StructField::not_null("code", DataType::String),
        ]);
        assert_eq!(
            TypeInferenceEngine::column_type("id", &schema),
            DataType::Long
        );
        assert_eq!(
            TypeInferenceEngine::column_type("ID", &schema),
            DataType::Long
        );
        assert_eq!(TypeInferenceEngine::column_nullable("code", &schema), false);
        assert_eq!(
            TypeInferenceEngine::column_type("missing", &schema),
            DataType::Unresolved
        );
    }

    #[test]
    fn exists_returns_boolean() {
        use DataType::*;
        assert_eq!(
            TypeInferenceEngine::function_return_type("exists", &[Array(Box::new(Integer), false)]),
            Boolean
        );
    }

    #[test]
    fn aggregate_returns_init_type() {
        use DataType::*;
        assert_eq!(
            TypeInferenceEngine::function_return_type(
                "aggregate",
                &[Array(Box::new(Integer), false), Long]
            ),
            Long
        );
    }

    #[test]
    fn augment_schema_adds_lambda_params() {
        use crate::types::{StructField, StructType};
        let schema = StructType::new(vec![StructField::nullable("id", DataType::Long)]);
        let augmented = TypeInferenceEngine::augment_schema_with_lambda_params(
            &schema,
            &["x".to_owned(), "i".to_owned()],
            &DataType::Integer,
            false,
        );
        assert_eq!(augmented.fields.len(), 3);
        assert_eq!(augmented.fields[1].name, "x");
        assert_eq!(augmented.fields[1].data_type, DataType::Integer);
        assert!(!augmented.fields[1].nullable);
        assert_eq!(augmented.fields[2].name, "i");
        assert_eq!(augmented.fields[2].data_type, DataType::Integer);
        assert!(!augmented.fields[2].nullable);
    }

    #[test]
    fn column_type_dot_notation_struct_field() {
        use crate::types::{StructField, StructType};
        let schema = StructType::new(vec![StructField::not_null(
            "person",
            DataType::Struct(StructType::new(vec![
                StructField::not_null("name", DataType::String),
                StructField::nullable("age", DataType::Integer),
            ])),
        )]);
        assert_eq!(
            TypeInferenceEngine::column_type("person.name", &schema),
            DataType::String
        );
        assert_eq!(
            TypeInferenceEngine::column_type("person.age", &schema),
            DataType::Integer
        );
        assert_eq!(
            TypeInferenceEngine::column_type("person.missing", &schema),
            DataType::Unresolved
        );
        assert_eq!(
            TypeInferenceEngine::column_type("missing.name", &schema),
            DataType::Unresolved
        );
    }

    #[test]
    fn column_nullable_dot_notation_struct_field() {
        use crate::types::{StructField, StructType};
        let schema = StructType::new(vec![
            StructField::not_null(
                "person",
                DataType::Struct(StructType::new(vec![
                    StructField::not_null("name", DataType::String),
                    StructField::nullable("age", DataType::Integer),
                ])),
            ),
            StructField::nullable(
                "nullable_person",
                DataType::Struct(StructType::new(vec![StructField::not_null(
                    "street",
                    DataType::String,
                )])),
            ),
        ]);
        // person NOT NULL, name NOT NULL => false
        assert!(!TypeInferenceEngine::column_nullable(
            "person.name",
            &schema
        ));
        // person NOT NULL, age NULLABLE => true
        assert!(TypeInferenceEngine::column_nullable("person.age", &schema));
        // nullable_person NULLABLE, street NOT NULL => true
        assert!(TypeInferenceEngine::column_nullable(
            "nullable_person.street",
            &schema
        ));
    }

    #[test]
    fn qualified_column_type_struct_field() {
        use crate::types::{StructField, StructType};
        let schema = StructType::new(vec![StructField::not_null(
            "person",
            DataType::Struct(StructType::new(vec![StructField::not_null(
                "name",
                DataType::String,
            )])),
        )]);
        assert_eq!(
            TypeInferenceEngine::qualified_column_type("name", Some("person"), &schema),
            DataType::String
        );
        // No qualifier -> flat lookup (won't find "name" at top level)
        assert_eq!(
            TypeInferenceEngine::qualified_column_type("name", None, &schema),
            DataType::Unresolved
        );
    }

    #[test]
    fn qualified_column_nullable_struct_field() {
        use crate::types::{StructField, StructType};
        let schema = StructType::new(vec![StructField::not_null(
            "person",
            DataType::Struct(StructType::new(vec![StructField::not_null(
                "name",
                DataType::String,
            )])),
        )]);
        assert!(!TypeInferenceEngine::qualified_column_nullable(
            "name",
            Some("person"),
            &schema
        ));
    }

    #[test]
    fn aggregate_return_type_grouping_returns_byte() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("grouping", &DataType::String),
            DataType::Byte,
        );
    }

    #[test]
    fn aggregate_return_type_grouping_id_returns_long() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("grouping_id", &DataType::String),
            DataType::Long,
        );
    }

    // ── Spark hash / xxhash64 ──────────────────────────────────────────────────

    /// Spark `hash` returns signed INT32 → DuckDB INTEGER.
    #[test]
    fn hash_return_type_is_integer() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("hash", &[DataType::Integer]),
            DataType::Integer,
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("hash", &[DataType::String, DataType::Long]),
            DataType::Integer,
        );
    }

    /// Spark `xxhash64` returns signed INT64 → DuckDB BIGINT. Regression for
    /// the previous shared arm that incorrectly returned Integer.
    #[test]
    fn xxhash64_return_type_is_long() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("xxhash64", &[DataType::Integer]),
            DataType::Long,
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type(
                "xxhash64",
                &[DataType::String, DataType::Long]
            ),
            DataType::Long,
        );
    }
}
