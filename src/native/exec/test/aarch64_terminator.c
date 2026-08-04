#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/terminator.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "terminator:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    _Alignas(16) uint8_t guest_stack[1024];
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    CHECK(hl_a64_terminator_emit(&assembler, 0xd4000001u, UINT64_C(0x123456789abcdef0)));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    for (unsigned reg = 0; reg < 31; reg++)
        cpu.registers[reg] = UINT64_C(0x1000000000000000) + reg;
    for (unsigned lane = 0; lane < 64; lane++)
        cpu.vectors[lane] = UINT64_C(0x2000000000000000) + lane;
    cpu.stack = ((uint64_t)(uintptr_t)(guest_stack + sizeof(guest_stack))) & ~UINT64_C(15);
    cpu.flags = UINT64_C(0xa0000000);
    uint64_t stack = cpu.stack;
    execute(&cpu, code);
    CHECK(cpu.reason == HL_NATIVE_EXIT_SYSCALL);
    CHECK(cpu.program == UINT64_C(0x123456789abcdef0));
    CHECK(cpu.program + 4 == UINT64_C(0x123456789abcdef4));
    for (unsigned reg = 0; reg < 31; reg++)
        CHECK(cpu.registers[reg] == UINT64_C(0x1000000000000000) + reg);
    for (unsigned lane = 0; lane < 64; lane++)
        CHECK(cpu.vectors[lane] == UINT64_C(0x2000000000000000) + lane);
    CHECK(cpu.stack == stack && cpu.flags == UINT64_C(0xa0000000));
    CHECK(cpu.registers[8] == UINT64_C(0x1000000000000008));
    for (unsigned argument = 0; argument < 6; argument++)
        CHECK(cpu.registers[argument] == UINT64_C(0x1000000000000000) + argument);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_TERMINATOR_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_terminator_emit(&assembler, 0xd4000001u, 0x1000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_terminator_emit(&assembler, 0xd4200000u, 0x1000)); /* brk #0 */
    CHECK(!hl_a64_terminator_emit(&assembler, 0xd4400000u, 0x1000)); /* hlt #0 */
    CHECK(!hl_a64_terminator_emit(&assembler, 0x00000000u, 0x1000)); /* udf #0 */
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
