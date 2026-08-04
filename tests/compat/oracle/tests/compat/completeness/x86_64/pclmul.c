// PCLMULQDQ (task #342): carry-less 64x64->128 multiply over all 4 half-select immediates and many
// vectors incl. all-zero / all-one / MSB edges. Full stdout byte-compared jit-vs-qemu.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <wmmintrin.h>
#include <emmintrin.h>
#include <smmintrin.h>

static void ph(const char *t, __m128i v) {
    uint8_t b[16]; _mm_storeu_si128((__m128i *)b, v);
    printf("%s ", t); for (int i = 0; i < 16; i++) printf("%02x", b[i]); printf("\n");
}

__attribute__((target("pclmul,sse4.1"))) static long go(void) {
    static const uint64_t v[][2] = {
        {0, 0}, {~0ULL, ~0ULL}, {0x1122334455667788ULL, 0x99aabbccddeeff00ULL},
        {0x8000000000000000ULL, 0x0000000000000001ULL}, {0xdeadbeefcafebabeULL, 0x0123456789abcdefULL},
        {0xffULL, 0xff00000000000000ULL}, {0x0f0f0f0f0f0f0f0fULL, 0xf0f0f0f0f0f0f0f0ULL},
    };
    int n = sizeof(v) / sizeof(v[0]);
    for (int i = 0; i < n; i++) {
        __m128i a = _mm_set_epi64x(v[i][1], v[i][0]);
        for (int j = 0; j < n; j++) {
            __m128i b = _mm_set_epi64x(v[j][1], v[j][0]);
            ph("clmul00", _mm_clmulepi64_si128(a, b, 0x00));
            ph("clmul01", _mm_clmulepi64_si128(a, b, 0x01));
            ph("clmul10", _mm_clmulepi64_si128(a, b, 0x10));
            ph("clmul11", _mm_clmulepi64_si128(a, b, 0x11));
        }
    }
    // A GHASH-style fold chain to exercise the op the way AES-GCM does.
    __m128i acc = _mm_setzero_si128(), H = _mm_set_epi64x(0x0388dace60b6a392ULL, 0x66e94bd4ef8a2c3bULL);
    for (int r = 0; r < 8; r++) {
        __m128i x = _mm_xor_si128(acc, _mm_set_epi64x((uint64_t)r * 0x9e3779b97f4a7c15ULL, ~(uint64_t)r));
        __m128i t0 = _mm_clmulepi64_si128(x, H, 0x00);
        __m128i t1 = _mm_clmulepi64_si128(x, H, 0x11);
        __m128i t2 = _mm_xor_si128(_mm_clmulepi64_si128(x, H, 0x10), _mm_clmulepi64_si128(x, H, 0x01));
        acc = _mm_xor_si128(_mm_xor_si128(t0, t1), t2);
    }
    ph("ghashfold", acc);
    return (long)(_mm_extract_epi64(acc, 0) ^ _mm_extract_epi64(acc, 1)) & 0xffffff;
}
int main(void) { printf("pclmul r=%ld\n", go()); return 0; }
