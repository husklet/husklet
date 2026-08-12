#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/logical.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "logical:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define LOGI(sf, opc, n, immr, imms, rn, rd) \
    (((uint32_t)(sf) << 31) | ((uint32_t)(opc) << 29) | 0x12000000u | ((uint32_t)(n) << 22) | \
     ((uint32_t)(immr) << 16) | ((uint32_t)(imms) << 10) | ((uint32_t)(rn) << 5) | (rd))

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
    uint32_t words[10];
    size_t offsets[10];
    size_t count = 0;
    uint64_t mask;
    CHECK(hl_a64_logical_mask(LOGI(1, 0, 1, 0, 0, 0, 0), &mask) && mask == 1);
    CHECK(hl_a64_logical_mask(LOGI(1, 0, 1, 1, 0, 0, 0), &mask) &&
          mask == UINT64_C(0x8000000000000000));
    CHECK(hl_a64_logical_mask(LOGI(0, 0, 0, 0, 39, 0, 0), &mask) && mask == UINT64_C(0x00ff00ff));
    CHECK(!hl_a64_logical_mask(LOGI(0, 0, 1, 0, 0, 0, 0), &mask));
    CHECK(!hl_a64_logical_mask(LOGI(1, 0, 1, 0, 63, 0, 0), &mask));
    CHECK(!hl_a64_logical_mask(LOGI(1, 0, 0, 0, 62, 0, 0), &mask));

    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++)
        words[count++] = LOGI(1, 1, 1, 0, 0, 31, stolen[i]); /* orr Xd,xzr,#1 */
    words[count++] = LOGI(1, 0, 1, 0, 0, 30, 18); /* and x18,x30,#1 */
    words[count++] = LOGI(0, 1, 0, 0, 39, 31, 1); /* orr w1,wzr,#0x00ff00ff */
    words[count++] = LOGI(1, 3, 1, 0, 0, 30, 31); /* tst x30,#1 */
    words[count++] = LOGI(1, 3, 1, 1, 0, 30, 2);  /* ands x2,x30,#bit63 */
    words[count++] = LOGI(1, 2, 1, 0, 0, 30, 1);  /* eor x1,x30,#1 */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_logical_emit(&assembler, words[i], 0x7000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        cpu.flags = UINT64_C(0xf0000000);
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[stolen[i]] == 1 && cpu.flags == UINT64_C(0xf0000000));
    }
    cpu.registers[30] = 3;
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.registers[18] == 1);
    cpu.registers[1] = UINT64_MAX;
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.registers[1] == UINT64_C(0x00ff00ff));
    cpu.registers[30] = 0;
    execute(&cpu, code + offsets[7]);
    CHECK(cpu.flags == UINT64_C(0x40000000));
    cpu.registers[30] = 1;
    execute(&cpu, code + offsets[7]);
    CHECK(cpu.flags == 0);
    cpu.registers[30] = UINT64_C(0x8000000000000000);
    execute(&cpu, code + offsets[8]);
    CHECK(cpu.registers[2] == UINT64_C(0x8000000000000000));
    CHECK(cpu.flags == UINT64_C(0x80000000));
    cpu.registers[30] = 3;
    cpu.flags = UINT64_C(0x60000000);
    execute(&cpu, code + offsets[9]);
    CHECK(cpu.registers[1] == 2 && cpu.flags == UINT64_C(0x60000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_LOGICAL_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_logical_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_logical_emit(&assembler, LOGI(1, 0, 1, 0, 63, 0, 0), 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
