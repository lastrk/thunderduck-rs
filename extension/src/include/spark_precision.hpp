#pragma once

#include "duckdb.hpp"
#include <algorithm>
#include <cstdint>

namespace duckdb {

static constexpr uint8_t SPARK_MAX_PRECISION = 38;
static constexpr uint8_t SPARK_MIN_ADJUSTED_SCALE = 6;

struct SparkDecimalResult {
	uint8_t precision;
	uint8_t scale;
};

// Spark 4.1 DECIMAL division:
//   scale = max(6, s1 + p2 + 1)
//   precision = (p1 - s1) + s2 + scale
// If precision exceeds 38, retain at least scale 6 while fitting the integer
// digits into precision 38. SUM adds 10 precision digits; AVG adds 4 and caps
// scale at 18 (and never above the resulting precision).
inline SparkDecimalResult ComputeDivisionType(uint8_t p1, uint8_t s1, uint8_t p2, uint8_t s2) {
	uint8_t result_scale = std::max(static_cast<uint8_t>(6), static_cast<uint8_t>(s1 + p2 + 1));
	uint8_t result_precision = (p1 - s1) + s2 + result_scale;

	if (result_precision > SPARK_MAX_PRECISION) {
		uint8_t int_digits = result_precision - result_scale;
		uint8_t min_scale = std::min(result_scale, SPARK_MIN_ADJUSTED_SCALE);
		if (SPARK_MAX_PRECISION > int_digits) {
			result_scale = std::max(static_cast<uint8_t>(SPARK_MAX_PRECISION - int_digits), min_scale);
		} else {
			result_scale = min_scale;
		}
		result_precision = SPARK_MAX_PRECISION;
	}

	return {result_precision, result_scale};
}

inline SparkDecimalResult ComputeSumType(uint8_t p, uint8_t s) {
	uint8_t result_precision = std::min(static_cast<uint8_t>(p + 10), SPARK_MAX_PRECISION);
	return {result_precision, s};
}

inline SparkDecimalResult ComputeAvgType(uint8_t p, uint8_t s) {
	uint8_t result_precision = std::min(static_cast<uint8_t>(p + 4), SPARK_MAX_PRECISION);
	uint8_t result_scale = std::min(static_cast<uint8_t>(s + 4), static_cast<uint8_t>(18));
	result_scale = std::min(result_scale, result_precision);
	return {result_precision, result_scale};
}

// Bind data storing precomputed division parameters.
struct SparkDivBindData : public FunctionData {
	uint32_t scale_adj; // result scale - numerator scale + denominator scale

	explicit SparkDivBindData(uint32_t scale_adj_p) : scale_adj(scale_adj_p) {
	}

	unique_ptr<FunctionData> Copy() const override {
		return make_uniq<SparkDivBindData>(scale_adj);
	}

	bool Equals(const FunctionData &other_p) const override {
		auto &other = other_p.Cast<SparkDivBindData>();
		return scale_adj == other.scale_adj;
	}
};

} // namespace duckdb
