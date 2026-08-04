#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_integer.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "simd-integer:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t integer(unsigned q, unsigned u, unsigned size, unsigned opcode,
                        unsigned rm, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200400) | (q << 30) | (u << 29) | (size << 22) |
           (rm << 16) | (opcode << 11) | (rn << 5) | rd;
}

static uint64_t load_lane(const uint8_t *source, unsigned lane, unsigned bytes) {
    uint64_t value = 0;
    memcpy(&value, source + lane * bytes, bytes);
    return value;
}

static void store_lane(uint8_t *destination, unsigned lane, unsigned bytes, uint64_t value) {
    memcpy(destination + lane * bytes, &value, bytes);
}

static int64_t signed_lane(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    return (int64_t)((value ^ sign) - sign);
}

static uint8_t polynomial(uint8_t left, uint8_t right) {
    unsigned product = 0;
    for (unsigned bit = 0; bit < 8; bit++)
        if ((right >> bit) & 1u) product ^= (unsigned)left << bit;
    return (uint8_t)product;
}

static void expected(uint32_t word, const uint8_t before[32][16], uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u, u = (word >> 29) & 1u;
    unsigned size = (word >> 22) & 3u, opcode = (word >> 11) & 31u;
    unsigned rm = (word >> 16) & 31u, rn = (word >> 5) & 31u, rd = word & 31u;
    unsigned bytes = 1u << size, lanes = (q ? 16u : 8u) / bytes, bits = bytes * 8u;
    uint64_t mask = bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    memset(output, 0, 16);
    for (unsigned lane = 0; lane < lanes; lane++) {
        uint64_t left = load_lane(before[rn], lane, bytes);
        uint64_t right = load_lane(before[rm], lane, bytes);
        uint64_t value;
        if (opcode == 0x10u) {
            value = u ? left - right : left + right;
        } else if (opcode == 0x0cu || opcode == 0x0du) {
            int take;
            if (u)
                take = opcode == 0x0cu ? left > right : left < right;
            else
                take = opcode == 0x0cu
                           ? signed_lane(left, bits) > signed_lane(right, bits)
                           : signed_lane(left, bits) < signed_lane(right, bits);
            value = take ? left : right;
        } else if (opcode == 0x12u) {
            uint64_t base = load_lane(before[rd], lane, bytes);
            value = u ? base - left * right : base + left * right;
        } else if (u) {
            value = polynomial((uint8_t)left, (uint8_t)right);
        } else {
            value = left * right;
        }
        store_lane(output, lane, bytes, value & mask);
    }
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void append(uint32_t words[58], size_t *count, unsigned q, unsigned u,
                   unsigned size, unsigned opcode) {
    unsigned index = (unsigned)*count;
    unsigned rn = (index * 7u + 3u) & 31u, rm = (index * 11u + 5u) & 31u;
    unsigned rd = index % 3u == 0 ? rn : index % 3u == 1 ? rm : (index * 13u + 9u) & 31u;
    words[(*count)++] = integer(q, u, size, opcode, rm, rn, rd);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[58];
    size_t count = 0;
    for (unsigned u = 0; u <= 1; u++)
        for (unsigned q = 0; q <= 1; q++)
            for (unsigned size = 0; size < 4; size++)
                if (q || size != 3) append(words, &count, q, u, size, 0x10u);
    for (unsigned opcode = 0x0cu; opcode <= 0x0du; opcode++)
        for (unsigned u = 0; u <= 1; u++)
            for (unsigned q = 0; q <= 1; q++)
                for (unsigned size = 0; size < 3; size++) append(words, &count, q, u, size, opcode);
    for (unsigned u = 0; u <= 1; u++)
        for (unsigned q = 0; q <= 1; q++)
            for (unsigned size = 0; size < 3; size++) append(words, &count, q, u, size, 0x12u);
    for (unsigned q = 0; q <= 1; q++)
        for (unsigned size = 0; size < 3; size++) append(words, &count, q, 0, size, 0x13u);
    for (unsigned q = 0; q <= 1; q++) append(words, &count, q, 1, 0, 0x13u);
    CHECK(count == sizeof(words) / sizeof(words[0]));

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_integer_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }
    const uint32_t invalid[] = {
        integer(0, 0, 3, 0x10, 2, 1, 0), integer(1, 0, 3, 0x0c, 2, 1, 0),
        integer(1, 0, 3, 0x0d, 2, 1, 0), integer(1, 0, 3, 0x12, 2, 1, 0),
        integer(1, 0, 3, 0x13, 2, 1, 0), integer(1, 1, 1, 0x13, 2, 1, 0),
        integer(1, 0, 0, 0x01, 2, 1, 0), integer(1, 0, 0, 0x06, 2, 1, 0),
        integer(1, 0, 0, 0x08, 2, 1, 0), words[0] ^ UINT32_C(0x00200000),
        words[0] ^ UINT32_C(0x10000000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_simd_integer_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
    }

    uint8_t short_buffer[HL_A64_SIMD_INTEGER_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_integer_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 16;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[58];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_integer_emit(&assembler, words[index], UINT64_C(0x8000) + index * 4));
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
                    (uint8_t)(0x83u + vector * 31u + byte * 47u);
        for (unsigned reg = 0; reg < 31; reg++) cpu.registers[reg] = UINT64_C(0x2345000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0xa0000000);
        cpu.fpcr = UINT64_C(0x00c00000);
        cpu.fpsr = UINT64_C(0x08000091);
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        uint8_t result[16];
        expected(words[index], (const uint8_t (*)[16])vectors, result);
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x8004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0xa0000000) && cpu.fpcr == UINT64_C(0x00c00000) &&
              cpu.fpsr == UINT64_C(0x08000091));
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
