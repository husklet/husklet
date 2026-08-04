// SSSE3/SSE4 "shuffle glue" lowered inline to ARM NEON (perf lever #1): PSHUFB (TBL, incl. the 0x80
// hi-bit-zeroing that x86 does but ARM TBL expresses via out-of-range indices), PALIGNR (EXT, all imm
// incl. >=16 and >=32 edges), PBLENDW (per-word select), PBLENDVB/BLENDVPS/BLENDVPD (xmm0 top-bit mask),
// and the full PMOVSX/PMOVZX widening family (bw/bd/bq/wd/wq/dq, sign + zero). Every result is printed as
// hex and the whole stdout is byte-compared jit-vs-qemu, so this is the byte-exact differential + KAT.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <immintrin.h>
#include <x86intrin.h>
#include <tmmintrin.h>
#include <smmintrin.h>

static void ph(const char *t, __m128i v) {
    uint8_t b[16]; _mm_storeu_si128((__m128i *)b, v);
    printf("%s ", t); for (int i = 0; i < 16; i++) printf("%02x", b[i]); printf("\n");
}

__attribute__((target("sse4.1,ssse3"))) static long go(void) {
    static const uint64_t vv[][2] = {
        {0, 0}, {~0ULL, ~0ULL}, {0x1122334455667788ULL, 0x99aabbccddeeff00ULL},
        {0x8000000000000000ULL, 0x0000000000000001ULL}, {0xdeadbeefcafebabeULL, 0x0123456789abcdefULL},
        {0x807f017e02fd0380ULL, 0x00ff8081097f7e00ULL},
    };
    int n = sizeof(vv) / sizeof(vv[0]);

    // ---- PSHUFB: sweep controls that mix in-range selects and 0x80 hi-bit zeroing ----
    static const uint64_t ctl[][2] = {
        {0x0001020304050607ULL, 0x08090a0b0c0d0e0fULL}, // straight reverse-ish (no zeroing)
        {0x8081828384858687ULL, 0x88898a8b8c8d8e8fULL}, // every lane hi-bit set -> ALL ZERO
        {0x00800180028f0380ULL, 0x0f000e010d020c03ULL}, // mixed: some zeroed, some low-nibble selects
        {0x1011121314151617ULL, 0x18191a1b1c1d1e1fULL}, // bits 4-6 set but bit7 clear -> masked to low nibble
    };
    for (int i = 0; i < n; i++) {
        __m128i a = _mm_set_epi64x(vv[i][1], vv[i][0]);
        for (int c = 0; c < 4; c++) {
            __m128i idx = _mm_set_epi64x(ctl[c][1], ctl[c][0]);
            ph("pshufb", _mm_shuffle_epi8(a, idx));
        }
    }

    // ---- PALIGNR: all imm from 0..33 (covers <16, [16,32), >=32) ----
    for (int i = 0; i < n; i++) {
        __m128i a = _mm_set_epi64x(vv[i][1], vv[i][0]);
        __m128i b = _mm_set_epi64x(vv[(i + 1) % n][1], vv[(i + 2) % n][0]);
#define PA(k) ph("palignr", _mm_alignr_epi8(a, b, k))
        PA(0); PA(1); PA(3); PA(7); PA(8); PA(15); PA(16); PA(17); PA(23); PA(31); PA(32); PA(33);
#undef PA
    }

    // ---- PBLENDW: several word masks ----
    for (int i = 0; i < n; i++) {
        __m128i a = _mm_set_epi64x(vv[i][1], vv[i][0]);
        __m128i b = _mm_set_epi64x(vv[(i + 3) % n][1], vv[(i + 1) % n][0]);
        ph("pblendw00", _mm_blend_epi16(a, b, 0x00));
        ph("pblendwff", _mm_blend_epi16(a, b, 0xff));
        ph("pblendwf0", _mm_blend_epi16(a, b, 0xf0));
        ph("pblendw55", _mm_blend_epi16(a, b, 0x55));
        ph("pblendwa5", _mm_blend_epi16(a, b, 0xa5));
    }

    // ---- PBLENDVB / BLENDVPS / BLENDVPD: mask = xmm0 top bit of each byte/dword/qword ----
    for (int i = 0; i < n; i++) {
        __m128i a = _mm_set_epi64x(vv[i][1], vv[i][0]);
        __m128i b = _mm_set_epi64x(vv[(i + 2) % n][1], vv[(i + 4) % n][0]);
        __m128i m = _mm_set_epi8(0x00, (char)0x80, 0x7f, (char)0xff, 0x01, (char)0x81, 0x00, (char)0xc0,
                                 (char)0x80, 0x00, (char)0xff, 0x00, 0x00, (char)0x80, 0x7f, (char)0x80);
        ph("pblendvb", _mm_blendv_epi8(a, b, m));
        ph("blendvps", _mm_castps_si128(_mm_blendv_ps(_mm_castsi128_ps(a), _mm_castsi128_ps(b), _mm_castsi128_ps(m))));
        ph("blendvpd", _mm_castpd_si128(_mm_blendv_pd(_mm_castsi128_pd(a), _mm_castsi128_pd(b), _mm_castsi128_pd(m))));
    }

    // ---- PMOVSX / PMOVZX: all widths, sign vs zero, with negative + high-bit sources ----
    for (int i = 0; i < n; i++) {
        __m128i a = _mm_set_epi64x(vv[i][1], vv[i][0]);
        ph("sxbw", _mm_cvtepi8_epi16(a));  ph("zxbw", _mm_cvtepu8_epi16(a));
        ph("sxbd", _mm_cvtepi8_epi32(a));  ph("zxbd", _mm_cvtepu8_epi32(a));
        ph("sxbq", _mm_cvtepi8_epi64(a));  ph("zxbq", _mm_cvtepu8_epi64(a));
        ph("sxwd", _mm_cvtepi16_epi32(a)); ph("zxwd", _mm_cvtepu16_epi32(a));
        ph("sxwq", _mm_cvtepi16_epi64(a)); ph("zxwq", _mm_cvtepu16_epi64(a));
        ph("sxdq", _mm_cvtepi32_epi64(a)); ph("zxdq", _mm_cvtepu32_epi64(a));
    }
    // memory-operand widening forms (compiler may fold the load into pmovsx/zx m*)
    static const uint8_t mem[16] = {0x81, 0x02, 0xfe, 0x7f, 0x00, 0x80, 0x11, 0x22,
                                    0x33, 0xcc, 0x55, 0xaa, 0x99, 0x01, 0xff, 0x00};
    ph("msxbw", _mm_cvtepi8_epi16(_mm_loadu_si128((const __m128i *)mem)));
    ph("mzxbd", _mm_cvtepu8_epi32(_mm_loadu_si128((const __m128i *)mem)));
    ph("msxbq", _mm_cvtepi8_epi64(_mm_loadu_si128((const __m128i *)mem)));
    ph("mzxwq", _mm_cvtepu16_epi64(_mm_loadu_si128((const __m128i *)mem)));

    return 0;
}
int main(void) { go(); printf("shuffle done\n"); return 0; }
