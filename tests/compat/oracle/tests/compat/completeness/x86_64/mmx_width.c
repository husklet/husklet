// The no-prefix (MMX) form of an integer-SIMD opcode is 64-bit; the 0x66 form of the same opcode is
// 128-bit. Lowering the MMX form through the SSE arms made every one of them 128-bit, which is
// guest-visible twice over:
//   1. OVER-READ  -- an 8-byte memory operand at the end of a page reads 16 and #PFs on a page the guest
//      never touched. Part 4 puts each operand in the last 8 bytes of a mapped page, next page PROT_NONE.
//   2. OVER-WRITE -- bits 127:64 of the destination get written when MMX leaves them alone. mm0-7 alias
//      the low halves of xmm0-7 here, so that is live state. Part 1 catches it through memory, part 3
//      through pmovmskb's mask width (8 bits for mm, 16 for xmm).
// Cross-lane ops need a real 64-bit form, not just a narrower operand: at 128 bits punpckh* reads bytes
// 8..15 and pack* narrows 8 source lanes per operand instead of 4, so their LOW half comes out wrong too
// (part 2). pextrw/pinsrw wrap the lane index at 4 for mm, not 8.
// Golden generated on native x86-64 hardware.
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static void show(const char *name, uint64_t r) { printf("%-12s %016llx\n", name, (unsigned long long)r); }

// ---- part 1: a 64-bit MMX store must not touch the 8 bytes that follow ----
static void part1_store_width(void) {
    unsigned char src[8] = {1, 2, 3, 4, 5, 6, 7, 8};
    unsigned char dst[16];
    memset(dst, 0xaa, sizeof dst);
    __asm__ volatile("movq (%0), %%mm0\n\t" // 64-bit MMX load
                     "paddb %%mm0, %%mm0\n\t"
                     "movq %%mm0, (%1)\n\t" // 64-bit MMX store: must not touch dst[8..15]
                     "emms"
                     :
                     : "r"(src), "r"(dst)
                     : "mm0", "memory");
    printf("mmx dst:");
    for (int i = 0; i < 16; i++)
        printf(" %02x", dst[i]);
    printf("\n");
}

// ---- part 2: cross-lane ops at MMX width ----
#define MMX2(mn, a, b)                                                                                                 \
    do {                                                                                                               \
        uint64_t out;                                                                                                  \
        uint64_t x = (a), y = (b);                                                                                     \
        __asm__ volatile("movq %1, %%mm0\n\tmovq %2, %%mm1\n\t" mn " %%mm1, %%mm0\n\tmovq %%mm0, %0\n\temms"           \
                         : "=m"(out)                                                                                   \
                         : "m"(x), "m"(y)                                                                              \
                         : "mm0", "mm1");                                                                              \
        show(mn, out);                                                                                                 \
    } while (0)

static void part2_cross_lane(void) {
    const uint64_t A = 0x0102030405060708ull, B = 0xfffefdfcfbfaf9f8ull;
    // punpckh* must interleave the HIGH 4 bytes / 2 words / 1 dword of a 64-bit operand. A 128-bit ZIP2
    // reads bytes 8..15 -- which for an MMX operand do not exist -- so the result is wholly wrong.
    MMX2("punpckhbw", A, B);
    MMX2("punpckhwd", A, B);
    MMX2("punpckhdq", A, B);
    MMX2("punpcklbw", A, B);
    MMX2("punpcklwd", A, B);
    MMX2("punpckldq", A, B);
    // pack*: 4 lanes from dst then 4 from src. A 128-bit narrow takes 8 from each and the low half is
    // then entirely dst's, with src's lanes nowhere in the result.
    MMX2("packsswb", A, B);
    MMX2("packuswb", A, B);
    MMX2("packssdw", A, B);
    // lane-local, but the multiply width still has to match
    MMX2("pmulhw", A, B);
    MMX2("pmulhuw", A, B);
    MMX2("pmaddwd", A, B);
    MMX2("pmuludq", A, B);
    MMX2("psadbw", A, B);
}

// ---- part 3: destination bits 127:64 must stay untouched ----
static void part3_overwrite(void) {
    // pmovmskb on mm gathers 8 byte-MSBs; on xmm it gathers 16. pcmpeqb mm,mm sets all 8 bytes to 0xFF,
    // and at 128-bit width would set all 16 -- so a mask wider than 0xff proves the destination's high
    // half was written. This is the over-write made observable WITHOUT reading the aliased xmm (which
    // real hardware leaves alone and this engine's register model deliberately does not model).
    unsigned m0, m1, m2;
    uint64_t a = 0xfffefdfcfbfaf9f8ull, b = 0x0102030405060708ull;
    __asm__ volatile("pcmpeqb %%mm1, %%mm1\n\tpmovmskb %%mm1, %0\n\temms" : "=r"(m0) : : "mm1");
    printf("pmovmskb ones %08x\n", m0);
    __asm__ volatile("movq %1, %%mm0\n\tpmovmskb %%mm0, %0\n\temms" : "=r"(m1) : "m"(a) : "mm0");
    printf("pmovmskb neg  %08x\n", m1);
    // psubb leaves a high half only if the arm wrote 128 bits; the mask must still be 8 bits wide.
    __asm__ volatile("movq %1, %%mm0\n\tmovq %2, %%mm1\n\tpsubb %%mm1, %%mm0\n\tpmovmskb %%mm0, %0\n\temms"
                     : "=r"(m2)
                     : "m"(a), "m"(b)
                     : "mm0", "mm1");
    printf("pmovmskb sub  %08x\n", m2);

    // pextrw/pinsrw index 4 words on mm, so imm8 wraps at 4 -- $4 is lane 0, $7 is lane 3. The 8-word
    // xmm masking reads/writes lanes 4..7, i.e. the half that does not exist.
    unsigned e0, e4, e7;
    __asm__ volatile("movq %3, %%mm0\n\tpextrw $0, %%mm0, %0\n\tpextrw $4, %%mm0, %1\n\tpextrw $7, %%mm0, %2\n\temms"
                     : "=r"(e0), "=r"(e4), "=r"(e7)
                     : "m"(b)
                     : "mm0");
    printf("pextrw 0/4/7  %04x %04x %04x\n", e0, e4, e7);
    uint64_t i4, i7;
    unsigned w = 0x1234;
    __asm__ volatile("movq %2, %%mm0\n\tpinsrw $4, %3, %%mm0\n\tmovq %%mm0, %0\n\t"
                     "movq %2, %%mm0\n\tpinsrw $7, %3, %%mm0\n\tmovq %%mm0, %1\n\temms"
                     : "=m"(i4), "=m"(i7)
                     : "m"(b), "r"(w)
                     : "mm0");
    show("pinsrw $4", i4);
    show("pinsrw $7", i7);
}

// ---- part 4: an 8-byte MMX operand at a page edge must not read 16 ----
// `p` is the last 8 bytes of a mapped page; the page after it is PROT_NONE. A 128-bit load faults.
#define EDGE(mn)                                                                                                       \
    do {                                                                                                               \
        uint64_t out;                                                                                                  \
        __asm__ volatile("movq %2, %%mm0\n\t" mn " (%1), %%mm0\n\tmovq %%mm0, %0\n\temms"                              \
                         : "=m"(out)                                                                                   \
                         : "r"(p), "m"(seed)                                                                           \
                         : "mm0", "memory");                                                                           \
        show(mn, out);                                                                                                 \
    } while (0)

static void part4_page_edge(void) {
    long ps = sysconf(_SC_PAGESIZE);
    char *region = mmap(NULL, (size_t)ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (region == MAP_FAILED) {
        printf("page-edge: mmap failed\n");
        return;
    }
    if (mprotect(region + ps, (size_t)ps, PROT_NONE) != 0) {
        printf("page-edge: mprotect failed\n");
        return;
    }
    unsigned char *p = (unsigned char *)region + ps - 8; // operand ends exactly at the guard page
    for (int i = 0; i < 8; i++)
        p[i] = (unsigned char)(0x11 * (i + 1));
    uint64_t seed = 0x0102030405060708ull;

    EDGE("paddb");
    EDGE("paddw");
    EDGE("paddd");
    EDGE("paddq");
    EDGE("psubb");
    EDGE("psubusb");
    EDGE("paddsw");
    EDGE("pand");
    EDGE("pandn");
    EDGE("por");
    EDGE("pxor");
    EDGE("pcmpeqb");
    EDGE("pcmpgtw");
    EDGE("pmaxub");
    EDGE("pminsw");
    EDGE("pavgb");
    EDGE("pmullw");
    EDGE("pmulhw");
    EDGE("pmulhuw");
    EDGE("pmaddwd");
    EDGE("pmuludq");
    EDGE("psadbw");
    EDGE("punpcklbw");
    EDGE("punpckhbw");
    EDGE("punpckhwd");
    EDGE("punpckhdq");
    EDGE("packsswb");
    EDGE("packuswb");
    EDGE("packssdw");
    EDGE("psllw");
    EDGE("psrld");
    EDGE("psraw");
    EDGE("movq"); // the originally-fixed case, kept under the guard page
    printf("page-edge: survived\n");
    munmap(region, (size_t)ps * 2);
}

int main(void) {
    part1_store_width();
    part2_cross_lane();
    part3_overwrite();
    part4_page_edge();
    return 0;
}
