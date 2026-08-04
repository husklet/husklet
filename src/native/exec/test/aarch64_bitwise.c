#include "../src/arch/aarch64/bitwise.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "bitwise:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define LOGR(sf, opc, shift, invert, rm, amount, rn, rd) \
    (((uint32_t)(sf) << 31) | ((uint32_t)(opc) << 29) | 0x0a000000u | ((uint32_t)(shift) << 22) | \
     ((uint32_t)(invert) << 21) | ((uint32_t)(rm) << 16) | ((uint32_t)(amount) << 10) | \
     ((uint32_t)(rn) << 5) | (rd))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const unsigned stolen[] = {16, 17, 18, 28, 30};
    uint32_t words[17];
    size_t offsets[17];
    size_t count = 0;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++)
        words[count++] = LOGR(1, 1, 0, 0, 2, 0, 1, stolen[i]);
    words[count++] = LOGR(1, 1, 0, 0, 16, 0, 17, 18); /* swapped stolen */
    words[count++] = LOGR(1, 0, 0, 0, 3, 0, 2, 1);   /* and */
    words[count++] = LOGR(0, 0, 0, 1, 3, 31, 2, 1);  /* bic w,lsl#31 */
    words[count++] = LOGR(1, 1, 1, 0, 3, 63, 2, 1);  /* orr lsr#63 */
    words[count++] = LOGR(1, 1, 2, 1, 3, 63, 2, 1);  /* orn asr#63 */
    words[count++] = LOGR(1, 2, 3, 0, 3, 63, 2, 1);  /* eor ror#63 */
    words[count++] = LOGR(1, 2, 0, 1, 3, 0, 2, 1);   /* eon */
    words[count++] = LOGR(1, 3, 0, 0, 3, 0, 2, 1);   /* ands */
    words[count++] = LOGR(1, 3, 0, 1, 3, 0, 2, 1);   /* bics */
    words[count++] = LOGR(1, 1, 0, 0, 3, 0, 31, 1);  /* mov alias */
    words[count++] = LOGR(1, 1, 0, 1, 3, 0, 31, 1);  /* mvn alias */
    words[count++] = LOGR(1, 3, 0, 0, 3, 0, 2, 31);  /* tst alias */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_bitwise_emit(&assembler, words[i], 0x9000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.registers[1] = 4;
    cpu.registers[2] = 2;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[stolen[i]] == 6);
    }
    cpu.registers[17] = 4;
    cpu.registers[16] = 2;
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.registers[18] == 6);
    cpu.registers[2] = 6; cpu.registers[3] = 3;
    execute(&cpu, code + offsets[6]); CHECK(cpu.registers[1] == 2);
    cpu.registers[2] = UINT64_C(0xffffffff); cpu.registers[3] = 1;
    execute(&cpu, code + offsets[7]); CHECK(cpu.registers[1] == UINT64_C(0x7fffffff));
    cpu.registers[2] = 4; cpu.registers[3] = UINT64_C(0x8000000000000000);
    execute(&cpu, code + offsets[8]); CHECK(cpu.registers[1] == 5);
    cpu.registers[2] = 4; cpu.registers[3] = UINT64_MAX;
    execute(&cpu, code + offsets[9]); CHECK(cpu.registers[1] == 4);
    cpu.registers[2] = 4; cpu.registers[3] = 1;
    execute(&cpu, code + offsets[10]); CHECK(cpu.registers[1] == 6);
    cpu.registers[2] = 6; cpu.registers[3] = 3;
    execute(&cpu, code + offsets[11]); CHECK(cpu.registers[1] == UINT64_C(0xfffffffffffffffa));
    cpu.registers[2] = UINT64_MAX; cpu.registers[3] = UINT64_C(0x8000000000000000); cpu.flags = UINT64_C(0x30000000);
    execute(&cpu, code + offsets[12]); CHECK(cpu.registers[1] == UINT64_C(0x8000000000000000));
    CHECK(cpu.flags == UINT64_C(0x80000000));
    cpu.registers[2] = 3; cpu.registers[3] = UINT64_MAX; cpu.flags = UINT64_C(0xf0000000);
    execute(&cpu, code + offsets[13]); CHECK(cpu.registers[1] == 0 && cpu.flags == UINT64_C(0x40000000));
    cpu.registers[3] = UINT64_C(0x123456789abcdef0);
    execute(&cpu, code + offsets[14]); CHECK(cpu.registers[1] == cpu.registers[3]);
    execute(&cpu, code + offsets[15]); CHECK(cpu.registers[1] == ~cpu.registers[3]);
    cpu.registers[2] = 0; cpu.flags = UINT64_C(0xf0000000);
    execute(&cpu, code + offsets[16]); CHECK(cpu.flags == UINT64_C(0x40000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_BITWISE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_bitwise_emit(&assembler, words[0], 0xa000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_bitwise_emit(&assembler, LOGR(0, 0, 0, 0, 1, 32, 2, 3), 0xa000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
