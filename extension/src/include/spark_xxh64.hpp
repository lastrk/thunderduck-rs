#pragma once

// Spark's XXH64 variant. Arithmetic uses uint64_t for Java's modulo-2^64
// behavior; little-endian reads use memcpy for portable unaligned access.

#include <cstddef>
#include <cstdint>
#include <cstring>

namespace duckdb {
namespace spark_hash {

constexpr uint64_t XXH64_P1 = 0x9E3779B185EBCA87ULL;
constexpr uint64_t XXH64_P2 = 0xC2B2AE3D27D4EB4FULL;
constexpr uint64_t XXH64_P3 = 0x165667B19E3779F9ULL;
constexpr uint64_t XXH64_P4 = 0x85EBCA77C2B2AE63ULL;
constexpr uint64_t XXH64_P5 = 0x27D4EB2F165667C5ULL;

static inline uint64_t XXH64Rotl(uint64_t x, int r) {
	return (x << r) | (x >> (64 - r));
}

static inline uint64_t XXH64Read64LE(const uint8_t *p) {
	uint64_t v;
	std::memcpy(&v, p, 8);
	return v;
}

static inline uint32_t XXH64Read32LE(const uint8_t *p) {
	uint32_t v;
	std::memcpy(&v, p, 4);
	return v;
}

static inline uint64_t XXH64Fmix(uint64_t h) {
	h ^= h >> 33;
	h *= XXH64_P2;
	h ^= h >> 29;
	h *= XXH64_P3;
	h ^= h >> 32;
	return h;
}

static inline uint64_t XXH64HashInt(int32_t input, uint64_t seed) {
	uint64_t h = seed + XXH64_P5 + 4;
	h ^= static_cast<uint64_t>(static_cast<uint32_t>(input)) * XXH64_P1;
	h = XXH64Rotl(h, 23) * XXH64_P2 + XXH64_P3;
	return XXH64Fmix(h);
}

static inline uint64_t XXH64HashLong(int64_t input, uint64_t seed) {
	uint64_t h = seed + XXH64_P5 + 8;
	uint64_t k1 = XXH64Rotl(static_cast<uint64_t>(input) * XXH64_P2, 31) * XXH64_P1;
	h ^= k1;
	h = XXH64Rotl(h, 27) * XXH64_P1 + XXH64_P4;
	return XXH64Fmix(h);
}

inline uint64_t XXH64HashBytes(const uint8_t *p, size_t length, uint64_t seed) {
	const uint8_t *const end = p + length;
	uint64_t h;

	if (length >= 32) {
		const uint8_t *const limit = end - 32;
		uint64_t v1 = seed + XXH64_P1 + XXH64_P2;
		uint64_t v2 = seed + XXH64_P2;
		uint64_t v3 = seed;
		uint64_t v4 = seed - XXH64_P1;
		do {
			v1 += XXH64Read64LE(p) * XXH64_P2;
			v1 = XXH64Rotl(v1, 31);
			v1 *= XXH64_P1;
			p += 8;
			v2 += XXH64Read64LE(p) * XXH64_P2;
			v2 = XXH64Rotl(v2, 31);
			v2 *= XXH64_P1;
			p += 8;
			v3 += XXH64Read64LE(p) * XXH64_P2;
			v3 = XXH64Rotl(v3, 31);
			v3 *= XXH64_P1;
			p += 8;
			v4 += XXH64Read64LE(p) * XXH64_P2;
			v4 = XXH64Rotl(v4, 31);
			v4 *= XXH64_P1;
			p += 8;
		} while (p <= limit);

		h = XXH64Rotl(v1, 1) + XXH64Rotl(v2, 7) + XXH64Rotl(v3, 12) + XXH64Rotl(v4, 18);
		v1 *= XXH64_P2;
		v1 = XXH64Rotl(v1, 31);
		v1 *= XXH64_P1;
		h ^= v1;
		h = h * XXH64_P1 + XXH64_P4;
		v2 *= XXH64_P2;
		v2 = XXH64Rotl(v2, 31);
		v2 *= XXH64_P1;
		h ^= v2;
		h = h * XXH64_P1 + XXH64_P4;
		v3 *= XXH64_P2;
		v3 = XXH64Rotl(v3, 31);
		v3 *= XXH64_P1;
		h ^= v3;
		h = h * XXH64_P1 + XXH64_P4;
		v4 *= XXH64_P2;
		v4 = XXH64Rotl(v4, 31);
		v4 *= XXH64_P1;
		h ^= v4;
		h = h * XXH64_P1 + XXH64_P4;
	} else {
		h = seed + XXH64_P5;
	}

	h += static_cast<uint64_t>(length);

	while (p + 8 <= end) {
		uint64_t k1 = XXH64Read64LE(p) * XXH64_P2;
		k1 = XXH64Rotl(k1, 31);
		k1 *= XXH64_P1;
		h ^= k1;
		h = XXH64Rotl(h, 27) * XXH64_P1 + XXH64_P4;
		p += 8;
	}
	if (p + 4 <= end) {
		h ^= static_cast<uint64_t>(XXH64Read32LE(p)) * XXH64_P1;
		h = XXH64Rotl(h, 23) * XXH64_P2 + XXH64_P3;
		p += 4;
	}
	while (p < end) {
		h ^= static_cast<uint64_t>(*p) * XXH64_P5;
		h = XXH64Rotl(h, 11) * XXH64_P1;
		p++;
	}
	return XXH64Fmix(h);
}

} // namespace spark_hash
} // namespace duckdb
