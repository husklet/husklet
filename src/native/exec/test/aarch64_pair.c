#include "../src/arch/aarch64/pair.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "pair:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void);
    _Static_assert(sizeof(entry) == sizeof(address), "native code pointer size drifted");
    memcpy(&entry, &address, sizeof(entry));
    hl_native_aarch64_enter(cpu, entry);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 2;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    size_t store = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_pair_emit(&assembler, 0xa9bf7bfdu, 0x4000)); /* stp x29,x30,[sp,#-16]! */
    size_t load = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_pair_emit(&assembler, 0xa8c17bfdu, 0x4004)); /* ldp x29,x30,[sp],#16 */
    size_t stolen_store = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_pair_emit(&assembler, 0xa907c3f1u, 0x4008)); /* stp x17,x16,[sp,#120] */
    size_t stolen_load = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_pair_emit(&assembler, 0xa947c3f1u, 0x400c)); /* ldp x17,x16,[sp,#120] */
    size_t vector_store = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_pair_emit(&assembler, 0xad0307e0u, 0x4010)); /* stp q0,q1,[sp,#96] */
    size_t vector_load = hl_a64_assembler_size(&assembler);
    CHECK(hl_a64_pair_emit(&assembler, 0xad4307e0u, 0x4014)); /* ldp q0,q1,[sp,#96] */
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    _Alignas(16) uint8_t stack[256] = {0};
    hl_native_aarch64_cpu cpu = {0};
    uint64_t top = ((uint64_t)(uintptr_t)(stack + sizeof(stack))) & ~UINT64_C(15);
    cpu.stack = top;
    cpu.registers[29] = UINT64_C(0x2929292929292929);
    cpu.registers[30] = UINT64_C(0x3030303030303030);
    cpu.flags = UINT64_C(0xa0000000);
    cpu.memory_first = (uint64_t)(uintptr_t)stack;
    cpu.memory_last = top;
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    execute(&cpu, code + store);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x4004 && cpu.stack == top - 16);
    CHECK(*(uint64_t *)(uintptr_t)(top - 16) == UINT64_C(0x2929292929292929));
    CHECK(*(uint64_t *)(uintptr_t)(top - 8) == UINT64_C(0x3030303030303030));
    CHECK(cpu.flags == UINT64_C(0xa0000000));
    cpu.registers[29] = cpu.registers[30] = 0;
    execute(&cpu, code + load);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x4008 && cpu.stack == top);
    CHECK(cpu.registers[29] == UINT64_C(0x2929292929292929));
    CHECK(cpu.registers[30] == UINT64_C(0x3030303030303030));
    CHECK(cpu.flags == UINT64_C(0xa0000000));

    cpu.stack = (uint64_t)(uintptr_t)stack;
    cpu.registers[16] = UINT64_C(0x1616161616161616);
    cpu.registers[17] = UINT64_C(0x1717171717171717);
    execute(&cpu, code + stolen_store);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x400c);
    CHECK(*(uint64_t *)(void *)(stack + 120) == UINT64_C(0x1717171717171717));
    CHECK(*(uint64_t *)(void *)(stack + 128) == UINT64_C(0x1616161616161616));
    cpu.registers[16] = cpu.registers[17] = 0;
    execute(&cpu, code + stolen_load);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x4010);
    CHECK(cpu.registers[16] == UINT64_C(0x1616161616161616));
    CHECK(cpu.registers[17] == UINT64_C(0x1717171717171717));

    cpu.vectors[0] = UINT64_C(0x0011223344556677);
    cpu.vectors[1] = UINT64_C(0x8899aabbccddeeff);
    cpu.vectors[2] = UINT64_C(0x1021324354657687);
    cpu.vectors[3] = UINT64_C(0x98a9bacbdcedfe0f);
    execute(&cpu, code + vector_store);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x4014);
    CHECK(*(uint64_t *)(void *)(stack + 96) == cpu.vectors[0]);
    CHECK(*(uint64_t *)(void *)(stack + 104) == cpu.vectors[1]);
    CHECK(*(uint64_t *)(void *)(stack + 112) == cpu.vectors[2]);
    CHECK(*(uint64_t *)(void *)(stack + 120) == cpu.vectors[3]);
    memset(cpu.vectors, 0, 4 * sizeof(cpu.vectors[0]));
    execute(&cpu, code + vector_load);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x4018);
    CHECK(cpu.vectors[0] == UINT64_C(0x0011223344556677));
    CHECK(cpu.vectors[1] == UINT64_C(0x8899aabbccddeeff));
    CHECK(cpu.vectors[2] == UINT64_C(0x1021324354657687));
    CHECK(cpu.vectors[3] == UINT64_C(0x98a9bacbdcedfe0f));

    memset(stack + sizeof(stack) - 16, 0x5a, 16);
    cpu.stack = top;
    cpu.memory_last = top - 8;
    execute(&cpu, code + store);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x4000 && cpu.stack == top);
    CHECK(cpu.fault_address == top - 16 && cpu.fault_access == HL_A64_PERMISSION_WRITE);
    for (unsigned index = 0; index < 16; index++) CHECK(stack[sizeof(stack) - 16 + index] == 0x5a);
    CHECK(cpu.flags == UINT64_C(0xa0000000));
    cpu.memory_last = top;
    cpu.memory_permissions = HL_A64_PERMISSION_READ;
    cpu.stack = top;
    execute(&cpu, code + store);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.stack == top);
    CHECK(cpu.fault_access == HL_A64_PERMISSION_WRITE);
    for (unsigned index = 0; index < 16; index++) CHECK(stack[sizeof(stack) - 16 + index] == 0x5a);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_PAIR_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_pair_emit(&assembler, 0xa9bf7bfdu, 0x5000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    for (size_t index = 0; index < sizeof(short_buffer); index++) CHECK(short_buffer[index] == 0xa5);
    return 0;
#endif
}
