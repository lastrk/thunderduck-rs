#pragma once

#include "wide_integer.hpp"

namespace duckdb {

// Divides scaled DECIMAL integers using Spark's ROUND_HALF_UP semantics.
//
// pow10_val must be precomputed as Pow10_128(scale_adj) by the caller.
// When scale_adj == 0, pass pow10_val = 0 to skip scaling entirely.
//
// Returns the result as a signed __int128.
// Caller must handle division by zero before calling this function.
inline __int128 SparkDecimalDivide(__int128 a, __int128 b, unsigned __int128 pow10_val) {
	// Handle signs separately so rounding is away from zero.
	bool negative = (a < 0) != (b < 0);
	unsigned __int128 abs_a = Abs128(a);
	unsigned __int128 abs_b = Abs128(b);

	unsigned __int128 quotient;
	unsigned __int128 remainder;

	if (pow10_val == 0) {
		quotient = abs_a / abs_b;
		remainder = abs_a % abs_b;
	} else {
		// DECIMAL(38) scaling can exceed 128 bits; preserve the exact product
		// and use the 256-bit divider when the fast-path multiplication overflows.
		unsigned __int128 scaled;
		bool overflow = __builtin_mul_overflow(abs_a, pow10_val, &scaled);

		if (__builtin_expect(!overflow, 1)) {
			quotient = scaled / abs_b;
			remainder = scaled % abs_b;
		} else {
			uint256_t scaled_wide = Mul128(abs_a, pow10_val);
			quotient = Div256By128(scaled_wide, abs_b, &remainder);
		}
	}

	// ROUND_HALF_UP: remainder*2 is safe because the divisor is bounded by 10^38.
	quotient += static_cast<unsigned __int128>(remainder * 2 >= abs_b);

	// Apply the sign without signed overflow.
	unsigned __int128 sign_mask = -static_cast<unsigned __int128>(negative);
	unsigned __int128 result_unsigned = (quotient ^ sign_mask) + (sign_mask & 1);
	return static_cast<__int128>(result_unsigned);
}

} // namespace duckdb
