#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/saturating_narrow.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "satnarrow:%d: %s\n", __LINE__, #x); return 1; } } while (0)

#define QC UINT64_C(0x08000000)

static uint32_t narrow(unsigned scalar, unsigned q, unsigned u, unsigned size,
                       unsigned opcode, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200800) | (scalar << 28) | (q << 30) | (u << 29) |
           (size << 22) | (opcode << 12) | (rn << 5) | rd;
}

static uint64_t mask(unsigned bits) {
    return bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
}

static int64_t signed_value(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    return (int64_t)((value ^ sign) - sign);
}

static uint64_t load_lane(const uint8_t *source, unsigned lane, unsigned bytes) {
    uint64_t value = 0;
    memcpy(&value, source + lane * bytes, bytes);
    return value;
}

static void store_lane(uint8_t *destination, unsigned lane, unsigned bytes, uint64_t value) {
    memcpy(destination + lane * bytes, &value, bytes);
}

static int clamp_lane(uint32_t word, uint64_t wide, uint64_t *result) {
    unsigned u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 12) & 31u;
    unsigned bits = 8u << size;
    uint64_t maximum = mask(bits);
    if (opcode == 0x14u && !u) {
        int64_t value = signed_value(wide, bits * 2u);
        int64_t minimum = -(INT64_C(1) << (bits - 1u));
        int64_t max_signed = (INT64_C(1) << (bits - 1u)) - 1;
        if (value < minimum) { *result = (uint64_t)minimum & maximum; return 1; }
        if (value > max_signed) { *result = (uint64_t)max_signed; return 1; }
        *result = (uint64_t)value & maximum;
        return 0;
    }
    if (opcode == 0x12u) {
        int64_t value = signed_value(wide, bits * 2u);
        if (value < 0) { *result = 0; return 1; }
        if ((uint64_t)value > maximum) { *result = maximum; return 1; }
        *result = (uint64_t)value;
        return 0;
    }
    if (wide > maximum) { *result = maximum; return 1; }
    *result = wide;
    return 0;
}

static int expected(uint32_t word, const uint8_t before[32][16], uint8_t output[16]) {
    unsigned scalar = (word >> 28) & 1u;
    unsigned q = (word >> 30) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned rn = (word >> 5) & 31u;
    unsigned rd = word & 31u;
    unsigned narrow_bytes = 1u << size;
    unsigned wide_bytes = narrow_bytes * 2u;
    unsigned lanes = scalar ? 1u : 8u / narrow_bytes;
    uint8_t packed[8] = {0};
    int saturated = 0;
    for (unsigned lane = 0; lane < lanes; lane++) {
        uint64_t value;
        saturated |= clamp_lane(word, load_lane(before[rn], lane, wide_bytes), &value);
        store_lane(packed, lane, narrow_bytes, value);
    }
    memset(output, 0, 16);
    if (!scalar && q) memcpy(output, before[rd], 8);
    memcpy(output + (!scalar && q ? 8 : 0), packed, scalar ? narrow_bytes : 8);
    return saturated;
}

static void force_values(uint32_t word, unsigned index, uint8_t vectors[32][16]) {
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 12) & 31u;
    unsigned u = (word >> 29) & 1u;
    unsigned rn = (word >> 5) & 31u;
    unsigned bytes = 2u << size;
    unsigned bits = 8u << size;
    unsigned lanes = ((word >> 28) & 1u) ? 1u : 64u / bits;
    uint64_t narrow_max = mask(bits);
    uint64_t wide_max = mask(bits * 2u);
    uint64_t values[4] = {1, narrow_max, 0, narrow_max > 1 ? narrow_max - 1 : 0};
    if (index % 4u != 0) {
        if (opcode == 0x14u && !u) {
            values[0] = (UINT64_C(1) << (bits - 1u));
            values[1] = (uint64_t)(-(INT64_C(1) << (bits - 1u)) - 1) & wide_max;
        } else if (opcode == 0x12u) {
            values[0] = wide_max;
            values[1] = narrow_max + 1u;
        } else {
            values[0] = narrow_max + 1u;
            values[1] = wide_max;
        }
    } else if (opcode == 0x14u && !u) {
        values[0] = (UINT64_C(1) << (bits - 1u)) - 1u;
        values[1] = (uint64_t)(-(INT64_C(1) << (bits - 1u))) & wide_max;
    }
    for (unsigned lane = 0; lane < lanes; lane++)
        store_lane(vectors[rn], lane, bytes, values[lane & 3u]);
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void append(uint32_t words[27], size_t *count, unsigned scalar, unsigned q,
                   unsigned u, unsigned size, unsigned opcode) {
    unsigned index = (unsigned)*count;
    unsigned rn = (index * 7u + 3u) & 31u;
    unsigned rd = index % 3u == 0 ? rn : (index * 11u + 5u) & 31u;
    words[(*count)++] = narrow(scalar, q, u, size, opcode, rn, rd);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[27];
    size_t count = 0;
    for (unsigned operation = 0; operation < 3; operation++)
        for (unsigned q = 0; q <= 1; q++)
            for (unsigned size = 0; size < 3; size++)
                append(words, &count, 0, q, operation != 0, size,
                       operation == 2 ? 0x12u : 0x14u);
    for (unsigned operation = 0; operation < 3; operation++)
        for (unsigned size = 0; size < 3; size++)
            append(words, &count, 1, 1, operation != 0, size,
                   operation == 2 ? 0x12u : 0x14u);
    CHECK(count == 27);

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_saturating_narrow_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }
    const uint32_t invalid[] = {
        narrow(0, 0, 0, 0, 0x12, 1, 0), narrow(1, 1, 0, 0, 0x12, 1, 0),
        narrow(0, 1, 0, 3, 0x14, 1, 0), narrow(1, 0, 0, 0, 0x14, 1, 0),
        narrow(0, 1, 0, 0, 0x13, 1, 0), narrow(0, 1, 0, 0, 0x14, 1, 0) ^ UINT32_C(0x00000400),
        narrow(0, 1, 0, 0, 0x14, 1, 0) ^ UINT32_C(0x00200000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_saturating_narrow_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
        for (size_t byte = 0; byte < sizeof(encoded); byte++) CHECK(encoded[byte] == 0xa5);
    }
    uint8_t short_buffer[HL_A64_SATURATING_NARROW_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_saturating_narrow_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t byte = 0; byte < sizeof(short_buffer); byte++) CHECK(short_buffer[byte] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 14u;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[27];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_saturating_narrow_emit(&assembler, words[index],
                                             UINT64_C(0x9000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    uint8_t stack[256] __attribute__((aligned(16)));
    for (size_t index = 0; index < count; index++) {
        hl_native_aarch64_cpu cpu;
        memset(&cpu, 0, sizeof(cpu));
        for (unsigned vector = 0; vector < 32; vector++)
            for (unsigned byte = 0; byte < 16; byte++)
                ((uint8_t *)cpu.vectors)[vector * 16 + byte] =
                    (uint8_t)(0x63u + vector * 31u + byte * 41u);
        force_values(words[index], (unsigned)index, (uint8_t (*)[16])cpu.vectors);
        for (unsigned reg = 0; reg < 31; reg++) cpu.registers[reg] = UINT64_C(0xcafe000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0xa0000000);
        cpu.fpcr = UINT64_C(0x00400000);
        cpu.fpsr = UINT64_C(0x91) | (index & 1u ? QC : 0);
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        uint8_t result[16];
        int saturated = expected(words[index], (const uint8_t (*)[16])vectors, result);
        uint64_t expected_fpsr = UINT64_C(0x91) | (saturated || (index & 1u) ? QC : 0);
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
        CHECK(cpu.program == UINT64_C(0x9004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0xa0000000));
        CHECK(cpu.fpcr == UINT64_C(0x00400000));
        CHECK(cpu.fpsr == expected_fpsr);
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
