// #123/#223 regression: a NON-PIE x86_64 glibc ET_EXEC that dereferences a baked LOW absolute pointer
// through the two C-emulated vector paths the x86 engine exits to -- do_avx (VEX/AVX) and do_sse3b (legacy
// 0F38/0F3A SSSE3/SSE4). Both funnel every guest-memory operand through avx_ea(), which formerly returned
// the guest's LOW link address 1:1 ("PIE images load 1:1") instead of folding +g_nonpie_bias like the
// JIT-emitted path (ea_bias17). A base-register vector operand holding a baked low image pointer then hit
// the UNMAPPED low vaddr -> SIGSEGV. This is exactly node --version's V8 init crash (a low image pointer in
// rsi/rax dereferenced by an AVX/SSSE3 load); the SIGSEGV was masked into corrupt data by the lazy
// zero-page mapper. Fixed by folding the non-PIE bias in avx_ea (translate/x86_64/avx.c).
//
// Built -static -no-pie (ET_EXEC) so g_nonpie_lo is armed (PIE/static-PIE leave it 0 -> avx_ea inert).
// Deterministic output, diffed byte-exact vs the qemu-x86_64 oracle. Inline asm (not intrinsics) so the
// exact instruction + base-register-into-low-image addressing form is emitted regardless of -march: the
// harness compiles with no per-test -march, and `as` assembles these mnemonics unconditionally.
#include <stdio.h>
#include <stdint.h>

// Low absolute static data (non-PIE .rodata): distinct signed byte patterns so a wrong address/width/fold
// shows up as a bad checksum rather than a lucky match. 64 bytes = one 256-bit + one 128-bit source.
static const int8_t g_data[64] __attribute__((aligned(64))) = {
    0x00, -0x11, 0x22, -0x33, 0x44, -0x55, 0x66, -0x77, -0x08, 0x19, -0x2a, 0x3b, -0x4c, 0x5d, -0x6e, 0x7f,
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, -0x01, -0x02, -0x03, -0x04, -0x05, -0x06, -0x07, -0x08,
    0x7f, -0x80, 0x40, -0x40, 0x20, -0x20, 0x10, -0x10, 0x0f, -0x0f, 0x70, -0x70, 0x3c, -0x3c, 0x5a, -0x5a,
    0x12, -0x34, 0x56, -0x78, 0x1a, -0x2b, 0x3c, -0x4d, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, -0x78,
};

int main(void) {
    // Materialize the LOW link address of g_data as a 32-bit ABSOLUTE IMMEDIATE (R_X86_64_32; valid because
    // -no-pie and the image links under 4GiB). Code immediates are NOT rebased by the loader (unlike a
    // .data.rel.ro pointer, which RELRO-rebases to the high mapping), so this reproduces V8's exact case: a
    // baked LOW image pointer sitting in a base register, dereferenced by a vector op -> avx_ea sees LOW.
    const int8_t *p;
    __asm__ volatile("movl %1, %k0" : "=r"(p) : "i"(g_data));
    uint8_t out_avx[32], out_sse[16];

    // do_avx path: `vmovdqu (p), %ymm0` -- a VEX-encoded 256-bit load whose base register holds the low
    // image address; the translator exits the block to do_avx(), which resolves the EA via avx_ea().
    __asm__ volatile("vmovdqu (%1), %%ymm0\n\t"
                     "vmovdqu %%ymm0, (%0)\n\t"
                     "vzeroupper"
                     :
                     : "r"(out_avx), "r"(p)
                     : "ymm0", "memory");

    // do_sse3b path: `pabsb (p), %xmm0` -- a legacy 0F38 1C SSSE3 op with a memory source from the low
    // image (pabsb is not lowered inline like pshufb/AES, so it exits to do_sse3b -> sse_get_rm -> avx_ea).
    __asm__ volatile("pxor %%xmm0, %%xmm0\n\t"
                     "pabsb (%1), %%xmm0\n\t"
                     "movdqu %%xmm0, (%0)"
                     :
                     : "r"(out_sse), "r"(p)
                     : "xmm0", "memory");

    // FNV-1a over both results -- any wrong byte (wrong address, missing fold, wrong width) diverges.
    uint64_t h = 1469598103934665603ull;
    for (int i = 0; i < 32; i++) h = (h ^ out_avx[i]) * 1099511628211ull;
    for (int i = 0; i < 16; i++) h = (h ^ out_sse[i]) * 1099511628211ull;

    printf("nonpie-vec avx[0]=%02x avx[31]=%02x sse[0]=%02x sse[15]=%02x fnv=%016llx\n",
           out_avx[0], out_avx[31], out_sse[0], out_sse[15], (unsigned long long)h);
    return 0;
}
