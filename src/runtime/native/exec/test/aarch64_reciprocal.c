#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_reciprocal.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "reciprocal:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t estimate(unsigned scalar, unsigned q, unsigned u, unsigned size,
                         unsigned opcode, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200800) | (scalar << 28) | (q << 30) | (u << 29) |
           (size << 22) | (opcode << 12) | (rn << 5) | rd;
}

static uint32_t step(unsigned scalar, unsigned q, unsigned size, unsigned rm,
                     unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200400) | (scalar << 28) | (q << 30) | (size << 22) |
           (rm << 16) | (0x1fu << 11) | (rn << 5) | rd;
}

static unsigned reciprocal_table(unsigned value) {
    value = value * 2u + 1u;
    unsigned quotient = (1u << 19) / value;
    return (quotient + 1u) / 2u;
}

static unsigned rsqrt_table(unsigned value) {
    value = value < 256u ? value * 2u + 1u : (((value >> 1) << 1) + 1u) * 2u;
    uint64_t estimate = 512;
    while ((uint64_t)value * (estimate + 1u) * (estimate + 1u) < (UINT64_C(1) << 28)) estimate++;
    return (unsigned)((estimate + 1u) / 2u);
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void seed(hl_native_aarch64_cpu *cpu, uint64_t stack) {
    memset(cpu, 0, sizeof(*cpu));
    for (unsigned vector = 0; vector < 32; vector++)
        for (unsigned byte = 0; byte < 16; byte++)
            ((uint8_t *)cpu->vectors)[vector * 16 + byte] = (uint8_t)(vector * 17u + byte * 29u);
    for (unsigned reg = 0; reg < 31; reg++) cpu->registers[reg] = UINT64_C(0x6789000000000000) + reg;
    cpu->stack = stack;
    cpu->flags = UINT64_C(0xa0000000);
    cpu->fpsr = UINT64_C(0x10);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[24];
    size_t count = 0;
    for (unsigned u = 0; u <= 1; u++)
        for (unsigned size = 2; size <= 3; size++) words[count++] = estimate(1, 1, u, size, 0x1d, 1, 0);
    for (unsigned u = 0; u <= 1; u++) {
        words[count++] = estimate(0, 0, u, 2, 0x1d, 2, 3);
        words[count++] = estimate(0, 1, u, 2, 0x1d, 2, 3);
        words[count++] = estimate(0, 1, u, 3, 0x1d, 2, 3);
    }
    for (unsigned u = 0; u <= 1; u++)
        for (unsigned q = 0; q <= 1; q++) words[count++] = estimate(0, q, u, 2, 0x1c, 4, 5);
    for (unsigned size = 0; size < 4; size++) words[count++] = step(1, 1, size, 7, 6, 6);
    for (unsigned size = 0; size < 4; size++)
        for (unsigned q = size & 1u ? 1u : 0u; q <= 1; q++) words[count++] = step(0, q, size, 8, 9, 10);
    CHECK(count == sizeof(words) / sizeof(words[0]));
    words[18] = step(0, 0, 0, 8, 9, 8); /* Rd == Rm */

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_reciprocal_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }
    const uint32_t invalid[] = {
        estimate(1, 0, 0, 2, 0x1d, 1, 0), estimate(0, 0, 0, 3, 0x1d, 1, 0),
        estimate(1, 1, 0, 2, 0x1c, 1, 0), estimate(0, 1, 0, 3, 0x1c, 1, 0),
        estimate(0, 1, 0, 2, 0x1b, 1, 0), step(1, 0, 0, 2, 1, 0),
        step(0, 0, 1, 2, 1, 0), step(0, 1, 0, 2, 1, 0) ^ UINT32_C(0x20000000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_simd_reciprocal_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
    }
    uint8_t short_buffer[HL_A64_SIMD_RECIPROCAL_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_reciprocal_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 12;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[24];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_reciprocal_emit(&assembler, words[index], UINT64_C(0x8000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    uint8_t stack[256] __attribute__((aligned(16)));

    for (size_t index = 0; index < count; index++) {
        hl_native_aarch64_cpu cpu;
        seed(&cpu, (uint64_t)(uintptr_t)(stack + sizeof(stack)));
        unsigned size = (words[index] >> 22) & 3u;
        unsigned rn = (words[index] >> 5) & 31u, rm = (words[index] >> 16) & 31u;
        unsigned opcode12 = (words[index] >> 12) & 31u;
        uint64_t one = opcode12 == 0x1cu ? UINT32_C(0x80000000)
                       : size & 1u ? UINT64_C(0x3ff0000000000000) : UINT32_C(0x3f800000);
        for (unsigned byte = 0; byte < 16; byte += size & 1u ? 8u : 4u) {
            memcpy((uint8_t *)&cpu.vectors[rn * 2] + byte, &one, size & 1u ? 8u : 4u);
            memcpy((uint8_t *)&cpu.vectors[rm * 2] + byte, &one, size & 1u ? 8u : 4u);
        }
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x8004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0xa0000000) && cpu.fpcr == 0 && cpu.fpsr == UINT64_C(0x10));
        CHECK(memcmp(cpu.registers, registers, sizeof(registers)) == 0);
        unsigned rd = words[index] & 31u;
        uint64_t expected;
        unsigned result_bytes;
        if (index < 14) {
            if (opcode12 == 0x1cu) {
                expected = (uint64_t)(index >= 12 ? rsqrt_table(256u) : reciprocal_table(256u)) << 23;
                result_bytes = 4;
            } else if (size & 1u) {
                unsigned table = (words[index] >> 29) & 1u ? rsqrt_table(128u) : reciprocal_table(256u);
                unsigned exponent = 1022u;
                expected = ((uint64_t)exponent << 52) | ((uint64_t)(table & 255u) << 44);
                result_bytes = 8;
            } else {
                unsigned table = (words[index] >> 29) & 1u ? rsqrt_table(128u) : reciprocal_table(256u);
                unsigned exponent = 126u;
                expected = (exponent << 23) | ((table & 255u) << 15);
                result_bytes = 4;
            }
        } else {
            expected = size & 1u ? UINT64_C(0x3ff0000000000000) : UINT32_C(0x3f800000);
            result_bytes = size & 1u ? 8u : 4u;
        }
        if (memcmp(&cpu.vectors[rd * 2], &expected, result_bytes) != 0) {
            fprintf(stderr, "reciprocal smoke %zu result mismatch\n", index);
            return 1;
        }
        unsigned q = (words[index] >> 30) & 1u, scalar = (words[index] >> 28) & 1u;
        if (q && !scalar)
            CHECK(memcmp((uint8_t *)&cpu.vectors[rd * 2] + result_bytes, &expected, result_bytes) == 0);
        if (scalar || !q)
            for (unsigned byte = scalar ? result_bytes : 8u; byte < 16; byte++)
                CHECK(((uint8_t *)&cpu.vectors[rd * 2])[byte] == 0);
        for (unsigned vector = 0; vector < 32; vector++)
            if (vector != rd)
                CHECK(memcmp(&cpu.vectors[vector * 2], &vectors[vector * 2], 16) == 0);
    }

    for (unsigned index = 0; index < 256; index++) {
        hl_native_aarch64_cpu cpu;
        seed(&cpu, (uint64_t)(uintptr_t)(stack + sizeof(stack)));
        uint32_t input = (UINT32_C(127) << 23) | (index << 15);
        memcpy(&cpu.vectors[2], &input, 4);
        execute(&cpu, code + offsets[0]);
        uint32_t actual, expected = (UINT32_C(126) << 23) | ((reciprocal_table(256u + index) & 255u) << 15);
        memcpy(&actual, &cpu.vectors[0], 4);
        CHECK(actual == expected && cpu.fpsr == UINT64_C(0x10));
    }
    for (unsigned scaled = 128; scaled < 512; scaled++) {
        hl_native_aarch64_cpu cpu;
        seed(&cpu, (uint64_t)(uintptr_t)(stack + sizeof(stack)));
        unsigned exponent = scaled < 256 ? 127u : 128u;
        uint32_t fraction = scaled < 256 ? (scaled - 128u) << 16 : (scaled - 256u) << 15;
        uint32_t input = (exponent << 23) | fraction;
        memcpy(&cpu.vectors[2], &input, 4);
        execute(&cpu, code + offsets[2]);
        unsigned result_exponent = (3u * 127u - 1u - exponent) / 2u;
        uint32_t expected = (result_exponent << 23) | ((rsqrt_table(scaled) & 255u) << 15), actual;
        memcpy(&actual, &cpu.vectors[0], 4);
        CHECK(actual == expected && cpu.fpsr == UINT64_C(0x10));
    }

    for (unsigned index = 0; index < 256; index++) {
        hl_native_aarch64_cpu cpu;
        seed(&cpu, (uint64_t)(uintptr_t)(stack + sizeof(stack)));
        uint32_t input = (256u + index) << 23;
        memcpy(&cpu.vectors[8], &input, 4);
        execute(&cpu, code + offsets[10]);
        uint32_t actual, expected = reciprocal_table(256u + index) << 23;
        memcpy(&actual, &cpu.vectors[10], 4);
        CHECK(actual == expected && cpu.fpsr == UINT64_C(0x10));
    }
    for (unsigned scaled = 128; scaled < 512; scaled++) {
        hl_native_aarch64_cpu cpu;
        seed(&cpu, (uint64_t)(uintptr_t)(stack + sizeof(stack)));
        uint32_t input = scaled << 23;
        memcpy(&cpu.vectors[8], &input, 4);
        execute(&cpu, code + offsets[12]);
        uint32_t actual, expected = rsqrt_table(scaled) << 23;
        memcpy(&actual, &cpu.vectors[10], 4);
        CHECK(actual == expected && cpu.fpsr == UINT64_C(0x10));
    }

    struct special { size_t offset; uint32_t a, b, expected; uint64_t fpcr, fpsr; } specials[] = {
        {0, 0, 0, UINT32_C(0x7f800000), 0, UINT64_C(0x12)},
        {2, UINT32_C(0xbf800000), 0, UINT32_C(0x7fc00000), 0, UINT64_C(0x11)},
        {0, UINT32_C(0x7f800045), 0, UINT32_C(0x7fc00000), UINT64_C(1) << 25, UINT64_C(0x11)},
        {0, 1, 0, UINT32_C(0x7f800000), UINT64_C(1) << 24, UINT64_C(0x92)},
        {0, UINT32_C(0x80000000), 0, UINT32_C(0xff800000), 0, UINT64_C(0x12)},
        {0, UINT32_C(0x7f800000), 0, 0, 0, UINT64_C(0x10)},
        {0, 1, 0, UINT32_C(0x7f800000), 0, UINT64_C(0x14)},
        {0, 1, 0, UINT32_C(0x7f7fffff), UINT64_C(3) << 22, UINT64_C(0x14)},
        {0, UINT32_C(0x7e800000), 0, 0, UINT64_C(1) << 24, UINT64_C(0x08)},
        {0, UINT32_C(0x7fc00123), 0, UINT32_C(0x7fc00123), 0, UINT64_C(0x10)},
        {14, 0, UINT32_C(0x7f800000), UINT32_C(0x40000000), 0, UINT64_C(0x10)},
        {16, 0, UINT32_C(0x7f800000), UINT32_C(0x3fc00000), 0, UINT64_C(0x10)},
    };
    for (size_t index = 0; index < sizeof(specials) / sizeof(specials[0]); index++) {
        hl_native_aarch64_cpu cpu;
        seed(&cpu, (uint64_t)(uintptr_t)(stack + sizeof(stack)));
        if (index >= 6 && index <= 8) cpu.fpsr = 0;
        cpu.fpcr = specials[index].fpcr;
        uint32_t word = words[specials[index].offset];
        unsigned rn = (word >> 5) & 31u, rm = (word >> 16) & 31u, rd = word & 31u;
        memcpy((uint8_t *)&cpu.vectors[rn * 2], &specials[index].a, 4);
        if (specials[index].offset >= 14)
            memcpy((uint8_t *)&cpu.vectors[rm * 2], &specials[index].b, 4);
        execute(&cpu, code + offsets[specials[index].offset]);
        uint32_t actual;
        memcpy(&actual, (uint8_t *)&cpu.vectors[rd * 2], 4);
        if (actual != specials[index].expected || cpu.fpsr != specials[index].fpsr) {
            fprintf(stderr, "reciprocal special %zu: result=%08x fpsr=%llx\n", index, actual,
                    (unsigned long long)cpu.fpsr);
            return 1;
        }
    }
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
