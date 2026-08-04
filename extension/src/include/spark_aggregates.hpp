#pragma once

#include "duckdb.hpp"
#include "duckdb/function/aggregate_function.hpp"
#include "duckdb/function/aggregate_state.hpp"
#include "duckdb/common/types/decimal.hpp"
#include "spark_precision.hpp"
#include "wide_integer.hpp"
#include "decimal_division.hpp"
#include <cmath>

// ============================================================================
// Aggregate-state alignment invariant
//
// DuckDB lays aggregate states out inside its row format and aligns their
// offsets with AlignValue<T, val=8> — that is, to 8 bytes ONLY (see
// duckdb/src/common/types/row/tuple_data_layout.cpp and
// duckdb/src/include/duckdb/common/helper.hpp). A state that requires stronger
// alignment therefore lands at a misaligned address in practice (observed at
// addr % 16 == 8 for a plain GROUP BY over DECIMAL), and every access to an
// over-aligned member is undefined behavior.
//
// Consequence: an aggregate state must never contain a raw `__int128` (alignof
// 16). Store `hugeint_t` (alignof 8) and convert to `__int128` only for
// stack-local arithmetic, via HugeintToInt128 / Int128ToHugeint.
//
// SPARK_ASSERT_STATE_ALIGNMENT below makes this mechanical, which matters
// because no runtime test can be relied on to catch a regression: the symptom is
// optimizer- and ISA-dependent (aarch64 LDP/STP and x86 movdqu tolerate 8-byte
// alignment, so misaligned code can still produce correct results). Note the
// check is opt-in per state — a NEW aggregate state must add its own invocation.
// ============================================================================

#define SPARK_ASSERT_STATE_ALIGNMENT(STATE_TYPE)                                                                       \
	static_assert(alignof(STATE_TYPE) <= 8, #STATE_TYPE " must not require >8-byte alignment: DuckDB aligns "          \
	                                                    "aggregate states to 8 bytes, so an over-aligned member "      \
	                                                    "(e.g. a raw __int128) is undefined behavior. Store "          \
	                                                    "hugeint_t and convert for arithmetic instead.")

namespace duckdb {

// ============================================================================
// Bind data for spark_sum and spark_avg (stores the input scale for finalize)
// ============================================================================

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

// ============================================================================
// Helper: Convert __int128 result to the target DECIMAL physical type
// ============================================================================

template <typename T>
static inline void WriteAggResult(T &target, __int128 val) {
	target = static_cast<T>(val);
}

template <>
inline void WriteAggResult<hugeint_t>(hugeint_t &target, __int128 val) {
	target = Int128ToHugeint(val);
}

// ============================================================================
// spark_sum: DECIMAL path
//
// Accumulates into a hugeint_t state, converting to __int128 only for
// stack-local arithmetic (see the alignment invariant at the top of this file).
// Input is promoted to DECIMAL(38, s) by DuckDB's implicit cast.
// Returns DECIMAL(min(p+10, 38), s) per Spark rules.
// ============================================================================

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
SPARK_ASSERT_STATE_ALIGNMENT(SparkSumDecimalState);

// Templatized operation so Finalize can target different physical types
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

// Helper: create a SparkSumDecimal AggregateFunction for a specific result physical type
template <typename RESULT_TYPE>
static AggregateFunction GetSparkSumDecimalFunction() {
	return AggregateFunction::UnaryAggregate<SparkSumDecimalState, hugeint_t, RESULT_TYPE,
	                                         SparkSumDecimalOperation<RESULT_TYPE>>(LogicalType::DECIMAL(38, 0),
	                                                                                LogicalType::DECIMAL(38, 0));
}

// Helper: look up the SparkSumDecimal function for a given physical type
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

	// Promote input to DECIMAL(38, s) -> hugeint_t physical type
	function.arguments[0] = LogicalType::DECIMAL(38, s);
	auto result_type = LogicalType::DECIMAL(result.precision, result.scale);
	function.return_type = result_type;

	// Select the correct function implementation based on result physical type
	{
		auto tf = GetSparkSumByPhysicalType(result_type.InternalType());
		function.update = tf.update;
		function.combine = tf.combine;
		function.finalize = tf.finalize;
		function.simple_update = tf.simple_update;
	}

	return make_uniq<SparkAggBindData>(s, result.scale);
}

// ============================================================================
// spark_sum: Integer path
//
// Spark: SUM(int/long/short/byte) -> BIGINT
// Accumulates into int64_t, returns BIGINT.
// ============================================================================

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
SPARK_ASSERT_STATE_ALIGNMENT(SparkSumIntegerState);

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

// ============================================================================
// spark_avg: DECIMAL path
//
// Accumulates sum (hugeint_t state, converted to __int128 only for stack-local
// arithmetic — see the alignment invariant at the top of this file) and count.
// At finalize, divides sum/count using SparkDecimalDivide with ROUND_HALF_UP.
// Returns DECIMAL(min(p+4, 38), min(s+4, 18)) per Spark rules.
// ============================================================================

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
SPARK_ASSERT_STATE_ALIGNMENT(SparkAvgDecimalState);

// Templatized so Finalize can target different physical result types
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

// Helper: create a SparkAvgDecimal AggregateFunction for a specific result physical type
template <typename RESULT_TYPE>
static AggregateFunction GetSparkAvgDecimalFunction() {
	return AggregateFunction::UnaryAggregate<SparkAvgDecimalState, hugeint_t, RESULT_TYPE,
	                                         SparkAvgDecimalOperation<RESULT_TYPE>>(LogicalType::DECIMAL(38, 0),
	                                                                                LogicalType::DECIMAL(38, 0));
}

// Helper: look up the SparkAvgDecimal function for a given physical type
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

	// Promote input to DECIMAL(38, s) -> hugeint_t physical type
	function.arguments[0] = LogicalType::DECIMAL(38, s);
	auto result_type = LogicalType::DECIMAL(result.precision, result.scale);
	function.return_type = result_type;

	// Select the correct function implementation based on result physical type
	{
		auto tf = GetSparkAvgByPhysicalType(result_type.InternalType());
		function.update = tf.update;
		function.combine = tf.combine;
		function.finalize = tf.finalize;
		function.simple_update = tf.simple_update;
	}

	return make_uniq<SparkAggBindData>(s, result.scale);
}

// spark_count is NOT needed as a separate extension function.
// DuckDB's built-in COUNT already returns BIGINT, matching Spark semantics.

// ============================================================================
// Factory functions to create the AggregateFunctionSets
// ============================================================================

inline AggregateFunctionSet CreateSparkSumFunctionSet() {
	AggregateFunctionSet set("spark_sum");

	// DECIMAL overload: input DECIMAL -> result DECIMAL(min(p+10,38), s)
	// Initial template uses hugeint_t; bind function swaps to correct physical type
	auto decimal_func = GetSparkSumDecimalFunction<hugeint_t>();
	decimal_func.bind = BindSparkSumDecimal;
	decimal_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(decimal_func);

	// Integer overloads: all return BIGINT (Spark semantics)
	// TINYINT
	auto tinyint_func =
	    AggregateFunction::UnaryAggregate<SparkSumIntegerState, int8_t, int64_t, SparkSumIntegerOperation>(
	        LogicalType::TINYINT, LogicalType::BIGINT);
	tinyint_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(tinyint_func);

	// SMALLINT
	auto smallint_func =
	    AggregateFunction::UnaryAggregate<SparkSumIntegerState, int16_t, int64_t, SparkSumIntegerOperation>(
	        LogicalType::SMALLINT, LogicalType::BIGINT);
	smallint_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(smallint_func);

	// INTEGER
	auto int_func = AggregateFunction::UnaryAggregate<SparkSumIntegerState, int32_t, int64_t, SparkSumIntegerOperation>(
	    LogicalType::INTEGER, LogicalType::BIGINT);
	int_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(int_func);

	// BIGINT
	auto bigint_func =
	    AggregateFunction::UnaryAggregate<SparkSumIntegerState, int64_t, int64_t, SparkSumIntegerOperation>(
	        LogicalType::BIGINT, LogicalType::BIGINT);
	bigint_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(bigint_func);

	return set;
}

inline AggregateFunctionSet CreateSparkAvgFunctionSet() {
	AggregateFunctionSet set("spark_avg");

	// DECIMAL overload: input DECIMAL -> result DECIMAL(min(p+4,38), min(s+4,18))
	// Initial template uses hugeint_t; bind function swaps to correct physical type
	auto decimal_func = GetSparkAvgDecimalFunction<hugeint_t>();
	decimal_func.bind = BindSparkAvgDecimal;
	decimal_func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	set.AddFunction(decimal_func);

	return set;
}

// No CreateSparkCountFunctionSet — DuckDB COUNT already matches Spark.

// ============================================================================
// spark_skewness: Population skewness (matches Spark's skewness())
//
// Pebay's numerically stable online algorithm (Sandia 2008) with state
// (n, mean, m2, m3), where m2/m3 are central moment sums.
//
// Formula: spark_skewness = sqrt(n) * m3 / sqrt((m2*m2)*m2)
//
// This replaced an earlier form that accumulated sum/sum_sqr/sum_cub and
// reconstructed the moments at finalize; that suffered catastrophic cancellation
// (e.g. on [1.0, 1.0, 1.0000000000000002] it returned -164382474.0 where Spark
// returns 0.7071067811865475).
//
// -- Why NOT Spark's literal expression tree ---------------------------------
// Spark 4.1.1's own CentralMomentAgg update is
//     m2 = m2 + delta * (delta - deltaN)
//     m3 = (m3 - 3*deltaN*m2_NEW) + delta * (delta*delta - deltaN*deltaN)
// and transcribing it verbatim DOES reproduce Spark bit-for-bit — but only when
// floating-point contraction is disabled. `delta*delta - deltaN*deltaN` must
// cancel to exactly 0 on the first row (where deltaN == delta); with GCC's
// default -ffp-contract=fast the compiler fuses it into an FMA that computes
// delta*delta without intermediate rounding, so the cancellation leaves a large
// residue. Measured on this TU, x = [1e12 .. 1e12+9, 1e12+20]:
//     -ffp-contract=off  -> 1.5383370916855739  (== Spark 4.1.1, bit-exact)
//     -ffp-contract=fast -> 1.0660149014611716e+16  (m3 = 1.68e19, garbage)
// Pinning contraction off would mean build-flag changes this directory's
// CLAUDE.md forbids for feature work, and per-compiler pragmas across the 4
// shipped platforms (gcc on linux, clang on macOS); `#pragma STDC FP_CONTRACT`
// is ignored by gcc in C++ mode. The Pebay form below has no such
// exact-cancellation dependency — its result is unchanged under
// -ffp-contract=off/on/fast — at the cost of ~12 ULP vs Spark on a
// single-partition run. That trade is deliberate: DO NOT "fix" the ULP gap by
// pasting in Spark's expression.
//
// Note also that "bit-exact vs Spark" is only well defined for a single
// partition: Spark's own answer for the 15-value vector in test/sql/skewness.test
// is 0.017475922407012685 under --master local[1], 0.017475922407012644 under
// local[*], and 0.017475922407012155 under REPARTITION(5) — a ~150 ULP spread,
// an order of magnitude wider than the gap being discussed here.
//
// Edge cases (verified against Spark 4.1.1, ANSI mode):
//   - n == 0:       NULL
//   - n == 1:       NULL for a finite value (m2 is exactly 0.0, which Spark also
//                   maps to NULL). KNOWN DIVERGENCE for a single value that is
//                   non-finite or large enough to overflow delta*delta
//                   (|x| >~ 1.34e154): Spark returns NaN there, because its merge
//                   into the zero-initialized buffer multiplies inf by a zero
//                   count; the `target.n == 0` short-circuit in Combine below
//                   deliberately suppresses that, so we return NULL.
//                   Measured: skewness(Infinity) / skewness(NaN) / skewness(1e308)
//                   over one row = NaN in Spark, NULL here.
//   - m2 == 0:      NULL   (zero variance / all values equal; NOT a
//                           DIVIDE_BY_ZERO error even under ANSI mode)
//   - m2 overflow:  Spark's SQRT((m2*m2)*m2) goes +inf, so the result is 0.0
//                   (e.g. skewness(0, 1e60, 2e60, 3e60, 1e61) = 0.0)
//   - non-finite:   propagated as-is (NaN/inf). Spark has NO finiteness guard
//                   here; it returns NaN — e.g. skewness(0, 1e110, 2e110,
//                   3e110, 1e111) = NaN. DuckDB's built-in skewness() throws
//                   "SKEW is out of range!" instead; matching DuckDB there would
//                   be a parity violation, so we do not.
// ============================================================================

struct SparkSkewState {
	uint64_t n;
	double mean;
	double m2;
	double m3;
};
SPARK_ASSERT_STATE_ALIGNMENT(SparkSkewState);

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

	// Pebay online update: add a single value x.
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

	// Pebay parallel merge: combine two partial aggregates.
	template <class STATE, class OP>
	static void Combine(const STATE &source, STATE &target, AggregateInputData &) {
		// Identity fast paths. DuckDB merges far more often than Spark does (per
		// thread, per partition, and the ungrouped path always merges into a
		// zero-initialized global state), so short-circuiting an empty side keeps
		// the result independent of merge topology. Spark instead relies on
		// multiply-by-zero, which would degrade to inf * 0 = NaN for inputs large
		// enough to overflow delta^3 below.
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

	// Spark: IF(n = 0, NULL, IF(m2 = 0, NULL, (SQRT(n) * m3) / SQRT((m2*m2)*m2)))
	template <class TARGET_TYPE, class STATE>
	static void Finalize(STATE &state, TARGET_TYPE &target, AggregateFinalizeData &finalize_data) {
		// n < 2 rather than Spark's n == 0. For a finite single value these agree
		// (delta_n == delta, so m2 is exactly 0.0, and Spark's own m2 == 0 branch
		// maps that to NULL). They DIVERGE for a single non-finite or
		// magnitude-overflowing value — see the edge-case list above.
		if (state.n < 2 || state.m2 == 0.0) {
			finalize_data.ReturnNull();
			return;
		}
		double n = static_cast<double>(state.n);
		// NOT pow(m2, 1.5): Spark uses sqrt((m2*m2)*m2), which overflows to +inf
		// (=> result 0.0) where pow stays finite, and unlike pow is IEEE-754 exact
		// and therefore identical across the platforms this extension ships for.
		target = (std::sqrt(n) * state.m3) / std::sqrt((state.m2 * state.m2) * state.m2);
		// No finiteness guard on purpose — Spark propagates NaN here. See the
		// header comment above.
	}

	static bool IgnoreNull() {
		return true;
	}
};

inline AggregateFunction CreateSparkSkewnessFunction() {
	auto func = AggregateFunction::UnaryAggregate<SparkSkewState, double, double, SparkSkewnessOperation>(
	    LogicalType::DOUBLE, LogicalType::DOUBLE);
	func.name = "spark_skewness";
	func.order_dependent = AggregateOrderDependent::NOT_ORDER_DEPENDENT;
	return func;
}

} // namespace duckdb
