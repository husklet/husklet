#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_permute.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "permute:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t ext(unsigned q, unsigned rm, unsigned position, unsigned rn, unsigned rd) {
    return UINT32_C(0x2e000000) | (q << 30) | (rm << 16) | (position << 11) | (rn << 5) | rd;
}

static uint32_t table(unsigned q, unsigned rm, unsigned length, unsigned extend,
                      unsigned rn, unsigned rd) {
    return UINT32_C(0x0e000000) | (q << 30) | (rm << 16) | (length << 13) |
           (extend << 12) | (rn << 5) | rd;
}

static uint32_t permute(unsigned q, unsigned size, unsigned rm, unsigned opcode,
                        unsigned rn, unsigned rd) {
    return UINT32_C(0x0e000800) | (q << 30) | (size << 22) | (rm << 16) |
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
            ((uint8_t *)cpu->vectors)[vector * 16 + byte] = (uint8_t)(vector * 17u + byte * 7u);
    for (unsigned reg = 0; reg < 31; reg++) cpu->registers[reg] = UINT64_C(0x1234000000000000) + reg;
    cpu->stack = stack;
    cpu->flags = UINT64_C(0x90000000);
    cpu->fpcr = UINT64_C(0x00400000);
    cpu->fpsr = UINT64_C(0x00000090);
}

static void element_copy(uint8_t *destination, unsigned lane, const uint8_t *source,
                         unsigned source_lane, unsigned bytes) {
    memcpy(destination + lane * bytes, source + source_lane * bytes, bytes);
}

static void expected(uint32_t word, const uint8_t before[32][16], uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u;
    unsigned rd = word & 31u, rn = (word >> 5) & 31u, rm = (word >> 16) & 31u;
    unsigned bytes = q ? 16u : 8u;
    memset(output, 0, 16);
    if ((word & UINT32_C(0xbfe08400)) == UINT32_C(0x2e000000)) {
        unsigned position = (word >> 11) & 15u;
        for (unsigned index = 0; index < bytes; index++) {
            unsigned source = position + index;
            output[index] = source < bytes ? before[rn][source] : before[rm][source - bytes];
        }
        return;
    }
    if ((word & UINT32_C(0xbf208c00)) == UINT32_C(0x0e000000)) {
        unsigned length = (word >> 13) & 3u;
        unsigned extend = (word >> 12) & 1u;
        for (unsigned index = 0; index < bytes; index++) {
            unsigned selector = before[rm][index];
            if (selector < (length + 1u) * 16u)
                output[index] = before[(rn + selector / 16u) & 31u][selector & 15u];
            else if (extend)
                output[index] = before[rd][index];
        }
        return;
    }
    unsigned size = (word >> 22) & 3u;
    unsigned opcode = (word >> 12) & 7u;
    unsigned element = 1u << size;
    unsigned lanes = bytes / element;
    unsigned half = lanes / 2u;
    for (unsigned lane = 0; lane < lanes; lane++) {
        const uint8_t *source;
        unsigned source_lane;
        if (opcode == 1u || opcode == 5u) {
            source = lane < half ? before[rn] : before[rm];
            source_lane = (lane < half ? lane : lane - half) * 2u + (opcode == 5u);
        } else if (opcode == 2u || opcode == 6u) {
            source = (lane & 1u) ? before[rm] : before[rn];
            source_lane = (lane & ~1u) + (opcode == 6u);
        } else {
            source = (lane & 1u) ? before[rm] : before[rn];
            source_lane = (opcode == 7u ? half : 0u) + lane / 2u;
        }
        element_copy(output, lane, source, source_lane, element);
    }
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[62];
    size_t count = 0;
    words[count++] = ext(0, 2, 0, 1, 0);
    words[count++] = ext(0, 2, 7, 1, 1);  /* rd == rn */
    words[count++] = ext(1, 3, 0, 3, 3);  /* all overlap */
    words[count++] = ext(1, 2, 15, 1, 2); /* rd == rm */
    for (unsigned q = 0; q <= 1; q++)
        for (unsigned length = 0; length < 4; length++)
            for (unsigned extend = 0; extend <= 1; extend++) {
                unsigned rn = length == 3 ? 30 : 4;
                unsigned rd = extend ? rn : 8;
                unsigned rm = q ? 9 : rd;
                words[count++] = table(q, rm, length, extend, rn, rd);
            }
    static const unsigned opcodes[] = {1, 2, 3, 5, 6, 7};
    for (unsigned q = 0; q <= 1; q++)
        for (unsigned size = 0; size < 4; size++) {
            if (!q && size == 3) continue;
            for (size_t op = 0; op < sizeof(opcodes) / sizeof(opcodes[0]); op++) {
                unsigned rn = 10 + size, rm = 20 + size;
                unsigned rd = op % 3 == 0 ? rn : op % 3 == 1 ? rm : 30;
                words[count++] = permute(q, size, rm, opcodes[op], rn, rd);
            }
        }
    CHECK(count == sizeof(words) / sizeof(words[0]));

    uint8_t encoded[4];
    hl_a64_assembler assembler;
    for (size_t index = 0; index < count; index++) {
        memset(encoded, 0, sizeof(encoded));
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_permute_body(&assembler, words[index]));
        CHECK(memcmp(encoded, &words[index], sizeof(encoded)) == 0);
    }
    CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
    CHECK(!hl_a64_simd_permute_body(&assembler, ext(0, 2, 8, 1, 0)));
    for (unsigned opcode = 0; opcode < 8; opcode++) {
        int allocated = opcode != 0 && opcode != 4;
        CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
        CHECK(hl_a64_simd_permute_body(&assembler, permute(1, 0, 2, opcode, 1, 0)) == allocated);
    }
    CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
    CHECK(!hl_a64_simd_permute_body(&assembler, permute(0, 3, 2, 1, 1, 0)));
    CHECK(hl_a64_assembler_begin(&assembler, encoded, encoded, sizeof(encoded)));
    CHECK(!hl_a64_simd_permute_body(&assembler, words[0] ^ UINT32_C(0x00200000)));

    uint8_t short_buffer[HL_A64_SIMD_PERMUTE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_permute_emit(&assembler, words[0], UINT64_C(0x4000)));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 16;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t offsets[62];
    for (size_t index = 0; index < count; index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_permute_emit(&assembler, words[index], UINT64_C(0x8000) + index * 4));
    }
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
        CHECK(cpu.flags == UINT64_C(0x90000000) && cpu.fpcr == UINT64_C(0x00400000) &&
              cpu.fpsr == UINT64_C(0x00000090));
        CHECK(memcmp(cpu.registers, registers, sizeof(registers)) == 0);
        unsigned destination = words[index] & 31u;
        CHECK(memcmp(&cpu.vectors[destination * 2], result, 16) == 0);
        for (unsigned vector = 0; vector < 32; vector++)
            if (vector != destination)
                CHECK(memcmp(&cpu.vectors[vector * 2], &vectors[vector * 2], 16) == 0);
    }
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
