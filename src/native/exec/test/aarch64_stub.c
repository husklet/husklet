#include "../src/arch/aarch64/stub.h"
#include "../src/arch/aarch64/entry.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "stub:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void initialize(hl_native_aarch64_cpu *cpu, void *stack, size_t stack_size) {
    memset(cpu, 0, sizeof(*cpu));
    for (unsigned reg = 0; reg < 31; reg++) cpu->registers[reg] = UINT64_C(0x1000000000000000) + reg;
    for (unsigned lane = 0; lane < 64; lane++) cpu->vectors[lane] = UINT64_C(0x2000000000000000) + lane;
    cpu->stack = ((uint64_t)(uintptr_t)stack + stack_size) & ~UINT64_C(15);
    cpu->flags = UINT64_C(0xa0000000);
}

static int unchanged(const hl_native_aarch64_cpu *cpu, uint64_t stack) {
    for (unsigned reg = 0; reg < 31; reg++)
        if (cpu->registers[reg] != UINT64_C(0x1000000000000000) + reg) return 0;
    for (unsigned lane = 0; lane < 64; lane++)
        if (cpu->vectors[lane] != UINT64_C(0x2000000000000000) + lane) return 0;
    return cpu->stack == stack && cpu->flags == UINT64_C(0xa0000000);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t kinds[] = {HL_NATIVE_EXIT_BRANCH, HL_NATIVE_EXIT_SYSCALL, HL_NATIVE_EXIT_FALLBACK,
                              HL_NATIVE_EXIT_FAULT, HL_NATIVE_EXIT_INTERRUPT, HL_NATIVE_EXIT_YIELD};
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, (size_t)page));
    size_t offsets[sizeof(kinds) / sizeof(kinds[0])];
    for (size_t index = 0; index < sizeof(kinds) / sizeof(kinds[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_stub_emit(&assembler, kinds[index], UINT64_C(0x4000) + index * 4));
    }
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0);

    _Alignas(16) uint8_t guest_stack[4096];
    hl_native_aarch64_cpu cpu;
    for (unsigned repetition = 0; repetition < 8; repetition++) {
        for (size_t index = 0; index < sizeof(kinds) / sizeof(kinds[0]); index++) {
            initialize(&cpu, guest_stack, sizeof(guest_stack));
            uint64_t stack = cpu.stack;
            void (*entry)(void);
            void *address = code + offsets[index];
            _Static_assert(sizeof(entry) == sizeof(address), "native code pointer size drifted");
            memcpy(&entry, &address, sizeof(entry));
            hl_native_aarch64_enter(&cpu, entry);
            CHECK(cpu.program == UINT64_C(0x4000) + index * 4);
            CHECK(cpu.reason == kinds[index]);
            CHECK(unchanged(&cpu, stack));
        }
    }
    CHECK(munmap(code, (size_t)page) == 0);

    uint8_t short_buffer[HL_A64_STUB_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_stub_emit(&assembler, HL_NATIVE_EXIT_FALLBACK, 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);

    /* The budget guard captures its placeholders before emitting them, so an
     * exactly exhausted buffer leaves them one past the end.  Those patchers
     * mostly OR in zero bits there, so a value canary cannot see them; end the
     * buffer on an unmapped page instead and let the write fault. */
    hl_a64_budget_guard budget;
    uint8_t *bounded = mmap(NULL, (size_t)page * 2, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(bounded != MAP_FAILED);
    CHECK(mprotect(bounded, (size_t)page, PROT_READ | PROT_WRITE) == 0);
    uint8_t *edge = bounded + (size_t)page - 64;
    CHECK(hl_a64_assembler_begin(&assembler, edge, edge, 64));
    while (hl_a64_assembler_remaining(&assembler) != 0)
        hl_a64_emit32(&assembler, 0xd503201fu);
    CHECK(hl_a64_assembler_ok(&assembler));
    hl_a64_stub_budget_begin(&assembler, 0x9000, &budget);
    hl_a64_stub_budget_finish(&assembler, &budget, 4);
    CHECK(!hl_a64_assembler_ok(&assembler));
    CHECK(munmap(bounded, (size_t)page * 2) == 0);
    return 0;
#endif
}
