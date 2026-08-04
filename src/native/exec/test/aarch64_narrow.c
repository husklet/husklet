#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/simd_narrow.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "narrow:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define NARROW(q, opcode, immh, immb, rn, rd) \
    (0x0f000400u | ((uint32_t)(q) << 30) | ((uint32_t)(immh) << 19) | \
     ((uint32_t)(immb) << 16) | ((uint32_t)(opcode) << 11) | \
     ((uint32_t)(rn) << 5) | (rd))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static void source(hl_native_aarch64_cpu *cpu, unsigned reg, const void *value) {
    memcpy(&cpu->vectors[reg * 2], value, 16);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        0x0f0c8464u,                     /* shrn v4.8b,v3.8h,#4 */
        NARROW(0, 0x11, 1, 4, 3, 4),    /* rshrn v4.8b,v3.8h,#4 */
        NARROW(1, 0x10, 1, 4, 3, 4),    /* shrn2 v4.16b,v3.8h,#4 */
        NARROW(0, 0x10, 1, 4, 3, 3),    /* alias */
        NARROW(0, 0x10, 3, 0, 30, 28),  /* shrn v28.4h,v30.4s,#8 */
        NARROW(0, 0x11, 6, 0, 3, 4),    /* rshrn v4.2s,v3.2d,#16 */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_simd_narrow_emit(&assembler, words[index], 0xa000 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    const uint16_t half[] = {0x0000, 0x000f, 0x0010, 0x00ff, 0x0100, 0x0ff0, 0xfff0, 0xffff};
    const uint8_t truncated[] = {0x00, 0x00, 0x01, 0x0f, 0x10, 0xff, 0xff, 0xff};
    const uint8_t rounded[] = {0x00, 0x01, 0x01, 0x10, 0x10, 0xff, 0xff, 0x00};
    cpu.flags = UINT64_C(0x50000000);
    cpu.tls = UINT64_C(0x123456789abcdef0);
    source(&cpu, 3, half);
    execute(&cpu, code + offsets[0]);
    CHECK(memcmp(&cpu.vectors[8], truncated, sizeof(truncated)) == 0 && cpu.vectors[9] == 0);
    source(&cpu, 3, half);
    execute(&cpu, code + offsets[1]);
    CHECK(memcmp(&cpu.vectors[8], rounded, sizeof(rounded)) == 0 && cpu.vectors[9] == 0);
    source(&cpu, 3, half);
    cpu.vectors[8] = UINT64_C(0x1122334455667788);
    cpu.vectors[9] = 0;
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.vectors[8] == UINT64_C(0x1122334455667788));
    CHECK(memcmp(&cpu.vectors[9], truncated, sizeof(truncated)) == 0);
    source(&cpu, 3, half);
    execute(&cpu, code + offsets[3]);
    CHECK(memcmp(&cpu.vectors[6], truncated, sizeof(truncated)) == 0 && cpu.vectors[7] == 0);

    const uint32_t words32[] = {0x00000000, 0x000000ff, 0x00000100, 0xffffff00};
    const uint16_t narrowed16[] = {0x0000, 0x0000, 0x0001, 0xffff};
    source(&cpu, 30, words32);
    execute(&cpu, code + offsets[4]);
    CHECK(memcmp(&cpu.vectors[56], narrowed16, sizeof(narrowed16)) == 0 && cpu.vectors[57] == 0);
    const uint64_t words64[] = {UINT64_C(0x00000000ffff8000), UINT64_C(0xffffffffffff8000)};
    const uint32_t narrowed32[] = {0x00010000u, 0x00000000u};
    source(&cpu, 3, words64);
    execute(&cpu, code + offsets[5]);
    CHECK(memcmp(&cpu.vectors[8], narrowed32, sizeof(narrowed32)) == 0 && cpu.vectors[9] == 0);
    CHECK(cpu.flags == UINT64_C(0x50000000));
    CHECK(cpu.tls == UINT64_C(0x123456789abcdef0));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_SIMD_NARROW_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_narrow_emit(&assembler, words[0], 0xb000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_narrow_emit(&assembler, NARROW(0, 0x10, 8, 0, 3, 4), 0xb000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_simd_narrow_emit(&assembler, NARROW(0, 0x12, 1, 4, 3, 4), 0xb000));
    return 0;
#endif
}
