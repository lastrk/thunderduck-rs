#define DUCKDB_EXTENSION_MAIN

#include "thdck_spark_funcs_extension.hpp"
#include "spark_precision.hpp"
#include "decimal_division.hpp"
#include "spark_aggregates.hpp"
#include "spark_try_aggregates.hpp"
#include "spark_hash.hpp"
#include "spark_schema_of_json.hpp"

#include "duckdb/common/exception.hpp"
#include "duckdb/common/types/decimal.hpp"
#include "duckdb/function/scalar_function.hpp"
#include "duckdb/function/function_set.hpp"
#include "duckdb/planner/expression/bound_function_expression.hpp"

namespace duckdb {

template <typename T>
static inline void WriteResult(T *data, idx_t idx, __int128 val) {
	data[idx] = static_cast<T>(val);
}

template <>
inline void WriteResult<hugeint_t>(hugeint_t *data, idx_t idx, __int128 val) {
	data[idx] = Int128ToHugeint(val);
}

template <typename RESULT_TYPE>
static void SparkDivExec(DataChunk &args, ExpressionState &state, Vector &result) {
	auto &func_expr = state.expr.Cast<BoundFunctionExpression>();
	auto &bind_data = func_expr.bind_info->Cast<SparkDivBindData>();
	uint32_t scale_adj = bind_data.scale_adj;

	unsigned __int128 pow10_val = (scale_adj > 0) ? Pow10_128(scale_adj) : 0;

	idx_t count = args.size();
	result.SetVectorType(VectorType::FLAT_VECTOR);
	auto *__restrict result_data = FlatVector::GetData<RESULT_TYPE>(result);
	auto &result_validity = FlatVector::Validity(result);

	auto &a_vec = args.data[0];
	auto &b_vec = args.data[1];

	// Flat vectors use the fast path; retain the validity-aware branch for NULLs.
	if (a_vec.GetVectorType() == VectorType::FLAT_VECTOR && b_vec.GetVectorType() == VectorType::FLAT_VECTOR) {
		auto *__restrict a_data = FlatVector::GetData<hugeint_t>(a_vec);
		auto *__restrict b_data = FlatVector::GetData<hugeint_t>(b_vec);
		auto &a_validity = FlatVector::Validity(a_vec);
		auto &b_validity = FlatVector::Validity(b_vec);

		if (a_validity.AllValid() && b_validity.AllValid()) {
			for (idx_t i = 0; i < count; i++) {
				__int128 b_val = HugeintToInt128(b_data[i]);
				if (__builtin_expect(b_val == 0, 0)) {
					result_validity.SetInvalid(i);
					continue;
				}
				__int128 a_val = HugeintToInt128(a_data[i]);
				WriteResult(result_data, i, SparkDecimalDivide(a_val, b_val, pow10_val));
			}
		} else {
			for (idx_t i = 0; i < count; i++) {
				if (!a_validity.RowIsValid(i) || !b_validity.RowIsValid(i)) {
					result_validity.SetInvalid(i);
					continue;
				}
				__int128 b_val = HugeintToInt128(b_data[i]);
				if (__builtin_expect(b_val == 0, 0)) {
					result_validity.SetInvalid(i);
					continue;
				}
				__int128 a_val = HugeintToInt128(a_data[i]);
				WriteResult(result_data, i, SparkDecimalDivide(a_val, b_val, pow10_val));
			}
		}
		return;
	}

	// UnifiedVectorFormat is required for constant/dictionary vectors; always use
	// its selection indices and validity mask when reading each logical row.
	UnifiedVectorFormat a_fmt, b_fmt;
	a_vec.ToUnifiedFormat(count, a_fmt);
	b_vec.ToUnifiedFormat(count, b_fmt);

	const auto *__restrict a_data = UnifiedVectorFormat::GetData<hugeint_t>(a_fmt);
	const auto *__restrict b_data = UnifiedVectorFormat::GetData<hugeint_t>(b_fmt);

	for (idx_t i = 0; i < count; i++) {
		auto a_idx = a_fmt.sel->get_index(i);
		auto b_idx = b_fmt.sel->get_index(i);

		// NULL operands and division by zero produce NULL, not a numeric result.
		if (!a_fmt.validity.RowIsValid(a_idx) || !b_fmt.validity.RowIsValid(b_idx)) {
			result_validity.SetInvalid(i);
			continue;
		}

		__int128 b_val = HugeintToInt128(b_data[b_idx]);

		if (__builtin_expect(b_val == 0, 0)) {
			result_validity.SetInvalid(i);
			continue;
		}

		__int128 a_val = HugeintToInt128(a_data[a_idx]);
		__int128 div_result = SparkDecimalDivide(a_val, b_val, pow10_val);

		WriteResult(result_data, i, div_result);
	}
}

static LogicalType PromoteIntToDecimal(const LogicalType &type) {
	switch (type.id()) {
	case LogicalTypeId::TINYINT:
		return LogicalType::DECIMAL(3, 0);
	case LogicalTypeId::SMALLINT:
		return LogicalType::DECIMAL(5, 0);
	case LogicalTypeId::INTEGER:
		return LogicalType::DECIMAL(10, 0);
	case LogicalTypeId::BIGINT:
		return LogicalType::DECIMAL(19, 0);
	case LogicalTypeId::HUGEINT:
		return LogicalType::DECIMAL(38, 0);
	default:
		return LogicalType::INVALID;
	}
}

static unique_ptr<FunctionData> BindSparkDecimalDiv(ClientContext &context, ScalarFunction &bound_function,
                                                    vector<unique_ptr<Expression>> &arguments) {
	auto type_a = arguments[0]->return_type;
	auto type_b = arguments[1]->return_type;

	// DuckDB binds integer literals before the DECIMAL operator override, so
	// promote integral operands here as well.
	if (type_a.id() != LogicalTypeId::DECIMAL) {
		auto promoted = PromoteIntToDecimal(type_a);
		if (promoted.id() != LogicalTypeId::INVALID) {
			type_a = promoted;
		}
	}
	if (type_b.id() != LogicalTypeId::DECIMAL) {
		auto promoted = PromoteIntToDecimal(type_b);
		if (promoted.id() != LogicalTypeId::INVALID) {
			type_b = promoted;
		}
	}

	if (type_a.id() != LogicalTypeId::DECIMAL || type_b.id() != LogicalTypeId::DECIMAL) {
		throw InvalidInputException("spark_decimal_div requires DECIMAL arguments, got %s and %s", type_a.ToString(),
		                            type_b.ToString());
	}

	uint8_t p1 = DecimalType::GetWidth(type_a);
	uint8_t s1 = DecimalType::GetScale(type_a);
	uint8_t p2 = DecimalType::GetWidth(type_b);
	uint8_t s2 = DecimalType::GetScale(type_b);

	auto result = ComputeDivisionType(p1, s1, p2, s2);

	uint32_t scale_adj = static_cast<uint32_t>(result.scale) - static_cast<uint32_t>(s1) + static_cast<uint32_t>(s2);

	bound_function.arguments[0] = LogicalType::DECIMAL(38, s1);
	bound_function.arguments[1] = LogicalType::DECIMAL(38, s2);

	auto result_type = LogicalType::DECIMAL(result.precision, result.scale);
	bound_function.return_type = result_type;

	switch (result_type.InternalType()) {
	case PhysicalType::INT16:
		bound_function.function = SparkDivExec<int16_t>;
		break;
	case PhysicalType::INT32:
		bound_function.function = SparkDivExec<int32_t>;
		break;
	case PhysicalType::INT64:
		bound_function.function = SparkDivExec<int64_t>;
		break;
	case PhysicalType::INT128:
		bound_function.function = SparkDivExec<hugeint_t>;
		break;
	default:
		throw InternalException("Unexpected physical type for DECIMAL result");
	}

	return make_uniq<SparkDivBindData>(scale_adj);
}

// spark_try_divide: Spark 3.5+ try_divide(dividend, divisor)
//
// divisor == 0 (or NULL operand) -> NULL. Otherwise divide using Spark's type
// promotion: any DECIMAL operand -> DECIMAL result (identical to spark_decimal_div,
// which already returns NULL on zero); all other numeric operands -> DOUBLE.
static void SparkTryDivideDoubleExec(DataChunk &args, ExpressionState &state, Vector &result) {
	idx_t count = args.size();
	result.SetVectorType(VectorType::FLAT_VECTOR);
	auto *__restrict result_data = FlatVector::GetData<double>(result);
	auto &result_validity = FlatVector::Validity(result);

	UnifiedVectorFormat a_fmt, b_fmt;
	args.data[0].ToUnifiedFormat(count, a_fmt);
	args.data[1].ToUnifiedFormat(count, b_fmt);
	const auto *__restrict a_data = UnifiedVectorFormat::GetData<double>(a_fmt);
	const auto *__restrict b_data = UnifiedVectorFormat::GetData<double>(b_fmt);

	for (idx_t i = 0; i < count; i++) {
		auto a_idx = a_fmt.sel->get_index(i);
		auto b_idx = b_fmt.sel->get_index(i);
		if (!a_fmt.validity.RowIsValid(a_idx) || !b_fmt.validity.RowIsValid(b_idx)) {
			result_validity.SetInvalid(i);
			continue;
		}
		double b_val = b_data[b_idx];
		if (b_val == 0.0) {
			result_validity.SetInvalid(i);
			continue;
		}
		result_data[i] = a_data[a_idx] / b_val;
	}
}

static unique_ptr<FunctionData> BindSparkTryDivide(ClientContext &context, ScalarFunction &bound_function,
                                                   vector<unique_ptr<Expression>> &arguments) {
	auto type_a = arguments[0]->return_type;
	auto type_b = arguments[1]->return_type;

	auto is_decimal_or_integral = [](LogicalTypeId id) {
		switch (id) {
		case LogicalTypeId::DECIMAL:
		case LogicalTypeId::TINYINT:
		case LogicalTypeId::SMALLINT:
		case LogicalTypeId::INTEGER:
		case LogicalTypeId::BIGINT:
		case LogicalTypeId::HUGEINT:
			return true;
		default:
			return false;
		}
	};
	if ((type_a.id() == LogicalTypeId::DECIMAL || type_b.id() == LogicalTypeId::DECIMAL) &&
	    is_decimal_or_integral(type_a.id()) && is_decimal_or_integral(type_b.id())) {
		return BindSparkDecimalDiv(context, bound_function, arguments);
	}

	bound_function.arguments[0] = LogicalType::DOUBLE;
	bound_function.arguments[1] = LogicalType::DOUBLE;
	bound_function.return_type = LogicalType::DOUBLE;
	bound_function.function = SparkTryDivideDoubleExec;
	return nullptr;
}

static void LoadInternal(ExtensionLoader &loader) {
	vector<LogicalType> args = {LogicalType::ANY, LogicalType::ANY};
	ScalarFunction func("spark_decimal_div", std::move(args), LogicalType::ANY, SparkDivExec<hugeint_t>,
	                    BindSparkDecimalDiv);
	func.null_handling = FunctionNullHandling::SPECIAL_HANDLING;

	loader.RegisterFunction(func);

	vector<LogicalType> try_div_args = {LogicalType::ANY, LogicalType::ANY};
	ScalarFunction try_div("spark_try_divide", std::move(try_div_args), LogicalType::ANY, SparkTryDivideDoubleExec,
	                       BindSparkTryDivide);
	try_div.null_handling = FunctionNullHandling::SPECIAL_HANDLING;
	loader.RegisterFunction(try_div);

	// Register every Spark-compatible scalar, aggregate, JSON, and hash entry
	// point here; DuckDB COUNT already supplies Spark's BIGINT count semantics.
	loader.RegisterFunction(CreateSparkSumFunctionSet());
	loader.RegisterFunction(CreateSparkAvgFunctionSet());
	loader.RegisterFunction(CreateSparkTrySumFunctionSet());
	loader.RegisterFunction(CreateSparkTryAvgFunctionSet());
	loader.RegisterFunction(CreateSparkSkewnessFunction());
	loader.RegisterFunction(CreateSparkSchemaOfJsonFunction());

	loader.RegisterFunction(CreateSparkXxhash64Function());
	loader.RegisterFunction(CreateSparkHashFunction());
}

void ThdckSparkFuncsExtension::Load(ExtensionLoader &loader) {
	LoadInternal(loader);
}

std::string ThdckSparkFuncsExtension::Name() {
	return "thdck_spark_funcs";
}

} // namespace duckdb

extern "C" {

DUCKDB_CPP_EXTENSION_ENTRY(thdck_spark_funcs, loader) {
	duckdb::LoadInternal(loader);
}

DUCKDB_EXTENSION_API const char *thdck_spark_funcs_version() {
	return duckdb::DuckDB::LibraryVersion();
}
}

#ifndef DUCKDB_EXTENSION_MAIN
#error DUCKDB_EXTENSION_MAIN not defined
#endif
