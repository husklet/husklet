// Direction-flag (DF) cross-block persistence differential (translate.c/repstr.c M-item).
// DF used to be translate-time-only (reset to forward at every block entry, never restored by popfq), so a
// `std` (or popf-set DF) whose `rep movs/stos/scas` executed in a LATER block ran FORWARD -- a silent
// wrong-direction copy. DF is now a real runtime cpu-state bit: std/cld/popfq update it and the rep-string
// lowering honors it at runtime. To force the string op into a SEPARATE block from the `std`, the rep is in a
// noinline function reached through a call (a real block boundary the block-local optimizer cannot stitch).
// Native x86 (the oracle) has real DF, so this is byte-exact.
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

// rep movsb starting at the LAST byte of each region (backward). Separate block via the call boundary.
__attribute__((noinline)) static void rep_movsb(void *dst_last, const void *src_last, size_t n) {
    asm volatile("rep movsb" : "+D"(dst_last), "+S"(src_last), "+c"(n) : : "memory", "cc");
}
__attribute__((noinline)) static void rep_stosb(void *dst_last, int val, size_t n) {
    unsigned long a = (unsigned char)val;
    asm volatile("rep stosb" : "+D"(dst_last), "+c"(n) : "a"(a) : "memory", "cc");
}
static void set_df(void) { asm volatile("std" : : : "memory", "cc"); }
static void clr_df(void) { asm volatile("cld" : : : "memory", "cc"); }
// Set DF via popfq (bit10) -- exercises the popfq DF-restore path specifically.
static void set_df_popf(void) {
    asm volatile("pushfq\n\t"
                 "orq $0x400, (%%rsp)\n\t"
                 "popfq"
                 :
                 :
                 : "cc", "memory");
}

static void dump(const char *tag, const unsigned char *p, int n) {
    printf("%s", tag);
    for (int i = 0; i < n; i++) printf("%02x", p[i]);
    printf("\n");
}

int main(void) {
    // 1) backward rep movsb across a call boundary (std in main, rep in rep_movsb's block).
    {
        unsigned char src[8] = {0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88};
        unsigned char dst[8] = {0};
        set_df();                          // DF=1 (backward) -- in THIS block
        rep_movsb(dst + 7, src + 7, 8);    // rep runs in a LATER block; must copy backward
        clr_df();
        dump("movsb-bwd ", dst, 8); // expect 11 22 33 44 55 66 77 88 (same bytes, backward-walked)
    }
    // 2) backward rep stosb across a call boundary.
    {
        unsigned char dst[6] = {1, 2, 3, 4, 5, 6};
        set_df();
        rep_stosb(dst + 5, 0xAB, 4); // fill the top 4 bytes going down
        clr_df();
        dump("stosb-bwd ", dst, 6); // expect 01 02 ab ab ab ab
    }
    // 3) DF set via popfq, then backward rep movsb in a later block (popfq-restore + persistence).
    {
        unsigned char src[5] = {0xde, 0xad, 0xbe, 0xef, 0x99};
        unsigned char dst[5] = {0};
        set_df_popf();                 // DF=1 through popfq
        rep_movsb(dst + 4, src + 4, 5);
        clr_df();
        dump("movsb-popf", dst, 5); // expect de ad be ef 99
    }
    // 4) overlapping backward copy (the memmove dst>src case DF exists for): shift a buffer up by 2.
    {
        unsigned char buf[10] = {0, 1, 2, 3, 4, 5, 6, 7, 0, 0};
        set_df();
        rep_movsb(buf + 9, buf + 7, 8); // dst[2..9] = src[0..7], backward (no smear)
        clr_df();
        dump("movsb-ovl ", buf, 10); // expect 00 01 00 01 02 03 04 05 06 07
    }
    // 5) control: forward copy after an explicit cld (must stay forward).
    {
        unsigned char src[4] = {0xa1, 0xa2, 0xa3, 0xa4};
        unsigned char dst[4] = {0};
        clr_df();
        rep_movsb(dst, src, 4); // forward: dst/src at FIRST byte
        dump("movsb-fwd ", dst, 4);
    }
    return 0;
}
