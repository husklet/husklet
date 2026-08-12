#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_widening.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "widening:%d: %s\n", __LINE__, #x); return 1; } } while (0)

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

static uint64_t mask(unsigned bits) {
    return bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
}

static int64_t sign_extend(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    return (int64_t)((value ^ sign) - sign);
}

static uint64_t extend(uint64_t value, unsigned narrow_bits,
                       unsigned wide_bits, int unsigned_elements) {
    value &= mask(narrow_bits);
    return unsigned_elements ? value : (uint64_t)sign_extend(value, narrow_bits) & mask(wide_bits);
}

static void expected(uint32_t word, const uint8_t before[32][16], uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u;
    unsigned u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 12) & 15u;
    unsigned rm = (word >> 16) & 31u;
    unsigned rn = (word >> 5) & 31u;
    unsigned narrow_bytes = 1u << size;
    unsigned wide_bytes = narrow_bytes * 2u;
    unsigned narrow_bits = narrow_bytes * 8u;
    unsigned wide_bits = wide_bytes * 8u;
    unsigned lanes = 8u / narrow_bytes;
    unsigned source_base = q ? lanes : 0u;
    int wide_left = opcode == 1u || opcode == 3u;
    int subtract = opcode >= 2u;
    memset(output, 0, 16);
    for (unsigned lane = 0; lane < lanes; lane++) {
        uint64_t left = wide_left
                            ? load_lane(before[rn], lane, wide_bytes)
                            : extend(load_lane(before[rn], source_base + lane, narrow_bytes),
                                     narrow_bits, wide_bits, u);
        uint64_t right = extend(load_lane(before[rm], source_base + lane, narrow_bytes),
                                narrow_bits, wide_bits, u);
        store_lane(output, lane, wide_bytes,
                   (subtract ? left - right : left + right) & mask(wide_bits));
    }
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void append(uint32_t words[48], size_t *count, unsigned q, unsigned u,
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
                (uint8_t)(0x87u + vector * 31u + byte * 41u);
}

static void force_halves(uint32_t word, uint8_t vectors[32][16]) {
    unsigned u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 12) & 15u;
    unsigned rm = (word >> 16) & 31u;
    unsigned rn = (word >> 5) & 31u;
    unsigned bytes = 1u << size;
    unsigned bits = bytes * 8u;
    unsigned lanes = 8u / bytes;
    uint64_t narrow_mask = mask(bits);
    uint64_t minimum = UINT64_C(1) << (bits - 1u);

    /* Vm's halves disagree for every form, so Q selection is observable. */
    store_lane(vectors[rm], 0, bytes, u ? narrow_mask : minimum);
    store_lane(vectors[rm], lanes, bytes, u ? 1u : minimum - 1u);
    if (lanes > 1u) {
        store_lane(vectors[rm], 1, bytes, 0);
        store_lane(vectors[rm], lanes + 1u, bytes, narrow_mask);
    }

    if (opcode == 0u || opcode == 2u) {
        /* Long forms select the same half from both narrow operands. */
        store_lane(vectors[rn], 0, bytes, u ? 1u : minimum - 1u);
        store_lane(vectors[rn], lanes, bytes, u ? narrow_mask : minimum);
        if (lanes > 1u) {
            store_lane(vectors[rn], 1, bytes, narrow_mask);
            store_lane(vectors[rn], lanes + 1u, bytes, 0);
        }
    } else {
        /* Wide Vn is invariant under Q and includes modular wrap boundaries. */
        unsigned wide_bytes = bytes * 2u;
        unsigned wide_bits = bits * 2u;
        store_lane(vectors[rn], 0, wide_bytes, mask(wide_bits));
        if (lanes > 1u) store_lane(vectors[rn], 1, wide_bytes, 1);
    }
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[48];
    size_t count = 0;
    for (unsigned opcode = 0; opcode < 4; opcode++)
        for (unsigned u = 0; u <= 1; u++)
            for (unsigned q = 0; q <= 1; q++)
                for (unsigned size = 0; size < 3; size++)
                    append(words, &count, q, u, size, opcode);
    CHECK(count == 48);

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_widening_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }

    const uint32_t invalid[] = {
        widening(0, 0, 3, 0, 2, 1, 0), widening(1, 1, 3, 3, 2, 1, 0),
        widening(1, 0, 0, 4, 2, 1, 0), widening(1, 0, 0, 5, 2, 1, 0),
        widening(1, 0, 0, 0, 2, 1, 0) ^ UINT32_C(0x10000000),
        widening(1, 0, 0, 0, 2, 1, 0) ^ UINT32_C(0x00000400),
        widening(1, 0, 0, 0, 2, 1, 0) ^ UINT32_C(0x00200000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_simd_widening_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
        for (size_t byte = 0; byte < sizeof(encoded); byte++) CHECK(encoded[byte] == 0xa5);
    }

    uint8_t short_buffer[HL_A64_SIMD_WIDENING_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_widening_emit(&assembler, words[0], UINT64_C(0x4000)));
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
        CHECK(hl_a64_simd_widening_emit(&assembler, words[index],
                                        UINT64_C(0x8000) + index * 4));
    }
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    uint8_t stack[256] __attribute__((aligned(16)));
    for (size_t index = 0; index < count; index++) {
        hl_native_aarch64_cpu cpu;
        memset(&cpu, 0, sizeof(cpu));
        seed_vectors(&cpu);
        force_halves(words[index], (uint8_t (*)[16])cpu.vectors);
        for (unsigned reg = 0; reg < 31; reg++)
            cpu.registers[reg] = UINT64_C(0x9abc000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0xa0000000);
        cpu.fpcr = UINT64_C(0x00400000);
        cpu.fpsr = UINT64_C(0x08000091);
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        uint8_t result[16];
        expected(words[index], (const uint8_t (*)[16])vectors, result);
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
        CHECK(cpu.program == UINT64_C(0x8004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0xa0000000));
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
