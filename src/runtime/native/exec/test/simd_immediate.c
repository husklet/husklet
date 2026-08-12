#include "../src/arch/aarch64/simd_immediate.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "simd_immediate:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t encoding(unsigned q, unsigned op, unsigned cmode,
                         unsigned o2, unsigned imm8, unsigned destination) {
    return UINT32_C(0x0f000400) | (q << 30) | (op << 29) |
           ((imm8 >> 5) << 16) | (cmode << 12) | (o2 << 11) |
           ((imm8 & 31u) << 5) | destination;
}

static int expected(unsigned q, unsigned op, unsigned cmode, unsigned o2) {
    unsigned selector = cmode >> 1;
    unsigned low = cmode & 1u;
    if (selector != 7u) return o2 == 0u;
    if ((low == 0u || op != 0u) && o2 != 0u) return 0;
    return low == 0u || op == 0u || q != 0u;
}

static int accepted(uint32_t word) {
    uint8_t code[8];
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, sizeof(code)));
    int result = hl_a64_simd_immediate_body(&assembler, word);
    if (result) CHECK(!memcmp(code, &word, sizeof(word)));
    return result;
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
    for (unsigned q = 0; q < 2; ++q)
        for (unsigned op = 0; op < 2; ++op)
            for (unsigned cmode = 0; cmode < 16; ++cmode)
                for (unsigned o2 = 0; o2 < 2; ++o2)
                    for (unsigned imm8 = 0; imm8 < 256; ++imm8)
                        CHECK(accepted(encoding(q, op, cmode, o2, imm8, 31)) ==
                              expected(q, op, cmode, o2));
    CHECK(!accepted(0));
    CHECK(!accepted(UINT32_C(0x0f000000)));
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        encoding(1, 1, 0, 0, 0, 31),    /* MVNI v31.4s,#0 */
        encoding(1, 1, 14, 0, 0, 30),   /* MOVI byte-mask spelling: all zero */
        encoding(1, 0, 14, 0, 0xa5, 0), /* MOVI v0.16b,#0xa5 */
        encoding(1, 0, 3, 0, 0x12, 1),  /* ORR v1.4s,#0x12,LSL #8 */
        encoding(1, 1, 3, 0, 0x12, 2),  /* BIC v2.4s,#0x12,LSL #8 */
        encoding(1, 0, 15, 0, 0x70, 3), /* FMOV v3.4s,#1.0 */
        encoding(1, 1, 15, 0, 0x70, 4), /* FMOV v4.2d,#1.0 */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); ++index) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_immediate_emit(&assembler, words[index], 0x6000 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);
    hl_native_aarch64_cpu cpu = {0};
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.vectors[62] == UINT64_MAX && cpu.vectors[63] == UINT64_MAX);
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.vectors[60] == 0 && cpu.vectors[61] == 0);
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.vectors[0] == UINT64_C(0xa5a5a5a5a5a5a5a5) && cpu.vectors[1] == cpu.vectors[0]);
    cpu.vectors[2] = cpu.vectors[3] = UINT64_C(0x1000100010001000);
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.vectors[2] == UINT64_C(0x1000120010001200) && cpu.vectors[3] == cpu.vectors[2]);
    cpu.vectors[4] = cpu.vectors[5] = UINT64_MAX;
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.vectors[4] == UINT64_C(0xffffedffffffedff) && cpu.vectors[5] == cpu.vectors[4]);
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.vectors[6] == UINT64_C(0x3f8000003f800000) && cpu.vectors[7] == cpu.vectors[6]);
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.vectors[8] == UINT64_C(0x3ff0000000000000) && cpu.vectors[9] == cpu.vectors[8]);
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
