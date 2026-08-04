#pragma once

#include "duckdb.hpp"
#include <cstdint>

namespace duckdb {

// ---------------------------------------------------------------------------
// hugeint_t <-> __int128 conversion
// ---------------------------------------------------------------------------

inline __int128 HugeintToInt128(const hugeint_t &h) {
	unsigned __int128 result = (static_cast<unsigned __int128>(static_cast<uint64_t>(h.upper)) << 64) | h.lower;
	return static_cast<__int128>(result);
}

inline hugeint_t Int128ToHugeint(__int128 v) {
	hugeint_t result;
	result.upper = static_cast<int64_t>(v >> 64);
	result.lower = static_cast<uint64_t>(v);
	return result;
}

// ---------------------------------------------------------------------------
// Absolute value for signed __int128
// ---------------------------------------------------------------------------

inline unsigned __int128 Abs128(__int128 x) {
	return x < 0 ? -static_cast<unsigned __int128>(x) : static_cast<unsigned __int128>(x);
}

// ---------------------------------------------------------------------------
// 256-bit unsigned integer (two 128-bit halves)
// ---------------------------------------------------------------------------

struct uint256_t {
	unsigned __int128 hi;
	unsigned __int128 lo;
};

// Multiply two unsigned 128-bit values, producing a 256-bit result.
// Uses schoolbook multiplication with 64-bit limbs.
inline uint256_t Mul128(unsigned __int128 a, unsigned __int128 b) {
	uint64_t a_lo = static_cast<uint64_t>(a);
	uint64_t a_hi = static_cast<uint64_t>(a >> 64);
	uint64_t b_lo = static_cast<uint64_t>(b);
	uint64_t b_hi = static_cast<uint64_t>(b >> 64);

	// Four partial products (each fits in unsigned __int128)
	unsigned __int128 p0 = static_cast<unsigned __int128>(a_lo) * b_lo;
	unsigned __int128 p1 = static_cast<unsigned __int128>(a_lo) * b_hi;
	unsigned __int128 p2 = static_cast<unsigned __int128>(a_hi) * b_lo;
	unsigned __int128 p3 = static_cast<unsigned __int128>(a_hi) * b_hi;

	// Accumulate middle terms
	unsigned __int128 mid = p1 + p2;
	unsigned __int128 mid_carry = (mid < p1) ? (static_cast<unsigned __int128>(1) << 64) : 0;

	// Low 128 bits
	unsigned __int128 lo = p0 + (mid << 64);
	unsigned __int128 lo_carry = (lo < p0) ? 1 : 0;

	// High 128 bits
	unsigned __int128 hi = p3 + (mid >> 64) + mid_carry + lo_carry;

	return {hi, lo};
}

// Divide a 256-bit unsigned value by a 128-bit unsigned divisor.
// Returns quotient (must fit in 128 bits) and sets *remainder.
//
// Uses Knuth's Algorithm D with 64-bit "digits" for the main path,
// replacing the 128-iteration bit-by-bit binary long division.
// Special fast paths for num.hi == 0 and den < 2^64.
inline unsigned __int128 Div256By128(uint256_t num, unsigned __int128 den, unsigned __int128 *remainder) {
	// Fast path: high part is zero -> simple 128-bit division
	if (num.hi == 0) {
		unsigned __int128 quot = num.lo / den;
		if (remainder) {
			*remainder = num.lo % den;
		}
		return quot;
	}

	D_ASSERT(num.hi < den); // quotient must fit in 128 bits

	// Fast path: divisor fits in 64 bits -> 3-digit by 1-digit division
	if (static_cast<uint64_t>(den >> 64) == 0) {
		uint64_t d = static_cast<uint64_t>(den);
		// num.hi < den < 2^64, so num.hi fits in 64 bits
		uint64_t hi_lo = static_cast<uint64_t>(num.hi);
		uint64_t lo_hi = static_cast<uint64_t>(num.lo >> 64);
		uint64_t lo_lo = static_cast<uint64_t>(num.lo);

		// First digit: [hi_lo, lo_hi] / d
		unsigned __int128 tmp = (static_cast<unsigned __int128>(hi_lo) << 64) | lo_hi;
		uint64_t q1 = static_cast<uint64_t>(tmp / d);
		uint64_t r1 = static_cast<uint64_t>(tmp % d);

		// Second digit: [r1, lo_lo] / d
		tmp = (static_cast<unsigned __int128>(r1) << 64) | lo_lo;
		uint64_t q0 = static_cast<uint64_t>(tmp / d);
		uint64_t r0 = static_cast<uint64_t>(tmp % d);

		if (remainder) {
			*remainder = r0;
		}
		return (static_cast<unsigned __int128>(q1) << 64) | q0;
	}

	// Knuth's Algorithm D: normalize so top bit of divisor is set
	uint64_t den_hi = static_cast<uint64_t>(den >> 64);
	int shift = __builtin_clzll(den_hi);

	// Normalize divisor
	unsigned __int128 den_norm = den << shift;
	uint64_t d1 = static_cast<uint64_t>(den_norm >> 64);
	uint64_t d0 = static_cast<uint64_t>(den_norm);

	// Normalize numerator (256-bit left shift by 'shift')
	unsigned __int128 n_hi, n_lo;
	if (shift > 0) {
		n_hi = (num.hi << shift) | (num.lo >> (128 - shift));
		n_lo = num.lo << shift;
	} else {
		n_hi = num.hi;
		n_lo = num.lo;
	}

	// Split into 64-bit limbs: n = [n3, n2, n1, n0]
	uint64_t n3 = static_cast<uint64_t>(n_hi >> 64);
	uint64_t n2 = static_cast<uint64_t>(n_hi);
	uint64_t n1 = static_cast<uint64_t>(n_lo >> 64);
	uint64_t n0 = static_cast<uint64_t>(n_lo);

	// First quotient digit: q1 = floor([n3, n2, n1] / [d1, d0])
	// Estimate: qhat = floor([n3, n2] / d1)
	unsigned __int128 tmp = (static_cast<unsigned __int128>(n3) << 64) | n2;
	uint64_t qhat = static_cast<uint64_t>(tmp / d1);
	uint64_t rhat = static_cast<uint64_t>(tmp % d1);

	// Refine: while qhat * d0 > [rhat, n1]
	while (static_cast<unsigned __int128>(qhat) * d0 > ((static_cast<unsigned __int128>(rhat) << 64) | n1)) {
		qhat--;
		rhat += d1;
		if (rhat < d1) {
			break; // overflow means rhat >= 2^64, condition is false
		}
	}

	// Compute partial remainder: [n3,n2,n1] - qhat * [d1,d0]
	unsigned __int128 hi_part = tmp - static_cast<unsigned __int128>(qhat) * d1;
	unsigned __int128 rem_hi_lo = (hi_part << 64) | n1;
	unsigned __int128 sub = static_cast<unsigned __int128>(qhat) * d0;

	// Check for borrow and correct
	if (rem_hi_lo < sub) {
		qhat--;
		rem_hi_lo += den_norm;
	}
	unsigned __int128 rem1 = rem_hi_lo - sub;

	uint64_t q1 = qhat;

	// Second quotient digit: q0 = floor([rem1, n0] / [d1, d0])
	uint64_t rem1_hi = static_cast<uint64_t>(rem1 >> 64);
	uint64_t rem1_lo = static_cast<uint64_t>(rem1);

	tmp = (static_cast<unsigned __int128>(rem1_hi) << 64) | rem1_lo;
	qhat = static_cast<uint64_t>(tmp / d1);
	rhat = static_cast<uint64_t>(tmp % d1);

	while (static_cast<unsigned __int128>(qhat) * d0 > ((static_cast<unsigned __int128>(rhat) << 64) | n0)) {
		qhat--;
		rhat += d1;
		if (rhat < d1) {
			break;
		}
	}

	hi_part = tmp - static_cast<unsigned __int128>(qhat) * d1;
	rem_hi_lo = (hi_part << 64) | n0;
	sub = static_cast<unsigned __int128>(qhat) * d0;

	if (rem_hi_lo < sub) {
		qhat--;
		rem_hi_lo += den_norm;
	}
	unsigned __int128 rem_final = rem_hi_lo - sub;

	uint64_t q0 = qhat;

	// Un-normalize remainder
	if (remainder) {
		*remainder = rem_final >> shift;
	}

	return (static_cast<unsigned __int128>(q1) << 64) | q0;
}

// ---------------------------------------------------------------------------
// Power-of-10 lookup for unsigned __int128 (up to 10^38)
// ---------------------------------------------------------------------------

// Helper to construct unsigned __int128 from high and low 64-bit halves.
inline constexpr unsigned __int128 MakeUint128(uint64_t hi, uint64_t lo) {
	return (static_cast<unsigned __int128>(hi) << 64) | lo;
}

// O(1) lookup table covering 10^0 through 10^38.
// D_ASSERT guards against out-of-range exponents.
inline unsigned __int128 Pow10_128(uint32_t exp) {
	// clang-format off
	static constexpr unsigned __int128 table[] = {
	    // 10^0 - 10^19: fit in uint64_t
	    MakeUint128(0ULL,                    1ULL),                     // 10^0
	    MakeUint128(0ULL,                    10ULL),                    // 10^1
	    MakeUint128(0ULL,                    100ULL),                   // 10^2
	    MakeUint128(0ULL,                    1000ULL),                  // 10^3
	    MakeUint128(0ULL,                    10000ULL),                 // 10^4
	    MakeUint128(0ULL,                    100000ULL),                // 10^5
	    MakeUint128(0ULL,                    1000000ULL),               // 10^6
	    MakeUint128(0ULL,                    10000000ULL),              // 10^7
	    MakeUint128(0ULL,                    100000000ULL),             // 10^8
	    MakeUint128(0ULL,                    1000000000ULL),            // 10^9
	    MakeUint128(0ULL,                    10000000000ULL),           // 10^10
	    MakeUint128(0ULL,                    100000000000ULL),          // 10^11
	    MakeUint128(0ULL,                    1000000000000ULL),         // 10^12
	    MakeUint128(0ULL,                    10000000000000ULL),        // 10^13
	    MakeUint128(0ULL,                    100000000000000ULL),       // 10^14
	    MakeUint128(0ULL,                    1000000000000000ULL),      // 10^15
	    MakeUint128(0ULL,                    10000000000000000ULL),     // 10^16
	    MakeUint128(0ULL,                    100000000000000000ULL),    // 10^17
	    MakeUint128(0ULL,                    1000000000000000000ULL),   // 10^18
	    MakeUint128(0ULL,                    10000000000000000000ULL),  // 10^19
	    // 10^20 - 10^38: require both halves
	    MakeUint128(5ULL,                    7766279631452241920ULL),   // 10^20
	    MakeUint128(54ULL,                   3875820019684212736ULL),   // 10^21
	    MakeUint128(542ULL,                  1864712049423024128ULL),   // 10^22
	    MakeUint128(5421ULL,                 200376420520689664ULL),    // 10^23
	    MakeUint128(54210ULL,                2003764205206896640ULL),   // 10^24
	    MakeUint128(542101ULL,               1590897978359414784ULL),   // 10^25
	    MakeUint128(5421010ULL,              15908979783594147840ULL),  // 10^26
	    MakeUint128(54210108ULL,             11515845246265065472ULL),  // 10^27
	    MakeUint128(542101086ULL,            4477988020393345024ULL),   // 10^28
	    MakeUint128(5421010862ULL,           7886392056514347008ULL),   // 10^29
	    MakeUint128(54210108624ULL,          5076944270305263616ULL),   // 10^30
	    MakeUint128(542101086242ULL,         13875954555633532928ULL),  // 10^31
	    MakeUint128(5421010862427ULL,        9632337040368467968ULL),   // 10^32
	    MakeUint128(54210108624275ULL,       4089650035136921600ULL),   // 10^33
	    MakeUint128(542101086242752ULL,      4003012203950112768ULL),   // 10^34
	    MakeUint128(5421010862427522ULL,     3136633892082024448ULL),   // 10^35
	    MakeUint128(54210108624275221ULL,    12919594847110692864ULL),  // 10^36
	    MakeUint128(542101086242752217ULL,   68739955140067328ULL),     // 10^37
	    MakeUint128(5421010862427522170ULL,  687399551400673280ULL),    // 10^38
	};
	// clang-format on

	D_ASSERT(exp <= 38);
	return table[exp];
}

} // namespace duckdb
