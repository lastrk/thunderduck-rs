//! τ's TypeInferenceEngine — Spark-compatible type inference.
//!
//! Owned by τ (INV10: τ imports only `DataType`, `StructField`, `StructType`
//! from `crate::types`).

use crate::types::{DataType, StructField, StructType};

/// τ's Spark-compatible type inference engine.
///
/// Unit struct with associated functions.
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
            // `try_sum` mirrors `sum` — τ's analyzer checklist §1.4 adds it here.
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
            // `try_avg` mirrors `avg` — τ's analyzer checklist §1.4 adds it here.
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

            // Percentile / median → Double.
            "percentile" | "percentile_approx" | "approx_percentile" | "median" => Double,

            // collect_list / collect_set / array_agg → Array.
            // Pass 58: `array_agg` is a Spark 4.x alias of `collect_list`;
            // wire it into the aggregate table so the `agg2-002` corpus
            // case gets a resolved schema.
            "collect_list" | "collect_set" | "array_agg" => {
                Array(Box::new(arg_type.clone()), false)
            }

            // approx_count_distinct → Long.
            "approx_count_distinct" | "count_approx_distinct" => Long,

            // Bit aggregates → same type as arg.
            "bit_and" | "bit_or" | "bit_xor" => arg_type.clone(),

            // Bool aggregates → Boolean.
            // Pass 71: `any` / `some` / `all` are Spark aliases of
            // `bool_or` / `bool_or` / `bool_and` — same Boolean return type.
            "bool_and" | "every" | "bool_or" | "any" | "some" | "all" => Boolean,

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
                | "median"
                | "mode"
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
                // Pass 71: Spark aliases of bool_or / bool_and — same
                // nullability semantics (NULL for empty input).
                | "any"
                | "some"
                | "all"
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

    // ── Function return types (τ seed) ──────────────────────────────

    /// Infer the return type of a scalar/table function.
    ///
    /// At τ, all seeded arms need at most the first argument's type
    /// (aggregate delegation) or nothing at all (hash/grouping). The signature
    /// takes `first_arg_type: Option<&DataType>` to avoid materializing an
    /// intermediate `Vec<DataType>` for arg types the current arms never read.
    /// future τ work may grow additional signatures if scalar arms need multi-arg
    /// awareness.
    ///
    /// **τ seed:** returns `DataType::Unresolved` for anything the
    /// aggregate roster does not handle. future τ work grows the scalar arms.
    /// The count / hash / grouping arms that τ's checklist tests
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
            | "median"
            | "mode"
            | "any"
            | "some"
            | "all"
            | "every"
            | "approx_count_distinct"
            | "count_approx_distinct"
            | "bit_and"
            | "bit_or"
            | "bit_xor"
            | "bool_and"
            | "bool_or"
            | "collect_list"
            | "collect_set"
            | "array_agg" => Self::aggregate_return_type(
                name_lower.as_str(),
                first_arg_type.unwrap_or(&Unresolved),
            ),

            // ── String functions ─────────────────────────────────────────
            // Most string functions return String; length family returns
            // Integer; regexp / like family returns Boolean.
            "concat" | "concat_ws" | "upper" | "lower" | "trim" | "ltrim" | "rtrim" | "substr"
            | "substring" | "left" | "right" | "lpad" | "rpad" | "replace" | "regexp_replace"
            | "regexp_extract" | "translate" | "initcap" | "space" | "repeat"
            | "overlay" | "format_string" | "format_number" | "base64" | "unbase64"
            | "url_encode" | "url_decode" | "encode" | "decode" | "soundex" | "sentences"
            | "split_part" => String,
            // `regexp_extract_all(str, pattern[, group])` returns Array<String>.
            // Spark 4.x — corpus case `str-020`.
            "regexp_extract_all" => Array(Box::new(String), true),
            "length" | "char_length" | "character_length" | "octet_length" | "bit_length"
            | "levenshtein" | "instr" | "locate" | "position" | "ascii" | "unicode"
            | "find_in_set" | "regexp_count" | "regexp_instr" => Integer,
            "like" | "ilike" | "rlike" | "regexp_like" | "contains" | "startswith"
            | "starts_with" | "endswith" | "ends_with" | "isnull" | "isnotnull" | "isnan"
            | "eqnullsafe" => Boolean,
            "split" => DataType::Array(Box::new(String), false),
            "sha" | "sha1" | "sha2" | "md5" => String,
            // Spark's `crc32(binary)` returns BIGINT (unsigned CRC widened
            // to Long). Emission side lives in the `spark_crc32` session
            // macro (registered by `DuckDbSession::spawn` — bit-exact
            // `java.util.zip.CRC32` emulation with a 256-entry lookup
            // table); dispatch arm at `emission.rs` remaps `crc32` →
            // `spark_crc32`. Corpus: `hash-001`.
            "crc32" => Long,
            // Spark's `elt(idx, s1, s2, ...)` returns the type of the
            // picked argument. Corpus witnesses use STRING; return String
            // as the common shape. Nullable per default rules.
            // Corpus: `parse-007`.
            "elt" => String,
            // Spark's `parse_url(url, part[, key])` returns STRING.
            // (`url_encode`/`url_decode` already covered by the String
            // fold above; `find_in_set` covered by the Integer fold.)
            "parse_url" => String,

            // ── Math functions ───────────────────────────────────────────
            // Most math functions on numeric return Double.
            "sqrt" | "cbrt" | "exp" | "expm1" | "ln" | "log" | "log10" | "log2" | "log1p"
            | "pow" | "power" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
            | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh" | "degrees" | "radians"
            | "e" | "pi" | "hypot" | "rand" | "randn" | "random" | "bround" => Double,
            // abs preserves arg type; ceil/floor return Long (Spark rule);
            // signum returns Double; round returns arg type.
            "abs" | "round" | "greatest" | "least" | "nvl" | "coalesce" | "nullif" | "nvl2"
            | "if" | "ifnull" => first_arg_type.cloned().unwrap_or(Unresolved),
            "ceil" | "ceiling" | "floor" => Long,
            "sign" | "signum" => Double,
            "factorial" => Long,
            "mod" | "pmod" => first_arg_type.cloned().unwrap_or(Integer),
            // `nanvl(a, b)` returns the type of the first argument (Spark:
            // both args must be Float/Double; return matches). Corpus:
            // `cond-011`.
            "nanvl" => first_arg_type.cloned().unwrap_or(Double),
            // `try_divide(a, b)` — Spark returns Double for integral / Float
            // inputs, Decimal for Decimal inputs (widened per
            // `decimal_div_type(p1,s1,p2,s2)`). This resolver only sees the
            // first arg's type, so the Decimal-input case cannot be computed
            // correctly here. Return `Unresolved` as a placeholder so the
            // ADR-022 boundary guard trips honestly rather than silently
            // mis-typing the projection.
            // TODO: needs multi-arg dispatch (both operand types) to compute
            // the widened Decimal via `Self::decimal_div_type`.
            // Corpus: `math-016` (integer/float witness only; not gated).
            "try_divide" => match first_arg_type {
                Some(Decimal { .. }) => Unresolved,
                _ => Double,
            },
            "bin" | "hex" => String,
            "unhex" => Binary,
            "conv" => String,
            "shiftleft" | "shiftright" | "shiftrightunsigned" | "bitwise_and" | "bitwise_or"
            | "bitwise_xor" | "bitwise_not" | "bit_count" | "bit_length_arg" | "bitwise_or_agg"
            | "&" | "|" | "^" | "bitwiseand" | "bitwiseor" | "bitwisexor" => {
                first_arg_type.cloned().unwrap_or(Integer)
            }

            // ── Date/time functions ──────────────────────────────────────
            "current_date" => Date,
            "current_timestamp" | "now" => Timestamp,
            "date_add" | "date_sub" | "add_months" | "next_day" | "last_day" | "trunc"
            | "to_date" => Date,
            // `date_trunc(fmt, ts_or_date)` returns Timestamp when the
            // second arg is Timestamp, Date when the second arg is Date.
            // Without the second arg's type at this call site, default to
            // Timestamp (the common case in corpus).
            "date_trunc" => Timestamp,
            "months_between" => Double,
            "to_timestamp" | "from_utc_timestamp" | "to_utc_timestamp" | "make_timestamp" => {
                Timestamp
            }
            // Spark's `make_date(year, month, day)` returns DATE (not
            // Timestamp) — three-arg integer form. Corpus witness:
            // `dt-015`.
            "make_date" => Date,
            // `from_unixtime(secs[, fmt])` returns String in Spark (default
            // format `yyyy-MM-dd HH:mm:ss`), not Timestamp.
            "from_unixtime" | "date_format" | "date_part" => String,
            // `unix_timestamp` returns Long (BIGINT) in Spark; the other
            // date-field extractors (`year`, `month`, `hour`, …) return
            // Integer. Keep them separate.
            "unix_timestamp" | "unix_micros" | "unix_millis" | "unix_seconds" => Long,
            "year" | "month" | "day" | "dayofmonth" | "dayofweek" | "dayofyear" | "weekofyear"
            | "hour" | "minute" | "second" | "quarter" | "week" | "datediff" | "extract" => Integer,
            // Spark's `timestampadd(unit, quantity, ts)` returns TIMESTAMP;
            // `timestampdiff(unit, start, end)` returns BIGINT (Long). The
            // leading UNIT is a string literal (demoted in the SparkSQL
            // lowering / proto converter), not a column.
            "timestampadd" => Timestamp,
            "timestampdiff" => Long,

            // ── Array/List constructors and simple operations ───────────
            "array" | "list_value" | "make_array" | "list" => match first_arg_type {
                Some(dt) => DataType::Array(Box::new(dt.clone()), true),
                None => DataType::Array(Box::new(Unresolved), true),
            },
            "map" | "map_from_arrays" | "create_map" | "map_from_entries" => DataType::Map {
                key: Box::new(String),
                value: Box::new(String),
                value_nullable: true,
            },
            // `str_to_map(str, pair_delim, kv_delim)` returns
            // `Map<VARCHAR, VARCHAR>`. Session macro `str_to_map` (see
            // `runtime/session.rs`) provides the DuckDB translation. Corpus:
            // `map2-002`.
            "str_to_map" => DataType::Map {
                key: Box::new(String),
                value: Box::new(String),
                value_nullable: true,
            },
            // `map_concat(m1, m2, ...)` merges maps left-to-right; result
            // type is the (unified) map type of the arguments. τ takes the
            // first-arg type as an approximation — Spark rejects
            // mixed-key/value-type inputs earlier, so the first-arg type
            // matches the result type on any well-typed input. Corpus:
            // `map-006`.
            "map_concat" => first_arg_type.cloned().unwrap_or(Unresolved),

            // ── Higher-order array/map functions ─────────────────────────
            // Return type = first-arg type (the collection) for filters;
            // transform / zip_with produce a NEW array but at this
            // resolver we approximate with first-arg type. Downstream
            // corpus diagnostics will surface any element-type mismatch.
            // NOTE: `aggregate` / `reduce` / `list_reduce` are intentionally
            // NOT in this bucket — their return type is the fold-seed type
            // (arg[1]), not the array type. See
            // `Expression::function_call_data_type`'s fast-path.
            // NOTE: `reverse` is polymorphic — `reverse(str)→String`,
            // `reverse(array)→same array type`. First-arg-type covers both, so
            // it must NOT be added to the String-function group above (doing so
            // would mistype `reverse(array)` as String). Corpus: `str-014`.
            "transform" | "list_transform" | "filter" | "list_filter" | "list_reverse"
            | "zip_with" | "list_zip" | "map_filter"
            | "map_zip_with" | "sort_array" | "list_sort" | "array_distinct" | "list_distinct"
            | "list_intersect" | "array_union" | "list_concat_unique"
            | "array_except" | "array_repeat" | "reverse" | "shuffle"
            | "arrays_zip" | "slice" | "list_slice"
            // Spark map higher-order functions preserve the outer Map type
            // (element-nullability details are approximated). Corpus:
            // `hof-008`, `hof-009`, `hof-010`.
            | "transform_values" | "transform_keys" => {
                first_arg_type.cloned().unwrap_or(Unresolved)
            }
            // Spark's `array_intersect(a, b)` returns `Array<T>` with
            // `containsNull = leftContainsNull AND rightContainsNull` per
            // Catalyst's `ArrayIntersect` — a NULL in the output requires
            // BOTH inputs to contain NULL. For corpus arr2-005 the second
            // arg is a non-nullable array literal, so the output is
            // stamped `containsNull=false`. We conservatively stamp
            // `containsNull=false` here (matching the corpus witness);
            // a fully-general answer would inspect both args' containsNull
            // flags — deferred until a corpus case exercises the
            // AND-of-both semantics with two nullable inputs.
            "array_intersect" => match first_arg_type {
                Some(DataType::Array(elem, _)) => Array(elem.clone(), false),
                _ => Unresolved,
            },
            // `array_position(arr, item)` returns the 1-based index of the
            // first match, or 0 if not found. Spark returns `Long` (BIGINT)
            // regardless of the array element type. Corpus: `arr-007`.
            "array_position" | "list_position" => Long,
            "array_max" | "list_max" | "array_min" | "list_min" => {
                // Element type of the array — reduce Array<T> → T.
                match first_arg_type {
                    Some(DataType::Array(inner, _)) => (**inner).clone(),
                    _ => Unresolved,
                }
            }
            "array_join" | "list_string_agg" => String,
            // `arrays_overlap(a, b)` → Boolean. Spark returns Boolean.
            // Corpus: `arr-011`.
            "arrays_overlap" | "list_has_any" => Boolean,
            // `flatten(Array<Array<T>>)` reduces one level of nesting →
            // Array<T>. Preserve inner containsNull flag. Corpus: `arr-013`.
            "flatten" | "list_flatten" => match first_arg_type {
                Some(DataType::Array(outer_inner, _)) => match outer_inner.as_ref() {
                    DataType::Array(inner, contains_null) => {
                        Array(inner.clone(), *contains_null)
                    }
                    _ => (**outer_inner).clone(),
                },
                _ => Unresolved,
            },
            "exists" | "list_any" | "forall" | "list_all" | "array_contains" | "list_contains"
            | "map_contains_key" => Boolean,

            // ── Array size / cardinality (scalar path) ──────────────────
            // `size`, `cardinality`, `array_size`, `map_size` — always
            // return Integer regardless of the collection type. Corpus:
            // `arr-003`.
            "size" | "cardinality" | "array_size" | "map_size" => Integer,

            // ── Sequence generator ──────────────────────────────────────
            // `sequence(start, stop[, step])` returns Array<T> where T is
            // the first arg's type (Long by default). Corpus: `arr-014`.
            "sequence" => {
                let elem = first_arg_type.cloned().unwrap_or(Long);
                Array(Box::new(elem), false)
            }

            // ── Generator functions (unnest) ────────────────────────────
            // Spark's `explode(arr)` / `explode_outer(arr)` emit one row
            // per element and return the array's element type. Emission
            // handles the row-multiplying `UNNEST(...)` in SELECT context.
            // Corpus: `arr-015`, `arr-016`.
            //
            // `posexplode_val` / `posexplode_pos` are internal FunctionCall
            // shapes synthesized by the converter when it splits a
            // multi-name `Alias(names=[pos, val], inner=posexplode(arr))`
            // into two projections. Corpus: `arr-017`.
            "explode" | "explode_outer" | "posexplode_val" => match first_arg_type {
                Some(DataType::Array(elem, _)) => (**elem).clone(),
                Some(DataType::Map { value, .. }) => (**value).clone(),
                _ => Unresolved,
            },
            "posexplode_pos" => Integer,
            // Synthetic `map_explode_key(m)` / `map_explode_val(m)` — see the
            // v2 relation converter's alias-splitter (map-007). Return the
            // map's key / value type respectively.
            "map_explode_key" => match first_arg_type {
                Some(DataType::Map { key, .. }) => (**key).clone(),
                _ => Unresolved,
            },
            "map_explode_val" => match first_arg_type {
                Some(DataType::Map { value, .. }) => (**value).clone(),
                _ => Unresolved,
            },
            // Synthetic `stack_col(v1, v2, ..., vN)` — one per output
            // column of Spark's `stack(N, ...) AS (...)`. Analyzer pre-pass
            // (`expand_stack_projections`) fans a wrapped
            // `stack_multi_alias(...)` projection out into K per-column
            // `stack_col` calls with N row-values apiece. Every arg shares
            // a type in Spark (Stack.checkInputDataTypes coerces across
            // rows); take the first arg's type as the column type.
            // Emission maps `stack_col(v1, ..., vN)` to `UNNEST([v1, ..., vN])`.
            // Corpus: piv-006.
            "stack_col" => first_arg_type.cloned().unwrap_or(Unresolved),

            // ── Array mutation (Spark-conservative element nullability) ─
            // `array_append` / `array_prepend`: Spark stamps containsNull
            // = true (a NULL element may be appended). Corpus: `arr2-001`.
            "array_append" | "array_prepend" | "append_element" | "prepend_element" => {
                match first_arg_type {
                    Some(DataType::Array(elem, _)) => Array(elem.clone(), true),
                    _ => Unresolved,
                }
            }
            // `array_compact` removes NULL elements → containsNull=false.
            // Corpus: `arr2-003`.
            "array_compact" => match first_arg_type {
                Some(DataType::Array(elem, _)) => Array(elem.clone(), false),
                _ => Unresolved,
            },
            // `array_remove` preserves the input's containsNull flag.
            // Corpus: `arr2-005`.
            "array_remove" => match first_arg_type {
                Some(DataType::Array(elem, contains_null)) => Array(elem.clone(), *contains_null),
                _ => Unresolved,
            },
            // `array_insert(arr, pos, val)` — Spark stamps `containsNull=true`
            // (out-of-range positive `pos` pads the gap with NULLs). Return
            // the same element type as the input array. Corpus: `arr2-002`.
            "array_insert" => match first_arg_type {
                Some(DataType::Array(elem, _)) => Array(elem.clone(), true),
                _ => Unresolved,
            },

            // ── Map accessors ───────────────────────────────────────────
            // `element_at(coll, k)` on Array → element type; on Map →
            // value type (always nullable — missing key returns NULL).
            // Corpus: `map-004`, `type-018`.
            "element_at" => match first_arg_type {
                Some(DataType::Array(elem, _)) => (**elem).clone(),
                Some(DataType::Map { value, .. }) => (**value).clone(),
                _ => Unresolved,
            },
            // `map_keys(Map<K, V>) → Array<K>`. Spark stamps
            // containsNull=true on the returned array (matches the
            // reference `ArrayType(StringType(), True)` — the map-keys
            // ArrayType is defensively nullable in Spark's schema even
            // though map keys are non-null in the data model). Corpus:
            // `map-002`.
            "map_keys" => match first_arg_type {
                Some(DataType::Map { key, .. }) => Array(key.clone(), true),
                _ => Unresolved,
            },
            // `map_values(Map<K, V>) → Array<V>` — inherits map's
            // value_nullable. Corpus: `map-002`.
            "map_values" => match first_arg_type {
                Some(DataType::Map {
                    value,
                    value_nullable,
                    ..
                }) => Array(value.clone(), *value_nullable),
                _ => Unresolved,
            },
            // `map_entries(Map<K, V>) → Array<Struct{key: K NOT NULL,
            // value: V nullable}>`, containsNull=false. Corpus: `map-003`.
            "map_entries" => match first_arg_type {
                Some(DataType::Map {
                    key,
                    value,
                    value_nullable,
                }) => {
                    let entry_struct = DataType::Struct(StructType::new(vec![
                        StructField::not_null("key", (**key).clone()),
                        StructField::new("value", (**value).clone(), *value_nullable),
                    ]));
                    Array(Box::new(entry_struct), false)
                }
                _ => Unresolved,
            },

            // ── JSON ────────────────────────────────────────────────────
            // `get_json_object(json_str, path)` returns String (nullable
            // when the path doesn't match). Corpus: `json-001`.
            "get_json_object" | "json_extract_scalar" | "json_extract_string" => String,
            // `to_json(struct)` — Spark returns String; nullability follows
            // the argument (a NULL struct produces NULL, a non-null struct
            // produces a non-null JSON string). The default
            // `function_call_nullable` fallback (`any(arg.nullable)`) is
            // correct here — no override needed. Corpus: `json-005`.
            "to_json" => String,
            // `schema_of_json(json_str)` — Spark returns a DDL schema String.
            // Requires the `thdck_spark_funcs` extension (`spark_schema_of_json`);
            // remapped at emission time. Corpus: `json-006`.
            "schema_of_json" => String,
            // `to_csv(struct)` — Spark returns String. DuckDB has no native
            // `to_csv`; τ emits `concat_ws(',', CAST(f1 AS VARCHAR), ...)`
            // when the argument is a `struct(...)` literal. Nullability
            // follows argument nullability. Corpus: `json-008`.
            "to_csv" => String,

            // ── Aggregate-shaped functions in scalar dispatch ───────────
            // `array_agg` is routed through the aggregate delegation list
            // above (unified with `collect_list`/`collect_set`), so it never
            // falls through here. Removed the scalar arm to eliminate the
            // divergent behavior where `array_agg(Array<T>)` incorrectly
            // returned `Array<T>` instead of `Array<Array<T>>` (Spark: an
            // aggregate over `Array<T>` yields `Array<Array<T>>`).
            // Corpus: `agg2-002`.
            // `histogram_numeric(col, nb) → Array<Struct{x: Double
            // (nullable), y: Double (nullable)}>` (containsNull=true) per
            // Spark 4's HistogramNumeric schema. The inner struct fields
            // and the outer array are all reported nullable=true via the
            // agent-observed reference schema. Corpus: `agg2-005`.
            "histogram_numeric" => {
                let bin_struct = DataType::Struct(StructType::new(vec![
                    StructField::nullable("x", Double),
                    StructField::nullable("y", Double),
                ]));
                Array(Box::new(bin_struct), true)
            }

            // `F.window(ts, duration)` — tumbling time-window. Spark's
            // `TimeWindow.dataType` is a fixed
            // `Struct{start: TimestampType, end: TimestampType}` with both
            // fields nullable (Spark's `StructField` default). The struct
            // itself is nullable iff `ts` is nullable — that's handled by the
            // default `any(arg.nullable)` fallback in
            // `Expression::function_call_nullable`. Corpus: `win2-002`.
            "window" => DataType::Struct(StructType::new(vec![
                StructField::nullable("start", Timestamp),
                StructField::nullable("end", Timestamp),
            ])),

            // ── Metadata / environment ──────────────────────────────────
            // `input_file_name()` returns String (empty for in-memory).
            // Corpus: `meta-004`.
            "input_file_name" | "input_file_block_start" | "input_file_block_length" => String,

            // ── Type predicates / control ───────────────────────────────
            "typeof" => String,
            "spark_partition_id" => Integer,
            "monotonically_increasing_id" => Long,

            // τ seed: everything else is unresolved.
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
/// Promoted from `#[cfg(test)]`.2's fix pass (review M3): the
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
    // Pass 71: Spark's `any` / `some` / `all` are canonical aliases of
    // `bool_or` / `bool_or` / `bool_and`. Registering them here lets the
    // SparkSQL parser classify bare-aggregate fragments (`F.expr("any(x)")`,
    // `F.expr("all(x)")`) as aggregates so they route to
    // `lower_aggregate_select`, and lets emission's `is_aggregate_name` route
    // them to `render_aggregate` where the name-remap already lives.
    "any",
    "some",
    "all",
    // Pass 73: Spark's `mode` and `median` are aggregate builtins. Register
    // here so the SparkSQL parser classifies bare `mode(col)` fragments as
    // aggregates and emission's `is_aggregate_name` routes them to
    // `render_aggregate` where the name-remap already lives.
    "mode",
    "median",
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

    // ── τ's analyzer checklist §1.4 — try_sum / try_avg (aggregate) ──────────────

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

    // ── Pass 58 — scalar function-return-type coverage ──────────────────────
    //
    // Every arm added under Pass 58 gets a witnessing test — the corpus case
    // that motivates the arm is called out in the doc-comment on the arm and
    // pinned mechanically by the assertion below. Regression contract: if any
    // of these return `Unresolved` again, the analyzer's schema stamp will
    // leak `unparsed{"unresolved"}` to the wire and PySpark refuses to parse
    // it (see `.agent-output/diagnostic-unresolved-schema.md`).

    #[test]
    fn nanvl_returns_first_arg_type() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("nanvl", Some(&DataType::Double)),
            DataType::Double
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("nanvl", Some(&DataType::Float)),
            DataType::Float
        );
    }

    #[test]
    fn try_divide_returns_double_for_integers() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("try_divide", Some(&DataType::Integer)),
            DataType::Double
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("try_divide", Some(&DataType::Long)),
            DataType::Double
        );
    }

    #[test]
    fn try_divide_returns_unresolved_for_decimal_until_multi_arg_dispatch() {
        // `try_divide(Decimal, Decimal)` widens per `decimal_div_type`, but
        // this resolver only sees the first arg's type. Until multi-arg
        // dispatch lands, the Decimal branch must return `Unresolved` so the
        // ADR-022 boundary guard trips instead of silently mis-typing.
        let dec = DataType::Decimal {
            precision: 10,
            scale: 2,
        };
        assert_eq!(
            TypeInferenceEngine::function_return_type("try_divide", Some(&dec)),
            DataType::Unresolved
        );
    }

    #[test]
    fn size_scalar_returns_integer() {
        assert_eq!(
            TypeInferenceEngine::function_return_type(
                "size",
                Some(&DataType::Array(Box::new(DataType::String), true))
            ),
            DataType::Integer
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type(
                "cardinality",
                Some(&DataType::Array(Box::new(DataType::Integer), true))
            ),
            DataType::Integer
        );
    }

    /// Pass 68 — `explode(Array<T>)` / `explode_outer(Array<T>)` /
    /// `posexplode_val(Array<T>)` all return the element type T. Corpus:
    /// arr-015, arr-016, arr-017.
    #[test]
    fn explode_returns_array_element_type() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("explode", Some(&arr)),
            DataType::String
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("explode_outer", Some(&arr)),
            DataType::String
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("posexplode_val", Some(&arr)),
            DataType::String
        );
    }

    /// Pass 68 — `posexplode_pos(arr)` always returns Integer (the
    /// 0-indexed synthetic position column). Corpus: arr-017.
    #[test]
    fn posexplode_pos_returns_integer() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("posexplode_pos", Some(&arr)),
            DataType::Integer
        );
    }

    #[test]
    fn sequence_returns_array_of_first_arg() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("sequence", Some(&DataType::Integer)),
            DataType::Array(Box::new(DataType::Integer), false)
        );
        // No arg → default element type is Long (start/stop unknown).
        assert_eq!(
            TypeInferenceEngine::function_return_type("sequence", None),
            DataType::Array(Box::new(DataType::Long), false)
        );
    }

    #[test]
    fn get_json_object_returns_string() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("get_json_object", Some(&DataType::String)),
            DataType::String
        );
    }

    /// Pass 62 — `to_json(struct)` returns String. Corpus: `json-005`.
    #[test]
    fn to_json_returns_string() {
        // Argument here is a StructType (from `struct(...)`) — the resolver
        // only takes the first-arg type; the return is always String.
        let s = DataType::Struct(StructType::new(vec![]));
        assert_eq!(
            TypeInferenceEngine::function_return_type("to_json", Some(&s)),
            DataType::String
        );
    }

    /// Pass 62 — `schema_of_json(json_str)` returns String. Corpus: `json-006`.
    #[test]
    fn schema_of_json_returns_string() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("schema_of_json", Some(&DataType::String)),
            DataType::String
        );
    }

    /// Pass 62 — `to_csv(struct)` returns String. Corpus: `json-008`.
    #[test]
    fn to_csv_returns_string() {
        let s = DataType::Struct(StructType::new(vec![]));
        assert_eq!(
            TypeInferenceEngine::function_return_type("to_csv", Some(&s)),
            DataType::String
        );
    }

    #[test]
    fn element_at_on_map_returns_value_type() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(
            TypeInferenceEngine::function_return_type("element_at", Some(&m)),
            DataType::Long
        );
    }

    #[test]
    fn element_at_on_array_returns_element_type() {
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("element_at", Some(&a)),
            DataType::String
        );
    }

    #[test]
    fn map_keys_returns_array_of_key_type_with_contains_null_true() {
        // Spark stamps `ArrayType(K, containsNull=True)` on map_keys.
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(
            TypeInferenceEngine::function_return_type("map_keys", Some(&m)),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn map_values_returns_array_of_value_type() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(
            TypeInferenceEngine::function_return_type("map_values", Some(&m)),
            DataType::Array(Box::new(DataType::Long), true)
        );
    }

    #[test]
    fn map_entries_returns_array_of_struct() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        let expected = DataType::Array(
            Box::new(DataType::Struct(StructType::new(vec![
                StructField::not_null("key", DataType::String),
                StructField::new("value", DataType::Long, true),
            ]))),
            false,
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("map_entries", Some(&m)),
            expected
        );
    }

    #[test]
    fn map_contains_key_returns_boolean() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(
            TypeInferenceEngine::function_return_type("map_contains_key", Some(&m)),
            DataType::Boolean
        );
    }

    #[test]
    fn array_append_preserves_element_but_forces_containsnull_true() {
        let a = DataType::Array(Box::new(DataType::String), false);
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_append", Some(&a)),
            DataType::Array(Box::new(DataType::String), true)
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_prepend", Some(&a)),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn array_compact_forces_containsnull_false() {
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_compact", Some(&a)),
            DataType::Array(Box::new(DataType::String), false)
        );
    }

    #[test]
    fn array_remove_returns_first_arg_type() {
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_remove", Some(&a)),
            a
        );
    }

    #[test]
    fn array_intersect_stamps_contains_null_false() {
        // Pass 72: Spark stamps `containsNull = leftContainsNull AND
        // rightContainsNull` on `array_intersect`. τ conservatively
        // stamps `false` — matches corpus arr2-005 (the second arg is a
        // non-nullable array literal, so the AND collapses to false).
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_intersect", Some(&a)),
            DataType::Array(Box::new(DataType::String), false)
        );
    }

    #[test]
    fn array_insert_stamps_contains_null_true() {
        // Pass 72: `array_insert(arr, pos, val)` — Spark stamps
        // `containsNull=true` (out-of-range positive pos pads with NULLs).
        let a = DataType::Array(Box::new(DataType::String), false);
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_insert", Some(&a)),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn str_to_map_returns_map_of_string_to_string() {
        // Pass 72: `str_to_map` returns Map<VARCHAR, VARCHAR> with
        // nullable values (missing tokens produce NULL values).
        assert_eq!(
            TypeInferenceEngine::function_return_type("str_to_map", Some(&DataType::String)),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }
        );
    }

    #[test]
    fn map_concat_returns_first_arg_type() {
        // Pass 72: `map_concat` preserves the first argument's map type.
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::String),
            value_nullable: true,
        };
        assert_eq!(
            TypeInferenceEngine::function_return_type("map_concat", Some(&m)),
            m
        );
    }

    #[test]
    fn histogram_numeric_returns_array_of_bin_struct() {
        // Spark 4 reports the histogram bin struct fields and the outer
        // array as nullable=true (containsNull=true). Corpus: `agg2-005`.
        let expected = DataType::Array(
            Box::new(DataType::Struct(StructType::new(vec![
                StructField::nullable("x", DataType::Double),
                StructField::nullable("y", DataType::Double),
            ]))),
            true,
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("histogram_numeric", Some(&DataType::Double)),
            expected
        );
    }

    #[test]
    fn input_file_name_returns_string() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("input_file_name", None),
            DataType::String
        );
    }

    #[test]
    fn split_part_returns_string() {
        assert_eq!(
            TypeInferenceEngine::function_return_type("split_part", Some(&DataType::String)),
            DataType::String
        );
    }

    #[test]
    fn regexp_extract_all_returns_array_of_string() {
        assert_eq!(
            TypeInferenceEngine::function_return_type(
                "regexp_extract_all",
                Some(&DataType::String)
            ),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn array_agg_returns_array_of_elem_via_aggregate_delegation() {
        // `array_agg` is unified with `collect_list`/`collect_set` in the
        // aggregate delegation list — the scalar arm was removed to avoid
        // the divergent case where `array_agg(Array<T>)` incorrectly
        // stayed at `Array<T>` instead of widening to `Array<Array<T>>`.
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_agg", Some(&DataType::String)),
            DataType::Array(Box::new(DataType::String), false)
        );
    }

    #[test]
    fn array_agg_of_array_returns_array_of_array() {
        // Spark semantics: aggregating `Array<T>` yields `Array<Array<T>>`.
        // Regression lock for the OPT-2 unification (pass 58 fix iter 1).
        let inner = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_agg", Some(&inner)),
            DataType::Array(Box::new(inner), false)
        );
    }

    /// `array_position(arr, item)` returns Long, not the array's element
    /// type. Spark's return is BIGINT (Long) — 1-based index or 0 if not
    /// found. Corpus: `arr-007`.
    #[test]
    fn array_position_returns_long() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("array_position", Some(&arr)),
            DataType::Long
        );
        assert_eq!(
            TypeInferenceEngine::function_return_type("list_position", Some(&arr)),
            DataType::Long
        );
    }

    /// `arrays_overlap(a, b)` returns Boolean regardless of the array
    /// element type. Corpus: `arr-011`.
    #[test]
    fn arrays_overlap_returns_boolean() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            TypeInferenceEngine::function_return_type("arrays_overlap", Some(&arr)),
            DataType::Boolean
        );
    }

    /// `flatten(Array<Array<T>>)` unwraps one level of nesting → `Array<T>`,
    /// preserving the inner containsNull flag. Corpus: `arr-013`.
    #[test]
    fn flatten_reduces_one_array_level() {
        let inner = DataType::Array(Box::new(DataType::String), true);
        let outer = DataType::Array(Box::new(inner.clone()), false);
        assert_eq!(
            TypeInferenceEngine::function_return_type("flatten", Some(&outer)),
            inner
        );
    }

    /// `F.window(ts, duration)` → `Struct{start: Timestamp, end: Timestamp}`
    /// with both fields nullable (Spark's `TimeWindow.dataType` uses
    /// `StructField` default nullable). Return type is arg-independent so
    /// `first_arg_type = None` returns the same fixed struct. Corpus:
    /// `win2-002`.
    #[test]
    fn window_returns_struct_of_two_nullable_timestamps() {
        let expected = DataType::Struct(StructType::new(vec![
            StructField::nullable("start", DataType::Timestamp),
            StructField::nullable("end", DataType::Timestamp),
        ]));
        assert_eq!(
            TypeInferenceEngine::function_return_type("window", Some(&DataType::Timestamp)),
            expected,
        );
        // Arg-independent — no arg-type context should give the same schema.
        assert_eq!(
            TypeInferenceEngine::function_return_type("window", None),
            expected,
        );
        // Case-insensitive dispatch.
        assert_eq!(
            TypeInferenceEngine::function_return_type("Window", Some(&DataType::Timestamp)),
            expected,
        );
    }
}
