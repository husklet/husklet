// SHA-NI FIPS-180 known-answer + random-length differential suite. Complete SHA-256 and SHA-1
// implementations (padding included) built on the SHA-NI instructions the JIT lowers to the ARM SHA
// extension (SHA256RNDS2/MSG1/MSG2, SHA1RNDS4/NEXTE/MSG1/MSG2). The FIPS-180 vectors (empty, "abc",
// the 448-bit two-block message, one million 'a') SELF-ASSERT against the published digests -- a wrong
// hash is silent corruption, so these must hold on any host. On top, 48 xorshift-generated messages of
// pseudo-random lengths (0..~600 bytes, crossing every padding/block boundary class) are digested and
// printed; the harness byte-compares all stdout jit-vs-qemu (.oracle()).
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <immintrin.h>
#include <x86intrin.h>

// ---------------- SHA-256 via SHA-NI ----------------
static const uint32_t K256[64] = {
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2};

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
        MSG = _mm_add_epi32(cur, _mm_loadu_si128((const __m128i *)&K256[4 * i]));
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

// full message digest: init + blocks + FIPS-180 padding (len < 2^32 bytes here)
static void sha256(const uint8_t *msg, uint64_t len, uint32_t out[8]) {
    uint32_t st[8] = {0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19};
    uint64_t off = 0;
    for (; off + 64 <= len; off += 64) sha256_block(st, msg + off);
    uint8_t last[128]; memset(last, 0, sizeof last);
    uint64_t rem = len - off;
    memcpy(last, msg + off, rem);
    last[rem] = 0x80;
    uint64_t bits = len * 8;
    int n = (rem <= 55) ? 64 : 128;
    for (int i = 0; i < 8; i++) last[n - 1 - i] = (uint8_t)(bits >> (8 * i));
    sha256_block(st, last);
    if (n == 128) sha256_block(st, last + 64);
    memcpy(out, st, 32);
}

// ---------------- SHA-1 via SHA-NI ----------------
__attribute__((target("sha,sse4.1,ssse3")))
static void sha1_blocks(uint32_t state[5], const uint8_t *data, uint64_t length) {
    __m128i ABCD, ABCD_SAVE, E0, E0_SAVE, E1, MSG0, MSG1, MSG2, MSG3;
    const __m128i MASK = _mm_set_epi64x(0x0001020304050607ULL, 0x08090a0b0c0d0e0fULL);
    ABCD = _mm_loadu_si128((const __m128i *)state);
    E0 = _mm_set_epi32((int)state[4], 0, 0, 0);
    ABCD = _mm_shuffle_epi32(ABCD, 0x1B);
    while (length >= 64) {
        ABCD_SAVE = ABCD; E0_SAVE = E0;
        // round body generated systematically: at group g (rounds 4g..4g+3, K-imm g/5, E via
        // SHA1NEXTE alternating E0/E1), the schedule quads live in slots MSG[k&3]:
        //   q_{g+1} completed:  MSG[(g+1)&3] = SHA1MSG2(MSG[(g+1)&3], MSG[g&3])      (g in 3..18)
        //   q_{g+3} started:    MSG[(g+3)&3] = SHA1MSG1(MSG[(g+3)&3], MSG[g&3])      (g in 1..16)
        //   q_{g+2} W[t-8] term: MSG[(g+2)&3] ^= MSG[g&3]                            (g in 2..17)
        /* rounds 0-3 */
        MSG0 = _mm_shuffle_epi8(_mm_loadu_si128((const __m128i *)(data + 0)), MASK);
        E0 = _mm_add_epi32(E0, MSG0);
        E1 = ABCD;
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 0);
        /* rounds 4-7 */
        MSG1 = _mm_shuffle_epi8(_mm_loadu_si128((const __m128i *)(data + 16)), MASK);
        E1 = _mm_sha1nexte_epu32(E1, MSG1);
        E0 = ABCD;
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 0);
        MSG0 = _mm_sha1msg1_epu32(MSG0, MSG1);
        /* rounds 8-11 */
        MSG2 = _mm_shuffle_epi8(_mm_loadu_si128((const __m128i *)(data + 32)), MASK);
        E0 = _mm_sha1nexte_epu32(E0, MSG2);
        E1 = ABCD;
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 0);
        MSG1 = _mm_sha1msg1_epu32(MSG1, MSG2);
        MSG0 = _mm_xor_si128(MSG0, MSG2);
        /* rounds 12-15 */
        MSG3 = _mm_shuffle_epi8(_mm_loadu_si128((const __m128i *)(data + 48)), MASK);
        E1 = _mm_sha1nexte_epu32(E1, MSG3);
        E0 = ABCD;
        MSG0 = _mm_sha1msg2_epu32(MSG0, MSG3);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 0);
        MSG2 = _mm_sha1msg1_epu32(MSG2, MSG3);
        MSG1 = _mm_xor_si128(MSG1, MSG3);
        /* rounds 16-19 */
        E0 = _mm_sha1nexte_epu32(E0, MSG0);
        E1 = ABCD;
        MSG1 = _mm_sha1msg2_epu32(MSG1, MSG0);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 0);
        MSG3 = _mm_sha1msg1_epu32(MSG3, MSG0);
        MSG2 = _mm_xor_si128(MSG2, MSG0);
        /* rounds 20-23 */
        E1 = _mm_sha1nexte_epu32(E1, MSG1);
        E0 = ABCD;
        MSG2 = _mm_sha1msg2_epu32(MSG2, MSG1);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 1);
        MSG0 = _mm_sha1msg1_epu32(MSG0, MSG1);
        MSG3 = _mm_xor_si128(MSG3, MSG1);
        /* rounds 24-27 */
        E0 = _mm_sha1nexte_epu32(E0, MSG2);
        E1 = ABCD;
        MSG3 = _mm_sha1msg2_epu32(MSG3, MSG2);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 1);
        MSG1 = _mm_sha1msg1_epu32(MSG1, MSG2);
        MSG0 = _mm_xor_si128(MSG0, MSG2);
        /* rounds 28-31 */
        E1 = _mm_sha1nexte_epu32(E1, MSG3);
        E0 = ABCD;
        MSG0 = _mm_sha1msg2_epu32(MSG0, MSG3);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 1);
        MSG2 = _mm_sha1msg1_epu32(MSG2, MSG3);
        MSG1 = _mm_xor_si128(MSG1, MSG3);
        /* rounds 32-35 */
        E0 = _mm_sha1nexte_epu32(E0, MSG0);
        E1 = ABCD;
        MSG1 = _mm_sha1msg2_epu32(MSG1, MSG0);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 1);
        MSG3 = _mm_sha1msg1_epu32(MSG3, MSG0);
        MSG2 = _mm_xor_si128(MSG2, MSG0);
        /* rounds 36-39 */
        E1 = _mm_sha1nexte_epu32(E1, MSG1);
        E0 = ABCD;
        MSG2 = _mm_sha1msg2_epu32(MSG2, MSG1);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 1);
        MSG0 = _mm_sha1msg1_epu32(MSG0, MSG1);
        MSG3 = _mm_xor_si128(MSG3, MSG1);
        /* rounds 40-43 */
        E0 = _mm_sha1nexte_epu32(E0, MSG2);
        E1 = ABCD;
        MSG3 = _mm_sha1msg2_epu32(MSG3, MSG2);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 2);
        MSG1 = _mm_sha1msg1_epu32(MSG1, MSG2);
        MSG0 = _mm_xor_si128(MSG0, MSG2);
        /* rounds 44-47 */
        E1 = _mm_sha1nexte_epu32(E1, MSG3);
        E0 = ABCD;
        MSG0 = _mm_sha1msg2_epu32(MSG0, MSG3);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 2);
        MSG2 = _mm_sha1msg1_epu32(MSG2, MSG3);
        MSG1 = _mm_xor_si128(MSG1, MSG3);
        /* rounds 48-51 */
        E0 = _mm_sha1nexte_epu32(E0, MSG0);
        E1 = ABCD;
        MSG1 = _mm_sha1msg2_epu32(MSG1, MSG0);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 2);
        MSG3 = _mm_sha1msg1_epu32(MSG3, MSG0);
        MSG2 = _mm_xor_si128(MSG2, MSG0);
        /* rounds 52-55 */
        E1 = _mm_sha1nexte_epu32(E1, MSG1);
        E0 = ABCD;
        MSG2 = _mm_sha1msg2_epu32(MSG2, MSG1);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 2);
        MSG0 = _mm_sha1msg1_epu32(MSG0, MSG1);
        MSG3 = _mm_xor_si128(MSG3, MSG1);
        /* rounds 56-59 */
        E0 = _mm_sha1nexte_epu32(E0, MSG2);
        E1 = ABCD;
        MSG3 = _mm_sha1msg2_epu32(MSG3, MSG2);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 2);
        MSG1 = _mm_sha1msg1_epu32(MSG1, MSG2);
        MSG0 = _mm_xor_si128(MSG0, MSG2);
        /* rounds 60-63 */
        E1 = _mm_sha1nexte_epu32(E1, MSG3);
        E0 = ABCD;
        MSG0 = _mm_sha1msg2_epu32(MSG0, MSG3);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 3);
        MSG2 = _mm_sha1msg1_epu32(MSG2, MSG3);
        MSG1 = _mm_xor_si128(MSG1, MSG3);
        /* rounds 64-67 */
        E0 = _mm_sha1nexte_epu32(E0, MSG0);
        E1 = ABCD;
        MSG1 = _mm_sha1msg2_epu32(MSG1, MSG0);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 3);
        MSG3 = _mm_sha1msg1_epu32(MSG3, MSG0);
        MSG2 = _mm_xor_si128(MSG2, MSG0);
        /* rounds 68-71 */
        E1 = _mm_sha1nexte_epu32(E1, MSG1);
        E0 = ABCD;
        MSG2 = _mm_sha1msg2_epu32(MSG2, MSG1);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 3);
        MSG3 = _mm_xor_si128(MSG3, MSG1);
        /* rounds 72-75 */
        E0 = _mm_sha1nexte_epu32(E0, MSG2);
        E1 = ABCD;
        MSG3 = _mm_sha1msg2_epu32(MSG3, MSG2);
        ABCD = _mm_sha1rnds4_epu32(ABCD, E0, 3);
        /* rounds 76-79 */
        E1 = _mm_sha1nexte_epu32(E1, MSG3);
        E0 = ABCD;
        ABCD = _mm_sha1rnds4_epu32(ABCD, E1, 3);
        /* fold */
        E0 = _mm_sha1nexte_epu32(E0, E0_SAVE);
        ABCD = _mm_add_epi32(ABCD, ABCD_SAVE);
        data += 64; length -= 64;
    }
    ABCD = _mm_shuffle_epi32(ABCD, 0x1B);
    _mm_storeu_si128((__m128i *)state, ABCD);
    state[4] = (uint32_t)_mm_extract_epi32(E0, 3);
}

static void sha1(const uint8_t *msg, uint64_t len, uint32_t out[5]) {
    uint32_t st[5] = {0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0};
    uint64_t full = len & ~63ULL;
    sha1_blocks(st, msg, full);
    uint8_t last[128]; memset(last, 0, sizeof last);
    uint64_t rem = len - full;
    memcpy(last, msg + full, rem);
    last[rem] = 0x80;
    uint64_t bits = len * 8;
    int n = (rem <= 55) ? 64 : 128;
    for (int i = 0; i < 8; i++) last[n - 1 - i] = (uint8_t)(bits >> (8 * i));
    sha1_blocks(st, last, (uint64_t)n);
    memcpy(out, st, 20);
}

// ---------------- vectors ----------------
static void p256(const char *tag, const uint32_t d[8]) {
    printf("%s ", tag);
    for (int i = 0; i < 8; i++) printf("%08x", d[i]);
    printf("\n");
}
static void p160(const char *tag, const uint32_t d[5]) {
    printf("%s ", tag);
    for (int i = 0; i < 5; i++) printf("%08x", d[i]);
    printf("\n");
}
static int eq(const uint32_t *a, const uint32_t *b, int n) {
    for (int i = 0; i < n; i++) if (a[i] != b[i]) return 0;
    return 1;
}

int main(void) {
    static uint8_t buf[1000000];
    uint32_t d8[8], d5[5];
    int ok = 1;

    // FIPS-180-4 SHA-256 KATs
    static const uint32_t k256_empty[8] = {0xe3b0c442,0x98fc1c14,0x9afbf4c8,0x996fb924,0x27ae41e4,0x649b934c,0xa495991b,0x7852b855};
    static const uint32_t k256_abc[8]   = {0xba7816bf,0x8f01cfea,0x414140de,0x5dae2223,0xb00361a3,0x96177a9c,0xb410ff61,0xf20015ad};
    static const uint32_t k256_448[8]   = {0x248d6a61,0xd20638b8,0xe5c02693,0x0c3e6039,0xa33ce459,0x64ff2167,0xf6ecedd4,0x19db06c1};
    static const uint32_t k256_mil[8]   = {0xcdc76e5c,0x9914fb92,0x81a1c7e2,0x84d73e67,0xf1809a48,0xa497200e,0x046d39cc,0xc7112cd0};
    sha256((const uint8_t *)"", 0, d8);                  p256("sha256-empty", d8); ok &= eq(d8, k256_empty, 8);
    sha256((const uint8_t *)"abc", 3, d8);               p256("sha256-abc", d8);   ok &= eq(d8, k256_abc, 8);
    sha256((const uint8_t *)"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", 56, d8);
    p256("sha256-448", d8); ok &= eq(d8, k256_448, 8);
    memset(buf, 'a', 1000000);
    sha256(buf, 1000000, d8);                            p256("sha256-million-a", d8); ok &= eq(d8, k256_mil, 8);

    // FIPS-180-4 SHA-1 KATs
    static const uint32_t k1_empty[5] = {0xda39a3ee,0x5e6b4b0d,0x3255bfef,0x95601890,0xafd80709};
    static const uint32_t k1_abc[5]   = {0xa9993e36,0x4706816a,0xba3e2571,0x7850c26c,0x9cd0d89d};
    static const uint32_t k1_448[5]   = {0x84983e44,0x1c3bd26e,0xbaae4aa1,0xf95129e5,0xe54670f1};
    static const uint32_t k1_mil[5]   = {0x34aa973c,0xd4c4daa4,0xf61eeb2b,0xdbad2731,0x6534016f};
    sha1((const uint8_t *)"", 0, d5);                    p160("sha1-empty", d5); ok &= eq(d5, k1_empty, 5);
    sha1((const uint8_t *)"abc", 3, d5);                 p160("sha1-abc", d5);   ok &= eq(d5, k1_abc, 5);
    sha1((const uint8_t *)"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", 56, d5);
    p160("sha1-448", d5); ok &= eq(d5, k1_448, 5);
    sha1(buf, 1000000, d5);                              p160("sha1-million-a", d5); ok &= eq(d5, k1_mil, 5);

    // random-length messages (xorshift content + lengths crossing all padding boundaries: 0..~600,
    // incl. exact block multiples and the 55/56/63/64 pad edges). Not self-asserted (no published
    // digest) -- validated by the jit-vs-qemu byte diff.
    uint64_t x = 0x243F6A8885A308D3ULL;
    static const int lens[] = {1, 3, 8, 31, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128, 129,
                               191, 255, 256, 300, 447, 448, 511, 512, 577};
    for (unsigned i = 0; i < sizeof lens / sizeof lens[0]; i++) {
        int L = lens[i];
        for (int j = 0; j < L; j++) { x ^= x << 13; x ^= x >> 7; x ^= x << 17; buf[j] = (uint8_t)x; }
        char tag[32];
        sha256(buf, (uint64_t)L, d8); snprintf(tag, sizeof tag, "r256-%d", L); p256(tag, d8);
        sha1(buf, (uint64_t)L, d5);   snprintf(tag, sizeof tag, "r1-%d", L);   p160(tag, d5);
    }

    printf("kat=%d\n", ok);
    return ok ? 0 : 1;
}
