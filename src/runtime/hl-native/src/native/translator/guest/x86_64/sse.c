#include "avx_internal.h"
#include "rep_runtime.h"
#include "../../../host/cpu.h"

#include <fenv.h>
#include <math.h>
#include <stdio.h>
#include <string.h>
#if defined(HL_HOST_CPU_X86_64)
#include <xmmintrin.h>
#endif

static int g_sse_warned;

const uint8_t k_aes_sbox[256] = {
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76, 0xca, 0x82, 0xc9,
    0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f,
    0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07,
    0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3,
    0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58,
    0xcf, 0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3,
    0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec, 0x5f,
    0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73, 0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88,
    0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac,
    0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a,
    0xae, 0x08, 0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a, 0x70,
    0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1, 0xf8, 0x98, 0x11,
    0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf, 0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42,
    0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16};
const uint8_t k_aes_isbox[256] = {
    0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3, 0xd7, 0xfb, 0x7c, 0xe3, 0x39,
    0x82, 0x9b, 0x2f, 0xff, 0x87, 0x34, 0x8e, 0x43, 0x44, 0xc4, 0xde, 0xe9, 0xcb, 0x54, 0x7b, 0x94, 0x32, 0xa6, 0xc2,
    0x23, 0x3d, 0xee, 0x4c, 0x95, 0x0b, 0x42, 0xfa, 0xc3, 0x4e, 0x08, 0x2e, 0xa1, 0x66, 0x28, 0xd9, 0x24, 0xb2, 0x76,
    0x5b, 0xa2, 0x49, 0x6d, 0x8b, 0xd1, 0x25, 0x72, 0xf8, 0xf6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xd4, 0xa4, 0x5c, 0xcc,
    0x5d, 0x65, 0xb6, 0x92, 0x6c, 0x70, 0x48, 0x50, 0xfd, 0xed, 0xb9, 0xda, 0x5e, 0x15, 0x46, 0x57, 0xa7, 0x8d, 0x9d,
    0x84, 0x90, 0xd8, 0xab, 0x00, 0x8c, 0xbc, 0xd3, 0x0a, 0xf7, 0xe4, 0x58, 0x05, 0xb8, 0xb3, 0x45, 0x06, 0xd0, 0x2c,
    0x1e, 0x8f, 0xca, 0x3f, 0x0f, 0x02, 0xc1, 0xaf, 0xbd, 0x03, 0x01, 0x13, 0x8a, 0x6b, 0x3a, 0x91, 0x11, 0x41, 0x4f,
    0x67, 0xdc, 0xea, 0x97, 0xf2, 0xcf, 0xce, 0xf0, 0xb4, 0xe6, 0x73, 0x96, 0xac, 0x74, 0x22, 0xe7, 0xad, 0x35, 0x85,
    0xe2, 0xf9, 0x37, 0xe8, 0x1c, 0x75, 0xdf, 0x6e, 0x47, 0xf1, 0x1a, 0x71, 0x1d, 0x29, 0xc5, 0x89, 0x6f, 0xb7, 0x62,
    0x0e, 0xaa, 0x18, 0xbe, 0x1b, 0xfc, 0x56, 0x3e, 0x4b, 0xc6, 0xd2, 0x79, 0x20, 0x9a, 0xdb, 0xc0, 0xfe, 0x78, 0xcd,
    0x5a, 0xf4, 0x1f, 0xdd, 0xa8, 0x33, 0x88, 0x07, 0xc7, 0x31, 0xb1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xec, 0x5f, 0x60,
    0x51, 0x7f, 0xa9, 0x19, 0xb5, 0x4a, 0x0d, 0x2d, 0xe5, 0x7a, 0x9f, 0x93, 0xc9, 0x9c, 0xef, 0xa0, 0xe0, 0x3b, 0x4d,
    0xae, 0x2a, 0xf5, 0xb0, 0xc8, 0xeb, 0xbb, 0x3c, 0x83, 0x53, 0x99, 0x61, 0x17, 0x2b, 0x04, 0x7e, 0xba, 0x77, 0xd6,
    0x26, 0xe1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0c, 0x7d};

static uint8_t aes_gfmul(uint8_t a, uint8_t b) {
    uint8_t p = 0;
    for (int i = 0; i < 8; i++) {
        if (b & 1) p ^= a;
        uint8_t hi = a & 0x80;
        a <<= 1;
        if (hi) a ^= 0x1b;
        b >>= 1;
    }
    return p;
}

void aes_subbytes(uint8_t s[16], const uint8_t box[256]) {
    for (int i = 0; i < 16; i++)
        s[i] = box[s[i]];
}

// ShiftRows (inv=0) / InvShiftRows (inv=1). State is column-major: s[4*col+row].
void aes_shiftrows(const uint8_t in[16], uint8_t out[16], int inv) {
    for (int col = 0; col < 4; col++)
        for (int row = 0; row < 4; row++) {
            int sc = inv ? ((col - row) & 3) : ((col + row) & 3);
            out[4 * col + row] = in[4 * sc + row];
        }
}

void aes_mixcolumns(uint8_t s[16], int inv) {
    for (int col = 0; col < 4; col++) {
        uint8_t a0 = s[4 * col], a1 = s[4 * col + 1], a2 = s[4 * col + 2], a3 = s[4 * col + 3];
        if (!inv) {
            s[4 * col] = aes_gfmul(a0, 2) ^ aes_gfmul(a1, 3) ^ a2 ^ a3;
            s[4 * col + 1] = a0 ^ aes_gfmul(a1, 2) ^ aes_gfmul(a2, 3) ^ a3;
            s[4 * col + 2] = a0 ^ a1 ^ aes_gfmul(a2, 2) ^ aes_gfmul(a3, 3);
            s[4 * col + 3] = aes_gfmul(a0, 3) ^ a1 ^ a2 ^ aes_gfmul(a3, 2);
        } else {
            s[4 * col] = aes_gfmul(a0, 14) ^ aes_gfmul(a1, 11) ^ aes_gfmul(a2, 13) ^ aes_gfmul(a3, 9);
            s[4 * col + 1] = aes_gfmul(a0, 9) ^ aes_gfmul(a1, 14) ^ aes_gfmul(a2, 11) ^ aes_gfmul(a3, 13);
            s[4 * col + 2] = aes_gfmul(a0, 13) ^ aes_gfmul(a1, 9) ^ aes_gfmul(a2, 14) ^ aes_gfmul(a3, 11);
            s[4 * col + 3] = aes_gfmul(a0, 11) ^ aes_gfmul(a1, 13) ^ aes_gfmul(a2, 9) ^ aes_gfmul(a3, 14);
        }
    }
}

static inline uint32_t rotr32(uint32_t x, int n) {
    return (x >> n) | (x << (32 - n));
}

static inline uint32_t rotl32(uint32_t x, int n) {
    return (x << n) | (x >> (32 - n));
}

// CRC-32C (Castagnoli, reflected poly 0x82F63B78) -- the polynomial used by the x86 CRC32 instruction.
static uint32_t crc32c_step(uint32_t crc, uint64_t v, int nbytes) {
    for (int b = 0; b < nbytes; b++) {
        crc ^= (uint8_t)(v >> (8 * b));
        for (int i = 0; i < 8; i++)
            crc = (crc >> 1) ^ (0x82F63B78u & (uint32_t)(-(int32_t)(crc & 1)));
    }
    return crc;
}

// ROUND* rounding. When the imm's bit2 is set the op must use the CURRENT rounding mode (MXCSR.RC),
// which ldmxcsr has already threaded into the host FPCR.RMode -- so __builtin_rint (current-mode) honors
// it here (fixes the "treated as nearest" gap). The explicit modes 0..3 must instead force that specific
// direction regardless of MXCSR: floor/ceil/trunc already do, and explicit nearest is round-to-nearest-
// EVEN independent of the current mode (__builtin_roundeven), not __builtin_rint (which would follow RC).
//
// __builtin_rint cannot serve that on an x86-64 host, and fails silently: with no ROUNDSD available
// (-msse4.1 is not assumable) gcc inlines `|x| + 2^52 - 2^52` with the sign re-applied, rounding the
// MAGNITUDE -- valid only under round-to-nearest, i.e. not the mode LDMXCSR just set. So resolve MXCSR.RC
// (bit-for-bit the ROUND* imm[1:0] encoding) instead. AArch64 keeps the builtin; it lowers to FRINTX.
#if defined(HL_HOST_CPU_X86_64)
int sse_host_rounding_control(void) {
    return (int)((_mm_getcsr() >> 13) & 3u); // MXCSR bits 14:13 = RC
}
#endif

// ROUND* raises a SMALLER set than any rounding primitive available here, so the whole step runs with the
// sticky flags parked and the caller's set is raised explicitly. Measured on Zen 4, all imm encodings:
//   #P iff the result differs from the source, and imm[3] (the suppress-precision bit) is clear;
//   #I iff the source is an SNaN, which the result also QUIETS (imm[3] suppresses neither);
//   NOTHING else -- notably NEVER #D, which is what glibc's trunc/floor add via ROUNDSD (roundps of a
//   denormal reported 22 here against native's 20).
// Difference is decided on the BIT PATTERN: an FP compare of the two is a COMISD, which raises #D for a
// denormal operand, and the sign of a zero must count (trunc(-0.4) = -0.0 is inexact). DAZ has to be
// applied by hand for the same reason -- the hardware zeroes a denormal source before ROUND* runs, making
// it exact, but a bit-pattern comparison against the UNzeroed source would call it inexact.
double sse_round_d(double x, int imm) {
    unsigned parked = cvt_fp_flags(), raise = 0;
    uint64_t xb;
    memcpy(&xb, &x, 8);
    if (sse_daz_active() && sse_is_denorm_f64(xb)) { // DAZ: the source reads as a zero of the same sign
        xb &= UINT64_C(0x8000000000000000);
        memcpy(&x, &xb, 8);
    }
    int mode = imm & 3, use_mxcsr = (imm & 4) != 0;
#if defined(HL_HOST_CPU_X86_64)
    if (use_mxcsr) {
        mode = sse_host_rounding_control();
        use_mxcsr = 0;
    }
#endif
    double r;
    if (use_mxcsr)
        r = __builtin_rint(x); // honor MXCSR.RC (mirrored into host FPCR by ldmxcsr)
    else
        switch (mode) {
        case 1: r = __builtin_floor(x); break;
        case 2: r = __builtin_ceil(x); break;
        case 3: r = __builtin_trunc(x); break;
        default: r = __builtin_roundeven(x); break; // explicit round-to-nearest-even
        }
    uint64_t rb;
    memcpy(&rb, &r, 8);
    if ((xb & UINT64_C(0x7ff0000000000000)) == UINT64_C(0x7ff0000000000000) &&
        (xb & UINT64_C(0x000fffffffffffff)) != 0) { // NaN
        if (!(xb & UINT64_C(0x0008000000000000))) { // signalling
            raise = SSE_XI;
            rb = xb | UINT64_C(0x0008000000000000); // x86 returns it QUIET; glibc trunc/floor do not
            memcpy(&r, &rb, 8);
        }
    } else if (rb != xb && !(imm & 8))
        raise = SSE_XP;
    cvt_fp_flags_set(parked);
    hl_x86_sse_raise(raise);
    return r;
}

float sse_round_f(float x, int imm) {
    unsigned parked = cvt_fp_flags(), raise = 0;
    uint32_t xb;
    memcpy(&xb, &x, 4);
    if (sse_daz_active() && sse_is_denorm_f32(xb)) {
        xb &= 0x80000000u;
        memcpy(&x, &xb, 4);
    }
    int mode = imm & 3, use_mxcsr = (imm & 4) != 0;
#if defined(HL_HOST_CPU_X86_64)
    if (use_mxcsr) {
        mode = sse_host_rounding_control();
        use_mxcsr = 0;
    }
#endif
    float r;
    if (use_mxcsr)
        r = __builtin_rintf(x);
    else
        switch (mode) {
        case 1: r = __builtin_floorf(x); break;
        case 2: r = __builtin_ceilf(x); break;
        case 3: r = __builtin_truncf(x); break;
        default: r = __builtin_roundevenf(x); break;
        }
    uint32_t rb;
    memcpy(&rb, &r, 4);
    if ((xb & 0x7f800000u) == 0x7f800000u && (xb & 0x007fffffu) != 0) {
        if (!(xb & 0x00400000u)) {
            raise = SSE_XI;
            rb = xb | 0x00400000u;
            memcpy(&r, &rb, 4);
        }
    } else if (rb != xb && !(imm & 8))
        raise = SSE_XP;
    cvt_fp_flags_set(parked);
    hl_x86_sse_raise(raise);
    return r;
}

int sat_s16(int v) {
    return v < -32768 ? -32768 : v > 32767 ? 32767 : v;
}

static inline int sat_u16(int v) {
    return v < 0 ? 0 : v > 65535 ? 65535 : v;
}

// Read the 16-byte r/m operand (xmm register or m128) of a legacy SSE insn.
static void sse_get_rm(const hl_x86_avx_state *state, struct cpu *c, struct insn *I, uint64_t next, uint8_t buf[16]) {
    if (I->is_mem)
        (void)avx_memory_read(state, avx_ea(state, c, I, next, 16), buf, 16);
    else
        memcpy(buf, &c->v[2 * I->rm_reg], 16);
}

// ---- SSE4.2 packed string compare core (PCMP{I,E}STR{I,M}) --------------------------------------
// imm8 control byte: [0]=word(1)/byte(0) elements, [1]=signed, [3:2]=aggregation (0 equal-any,
// 1 ranges, 2 equal-each, 3 equal-ordered), [4]=negate polarity, [5]=negate valid positions only,
// [6]=index lsb(0)/msb(1) OR mask bit(0)/element-wide(1). a=operand1 (reg/xmm1), b=operand2 (r/m);
// la/lb are the element lengths of a/b (implicit: first-null scan; explicit: EAX/EDX, saturated).
static int64_t sse42_elem(const uint8_t *p, int i, int wordsz, int sgn) {
    if (wordsz == 1) return sgn ? (int64_t)(int8_t)p[i] : (int64_t)p[i];
    uint16_t w;
    memcpy(&w, p + 2 * i, 2);
    return sgn ? (int64_t)(int16_t)w : (int64_t)w;
}

// Implicit length: index of the first null element, else n.
static int sse42_ilen(const uint8_t *p, int wordsz, int n) {
    for (int i = 0; i < n; i++) {
        uint64_t e = 0;
        memcpy(&e, p + i * wordsz, (size_t)wordsz);
        if (e == 0) return i;
    }
    return n;
}

// Explicit length (PCMPESTR*): abs value of the GPR, saturated to the element count.
static int sse42_elen(int64_t v, int n) {
    if (v < 0) v = -v;
    return (v > n) ? n : (int)v;
}

// Compute the polarity-adjusted intermediate mask (IntRes2) over n elements per the imm8 control.
static int sse42_intres(const uint8_t *a, const uint8_t *b, int la, int lb, int imm, int n) {
    int wordsz = (imm & 1) ? 2 : 1;
    int sgn = (imm >> 1) & 1;
    int agg = (imm >> 2) & 3;
    int res = 0;
    for (int i = 0; i < n; i++) {
        int bit = 0;
        switch (agg) {
        case 0: // equal-any: b[i] occurs anywhere in the set a[0..la)
            if (i < lb)
                for (int j = 0; j < la; j++)
                    if (sse42_elem(a, j, wordsz, sgn) == sse42_elem(b, i, wordsz, sgn)) {
                        bit = 1;
                        break;
                    }
            break;
        case 1: // ranges: a[] holds [lo,hi] pairs; test lo <= b[i] <= hi
            if (i < lb) {
                int64_t bi = sse42_elem(b, i, wordsz, sgn);
                for (int j = 0; j + 1 < la; j += 2)
                    if (sse42_elem(a, j, wordsz, sgn) <= bi && bi <= sse42_elem(a, j + 1, wordsz, sgn)) {
                        bit = 1;
                        break;
                    }
            }
            break;
        case 2: // equal-each: a[i]==b[i], with both-invalid forced equal
            if (i < la && i < lb)
                bit = (sse42_elem(a, i, wordsz, sgn) == sse42_elem(b, i, wordsz, sgn));
            else if (i >= la && i >= lb)
                bit = 1;
            break;
        case 3: // equal-ordered: needle a[0..la) matches b starting at position i
            // The inner loop is bounded by the ELEMENT COUNT (k = i+j < n), not by la: the SDM's
            // per-element validity table decides the rest, and it is NOT symmetric --
            //   a valid,   b valid   -> compare
            //   a valid,   b invalid -> force FALSE   (needle runs off the end of the haystack)
            //   a invalid, b either  -> force TRUE    (needle exhausted -> the match stands)
            // Bounding the loop by `j < la` instead evaluated a k that had walked past the last
            // element, so a needle reaching the end of a full (null-free) operand2 -- e.g. the
            // all-ones vector a `cmppd` leaves behind -- was wrongly reported as a mismatch.
            bit = 1;
            for (int j = 0; i + j < n; j++) {
                int k = i + j;
                if (j >= la) break; // a invalid -> forced TRUE
                if (k >= lb) {      // a valid, b invalid -> FALSE
                    bit = 0;
                    break;
                }
                if (sse42_elem(a, j, wordsz, sgn) != sse42_elem(b, k, wordsz, sgn)) {
                    bit = 0;
                    break;
                }
            }
            break;
        }
        if (bit) res |= (1 << i);
    }
    if ((imm >> 4) & 1) { // negate polarity
        if ((imm >> 5) & 1)
            res ^= ((1 << lb) - 1); // masked: negate only valid operand2 positions
        else
            res ^= ((1 << n) - 1);
    }
    return res;
}

// SSE4.2 string-compare flags (Intel SDM): CF=(IntRes2!=0), ZF=operand2 has a null (lb<n),
// SF=operand1 has a null (la<n), OF=IntRes2[0], AF=PF=0. glibc's SSE4.2 strlen/strchr/strstr branch
// on these (jbe/ja/jc/jz) right after the op, so they MUST be set. Substrate: x86 CF = NOT stored-C.
static void sse42_flags(struct cpu *c, int res, int la, int lb, int n) {
    int cf = (res != 0), zf = (lb < n), sf = (la < n), of = (res & 1);
    c->nzcv = ((uint64_t)sf << 31) | ((uint64_t)zf << 30) | ((uint64_t)(!cf) << 29) | ((uint64_t)of << 28);
    c->pf = 1; // PF source byte with odd popcount -> x86 PF=0
    c->af = 0; // Intel SDM: PCMP*STR* clears AF
}

// Index output (PCMP*STRI) -> ECX: first/last set bit of IntRes2 (imm[6] picks lsb/msb), else n.
static void sse42_index(struct cpu *c, int res, int imm, int n) {
    int idx;
    if (res == 0)
        idx = n;
    else if ((imm >> 6) & 1)
        idx = 31 - __builtin_clz((unsigned)res);
    else
        idx = __builtin_ctz((unsigned)res);
    c->r[RCX] = (uint64_t)idx;
}

// Mask output (PCMP*STRM) -> XMM0. imm[6]=0: bit mask zero-extended; 1: element-wide (per-element) mask.
static void sse42_mask(struct cpu *c, int res, int imm, int n) {
    int wordsz = (imm & 1) ? 2 : 1;
    uint8_t out[16];
    memset(out, 0, 16);
    if ((imm >> 6) & 1) {
        for (int i = 0; i < n; i++)
            if (res & (1 << i)) memset(out + i * wordsz, 0xFF, (size_t)wordsz);
    } else {
        out[0] = (uint8_t)(res & 0xFF);
        if (n > 8) out[1] = (uint8_t)((res >> 8) & 0xFF);
    }
    memcpy(&c->v[0], out, 16); // XMM0 == c->v[0..1]; legacy SSE leaves the upper YMM bits intact
}

static enum avx_dispatch_result sse_dispatch_string_compare(const hl_x86_avx_state *state, struct cpu *c,
                                                            struct insn *I, uint64_t next, const uint8_t operand1[16]) {
    int op = I->op;
    if (I->map3 != 3 || (op != 0x60 && op != 0x61 && op != 0x62 && op != 0x63)) return AVX_DISPATCH_UNMATCHED;

    uint8_t operand2[16];
    sse_get_rm(state, c, I, next, operand2);
    int immediate = (int)I->imm;
    int element_width = (immediate & 1) ? 2 : 1;
    int elements = 16 / element_width;
    int operand1_length;
    int operand2_length;
    if (op == 0x60 || op == 0x61) {
        operand1_length = sse42_elen(I->rexW ? (int64_t)c->r[RAX] : (int32_t)c->r[RAX], elements);
        operand2_length = sse42_elen(I->rexW ? (int64_t)c->r[RDX] : (int32_t)c->r[RDX], elements);
    } else {
        operand1_length = sse42_ilen(operand1, element_width, elements);
        operand2_length = sse42_ilen(operand2, element_width, elements);
    }
    int result = sse42_intres(operand1, operand2, operand1_length, operand2_length, immediate, elements);
    if (op == 0x60 || op == 0x62)
        sse42_mask(c, result, immediate, elements);
    else
        sse42_index(c, result, immediate, elements);
    sse42_flags(c, result, operand1_length, operand2_length, elements);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_floating(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                      uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->two && I->map3 == 0 && (op == 0x58 || op == 0x59 || op == 0x5C || op == 0x5E)) {
        int packed = !I->repne && !I->rep;
        int dbl = packed ? I->p66 : I->repne;
        int element_size = dbl ? 8 : 4;
        uint8_t operand[64];
        avx_get_rm(state, c, I, next, packed ? 16 : element_size, operand);
        int bytes = packed ? 16 : element_size;
        for (int offset = 0; offset < bytes; offset += element_size) {
            if (dbl) {
                double x, y;
                memcpy(&x, destination + offset, 8);
                memcpy(&y, operand + offset, 8);
                double result = avx_fp_arith_f64(op, x, y);
                memcpy(destination + offset, &result, 8);
            } else {
                float x, y;
                memcpy(&x, destination + offset, 4);
                memcpy(&y, operand + offset, 4);
                float result = avx_fp_arith_f32(op, x, y);
                memcpy(destination + offset, &result, 4);
            }
        }
        c->rip = next;
        return AVX_DISPATCH_HANDLED;
    }
    if (!(I->two && I->map3 == 0 && (op == 0x7C || op == 0x7D || op == 0xD0))) return AVX_DISPATCH_UNMATCHED;

    int dbl = I->p66 != 0;
    int subtract = op == 0x7D;
    uint8_t operand[64], output[16];
    avx_get_rm(state, c, I, next, 16, operand);
    if (dbl) {
        double x[2], y[2], result[2];
        memcpy(x, destination, 16);
        memcpy(y, operand, 16);
        if (op == 0xD0) {
            result[0] = avx_dnan_f64(x[0] - y[0], x[0], y[0]);
            result[1] = avx_dnan_f64(x[1] + y[1], x[1], y[1]);
        } else {
            result[0] = subtract ? avx_dnan_f64(x[0] - x[1], x[0], x[1]) : avx_dnan_f64(x[0] + x[1], x[0], x[1]);
            result[1] = subtract ? avx_dnan_f64(y[0] - y[1], y[0], y[1]) : avx_dnan_f64(y[0] + y[1], y[0], y[1]);
        }
        memcpy(output, result, 16);
    } else {
        float x[4], y[4], result[4];
        memcpy(x, destination, 16);
        memcpy(y, operand, 16);
        if (op == 0xD0) {
            for (int i = 0; i < 4; i++)
                result[i] = (i & 1) ? avx_dnan_f32(x[i] + y[i], x[i], y[i]) : avx_dnan_f32(x[i] - y[i], x[i], y[i]);
        } else {
            result[0] = subtract ? avx_dnan_f32(x[0] - x[1], x[0], x[1]) : avx_dnan_f32(x[0] + x[1], x[0], x[1]);
            result[1] = subtract ? avx_dnan_f32(x[2] - x[3], x[2], x[3]) : avx_dnan_f32(x[2] + x[3], x[2], x[3]);
            result[2] = subtract ? avx_dnan_f32(y[0] - y[1], y[0], y[1]) : avx_dnan_f32(y[0] + y[1], y[0], y[1]);
            result[3] = subtract ? avx_dnan_f32(y[2] - y[3], y[2], y[3]) : avx_dnan_f32(y[2] + y[3], y[2], y[3]);
        }
        memcpy(output, result, 16);
    }
    memcpy(destination, output, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_crc_movbe(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                       uint64_t next) {
    int op = I->op;
    if (I->map3 != 2 || (op != 0xF0 && op != 0xF1)) return AVX_DISPATCH_UNMATCHED;

    if (I->repne) { // CRC32 r, r/m
        int bytes = op == 0xF0 ? 1 : I->opsize;
        uint64_t value;
        if (I->is_mem) {
            uint64_t address = avx_ea(state, c, I, next, bytes);
            value = 0;
            (void)avx_memory_read(state, address, &value, (size_t)bytes);
        } else if (bytes == 1 && !I->has_rex && I->rm_reg >= 4 && I->rm_reg <= 7) {
            value = (c->r[I->rm_reg - 4] >> 8) & 0xff; // AH/CH/DH/BH without REX
        } else {
            value = c->r[I->rm_reg];
        }
        c->r[I->reg] = crc32c_step((uint32_t)c->r[I->reg], value, bytes);
    } else { // MOVBE: byte-swapping memory load/store
        int bytes = I->opsize;
        uint64_t address = avx_ea(state, c, I, next, bytes);
        uint64_t value = 0;
        if (op == 0xF0)
            (void)avx_memory_read(state, address, &value, (size_t)bytes);
        else
            value = c->r[I->reg];
        uint64_t swapped = 0;
        for (int index = 0; index < bytes; index++)
            swapped |= ((value >> (8 * index)) & 0xff) << (8 * (bytes - 1 - index));
        if (op == 0xF1) {
            (void)avx_memory_write(state, address, &swapped, (size_t)bytes);
        } else if (bytes == 2) {
            c->r[I->reg] = (c->r[I->reg] & ~0xffffull) | (swapped & 0xffff);
        } else {
            c->r[I->reg] = swapped;
        }
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_lane_transfer(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                           uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 3) return AVX_DISPATCH_UNMATCHED;

    if (op == 0x14 || op == 0x15 || op == 0x16 || op == 0x17) { // PEXTR* / EXTRACTPS
        uint8_t immediate = (uint8_t)I->imm;
        uint64_t value;
        int width;
        if (op == 0x14) {
            width = 1;
            value = destination[immediate & 15];
        } else if (op == 0x15) {
            uint16_t word;
            width = 2;
            memcpy(&word, destination + 2 * (immediate & 7), 2);
            value = word;
        } else if (op == 0x16) {
            width = I->rexW ? 8 : 4;
            memcpy(&value, destination + (I->rexW ? 8 * (immediate & 1) : 4 * (immediate & 3)), (size_t)width);
        } else {
            uint32_t word;
            width = 4;
            memcpy(&word, destination + 4 * (immediate & 3), 4);
            value = word;
        }
        if (I->is_mem) {
            uint64_t address = avx_ea(state, c, I, next, width);
            (void)avx_memory_write(state, address, &value, (size_t)width);
        } else if (width == 8) {
            c->r[I->rm_reg] = value;
        } else {
            c->r[I->rm_reg] = (uint32_t)value;
        }
        c->rip = next;
        return AVX_DISPATCH_HANDLED;
    }

    if (op != 0x20 && op != 0x21 && op != 0x22) return AVX_DISPATCH_UNMATCHED;

    int immediate = (int)I->imm;
    if (op == 0x20) { // PINSRB
        uint8_t value = (uint8_t)c->r[I->rm_reg];
        if (I->is_mem) (void)avx_memory_read(state, avx_ea(state, c, I, next, 1), &value, 1);
        destination[immediate & 15] = value;
    } else if (op == 0x22) { // PINSRD / PINSRQ
        if (I->rexW) {
            uint64_t value = c->r[I->rm_reg];
            if (I->is_mem) (void)avx_memory_read(state, avx_ea(state, c, I, next, 8), &value, 8);
            memcpy(destination + 8 * (immediate & 1), &value, 8);
        } else {
            uint32_t value = (uint32_t)c->r[I->rm_reg];
            if (I->is_mem) (void)avx_memory_read(state, avx_ea(state, c, I, next, 4), &value, 4);
            memcpy(destination + 4 * (immediate & 3), &value, 4);
        }
    } else { // INSERTPS
        uint32_t source;
        if (I->is_mem)
            (void)avx_memory_read(state, avx_ea(state, c, I, next, 4), &source, 4);
        else
            memcpy(&source, (uint8_t *)&c->v[2 * I->rm_reg] + 4 * ((immediate >> 6) & 3), 4);
        memcpy(destination + 4 * ((immediate >> 4) & 3), &source, 4);
        for (int lane = 0; lane < 4; lane++)
            if (immediate & (1 << lane)) memset(destination + 4 * lane, 0, 4);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_integer_extension(const hl_x86_avx_state *state, struct cpu *c,
                                                               struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    int sign_extend = op >= 0x20 && op <= 0x25;
    int zero_extend = op >= 0x30 && op <= 0x35;
    if (I->map3 != 2 || (!sign_extend && !zero_extend)) return AVX_DISPATCH_UNMATCHED;

    uint8_t source[16];
    uint8_t result[16];
    sse_get_rm(state, c, I, next, source);
    if (sign_extend) {
        int8_t bytes[16];
        int16_t words[8];
        int32_t dwords[4];
        memcpy(bytes, source, 16);
        memcpy(words, source, 16);
        memcpy(dwords, source, 16);
        if (op == 0x20) {
            int16_t output[8];
            for (int index = 0; index < 8; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x21) {
            int32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x22) {
            int64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x23) {
            int32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else if (op == 0x24) {
            int64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else {
            int64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = dwords[index];
            memcpy(result, output, 16);
        }
    } else {
        uint8_t bytes[16];
        uint16_t words[8];
        uint32_t dwords[4];
        memcpy(bytes, source, 16);
        memcpy(words, source, 16);
        memcpy(dwords, source, 16);
        if (op == 0x30) {
            uint16_t output[8];
            for (int index = 0; index < 8; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x31) {
            uint32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x32) {
            uint64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x33) {
            uint32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else if (op == 0x34) {
            uint64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else {
            uint64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = dwords[index];
            memcpy(result, output, 16);
        }
    }
    memcpy(destination, result, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_packed_arithmetic(const hl_x86_avx_state *state, struct cpu *c,
                                                               struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || op < 0x01 || op > 0x0B) return AVX_DISPATCH_UNMATCHED;

    uint8_t source[16];
    uint8_t result[16];
    sse_get_rm(state, c, I, next, source);
    if (op == 0x04) { // PMADDUBSW
        int16_t output[8];
        for (int lane = 0; lane < 8; lane++) {
            int product = (int)(uint8_t)destination[2 * lane] * (int)(int8_t)source[2 * lane] +
                          (int)(uint8_t)destination[2 * lane + 1] * (int)(int8_t)source[2 * lane + 1];
            output[lane] = (int16_t)sat_s16(product);
        }
        memcpy(result, output, 16);
    } else if (op <= 0x07) { // PHADD / PHSUB
        int subtract = op >= 0x05;
        int saturate = op == 0x03 || op == 0x07;
        if (op == 0x02 || op == 0x06) {
            int32_t left[4], right[4], output[4];
            memcpy(left, destination, 16);
            memcpy(right, source, 16);
            output[0] = subtract ? left[0] - left[1] : left[0] + left[1];
            output[1] = subtract ? left[2] - left[3] : left[2] + left[3];
            output[2] = subtract ? right[0] - right[1] : right[0] + right[1];
            output[3] = subtract ? right[2] - right[3] : right[2] + right[3];
            memcpy(result, output, 16);
        } else {
            int16_t left[8], right[8], output[8];
            memcpy(left, destination, 16);
            memcpy(right, source, 16);
            for (int lane = 0; lane < 4; lane++) {
                int left_value = subtract ? left[2 * lane] - left[2 * lane + 1] : left[2 * lane] + left[2 * lane + 1];
                int right_value =
                    subtract ? right[2 * lane] - right[2 * lane + 1] : right[2 * lane] + right[2 * lane + 1];
                output[lane] = saturate ? (int16_t)sat_s16(left_value) : (int16_t)left_value;
                output[lane + 4] = saturate ? (int16_t)sat_s16(right_value) : (int16_t)right_value;
            }
            memcpy(result, output, 16);
        }
    } else if (op <= 0x0A) { // PSIGNB / PSIGNW / PSIGND
        int element_width = op == 0x08 ? 1 : op == 0x09 ? 2 : 4;
        for (int offset = 0; offset < 16; offset += element_width) {
            uint64_t value = 0;
            uint64_t control = 0;
            memcpy(&value, destination + offset, (size_t)element_width);
            memcpy(&control, source + offset, (size_t)element_width);
            uint64_t output = simd_element_negative(control, element_width) ? simd_element_negate(value, element_width)
                              : control == 0                                ? 0
                                                                            : value;
            memcpy(result + offset, &output, (size_t)element_width);
        }
    } else { // PMULHRSW
        int16_t left[8], right[8], output[8];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 8; lane++)
            output[lane] = (int16_t)((((left[lane] * right[lane]) >> 14) + 1) >> 1);
        memcpy(result, output, 16);
    }
    memcpy(destination, result, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static uint32_t aes_subword(uint32_t value) {
    return (uint32_t)k_aes_sbox[value & 0xff] | ((uint32_t)k_aes_sbox[(value >> 8) & 0xff] << 8) |
           ((uint32_t)k_aes_sbox[(value >> 16) & 0xff] << 16) | ((uint32_t)k_aes_sbox[(value >> 24) & 0xff] << 24);
}

static enum avx_dispatch_result sse_dispatch_aes(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                 uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    int round = I->map3 == 2 && op >= 0xDB && op <= 0xDF;
    int keygen = I->map3 == 3 && op == 0xDF;
    if (!round && !keygen) return AVX_DISPATCH_UNMATCHED;

    uint8_t source[16];
    uint8_t result[16];
    sse_get_rm(state, c, I, next, source);
    if (keygen) {
        uint32_t words[4];
        uint32_t output[4];
        memcpy(words, source, 16);
        uint32_t subword1 = aes_subword(words[1]);
        uint32_t subword3 = aes_subword(words[3]);
        uint32_t round_constant = (uint32_t)((uint8_t)I->imm);
        output[0] = subword1;
        output[1] = rotr32(subword1, 8) ^ round_constant;
        output[2] = subword3;
        output[3] = rotr32(subword3, 8) ^ round_constant;
        memcpy(result, output, 16);
    } else if (op == 0xDB) { // AESIMC
        memcpy(result, source, 16);
        aes_mixcolumns(result, 1);
    } else {
        int inverse = op == 0xDE || op == 0xDF;
        uint8_t transformed[16];
        aes_shiftrows(destination, transformed, inverse);
        aes_subbytes(transformed, inverse ? k_aes_isbox : k_aes_sbox);
        if (op == 0xDC || op == 0xDE) aes_mixcolumns(transformed, inverse);
        for (int byte = 0; byte < 16; byte++)
            result[byte] = transformed[byte] ^ source[byte];
    }
    memcpy(destination, result, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_test(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                  uint64_t next, const uint8_t destination[16]) {
    if (I->map3 != 2 || I->op != 0x17) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    uint64_t destination_low, destination_high, source_low, source_high;
    sse_get_rm(state, c, I, next, source);
    memcpy(&destination_low, destination, 8);
    memcpy(&destination_high, destination + 8, 8);
    memcpy(&source_low, source, 8);
    memcpy(&source_high, source + 8, 8);
    int zero = (destination_low & source_low) == 0 && (destination_high & source_high) == 0;
    int carry = (source_low & ~destination_low) == 0 && (source_high & ~destination_high) == 0;
    c->nzcv = ((uint64_t)zero << 30) | ((uint64_t)(!carry) << 29);
    c->pf = 1;
    c->af = 0;
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_absolute(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                      uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || (op != 0x1C && op != 0x1D && op != 0x1E)) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    int element_width = op == 0x1C ? 1 : op == 0x1D ? 2 : 4;
    sse_get_rm(state, c, I, next, source);
    for (int offset = 0; offset < 16; offset += element_width) {
        uint64_t value = 0;
        memcpy(&value, source + offset, (size_t)element_width);
        uint64_t result =
            simd_element_negative(value, element_width) ? simd_element_negate(value, element_width) : value;
        memcpy(destination + offset, &result, (size_t)element_width);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_minmax(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                    uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || op < 0x38 || op > 0x3F) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    sse_get_rm(state, c, I, next, source);
    int maximum = op >= 0x3C;
    if (op == 0x38 || op == 0x3C) {
        int8_t *left = (int8_t *)destination;
        int8_t *right = (int8_t *)source;
        for (int lane = 0; lane < 16; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
    } else if (op == 0x3A || op == 0x3E) {
        uint16_t left[8], right[8];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 8; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
        memcpy(destination, left, 16);
    } else if (op == 0x39 || op == 0x3D) {
        int32_t left[4], right[4];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 4; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
        memcpy(destination, left, 16);
    } else {
        uint32_t left[4], right[4];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 4; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
        memcpy(destination, left, 16);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_implicit_blend(const hl_x86_avx_state *state, struct cpu *c,
                                                            struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || (op != 0x10 && op != 0x14 && op != 0x15)) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    uint8_t mask[16];
    sse_get_rm(state, c, I, next, source);
    memcpy(mask, &c->v[0], 16);
    if (op == 0x10) {
        for (int lane = 0; lane < 16; lane++)
            if (mask[lane] & 0x80) destination[lane] = source[lane];
    } else {
        int lane_width = op == 0x14 ? 4 : 8;
        for (int offset = 0; offset < 16; offset += lane_width)
            if (mask[offset + lane_width - 1] & 0x80) memcpy(destination + offset, source + offset, (size_t)lane_width);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_immediate_blend(const hl_x86_avx_state *state, struct cpu *c,
                                                             struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 3 || op < 0x0C || op > 0x0E) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    unsigned immediate = (uint8_t)I->imm;
    int lane_width = op == 0x0C ? 4 : op == 0x0D ? 8 : 2;
    sse_get_rm(state, c, I, next, source);
    for (int lane = 0; lane < 16 / lane_width; lane++)
        if (immediate & (1u << lane))
            memcpy(destination + lane * lane_width, source + lane * lane_width, (size_t)lane_width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static void sse_sha1_rounds4(uint8_t imm, const uint8_t state_bytes[16], const uint8_t word_bytes[16],
                             uint8_t result[16]) {
    uint32_t function = imm & 3;
    uint32_t constant = (function == 0)   ? 0x5A827999u
                        : (function == 1) ? 0x6ED9EBA1u
                        : (function == 2) ? 0x8F1BBCDCu
                                          : 0xCA62C1D6u;
    uint32_t state[4], words[4];
    memcpy(state, state_bytes, 16);
    memcpy(words, word_bytes, 16);
    uint32_t a = state[3], b = state[2], c = state[1], d = state[0];
    uint32_t schedule[4] = {words[3], words[2], words[1], words[0]};
    uint32_t e = 0;
    for (int i = 0; i < 4; i++) {
        uint32_t f = (function == 0)   ? ((b & c) | (~b & d))
                     : (function == 2) ? ((b & c) | (b & d) | (c & d))
                                       : (b ^ c ^ d);
        uint32_t next = f + rotl32(a, 5) + schedule[i] + constant + e;
        e = d;
        d = c;
        c = rotl32(b, 30);
        b = a;
        a = next;
    }
    uint32_t output[4] = {d, c, b, a};
    memcpy(result, output, 16);
}

static enum avx_dispatch_result sse_dispatch_immediate_arithmetic(int op, uint8_t imm, const uint8_t destination[16],
                                                                  const uint8_t source[16], uint8_t result[16]) {
    switch (op) {
    case 0x08:
    case 0x09:
    case 0x0A:
    case 0x0B: {          // roundps/pd/ss/sd, mode in imm[3:0]; bit2 set = use MXCSR.RC (current host FPCR)
        if (op == 0x08) { // roundps
            float input[4], output[4];
            memcpy(input, source, 16);
            for (int i = 0; i < 4; i++)
                output[i] = sse_round_f(input[i], imm);
            memcpy(result, output, 16);
        } else if (op == 0x09) { // roundpd
            double input[2], output[2];
            memcpy(input, source, 16);
            for (int i = 0; i < 2; i++)
                output[i] = sse_round_d(input[i], imm);
            memcpy(result, output, 16);
        } else if (op == 0x0A) { // roundss: low lane from src, rest from dst
            float input;
            memcpy(&input, source, 4);
            input = sse_round_f(input, imm);
            memcpy(result, &input, 4);
        } else { // roundsd
            double input;
            memcpy(&input, source, 8);
            input = sse_round_d(input, imm);
            memcpy(result, &input, 8);
        }
        return AVX_DISPATCH_HANDLED;
    }
    case 0x0F: { // palignr: (dst:src) >> imm8 bytes
        uint8_t combined[32];
        memcpy(combined, source, 16);
        memcpy(combined + 16, destination, 16);
        for (int i = 0; i < 16; i++)
            result[i] = imm < (unsigned)(32 - i) ? combined[imm + (unsigned)i] : 0;
        return AVX_DISPATCH_HANDLED;
    }
    case 0x40: { // dpps: packed-single dot product
        float left[4], right[4];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        float sum = 0;
        for (int i = 0; i < 4; i++)
            if (imm & (0x10 << i)) sum += left[i] * right[i];
        float output[4];
        for (int i = 0; i < 4; i++)
            output[i] = (imm & (1 << i)) ? sum : 0.0f;
        memcpy(result, output, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0x41: { // dppd: packed-double dot product
        double left[2], right[2];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        double sum = 0;
        for (int i = 0; i < 2; i++)
            if (imm & (0x10 << i)) sum += left[i] * right[i];
        double output[2];
        for (int i = 0; i < 2; i++)
            output[i] = (imm & (1 << i)) ? sum : 0.0;
        memcpy(result, output, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0x42: { // mpsadbw: eight 4-byte sum-of-absolute-differences windows.
        int source_offset = (imm & 3) * 4;
        int destination_offset = ((imm >> 2) & 1) * 4;
        uint16_t output[8];
        for (int i = 0; i < 8; i++) {
            int sum = 0;
            for (int k = 0; k < 4; k++) {
                int difference = (int)destination[destination_offset + i + k] - (int)source[source_offset + k];
                sum += difference < 0 ? -difference : difference;
            }
            output[i] = (uint16_t)sum;
        }
        memcpy(result, output, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0x44: { // pclmulqdq: carryless multiply of selected 64-bit halves
        uint64_t left, right;
        memcpy(&left, destination + 8 * (imm & 1), 8);
        memcpy(&right, source + 8 * ((imm >> 4) & 1), 8);
// __int128: pre-C23 GNU/clang extension needed for the PCLMULQDQ carryless product; scope -Wpedantic.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
        unsigned __int128 product = 0;
        for (int i = 0; i < 64; i++)
            if ((right >> i) & 1) product ^= (unsigned __int128)left << i;
#pragma GCC diagnostic pop
        memcpy(result, &product, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0xCC: { // sha1rnds4: 4 SHA-1 rounds, function/constant from imm[1:0]
        sse_sha1_rounds4(imm, destination, source, result);
        return AVX_DISPATCH_HANDLED;
    }
    default: return AVX_DISPATCH_UNIMPLEMENTED;
    }
}

static enum avx_dispatch_result sse_dispatch_sha1_message(int op, const uint8_t destination[16],
                                                          const uint8_t source[16], uint8_t result[16]) {
    uint32_t left[4], right[4], output[4];
    memcpy(left, destination, 16);
    memcpy(right, source, 16);
    switch (op) {
    case 0xC8: // sha1nexte
        output[3] = right[3] + rotl32(left[3], 30);
        output[2] = right[2];
        output[1] = right[1];
        output[0] = right[0];
        break;
    case 0xC9: // sha1msg1
        output[3] = left[1] ^ left[3];
        output[2] = left[0] ^ left[2];
        output[1] = right[3] ^ left[1];
        output[0] = right[2] ^ left[0];
        break;
    case 0xCA: // sha1msg2
        output[3] = rotl32(left[3] ^ right[2], 1);
        output[2] = rotl32(left[2] ^ right[1], 1);
        output[1] = rotl32(left[1] ^ right[0], 1);
        output[0] = rotl32(left[0] ^ output[3], 1);
        break;
    default: return AVX_DISPATCH_UNMATCHED;
    }
    memcpy(result, output, 16);
    return AVX_DISPATCH_HANDLED;
}

void hl_x86_sse_execute(const hl_x86_avx_state *state, struct cpu *c) {
    struct insn I;
    hl_x86_decode(c->rip, &I);
    uint64_t next = c->rip + (uint64_t)I.len;
    int map = I.map3, op = I.op;
    uint8_t *D = (uint8_t *)&c->v[2 * I.reg]; // dst xmm == src1 (destructive)
    uint8_t s[16], r[16];

    if (sse_dispatch_floating(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_crc_movbe(state, c, &I, next) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_lane_transfer(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_string_compare(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;

    if (sse_dispatch_test(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;

    if (sse_dispatch_integer_extension(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_packed_arithmetic(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_aes(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_absolute(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_minmax(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_implicit_blend(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_immediate_blend(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;

    // ---- the remaining ops are xmm-destructive: load the r/m source, compute into r, write to D -----
    sse_get_rm(state, c, &I, next, s);
    memcpy(r, D, 16);

    if (map == 2) {
        if (sse_dispatch_sha1_message(op, D, s, r) == AVX_DISPATCH_HANDLED) {
            memcpy(D, r, 16);
            c->rip = next;
            return;
        }
        switch (op) {
        case 0x00: { // pshufb
            uint8_t t[16];
            memcpy(t, D, 16);
            for (int i = 0; i < 16; i++)
                r[i] = (s[i] & 0x80) ? 0 : t[s[i] & 0x0f];
            break;
        }
        case 0x28: { // pmuldq: signed (a.dword[0]*s.dword[0], a.dword[2]*s.dword[2]) -> 2 qwords
            int32_t a[4], b[4];
            int64_t o[2];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            o[0] = (int64_t)a[0] * (int64_t)b[0];
            o[1] = (int64_t)a[2] * (int64_t)b[2];
            memcpy(r, o, 16);
            break;
        }
        case 0x29: { // pcmpeqq
            uint64_t a[2], b[2], o[2];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            o[0] = (a[0] == b[0]) ? ~0ull : 0;
            o[1] = (a[1] == b[1]) ? ~0ull : 0;
            memcpy(r, o, 16);
            break;
        }
        case 0x2A: { // movntdqa (66): streaming aligned load m128 -> xmm reg (src already in s)
            memcpy(r, s, 16);
            break;
        }
        case 0x2B: { // packusdw: pack signed dword -> unsigned word (saturate); dst low, src high
            int32_t a[4], b[4];
            uint16_t o[8];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            for (int i = 0; i < 4; i++)
                o[i] = (uint16_t)sat_u16(a[i]);
            for (int i = 0; i < 4; i++)
                o[i + 4] = (uint16_t)sat_u16(b[i]);
            memcpy(r, o, 16);
            break;
        }
        case 0x37: { // pcmpgtq (signed)
            int64_t a[2], b[2];
            uint64_t o[2];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            o[0] = (a[0] > b[0]) ? ~0ull : 0;
            o[1] = (a[1] > b[1]) ? ~0ull : 0;
            memcpy(r, o, 16);
            break;
        }
        case 0x40: { // pmulld: 32-bit low product
            int32_t a[4], b[4], o[4];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            for (int i = 0; i < 4; i++)
                o[i] = a[i] * b[i];
            memcpy(r, o, 16);
            break;
        }
        case 0x41: { // phminposuw: single source r/m -> word0 = min unsigned word, word1 = its
            // (lowest) index, words2..7 = 0. dst src1(D) is ignored.
            uint16_t w[8];
            memcpy(w, s, 16);
            uint16_t best = w[0];
            int idx = 0;
            for (int i = 1; i < 8; i++)
                if (w[i] < best) {
                    best = w[i];
                    idx = i;
                }
            uint16_t o[8] = {best, (uint16_t)idx, 0, 0, 0, 0, 0, 0};
            memcpy(r, o, 16);
            break;
        }
        case 0xCB: { // sha256rnds2: dst,src, implicit xmm0 = WK0/WK1
            uint32_t st1[4], st2[4], wk[4];
            memcpy(st1, D, 16); // C0=st1[3],D0=st1[2],G0=st1[1],H0=st1[0]
            memcpy(st2, s, 16); // A0=st2[3],B0=st2[2],E0=st2[1],F0=st2[0]
            memcpy(wk, &c->v[0], 16);
            uint32_t A = st2[3], B = st2[2], Cc = st1[3], Dd = st1[2];
            uint32_t E = st2[1], F = st2[0], G = st1[1], H = st1[0];
            for (int i = 0; i < 2; i++) {
                uint32_t WK = wk[i];
                uint32_t s1 = rotr32(E, 6) ^ rotr32(E, 11) ^ rotr32(E, 25);
                uint32_t ch = (E & F) ^ (~E & G);
                uint32_t s0 = rotr32(A, 2) ^ rotr32(A, 13) ^ rotr32(A, 22);
                uint32_t maj = (A & B) ^ (A & Cc) ^ (B & Cc);
                uint32_t t1 = H + s1 + ch + WK;
                uint32_t An = t1 + s0 + maj;
                uint32_t En = t1 + Dd;
                H = G;
                G = F;
                F = E;
                E = En;
                Dd = Cc;
                Cc = B;
                B = A;
                A = An;
            }
            uint32_t o[4] = {F, E, B, A}; // DEST: [31:0]=F2,[63:32]=E2,[95:64]=B2,[127:96]=A2
            memcpy(r, o, 16);
            break;
        }
        case 0xCC: { // sha256msg1
            uint32_t w[4], w4;
            memcpy(w, D, 16);  // W0=w[0]..W3=w[3]
            memcpy(&w4, s, 4); // W4 = src[31:0]
            uint32_t in[5] = {w[0], w[1], w[2], w[3], w4};
            uint32_t o[4];
            for (int i = 0; i < 4; i++) {
                uint32_t x = in[i + 1];
                uint32_t s0 = rotr32(x, 7) ^ rotr32(x, 18) ^ (x >> 3);
                o[i] = in[i] + s0;
            }
            memcpy(r, o, 16);
            break;
        }
        case 0xCD: { // sha256msg2: W14=SRC2.dw2, W15=SRC2.dw3; W16..19 = SRC1.dw + sigma1(prev W)
            uint32_t Dw[4], sw[4], o[4];
            memcpy(Dw, D, 16);
            memcpy(sw, s, 16);
#define SHA_SIG1(x) (rotr32((x), 17) ^ rotr32((x), 19) ^ ((x) >> 10))
            uint32_t W16 = Dw[0] + SHA_SIG1(sw[2]);
            uint32_t W17 = Dw[1] + SHA_SIG1(sw[3]);
            uint32_t W18 = Dw[2] + SHA_SIG1(W16);
            uint32_t W19 = Dw[3] + SHA_SIG1(W17);
#undef SHA_SIG1
            o[0] = W16;
            o[1] = W17;
            o[2] = W18;
            o[3] = W19;
            memcpy(r, o, 16);
            break;
        }
        default: goto unimpl;
        }
        memcpy(D, r, 16);
        c->rip = next;
        return;
    }

    // ---- map == 3 (0F3A), the xmm-destructive imm8 forms ------------------------------------------
    if (sse_dispatch_immediate_arithmetic(op, (uint8_t)I.imm, D, s, r) == AVX_DISPATCH_HANDLED) {
        memcpy(D, r, 16);
        c->rip = next;
        return;
    }

unimpl:
    if (!g_sse_warned) {
        g_sse_warned = 1;
        fprintf(stderr, "[sse3b] UNIMPLEMENTED map=%d op=0x%02x p66=%d rep=%d repne=%d rip=%llx\n", map, op, I.p66,
                I.rep, I.repne, (unsigned long long)c->rip);
    }
    c->exited = 1;
    c->exit_code = 70;
}
