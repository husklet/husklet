/* memmove with overlapping forward/backward ranges at odd sizes + memset/memcpy of non-power-of-two
   lengths. The translator's inlined/libc memory ops must match byte-for-byte. Arch-neutral. */
#include <stdio.h>
#include <string.h>
#include <stdint.h>

static void dump(const char *tag, uint8_t *b, int n) {
    unsigned long acc = 2166136261u;
    for (int i = 0; i < n; i++) acc = (acc ^ b[i]) * 16777619u;
    printf("%s n=%d acc=%08lx\n", tag, n, acc & 0xFFFFFFFFul);
}

int main(void) {
    uint8_t b[128];
    for (int sz = 1; sz <= 40; sz++) {
        for (int i = 0; i < 128; i++) b[i] = (uint8_t)(i + 1);
        memmove(b + 3, b, sz);          /* forward overlap */
        memmove(b + 60, b + 63, sz);    /* backward overlap */
        memset(b + 100, (int)(sz & 0xff), sz % 20);
        char t[16]; snprintf(t, sizeof t, "sz%02d", sz);
        dump(t, b, 128);
    }
    /* odd-size memcpy round trip */
    uint8_t src[37], dst[37];
    for (int i = 0; i < 37; i++) src[i] = (uint8_t)(i * 3 + 7);
    memcpy(dst, src, 37);
    dump("cpy37", dst, 37);
    return 0;
}
