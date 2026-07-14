#pragma once

#include "wide_integer.hpp"

namespace duckdb {

// Perform decimal division with ROUND_HALF_UP rounding (Spark semantics).
//
// Given two scaled integers a and b (representing DECIMAL values),
// compute: result = (a * pow10_val) / b, rounded HALF_UP.
//
// pow10_val must be precomputed as Pow10_128(scale_adj) by the caller.
// When scale_adj == 0, pass pow10_val = 0 to skip scaling entirely.
//
// Returns the result as a signed __int128.
// Caller must handle division by zero before calling this function.
inline __int128 SparkDecimalDivide(__int128 a, __int128 b, unsigned __int128 pow10_val) {
	// Handle signs separately, work with absolute values
	bool negative = (a < 0) != (b < 0);
	unsigned __int128 abs_a = Abs128(a);
	unsigned __int128 abs_b = Abs128(b);

	unsigned __int128 quotient;
	unsigned __int128 remainder;

	if (pow10_val == 0) {
		// No scaling needed (scale_adj was 0)
		quotient = abs_a / abs_b;
		remainder = abs_a % abs_b;
	} else {
		// Check if abs_a * pow10_val would overflow unsigned __int128
		// __builtin_mul_overflow compiles to a single mul instruction + flag check,
		// avoiding the expensive division (UINT128_MAX / abs_a) of the naive approach.
		unsigned __int128 scaled;
		bool overflow = __builtin_mul_overflow(abs_a, pow10_val, &scaled);

		if (__builtin_expect(!overflow, 1)) {
			// Fast path: fits in 128 bits
			quotient = scaled / abs_b;
			remainder = scaled % abs_b;
		} else {
			// Slow path: use 256-bit intermediate
			uint256_t scaled_wide = Mul128(abs_a, pow10_val);
			quotient = Div256By128(scaled_wide, abs_b, &remainder);
		}
	}

	// ROUND_HALF_UP: round away from zero when remainder >= half of divisor.
	// Branchless: add 1 if (2 * remainder >= abs_b), 0 otherwise.
	// Note: 2 * remainder cannot overflow unsigned __int128 because
	// remainder < abs_b <= 10^38, and 2 * 10^38 < 2^128.
	quotient += static_cast<unsigned __int128>(remainder * 2 >= abs_b);

	// Apply sign branchlessly using two's complement arithmetic:
	// If negative: result = ~quotient + 1 = -(quotient)
	// If positive: result = quotient
	unsigned __int128 sign_mask = -static_cast<unsigned __int128>(negative);
	unsigned __int128 result_unsigned = (quotient ^ sign_mask) + (sign_mask & 1);
	return static_cast<__int128>(result_unsigned);
}

} // namespace duckdb
