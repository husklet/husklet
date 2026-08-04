// SHA-NI (task #342): the FULL SHA-NI surface -- SHA256RNDS2 / SHA256MSG1 / SHA256MSG2 and
// SHA1RNDS4 (all 4 imm2) / SHA1NEXTE / SHA1MSG1 / SHA1MSG2 -- all lowered to the inline ARM SHA
// extension -- over many vectors, PLUS operand-shape corners (memory operands, aliased dst==src,
// dst/src == the implicit xmm0) via inline asm, PLUS a SHA-256("abc") known-answer digest. Full
// stdout byte-compared jit-vs-qemu; the KAT line self-asserts. (Full FIPS-180 digest KATs +
// random-length messages live in x86_sha_kat.c.)
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <immintrin.h>
#include <x86intrin.h>

static void ph(const char *t, __m128i v) {
    uint8_t b[16]; _mm_storeu_si128((__m128i *)b, v);
    printf("%s ", t); for (int i = 0; i < 16; i++) printf("%02x", b[i]); printf("\n");
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

// SHA-256 one block via SHA-NI (K-table form; the K[] lane order matches _mm_loadu of 4 words).
__attribute__((target("sha,sse4.1,ssse3"))) static void sha256_ni_block(uint32_t state[8], const uint8_t *data) {
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
        if (i >= 3 && i <= 14) { // schedule the quad used 4 groups later
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

__attribute__((target("sha,sse4.1,ssse3"))) static long go(void) {
    static const uint32_t seeds[] = {0, 0xffffffff, 0x12345678, 0x9abcdef0, 0x80000000, 1, 0xdeadbeef, 0x6a09e667};
    int n = sizeof(seeds) / sizeof(seeds[0]);
    for (int i = 0; i < n; i++)
        for (int j = 0; j < n; j++) {
            __m128i a = _mm_set_epi32(seeds[i], seeds[j], seeds[(i + 1) % n], seeds[(j + 1) % n]);
            __m128i b = _mm_set_epi32(seeds[j], seeds[i], seeds[(j + 2) % n], seeds[(i + 3) % n]);
            __m128i wk = _mm_set_epi32(seeds[(i + j) % n], seeds[(i * 3 + 1) % n], seeds[i], seeds[j]);
            ph("rnds2", _mm_sha256rnds2_epu32(a, b, wk));
            ph("msg1", _mm_sha256msg1_epu32(a, b));
            ph("msg2", _mm_sha256msg2_epu32(a, b));
            switch ((i + j) & 3) { // sha1rnds4 imm must be a compile-time constant
            case 0: ph("sha1r4", _mm_sha1rnds4_epu32(a, b, 0)); break;
            case 1: ph("sha1r4", _mm_sha1rnds4_epu32(a, b, 1)); break;
            case 2: ph("sha1r4", _mm_sha1rnds4_epu32(a, b, 2)); break;
            default: ph("sha1r4", _mm_sha1rnds4_epu32(a, b, 3)); break;
            }
            ph("sha1nexte", _mm_sha1nexte_epu32(a, b));
            ph("sha1msg1", _mm_sha1msg1_epu32(a, b));
            ph("sha1msg2", _mm_sha1msg2_epu32(a, b));
        }
    // ---- operand-shape corners (inline asm; intrinsics never emit these shapes) ----
    // Every SHA-NI op with a MEMORY r/m operand, aliased dst==src, and rnds2 where dst or src IS the
    // implicit xmm0 -- exercises crypto_rm_vec + the register-alias paths of the ARM lowering.
    {
        __m128i mem[2];
        mem[0] = _mm_set_epi32(0x00010203, 0x8899aabb, 0x7f7f7f7f, 0xdeadbeef);
        mem[1] = _mm_set_epi32(0xcafebabe, 0x31415926, 0x27182818, 0x00000001);
        __m128i a = _mm_set_epi32(0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a);
        __m128i wk = _mm_set_epi32(0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5);
        __m128i t;
        register __m128i x0 asm("xmm0");
        // memory r/m forms
        x0 = wk; t = a;
        asm("sha256rnds2 %1, %0" : "+x"(t) : "m"(mem[0]), "x"(x0));
        ph("m_rnds2", t);
        t = a; asm("sha256msg1 %1, %0" : "+x"(t) : "m"(mem[0])); ph("m_msg1", t);
        t = a; asm("sha256msg2 %1, %0" : "+x"(t) : "m"(mem[1])); ph("m_msg2", t);
        t = a; asm("sha1rnds4 $2, %1, %0" : "+x"(t) : "m"(mem[0])); ph("m_sha1r4", t);
        t = a; asm("sha1nexte %1, %0" : "+x"(t) : "m"(mem[1])); ph("m_sha1nexte", t);
        t = a; asm("sha1msg1 %1, %0" : "+x"(t) : "m"(mem[0])); ph("m_sha1msg1", t);
        t = a; asm("sha1msg2 %1, %0" : "+x"(t) : "m"(mem[1])); ph("m_sha1msg2", t);
        // aliased dst==src (the SU1 alias path in sha256msg2; read-before-write everywhere else)
        x0 = wk; t = a;
        asm("sha256rnds2 %0, %0" : "+x"(t) : "x"(x0)); ph("al_rnds2", t);
        t = a; asm("sha256msg1 %0, %0" : "+x"(t)); ph("al_msg1", t);
        t = a; asm("sha256msg2 %0, %0" : "+x"(t)); ph("al_msg2", t);
        t = a; asm("sha1rnds4 $1, %0, %0" : "+x"(t)); ph("al_sha1r4", t);
        t = a; asm("sha1nexte %0, %0" : "+x"(t)); ph("al_sha1nexte", t);
        t = a; asm("sha1msg1 %0, %0" : "+x"(t)); ph("al_sha1msg1", t);
        t = a; asm("sha1msg2 %0, %0" : "+x"(t)); ph("al_sha1msg2", t);
        // rnds2 with dst == xmm0 (state, key and WK all in one reg) and src == xmm0
        x0 = wk;
        asm("sha256rnds2 %1, %0" : "+x"(x0) : "x"(a)); ph("x0_rnds2d", x0);
        x0 = wk; t = a;
        asm("sha256rnds2 %1, %0" : "+x"(t) : "x"(x0)); ph("x0_rnds2s", t);
        // all-in-one: dst == src == xmm0
        x0 = wk;
        asm("sha256rnds2 %0, %0" : "+x"(x0)); ph("x0_rnds2ds", x0);
    }
    // SHA-256("abc") = ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad
    uint32_t st[8] = {0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19};
    uint8_t blk[64]; memset(blk, 0, 64);
    blk[0]='a'; blk[1]='b'; blk[2]='c'; blk[3]=0x80; blk[63]=0x18;
    sha256_ni_block(st, blk);
    static const uint32_t kat[8] = {0xba7816bf,0x8f01cfea,0x414140de,0x5dae2223,0xb00361a3,0x96177a9c,0xb410ff61,0xf20015ad};
    int ok = 1; for (int i = 0; i < 8; i++) if (st[i] != kat[i]) ok = 0;
    printf("sha256(abc) kat=%d digest=%08x%08x\n", ok, st[0], st[7]);
    return (long)st[0];
}
int main(void) { printf("sha r=%ld\n", go()); return 0; }
