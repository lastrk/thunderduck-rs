#pragma once

// ============================================================================
// CONTRACT (mirrored in README.md "Spark hash functions" section). Both
// functions registered below MUST keep these semantics; an integration agent
// wiring this into Thunderduck / Thunderduck-RS should NOT add coalesce
// wrappers, CAST wrappers, or NULL filters. See README for the full rationale
// and the failure modes that motivated this design.
//
//   spark_xxhash64(VARIADIC ANY) -> BIGINT   (signed two's complement)
//   spark_hash    (VARIADIC ANY) -> INTEGER  (signed two's complement)
//
// 1. INITIAL SEED 42 (for BOTH functions). `spark_xxhash64()` returns 42L,
//    `spark_hash()` returns 42 — matches Spark's HashExpression default seed.
//
// 2. NULL SKIP: a NULL value at a given column/row leaves the running hash
//    seed UNCHANGED for that row. The column is skipped, not propagated.
//    Matches Spark's `HashExpression.eval`: `if (value == null) continue`.
//
//      spark_xxhash64(1::INT, NULL::INT, 2::INT)  ==  spark_xxhash64(1, 2)
//
//    This is enforced by `FunctionNullHandling::SPECIAL_HANDLING` on
//    registration. Without that flag, DuckDB would short-circuit any row
//    containing a NULL argument and return NULL — the OPPOSITE of Spark.
//    DO NOT WRAP CALLERS IN coalesce/IFNULL; the resulting hash diverges.
//
//    Same rule applies to nested types:
//      - LIST<T>:     null elements skipped
//      - ARRAY<T,n>:  null elements skipped
//      - STRUCT:      null field values skipped (field names not hashed)
//      - MAP:         null values skipped; null keys are a Spark error
//
// 3. SIGNED RETURN TYPE is the direct bit-reinterpret of the unsigned
//    XXH64/Murmur3 output (uint64 -> int64, uint32 -> int32). DO NOT
//    wrap the result in `CAST(... AS BIGINT)` — DuckDB raises
//    "Type UINT64 ... can't be cast ... to INT64" for any hash whose high
//    bit is set (i.e. roughly half of all inputs), so a cast-based bridge
//    from the hashfuncs community extension's UBIGINT/UINTEGER outputs is
//    not viable. This native path sidesteps that entirely.
//
// 4. UNSUPPORTED DUCKDB TYPES throw at bind time with a clear message:
//    UTINYINT, USMALLINT, UINTEGER, UBIGINT, HUGEINT (no Spark equivalent);
//    TIME, TIME_TZ, TIMESTAMP_SEC, TIMESTAMP_MS, TIMESTAMP_NS, UUID, BIT,
//    ENUM, UNION, VARINT (no exact Spark equivalent). The check is
//    recursive: `LIST<UTINYINT>`, `STRUCT(x UTINYINT)`, `MAP<INT,HUGEINT>`
//    all fail.
//
// ============================================================================

#include "duckdb.hpp"
#include "duckdb/common/exception.hpp"
#include "duckdb/common/types/decimal.hpp"
#include "duckdb/common/types/vector.hpp"
#include "duckdb/function/scalar_function.hpp"

#include "spark_murmur3.hpp"
#include "spark_xxh64.hpp"
#include "wide_integer.hpp"

#include <cmath>
#include <cstdint>
#include <cstring>

namespace duckdb {

// ----------------------------------------------------------------------------
// Tag types: parameterize the templated execution over the two algorithms.
// ----------------------------------------------------------------------------
struct SparkXxh64Tag {
	using Seed = uint64_t;
	using Out = int64_t;
	static constexpr Seed INIT_SEED = 42;
	static constexpr const char *FUNCTION_NAME = "spark_xxhash64";
	static inline Seed HashInt(int32_t v, Seed s) {
		return spark_hash::XXH64HashInt(v, s);
	}
	static inline Seed HashLong(int64_t v, Seed s) {
		return spark_hash::XXH64HashLong(v, s);
	}
	static inline Seed HashBytes(const uint8_t *p, size_t n, Seed s) {
		return spark_hash::XXH64HashBytes(p, n, s);
	}
};

struct SparkMurmur3Tag {
	using Seed = uint32_t;
	using Out = int32_t;
	static constexpr Seed INIT_SEED = 42;
	static constexpr const char *FUNCTION_NAME = "spark_hash";
	static inline Seed HashInt(int32_t v, Seed s) {
		return spark_hash::Murmur3HashInt(v, s);
	}
	static inline Seed HashLong(int64_t v, Seed s) {
		return spark_hash::Murmur3HashLong(v, s);
	}
	static inline Seed HashBytes(const uint8_t *p, size_t n, Seed s) {
		return spark_hash::Murmur3HashBytes(p, n, s);
	}
};

// ----------------------------------------------------------------------------
// Bind-time type validation.
// ----------------------------------------------------------------------------
static inline void ValidateSparkSupportedType(const LogicalType &type, const char *fn_name) {
	switch (type.id()) {
	case LogicalTypeId::BOOLEAN:
	case LogicalTypeId::TINYINT:
	case LogicalTypeId::SMALLINT:
	case LogicalTypeId::INTEGER:
	case LogicalTypeId::BIGINT:
	case LogicalTypeId::FLOAT:
	case LogicalTypeId::DOUBLE:
	case LogicalTypeId::DATE:
	case LogicalTypeId::TIMESTAMP:
	case LogicalTypeId::TIMESTAMP_TZ:
	case LogicalTypeId::INTERVAL:
	case LogicalTypeId::VARCHAR:
	case LogicalTypeId::BLOB:
	case LogicalTypeId::DECIMAL:
	case LogicalTypeId::SQLNULL:
		return;
	case LogicalTypeId::LIST:
		ValidateSparkSupportedType(ListType::GetChildType(type), fn_name);
		return;
	case LogicalTypeId::ARRAY:
		ValidateSparkSupportedType(ArrayType::GetChildType(type), fn_name);
		return;
	case LogicalTypeId::STRUCT: {
		auto &fields = StructType::GetChildTypes(type);
		for (auto &f : fields) {
			ValidateSparkSupportedType(f.second, fn_name);
		}
		return;
	}
	case LogicalTypeId::MAP:
		ValidateSparkSupportedType(MapType::KeyType(type), fn_name);
		ValidateSparkSupportedType(MapType::ValueType(type), fn_name);
		return;
	default:
		throw InvalidInputException("%s: type %s has no Spark equivalent; cast to a Spark-supported type explicitly",
		                            fn_name, type.ToString());
	}
}

// ----------------------------------------------------------------------------
// BigInteger.toByteArray()-equivalent for __int128.
// Java semantics: minimum-length big-endian two's-complement byte array,
// always >= 1 byte. Validated against `BigInteger.valueOf(v).toByteArray()`
// for: 0 -> [00], 1 -> [01], 127 -> [7F], 128 -> [00 80], -1 -> [FF],
// -128 -> [80], -129 -> [FF 7F].
// ----------------------------------------------------------------------------
static inline size_t Int128ToBigIntegerBytes(__int128 v, uint8_t out[16]) {
	unsigned __int128 uv = static_cast<unsigned __int128>(v);
	uint8_t buf[16];
	for (int i = 15; i >= 0; --i) {
		buf[i] = static_cast<uint8_t>(uv & 0xFF);
		uv >>= 8;
	}
	bool negative = (buf[0] & 0x80) != 0;
	int start = 0;
	if (negative) {
		while (start < 15 && buf[start] == 0xFF && (buf[start + 1] & 0x80) != 0) {
			start++;
		}
	} else {
		while (start < 15 && buf[start] == 0x00 && (buf[start + 1] & 0x80) == 0) {
			start++;
		}
	}
	size_t len = static_cast<size_t>(16 - start);
	std::memcpy(out, buf + start, len);
	return len;
}

// ----------------------------------------------------------------------------
// Float/Double NaN canonicalization (matches Java's Float.NaN /
// Double.NaN bit patterns: 0x7FC00000 and 0x7FF8000000000000).
// ----------------------------------------------------------------------------
static inline uint32_t SparkFloatBits(float v) {
	if (std::isnan(v)) {
		return 0x7FC00000U;
	}
	uint32_t bits;
	std::memcpy(&bits, &v, 4);
	return bits;
}

static inline uint64_t SparkDoubleBits(double v) {
	if (std::isnan(v)) {
		return 0x7FF8000000000000ULL;
	}
	uint64_t bits;
	std::memcpy(&bits, &v, 8);
	return bits;
}

// ----------------------------------------------------------------------------
// Recursive fold: hash a value at physical index `idx` of a flat-layout
// Vector. Caller MUST ensure validity[idx] is true; this function does not
// re-check. For nested types, the entire descendant subtree must have been
// flattened by PrepareNestedForHashing before the first call.
// ----------------------------------------------------------------------------
template <class Tag>
static typename Tag::Seed FoldFlatValueAt(Vector &flat_vec, idx_t idx, typename Tag::Seed seed) {
	auto &type = flat_vec.GetType();
	switch (type.id()) {
	case LogicalTypeId::BOOLEAN: {
		auto data = FlatVector::GetData<bool>(flat_vec);
		return Tag::HashInt(data[idx] ? 1 : 0, seed);
	}
	case LogicalTypeId::TINYINT: {
		auto data = FlatVector::GetData<int8_t>(flat_vec);
		return Tag::HashInt(static_cast<int32_t>(data[idx]), seed);
	}
	case LogicalTypeId::SMALLINT: {
		auto data = FlatVector::GetData<int16_t>(flat_vec);
		return Tag::HashInt(static_cast<int32_t>(data[idx]), seed);
	}
	case LogicalTypeId::INTEGER: {
		auto data = FlatVector::GetData<int32_t>(flat_vec);
		return Tag::HashInt(data[idx], seed);
	}
	case LogicalTypeId::BIGINT: {
		auto data = FlatVector::GetData<int64_t>(flat_vec);
		return Tag::HashLong(data[idx], seed);
	}
	case LogicalTypeId::DATE: {
		auto data = FlatVector::GetData<date_t>(flat_vec);
		return Tag::HashInt(data[idx].days, seed);
	}
	case LogicalTypeId::TIMESTAMP:
	case LogicalTypeId::TIMESTAMP_TZ: {
		auto data = FlatVector::GetData<timestamp_t>(flat_vec);
		return Tag::HashLong(data[idx].value, seed);
	}
	case LogicalTypeId::FLOAT: {
		auto data = FlatVector::GetData<float>(flat_vec);
		return Tag::HashInt(static_cast<int32_t>(SparkFloatBits(data[idx])), seed);
	}
	case LogicalTypeId::DOUBLE: {
		auto data = FlatVector::GetData<double>(flat_vec);
		return Tag::HashLong(static_cast<int64_t>(SparkDoubleBits(data[idx])), seed);
	}
	case LogicalTypeId::INTERVAL: {
		auto data = FlatVector::GetData<interval_t>(flat_vec);
		interval_t iv = data[idx];
		typename Tag::Seed s = Tag::HashInt(iv.months, seed);
		s = Tag::HashInt(iv.days, s);
		return Tag::HashLong(iv.micros, s);
	}
	case LogicalTypeId::VARCHAR:
	case LogicalTypeId::BLOB: {
		auto data = FlatVector::GetData<string_t>(flat_vec);
		const string_t &s = data[idx];
		return Tag::HashBytes(reinterpret_cast<const uint8_t *>(s.GetData()), s.GetSize(), seed);
	}
	case LogicalTypeId::DECIMAL: {
		uint8_t precision = DecimalType::GetWidth(type);
		if (precision <= 18) {
			int64_t unscaled;
			switch (type.InternalType()) {
			case PhysicalType::INT16:
				unscaled = static_cast<int64_t>(FlatVector::GetData<int16_t>(flat_vec)[idx]);
				break;
			case PhysicalType::INT32:
				unscaled = static_cast<int64_t>(FlatVector::GetData<int32_t>(flat_vec)[idx]);
				break;
			case PhysicalType::INT64:
				unscaled = FlatVector::GetData<int64_t>(flat_vec)[idx];
				break;
			default:
				throw InternalException("Unexpected DECIMAL physical type for precision <= 18");
			}
			return Tag::HashLong(unscaled, seed);
		}
		hugeint_t h = FlatVector::GetData<hugeint_t>(flat_vec)[idx];
		__int128 unscaled = HugeintToInt128(h);
		uint8_t bytes[16];
		size_t len = Int128ToBigIntegerBytes(unscaled, bytes);
		return Tag::HashBytes(bytes, len, seed);
	}
	case LogicalTypeId::LIST: {
		auto list_data = FlatVector::GetData<list_entry_t>(flat_vec);
		const auto &entry = list_data[idx];
		auto &child = ListVector::GetEntry(flat_vec);
		auto &child_validity = FlatVector::Validity(child);
		typename Tag::Seed s = seed;
		for (idx_t i = 0; i < entry.length; i++) {
			idx_t cidx = entry.offset + i;
			if (child_validity.RowIsValid(cidx)) {
				s = FoldFlatValueAt<Tag>(child, cidx, s);
			}
		}
		return s;
	}
	case LogicalTypeId::ARRAY: {
		idx_t array_size = ArrayType::GetSize(type);
		auto &child = ArrayVector::GetEntry(flat_vec);
		auto &child_validity = FlatVector::Validity(child);
		typename Tag::Seed s = seed;
		idx_t base = idx * array_size;
		for (idx_t i = 0; i < array_size; i++) {
			idx_t cidx = base + i;
			if (child_validity.RowIsValid(cidx)) {
				s = FoldFlatValueAt<Tag>(child, cidx, s);
			}
		}
		return s;
	}
	case LogicalTypeId::STRUCT: {
		auto &children = StructVector::GetEntries(flat_vec);
		typename Tag::Seed s = seed;
		for (auto &child_ptr : children) {
			if (FlatVector::Validity(*child_ptr).RowIsValid(idx)) {
				s = FoldFlatValueAt<Tag>(*child_ptr, idx, s);
			}
		}
		return s;
	}
	case LogicalTypeId::MAP: {
		// MAP is physically LIST<STRUCT<key, value>> in DuckDB; iterate the
		// LIST extents and pull keys/values from the parallel child vectors.
		auto list_data = FlatVector::GetData<list_entry_t>(flat_vec);
		const auto &entry = list_data[idx];
		auto &keys = MapVector::GetKeys(flat_vec);
		auto &values = MapVector::GetValues(flat_vec);
		auto &val_validity = FlatVector::Validity(values);
		typename Tag::Seed s = seed;
		for (idx_t i = 0; i < entry.length; i++) {
			idx_t cidx = entry.offset + i;
			// Spark requires non-null map keys, so we don't validity-check.
			s = FoldFlatValueAt<Tag>(keys, cidx, s);
			if (val_validity.RowIsValid(cidx)) {
				s = FoldFlatValueAt<Tag>(values, cidx, s);
			}
		}
		return s;
	}
	case LogicalTypeId::SQLNULL:
		// Caller validity check should have skipped this, but defensively
		// preserve the seed.
		return seed;
	default:
		throw InternalException("spark_xxhash64 / spark_hash: unexpected LogicalTypeId %d at exec time",
		                        static_cast<int>(type.id()));
	}
}

// ----------------------------------------------------------------------------
// Recursively flatten every descendant child vector so FoldFlatValueAt can
// use direct FlatVector::GetData / Validity access in the inner loops.
// ----------------------------------------------------------------------------
static inline void PrepareNestedForHashing(Vector &vec, idx_t count) {
	switch (vec.GetType().id()) {
	case LogicalTypeId::LIST: {
		auto &child = ListVector::GetEntry(vec);
		idx_t child_count = ListVector::GetListSize(vec);
		child.Flatten(child_count);
		PrepareNestedForHashing(child, child_count);
		break;
	}
	case LogicalTypeId::ARRAY: {
		auto &child = ArrayVector::GetEntry(vec);
		idx_t child_count = ArrayVector::GetTotalSize(vec);
		child.Flatten(child_count);
		PrepareNestedForHashing(child, child_count);
		break;
	}
	case LogicalTypeId::STRUCT: {
		auto &children = StructVector::GetEntries(vec);
		for (auto &child : children) {
			child->Flatten(count);
			PrepareNestedForHashing(*child, count);
		}
		break;
	}
	case LogicalTypeId::MAP: {
		auto &keys = MapVector::GetKeys(vec);
		auto &values = MapVector::GetValues(vec);
		idx_t child_count = ListVector::GetListSize(vec);
		keys.Flatten(child_count);
		values.Flatten(child_count);
		PrepareNestedForHashing(keys, child_count);
		PrepareNestedForHashing(values, child_count);
		break;
	}
	default:
		break;
	}
}

// ----------------------------------------------------------------------------
// Top-level column fold. Scalar types use UnifiedVectorFormat (so
// CONSTANT/DICTIONARY vectors keep their fast paths). Nested types are
// flattened in-place and processed row-major by FoldFlatValueAt.
// ----------------------------------------------------------------------------
template <class Tag>
static void FoldColumn(Vector &vec, idx_t count, typename Tag::Seed *seeds) {
	auto type_id = vec.GetType().id();

	if (type_id == LogicalTypeId::SQLNULL) {
		return; // every row is NULL — seeds unchanged
	}

	switch (type_id) {
	case LogicalTypeId::BOOLEAN: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<bool>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashInt(data[idx] ? 1 : 0, seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::TINYINT: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<int8_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashInt(static_cast<int32_t>(data[idx]), seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::SMALLINT: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<int16_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashInt(static_cast<int32_t>(data[idx]), seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::INTEGER: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<int32_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashInt(data[idx], seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::BIGINT: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<int64_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashLong(data[idx], seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::DATE: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<date_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashInt(data[idx].days, seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::TIMESTAMP:
	case LogicalTypeId::TIMESTAMP_TZ: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<timestamp_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashLong(data[idx].value, seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::FLOAT: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<float>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashInt(static_cast<int32_t>(SparkFloatBits(data[idx])), seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::DOUBLE: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<double>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				seeds[r] = Tag::HashLong(static_cast<int64_t>(SparkDoubleBits(data[idx])), seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::INTERVAL: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<interval_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				interval_t iv = data[idx];
				typename Tag::Seed s = Tag::HashInt(iv.months, seeds[r]);
				s = Tag::HashInt(iv.days, s);
				seeds[r] = Tag::HashLong(iv.micros, s);
			}
		}
		return;
	}
	case LogicalTypeId::VARCHAR:
	case LogicalTypeId::BLOB: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		auto data = UnifiedVectorFormat::GetData<string_t>(fmt);
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				const string_t &s = data[idx];
				seeds[r] = Tag::HashBytes(reinterpret_cast<const uint8_t *>(s.GetData()), s.GetSize(), seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::DECIMAL: {
		UnifiedVectorFormat fmt;
		vec.ToUnifiedFormat(count, fmt);
		uint8_t precision = DecimalType::GetWidth(vec.GetType());
		if (precision <= 18) {
			switch (vec.GetType().InternalType()) {
			case PhysicalType::INT16: {
				auto data = UnifiedVectorFormat::GetData<int16_t>(fmt);
				for (idx_t r = 0; r < count; r++) {
					idx_t idx = fmt.sel->get_index(r);
					if (fmt.validity.RowIsValid(idx)) {
						seeds[r] = Tag::HashLong(static_cast<int64_t>(data[idx]), seeds[r]);
					}
				}
				return;
			}
			case PhysicalType::INT32: {
				auto data = UnifiedVectorFormat::GetData<int32_t>(fmt);
				for (idx_t r = 0; r < count; r++) {
					idx_t idx = fmt.sel->get_index(r);
					if (fmt.validity.RowIsValid(idx)) {
						seeds[r] = Tag::HashLong(static_cast<int64_t>(data[idx]), seeds[r]);
					}
				}
				return;
			}
			case PhysicalType::INT64: {
				auto data = UnifiedVectorFormat::GetData<int64_t>(fmt);
				for (idx_t r = 0; r < count; r++) {
					idx_t idx = fmt.sel->get_index(r);
					if (fmt.validity.RowIsValid(idx)) {
						seeds[r] = Tag::HashLong(data[idx], seeds[r]);
					}
				}
				return;
			}
			default:
				throw InternalException("Unexpected DECIMAL physical type for precision <= 18");
			}
		}
		// precision > 18: hugeint physical, BigInteger.toByteArray path
		auto data = UnifiedVectorFormat::GetData<hugeint_t>(fmt);
		uint8_t bytes[16];
		for (idx_t r = 0; r < count; r++) {
			idx_t idx = fmt.sel->get_index(r);
			if (fmt.validity.RowIsValid(idx)) {
				__int128 unscaled = HugeintToInt128(data[idx]);
				size_t len = Int128ToBigIntegerBytes(unscaled, bytes);
				seeds[r] = Tag::HashBytes(bytes, len, seeds[r]);
			}
		}
		return;
	}
	case LogicalTypeId::LIST:
	case LogicalTypeId::ARRAY:
	case LogicalTypeId::STRUCT:
	case LogicalTypeId::MAP: {
		// Nested: flatten the whole subtree once, then row-major fold.
		vec.Flatten(count);
		PrepareNestedForHashing(vec, count);
		auto &validity = FlatVector::Validity(vec);
		for (idx_t r = 0; r < count; r++) {
			if (validity.RowIsValid(r)) {
				seeds[r] = FoldFlatValueAt<Tag>(vec, r, seeds[r]);
			}
		}
		return;
	}
	default:
		throw InternalException("spark_xxhash64 / spark_hash: unhandled LogicalTypeId %d in FoldColumn",
		                        static_cast<int>(type_id));
	}
}

// ----------------------------------------------------------------------------
// Exec function template — one body, two instantiations.
// ----------------------------------------------------------------------------
template <class Tag>
static void SparkHashExec(DataChunk &args, ExpressionState &state, Vector &result) {
	using Seed = typename Tag::Seed;
	using Out = typename Tag::Out;
	static_assert(sizeof(Seed) == sizeof(Out), "Seed and Out must have the same width for in-place reinterpret");

	idx_t rows = args.size();
	result.SetVectorType(VectorType::FLAT_VECTOR);
	Out *out_data = FlatVector::GetData<Out>(result);

	// Initialize all rows to INIT_SEED. INIT_SEED is a small positive
	// integer (42), so the bit pattern is identical between Seed (unsigned)
	// and Out (signed) — no special-case needed.
	for (idx_t r = 0; r < rows; r++) {
		out_data[r] = static_cast<Out>(Tag::INIT_SEED);
	}

	Seed *seeds = reinterpret_cast<Seed *>(out_data);

	for (idx_t c = 0; c < args.ColumnCount(); c++) {
		FoldColumn<Tag>(args.data[c], rows, seeds);
	}
}

// ----------------------------------------------------------------------------
// Bind function: validates that every argument's type has a Spark equivalent.
// Recurses into LIST/ARRAY/STRUCT/MAP. Does not store bind data — exec
// dispatches dynamically on Vector::GetType().
// ----------------------------------------------------------------------------
template <class Tag>
static unique_ptr<FunctionData> BindSparkHash(ClientContext &context, ScalarFunction &bound_function,
                                              vector<unique_ptr<Expression>> &arguments) {
	for (auto &arg : arguments) {
		ValidateSparkSupportedType(arg->return_type, Tag::FUNCTION_NAME);
	}
	return nullptr;
}

// ----------------------------------------------------------------------------
// Function factories.
//
// NOTE TO DOWNSTREAM INTEGRATION AGENTS (Thunderduck / Thunderduck-RS):
//   - Translate Spark `xxhash64(c1, ..., cN)` -> `spark_xxhash64(c1, ..., cN)`
//     directly. NO coalesce wrapper. NO CAST wrapper. Result is already
//     signed BIGINT; NULL skip happens inside the function.
//   - Translate Spark `hash(c1, ..., cN)` -> `spark_hash(c1, ..., cN)`
//     directly with the same rules.
// See the CONTRACT block at the top of this file and README.md for the
// rationale (in particular, the failure modes of the hashfuncs-based
// approach that this replaces).
// ----------------------------------------------------------------------------
inline ScalarFunction CreateSparkXxhash64Function() {
	ScalarFunction f("spark_xxhash64", {}, LogicalType::BIGINT, SparkHashExec<SparkXxh64Tag>,
	                 BindSparkHash<SparkXxh64Tag>);
	f.varargs = LogicalType::ANY;
	// SPECIAL_HANDLING is REQUIRED: without it, DuckDB short-circuits any
	// row containing a NULL argument and returns NULL. Spark's semantic is
	// "skip that column, keep folding". Removing this flag silently breaks
	// bit-parity with Spark — see the CONTRACT block above.
	f.null_handling = FunctionNullHandling::SPECIAL_HANDLING;
	return f;
}

inline ScalarFunction CreateSparkHashFunction() {
	ScalarFunction f("spark_hash", {}, LogicalType::INTEGER, SparkHashExec<SparkMurmur3Tag>,
	                 BindSparkHash<SparkMurmur3Tag>);
	f.varargs = LogicalType::ANY;
	// SPECIAL_HANDLING is REQUIRED — see comment on CreateSparkXxhash64Function.
	f.null_handling = FunctionNullHandling::SPECIAL_HANDLING;
	return f;
}

} // namespace duckdb
