// MMX movq (plain 0F 6F/7F, NO mandatory prefix) is a 64-bit operation. Lowering it through the 128-bit
// XMM load/store path made a `movq %mm, (mem)` store WRITE 8 bytes past the 64-bit destination, corrupting
// the adjacent guest memory (and a load over-read 8 bytes). This checks that a 64-bit MMX store leaves the
// following 8 bytes untouched after a load + paddb + store round-trip. Oracle-diffed vs qemu (bytes 8..15
// must stay at the 0xaa sentinel). Avoids EMMS (a separate unimplemented-opcode gap).
#include <stdio.h>
#include <string.h>

int main(void) {
    unsigned char src[8] = {1, 2, 3, 4, 5, 6, 7, 8};
    unsigned char dst[16];
    memset(dst, 0xaa, sizeof dst);
    __asm__ volatile("movq (%0), %%mm0\n\t" // 64-bit MMX load
                     "paddb %%mm0, %%mm0\n\t"
                     "movq %%mm0, (%1)\n\t" // 64-bit MMX store: must not touch dst[8..15]
                     :
                     : "r"(src), "r"(dst)
                     : "mm0", "memory");
    printf("mmx dst:");
    for (int i = 0; i < 16; i++)
        printf(" %02x", dst[i]);
    printf("\n");
    return 0;
}
