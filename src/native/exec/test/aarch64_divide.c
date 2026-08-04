#include "../src/arch/aarch64/divide.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "divide:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define DIV(sf, sign, rm, rn, rd) \
    (((uint32_t)(sf) << 31) | 0x1ac00800u | ((uint32_t)(sign) << 10) | \
     ((uint32_t)(rm) << 16) | ((uint32_t)(rn) << 5) | (rd))

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
    uint32_t words[13];
    size_t offsets[13];
    size_t count = 0;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++)
        words[count++] = DIV(1, 0, 2, 1, stolen[i]);
    words[count++] = DIV(1, 0, 2, 1, 3);   /* udiv x */
    words[count++] = DIV(1, 1, 2, 1, 3);   /* sdiv x */
    words[count++] = DIV(1, 0, 31, 1, 3);  /* zero divisor */
    words[count++] = DIV(1, 1, 2, 1, 3);   /* min/-1 */
    words[count++] = DIV(0, 0, 2, 1, 3);   /* udiv w */
    words[count++] = DIV(0, 1, 2, 1, 3);   /* sdiv w */
    words[count++] = DIV(1, 0, 17, 16, 28);/* all stolen */
    words[count++] = DIV(1, 1, 31, 31, 31);/* ZR discard */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_divide_emit(&assembler, words[i], 0xc000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xf0000000);
    cpu.registers[1] = 84; cpu.registers[2] = 2;
    for (size_t i = 0; i < sizeof(stolen) / sizeof(stolen[0]); i++) {
        execute(&cpu, code + offsets[i]);
        CHECK(cpu.registers[stolen[i]] == 42);
    }
    cpu.registers[1] = 10; cpu.registers[2] = 3;
    execute(&cpu, code + offsets[5]); CHECK(cpu.registers[3] == 3);
    cpu.registers[1] = (uint64_t)-10; cpu.registers[2] = 3;
    execute(&cpu, code + offsets[6]); CHECK(cpu.registers[3] == (uint64_t)-3);
    cpu.registers[1] = 99;
    execute(&cpu, code + offsets[7]); CHECK(cpu.registers[3] == 0);
    cpu.registers[1] = UINT64_C(0x8000000000000000); cpu.registers[2] = UINT64_MAX;
    execute(&cpu, code + offsets[8]); CHECK(cpu.registers[3] == UINT64_C(0x8000000000000000));
    cpu.registers[1] = UINT64_C(0x100000006); cpu.registers[2] = 2;
    execute(&cpu, code + offsets[9]); CHECK(cpu.registers[3] == 3);
    cpu.registers[1] = (uint32_t)-10; cpu.registers[2] = 3;
    execute(&cpu, code + offsets[10]); CHECK(cpu.registers[3] == UINT64_C(0xfffffffd));
    cpu.registers[16] = 100; cpu.registers[17] = 4;
    execute(&cpu, code + offsets[11]); CHECK(cpu.registers[28] == 25);
    cpu.registers[30] = UINT64_C(0x5555555555555555);
    execute(&cpu, code + offsets[12]); CHECK(cpu.registers[30] == UINT64_C(0x5555555555555555));
    CHECK(cpu.flags == UINT64_C(0xf0000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_DIVIDE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_divide_emit(&assembler, words[0], 0xd000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_divide_emit(&assembler, 0xd503201fu, 0xd000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
