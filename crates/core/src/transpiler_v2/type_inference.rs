//! τ's own TypeInferenceEngine — Spark-compatible type inference.
//!
//! This is an **independent** re-implementation of the type engine (INV10:
//! τ imports only `DataType`, `StructField`, `StructType` from `crate::types`;
//! no re-use of `crate::types::TypeInferenceEngine`). Shape mirrors the legacy
//! engine so cross-checking is straightforward.

use crate::types::{DataType, StructField, StructType};

/// τ's Spark-compatible type inference engine.
///
/// Unit struct with associated functions — matches legacy shape.
pub struct TypeInferenceEngine;

impl TypeInferenceEngine {
    // ── Column resolution ────────────────────────────────────────────────────

    /// Look up the type of `name` in `schema` (case-insensitive).
    /// Returns `DataType::Unresolved` if the column is not found.
    ///
    /// Supports dot-notation for struct fields: `"person.name"` resolves
    /// to the `name` field within the `person` struct column.
    pub fn column_type(name: &str, schema: &StructType) -> DataType {
        if let Some(f) = schema.field_by_name(name) {
            return f.data_type.clone();
        }
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
    pub fn column_nullable(name: &str, schema: &StructType) -> bool {
        if let Some(f) = schema.field_by_name(name) {
            return f.nullable;
        }
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
    pub fn qualified_column_type(
        name: &str,
        qualifier: Option<&str>,
        schema: &StructType,
    ) -> DataType {
        if let Some(q) = qualifier {
            if let Some(f) = schema.field_by_name(q) {
                if let DataType::Struct(st) = &f.data_type {
                    if let Some(ff) = st.field_by_name(name) {
                        return ff.data_type.clone();
                    }
                    if let Some(dt) = Self::resolve_nested_field_type(name, st) {
                        return dt;
                    }
                }
            }
        }
        Self::column_type(name, schema)
    }

    /// Look up the nullability of a qualified column reference in `schema`.
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
                    if let Some(nullable) = Self::resolve_nested_field_nullable(name, st) {
                        return f.nullable || nullable;
                    }
                }
            }
        }
        Self::column_nullable(name, schema)
    }

    fn resolve_nested_field_type(path: &str, st: &StructType) -> Option<DataType> {
        let dot_pos = path.find('.')?;
        let head = &path[..dot_pos];
        let tail = &path[dot_pos + 1..];
        let field = st.field_by_name(head)?;
        if let DataType::Struct(inner_st) = &field.data_type {
            if let Some(ff) = inner_st.field_by_name(tail) {
                Some(ff.data_type.clone())
            } else {
                Self::resolve_nested_field_type(tail, inner_st)
            }
        } else {
            None
        }
    }

    fn resolve_nested_field_nullable(path: &str, st: &StructType) -> Option<bool> {
        let dot_pos = path.find('.')?;
        let head = &path[..dot_pos];
        let tail = &path[dot_pos + 1..];
        let field = st.field_by_name(head)?;
        if let DataType::Struct(inner_st) = &field.data_type {
            if let Some(ff) = inner_st.field_by_name(tail) {
                Some(field.nullable || ff.nullable)
            } else {
                let inner_nullable = Self::resolve_nested_field_nullable(tail, inner_st)?;
                Some(field.nullable || inner_nullable)
            }
        } else {
            None
        }
    }

    // ── Lambda schema augmentation ───────────────────────────────────────────

    /// Creates a schema augmented with lambda parameter bindings.
    pub fn augment_schema_with_lambda_params(
        schema: &StructType,
        param_names: &[String],
        element_type: &DataType,
        element_nullable: bool,
    ) -> StructType {
        let mut fields: Vec<StructField> = schema
            .fields
            .iter()
            .filter(|f| !param_names.iter().any(|p| f.name.eq_ignore_ascii_case(p)))
            .cloned()
            .collect();
        if let Some(name) = param_names.first() {
            fields.push(StructField::new(
                name.clone(),
                element_type.clone(),
                element_nullable,
            ));
        }
        if param_names.len() > 1 {
            fields.push(StructField::new(
                param_names[1].clone(),
                DataType::Integer,
                false,
            ));
        }
        StructType::new(fields)
    }

    // ── Numeric promotion ────────────────────────────────────────────────────

    /// Promote two numeric types following Spark's rules.
    pub fn promote_numeric(left: &DataType, right: &DataType) -> DataType {
        use DataType::*;
        match (left, right) {
            (Unresolved, _) | (_, Unresolved) => Unresolved,
            (a, b) if a == b => a.clone(),
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
            (Double, _) | (_, Double) => Double,
            (Float, b) | (b, Float) if b.is_integral() => Double,
            (Float, Float) => Float,
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
            (Long, _) | (_, Long) => Long,
            (Integer, _) | (_, Integer) => Integer,
            (Short, _) | (_, Short) => Short,
            (Byte, Byte) => Byte,
            _ => Double,
        }
    }

    /// Spark-compatible type unification (TypeCoercion.findTightestCommonType).
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

    // ── Decimal arithmetic formulas ──────────────────────────────────────────

    /// Spark's decimal addition/subtraction result type.
    pub fn decimal_add_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale = s1.max(s2);
        let int_digits = (p1 as i16 - s1 as i16).max(p2 as i16 - s2 as i16);
        let precision = ((int_digits + scale as i16 + 1).min(38)) as u8;
        DataType::Decimal { precision, scale }
    }

    /// Spark's decimal multiplication result type.
    pub fn decimal_mul_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
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

    // ── Aggregate return types ───────────────────────────────────────────────

    /// Return type of an aggregate function given its argument type.
    /// Follows Spark's semantics exactly.
    ///
    /// **Checklist §1.1 (`count_if`)** and **§1.3 (correlation family)** are
    /// enforced here — see the corresponding matches below.
    pub fn aggregate_return_type(name: &str, arg_type: &DataType) -> DataType {
        use DataType::*;
        match name.to_lowercase().as_str() {
            // COUNT family — checklist §1.1 pins `count_if` alongside `count`.
            "count" | "count_distinct" | "count_if" => Long,

            // SUM family: integer types → Long, float → Double, decimal → wider.
            // `try_sum` mirrors `sum` — Slice B checklist §1.4 adds it here.
            "sum" | "sum_distinct" | "try_sum" => match arg_type {
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

            // AVG family: integer types → Double, decimal → wider.
            // `try_avg` mirrors `avg` — Slice B checklist §1.4 adds it here.
            "avg" | "mean" | "try_avg" => match arg_type {
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

            // MIN / MAX / first / last / any_value — same type as argument.
            "min" | "max" | "first" | "last" | "first_value" | "last_value" | "any_value" => {
                arg_type.clone()
            }

            // STDDEV / VARIANCE / SKEWNESS / KURTOSIS / CORR / COVAR / REGR → Double.
            //
            // Checklist §1.3 pins the full 11-name correlation / covariance /
            // regression family here. All members also appear in
            // `aggregate_is_always_nullable`.
            "stddev" | "stddev_samp" | "std" | "stddev_pop" | "variance" | "var_samp"
            | "var_pop" | "skewness" | "kurtosis" | "corr" | "covar_samp" | "covar_pop"
            | "regr_slope" | "regr_r2" | "regr_intercept" | "regr_avgx" | "regr_avgy"
            | "regr_sxx" | "regr_sxy" | "regr_syy" => Double,

            // Percentile → Double.
            "percentile" | "percentile_approx" | "approx_percentile" => Double,

            // collect_list / collect_set → Array.
            "collect_list" | "collect_set" => Array(Box::new(arg_type.clone()), false),

            // approx_count_distinct → Long.
            "approx_count_distinct" | "count_approx_distinct" => Long,

            // Bit aggregates → same type as arg.
            "bit_and" | "bit_or" | "bit_xor" => arg_type.clone(),

            // Bool aggregates → Boolean.
            "bool_and" | "every" | "bool_or" => Boolean,

            // Scalar-wrapper size functions when they appear as aggregates.
            "size" | "cardinality" | "map_size" | "array_size" => Integer,

            // GROUPING / GROUPING_ID.
            "grouping" => Byte,
            "grouping_id" => Long,

            _ => arg_type.clone(),
        }
    }

    /// Is this aggregate function always non-nullable?
    ///
    /// Case-insensitive wrapper. Prefer
    /// [`Self::aggregate_is_non_nullable_lower`] on hot paths where the caller
    /// has already lowercased the name.
    ///
    /// Checklist §1.1 pins `count_if` here alongside `count`.
    pub fn aggregate_is_non_nullable(name: &str) -> bool {
        Self::aggregate_is_non_nullable_lower(&name.to_lowercase())
    }

    /// Fast-path sibling of [`Self::aggregate_is_non_nullable`].
    ///
    /// **Precondition:** `name_lower` MUST already be lowercase. Debug builds
    /// `debug_assert!` this; release builds trust the contract to avoid an
    /// unnecessary allocation.
    pub fn aggregate_is_non_nullable_lower(name_lower: &str) -> bool {
        debug_assert!(
            name_lower.chars().all(|c| !c.is_ascii_uppercase()),
            "aggregate_is_non_nullable_lower requires pre-lowercased input; got `{name_lower}`",
        );
        matches!(
            name_lower,
            "count"
                | "count_distinct"
                | "count_if"
                | "grouping"
                | "grouping_id"
                | "collect_list"
                | "collect_set"
                | "array_agg"
                | "approx_count_distinct"
                | "count_approx_distinct"
        )
    }

    /// Aggregate functions that always return NULL for an empty group.
    ///
    /// Case-insensitive wrapper. Prefer
    /// [`Self::aggregate_is_always_nullable_lower`] on hot paths where the
    /// caller has already lowercased the name.
    ///
    /// Checklist §1.3 pins the full 11-name correlation / covariance /
    /// regression family here.
    pub fn aggregate_is_always_nullable(name: &str) -> bool {
        Self::aggregate_is_always_nullable_lower(&name.to_lowercase())
    }

    /// Fast-path sibling of [`Self::aggregate_is_always_nullable`].
    ///
    /// **Precondition:** `name_lower` MUST already be lowercase. Debug builds
    /// `debug_assert!` this; release builds trust the contract to avoid an
    /// unnecessary allocation.
    pub fn aggregate_is_always_nullable_lower(name_lower: &str) -> bool {
        debug_assert!(
            name_lower.chars().all(|c| !c.is_ascii_uppercase()),
            "aggregate_is_always_nullable_lower requires pre-lowercased input; got `{name_lower}`",
        );
        matches!(
            name_lower,
            "sum"
                | "sum_distinct"
                | "try_sum"
                | "avg"
                | "mean"
                | "try_avg"
                | "min"
                | "max"
                | "first"
                | "last"
                | "first_value"
                | "last_value"
                | "any_value"
                | "stddev"
                | "stddev_samp"
                | "std"
                | "stddev_pop"
                | "variance"
                | "var_samp"
                | "var_pop"
                | "skewness"
                | "kurtosis"
                | "percentile"
                | "percentile_approx"
                | "approx_percentile"
                | "collect_list"
                | "collect_set"
                | "array_agg"
                | "corr"
                | "covar_pop"
                | "covar_samp"
                | "regr_avgx"
                | "regr_avgy"
                | "regr_count"
                | "regr_r2"
                | "regr_slope"
                | "regr_intercept"
                | "regr_sxx"
                | "regr_sxy"
                | "regr_syy"
                | "bit_and"
                | "bit_or"
                | "bit_xor"
                | "bool_and"
                | "bool_or"
                | "every"
                | "nth_value"
        )
    }

    // ── Window function return types ─────────────────────────────────────────

    /// Return type of a window function given the optional argument type.
    pub fn window_return_type(name: &str, arg_type: Option<&DataType>) -> DataType {
        match name.to_lowercase().as_str() {
            "row_number" | "rank" | "dense_rank" | "ntile" => DataType::Integer,
            "percent_rank" | "cume_dist" => DataType::Double,
            "lag" | "lead" | "first_value" | "last_value" | "nth_value" => {
                arg_type.cloned().unwrap_or(DataType::Long)
            }
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

    /// Is this window function non-nullable (ranking + COUNT).
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

    // ── Function return types (Slice A.1 seed) ──────────────────────────────

    /// Infer the return type of a scalar/table function.
    ///
    /// At Slice A.1, all seeded arms need at most the first argument's type
    /// (aggregate delegation) or nothing at all (hash/grouping). The signature
    /// takes `first_arg_type: Option<&DataType>` to avoid materializing an
    /// intermediate `Vec<DataType>` for arg types the current arms never read.
    /// Slice C.2 may grow additional signatures if scalar arms need multi-arg
    /// awareness.
    ///
    /// **Slice A.1 seed:** returns `DataType::Unresolved` for anything the
    /// aggregate roster does not handle. Slice C.2 grows the scalar arms.
    /// The count / hash / grouping arms that Slice A.1's checklist tests
    /// exercise are wired here.
    pub fn function_return_type(name: &str, first_arg_type: Option<&DataType>) -> DataType {
        use DataType::*;
        let name_lower = name.to_lowercase();
        match name_lower.as_str() {
            // Checklist §1.2 — hash family pins return-types deterministically.
            "hash" | "murmur3" => Integer,
            "xxhash64" => Long,

            // Grouping indicators.
            "grouping" => Byte,
            "grouping_id" => Long,

            // Delegate to aggregate_return_type for known aggregates.
            "count"
            | "count_distinct"
            | "count_if"
            | "sum"
            | "sum_distinct"
            | "try_sum"
            | "avg"
            | "mean"
            | "try_avg"
            | "min"
            | "max"
            | "first"
            | "last"
            | "first_value"
            | "last_value"
            | "any_value"
            | "stddev"
            | "stddev_samp"
            | "std"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
            | "skewness"
            | "kurtosis"
            | "corr"
            | "covar_samp"
            | "covar_pop"
            | "regr_slope"
            | "regr_r2"
            | "regr_intercept"
            | "regr_avgx"
            | "regr_avgy"
            | "regr_sxx"
            | "regr_sxy"
            | "regr_syy"
            | "percentile"
            | "percentile_approx"
            | "approx_percentile"
            | "approx_count_distinct"
            | "count_approx_distinct"
            | "bit_and"
            | "bit_or"
            | "bit_xor"
            | "bool_and"
            | "bool_or"
            | "every"
            | "collect_list"
            | "collect_set" => Self::aggregate_return_type(
                name_lower.as_str(),
                first_arg_type.unwrap_or(&Unresolved),
            ),

            // ── String functions ─────────────────────────────────────────
            // Most string functions return String; length family returns
            // Integer; regexp / like family returns Boolean.
            "concat" | "concat_ws" | "upper" | "lower" | "trim" | "ltrim" | "rtrim"
            | "substr" | "substring" | "left" | "right" | "lpad" | "rpad" | "replace"
            | "regexp_replace" | "regexp_extract" | "translate" | "reverse"
            | "initcap" | "space" | "repeat" | "overlay" | "format_string"
            | "format_number" | "base64" | "unbase64" | "url_encode" | "url_decode"
            | "encode" | "decode" | "soundex" | "sentences" => String,
            "length" | "char_length" | "character_length" | "octet_length" | "bit_length"
            | "levenshtein" | "instr" | "locate" | "position" | "ascii" | "unicode"
            | "find_in_set" | "regexp_count" | "regexp_instr" => Integer,
            "like" | "ilike" | "rlike" | "regexp_like" | "contains" | "startswith"
            | "starts_with" | "endswith" | "ends_with" | "isnull" | "isnotnull"
            | "isnan" | "eqnullsafe" => Boolean,
            "split" => DataType::Array(Box::new(String), false),
            "sha" | "sha1" | "sha2" | "md5" | "crc32" => String,

            // ── Math functions ───────────────────────────────────────────
            // Most math functions on numeric return Double.
            "sqrt" | "cbrt" | "exp" | "expm1" | "ln" | "log" | "log10" | "log2"
            | "log1p" | "pow" | "power" | "sin" | "cos" | "tan" | "asin" | "acos"
            | "atan" | "atan2" | "sinh" | "cosh" | "tanh" | "asinh" | "acosh"
            | "atanh" | "degrees" | "radians" | "e" | "pi" | "hypot" | "rand"
            | "randn" | "random" | "bround" => Double,
            // abs preserves arg type; ceil/floor return Long (Spark rule);
            // signum returns Double; round returns arg type.
            "abs" | "round" | "greatest" | "least" | "nvl" | "coalesce"
            | "nullif" | "nvl2" | "if" | "ifnull" => {
                first_arg_type.cloned().unwrap_or(Unresolved)
            }
            "ceil" | "ceiling" | "floor" => Long,
            "sign" | "signum" => Double,
            "factorial" => Long,
            "mod" | "pmod" => first_arg_type.cloned().unwrap_or(Integer),
            "bin" | "hex" => String,
            "unhex" => Binary,
            "conv" => String,
            "shiftleft" | "shiftright" | "shiftrightunsigned" | "bitwise_and"
            | "bitwise_or" | "bitwise_xor" | "bitwise_not" | "bit_count"
            | "bit_length_arg" | "bitwise_or_agg" => {
                first_arg_type.cloned().unwrap_or(Integer)
            }

            // ── Date/time functions ──────────────────────────────────────
            "current_date" => Date,
            "current_timestamp" | "now" => Timestamp,
            "date_add" | "date_sub" | "add_months" | "next_day"
            | "last_day" | "trunc" | "to_date" => Date,
            // `date_trunc(fmt, ts_or_date)` returns Timestamp when the
            // second arg is Timestamp, Date when the second arg is Date.
            // Without the second arg's type at this call site, default to
            // Timestamp (the common case in corpus).
            "date_trunc" => Timestamp,
            "months_between" => Double,
            "to_timestamp" | "from_unixtime" | "from_utc_timestamp" | "to_utc_timestamp"
            | "make_timestamp" | "make_date" => Timestamp,
            "date_format" | "date_part" => String,
            "year" | "month" | "day" | "dayofmonth" | "dayofweek" | "dayofyear"
            | "weekofyear" | "hour" | "minute" | "second" | "quarter" | "week"
            | "datediff" | "unix_timestamp" | "unix_micros" | "unix_millis"
            | "unix_seconds" | "extract" => Integer,

            // ── Higher-order array/map functions ─────────────────────────
            // Return type = first-arg type (the collection) for filters;
            // transform / zip_with produce a NEW array but at this
            // resolver we approximate with first-arg type. Downstream
            // corpus diagnostics will surface any element-type mismatch.
            "transform" | "list_transform" | "filter" | "list_filter"
            | "list_reverse" | "aggregate" | "list_reduce" | "reduce"
            | "zip_with" | "list_zip" | "map_filter" | "map_zip_with" => {
                first_arg_type.cloned().unwrap_or(Unresolved)
            }
            "exists" | "list_any" | "forall" | "list_all" | "array_contains"
            | "list_contains" => Boolean,

            // ── Type predicates / control ───────────────────────────────
            "typeof" => String,
            "spark_partition_id" => Integer,
            "monotonically_increasing_id" => Long,

            // Slice A.1 seed: everything else is unresolved.
            _ => Unresolved,
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn unify_decimal(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale = s1.max(s2);
        let int_digits = (p1 as i16 - s1 as i16).max(p2 as i16 - s2 as i16);
        let precision = ((int_digits + scale as i16).min(38)) as u8;
        DataType::Decimal { precision, scale }
    }

    /// Convert an integral type to its equivalent DECIMAL type.
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
    pub fn decimal_digits_for_value(v: i64) -> u8 {
        if v == 0 {
            return 1;
        }
        let abs = v.unsigned_abs();
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

// ── Canonical name lists used by symmetric-omission tests ────────────────────

/// Canonical aggregate names for the symmetric-omission mechanical checks
/// (§8 of the plan). Every entry must appear in exactly one of
/// `aggregate_is_non_nullable` XOR `aggregate_is_always_nullable`.
///
/// Promoted from `#[cfg(test)]` in Slice A.2's fix pass (review M3): the
/// SparkSQL front-end (`parser_v2::v2_lowering::is_aggregate_function_name`)
/// needs the canonical list at compile time to decide whether a projection
/// requires an `Aggregate` plan node. INV10-compliant — the constant lives
/// under the `transpiler_v2` tree that τ's front-ends are allowed to consume.
pub(crate) const AGGREGATE_NAMES: &[&str] = &[
    // COUNT family (non-nullable).
    "count",
    "count_distinct",
    "count_if",
    "grouping",
    "grouping_id",
    // SUM family (always-nullable).
    "sum",
    "sum_distinct",
    "try_sum",
    // AVG family.
    "avg",
    "mean",
    "try_avg",
    // MIN / MAX.
    "min",
    "max",
    "first",
    "last",
    "first_value",
    "last_value",
    "any_value",
    // STDDEV / VARIANCE.
    "stddev",
    "stddev_samp",
    "stddev_pop",
    "variance",
    "var_samp",
    "var_pop",
    "skewness",
    "kurtosis",
    // Correlation / covariance / regression family (checklist §1.3).
    "corr",
    "covar_samp",
    "covar_pop",
    "regr_slope",
    "regr_r2",
    "regr_intercept",
    "regr_avgx",
    "regr_avgy",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
    // Percentile.
    "percentile",
    "percentile_approx",
    "approx_percentile",
    // Collect.
    "collect_list",
    "collect_set",
    // Bit.
    "bit_and",
    "bit_or",
    "bit_xor",
    // Bool.
    "bool_and",
    "bool_or",
    "every",
];

/// The 11-name correlation / covariance / regression family (checklist §1.3).
#[cfg(test)]
pub(crate) const CORR_FAMILY_NAMES: &[&str] = &[
    "corr",
    "covar_samp",
    "covar_pop",
    "regr_slope",
    "regr_r2",
    "regr_intercept",
    "regr_avgx",
    "regr_avgy",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
];

/// The 3-name hash family (checklist §1.2) — non-nullable regardless of args.
#[cfg(test)]
pub(crate) const HASH_FAMILY_NAMES: &[&str] = &["hash", "murmur3", "xxhash64"];

#[cfg(test)]
mod tests {
    use super::*;

    // ── Checklist §1.1 — `count_if` ─────────────────────────────────────────

    #[test]
    fn count_if_returns_long() {
        for arg in [
            DataType::Boolean,
            DataType::Integer,
            DataType::String,
            DataType::Null,
        ] {
            assert_eq!(
                TypeInferenceEngine::aggregate_return_type("count_if", &arg),
                DataType::Long,
                "count_if({arg:?}) must return Long",
            );
        }
    }

    #[test]
    fn count_if_is_non_nullable() {
        assert!(TypeInferenceEngine::aggregate_is_non_nullable("count_if"));
    }

    #[test]
    fn count_if_case_insensitive() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("COUNT_IF", &DataType::Boolean),
            DataType::Long
        );
        assert!(TypeInferenceEngine::aggregate_is_non_nullable("Count_If"));
    }

    // ── Checklist §1.3 — correlation family ─────────────────────────────────

    #[test]
    fn corr_family_returns_double() {
        for name in CORR_FAMILY_NAMES {
            assert_eq!(
                TypeInferenceEngine::aggregate_return_type(name, &DataType::Integer),
                DataType::Double,
                "{name}(Integer) must return Double",
            );
        }
    }

    #[test]
    fn corr_family_is_always_nullable() {
        for name in CORR_FAMILY_NAMES {
            assert!(
                TypeInferenceEngine::aggregate_is_always_nullable(name),
                "{name} must be aggregate_is_always_nullable",
            );
        }
    }

    #[test]
    fn stddev_variance_family_still_double() {
        for name in [
            "stddev",
            "stddev_samp",
            "std",
            "stddev_pop",
            "variance",
            "var_samp",
            "var_pop",
            "skewness",
            "kurtosis",
        ] {
            assert_eq!(
                TypeInferenceEngine::aggregate_return_type(name, &DataType::Integer),
                DataType::Double,
                "{name}(Integer) must return Double",
            );
            assert!(
                TypeInferenceEngine::aggregate_is_always_nullable(name),
                "{name} must be aggregate_is_always_nullable",
            );
        }
    }

    // ── Count / sum / avg sanity anchors ────────────────────────────────────

    #[test]
    fn sum_integer_returns_long() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("sum", &DataType::Integer),
            DataType::Long
        );
    }

    #[test]
    fn sum_decimal_widens_precision() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type(
                "sum",
                &DataType::Decimal {
                    precision: 10,
                    scale: 2
                }
            ),
            DataType::Decimal {
                precision: 20,
                scale: 2
            }
        );
    }

    #[test]
    fn avg_integer_returns_double() {
        assert_eq!(
            TypeInferenceEngine::aggregate_return_type("avg", &DataType::Long),
            DataType::Double
        );
    }

    // ── Symmetric-omission mechanical checks (§8) ───────────────────────────

    /// §8.1 — every aggregate name in AGGREGATE_NAMES must appear in exactly
    /// one of `aggregate_is_non_nullable` XOR `aggregate_is_always_nullable`.
    #[test]
    fn every_aggregate_return_type_name_appears_in_a_nullability_predicate() {
        for name in AGGREGATE_NAMES {
            let non_null = TypeInferenceEngine::aggregate_is_non_nullable(name);
            let always_null = TypeInferenceEngine::aggregate_is_always_nullable(name);
            assert!(
                non_null ^ always_null,
                "aggregate `{name}` must appear in exactly one of \
                 aggregate_is_non_nullable ({non_null}) XOR \
                 aggregate_is_always_nullable ({always_null})",
            );
        }
    }

    /// §8.4 — corr/covar/regr family must be `→ Double` AND
    /// `aggregate_is_always_nullable` AND NOT `aggregate_is_non_nullable`.
    #[test]
    fn corr_family_is_symmetric_across_return_type_and_nullability_predicates() {
        for name in CORR_FAMILY_NAMES {
            assert_eq!(
                TypeInferenceEngine::aggregate_return_type(name, &DataType::Integer),
                DataType::Double,
                "{name} return-type must be Double",
            );
            assert!(
                TypeInferenceEngine::aggregate_is_always_nullable(name),
                "{name} must be aggregate_is_always_nullable",
            );
            assert!(
                !TypeInferenceEngine::aggregate_is_non_nullable(name),
                "{name} must NOT be aggregate_is_non_nullable",
            );
        }
    }

    // ── Column lookup sanity ────────────────────────────────────────────────

    #[test]
    fn column_lookup_case_insensitive() {
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
        assert!(!TypeInferenceEngine::column_nullable("code", &schema));
        assert_eq!(
            TypeInferenceEngine::column_type("missing", &schema),
            DataType::Unresolved
        );
    }

    // ── Slice B checklist §1.4 — try_sum / try_avg (aggregate) ──────────────

    /// `try_sum` and `try_avg` must be present in `AGGREGATE_NAMES` so the
    /// SparkSQL classifier (`is_aggregate_function_name`) picks them up.
    #[test]
    fn aggregate_names_contains_try_sum_and_try_avg() {
        assert!(
            AGGREGATE_NAMES.contains(&"try_sum"),
            "AGGREGATE_NAMES must contain try_sum (checklist §1.4)",
        );
        assert!(
            AGGREGATE_NAMES.contains(&"try_avg"),
            "AGGREGATE_NAMES must contain try_avg (checklist §1.4)",
        );
    }

    /// `try_divide` is a scalar function — it must NOT be in `AGGREGATE_NAMES`.
    #[test]
    fn aggregate_names_does_not_contain_try_divide() {
        assert!(
            !AGGREGATE_NAMES.contains(&"try_divide"),
            "AGGREGATE_NAMES must NOT contain try_divide (scalar per checklist §4.1)",
        );
    }

    /// `try_sum` return-type must mirror `sum` (Integer → Long, Double → Double,
    /// Decimal → widened).
    #[test]
    fn aggregate_return_type_try_sum_matches_sum_path() {
        for arg in [
            DataType::Byte,
            DataType::Short,
            DataType::Integer,
            DataType::Long,
            DataType::Float,
            DataType::Double,
            DataType::Decimal {
                precision: 10,
                scale: 2,
            },
        ] {
            assert_eq!(
                TypeInferenceEngine::aggregate_return_type("try_sum", &arg),
                TypeInferenceEngine::aggregate_return_type("sum", &arg),
                "try_sum({arg:?}) must return the same type as sum({arg:?})",
            );
        }
    }

    /// `try_avg` return-type must mirror `avg`.
    #[test]
    fn aggregate_return_type_try_avg_matches_avg_path() {
        for arg in [
            DataType::Byte,
            DataType::Short,
            DataType::Integer,
            DataType::Long,
            DataType::Float,
            DataType::Double,
            DataType::Decimal {
                precision: 10,
                scale: 2,
            },
        ] {
            assert_eq!(
                TypeInferenceEngine::aggregate_return_type("try_avg", &arg),
                TypeInferenceEngine::aggregate_return_type("avg", &arg),
                "try_avg({arg:?}) must return the same type as avg({arg:?})",
            );
        }
    }

    /// Both `try_sum` and `try_avg` must be in the always-nullable roster
    /// (empty groups return NULL, and `try_*` variants surface arithmetic
    /// overflows as NULL in place of errors).
    #[test]
    fn try_sum_and_try_avg_are_always_nullable() {
        assert!(TypeInferenceEngine::aggregate_is_always_nullable("try_sum"));
        assert!(TypeInferenceEngine::aggregate_is_always_nullable("try_avg"));
        assert!(!TypeInferenceEngine::aggregate_is_non_nullable("try_sum"));
        assert!(!TypeInferenceEngine::aggregate_is_non_nullable("try_avg"));
    }

    // ── Function dispatch sanity — hash family ──────────────────────────────

    #[test]
    fn hash_return_type_is_integer() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("hash", Some(&DataType::Integer)),
            DataType::Integer
        );
    }

    #[test]
    fn xxhash64_return_type_is_long() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("xxhash64", Some(&DataType::Integer)),
            DataType::Long
        );
    }

    #[test]
    fn murmur3_return_type_is_integer() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("murmur3", Some(&DataType::String)),
            DataType::Integer
        );
    }
}
