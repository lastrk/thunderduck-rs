#pragma once

// Spark-bit-parity Murmur3_x86_32 primitives, mirroring
// org.apache.spark.unsafe.hash.Murmur3_x86_32. Two notes that are easy to
// miss when porting:
//
//  1. Spark's `hashUnsafeBytes` processes the tail (the bytes after the last
//     4-byte-aligned word) ONE BYTE AT A TIME, running each tail byte
//     through mixK1 + mixH1 individually. The canonical MurmurHash3 packs
//     the tail bytes into a single k1 and does one mixK1. The two
//     algorithms therefore diverge on any input whose length is not a
//     multiple of 4. We mirror Spark's variant exactly here.
//
//  2. Java's signed-int arithmetic wraps modulo 2^32 with the same bit
//     pattern as unsigned. We use uint32_t throughout, which is well-defined
//     and matches bit-for-bit.

#include <cstddef>
#include <cstdint>
#include <cstring>

namespace duckdb {
namespace spark_hash {

constexpr uint32_t MURMUR3_C1 = 0xCC9E2D51U;
constexpr uint32_t MURMUR3_C2 = 0x1B873593U;

static inline uint32_t Murmur3Rotl(uint32_t x, int r) {
	return (x << r) | (x >> (32 - r));
}

static inline uint32_t Murmur3Read32LE(const uint8_t *p) {
	uint32_t v;
	std::memcpy(&v, p, 4);
	return v;
}

static inline uint32_t Murmur3MixK1(uint32_t k1) {
	k1 *= MURMUR3_C1;
	k1 = Murmur3Rotl(k1, 15);
	k1 *= MURMUR3_C2;
	return k1;
}

static inline uint32_t Murmur3MixH1(uint32_t h1, uint32_t k1) {
	h1 ^= k1;
	h1 = Murmur3Rotl(h1, 13);
	h1 = h1 * 5 + 0xE6546B64U;
	return h1;
}

static inline uint32_t Murmur3Fmix(uint32_t h1, uint32_t length) {
	h1 ^= length;
	h1 ^= h1 >> 16;
	h1 *= 0x85EBCA6BU;
	h1 ^= h1 >> 13;
	h1 *= 0xC2B2AE35U;
	h1 ^= h1 >> 16;
	return h1;
}

static inline uint32_t Murmur3HashInt(int32_t input, uint32_t seed) {
	uint32_t k1 = Murmur3MixK1(static_cast<uint32_t>(input));
	uint32_t h1 = Murmur3MixH1(seed, k1);
	return Murmur3Fmix(h1, 4);
}

static inline uint32_t Murmur3HashLong(int64_t input, uint32_t seed) {
	uint64_t uinput = static_cast<uint64_t>(input);
	uint32_t low = static_cast<uint32_t>(uinput);
	uint32_t high = static_cast<uint32_t>(uinput >> 32);
	uint32_t h1 = Murmur3MixH1(seed, Murmur3MixK1(low));
	h1 = Murmur3MixH1(h1, Murmur3MixK1(high));
	return Murmur3Fmix(h1, 8);
}

inline uint32_t Murmur3HashBytes(const uint8_t *p, size_t length, uint32_t seed) {
	uint32_t h1 = seed;
	const size_t aligned_length = length & ~static_cast<size_t>(3); // floor to multiple of 4
	for (size_t i = 0; i < aligned_length; i += 4) {
		uint32_t half_word = Murmur3Read32LE(p + i);
		h1 = Murmur3MixH1(h1, Murmur3MixK1(half_word));
	}
	// Spark variant: each tail byte processed individually through mixK1 +
	// mixH1. This is NOT the canonical MurmurHash3 tail.
	for (size_t i = aligned_length; i < length; i++) {
		uint32_t half_word = static_cast<uint32_t>(p[i]);
		h1 = Murmur3MixH1(h1, Murmur3MixK1(half_word));
	}
	return Murmur3Fmix(h1, static_cast<uint32_t>(length));
}

} // namespace spark_hash
} // namespace duckdb
