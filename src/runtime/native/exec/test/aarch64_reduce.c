#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_reduce.h"
#include "../src/arch/aarch64/stub.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "reduce:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t reduce(unsigned q, unsigned u, unsigned size, unsigned opcode,
                       unsigned rn, unsigned rd) {
    return UINT32_C(0x0e300800) | (q << 30) | (u << 29) | (size << 22) |
           (opcode << 12) | (rn << 5) | rd;
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
            ((uint8_t *)cpu->vectors)[vector * 16 + byte] =
                (uint8_t)(0x81u + vector * 29u + byte * 43u);
    for (unsigned reg = 0; reg < 31; reg++) cpu->registers[reg] = UINT64_C(0x7654000000000000) + reg;
    cpu->stack = stack;
    cpu->flags = UINT64_C(0xa0000000);
    cpu->fpcr = UINT64_C(0x00400000);
    cpu->fpsr = UINT64_C(0x00000090);
}

static uint64_t lane(const uint8_t *source, unsigned index, unsigned bytes) {
    uint64_t value = 0;
    memcpy(&value, source + index * bytes, bytes);
    return value;
}

static int64_t signed_lane(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    return (int64_t)((value ^ sign) - sign);
}

static void expected(uint32_t word, const uint8_t before[32][16], uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u, u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u, opcode = (word >> 12) & 31u;
    unsigned rn = (word >> 5) & 31u;
    unsigned bytes = 1u << size, bits = bytes * 8u;
    unsigned count = (q ? 16u : 8u) / bytes;
    uint64_t result = lane(before[rn], 0, bytes);
    if (opcode == 0x03u) {
        result = 0;
        for (unsigned index = 0; index < count; index++) {
            uint64_t value = lane(before[rn], index, bytes);
            result += u ? value : (uint64_t)signed_lane(value, bits);
        }
        bytes *= 2u;
    } else if (opcode == 0x1bu) {
        for (unsigned index = 1; index < count; index++) result += lane(before[rn], index, bytes);
    } else {
        for (unsigned index = 1; index < count; index++) {
            uint64_t value = lane(before[rn], index, bytes);
            int take = u ? (opcode == 0x0au ? value > result : value < result)
                         : (opcode == 0x0au
                                ? signed_lane(value, bits) > signed_lane(result, bits)
                                : signed_lane(value, bits) < signed_lane(result, bits));
            if (take) result = value;
        }
    }
    memset(output, 0, 16);
    memcpy(output, &result, bytes);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[35];
    size_t count = 0;
    static const unsigned opcodes[] = {0x03u, 0x0au, 0x1au, 0x1bu};
    for (size_t operation = 0; operation < sizeof(opcodes) / sizeof(opcodes[0]); operation++)
        for (unsigned u = 0; u <= (opcodes[operation] == 0x1bu ? 0u : 1u); u++)
            for (unsigned q = 0; q <= 1; q++)
                for (unsigned size = 0; size < 3; size++) {
                    if (!q && size == 2) continue;
                    unsigned rn = (unsigned)(count * 7u + 3u) & 31u;
                    unsigned rd = count % 4u == 0 ? rn : (unsigned)(count * 11u + 5u) & 31u;
                    words[count++] = reduce(q, u, size, opcodes[operation], rn, rd);
                }
    CHECK(count == sizeof(words) / sizeof(words[0]));

    uint8_t encoded[4];
    hl_a64_assembler assembler;
    for (size_t index = 0; index < count; index++) {
        memset(encoded, 0, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_reduce_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }
    static const uint32_t invalid[] = {
        UINT32_C(0x00200000),
        UINT32_C(0x00000400),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_simd_reduce_body(&assembler, words[0] ^ invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
    }
    CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
    CHECK(!hl_a64_simd_reduce_body(&assembler, reduce(0, 0, 2, 0x03, 1, 0)));
    CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
    CHECK(!hl_a64_simd_reduce_body(&assembler, reduce(1, 0, 3, 0x03, 1, 0)));
    CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
    CHECK(!hl_a64_simd_reduce_body(&assembler, reduce(1, 1, 0, 0x1b, 1, 0)));
    CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
    CHECK(!hl_a64_simd_reduce_body(&assembler, reduce(1, 0, 0, 0x0b, 1, 0)));

    uint8_t short_buffer[HL_A64_SIMD_REDUCE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_reduce_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 16;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[35];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_reduce_emit(&assembler, words[index], UINT64_C(0x8000) + index * 4));
    }
    size_t chained = hl_a64_assembler_size(&assembler);
    hl_a64_stub_prologue(&assembler);
    CHECK(hl_a64_simd_reduce_body(&assembler, reduce(1, 0, 0, 0x1b, 1, 0)));
    CHECK(hl_a64_simd_reduce_body(&assembler, reduce(1, 1, 0, 0x0a, 0, 2)));
    hl_a64_stub_exit(&assembler, HL_NATIVE_EXIT_BRANCH, UINT64_C(0xa008));
    CHECK(hl_a64_assembler_ok(&assembler));
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    for (size_t index = 0; index < count; index++) {
        hl_native_aarch64_cpu cpu;
        seed(&cpu, (uint64_t)(uintptr_t)(code + capacity - 16));
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        uint8_t result[16];
        expected(words[index], (const uint8_t (*)[16])vectors, result);
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x8004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0xa0000000) && cpu.fpcr == UINT64_C(0x00400000) &&
              cpu.fpsr == UINT64_C(0x00000090));
        CHECK(memcmp(cpu.registers, registers, sizeof(registers)) == 0);
        unsigned destination = words[index] & 31u;
        CHECK(memcmp(&cpu.vectors[destination * 2], result, 16) == 0);
        for (unsigned vector = 0; vector < 32; vector++)
            if (vector != destination)
                CHECK(memcmp(&cpu.vectors[vector * 2], &vectors[vector * 2], 16) == 0);
    }
    hl_native_aarch64_cpu cpu;
    seed(&cpu, (uint64_t)(uintptr_t)(code + capacity - 16));
    uint8_t before[32][16], first[16], second[16];
    memcpy(before, cpu.vectors, sizeof(before));
    expected(reduce(1, 0, 0, 0x1b, 1, 0), before, first);
    memcpy(before[0], first, 16);
    expected(reduce(1, 1, 0, 0x0a, 0, 2), before, second);
    execute(&cpu, code + chained);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0xa008));
    CHECK(memcmp(&cpu.vectors[0], first, 16) == 0 && memcmp(&cpu.vectors[4], second, 16) == 0);
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
