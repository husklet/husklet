#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_compare.h"
#include "../include/executor.h"

#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "vector:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define COMPARE(q, u, size, opcode, rm, rn, rd) \
    (0x0e200400u | ((uint32_t)(q) << 30) | ((uint32_t)(u) << 29) | \
     ((uint32_t)(size) << 22) | ((uint32_t)(opcode) << 11) | \
     ((uint32_t)(rm) << 16) | ((uint32_t)(rn) << 5) | (rd))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void vector(hl_native_aarch64_cpu *cpu, unsigned reg, uint64_t low, uint64_t high) {
    cpu->vectors[reg * 2] = low;
    cpu->vectors[reg * 2 + 1] = high;
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        COMPARE(1, 0, 0, 0x06, 30, 28, 0), /* cmgt */
        COMPARE(1, 1, 0, 0x06, 30, 28, 1), /* cmhi */
        COMPARE(1, 0, 0, 0x07, 30, 28, 2), /* cmge */
        COMPARE(1, 1, 0, 0x07, 30, 28, 3), /* cmhs */
        COMPARE(1, 0, 0, 0x11, 30, 28, 4), /* cmtst */
        COMPARE(1, 1, 0, 0x11, 30, 28, 5), /* cmeq */
        COMPARE(1, 1, 0, 0x11, 30, 28, 28),
        COMPARE(0, 1, 0, 0x11, 30, 28, 6),
        COMPARE(1, 0, 3, 0x06, 30, 28, 7),
        0x6e208c23u, /* cmeq v3.16b,v1.16b,v0.16b */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_compare_emit(&assembler, words[index], 0x8000 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xa0000000);
    cpu.tls = UINT64_C(0x123456789abcdef0);
    cpu.registers[0] = UINT64_C(0x8877665544332211);
    vector(&cpu, 28, UINT64_C(0x8080808080808080), UINT64_C(0x8080808080808080));
    vector(&cpu, 30, UINT64_C(0x7f7f7f7f7f7f7f7f), UINT64_C(0x7f7f7f7f7f7f7f7f));
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.vectors[0] == 0 && cpu.vectors[1] == 0);
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.vectors[2] == UINT64_MAX && cpu.vectors[3] == UINT64_MAX);
    vector(&cpu, 30, UINT64_C(0x8080808080808080), UINT64_C(0x8080808080808080));
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.vectors[4] == UINT64_MAX && cpu.vectors[5] == UINT64_MAX);
    vector(&cpu, 28, UINT64_C(0x0101010101010101), UINT64_C(0x0101010101010101));
    vector(&cpu, 30, UINT64_C(0x0202020202020202), UINT64_C(0x0202020202020202));
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.vectors[6] == 0 && cpu.vectors[7] == 0);
    vector(&cpu, 28, UINT64_C(0x5555555555555555), UINT64_C(0x5555555555555555));
    vector(&cpu, 30, UINT64_C(0x0f0f0f0f0f0f0f0f), UINT64_C(0x0f0f0f0f0f0f0f0f));
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.vectors[8] == UINT64_MAX && cpu.vectors[9] == UINT64_MAX);
    vector(&cpu, 30, UINT64_C(0x5555555555555555), UINT64_C(0x5555555555555555));
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.vectors[10] == UINT64_MAX && cpu.vectors[11] == UINT64_MAX);
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.vectors[56] == UINT64_MAX && cpu.vectors[57] == UINT64_MAX);
    vector(&cpu, 28, UINT64_C(0x1111111111111111), UINT64_C(0x2222222222222222));
    vector(&cpu, 30, UINT64_C(0x1111111111111111), UINT64_C(0x2222222222222222));
    vector(&cpu, 6, 0, UINT64_MAX);
    execute(&cpu, code + offsets[7]);
    CHECK(cpu.vectors[12] == UINT64_MAX && cpu.vectors[13] == 0);
    vector(&cpu, 28, (uint64_t)INT64_MIN, 5);
    vector(&cpu, 30, 0, 4);
    execute(&cpu, code + offsets[8]);
    CHECK(cpu.vectors[14] == 0 && cpu.vectors[15] == UINT64_MAX);
    vector(&cpu, 0, UINT64_C(0x0102030405060708), UINT64_C(0x1112131415161718));
    vector(&cpu, 1, UINT64_C(0x0102030405060708), UINT64_C(0x1112131415161718));
    execute(&cpu, code + offsets[9]);
    CHECK(cpu.vectors[6] == UINT64_MAX && cpu.vectors[7] == UINT64_MAX);
    CHECK(cpu.flags == UINT64_C(0xa0000000));
    CHECK(cpu.tls == UINT64_C(0x123456789abcdef0));
    CHECK(cpu.registers[0] == UINT64_C(0x8877665544332211));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_SIMD_COMPARE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_compare_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_compare_emit(&assembler, COMPARE(0, 1, 3, 0x11, 2, 1, 0), 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_compare_emit(&assembler, 0x4e20e420u, 0x9000));
    return 0;
#endif
}
