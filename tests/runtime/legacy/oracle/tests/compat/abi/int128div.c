/* __int128 signed/unsigned divide, modulo, and shifts at the extremes. libgcc's __divti3/__udivti3/
   __modti3 (or inlined sequences) must agree. Printed as decimal via manual base-10 expansion so the
   output is arch-neutral and does not depend on a 128-bit printf. */
#include <stdio.h>

typedef __int128 i128;
typedef unsigned __int128 u128;

static void put_u128(u128 v) {
    char buf[40];
    int i = 40;
    if (v == 0) { putchar('0'); return; }
    while (v) { buf[--i] = (char)('0' + (int)(v % 10)); v /= 10; }
    while (i < 40) putchar(buf[i++]);
}
static void put_i128(i128 v) {
    if (v < 0) { putchar('-'); put_u128((u128)(-v)); }
    else put_u128((u128)v);
}

int main(void) {
    u128 big = ((u128)0xFEDCBA9876543210ull << 64) | 0x0123456789ABCDEFull;
    u128 d = 1000000007ull;
    printf("udiv="); put_u128(big / d); printf(" umod="); put_u128(big % d); printf("\n");

    i128 sneg = -(((i128)0x0011223344556677ll << 64) | 0x8899AABBCCDDEEFFll);
    i128 sd = -1000003;
    printf("sdiv="); put_i128(sneg / sd); printf(" smod="); put_i128(sneg % sd); printf("\n");

    u128 one = 1;
    printf("shl127="); put_u128(one << 127); printf("\n");
    printf("shr="); put_u128(big >> 65); printf("\n");
    i128 smin = (i128)((u128)1 << 127);
    printf("asr="); put_i128(smin >> 3); printf("\n");

    /* multiply high */
    u128 m = (u128)0xFFFFFFFFFFFFFFFFull * (u128)0xFFFFFFFFFFFFFFFFull;
    printf("mul="); put_u128(m); printf("\n");
    return 0;
}
