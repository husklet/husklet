#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/indirect.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "indirect:%d: %s\n", __LINE__, #x); return 1; } } while (0)

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
        0xd61f0240u, /* br x18 */
        0xd61f0380u, /* br x28 */
        0xd65f03c0u, /* ret x30 */
        0xd63f03c0u, /* blr x30 */
        0xd63f0240u, /* blr x18 */
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
        CHECK(hl_a64_indirect_emit(&assembler, words[i], 0x4000 + i * 4));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.registers[18] = UINT64_C(0x1818181818181818);
    cpu.registers[28] = UINT64_C(0x2828282828282828);
    cpu.registers[30] = UINT64_C(0x3030303030303030);
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x1818181818181818));
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.program == UINT64_C(0x2828282828282828));
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.program == UINT64_C(0x3030303030303030));

    cpu.registers[30] = UINT64_C(0xaaaaaaaaaaaaaaaa);
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.program == UINT64_C(0xaaaaaaaaaaaaaaaa));
    CHECK(cpu.registers[30] == 0x4010);
    cpu.registers[18] = UINT64_C(0xbbbbbbbbbbbbbbbb);
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.program == UINT64_C(0xbbbbbbbbbbbbbbbb));
    CHECK(cpu.registers[30] == 0x4014);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_INDIRECT_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_indirect_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_indirect_emit(&assembler, 0xd61f03e0u, 0x9000)); /* reserved br x31 */
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
