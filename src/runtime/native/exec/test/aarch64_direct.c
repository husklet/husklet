#include "../src/arch/aarch64/direct.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "direct:%d: %s\n", __LINE__, #x); return 1; } } while (0)

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
    hl_a64_assembler assembler;
    size_t branch;
    size_t call;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    branch = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_direct_emit(&assembler, 0x17FFFFFFu, 0x4000)); /* b pc-4 */
    call = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_direct_emit(&assembler, 0x94000002u, 0x8000)); /* bl pc+8 */
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.registers[30] = UINT64_C(0xdeadbeef);
    execute(&cpu, code + branch);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x3ffc);
    CHECK(cpu.registers[30] == UINT64_C(0xdeadbeef));
    execute(&cpu, code + call);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x8008);
    CHECK(cpu.registers[30] == 0x8004);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_DIRECT_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_direct_emit(&assembler, 0x14000000u, 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_direct_emit(&assembler, 0xd503201fu, 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
