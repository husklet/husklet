#include "../src/arch/aarch64/add.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "add:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define ADDI(sf, sub, flags, shift, imm, rn, rd) \
    (((uint32_t)(sf) << 31) | ((uint32_t)(sub) << 30) | ((uint32_t)(flags) << 29) | 0x11000000u | \
     ((uint32_t)(shift) << 22) | ((uint32_t)(imm) << 10) | ((uint32_t)(rn) << 5) | (rd))
#define ADDR(sf, sub, flags, shift, rm, amount, rn, rd) \
    (((uint32_t)(sf) << 31) | ((uint32_t)(sub) << 30) | ((uint32_t)(flags) << 29) | 0x0b000000u | \
     ((uint32_t)(shift) << 22) | ((uint32_t)(rm) << 16) | ((uint32_t)(amount) << 10) | \
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
        words[count++] = ADDI(1, 0, 0, 0, 1, stolen[i], stolen[i]);
    words[count++] = ADDI(0, 0, 0, 0, 1, 30, 28);  /* add w28,w30,#1 */
    words[count++] = ADDI(1, 0, 1, 0, 1, 2, 1);    /* adds x1,x2,#1 */
    words[count++] = ADDI(1, 1, 1, 0, 1, 2, 1);    /* subs x1,x2,#1 */
    words[count++] = ADDI(1, 1, 1, 0, 5, 30, 31);  /* cmp x30,#5 */
    words[count++] = ADDI(1, 0, 1, 0, 1, 30, 31);  /* cmn x30,#1 */
    words[count++] = ADDI(1, 0, 0, 1, 1, 3, 4);    /* add x4,x3,#1,lsl#12 */
    words[count++] = ADDI(1, 0, 0, 0, 16, 31, 31); /* add sp,sp,#16 */
    words[count++] = ADDI(1, 1, 1, 0, 1, 2, 1);    /* signed overflow subtraction */
    words[count++] = ADDI(0, 0, 1, 0, 1, 2, 1);    /* 32-bit carry and zero */
    words[count++] = ADDR(1, 0, 0, 0, 0, 0, 21, 0);  /* add x0,x21,x0 */
    words[count++] = ADDR(1, 0, 0, 1, 0, 3, 18, 16); /* add x16,x18,x0,lsr#3 */
    words[count++] = ADDR(1, 1, 1, 0, 17, 0, 16, 0); /* subs x0,x16,x17 */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_add_emit(&assembler, words[i], 0x6000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        cpu.registers[stolen[i]] = 40;
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[stolen[i]] == 41);
    }
    cpu.registers[30] = UINT64_C(0xffffffff);
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.registers[28] == 0);

    cpu.registers[2] = UINT64_MAX;
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.registers[1] == 0 && cpu.flags == UINT64_C(0x60000000));
    cpu.registers[0] = 5;
    cpu.registers[21] = 7;
    execute(&cpu, code + offsets[14]);
    CHECK(cpu.registers[0] == 12);
    cpu.registers[0] = 24;
    cpu.registers[18] = 9;
    execute(&cpu, code + offsets[15]);
    CHECK(cpu.registers[16] == 12);
    cpu.registers[16] = 20;
    cpu.registers[17] = 3;
    execute(&cpu, code + offsets[16]);
    CHECK(cpu.registers[0] == 17 && cpu.flags == UINT64_C(0x20000000));
    cpu.registers[2] = UINT64_C(0x7fffffffffffffff);
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.registers[1] == UINT64_C(0x8000000000000000));
    CHECK(cpu.flags == UINT64_C(0x90000000));
    cpu.registers[2] = 0;
    execute(&cpu, code + offsets[7]);
    CHECK(cpu.registers[1] == UINT64_MAX && cpu.flags == UINT64_C(0x80000000));
    cpu.registers[30] = 5;
    execute(&cpu, code + offsets[8]);
    CHECK(cpu.registers[30] == 5 && cpu.flags == UINT64_C(0x60000000));
    cpu.registers[30] = UINT64_MAX;
    execute(&cpu, code + offsets[9]);
    CHECK(cpu.flags == UINT64_C(0x60000000));
    cpu.registers[3] = 7;
    execute(&cpu, code + offsets[10]);
    CHECK(cpu.registers[4] == 4103);
    uint64_t old_stack = cpu.stack;
    execute(&cpu, code + offsets[11]);
    CHECK(cpu.stack == old_stack + 16);
    cpu.registers[2] = UINT64_C(0x8000000000000000);
    execute(&cpu, code + offsets[12]);
    CHECK(cpu.registers[1] == UINT64_C(0x7fffffffffffffff));
    CHECK(cpu.flags == UINT64_C(0x30000000));
    cpu.registers[2] = UINT64_C(0xffffffffffffffff);
    execute(&cpu, code + offsets[13]);
    CHECK(cpu.registers[1] == 0 && cpu.flags == UINT64_C(0x60000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_ADD_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_add_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_add_emit(&assembler, 0xd503201fu, 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
