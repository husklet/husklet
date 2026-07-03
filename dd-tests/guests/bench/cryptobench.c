// Crypto throughput microbench for the x86->ARM64 JIT: exercises the exact hot instruction mix of
// openssl AES-128-GCM (AES-NI CTR + PCLMULQDQ GHASH, both wrapped in PSHUFB byte-swaps) and SHA-256-NI
// (SHA256RNDS2/MSG1/MSG2 wrapped in PSHUFB/PALIGNR/PSHUFD). Self-times with CLOCK_MONOTONIC and prints
// MB/s so the same binary can be run under the JIT with NOSSEOPT=1 (shuffle glue exits to the C
// softmulator = "before") vs unset (shuffle glue lowered inline = "after"). Timing only; not a KAT.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <time.h>
#include <immintrin.h>
#include <x86intrin.h>

#define BUFSZ (16 * 1024)
#define ITERS 24000

static double now_s(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec + (double)t.tv_nsec * 1e-9;
}

// ---- AES-128-GCM proxy: CTR keystream (10 rounds) XOR data, then a GHASH fold. Keys are dummy (timing
// only). Per 16-byte block: pshufb(counter bswap) + 10 aesenc/last + pshufb(ghash) + 4 pclmulqdq. ----
__attribute__((target("aes,pclmul,ssse3,sse4.1")))
static __m128i aesgcm_pass(uint8_t *buf, const __m128i rk[11], __m128i H, __m128i acc) {
    const __m128i bswap = _mm_set_epi64x(0x0001020304050607ULL, 0x08090a0b0c0d0e0fULL);
    __m128i ctr = _mm_set_epi64x(0, 1);
    for (int off = 0; off < BUFSZ; off += 16) {
        __m128i cb = _mm_shuffle_epi8(ctr, bswap);      // counter byte-swap (pshufb)
        __m128i ks = _mm_xor_si128(cb, rk[0]);
        ks = _mm_aesenc_si128(ks, rk[1]);  ks = _mm_aesenc_si128(ks, rk[2]);
        ks = _mm_aesenc_si128(ks, rk[3]);  ks = _mm_aesenc_si128(ks, rk[4]);
        ks = _mm_aesenc_si128(ks, rk[5]);  ks = _mm_aesenc_si128(ks, rk[6]);
        ks = _mm_aesenc_si128(ks, rk[7]);  ks = _mm_aesenc_si128(ks, rk[8]);
        ks = _mm_aesenc_si128(ks, rk[9]);  ks = _mm_aesenclast_si128(ks, rk[10]);
        __m128i pt = _mm_loadu_si128((const __m128i *)(buf + off));
        __m128i ct = _mm_xor_si128(pt, ks);
        _mm_storeu_si128((__m128i *)(buf + off), ct);
        // GHASH fold: x = bswap(ct) ^ acc; acc = clmul-reduce(x * H)  (4x pclmulqdq, Karatsuba-ish)
        __m128i x = _mm_xor_si128(_mm_shuffle_epi8(ct, bswap), acc);
        __m128i t0 = _mm_clmulepi64_si128(x, H, 0x00);
        __m128i t1 = _mm_clmulepi64_si128(x, H, 0x11);
        __m128i t2 = _mm_xor_si128(_mm_clmulepi64_si128(x, H, 0x10), _mm_clmulepi64_si128(x, H, 0x01));
        acc = _mm_xor_si128(_mm_xor_si128(t0, t1), _mm_slli_si128(t2, 8));
        acc = _mm_xor_si128(acc, _mm_srli_si128(t2, 8));
        ctr = _mm_add_epi64(ctr, _mm_set_epi64x(0, 1));
    }
    return acc;
}

static const uint32_t SHA256K[64] = {
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2};

// SHA-256-NI one block (identical to the validated x86_sha.c): pshufb + palignr + pshufd + sha256rnds2/msg.
__attribute__((target("sha,sse4.1,ssse3")))
static void sha256_block(uint32_t state[8], const uint8_t *data) {
    __m128i STATE0, STATE1, MSG, TMP, M[4], ABEF, CDGH;
    const __m128i MASK = _mm_set_epi64x(0x0c0d0e0f08090a0bULL, 0x0405060700010203ULL);
    TMP = _mm_shuffle_epi32(_mm_loadu_si128((const __m128i *)&state[0]), 0xB1);
    STATE1 = _mm_shuffle_epi32(_mm_loadu_si128((const __m128i *)&state[4]), 0x1B);
    STATE0 = _mm_alignr_epi8(TMP, STATE1, 8);
    STATE1 = _mm_blend_epi16(STATE1, TMP, 0xF0);
    ABEF = STATE0; CDGH = STATE1;
    for (int i = 0; i < 4; i++) M[i] = _mm_shuffle_epi8(_mm_loadu_si128((const __m128i *)(data + 16 * i)), MASK);
    for (int i = 0; i < 16; i++) {
        __m128i cur = M[i & 3];
        MSG = _mm_add_epi32(cur, _mm_loadu_si128((const __m128i *)&SHA256K[4 * i]));
        STATE1 = _mm_sha256rnds2_epu32(STATE1, STATE0, MSG);
        if (i >= 3 && i <= 14) {
            TMP = _mm_alignr_epi8(cur, M[(i + 3) & 3], 4);
            M[(i + 1) & 3] = _mm_sha256msg2_epu32(_mm_add_epi32(M[(i + 1) & 3], TMP), cur);
        }
        MSG = _mm_shuffle_epi32(MSG, 0x0E);
        STATE0 = _mm_sha256rnds2_epu32(STATE0, STATE1, MSG);
        if (i >= 1 && i <= 12) M[(i + 3) & 3] = _mm_sha256msg1_epu32(M[(i + 3) & 3], M[i & 3]);
    }
    STATE0 = _mm_add_epi32(STATE0, ABEF);
    STATE1 = _mm_add_epi32(STATE1, CDGH);
    TMP = _mm_shuffle_epi32(STATE0, 0x1B);
    STATE1 = _mm_shuffle_epi32(STATE1, 0xB1);
    _mm_storeu_si128((__m128i *)&state[0], _mm_blend_epi16(TMP, STATE1, 0xF0));
    _mm_storeu_si128((__m128i *)&state[4], _mm_alignr_epi8(STATE1, TMP, 8));
}

int main(void) {
    static uint8_t buf[BUFSZ];
    for (int i = 0; i < BUFSZ; i++) buf[i] = (uint8_t)(i * 31 + 7);
    __m128i rk[11];
    for (int i = 0; i < 11; i++) rk[i] = _mm_set_epi32(i * 0x01010101 + 3, i * 7 + 1, ~i, i);
    __m128i H = _mm_set_epi64x(0x0388dace60b6a392ULL, 0x66e94bd4ef8a2c3bULL);

    // AES-GCM
    __m128i acc = _mm_setzero_si128();
    acc = aesgcm_pass(buf, rk, H, acc); // warm
    double t0 = now_s();
    for (int it = 0; it < ITERS; it++) acc = aesgcm_pass(buf, rk, H, acc);
    double t1 = now_s();
    double bytes = (double)BUFSZ * ITERS;
    double aes_mbps = bytes / (t1 - t0) / 1e6;

    // SHA-256
    uint32_t st[8] = {0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19};
    for (int off = 0; off < BUFSZ; off += 64) sha256_block(st, buf + off); // warm
    double s0 = now_s();
    for (int it = 0; it < ITERS; it++)
        for (int off = 0; off < BUFSZ; off += 64) sha256_block(st, buf + off);
    double s1 = now_s();
    double sha_mbps = bytes / (s1 - s0) / 1e6;

    // sink to prevent dead-code elimination
    volatile uint64_t sink = (uint64_t)_mm_extract_epi64(acc, 0) ^ st[0];
    printf("AESGCM_MBps %.1f\n", aes_mbps);
    printf("SHA256_MBps %.1f\n", sha_mbps);
    printf("sink %llx\n", (unsigned long long)sink);
    return 0;
}
