#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/reverse.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "reverse:%d: %s\n", __LINE__, #x); return 1; } } while (0)
#define REVERSE(sf, opcode, rn, rd) \
    (((uint32_t)(sf) << 31) | 0x5ac00000u | ((uint32_t)(opcode) << 10) | \
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
    uint32_t words[] = {
        REVERSE(1, 0, 1, 2),  /* rbit x2, x1 */
        REVERSE(1, 1, 1, 2),  /* rev16 x2, x1 */
        REVERSE(1, 2, 1, 2),  /* rev32 x2, x1 */
        REVERSE(0, 2, 1, 2),  /* rev w2, w1 */
        REVERSE(1, 3, 1, 2),  /* rev x2, x1 */
        REVERSE(1, 3, 28, 30),
        REVERSE(1, 4, 1, 2),  /* clz x2, x1 */
        REVERSE(1, 5, 1, 2),  /* cls x2, x1 */
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
        CHECK(hl_a64_reverse_emit(&assembler, words[index], 0xf000 + index * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.registers[1] = UINT64_C(0x1122334455667788);
    execute(&cpu, code + offsets[0]); CHECK(cpu.registers[2] == UINT64_C(0x11ee66aa22cc4488));
    execute(&cpu, code + offsets[1]); CHECK(cpu.registers[2] == UINT64_C(0x2211443366558877));
    execute(&cpu, code + offsets[2]); CHECK(cpu.registers[2] == UINT64_C(0x4433221188776655));
    execute(&cpu, code + offsets[3]); CHECK(cpu.registers[2] == UINT64_C(0x88776655));
    execute(&cpu, code + offsets[4]); CHECK(cpu.registers[2] == UINT64_C(0x8877665544332211));
    cpu.registers[28] = UINT64_C(0x0102030405060708);
    execute(&cpu, code + offsets[5]); CHECK(cpu.registers[30] == UINT64_C(0x0807060504030201));
    cpu.registers[1] = 1;
    execute(&cpu, code + offsets[6]); CHECK(cpu.registers[2] == 63);
    cpu.registers[1] = UINT64_MAX;
    execute(&cpu, code + offsets[7]); CHECK(cpu.registers[2] == 63);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_REVERSE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_reverse_emit(&assembler, words[0], 0xf100));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_reverse_emit(&assembler, REVERSE(0, 3, 1, 2), 0xf100));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
