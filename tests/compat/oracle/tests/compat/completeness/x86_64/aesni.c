// AES-NI (task #342): AESENC/AESENCLAST/AESDEC/AESDECLAST/AESIMC/AESKEYGENASSIST over many vectors incl.
// all-zero / all-one / MSB edges, PLUS a FIPS-197 AES-128 known-answer + encrypt->decrypt round-trip.
// Differential: the full stdout is byte-compared jit-vs-qemu; the KAT line self-asserts the answer.
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

#define KEXP(k, rc) aes128_step(k, _mm_aeskeygenassist_si128(k, rc))
__attribute__((target("aes,sse4.1"))) static __m128i aes128_step(__m128i key, __m128i gen) {
    gen = _mm_shuffle_epi32(gen, 0xff);
    key = _mm_xor_si128(key, _mm_slli_si128(key, 4));
    key = _mm_xor_si128(key, _mm_slli_si128(key, 4));
    key = _mm_xor_si128(key, _mm_slli_si128(key, 4));
    return _mm_xor_si128(key, gen);
}

__attribute__((target("aes,sse4.1"))) static long go(void) {
    static const uint64_t v[][2] = {
        {0, 0}, {~0ULL, ~0ULL}, {0x0001020304050607ULL, 0x08090a0b0c0d0e0fULL},
        {0x1122334455667788ULL, 0x99aabbccddeeff00ULL}, {0x8000000000000000ULL, 0x0000000000000001ULL},
        {0xdeadbeefcafebabeULL, 0x0123456789abcdefULL}, {0x5a5a5a5a5a5a5a5aULL, 0xa5a5a5a5a5a5a5a5ULL},
        {0xfedcba9876543210ULL, 0xf0e0d0c0b0a09080ULL},
    };
    int n = sizeof(v) / sizeof(v[0]);
    for (int i = 0; i < n; i++) {
        __m128i s = _mm_set_epi64x(v[i][1], v[i][0]);
        for (int j = 0; j < n; j++) {
            __m128i k = _mm_set_epi64x(v[j][1], v[j][0]);
            ph("aesenc", _mm_aesenc_si128(s, k));
            ph("aesenclast", _mm_aesenclast_si128(s, k));
            ph("aesdec", _mm_aesdec_si128(s, k));
            ph("aesdeclast", _mm_aesdeclast_si128(s, k));
        }
        ph("aesimc", _mm_aesimc_si128(s));
        ph("keygen1", _mm_aeskeygenassist_si128(s, 0x01));
        ph("keygen8b", _mm_aeskeygenassist_si128(s, 0x8b));
    }
    // FIPS-197 AES-128 KAT: key=000102..0f, pt=00112233..ff -> ct=69c4e0d8 6a7b0430 d8cdb780 70b4c55a.
    __m128i rk[11];
    rk[0] = _mm_set_epi8(15,14,13,12,11,10,9,8,7,6,5,4,3,2,1,0);
    rk[1]=KEXP(rk[0],0x01); rk[2]=KEXP(rk[1],0x02); rk[3]=KEXP(rk[2],0x04); rk[4]=KEXP(rk[3],0x08);
    rk[5]=KEXP(rk[4],0x10); rk[6]=KEXP(rk[5],0x20); rk[7]=KEXP(rk[6],0x40); rk[8]=KEXP(rk[7],0x80);
    rk[9]=KEXP(rk[8],0x1b); rk[10]=KEXP(rk[9],0x36);
    __m128i pt = _mm_set_epi8(0xff,0xee,0xdd,0xcc,0xbb,0xaa,0x99,0x88,0x77,0x66,0x55,0x44,0x33,0x22,0x11,0x00);
    __m128i c = _mm_xor_si128(pt, rk[0]);
    for (int r = 1; r < 10; r++) c = _mm_aesenc_si128(c, rk[r]);
    c = _mm_aesenclast_si128(c, rk[10]);
    ph("fips197ct", c);
    // decrypt: derive decrypt round keys via AESIMC, run AESDEC.
    __m128i dk[11]; dk[0] = rk[10]; dk[10] = rk[0];
    for (int r = 1; r < 10; r++) dk[r] = _mm_aesimc_si128(rk[10 - r]);
    __m128i p = _mm_xor_si128(c, dk[0]);
    for (int r = 1; r < 10; r++) p = _mm_aesdec_si128(p, dk[r]);
    p = _mm_aesdeclast_si128(p, dk[10]);
    uint8_t pb[16], eb[16]; _mm_storeu_si128((__m128i *)pb, p); _mm_storeu_si128((__m128i *)eb, pt);
    int roundtrip = memcmp(pb, eb, 16) == 0;
    static const uint8_t kat[16] = {0x69,0xc4,0xe0,0xd8,0x6a,0x7b,0x04,0x30,0xd8,0xcd,0xb7,0x80,0x70,0xb4,0xc5,0x5a};
    uint8_t cb[16]; _mm_storeu_si128((__m128i *)cb, c);
    printf("fips197 kat=%d roundtrip=%d\n", memcmp(cb, kat, 16) == 0, roundtrip);
    long r = 0; r += _mm_extract_epi32(c, 0); r += _mm_extract_epi32(p, 1); return r;
}
int main(void) { printf("aesni r=%ld\n", go()); return 0; }
