#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/narrow_move.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "narrow-move:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t xtn(unsigned q, unsigned size, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e212800) | (q << 30) | (size << 22) | (rn << 5) | rd;
}

static uint64_t load_lane(const uint8_t *source, unsigned lane, unsigned bytes) {
    uint64_t value = 0;
    memcpy(&value, source + lane * bytes, bytes);
    return value;
}

static void store_lane(uint8_t *destination, unsigned lane, unsigned bytes, uint64_t value) {
    memcpy(destination + lane * bytes, &value, bytes);
}

static uint64_t lane_mask(unsigned bits) {
    return bits == 64u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
}

static void expected(uint32_t word, const uint8_t before[32][16], uint8_t result[16]) {
    unsigned q = (word >> 30) & 1u;
    unsigned size = (word >> 22) & 3u;
    unsigned rn = (word >> 5) & 31u;
    unsigned rd = word & 31u;
    unsigned narrow_bytes = 1u << size;
    unsigned wide_bytes = narrow_bytes * 2u;
    unsigned lanes = 8u / narrow_bytes;
    uint8_t packed[8] = {0};
    for (unsigned lane = 0; lane < lanes; lane++)
        store_lane(packed, lane, narrow_bytes,
                   load_lane(before[rn], lane, wide_bytes) & lane_mask(narrow_bytes * 8u));
    memset(result, 0, 16);
    if (q) memcpy(result, before[rd], 8);
    memcpy(result + (q ? 8 : 0), packed, 8);
}

static void force_high_bits(uint32_t word, uint8_t vectors[32][16]) {
    unsigned size = (word >> 22) & 3u;
    unsigned rn = (word >> 5) & 31u;
    unsigned narrow_bytes = 1u << size;
    unsigned wide_bytes = narrow_bytes * 2u;
    unsigned lanes = 8u / narrow_bytes;
    unsigned narrow_bits = narrow_bytes * 8u;
    uint64_t low_mask = lane_mask(narrow_bits);
    uint64_t wide_mask = lane_mask(narrow_bits * 2u);
    for (unsigned lane = 0; lane < lanes; lane++) {
        uint64_t low = (UINT64_C(0x35) * (lane + 1u)) & low_mask;
        uint64_t high = (wide_mask ^ low_mask) &
                        ((UINT64_C(0xa57b9d3f) * (lane + 3u)) << narrow_bits);
        store_lane(vectors[rn], lane, wide_bytes, high | low);
    }
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
    uint32_t words[6];
    size_t count = 0;
    for (unsigned q = 0; q <= 1; q++)
        for (unsigned size = 0; size < 3; size++) {
            unsigned rn = ((unsigned)count * 7u + 3u) & 31u;
            unsigned rd = count % 2u == 0 ? rn : ((unsigned)count * 11u + 5u) & 31u;
            words[count++] = xtn(q, size, rn, rd);
        }
    CHECK(count == 6);

    hl_a64_assembler assembler;
    uint8_t encoded[4];
    for (size_t index = 0; index < count; index++) {
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_narrow_move_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }
    const uint32_t invalid[] = {
        xtn(0, 3, 1, 0), xtn(0, 0, 1, 0) | UINT32_C(0x10000000),
        xtn(0, 0, 1, 0) | UINT32_C(0x20000000),
        xtn(0, 0, 1, 0) ^ UINT32_C(0x00002000),
        xtn(0, 0, 1, 0) ^ UINT32_C(0x00000400),
        xtn(0, 0, 1, 0) ^ UINT32_C(0x00200000),
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        memset(encoded, 0xa5, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(!hl_a64_narrow_move_body(&assembler, invalid[index]));
        CHECK(hl_a64_assembler_size(&assembler) == 0);
        for (size_t byte = 0; byte < sizeof(encoded); byte++) CHECK(encoded[byte] == 0xa5);
    }
    uint8_t short_buffer[HL_A64_NARROW_MOVE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_narrow_move_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t byte = 0; byte < sizeof(short_buffer); byte++) CHECK(short_buffer[byte] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 4u;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[6];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_narrow_move_emit(&assembler, words[index], UINT64_C(0xa000) + index * 4));
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
                    (uint8_t)(0x79u + vector * 23u + byte * 47u);
        force_high_bits(words[index], (uint8_t (*)[16])cpu.vectors);
        for (unsigned reg = 0; reg < 31; reg++) cpu.registers[reg] = UINT64_C(0xface000000000000) + reg;
        cpu.stack = (uint64_t)(uintptr_t)(stack + sizeof(stack));
        cpu.flags = UINT64_C(0x60000000);
        cpu.fpcr = UINT64_C(0x00400000);
        cpu.fpsr = UINT64_C(0x08000091);
        uint64_t registers[31], vectors[64];
        memcpy(registers, cpu.registers, sizeof(registers));
        memcpy(vectors, cpu.vectors, sizeof(vectors));
        uint8_t result[16];
        expected(words[index], (const uint8_t (*)[16])vectors, result);
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH);
        CHECK(cpu.program == UINT64_C(0xa004) + index * 4);
        CHECK(cpu.flags == UINT64_C(0x60000000));
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
