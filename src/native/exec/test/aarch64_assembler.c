#include "../src/arch/aarch64/assembler.h"

#include <stdio.h>
#include <string.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "assembler:%d: %s\n", __LINE__, #x); return 1; } } while (0)

int main(void) {
    uint8_t writable[32] = {0};
    uint8_t executable[32] = {0};
    const uint32_t expected[] = {0xd2902463u, 0xf2b8ace3u, 0xf2c93563u, 0xf2e02463u, 0xa9010c41u, 0xa9410c41u};
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, writable, executable, sizeof(writable)));
    hl_a64_movconst(&assembler, 3, UINT64_C(0x12349abc5678123));
    hl_a64_stp(&assembler, 1, 3, 2, 16);
    hl_a64_ldp(&assembler, 1, 3, 2, 16);
    CHECK(hl_a64_assembler_ok(&assembler));
    CHECK(hl_a64_assembler_size(&assembler) == sizeof(expected));
    CHECK(memcmp(writable, expected, sizeof(expected)) == 0);
    CHECK(hl_a64_assembler_rx(&assembler, writable + 8) == executable + 8);
    hl_a64_emit32(&assembler, 1);
    hl_a64_emit32(&assembler, 2);
    hl_a64_emit32(&assembler, 3);
    CHECK(!hl_a64_assembler_ok(&assembler));
    CHECK(hl_a64_assembler_size(&assembler) == sizeof(writable));
    return 0;
}
