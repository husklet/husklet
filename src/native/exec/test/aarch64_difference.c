#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_difference.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "difference:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t same_width(unsigned q, unsigned u, unsigned size, unsigned opcode,
                           unsigned rm, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200400) | (q << 30) | (u << 29) | (size << 22) |
           (rm << 16) | (opcode << 11) | (rn << 5) | rd;
}

static uint32_t widening(unsigned q, unsigned u, unsigned size, unsigned opcode,
                         unsigned rm, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200000) | (q << 30) | (u << 29) | (size << 22) |
           (rm << 16) | (opcode << 12) | (rn << 5) | rd;
}

static uint64_t load_lane(const uint8_t *source, unsigned lane, unsigned bytes) {
    uint64_t value = 0;
    memcpy(&value, source + lane * bytes, bytes);
    return value;
}

static void store_lane(uint8_t *destination, unsigned lane, unsigned bytes, uint64_t value) {
    memcpy(destination + lane * bytes, &value, bytes);
}

static int64_t sign_extend(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    return (int64_t)((value ^ sign) - sign);
}

static uint64_t lane_mask(unsigned bits) {
    return bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
}

static uint64_t absolute_difference(uint64_t left, uint64_t right,
                                    unsigned bits, int unsigned_elements) {
    uint64_t mask = lane_mask(bits);
    left &= mask;
    right &= mask;
    if (unsigned_elements) return left > right ? left - right : right - left;
    int64_t signed_left = sign_extend(left, bits);
    int64_t signed_right = sign_extend(right, bits);
    return (uint64_t)(signed_left > signed_right
                          ? signed_left - signed_right
                          : signed_right - signed_left);
}

static void expected_same_width(uint32_t word, const uint8_t before[32][16],
                                uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u;
    unsigned u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 11) & 31u;
    unsigned rm = (word >> 16) & 31u;
    unsigned rn = (word >> 5) & 31u;
    unsigned rd = word & 31u;
    unsigned bytes = 1u << size;
    unsigned bits = bytes * 8u;
    unsigned lanes = (q ? 16u : 8u) / bytes;
    uint64_t mask = lane_mask(bits);
    memset(output, 0, 16);
    for (unsigned lane = 0; lane < lanes; lane++) {
        uint64_t difference = absolute_difference(load_lane(before[rn], lane, bytes),
                                                  load_lane(before[rm], lane, bytes), bits, u);
        if (opcode == 0x0fu) difference += load_lane(before[rd], lane, bytes);
        store_lane(output, lane, bytes, difference & mask);
    }
}

static void expected_widening(uint32_t word, const uint8_t before[32][16],
                              uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u;
    unsigned u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 12) & 15u;
    unsigned rm = (word >> 16) & 31u;
    unsigned rn = (word >> 5) & 31u;
    unsigned rd = word & 31u;
    unsigned narrow_bytes = 1u << size;
    unsigned wide_bytes = narrow_bytes * 2u;
    unsigned narrow_bits = narrow_bytes * 8u;
    unsigned wide_bits = wide_bytes * 8u;
    unsigned lanes = 8u / narrow_bytes;
    unsigned source_base = q ? lanes : 0u;
    uint64_t wide_mask = lane_mask(wide_bits);
    memset(output, 0, 16);
    for (unsigned lane = 0; lane < lanes; lane++) {
        uint64_t difference = absolute_difference(
            load_lane(before[rn], source_base + lane, narrow_bytes),
            load_lane(before[rm], source_base + lane, narrow_bytes), narrow_bits, u);
        if (opcode == 0x05u) difference += load_lane(before[rd], lane, wide_bytes);
        store_lane(output, lane, wide_bytes, difference & wide_mask);
    }
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void append_same(uint32_t words[48], size_t *count, unsigned q, unsigned u,
                        unsigned size, unsigned opcode) {
    unsigned index = (unsigned)*count;
    unsigned rn = (index * 5u + 1u) & 31u;
    unsigned rm = (index * 9u + 3u) & 31u;
    unsigned rd = index % 3u == 0 ? rn : index % 3u == 1 ? rm : (index * 13u + 7u) & 31u;
    words[(*count)++] = same_width(q, u, size, opcode, rm, rn, rd);
}

static void append_widening(uint32_t words[48], size_t *count, unsigned q, unsigned u,
                            unsigned size, unsigned opcode) {
    unsigned index = (unsigned)*count;
    unsigned rn = (index * 5u + 1u) & 31u;
    unsigned rm = (index * 9u + 3u) & 31u;
    unsigned rd = index % 3u == 0 ? rn : index % 3u == 1 ? rm : (index * 13u + 7u) & 31u;
    words[(*count)++] = widening(q, u, size, opcode, rm, rn, rd);
}

static void seed_vectors(hl_native_aarch64_cpu *cpu) {
    for (unsigned vector = 0; vector < 32; vector++)
        for (unsigned byte = 0; byte < 16; byte++)
            ((uint8_t *)cpu->vectors)[vector * 16 + byte] =
                (uint8_t)(0x81u + vector * 29u + byte * 43u);
}

static void force_boundaries(uint32_t word, uint8_t vectors[32][16]) {
    unsigned u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned rm = (word >> 16) & 31u;
    unsigned rn = (word >> 5) & 31u;
    unsigned rd = word & 31u;
    unsigned bytes = 1u << size;
    unsigned bits = bytes * 8u;
    uint64_t mask = lane_mask(bits);
    uint64_t minimum = UINT64_C(1) << (bits - 1u);
    int long_form = (word & UINT32_C(0x00000c00)) == 0;
    unsigned lanes = 8u / bytes;

    /* Low and high halves deliberately disagree, so Q-half selection is observable. */
    store_lane(vectors[rn], 0, bytes, u ? 0 : minimum);
    store_lane(vectors[rm], 0, bytes, u ? mask : minimum - 1u);
    store_lane(vectors[rn], lanes, bytes, u ? mask : minimum - 1u);
    store_lane(vectors[rm], lanes, bytes, u ? 0 : minimum);
    if (lanes > 1u) {
        store_lane(vectors[rn], 1, bytes, 7);
        store_lane(vectors[rm], 1, bytes, 7); /* equality */
        store_lane(vectors[rn], lanes + 1u, bytes, 1);
        store_lane(vectors[rm], lanes + 1u, bytes, 9); /* reversed ordering */
    }

    if (long_form && ((word >> 12) & 15u) == 0x05u && rd != rn && rd != rm) {
        unsigned wide_bytes = bytes * 2u;
        unsigned wide_bits = bits * 2u;
        /* A one-unit difference added to max proves wide modular wrap and snapshot use. */
        store_lane(vectors[rd], 0, wide_bytes, lane_mask(wide_bits));
        unsigned source = ((word >> 30) & 1u) ? lanes : 0u;
        store_lane(vectors[rn], source, bytes, 1);
        store_lane(vectors[rm], source, bytes, 0);
    }
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[48];
    size_t count = 0;
    for (unsigned opcode = 0x0eu; opcode <= 0x0fu; opcode++)
        for (unsigned u = 0; u <= 1; u++)
            for (unsigned q = 0; q <= 1; q++)
                for (unsigned size = 0; size < 3; size++)
                    append_same(words, &count, q, u, size, opcode);
    for (unsigned opcode = 0x05u; opcode <= 0x07u; opcode += 2u)
        for (unsigned u = 0; u <= 1; u++)
            for (unsigned q = 0; q <= 1; q++)
                for (unsigned size = 0; size < 3; size++)
                    append_widening(words, &count, q, u, size, opcode);
    CHECK(count == 48);

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        memset(encoded, 0, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_difference_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }

    const uint32_t invalid[] = {
        same_width(1, 0, 3, 0x0e, 2, 1, 0),
        same_width(1, 1, 3, 0x0f, 2, 1, 0),
        widening(0, 0, 3, 0x05, 2, 1, 0),
        widening(1, 1, 3, 0x07, 2, 1, 0),
        same_width(1, 0, 0, 0x10, 2, 1, 0),
        widening(1, 0, 0, 0x06, 2, 1, 0),
        same_width(1, 0, 0, 0x0e, 2, 1, 0) ^ UINT32_C(0x10000000),
        widening(1, 0, 0, 0x05, 2, 1, 0) ^ UINT32_C(0x10000000),
        same_width(1, 0, 0, 0x0e, 2, 1, 0) ^ UINT32_C(0x00200000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_simd_difference_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
        for (size_t byte = 0; byte < sizeof(encoded); byte++) CHECK(encoded[byte] == 0xa5);
    }

    uint8_t short_buffer[HL_A64_SIMD_DIFFERENCE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_difference_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t byte = 0; byte < sizeof(short_buffer); byte++) CHECK(short_buffer[byte] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 20u;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[48];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_difference_emit(&assembler, words[index],
                                           UINT64_C(0x8000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    uint8_t stack[256] __attribute__((aligned(16)));
    for (size_t index = 0; index < count; index++) {
        hl_native_aarch64_cpu cpu;
        memset(&cpu, 0, sizeof(cpu));
        seed_vectors(&cpu);
        force_boundaries(words[index], (uint8_t (*)[16])cpu.vectors);
        for (unsigned reg = 0; reg < 31; reg++)
            cpu.registers[reg] = UINT64_C(0x89ab000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0x90000000);
        cpu.fpcr = UINT64_C(0x00400000);
        cpu.fpsr = UINT64_C(0x08000091);

        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        uint8_t result[16];
        if (index < 24)
            expected_same_width(words[index], (const uint8_t (*)[16])vectors, result);
        else
            expected_widening(words[index], (const uint8_t (*)[16])vectors, result);

        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
        CHECK(cpu.program == UINT64_C(0x8004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0x90000000));
        CHECK(cpu.fpcr == UINT64_C(0x00400000));
        CHECK(cpu.fpsr == UINT64_C(0x08000091));
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
