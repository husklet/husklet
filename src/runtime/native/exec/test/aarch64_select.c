#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/select.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "select:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define SELECT(sf, invert, increment, rm, condition, rn, rd) \
    (((uint32_t)(sf) << 31) | ((uint32_t)(invert) << 30) | 0x1a800000u | ((uint32_t)(rm) << 16) | \
     ((uint32_t)(condition) << 12) | ((uint32_t)(increment) << 10) | ((uint32_t)(rn) << 5) | (rd))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static int holds(unsigned condition, uint64_t flags) {
    int n = (flags >> 31) & 1;
    int z = (flags >> 30) & 1;
    int c = (flags >> 29) & 1;
    int v = (flags >> 28) & 1;
    int result;
    switch (condition >> 1) {
        case 0: result = z; break;
        case 1: result = c; break;
        case 2: result = n; break;
        case 3: result = v; break;
        case 4: result = c && !z; break;
        case 5: result = n == v; break;
        case 6: result = !z && n == v; break;
        default: return 1;
    }
    return result ^ (condition & 1u);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[27];
    size_t offsets[27];
    size_t count = 0;
    for (unsigned condition = 0; condition < 16; condition++)
        words[count++] = SELECT(1, 0, 0, 2, condition, 1, 3); /* csel */
    words[count++] = SELECT(1, 0, 1, 2, 0, 1, 3); /* csinc */
    words[count++] = SELECT(1, 1, 0, 2, 0, 1, 3); /* csinv */
    words[count++] = SELECT(1, 1, 1, 2, 0, 1, 3); /* csneg */
    words[count++] = SELECT(0, 0, 1, 31, 1, 31, 3); /* cset eq */
    words[count++] = SELECT(1, 1, 0, 31, 1, 31, 3); /* csetm eq */
    words[count++] = SELECT(1, 0, 1, 1, 1, 1, 3);  /* cinc eq */
    words[count++] = SELECT(1, 1, 0, 1, 1, 1, 3);  /* cinv eq */
    words[count++] = SELECT(1, 1, 1, 1, 1, 1, 3);  /* cneg eq */
    words[count++] = SELECT(1, 0, 0, 17, 0, 16, 28); /* all stolen */
    words[count++] = SELECT(0, 0, 0, 2, 0, 1, 3);    /* W zeroing */
    words[count++] = SELECT(1, 0, 0, 31, 0, 31, 31); /* ZR discard */

    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED && count == sizeof(words) / sizeof(words[0]));
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < count; i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_select_emit(&assembler, words[i], 0xe000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xa0000000); /* N=1,Z=0,C=1,V=0 */
    cpu.registers[1] = 11; cpu.registers[2] = 22;
    for (unsigned condition = 0; condition < 16; condition++) {
        execute(&cpu, code + offsets[condition]);
        CHECK(cpu.registers[3] == (holds(condition, UINT64_C(0xa0000000)) ? 11u : 22u));
    }
    cpu.flags = 0; cpu.registers[1] = 5; cpu.registers[2] = 7;
    execute(&cpu, code + offsets[16]); CHECK(cpu.registers[3] == 8);
    execute(&cpu, code + offsets[17]); CHECK(cpu.registers[3] == ~UINT64_C(7));
    execute(&cpu, code + offsets[18]); CHECK(cpu.registers[3] == (uint64_t)-7);
    cpu.flags = UINT64_C(0x40000000);
    execute(&cpu, code + offsets[19]); CHECK(cpu.registers[3] == 1);
    execute(&cpu, code + offsets[20]); CHECK(cpu.registers[3] == UINT64_MAX);
    cpu.registers[1] = 9;
    execute(&cpu, code + offsets[21]); CHECK(cpu.registers[3] == 10);
    execute(&cpu, code + offsets[22]); CHECK(cpu.registers[3] == ~UINT64_C(9));
    execute(&cpu, code + offsets[23]); CHECK(cpu.registers[3] == (uint64_t)-9);
    cpu.registers[16] = 33; cpu.registers[17] = 44;
    execute(&cpu, code + offsets[24]); CHECK(cpu.registers[28] == 33);
    cpu.registers[1] = UINT64_C(0x100000001); cpu.registers[2] = UINT64_MAX;
    execute(&cpu, code + offsets[25]); CHECK(cpu.registers[3] == 1);
    cpu.registers[30] = UINT64_C(0x5555555555555555);
    execute(&cpu, code + offsets[26]); CHECK(cpu.registers[30] == UINT64_C(0x5555555555555555));
    CHECK(cpu.flags == UINT64_C(0x40000000));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_SELECT_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_select_emit(&assembler, words[0], 0xf000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_select_emit(&assembler, 0xd503201fu, 0xf000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
