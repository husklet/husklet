// 0F38/0F3A GPR + lane glue lowered inline (AES-GCM endgame, perf wave 2) -- the byte-exact
// differential + KAT for translate/sse4x.c and the constant-hoist/PMULL2/PSHUFD upgrades:
//   MOVBE   load + store, 16/32/64 (incl. the 16-bit dest MERGE into the full GPR)
//   CRC32   8/16/32/64, reg + mem sources, incl. an AH-register (high-byte, no-REX) source
//   PEXTRB/W/D/Q + EXTRACTPS   every lane, reg + MEMORY destinations
//   PINSRB/D/Q                 every lane, reg + MEMORY sources
//   INSERTPS                   countS x countD sweeps, zmask edges (0, 0xF, dst-in-zmask), mem form
//   AESKEYGENASSIST            rcon edges incl. 0x00
//   PCLMULQDQ                  all four imm forms 0x00/0x01/0x10/0x11 (PMULL2 lowering)
//   PSHUFD                     the special-cased imms (E4/4E/B1/00/55/AA/FF) + general, src==dst + mem
//   PSLLDQ/PSRLDQ              0/1/8/15 and the >15 all-zero edge
//   AESENC/AESDEC runs         back-to-back chains (v26 zero-key hoist), state==key alias, interleaved
//                              pshufb (v27 mask hoist), rep-movsb between rounds (host-call claim clear),
//                              SHA256 rounds between AES ops (v20..v31 clobber claim clear)
// Every result is printed as hex; stdout is byte-compared jit-vs-qemu (oracle differential).
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <immintrin.h>
#include <x86intrin.h>
#include <tmmintrin.h>
#include <smmintrin.h>
#include <wmmintrin.h>

static void ph(const char *t, __m128i v) {
    uint8_t b[16]; _mm_storeu_si128((__m128i *)b, v);
    printf("%s ", t); for (int i = 0; i < 16; i++) printf("%02x", b[i]); printf("\n");
}
static void pq(const char *t, unsigned long long v) { printf("%s %016llx\n", t, v); }

static const uint64_t vv[][2] = {
    {0, 0}, {~0ULL, ~0ULL}, {0x1122334455667788ULL, 0x99aabbccddeeff00ULL},
    {0x8000000000000000ULL, 0x0000000000000001ULL}, {0xdeadbeefcafebabeULL, 0x0123456789abcdefULL},
    {0x807f017e02fd0380ULL, 0x00ff8081097f7e00ULL},
};
#define NV ((int)(sizeof(vv) / sizeof(vv[0])))

__attribute__((target("movbe"))) static void t_movbe(void) {
    struct { uint16_t w; uint32_t l; uint64_t q; } m;
    m.w = 0x1122; m.l = 0x11223344; m.q = 0x1122334455667788ULL;
    // loads: 16-bit dest MERGES into the low 16 of the 64-bit GPR (upper 48 preserved)
    uint64_t r = 0xa5a5a5a5a5a5a5a5ULL;
    __asm__("movbew %1, %w0" : "+r"(r) : "m"(m.w));
    pq("movbe.ld16", r);
    uint64_t r32 = 0xa5a5a5a5a5a5a5a5ULL;
    __asm__("movbel %1, %k0" : "+r"(r32) : "m"(m.l)); // 32-bit dest zero-extends
    pq("movbe.ld32", r32);
    uint64_t r64 = 0;
    __asm__("movbeq %1, %0" : "+r"(r64) : "m"(m.q));
    pq("movbe.ld64", r64);
    // stores
    struct { uint16_t w; uint32_t l; uint64_t q; } o; memset(&o, 0x77, sizeof o);
    uint64_t s = 0xfedcba9876543210ULL;
    __asm__("movbew %w1, %0" : "=m"(o.w) : "r"(s));
    __asm__("movbel %k1, %0" : "=m"(o.l) : "r"(s));
    __asm__("movbeq %1, %0" : "=m"(o.q) : "r"(s));
    pq("movbe.st16", o.w); pq("movbe.st32", o.l); pq("movbe.st64", o.q);
}

__attribute__((target("sse4.2"))) static void t_crc32(void) {
    uint32_t c = 0;
    for (int i = 0; i < NV; i++) {
        c = _mm_crc32_u8(c, (uint8_t)vv[i][0]);
        c = _mm_crc32_u16(c, (uint16_t)vv[i][0]);
        c = _mm_crc32_u32(c, (uint32_t)vv[i][1]);
        c = (uint32_t)_mm_crc32_u64(c, vv[i][1]);
    }
    pq("crc32.chain", c);
    // memory sources (all widths)
    uint64_t q = 0x123456789abcdef0ULL; uint32_t l = 0xcafebabe; uint16_t w = 0xbeef; uint8_t b = 0x5a;
    uint64_t cq = 0xffffffff;
    __asm__("crc32b %1, %k0" : "+r"(cq) : "m"(b));
    __asm__("crc32w %1, %k0" : "+r"(cq) : "m"(w));
    __asm__("crc32l %1, %k0" : "+r"(cq) : "m"(l));
    __asm__("crc32q %1, %0" : "+r"(cq) : "m"(q));
    pq("crc32.mem", cq);
    // AH source (high-byte register, no-REX encoding) + the REX.W r64 dest zero-extension
    uint64_t ch = 0x1111222233334444ULL;
    __asm__("movl $0x37bc0000, %%eax\n\tcrc32b %%ah, %k0" : "+r"(ch) : : "eax");
    pq("crc32.ah", ch);
}

__attribute__((target("sse4.1"))) static void t_pextr_pinsr(void) {
    for (int i = 0; i < NV; i++) {
        __m128i a = _mm_set_epi64x((long long)vv[i][1], (long long)vv[i][0]);
        // PEXTR reg dest, every width, first/last/middle lanes (zero-extension into r32/r64 is
        // visible through the 64-bit accumulate)
        uint64_t r = 0;
        r += (uint64_t)_mm_extract_epi8(a, 0) + (uint64_t)_mm_extract_epi8(a, 7) + (uint64_t)_mm_extract_epi8(a, 15);
        r += (uint64_t)_mm_extract_epi16(a, 0) + (uint64_t)_mm_extract_epi16(a, 3) + (uint64_t)_mm_extract_epi16(a, 7);
        r += (uint64_t)(uint32_t)_mm_extract_epi32(a, 0) + (uint64_t)(uint32_t)_mm_extract_epi32(a, 3);
        r += (uint64_t)_mm_extract_epi64(a, 0) + (uint64_t)_mm_extract_epi64(a, 1);
        pq("pextr.reg", r);
        // PEXTR memory dest (asm: the m8/m16/m32/m64 encodings)
        struct { uint8_t b; uint16_t w; uint32_t l; uint64_t q; uint32_t x; } o; memset(&o, 0xee, sizeof o);
        __asm__("pextrb $9, %1, %0" : "=m"(o.b) : "x"(a));
        __asm__("pextrw $5, %1, %0" : "=m"(o.w) : "x"(a));
        __asm__("pextrd $2, %1, %0" : "=m"(o.l) : "x"(a));
        __asm__("pextrq $1, %1, %0" : "=m"(o.q) : "x"(a));
        __asm__("extractps $3, %1, %0" : "=m"(o.x) : "x"(a));
        printf("pextr.mem %02x %04x %08x %016llx %08x\n", o.b, o.w, o.l, (unsigned long long)o.q, o.x);
        // EXTRACTPS to a GPR
        uint32_t xp; __asm__("extractps $1, %1, %0" : "=r"(xp) : "x"(a));
        pq("extractps.reg", xp);
        // PINSR from reg, every width, first/last lanes
        __m128i p = a;
        p = _mm_insert_epi8(p, 0x5a, 0); p = _mm_insert_epi8(p, (char)0x80, 15);
        p = _mm_insert_epi32(p, 0x13579bdf, 1); p = _mm_insert_epi32(p, (int)0xfeed0001, 3);
        p = _mm_insert_epi64(p, 0x0102030405060708LL, 0);
        ph("pinsr.reg", p);
        // PINSR from memory (asm m8/m32/m64 forms)
        uint8_t mb = 0xc3; uint32_t ml = 0x76543210; uint64_t mq = 0xf00dfacecafed00dULL;
        __m128i q2 = a;
        __asm__("pinsrb $3, %1, %0" : "+x"(q2) : "m"(mb));
        __asm__("pinsrd $2, %1, %0" : "+x"(q2) : "m"(ml));
        __asm__("pinsrq $1, %1, %0" : "+x"(q2) : "m"(mq));
        ph("pinsr.mem", q2);
    }
}

__attribute__((target("sse4.1"))) static void t_insertps(void) {
    for (int i = 0; i < NV; i++) {
        __m128 a = _mm_castsi128_ps(_mm_set_epi64x((long long)vv[i][1], (long long)vv[i][0]));
        __m128 b = _mm_castsi128_ps(_mm_set_epi64x((long long)vv[(i + 1) % NV][1], (long long)vv[(i + 2) % NV][0]));
        ph("insertps.00", _mm_castps_si128(_mm_insert_ps(a, b, 0x00)));   // b[0] -> a[0], no zero
        ph("insertps.d3", _mm_castps_si128(_mm_insert_ps(a, b, 0x30)));   // b[0] -> a[3]
        ph("insertps.s2d1", _mm_castps_si128(_mm_insert_ps(a, b, 0x90))); // b[2] -> a[1]
        ph("insertps.z5", _mm_castps_si128(_mm_insert_ps(a, b, 0xC5)));   // b[3] -> a[0], zero lanes 0,2
        ph("insertps.zf", _mm_castps_si128(_mm_insert_ps(a, b, 0x6F)));   // zmask 0xF: everything zeroed
        ph("insertps.dz", _mm_castps_si128(_mm_insert_ps(a, b, 0x14)));   // b[0] -> a[1], zmask hits OTHER lane 2
        ph("insertps.dzd", _mm_castps_si128(_mm_insert_ps(a, b, 0x12)));  // dst lane 1 itself in zmask -> zero wins
        // memory-operand form (m32 scalar; countS ignored)
        float mf; memcpy(&mf, &vv[i][0], 4);
        __m128 c = a;
        __asm__("insertps $0x20, %1, %0" : "+x"(c) : "m"(mf)); // m32 -> lane 2
        ph("insertps.mem", _mm_castps_si128(c));
        __m128 d = a;
        __asm__("insertps $0x2d, %1, %0" : "+x"(d) : "m"(mf)); // m32 -> lane 2, zero lanes 0,2,3
        ph("insertps.memz", _mm_castps_si128(d));
    }
}

__attribute__((target("aes,sse4.1"))) static void t_keygen(void) {
    for (int i = 0; i < NV; i++) {
        __m128i a = _mm_set_epi64x((long long)vv[i][1], (long long)vv[i][0]);
        ph("keygen.00", _mm_aeskeygenassist_si128(a, 0x00)); // rcon 0 edge
        ph("keygen.01", _mm_aeskeygenassist_si128(a, 0x01));
        ph("keygen.1b", _mm_aeskeygenassist_si128(a, 0x1b));
        ph("keygen.36", _mm_aeskeygenassist_si128(a, 0x36));
        ph("keygen.80", _mm_aeskeygenassist_si128(a, 0x80));
        ph("keygen.ff", _mm_aeskeygenassist_si128(a, 0xff));
    }
    // memory source form
    __m128i k;
    __asm__("aeskeygenassist $0x2a, %1, %0" : "=x"(k) : "m"(vv[2]));
    ph("keygen.mem", k);
}

__attribute__((target("pclmul"))) static void t_pclmul(void) {
    for (int i = 0; i < NV; i++) {
        __m128i a = _mm_set_epi64x((long long)vv[i][1], (long long)vv[i][0]);
        __m128i b = _mm_set_epi64x((long long)vv[(i + 3) % NV][1], (long long)vv[(i + 1) % NV][0]);
        ph("pclmul.00", _mm_clmulepi64_si128(a, b, 0x00));
        ph("pclmul.01", _mm_clmulepi64_si128(a, b, 0x01));
        ph("pclmul.10", _mm_clmulepi64_si128(a, b, 0x10));
        ph("pclmul.11", _mm_clmulepi64_si128(a, b, 0x11)); // PMULL2 single-insn lowering
        ph("pclmul.aa", _mm_clmulepi64_si128(a, a, 0x11)); // both operands aliased
    }
}

__attribute__((target("sse2"))) static void t_pshufd(void) {
    for (int i = 0; i < NV; i++) {
        __m128i a = _mm_set_epi64x((long long)vv[i][1], (long long)vv[i][0]);
        ph("pshufd.e4", _mm_shuffle_epi32(a, 0xE4)); // identity
        ph("pshufd.4e", _mm_shuffle_epi32(a, 0x4E)); // half swap (EXT)
        ph("pshufd.b1", _mm_shuffle_epi32(a, 0xB1)); // pairwise swap (REV64)
        ph("pshufd.00", _mm_shuffle_epi32(a, 0x00)); // broadcasts (DUP)
        ph("pshufd.55", _mm_shuffle_epi32(a, 0x55));
        ph("pshufd.aa", _mm_shuffle_epi32(a, 0xAA));
        ph("pshufd.ff", _mm_shuffle_epi32(a, 0xFF));
        ph("pshufd.1b", _mm_shuffle_epi32(a, 0x1B)); // full reverse (general path)
        ph("pshufd.c9", _mm_shuffle_epi32(a, 0xC9)); // rotate (general path)
        // src==dst general-path (forces the v17-staged variant)
        __m128i s = a;
        __asm__("pshufd $0x93, %0, %0" : "+x"(s));
        ph("pshufd.self", s);
        // memory source
        __m128i m;
        __asm__("pshufd $0x39, %1, %0" : "=x"(m) : "m"(vv[i]));
        ph("pshufd.mem", m);
    }
}

__attribute__((target("sse2"))) static void t_bshift(void) {
    for (int i = 0; i < NV; i++) {
        __m128i a = _mm_set_epi64x((long long)vv[i][1], (long long)vv[i][0]);
        ph("psrldq.0", _mm_srli_si128(a, 0));
        ph("psrldq.1", _mm_srli_si128(a, 1));
        ph("psrldq.8", _mm_srli_si128(a, 8));
        ph("psrldq.15", _mm_srli_si128(a, 15));
        ph("pslldq.0", _mm_slli_si128(a, 0));
        ph("pslldq.1", _mm_slli_si128(a, 1));
        ph("pslldq.8", _mm_slli_si128(a, 8));
        ph("pslldq.15", _mm_slli_si128(a, 15));
        // >15 (all-zero edge; via asm so the compiler can't const-fold it away)
        __m128i z1 = a, z2 = a;
        __asm__("psrldq $16, %0" : "+x"(z1));
        __asm__("pslldq $17, %0" : "+x"(z2));
        ph("psrldq.16", z1); ph("pslldq.17", z2);
    }
}

// AES chains: exercise the v26 zero-key hoist across long AESENC/AESDEC runs, the alias (state==key)
// path, the v27 pshufb-mask hoist interleaved with AES, and BOTH claim-clear paths (a rep-movsb host
// funnel and SHA256 rounds between AES ops).
__attribute__((target("aes,ssse3,sha,sse4.1"))) static void t_aes_runs(void) {
    const __m128i bswap = _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
    for (int i = 0; i < NV; i++) {
        __m128i s = _mm_set_epi64x((long long)vv[i][1], (long long)vv[i][0]);
        __m128i k1 = _mm_set_epi64x((long long)vv[(i + 1) % NV][1], (long long)vv[(i + 2) % NV][0]);
        __m128i k2 = _mm_set_epi64x((long long)vv[(i + 4) % NV][0], (long long)vv[(i + 3) % NV][1]);
        // back-to-back run (claim hoisted after the first)
        __m128i r = s;
        r = _mm_aesenc_si128(r, k1); r = _mm_aesenc_si128(r, k2);
        r = _mm_aesenc_si128(r, k1); r = _mm_aesenclast_si128(r, k2);
        ph("aesrun.enc", r);
        r = _mm_aesdec_si128(r, k2); r = _mm_aesdec_si128(r, k1);
        r = _mm_aesdeclast_si128(r, k2);
        ph("aesrun.dec", r);
        ph("aesrun.alias", _mm_aesenc_si128(s, s)); // state==key alias path
        ph("aesrun.imc", _mm_aesimc_si128(s));
        // pshufb interleaved with aesenc: both v26 (zero) and v27 (0x8f mask) claims active at once
        __m128i t = _mm_aesenc_si128(s, k1);
        t = _mm_shuffle_epi8(t, bswap);
        t = _mm_aesenc_si128(t, k2);
        t = _mm_shuffle_epi8(t, bswap);
        t = _mm_aesenclast_si128(t, k1);
        ph("aesrun.shufmix", t);
        // rep movsb BETWEEN two aesenc: the string funnel may blr a host helper (clobbers v16..v31);
        // the claims must be dropped -- a stale zero-key claim would corrupt the second aesenc.
        __m128i u = _mm_aesenc_si128(s, k1);
        char buf1[64], buf2[64];
        memset(buf1, i + 1, sizeof buf1);
        __asm__ volatile("rep movsb" : : "S"(buf1), "D"(buf2), "c"(sizeof buf1) : "memory");
        u = _mm_aesenc_si128(u, _mm_set1_epi8(buf2[7]));
        u = _mm_aesenc_si128(u, k2);
        ph("aesrun.repmix", u);
        // SHA256 rounds between AES ops: SHA clobbers v20..v31 (incl. v26/v27) -> claims must clear
        __m128i w = _mm_aesenc_si128(s, k1);
        __m128i sh = _mm_sha256msg1_epu32(w, k2);
        sh = _mm_sha256rnds2_epu32(sh, w, k1); // (xmm0-implicit WK operand)
        w = _mm_aesenc_si128(_mm_xor_si128(w, sh), k2);
        w = _mm_shuffle_epi8(w, bswap);
        ph("aesrun.shamix", w);
    }
}

int main(void) {
    t_movbe();
    t_crc32();
    t_pextr_pinsr();
    t_insertps();
    t_keygen();
    t_pclmul();
    t_pshufd();
    t_bshift();
    t_aes_runs();
    printf("sse4x done\n");
    return 0;
}
