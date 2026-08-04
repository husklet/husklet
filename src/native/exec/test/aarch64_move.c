#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/move.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "move:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define WIDE(sf, opc, hw, imm, rd) (((uint32_t)(sf) << 31) | ((uint32_t)(opc) << 29) | 0x12800000u | \
                                    ((uint32_t)(hw) << 21) | ((uint32_t)(imm) << 5) | (rd))

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        WIDE(1, 2, 1, 0x1234, 30), /* movz x30,#0x1234,lsl#16 */
        WIDE(1, 3, 2, 0xbeef, 30), /* movk x30,#0xbeef,lsl#32 */
        WIDE(0, 0, 0, 0, 28),      /* movn w28,#0 */
        WIDE(0, 2, 1, 0xabcd, 1), /* movz w1,#0xabcd,lsl#16 */
        WIDE(1, 2, 3, 0x9876, 31), /* movz xzr,#0x9876,lsl#48 */
    };
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t offsets[sizeof(words) / sizeof(words[0])];
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < sizeof(words) / sizeof(words[0]); i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_move_emit(&assembler, words[i], 0x5000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0xa0000000);
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.registers[30] == UINT64_C(0x12340000));
    cpu.registers[30] = UINT64_C(0x1111222233334444);
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.registers[30] == UINT64_C(0x1111beef33334444));
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.registers[28] == UINT64_C(0xffffffff));
    cpu.registers[1] = UINT64_MAX;
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.registers[1] == UINT64_C(0xabcd0000));
    cpu.registers[30] = UINT64_C(0x5555555555555555);
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.registers[30] == UINT64_C(0x5555555555555555));
    CHECK(cpu.flags == UINT64_C(0xa0000000));
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x5014);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_MOVE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_move_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_move_emit(&assembler, WIDE(0, 2, 2, 1, 0), 0x9000));
    CHECK(!hl_a64_move_emit(&assembler, WIDE(1, 1, 0, 1, 0), 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
