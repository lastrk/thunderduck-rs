#pragma once

#include "duckdb.hpp"
#include "duckdb/function/aggregate_function.hpp"
#include "duckdb/common/types/decimal.hpp"
#include "spark_precision.hpp"
#include "wide_integer.hpp"
#include "spark_aggregates.hpp"

namespace duckdb {

// spark_try_sum returns BIGINT/DECIMAL(min(p+10, 38), s) and returns NULL for
// arithmetic overflow or a decimal sum outside the declared Spark precision.
struct SparkTrySumIntegerState {
	int64_t value;
	bool isset;
	bool overflow;

	void Initialize() {
		value = 0;
		isset = false;
		overflow = false;
	}

	void Combine(const SparkTrySumIntegerState &other) {
		if (!other.isset) {
			return;
		}
		isset = true;
		if (other.overflow || __builtin_add_overflow(value, other.value, &value)) {
			overflow = true;
		}
	}
};
SPARK_ASSERT_STATE_LAYOUT(SparkTrySumIntegerState);

struct SparkTrySumIntegerOperation {
	template <class STATE>
	static void Initialize(STATE &state) {
		state.Initialize();
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void Operation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &) {
		state.isset = true;
		if (state.overflow) {
			return;
		}
		if (__builtin_add_overflow(state.value, static_cast<int64_t>(input), &state.value)) {
			state.overflow = true;
		}
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void ConstantOperation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &, idx_t count) {
		state.isset = true;
		if (state.overflow) {
			return;
		}
		int64_t add;
		if (__builtin_mul_overflow(static_cast<int64_t>(input), static_cast<int64_t>(count), &add) ||
		    __builtin_add_overflow(state.value, add, &state.value)) {
			state.overflow = true;
		}
	}

	template <class STATE, class OP>
	static void Combine(const STATE &source, STATE &target, AggregateInputData &) {
		target.Combine(source);
	}

	template <class T, class STATE>
	static void Finalize(STATE &state, T &target, AggregateFinalizeData &finalize_data) {
		if (!state.isset || state.overflow) {
			finalize_data.ReturnNull();
			return;
		}
		target = state.value;
	}

	static bool IgnoreNull() {
		return true;
	}
};

struct SparkTrySumDecimalBindData : public FunctionData {
	uint8_t result_precision;

	explicit SparkTrySumDecimalBindData(uint8_t p) : result_precision(p) {
	}
	unique_ptr<FunctionData> Copy() const override {
		return make_uniq<SparkTrySumDecimalBindData>(result_precision);
	}
	bool Equals(const FunctionData &other_p) const override {
		return result_precision == other_p.Cast<SparkTrySumDecimalBindData>().result_precision;
	}
};

// Keep the state 8-byte aligned; convert to __int128 only for arithmetic.
struct SparkTrySumDecimalState {
	hugeint_t value;
	bool isset;
	bool overflow;

	void Initialize() {
		value = hugeint_t(0, 0);
		isset = false;
		overflow = false;
	}
	void Combine(const SparkTrySumDecimalState &other) {
		if (!other.isset) {
			return;
		}
		isset = true;
		__int128 sum;
		if (overflow || other.overflow ||
		    __builtin_add_overflow(HugeintToInt128(value), HugeintToInt128(other.value), &sum)) {
			overflow = true;
			return;
		}
		value = Int128ToHugeint(sum);
	}
};
SPARK_ASSERT_STATE_LAYOUT(SparkTrySumDecimalState);

template <typename RESULT_TYPE>
struct SparkTrySumDecimalOperation {
	template <class STATE>
	static void Initialize(STATE &state) {
		state.Initialize();
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void Operation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &) {
		state.isset = true;
		if (state.overflow) {
			return;
		}
		__int128 input_val = HugeintToInt128(input);
		__int128 sum;
		if (__builtin_add_overflow(HugeintToInt128(state.value), input_val, &sum)) {
			state.overflow = true;
			return;
		}
		state.value = Int128ToHugeint(sum);
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void ConstantOperation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &, idx_t count) {
		state.isset = true;
		if (state.overflow) {
			return;
		}
		__int128 input_val = HugeintToInt128(input);
		__int128 mul;
		__int128 sum;
		if (__builtin_mul_overflow(input_val, static_cast<__int128>(count), &mul) ||
		    __builtin_add_overflow(HugeintToInt128(state.value), mul, &sum)) {
			state.overflow = true;
			return;
		}
		state.value = Int128ToHugeint(sum);
	}
	template <class STATE, class OP>
	static void Combine(const STATE &source, STATE &target, AggregateInputData &) {
		target.Combine(source);
	}

	template <class T, class STATE>
	static void Finalize(STATE &state, T &target, AggregateFinalizeData &finalize_data) {
		if (!state.isset || state.overflow) {
			finalize_data.ReturnNull();
			return;
		}
		auto &bind_data = finalize_data.input.bind_data->Cast<SparkTrySumDecimalBindData>();
		unsigned __int128 limit = Pow10_128(bind_data.result_precision);
		__int128 value = HugeintToInt128(state.value);
		// The sum may fit in __int128 but still exceed Spark's result precision.
		if (Abs128(value) >= limit) {
			finalize_data.ReturnNull();
			return;
		}
		WriteAggResult(target, value);
	}

	static bool IgnoreNull() {
		return true;
	}
};

template <typename RESULT_TYPE>
static AggregateFunction GetSparkTrySumDecimalFunction() {
	return SparkUnaryAggregate<SparkTrySumDecimalState, hugeint_t, RESULT_TYPE,
	                           SparkTrySumDecimalOperation<RESULT_TYPE>>(LogicalType::DECIMAL(38, 0),
	                                                                     LogicalType::DECIMAL(38, 0));
}

static AggregateFunction GetSparkTrySumByPhysicalType(PhysicalType pt) {
	switch (pt) {
	case PhysicalType::INT16:
		return GetSparkTrySumDecimalFunction<int16_t>();
	case PhysicalType::INT32:
		return GetSparkTrySumDecimalFunction<int32_t>();
	case PhysicalType::INT64:
		return GetSparkTrySumDecimalFunction<int64_t>();
	case PhysicalType::INT128:
		return GetSparkTrySumDecimalFunction<hugeint_t>();
	default:
		throw InternalException("Unexpected physical type for spark_try_sum DECIMAL result");
	}
}

static unique_ptr<FunctionData> BindSparkTrySumDecimal(ClientContext &, AggregateFunction &function,
                                                       vector<unique_ptr<Expression>> &arguments) {
	auto &type = arguments[0]->return_type;
	if (type.id() != LogicalTypeId::DECIMAL) {
		throw InvalidInputException("spark_try_sum DECIMAL overload requires DECIMAL argument");
	}
	uint8_t p = DecimalType::GetWidth(type);
	uint8_t s = DecimalType::GetScale(type);
	auto result = ComputeSumType(p, s);

	function.arguments[0] = LogicalType::DECIMAL(38, s);
	auto result_type = LogicalType::DECIMAL(result.precision, result.scale);
	function.return_type = result_type;

	auto tf = GetSparkTrySumByPhysicalType(result_type.InternalType());
	function.update = tf.update;
	function.combine = tf.combine;
	function.finalize = tf.finalize;
	function.simple_update = tf.simple_update;

	return make_uniq<SparkTrySumDecimalBindData>(result.precision);
}

inline AggregateFunctionSet CreateSparkTrySumFunctionSet() {
	AggregateFunctionSet set("spark_try_sum");

	auto decimal_func = GetSparkTrySumDecimalFunction<hugeint_t>();
	decimal_func.bind = BindSparkTrySumDecimal;
	decimal_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(decimal_func);

	{
		auto f = SparkUnaryAggregate<SparkTrySumIntegerState, int8_t, int64_t, SparkTrySumIntegerOperation>(
		    LogicalType::TINYINT, LogicalType::BIGINT);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTrySumIntegerState, int16_t, int64_t, SparkTrySumIntegerOperation>(
		    LogicalType::SMALLINT, LogicalType::BIGINT);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTrySumIntegerState, int32_t, int64_t, SparkTrySumIntegerOperation>(
		    LogicalType::INTEGER, LogicalType::BIGINT);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTrySumIntegerState, int64_t, int64_t, SparkTrySumIntegerOperation>(
		    LogicalType::BIGINT, LogicalType::BIGINT);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	return set;
}

// spark_try_avg uses DOUBLE for integer/floating inputs and does not turn a
// large integer average into NULL; DECIMAL inputs reuse Spark's decimal average
// rules, whose result remains within the input range.

struct SparkTryAvgDoubleState {
	double sum;
	uint64_t count;

	void Initialize() {
		sum = 0;
		count = 0;
	}
	void Combine(const SparkTryAvgDoubleState &other) {
		sum += other.sum;
		count += other.count;
	}
};
SPARK_ASSERT_STATE_LAYOUT(SparkTryAvgDoubleState);

struct SparkTryAvgDoubleOperation {
	template <class STATE>
	static void Initialize(STATE &state) {
		state.Initialize();
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void Operation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &) {
		state.count++;
		state.sum += static_cast<double>(input);
	}

	template <class INPUT_TYPE, class STATE, class OP>
	static void ConstantOperation(STATE &state, const INPUT_TYPE &input, AggregateUnaryInput &, idx_t count) {
		state.count += count;
		state.sum += static_cast<double>(input) * static_cast<double>(count);
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
		target = state.sum / static_cast<double>(state.count);
	}

	static bool IgnoreNull() {
		return true;
	}
};

inline AggregateFunctionSet CreateSparkTryAvgFunctionSet() {
	AggregateFunctionSet set("spark_try_avg");

	auto decimal_func = GetSparkAvgDecimalFunction<hugeint_t>();
	decimal_func.bind = BindSparkAvgDecimal;
	decimal_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(decimal_func);

	{
		auto f = SparkUnaryAggregate<SparkTryAvgDoubleState, int8_t, double, SparkTryAvgDoubleOperation>(
		    LogicalType::TINYINT, LogicalType::DOUBLE);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTryAvgDoubleState, int16_t, double, SparkTryAvgDoubleOperation>(
		    LogicalType::SMALLINT, LogicalType::DOUBLE);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTryAvgDoubleState, int32_t, double, SparkTryAvgDoubleOperation>(
		    LogicalType::INTEGER, LogicalType::DOUBLE);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTryAvgDoubleState, int64_t, double, SparkTryAvgDoubleOperation>(
		    LogicalType::BIGINT, LogicalType::DOUBLE);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTryAvgDoubleState, float, double, SparkTryAvgDoubleOperation>(
		    LogicalType::FLOAT, LogicalType::DOUBLE);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	{
		auto f = SparkUnaryAggregate<SparkTryAvgDoubleState, double, double, SparkTryAvgDoubleOperation>(
		    LogicalType::DOUBLE, LogicalType::DOUBLE);
		f.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
		set.AddFunction(f);
	}
	return set;
}

} // namespace duckdb
