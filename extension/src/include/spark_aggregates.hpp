#pragma once

#include "duckdb.hpp"
#include "duckdb/function/aggregate_function.hpp"
#include "duckdb/function/aggregate_state.hpp"
#include "duckdb/common/types/decimal.hpp"
#include "spark_precision.hpp"
#include "wide_integer.hpp"
#include "decimal_division.hpp"
#include <cmath>

// DuckDB places aggregate states at 8-byte-aligned offsets. Keep every state
// aligned to 8 bytes and sized to an 8-byte multiple; these checks are needed
// because release builds do not enforce either layout invariant. In particular,
// store hugeint_t rather than a raw __int128 and convert for stack arithmetic.

#define SPARK_ASSERT_STATE_LAYOUT(STATE_TYPE)                                                                          \
	static_assert(duckdb::SparkStateLayout<STATE_TYPE>::ok,                                                            \
	              #STATE_TYPE " violates the DuckDB aggregate-state layout invariants; see SparkStateLayout")

namespace duckdb {

template <class STATE>
struct SparkStateLayout {
	static_assert(alignof(STATE) <= 8, "aggregate state must not require >8-byte alignment: DuckDB aligns aggregate "
	                                   "states to 8 bytes, so an over-aligned member (e.g. a raw __int128) is "
	                                   "undefined behavior. Store hugeint_t and convert for arithmetic instead.");
	static_assert(sizeof(STATE) % 8 == 0, "aggregate state size must be a multiple of 8: DuckDB advances the row "
	                                      "layout by sizeof(state), so an odd size misaligns the FOLLOWING state. "
	                                      "DuckDB only D_ASSERTs this, which is a no-op in release builds.");
	static const bool ok = true;
};

// Every Spark aggregate uses this wrapper so a new registration cannot bypass
// the layout assertions. DuckDB owns these inline, trivially destructible
// states; Spark needs neither a custom NULL handler nor a destructor callback.
template <class STATE, class INPUT_TYPE, class RESULT_TYPE, class OP>
static AggregateFunction SparkUnaryAggregate(const LogicalType &input_type, LogicalType return_type) {
	static_assert(SparkStateLayout<STATE>::ok, "aggregate state violates the DuckDB layout invariants");
	return AggregateFunction::UnaryAggregate<STATE, INPUT_TYPE, RESULT_TYPE, OP>(input_type, return_type);
}

// Bind data is copied with the bound function and remains alive through
// Finalize, where DECIMAL input/result scales are needed after state merges.
struct SparkAggBindData : public FunctionData {
	uint8_t input_scale;
	uint8_t result_scale;

	SparkAggBindData(uint8_t input_scale_p, uint8_t result_scale_p)
	    : input_scale(input_scale_p), result_scale(result_scale_p) {
	}

	unique_ptr<FunctionData> Copy() const override {
		return make_uniq<SparkAggBindData>(input_scale, result_scale);
	}

	bool Equals(const FunctionData &other_p) const override {
		auto &other = other_p.Cast<SparkAggBindData>();
		return input_scale == other.input_scale && result_scale == other.result_scale;
	}
};

template <typename T>
static inline void WriteAggResult(T &target, __int128 val) {
	target = static_cast<T>(val);
}

template <>
inline void WriteAggResult<hugeint_t>(hugeint_t &target, __int128 val) {
	target = Int128ToHugeint(val);
}

// spark_sum DECIMAL path; the result is DECIMAL(min(p+10, 38), s).

struct SparkSumDecimalState {
	hugeint_t value;
	bool isset;

	void Initialize() {
		isset = false;
		value = hugeint_t(0, 0);
	}

	void Combine(const SparkSumDecimalState &other) {
		if (other.isset) {
			isset = true;
			__int128 result = HugeintToInt128(value) + HugeintToInt128(other.value);
			value = Int128ToHugeint(result);
		}
	}
};
SPARK_ASSERT_STATE_LAYOUT(SparkSumDecimalState);

template <typename RESULT_TYPE>
struct SparkSumDecimalOperation {
	template <class STATE>
	static void Initialize(STATE &state) {
		state.Initialize();
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void Operation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &) {
		state.isset = true;
		__int128 result = HugeintToInt128(state.value) + HugeintToInt128(input);
		state.value = Int128ToHugeint(result);
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void ConstantOperation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &, idx_t count) {
		state.isset = true;
		__int128 result = HugeintToInt128(state.value) + HugeintToInt128(input) * static_cast<__int128>(count);
		state.value = Int128ToHugeint(result);
	}

	template <class STATE, class OP>
	static void Combine(const STATE &source, STATE &target, AggregateInputData &) {
		target.Combine(source);
	}

	template <class T, class STATE>
	static void Finalize(STATE &state, T &target, AggregateFinalizeData &finalize_data) {
		if (!state.isset) {
			finalize_data.ReturnNull();
		} else {
			WriteAggResult(target, HugeintToInt128(state.value));
		}
	}

	static bool IgnoreNull() {
		return true;
	}
};

template <typename RESULT_TYPE>
static AggregateFunction GetSparkSumDecimalFunction() {
	return SparkUnaryAggregate<SparkSumDecimalState, hugeint_t, RESULT_TYPE, SparkSumDecimalOperation<RESULT_TYPE>>(
	    LogicalType::DECIMAL(38, 0), LogicalType::DECIMAL(38, 0));
}

static AggregateFunction GetSparkSumByPhysicalType(PhysicalType pt) {
	switch (pt) {
	case PhysicalType::INT16:
		return GetSparkSumDecimalFunction<int16_t>();
	case PhysicalType::INT32:
		return GetSparkSumDecimalFunction<int32_t>();
	case PhysicalType::INT64:
		return GetSparkSumDecimalFunction<int64_t>();
	case PhysicalType::INT128:
		return GetSparkSumDecimalFunction<hugeint_t>();
	default:
		throw InternalException("Unexpected physical type for spark_sum DECIMAL result");
	}
}

static unique_ptr<FunctionData> BindSparkSumDecimal(ClientContext &context, AggregateFunction &function,
                                                    vector<unique_ptr<Expression>> &arguments) {
	auto &type = arguments[0]->return_type;
	if (type.id() != LogicalTypeId::DECIMAL) {
		throw InvalidInputException("spark_sum DECIMAL overload requires DECIMAL argument");
	}

	uint8_t p = DecimalType::GetWidth(type);
	uint8_t s = DecimalType::GetScale(type);
	auto result = ComputeSumType(p, s);

	function.arguments[0] = LogicalType::DECIMAL(38, s);
	auto result_type = LogicalType::DECIMAL(result.precision, result.scale);
	function.return_type = result_type;

	{
		auto tf = GetSparkSumByPhysicalType(result_type.InternalType());
		function.update = tf.update;
		function.combine = tf.combine;
		function.finalize = tf.finalize;
		function.simple_update = tf.simple_update;
	}

	return make_uniq<SparkAggBindData>(s, result.scale);
}

// spark_sum integer path; Spark returns BIGINT.

struct SparkSumIntegerState {
	int64_t value;
	bool isset;

	void Initialize() {
		isset = false;
		value = 0;
	}

	void Combine(const SparkSumIntegerState &other) {
		if (other.isset) {
			isset = true;
			value += other.value;
		}
	}
};
SPARK_ASSERT_STATE_LAYOUT(SparkSumIntegerState);

struct SparkSumIntegerOperation {
	template <class STATE>
	static void Initialize(STATE &state) {
		state.Initialize();
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void Operation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &) {
		state.isset = true;
		state.value += static_cast<int64_t>(input);
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void ConstantOperation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &, idx_t count) {
		state.isset = true;
		state.value += static_cast<int64_t>(input) * static_cast<int64_t>(count);
	}

	template <class STATE, class OP>
	static void Combine(const STATE &source, STATE &target, AggregateInputData &) {
		target.Combine(source);
	}

	template <class T, class STATE>
	static void Finalize(STATE &state, T &target, AggregateFinalizeData &finalize_data) {
		if (!state.isset) {
			finalize_data.ReturnNull();
		} else {
			target = state.value;
		}
	}

	static bool IgnoreNull() {
		return true;
	}
};

// spark_avg DECIMAL path returns DECIMAL(min(p+4, 38), min(s+4, 18)); division
// uses SparkDecimalDivide and ROUND_HALF_UP.

struct SparkAvgDecimalState {
	hugeint_t sum;
	uint64_t count;

	void Initialize() {
		count = 0;
		sum = hugeint_t(0, 0);
	}

	void Combine(const SparkAvgDecimalState &other) {
		count += other.count;
		__int128 result = HugeintToInt128(sum) + HugeintToInt128(other.sum);
		sum = Int128ToHugeint(result);
	}
};
SPARK_ASSERT_STATE_LAYOUT(SparkAvgDecimalState);

template <typename RESULT_TYPE>
struct SparkAvgDecimalOperation {
	template <class STATE>
	static void Initialize(STATE &state) {
		state.Initialize();
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void Operation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &) {
		state.count++;
		__int128 result = HugeintToInt128(state.sum) + HugeintToInt128(input);
		state.sum = Int128ToHugeint(result);
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void ConstantOperation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &, idx_t count) {
		state.count += count;
		__int128 result = HugeintToInt128(state.sum) + HugeintToInt128(input) * static_cast<__int128>(count);
		state.sum = Int128ToHugeint(result);
	}

	template <class STATE, class OP>
	static void Combine(const STATE &source, STATE &target, AggregateInputData &) {
		target.Combine(source);
	}

	template <class T, class STATE>
	static void Finalize(STATE &state, T &target, AggregateFinalizeData &finalize_data) {
		if (state.count == 0) {
			finalize_data.ReturnNull();
			return;
		}

		auto &bind_data = finalize_data.input.bind_data->Cast<SparkAggBindData>();
		uint32_t scale_adj =
		    static_cast<uint32_t>(bind_data.result_scale) - static_cast<uint32_t>(bind_data.input_scale);

		__int128 sum_val = HugeintToInt128(state.sum);
		__int128 count_val = static_cast<__int128>(state.count);
		unsigned __int128 pow10_val = (scale_adj > 0) ? Pow10_128(scale_adj) : 0;
		__int128 result = SparkDecimalDivide(sum_val, count_val, pow10_val);

		WriteAggResult(target, result);
	}

	static bool IgnoreNull() {
		return true;
	}
};

template <typename RESULT_TYPE>
static AggregateFunction GetSparkAvgDecimalFunction() {
	return SparkUnaryAggregate<SparkAvgDecimalState, hugeint_t, RESULT_TYPE, SparkAvgDecimalOperation<RESULT_TYPE>>(
	    LogicalType::DECIMAL(38, 0), LogicalType::DECIMAL(38, 0));
}

static AggregateFunction GetSparkAvgByPhysicalType(PhysicalType pt) {
	switch (pt) {
	case PhysicalType::INT16:
		return GetSparkAvgDecimalFunction<int16_t>();
	case PhysicalType::INT32:
		return GetSparkAvgDecimalFunction<int32_t>();
	case PhysicalType::INT64:
		return GetSparkAvgDecimalFunction<int64_t>();
	case PhysicalType::INT128:
		return GetSparkAvgDecimalFunction<hugeint_t>();
	default:
		throw InternalException("Unexpected physical type for spark_avg DECIMAL result");
	}
}

static unique_ptr<FunctionData> BindSparkAvgDecimal(ClientContext &context, AggregateFunction &function,
                                                    vector<unique_ptr<Expression>> &arguments) {
	auto &type = arguments[0]->return_type;
	if (type.id() != LogicalTypeId::DECIMAL) {
		throw InvalidInputException("spark_avg DECIMAL overload requires DECIMAL argument");
	}

	uint8_t p = DecimalType::GetWidth(type);
	uint8_t s = DecimalType::GetScale(type);
	auto result = ComputeAvgType(p, s);

	function.arguments[0] = LogicalType::DECIMAL(38, s);
	auto result_type = LogicalType::DECIMAL(result.precision, result.scale);
	function.return_type = result_type;

	{
		auto tf = GetSparkAvgByPhysicalType(result_type.InternalType());
		function.update = tf.update;
		function.combine = tf.combine;
		function.finalize = tf.finalize;
		function.simple_update = tf.simple_update;
	}

	return make_uniq<SparkAggBindData>(s, result.scale);
}

inline AggregateFunctionSet CreateSparkSumFunctionSet() {
	AggregateFunctionSet set("spark_sum");

	auto decimal_func = GetSparkSumDecimalFunction<hugeint_t>();
	decimal_func.bind = BindSparkSumDecimal;
	decimal_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(decimal_func);

	auto tinyint_func = SparkUnaryAggregate<SparkSumIntegerState, int8_t, int64_t, SparkSumIntegerOperation>(
	    LogicalType::TINYINT, LogicalType::BIGINT);
	tinyint_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(tinyint_func);

	auto smallint_func = SparkUnaryAggregate<SparkSumIntegerState, int16_t, int64_t, SparkSumIntegerOperation>(
	    LogicalType::SMALLINT, LogicalType::BIGINT);
	smallint_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(smallint_func);

	auto int_func = SparkUnaryAggregate<SparkSumIntegerState, int32_t, int64_t, SparkSumIntegerOperation>(
	    LogicalType::INTEGER, LogicalType::BIGINT);
	int_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(int_func);

	auto bigint_func = SparkUnaryAggregate<SparkSumIntegerState, int64_t, int64_t, SparkSumIntegerOperation>(
	    LogicalType::BIGINT, LogicalType::BIGINT);
	bigint_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(bigint_func);

	return set;
}

inline AggregateFunctionSet CreateSparkAvgFunctionSet() {
	AggregateFunctionSet set("spark_avg");

	auto decimal_func = GetSparkAvgDecimalFunction<hugeint_t>();
	decimal_func.bind = BindSparkAvgDecimal;
	decimal_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(decimal_func);

	return set;
}

// Spark population skewness. Pebay's online moments avoid the cancellation in
// Spark's literal update under fused floating-point contraction. Keep
// sqrt((m2*m2)*m2) rather than pow(m2, 1.5): Spark overflows m2^3 to infinity,
// yielding 0.0, and propagates NaN instead of raising DuckDB's range error.

struct SparkSkewState {
	uint64_t n;
	double mean;
	double m2;
	double m3;
};
SPARK_ASSERT_STATE_LAYOUT(SparkSkewState);

struct SparkSkewnessOperation {
	template <class STATE>
	static void Initialize(STATE &state) {
		state.n = 0;
		state.mean = 0;
		state.m2 = 0;
		state.m3 = 0;
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void ConstantOperation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &unary_input,
	                              idx_t count) {
		for (idx_t i = 0; i < count; i++) {
			Operation<INPUT_TYPE, STATE, OP>(state, input, unary_input);
		}
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void Operation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &unary_input) {
		double x = static_cast<double>(input);
		uint64_t n1 = state.n;
		state.n++;
		double n_new = static_cast<double>(state.n);
		double delta = x - state.mean;
		double delta_n = delta / n_new;
		double term1 = delta * delta_n * static_cast<double>(n1);
		state.m3 += term1 * delta_n * (n_new - 2.0) - 3.0 * delta_n * state.m2;
		state.m2 += term1;
		state.mean += delta_n;
	}

	template <class STATE, class OP>
	static void Combine(const STATE &source, STATE &target, AggregateInputData &) {
		// DuckDB merges empty and zero-initialized partial states. These identity
		// branches avoid inf*0 -> NaN for extreme values and preserve the source
		// state when the target has no rows.
		if (source.n == 0) {
			return;
		}
		if (target.n == 0) {
			target.n = source.n;
			target.mean = source.mean;
			target.m2 = source.m2;
			target.m3 = source.m3;
			return;
		}
		double nA = static_cast<double>(target.n);
		double nB = static_cast<double>(source.n);
		double n_combined = nA + nB;
		double delta = source.mean - target.mean;
		double delta2 = delta * delta;
		double delta3 = delta2 * delta;
		double nA_nB = nA * nB;

		double new_m3 = target.m3 + source.m3 + delta3 * nA_nB * (nA - nB) / (n_combined * n_combined) +
		                3.0 * delta * (nA * source.m2 - nB * target.m2) / n_combined;
		double new_m2 = target.m2 + source.m2 + delta2 * nA_nB / n_combined;
		double new_mean = (nA * target.mean + nB * source.mean) / n_combined;

		target.n += source.n;
		target.mean = new_mean;
		target.m2 = new_m2;
		target.m3 = new_m3;
	}

	template <class TARGET_TYPE, class STATE>
	static void Finalize(STATE &state, TARGET_TYPE &target, AggregateFinalizeData &finalize_data) {
		// Finite singletons have zero variance and return NULL like Spark; a
		// non-finite singleton is a documented divergence (Spark returns NaN).
		if (state.n < 2 || state.m2 == 0.0) {
			finalize_data.ReturnNull();
			return;
		}
		double n = static_cast<double>(state.n);
		target = (std::sqrt(n) * state.m3) / std::sqrt((state.m2 * state.m2) * state.m2);
	}

	static bool IgnoreNull() {
		return true;
	}
};

inline AggregateFunction CreateSparkSkewnessFunction() {
	auto func = SparkUnaryAggregate<SparkSkewState, double, double, SparkSkewnessOperation>(LogicalType::DOUBLE,
	                                                                                        LogicalType::DOUBLE);
	func.name = "spark_skewness";
	func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	return func;
}

} // namespace duckdb
