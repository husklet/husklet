#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/fp_reduce.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "fp-reduce:%d: %s\n", __LINE__, #x); return 1; } } while (0)

typedef struct reduction_case {
    uint32_t word;
    uint8_t source[16];
    uint64_t fpcr;
    uint64_t fpsr;
    uint64_t expected;
    uint64_t expected_fpsr;
    unsigned bytes;
} reduction_case;

static uint32_t reduce(unsigned scalar, unsigned size, unsigned opcode,
                       unsigned rn, unsigned rd) {
    return UINT32_C(0x6e300800) | (scalar << 28) | (size << 22) |
           (opcode << 12) | (rn << 5) | rd;
}

static void put32(uint8_t source[16], unsigned lane, uint32_t value) {
    memcpy(source + lane * 4u, &value, sizeof(value));
}

static void put64(uint8_t source[16], unsigned lane, uint64_t value) {
    memcpy(source + lane * 8u, &value, sizeof(value));
}

static void basic(reduction_case *item, unsigned scalar, unsigned size, unsigned opcode,
                  unsigned rn, unsigned rd, unsigned rounding) {
    memset(item, 0, sizeof(*item));
    item->word = reduce(scalar, size, opcode, rn, rd);
    item->fpcr = (uint64_t)rounding << 22;
    item->fpsr = UINT64_C(0x10);
    item->expected_fpsr = UINT64_C(0x10);
    if (size & 1u) {
        put64(item->source, 0, UINT64_C(0x3ff0000000000000));
        put64(item->source, 1, UINT64_C(0xc000000000000000));
        item->expected = size & 2u ? UINT64_C(0xc000000000000000)
                                  : UINT64_C(0x3ff0000000000000);
        item->bytes = 8;
    } else {
        put32(item->source, 0, UINT32_C(0x3f800000));
        put32(item->source, 1, UINT32_C(0xc0000000));
        put32(item->source, 2, UINT32_C(0x40800000));
        put32(item->source, 3, UINT32_C(0x40400000));
        item->expected = size & 2u ? UINT32_C(0xc0000000)
                                  : scalar ? UINT32_C(0x3f800000) : UINT32_C(0x40800000);
        item->bytes = 4;
    }
}

static void special(reduction_case *item, unsigned numeric, unsigned minimum,
                    uint32_t a, uint32_t b, uint32_t c, uint32_t d,
                    uint64_t fpcr, uint64_t expected, uint64_t raised) {
    memset(item, 0, sizeof(*item));
    item->word = reduce(0, minimum ? 2u : 0u, numeric ? 0x0cu : 0x0fu, 7, 7);
    put32(item->source, 0, a);
    put32(item->source, 1, b);
    put32(item->source, 2, c);
    put32(item->source, 3, d);
    item->fpcr = fpcr;
    item->fpsr = UINT64_C(0x10);
    item->expected = expected;
    item->expected_fpsr = UINT64_C(0x10) | raised;
    item->bytes = 4;
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
    reduction_case cases[21];
    size_t count = 0;
    for (unsigned opcode_index = 0; opcode_index < 2; opcode_index++) {
        unsigned opcode = opcode_index ? 0x0fu : 0x0cu;
        for (unsigned size = 0; size <= 2; size += 2) {
            unsigned index = (unsigned)count;
            basic(&cases[count++], 0, size, opcode, 3 + index,
                  index % 3 == 0 ? 3 + index : 20 + index, index & 3u);
        }
        for (unsigned size = 0; size < 4; size++) {
            unsigned index = (unsigned)count;
            basic(&cases[count++], 1, size, opcode, 3 + index,
                  index % 3 == 0 ? 3 + index : 20 + index, index & 3u);
        }
    }
    CHECK(count == 12);
    special(&cases[count++], 0, 0, UINT32_C(0x7fc00123), UINT32_C(0x3f800000),
            UINT32_C(0x40000000), UINT32_C(0x40400000), 0, UINT32_C(0x7fc00123), 0);
    special(&cases[count++], 0, 1, UINT32_C(0x7fc00123), UINT32_C(0x3f800000),
            UINT32_C(0x7f800045), UINT32_C(0x40400000), 0, UINT32_C(0x7fc00123), 1);
    special(&cases[count++], 0, 1, UINT32_C(0x7fc00123), UINT32_C(0x3f800000),
            UINT32_C(0x7f800045), UINT32_C(0x40400000), UINT64_C(1) << 25,
            UINT32_C(0x7fc00000), 1);
    special(&cases[count++], 1, 0, UINT32_C(0x7fc00123), UINT32_C(0x3f800000),
            UINT32_C(0x40800000), UINT32_C(0x40400000), 0, UINT32_C(0x40800000), 0);
    special(&cases[count++], 1, 1, UINT32_C(0x7f800045), UINT32_C(0x3f800000),
            UINT32_C(0x40800000), UINT32_C(0x40400000), 0, UINT32_C(0x40400000), 1);
    special(&cases[count++], 0, 0, UINT32_C(0x80000000), UINT32_C(0),
            UINT32_C(0x80000000), UINT32_C(0), 0, UINT32_C(0), 0);
    special(&cases[count++], 0, 1, UINT32_C(0x80000000), UINT32_C(0),
            UINT32_C(0x80000000), UINT32_C(0), 0, UINT32_C(0x80000000), 0);
    special(&cases[count++], 0, 0, UINT32_C(0x80000001), UINT32_C(0x80000000),
            UINT32_C(0x80000001), UINT32_C(0x80000000), UINT64_C(1) << 24,
            UINT32_C(0x80000000), UINT64_C(0x80));
    special(&cases[count++], 0, 1, UINT32_C(1), UINT32_C(0), UINT32_C(1), UINT32_C(0),
            UINT64_C(1) << 24, UINT32_C(0), UINT64_C(0x80));
    CHECK(count == sizeof(cases) / sizeof(cases[0]));

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_fp_reduce_body(&assembler, cases[index].word));
        CHECK(memcmp(encoded, &cases[index].word, sizeof(encoded)) == 0);
    }
    const uint32_t invalid[] = {
        UINT32_C(0x00200000), reduce(0, 1, 0x0c, 1, 0), reduce(0, 3, 0x0f, 1, 0),
        reduce(0, 0, 0x0d, 1, 0), reduce(0, 0, 0x0c, 1, 0) ^ UINT32_C(0x20000000),
        reduce(0, 0, 0x0c, 1, 0) ^ UINT32_C(0x40000000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        uint32_t word = index == 0 ? cases[0].word ^ invalid[index] : invalid[index];
        CHECK(!hl_a64_simd_fp_reduce_body(&assembler, word));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
    }

    uint8_t short_buffer[HL_A64_SIMD_FP_REDUCE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_fp_reduce_emit(&assembler, cases[0].word, UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 16;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[21];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_fp_reduce_emit(&assembler, cases[index].word,
                                         UINT64_C(0x8000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    uint8_t stack[256] __attribute__((aligned(16)));
    for (size_t index = 0; index < count; index++) {
        hl_native_aarch64_cpu cpu;
        memset(&cpu, 0, sizeof(cpu));
        for (unsigned vector = 0; vector < 32; vector++)
            for (unsigned byte = 0; byte < 16; byte++)
                ((uint8_t *)cpu.vectors)[vector * 16 + byte] = (uint8_t)(vector * 19u + byte);
        for (unsigned reg = 0; reg < 31; reg++) cpu.registers[reg] = UINT64_C(0x4567000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0x90000000);
        cpu.fpcr = cases[index].fpcr;
        cpu.fpsr = cases[index].fpsr;
        unsigned rn = (cases[index].word >> 5) & 31u;
        memcpy(&cpu.vectors[rn * 2], cases[index].source, 16);
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
        if (memcmp(&cpu.vectors[rd * 2], result, 16) != 0) {
            uint64_t actual[2];
            memcpy(actual, &cpu.vectors[rd * 2], sizeof(actual));
            fprintf(stderr, "fp-reduce case %zu: got %016llx:%016llx expected %016llx\n", index,
                    (unsigned long long)actual[1], (unsigned long long)actual[0],
                    (unsigned long long)cases[index].expected);
            return 1;
        }
        for (unsigned vector = 0; vector < 32; vector++)
            if (vector != rd)
                CHECK(memcmp(&cpu.vectors[vector * 2], &vectors[vector * 2], 16) == 0);
    }
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
