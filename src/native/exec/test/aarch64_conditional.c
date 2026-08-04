#include "../src/arch/aarch64/conditional.h"
#include "../src/arch/aarch64/entry.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "conditional:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

static int outcome(hl_native_aarch64_cpu *cpu, void *code, size_t offset, uint64_t expected) {
    execute(cpu, (uint8_t *)code + offset);
    return cpu->reason == HL_NATIVE_EXIT_BRANCH && cpu->program == expected;
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint32_t words[] = {
        0x54000040u, /* b.eq pc+8 */
        0x5400004fu, /* b.nv pc+8: retained always alias */
        0x3400005eu, /* cbz w30,pc+8 */
        0xb5000052u, /* cbnz x18,pc+8 */
        0xb6f8005eu, /* tbz x30,#63,pc+8 */
        0x3738005cu, /* tbnz w28,#7,pc+8 */
        0x54ffffe0u, /* b.eq pc-4 */
        0x34fffffeu, /* cbz w30,pc-4 */
        0x36fffffeu, /* tbz w30,#31,pc-4 */
    };
    long page = sysconf(_SC_PAGESIZE);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    size_t offsets[sizeof(words) / sizeof(words[0])];
    hl_a64_assembler assembler;
    CHECK(code != MAP_FAILED);
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t i = 0; i < sizeof(words) / sizeof(words[0]); i++) {
        offsets[i] = hl_a64_assembler_size(&assembler);
        CHECK(hl_a64_conditional_emit(&assembler, words[i], 0x4000 + i * 0x10));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.flags = UINT64_C(0x40000000);
    CHECK(outcome(&cpu, code, offsets[0], 0x4008));
    CHECK(cpu.flags == UINT64_C(0x40000000));
    cpu.flags = 0;
    CHECK(outcome(&cpu, code, offsets[0], 0x4004));
    CHECK(outcome(&cpu, code, offsets[1], 0x4018));

    cpu.registers[30] = UINT64_C(0x100000000);
    CHECK(outcome(&cpu, code, offsets[2], 0x4028)); /* W30 is zero */
    cpu.registers[30] = 1;
    CHECK(outcome(&cpu, code, offsets[2], 0x4024));
    cpu.registers[18] = 7;
    CHECK(outcome(&cpu, code, offsets[3], 0x4038));
    cpu.registers[18] = 0;
    CHECK(outcome(&cpu, code, offsets[3], 0x4034));
    cpu.registers[30] = 0;
    CHECK(outcome(&cpu, code, offsets[4], 0x4048));
    cpu.registers[30] = UINT64_C(1) << 63;
    CHECK(outcome(&cpu, code, offsets[4], 0x4044));
    cpu.registers[28] = 1u << 7;
    CHECK(outcome(&cpu, code, offsets[5], 0x4058));
    cpu.registers[28] = 0;
    CHECK(outcome(&cpu, code, offsets[5], 0x4054));
    cpu.flags = UINT64_C(0x40000000);
    CHECK(outcome(&cpu, code, offsets[6], 0x405c));
    cpu.registers[30] = 0;
    CHECK(outcome(&cpu, code, offsets[7], 0x406c));
    CHECK(outcome(&cpu, code, offsets[8], 0x407c));
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_CONDITIONAL_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_conditional_emit(&assembler, words[0], 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_conditional_emit(&assembler, 0xd503201fu, 0x9000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
