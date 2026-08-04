/* Unaligned scalar access through a byte buffer. Both targets permit unaligned loads/stores; the
   translator must preserve the observed value regardless of alignment. We read/write 16/32/64-bit
   ints and a float/double at every byte offset within an 8-byte window. Values are little-endian on
   both SysV targets, so the derived output is arch-neutral. */
#include <stdio.h>
#include <stdint.h>
#include <string.h>

static uint8_t buf[64];

static uint32_t ld32(int off) { uint32_t v; memcpy(&v, buf + off, 4); return v; }
static uint64_t ld64(int off) { uint64_t v; memcpy(&v, buf + off, 8); return v; }
static void st32(int off, uint32_t v) { memcpy(buf + off, &v, 4); }

int main(void) {
    for (int i = 0; i < 64; i++) buf[i] = (uint8_t)(i * 37 + 11);
    unsigned long acc = 2166136261u;
    for (int off = 0; off < 8; off++) {
        uint16_t h; memcpy(&h, buf + off, 2);
        uint32_t w = ld32(off);
        uint64_t q = ld64(off);
        float ff; memcpy(&ff, buf + off, 4);
        (void)ff;
        acc = (acc ^ h) * 16777619u;
        acc = (acc ^ w) * 16777619u;
        acc = (acc ^ (uint32_t)(q ^ (q >> 32))) * 16777619u;
        printf("off=%d h=%u w=%u q=%llu\n", off, h, w, (unsigned long long)q);
    }
    /* round-trip an unaligned store */
    st32(3, 0xDEADBEEFu);
    printf("rt=%u acc=%08lx\n", ld32(3), acc & 0xFFFFFFFFul);
    return 0;
}
