#include "../src/arch/aarch64/compare.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "compare:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define CCMP(sf, subtract, immediate, operand, condition, rn, nzcv) \
    (((uint32_t)(sf) << 31) | ((uint32_t)(subtract) << 30) | 0x3a400000u | \
     ((uint32_t)(operand) << 16) | ((uint32_t)(condition) << 12) | ((uint32_t)(immediate) << 11) | \
     ((uint32_t)(rn) << 5) | (nzcv))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static int holds(unsigned condition, uint64_t flags) {
    int n = (flags >> 31) & 1, z = (flags >> 30) & 1;
    int c = (flags >> 29) & 1, v = (flags >> 28) & 1;
    int result;
    switch (condition >> 1) {
        case 0: result = z; break;
        case 1: result = c; break;
        case 2: result = n; break;
        case 3: result = v; break;
        case 4: result = c && !z; break;
        case 5: result = n == v; break;
        case 6: result = !z && n == v; break;
        default: return 1;
    }
    return result ^ (condition & 1u);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[22];
    size_t offsets[22];
    size_t count = 0;
    for (unsigned condition = 0; condition < 16; condition++)
        words[count++] = CCMP(1, 1, 0, 2, condition, 1, 9); /* ccmp x1,x2,#9,cond */
    words[count++] = CCMP(1, 0, 0, 2, 14, 1, 0); /* ccmn x1,x2,#0,al */
    words[count++] = CCMP(1, 1, 0, 2, 14, 1, 0); /* ccmp x1,x2,#0,al */
    words[count++] = CCMP(1, 1, 1, 5, 14, 1, 0); /* ccmp x1,#5,#0,al */
    words[count++] = CCMP(0, 1, 1, 1, 14, 1, 0); /* ccmp w1,#1,#0,al */
    words[count++] = CCMP(1, 1, 0, 17, 14, 16, 0); /* stolen sources */
    words[count++] = CCMP(1, 0, 0, 31, 14, 31, 0); /* ZR operands */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_compare_emit(&assembler, words[i], 0xf000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.registers[1] = 7; cpu.registers[2] = 7;
    for (unsigned condition = 0; condition < 16; condition++) {
        cpu.flags = UINT64_C(0xa0000000);
        execute(&cpu, code + offsets[condition]);
        CHECK(cpu.flags == (holds(condition, UINT64_C(0xa0000000)) ? UINT64_C(0x60000000) : UINT64_C(0x90000000)));
        CHECK(cpu.registers[1] == 7 && cpu.registers[2] == 7);
    }
    cpu.registers[1] = UINT64_C(0x7fffffffffffffff); cpu.registers[2] = 1;
    execute(&cpu, code + offsets[16]); CHECK(cpu.flags == UINT64_C(0x90000000));
    cpu.registers[1] = UINT64_MAX; cpu.registers[2] = 1;
    execute(&cpu, code + offsets[16]); CHECK(cpu.flags == UINT64_C(0x60000000));
    cpu.registers[1] = 0; cpu.registers[2] = 1;
    execute(&cpu, code + offsets[17]); CHECK(cpu.flags == UINT64_C(0x80000000));
    cpu.registers[1] = 5;
    execute(&cpu, code + offsets[18]); CHECK(cpu.flags == UINT64_C(0x60000000));
    cpu.registers[1] = UINT64_C(0x100000001);
    execute(&cpu, code + offsets[19]); CHECK(cpu.flags == UINT64_C(0x60000000));
    cpu.registers[16] = 9; cpu.registers[17] = 9;
    execute(&cpu, code + offsets[20]); CHECK(cpu.flags == UINT64_C(0x60000000));
    execute(&cpu, code + offsets[21]); CHECK(cpu.flags == UINT64_C(0x40000000));
    CHECK(cpu.registers[16] == 9 && cpu.registers[17] == 9);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_COMPARE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_compare_emit(&assembler, words[0], 0x10000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_compare_emit(&assembler, 0xd503201fu, 0x10000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
