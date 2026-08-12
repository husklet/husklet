#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/shift.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "shift:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define SHIFT(sf, operation, rm, rn, rd) \
    (((uint32_t)(sf) << 31) | 0x1ac02000u | ((uint32_t)(operation) << 10) | \
     ((uint32_t)(rm) << 16) | ((uint32_t)(rn) << 5) | (rd))

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
    uint32_t words[12];
    size_t offsets[12];
    size_t count = 0;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++)
        words[count++] = SHIFT(1, 0, 2, 1, stolen[i]);
    words[count++] = SHIFT(1, 0, 2, 1, 3);    /* lslv x */
    words[count++] = SHIFT(1, 1, 2, 1, 3);    /* lsrv x */
    words[count++] = SHIFT(1, 2, 2, 1, 3);    /* asrv x */
    words[count++] = SHIFT(1, 3, 2, 1, 3);    /* rorv x */
    words[count++] = SHIFT(0, 0, 2, 1, 3);    /* lslv w */
    words[count++] = SHIFT(1, 0, 17, 16, 28); /* all stolen */
    words[count++] = SHIFT(1, 3, 31, 31, 31); /* all ZR */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_shift_emit(&assembler, words[i], 0xd000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xf0000000);
    cpu.registers[1] = 3; cpu.registers[2] = 1;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[stolen[i]] == 6);
    }
    cpu.registers[1] = 3;
    for (uint64_t amount = 0; amount <= 65; amount += amount == 0 ? 64 : 1) {
        cpu.registers[2] = amount;
        execute(&cpu, code + offsets[5]);
        CHECK(cpu.registers[3] == (amount == 65 ? 6 : 3));
    }
    cpu.registers[1] = UINT64_C(0x8000000000000000); cpu.registers[2] = 63;
    execute(&cpu, code + offsets[6]); CHECK(cpu.registers[3] == 1);
    execute(&cpu, code + offsets[7]); CHECK(cpu.registers[3] == UINT64_MAX);
    cpu.registers[1] = 1; cpu.registers[2] = 1;
    execute(&cpu, code + offsets[8]); CHECK(cpu.registers[3] == UINT64_C(0x8000000000000000));
    cpu.registers[1] = UINT64_C(0x100000001); cpu.registers[2] = 32;
    execute(&cpu, code + offsets[9]); CHECK(cpu.registers[3] == 1);
    cpu.registers[2] = 33;
    execute(&cpu, code + offsets[9]); CHECK(cpu.registers[3] == 2);
    cpu.registers[16] = 7; cpu.registers[17] = 2;
    execute(&cpu, code + offsets[10]); CHECK(cpu.registers[28] == 28);
    cpu.registers[30] = UINT64_C(0x5555555555555555);
    execute(&cpu, code + offsets[11]); CHECK(cpu.registers[30] == UINT64_C(0x5555555555555555));
    CHECK(cpu.flags == UINT64_C(0xf0000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_SHIFT_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_shift_emit(&assembler, words[0], 0xe000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_shift_emit(&assembler, 0xd503201fu, 0xe000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
