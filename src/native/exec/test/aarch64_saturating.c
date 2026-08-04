#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_saturating.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "simd-saturating:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t saturating(unsigned q, unsigned u, unsigned size, unsigned opcode,
                           unsigned rm, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200400) | (q << 30) | (u << 29) | (size << 22) |
           (rm << 16) | (opcode << 11) | (rn << 5) | rd;
}

static uint64_t lane(const uint8_t *source, unsigned index, unsigned bytes) {
    uint64_t value = 0;
    memcpy(&value, source + index * bytes, bytes);
    return value;
}

static void put(uint8_t *destination, unsigned index, unsigned bytes, uint64_t value) {
    memcpy(destination + index * bytes, &value, bytes);
}

static int64_t signed_lane(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    return (int64_t)((value ^ sign) - sign);
}

static uint64_t signed_bits(__int128 value, unsigned bits, int *qc) {
    __int128 maximum = ((__int128)1 << (bits - 1u)) - 1;
    __int128 minimum = -maximum - 1;
    if (value > maximum) value = maximum, *qc = 1;
    if (value < minimum) value = minimum, *qc = 1;
    return (uint64_t)value;
}

static uint64_t unsigned_bits(__uint128_t value, unsigned bits, int negative, int *qc) {
    __uint128_t maximum = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    if (negative) return *qc = 1, 0;
    if (value > maximum) return *qc = 1, (uint64_t)maximum;
    return (uint64_t)value;
}

static int expected(uint32_t word, const uint8_t before[32][16], uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u, u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u, opcode = (word >> 11) & 31u;
    unsigned rm = (word >> 16) & 31u, rn = (word >> 5) & 31u;
    unsigned bytes = 1u << size, bits = bytes * 8u, count = (q ? 16u : 8u) / bytes;
    uint64_t mask = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    int qc = 0;
    memset(output, 0, 16);
    for (unsigned index = 0; index < count; index++) {
        uint64_t a = lane(before[rn], index, bytes) & mask;
        uint64_t b = lane(before[rm], index, bytes) & mask;
        uint64_t value;
        if (opcode == 0x00u || opcode == 0x02u || opcode == 0x04u) {
            if (u) {
                if (opcode == 0x04u)
                    value = (a - b) >> 1;
                else
                    value = (a + b + (opcode == 0x02u)) >> 1;
            } else {
                int64_t x = signed_lane(a, bits), y = signed_lane(b, bits);
                value = (uint64_t)(opcode == 0x04u ? (x - y) >> 1
                                                   : (x + y + (opcode == 0x02u)) >> 1);
            }
        } else if (opcode == 0x01u || opcode == 0x05u) {
            int subtract = opcode == 0x05u;
            if (u) {
                int negative = subtract && a < b;
                __uint128_t result = subtract ? (__uint128_t)(a - b) : (__uint128_t)a + b;
                value = unsigned_bits(result, bits, negative, &qc);
            } else {
                __int128 result = (__int128)signed_lane(a, bits) +
                                  (subtract ? -(__int128)signed_lane(b, bits)
                                            : (__int128)signed_lane(b, bits));
                value = signed_bits(result, bits, &qc);
            }
        } else {
            __int128 product = (__int128)signed_lane(a, bits) * signed_lane(b, bits) * 2;
            if (u) product += (__int128)1 << (bits - 1u);
            value = signed_bits(product >> bits, bits, &qc);
        }
        put(output, index, bytes, value & mask);
    }
    return qc;
}

static void force_boundary(uint32_t word, uint8_t vectors[32][16]) {
    unsigned u = (word >> 29) & 1u, size = (word >> 22) & 3u, opcode = (word >> 11) & 31u;
    unsigned rm = (word >> 16) & 31u, rn = (word >> 5) & 31u;
    unsigned bytes = 1u << size, bits = bytes * 8u;
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    uint64_t mask = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    if (opcode == 0x00u) {
        put(vectors[rn], 0, bytes, u ? 5u : mask - 2u); /* 5 or -3 */
        put(vectors[rm], 0, bytes, 2);
    } else if (opcode == 0x02u) {
        put(vectors[rn], 0, bytes, 2);
        put(vectors[rm], 0, bytes, 1); /* odd sum distinguishes rounded halving */
    } else if (opcode == 0x04u) {
        put(vectors[rn], 0, bytes, u ? 0 : mask - 2u); /* unsigned wrap or signed -3 */
        put(vectors[rm], 0, bytes, 2);
    } else if (opcode == 0x01u) {
        put(vectors[rn], 0, bytes, u ? mask : sign - 1u);
        put(vectors[rm], 0, bytes, 1);
    } else if (opcode == 0x05u) {
        put(vectors[rn], 0, bytes, u ? 0 : sign);
        put(vectors[rm], 0, bytes, 1);
    } else if (opcode == 0x16u) {
        put(vectors[rn], 0, bytes, sign);
        put(vectors[rm], 0, bytes, sign);
    }
}

static void append(uint32_t words[72], size_t *count, unsigned q, unsigned u,
                   unsigned size, unsigned opcode) {
    unsigned index = (unsigned)*count;
    unsigned rn = (index * 5u + 3u) & 31u, rm = (index * 9u + 7u) & 31u;
    unsigned rd = index % 3u == 0 ? rn : index % 3u == 1 ? rm : (index * 13u + 11u) & 31u;
    words[(*count)++] = saturating(q, u, size, opcode, rm, rn, rd);
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
    uint32_t words[72];
    size_t count = 0;
    static const unsigned halving[] = {0x00u, 0x02u, 0x04u};
    for (size_t operation = 0; operation < sizeof(halving) / sizeof(halving[0]); operation++)
        for (unsigned u = 0; u <= 1; u++)
            for (unsigned q = 0; q <= 1; q++)
                for (unsigned size = 0; size < 3; size++) append(words, &count, q, u, size, halving[operation]);
    static const unsigned saturated[] = {0x01u, 0x05u};
    for (size_t operation = 0; operation < sizeof(saturated) / sizeof(saturated[0]); operation++)
        for (unsigned u = 0; u <= 1; u++)
            for (unsigned q = 0; q <= 1; q++)
                for (unsigned size = 0; size < (q ? 4u : 3u); size++)
                    append(words, &count, q, u, size, saturated[operation]);
    for (unsigned u = 0; u <= 1; u++)
        for (unsigned q = 0; q <= 1; q++)
            for (unsigned size = 1; size <= 2; size++) append(words, &count, q, u, size, 0x16u);
    CHECK(count == sizeof(words) / sizeof(words[0]));

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_saturating_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }
    const uint32_t invalid[] = {
        saturating(1, 0, 3, 0x00, 2, 1, 0), saturating(1, 1, 3, 0x02, 2, 1, 0),
        saturating(1, 0, 3, 0x04, 2, 1, 0), saturating(0, 0, 3, 0x01, 2, 1, 0),
        saturating(0, 1, 3, 0x05, 2, 1, 0), saturating(1, 0, 0, 0x16, 2, 1, 0),
        saturating(1, 1, 3, 0x16, 2, 1, 0), saturating(1, 0, 0, 0x10, 2, 1, 0),
        words[0] ^ UINT32_C(0x00200000), words[0] ^ UINT32_C(0x10000000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_simd_saturating_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
    }

    uint8_t short_buffer[HL_A64_SIMD_SATURATING_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_saturating_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 24;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[72];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_saturating_emit(&assembler, words[index], UINT64_C(0x8000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    uint8_t stack[256] __attribute__((aligned(16)));
    for (size_t index = 0; index < count; index++) {
        hl_native_aarch64_cpu cpu;
        memset(&cpu, 0, sizeof(cpu));
        for (unsigned vector = 0; vector < 32; vector++)
            for (unsigned byte = 0; byte < 16; byte++)
                ((uint8_t *)cpu.vectors)[vector * 16 + byte] = (uint8_t)(0x91u + vector * 23u + byte * 41u);
        force_boundary(words[index], (uint8_t (*)[16])cpu.vectors);
        for (unsigned reg = 0; reg < 31; reg++) cpu.registers[reg] = UINT64_C(0x3456000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0x60000000);
        cpu.fpcr = UINT64_C(0x00400000);
        cpu.fpsr = UINT64_C(0x91) | (index & 1u ? UINT64_C(0x08000000) : 0);
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        uint8_t result[16];
        int qc = expected(words[index], (const uint8_t (*)[16])vectors, result);
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x8004) + index * 4);
        uint64_t expected_fpsr = UINT64_C(0x91) |
                                 (qc || (index & 1u) ? UINT64_C(0x08000000) : 0);
        CHECK(cpu.flags == UINT64_C(0x60000000) && cpu.fpcr == UINT64_C(0x00400000) &&
              cpu.fpsr == expected_fpsr);
        CHECK(memcmp(cpu.registers, registers, sizeof(registers)) == 0);
        unsigned rd = words[index] & 31u;
        CHECK(memcmp(&cpu.vectors[rd * 2], result, 16) == 0);
        for (unsigned vector = 0; vector < 32; vector++)
            if (vector != rd)
                CHECK(memcmp(&cpu.vectors[vector * 2], &vectors[vector * 2], 16) == 0);
    }
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
