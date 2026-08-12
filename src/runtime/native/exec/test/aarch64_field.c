#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/field.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "field:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define BFM(sf, opc, n, immr, imms, rn, rd) \
    (((uint32_t)(sf) << 31) | ((uint32_t)(opc) << 29) | 0x13000000u | ((uint32_t)(n) << 22) | \
     ((uint32_t)(immr) << 16) | ((uint32_t)(imms) << 10) | ((uint32_t)(rn) << 5) | (rd))
#define EXTR(sf, n, rm, lsb, rn, rd) \
    (((uint32_t)(sf) << 31) | 0x13800000u | ((uint32_t)(n) << 22) | ((uint32_t)(rm) << 16) | \
     ((uint32_t)(lsb) << 10) | ((uint32_t)(rn) << 5) | (rd))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const unsigned stolen[] = {16, 17, 18, 28, 30};
    uint32_t words[11];
    size_t offsets[11];
    size_t count = 0;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++)
        words[count++] = BFM(1, 2, 1, 0, 7, 1, stolen[i]); /* ubfm low byte */
    words[count++] = BFM(1, 0, 1, 0, 7, 1, 2);   /* sbfm sign byte */
    words[count++] = BFM(1, 2, 1, 60, 59, 1, 2); /* lsl x2,x1,#4 */
    words[count++] = BFM(1, 2, 1, 4, 63, 1, 2);  /* lsr x2,x1,#4 */
    words[count++] = BFM(1, 1, 1, 56, 7, 1, 18); /* bfi stolen dest,#8,#8 */
    words[count++] = EXTR(1, 1, 3, 8, 2, 1);      /* extr x1,x2,x3,#8 */
    words[count++] = EXTR(0, 0, 30, 31, 30, 28);  /* ror w28,w30,#31 */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_field_emit(&assembler, words[i], 0xa000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xf0000000);
    cpu.registers[1] = UINT64_C(0x123456789abcdeab);
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[stolen[i]] == 0xab);
    }
    cpu.registers[1] = 0x80;
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.registers[2] == UINT64_C(0xffffffffffffff80));
    cpu.registers[1] = 1;
    execute(&cpu, code + offsets[6]); CHECK(cpu.registers[2] == 16);
    cpu.registers[1] = 0x120;
    execute(&cpu, code + offsets[7]); CHECK(cpu.registers[2] == 0x12);
    cpu.registers[1] = 0xaa; cpu.registers[18] = UINT64_C(0xffff0000);
    execute(&cpu, code + offsets[8]); CHECK(cpu.registers[18] == UINT64_C(0xffffaa00));
    cpu.registers[2] = 0xaa; cpu.registers[3] = 0x1100;
    execute(&cpu, code + offsets[9]); CHECK(cpu.registers[1] == UINT64_C(0xaa00000000000011));
    cpu.registers[30] = 1;
    execute(&cpu, code + offsets[10]); CHECK(cpu.registers[28] == 2);
    CHECK(cpu.flags == UINT64_C(0xf0000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_FIELD_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_field_emit(&assembler, words[0], 0xb000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_field_emit(&assembler, BFM(0, 2, 1, 0, 7, 1, 2), 0xb000));
    CHECK(!hl_a64_field_emit(&assembler, EXTR(0, 0, 1, 32, 2, 3), 0xb000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
