/* Bitfield packing + struct layout: little-endian bitfield allocation is ABI-fixed and identical on
   both SysV targets. We decode fields (never raw bytes) so the verdict is arch-neutral. Exercises
   sub-byte fields spanning storage-unit boundaries, signed bitfields, and sizeof/packing. */
#include <stdio.h>
#include <stdint.h>
#include <string.h>

struct packed_bits {
    uint32_t a : 3;
    uint32_t b : 10;
    int32_t  c : 6;   /* signed: sign-extends */
    uint32_t d : 13;
};

struct mixed {
    uint8_t  flag : 1;
    uint8_t  kind : 3;
    uint16_t code : 12;
    uint32_t big  : 20;
};

int main(void) {
    struct packed_bits p;
    memset(&p, 0, sizeof p);
    p.a = 5; p.b = 900; p.c = -7; p.d = 5000;
    printf("packed sizeof=%zu a=%u b=%u c=%d d=%u\n",
           sizeof p, (unsigned)p.a, (unsigned)p.b, (int)p.c, (unsigned)p.d);

    struct mixed m;
    memset(&m, 0, sizeof m);
    m.flag = 1; m.kind = 6; m.code = 3000; m.big = 999999;
    printf("mixed sizeof=%zu flag=%u kind=%u code=%u big=%u\n",
           sizeof m, (unsigned)m.flag, (unsigned)m.kind, (unsigned)m.code, (unsigned)m.big);

    /* Overflow-truncation semantics: assigning out-of-range wraps to field width. */
    p.a = 8 + 3;      /* 11 -> 3 bits -> 3 */
    p.c = 40;         /* 6-bit signed -> 40 & 0x3f = 40 -> -24 */
    printf("wrap a=%u c=%d\n", (unsigned)p.a, (int)p.c);
    return 0;
}
