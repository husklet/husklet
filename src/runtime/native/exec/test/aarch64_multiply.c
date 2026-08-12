#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/multiply.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "multiply:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define MADD(sf, subtract, rm, ra, rn, rd) \
    (((uint32_t)(sf) << 31) | 0x1b000000u | ((uint32_t)(rm) << 16) | ((uint32_t)(subtract) << 15) | \
     ((uint32_t)(ra) << 10) | ((uint32_t)(rn) << 5) | (rd))
#define HIGH(unsign, rm, rn, rd) \
    (0x9b400000u | ((uint32_t)(unsign) << 23) | ((uint32_t)(rm) << 16) | \
     (31u << 10) | ((uint32_t)(rn) << 5) | (rd))
#define LONG(unsign, subtract, rm, ra, rn, rd) \
    (0x9b200000u | ((uint32_t)(unsign) << 23) | ((uint32_t)(rm) << 16) | \
     ((uint32_t)(subtract) << 15) | ((uint32_t)(ra) << 10) | ((uint32_t)(rn) << 5) | (rd))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static uint64_t long_result(uint64_t left, uint64_t right, uint64_t addend,
                            int unsign, int subtract) {
    uint64_t product = unsign
        ? (uint64_t)(uint32_t)left * (uint64_t)(uint32_t)right
        : (uint64_t)((int64_t)(int32_t)left * (int64_t)(int32_t)right);
    return subtract ? addend - product : addend + product;
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        MADD(1, 0, 2, 3, 1, 4),   /* madd x4,x1,x2,x3 */
        MADD(1, 1, 2, 3, 1, 4),   /* msub x4,x1,x2,x3 */
        MADD(1, 0, 2, 31, 1, 4),  /* mul alias */
        MADD(1, 1, 2, 31, 1, 4),  /* mneg alias */
        MADD(0, 0, 2, 3, 1, 4),   /* W wrapping */
        MADD(1, 0, 17, 18, 16, 28), /* all stolen inputs/dest */
        MADD(1, 0, 2, 18, 1, 9),  /* stolen accumulator, x9 destination */
        LONG(0, 0, 2, 3, 1, 4),   /* smaddl */
        LONG(0, 1, 2, 3, 1, 4),   /* smsubl */
        LONG(1, 0, 2, 3, 1, 4),   /* umaddl */
        LONG(1, 1, 2, 3, 1, 4),   /* umsubl */
        LONG(0, 0, 17, 18, 16, 28), /* stolen inputs, accumulator, destination */
        LONG(1, 1, 17, 18, 16, 9),  /* stolen inputs/accumulator, x9 destination */
        LONG(0, 0, 16, 16, 16, 16), /* every field aliases a stolen register */
        LONG(1, 0, 31, 31, 31, 30), /* umull zero-register sources */
        HIGH(0, 2, 1, 4),          /* smulh */
        HIGH(1, 2, 1, 4),          /* umulh */
        HIGH(1, 17, 16, 28),       /* stolen inputs/destination */
        HIGH(1, 31, 31, 31),       /* zero inputs, discard */
        MADD(1, 0, 31, 31, 31, 31), /* all ZR, discard */
    };
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t offsets[sizeof(words) / sizeof(words[0])];
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < sizeof(words) / sizeof(words[0]); i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_multiply_emit(&assembler, words[i], 0xb000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xf0000000);
    cpu.registers[1] = 3; cpu.registers[2] = 4; cpu.registers[3] = 5;
    execute(&cpu, code + offsets[0]); CHECK(cpu.registers[4] == 17);
    execute(&cpu, code + offsets[1]); CHECK(cpu.registers[4] == UINT64_C(0xfffffffffffffff9));
    execute(&cpu, code + offsets[2]); CHECK(cpu.registers[4] == 12);
    execute(&cpu, code + offsets[3]); CHECK(cpu.registers[4] == UINT64_C(0xfffffffffffffff4));
    cpu.registers[1] = UINT64_C(0xffffffff); cpu.registers[2] = 2; cpu.registers[3] = 1;
    execute(&cpu, code + offsets[4]); CHECK(cpu.registers[4] == UINT64_C(0xffffffff));
    cpu.registers[16] = 3; cpu.registers[17] = 4; cpu.registers[18] = 5;
    execute(&cpu, code + offsets[5]); CHECK(cpu.registers[28] == 17);
    cpu.registers[1] = 3; cpu.registers[2] = 4; cpu.registers[18] = 5; cpu.registers[9] = 99;
    execute(&cpu, code + offsets[6]); CHECK(cpu.registers[9] == 17);
    {
        const uint64_t operands[] = {
            0, 1, UINT64_C(0x7fffffff), UINT64_C(0x80000000),
            UINT64_C(0xffffffff), UINT64_C(0xaaaaaaaa80000000), UINT64_MAX,
        };
        for (size_t left = 0; left < sizeof(operands) / sizeof(operands[0]); left++) {
            for (size_t right = 0; right < sizeof(operands) / sizeof(operands[0]); right++) {
                for (size_t addend = 0; addend < sizeof(operands) / sizeof(operands[0]); addend++) {
                    for (size_t operation = 0; operation < 4; operation++) {
                        int unsign = operation >= 2;
                        int subtract = operation & 1;
                        cpu.registers[1] = operands[left];
                        cpu.registers[2] = operands[right];
                        cpu.registers[3] = operands[addend];
                        execute(&cpu, code + offsets[7 + operation]);
                        CHECK(cpu.registers[4] == long_result(operands[left], operands[right],
                                                             operands[addend], unsign, subtract));
                    }
                }
            }
        }
    }
    cpu.registers[1] = UINT64_C(0xaaaaaaaa80000000); cpu.registers[2] = UINT64_C(0xbbbbbbbbffffffff);
    cpu.registers[3] = UINT64_C(0x0123456789abcdef);
    execute(&cpu, code + offsets[7]); CHECK(cpu.registers[4] == UINT64_C(0x0123456809abcdef));
    execute(&cpu, code + offsets[8]); CHECK(cpu.registers[4] == UINT64_C(0x0123456709abcdef));
    execute(&cpu, code + offsets[9]); CHECK(cpu.registers[4] == UINT64_C(0x8123456709abcdef));
    execute(&cpu, code + offsets[10]); CHECK(cpu.registers[4] == UINT64_C(0x8123456809abcdef));
    cpu.registers[16] = UINT64_C(0xfeedface80000000); cpu.registers[17] = UINT64_C(0xcafebabeffffffff);
    cpu.registers[18] = UINT64_C(0x0123456789abcdef);
    execute(&cpu, code + offsets[11]); CHECK(cpu.registers[28] == UINT64_C(0x0123456809abcdef));
    execute(&cpu, code + offsets[12]); CHECK(cpu.registers[9] == UINT64_C(0x8123456809abcdef));
    cpu.registers[16] = UINT64_C(0x00000000ffffffff);
    execute(&cpu, code + offsets[13]); CHECK(cpu.registers[16] == UINT64_C(0x100000000));
    cpu.registers[30] = UINT64_C(0x5555555555555555);
    execute(&cpu, code + offsets[14]); CHECK(cpu.registers[30] == 0);
    cpu.registers[1] = UINT64_C(0xfffffffffffffffe); cpu.registers[2] = 3;
    execute(&cpu, code + offsets[15]); CHECK(cpu.registers[4] == UINT64_MAX);
    cpu.registers[1] = UINT64_MAX; cpu.registers[2] = UINT64_MAX;
    execute(&cpu, code + offsets[16]); CHECK(cpu.registers[4] == UINT64_C(0xfffffffffffffffe));
    cpu.registers[16] = UINT64_MAX; cpu.registers[17] = UINT64_MAX;
    execute(&cpu, code + offsets[17]); CHECK(cpu.registers[28] == UINT64_C(0xfffffffffffffffe));
    execute(&cpu, code + offsets[18]);
    cpu.registers[30] = UINT64_C(0x5555555555555555);
    execute(&cpu, code + offsets[19]); CHECK(cpu.registers[30] == UINT64_C(0x5555555555555555));
    CHECK(cpu.flags == UINT64_C(0xf0000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_MULTIPLY_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_multiply_emit(&assembler, words[0], 0xc000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_multiply_emit(&assembler, 0xd503201fu, 0xc000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_multiply_emit(&assembler, HIGH(1, 2, 1, 4) ^ (1u << 31), 0xc000));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_multiply_emit(&assembler, HIGH(1, 2, 1, 4) ^ (1u << 15), 0xc000));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_multiply_emit(&assembler, HIGH(1, 2, 1, 4) ^ (1u << 10), 0xc000));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_multiply_emit(&assembler, LONG(0, 0, 2, 3, 1, 4) ^ (1u << 31), 0xc000));
    return 0;
#endif
}
