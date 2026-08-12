#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_pairwise.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "pairwise:%d: %s\n", __LINE__, #x); return 1; } } while (0)

typedef struct pair_case {
    uint32_t word;
    uint64_t source[2];
    uint64_t fpcr;
    uint64_t fpsr;
    uint64_t expected;
    uint64_t expected_fpsr;
    unsigned bytes;
} pair_case;

static uint32_t pairwise(unsigned u, unsigned size, unsigned opcode,
                         unsigned rn, unsigned rd) {
    return UINT32_C(0x5e300800) | (u << 29) | (size << 22) |
           (opcode << 12) | (rn << 5) | rd;
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const pair_case cases[] = {
        {pairwise(0, 3, 0x1b, 1, 1), {UINT64_MAX, 1}, 0, UINT64_C(0x91), 0, UINT64_C(0x91), 8},
        {pairwise(1, 0, 0x0d, 2, 8), {UINT64_C(0x401000003fc00000), 0}, 0,
         UINT64_C(0x10), UINT32_C(0x40700000), UINT64_C(0x10), 4},
        {pairwise(1, 1, 0x0d, 3, 9), {UINT64_C(0x3ff8000000000000), UINT64_C(0x4002000000000000)}, 0,
         UINT64_C(0x10), UINT64_C(0x400e000000000000), UINT64_C(0x10), 8},
        {pairwise(1, 0, 0x0d, 4, 10), {UINT64_C(0x80000000), 0}, UINT64_C(2) << 22,
         UINT64_C(0x10), UINT32_C(0x80000000), UINT64_C(0x10), 4},
        {pairwise(1, 0, 0x0d, 5, 11), {UINT64_C(0x3f8000007f800045), 0}, 0,
         UINT64_C(0x10), UINT32_C(0x7fc00045), UINT64_C(0x11), 4},
        {pairwise(1, 0, 0x0d, 6, 6), {UINT64_C(0x3f8000007f800045), 0}, UINT64_C(1) << 25,
         UINT64_C(0x10), UINT32_C(0x7fc00000), UINT64_C(0x11), 4},
        {pairwise(1, 0, 0x0d, 7, 12), {UINT64_C(1), 0}, UINT64_C(1) << 24,
         UINT64_C(0x10), 0, UINT64_C(0x90), 4},
    };
    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_pairwise_body(&assembler, cases[index].word));
        CHECK(memcmp(encoded, &cases[index].word, sizeof(encoded)) == 0);
    }
    const uint32_t invalid[] = {
        pairwise(0, 0, 0x1b, 1, 0), pairwise(0, 2, 0x1b, 1, 0),
        pairwise(1, 2, 0x0d, 1, 0), pairwise(1, 3, 0x0d, 1, 0),
        pairwise(1, 0, 0x0c, 1, 0), pairwise(1, 0, 0x0f, 1, 0),
        pairwise(1, 0, 0x0d, 1, 0) ^ UINT32_C(0x10000000),
        pairwise(1, 0, 0x0d, 1, 0) ^ UINT32_C(0x40000000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_simd_pairwise_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
    }
    uint8_t short_buffer[HL_A64_SIMD_PAIRWISE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_pairwise_emit(&assembler, cases[0].word, UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[sizeof(cases) / sizeof(cases[0])];
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_pairwise_emit(&assembler, cases[index].word, UINT64_C(0x8000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    uint8_t stack[256] __attribute__((aligned(16)));
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        hl_native_aarch64_cpu cpu;
        memset(&cpu, 0, sizeof(cpu));
        for (unsigned vector = 0; vector < 32; vector++)
            for (unsigned byte = 0; byte < 16; byte++)
                ((uint8_t *)cpu.vectors)[vector * 16 + byte] = (uint8_t)(vector * 37u + byte * 13u);
        unsigned rn = (cases[index].word >> 5) & 31u;
        memcpy(&cpu.vectors[rn * 2], cases[index].source, 16);
        for (unsigned reg = 0; reg < 31; reg++) cpu.registers[reg] = UINT64_C(0x5678000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0x90000000);
        cpu.fpcr = cases[index].fpcr;
        cpu.fpsr = cases[index].fpsr;
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x8004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0x90000000) && cpu.fpcr == cases[index].fpcr &&
              cpu.fpsr == cases[index].expected_fpsr);
        CHECK(memcmp(cpu.registers, registers, sizeof(registers)) == 0);
        unsigned rd = cases[index].word & 31u;
        uint8_t result[16] = {0};
        memcpy(result, &cases[index].expected, cases[index].bytes);
        CHECK(memcmp(&cpu.vectors[rd * 2], result, 16) == 0);
        for (unsigned vector = 0; vector < 32; vector++)
            if (vector != rd)
                CHECK(memcmp(&cpu.vectors[vector * 2], &vectors[vector * 2], 16) == 0);
    }
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
