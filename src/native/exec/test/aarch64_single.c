#include "../src/arch/aarch64/single.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/projection.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "single:%d: %s\n", __LINE__, #x); return 1; } } while (0)

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
    size_t capacity = (size_t)page * 8;
    _Alignas(16) uint8_t stack[256] = {0};
    uint64_t base = (uint64_t)(uintptr_t)(stack + 64);
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    const uint32_t words[] = {
        0xf90007feu, /* str x30,[sp,#8] */
        0xf94007feu, /* ldr x30,[sp,#8] */
        0xf81f8ffeu, /* str x30,[sp,#-8]! */
        0xf84087feu, /* ldr x30,[sp],#8 */
        0xf8617bfeu, /* ldr x30,[sp,x1,lsl#3] */
        0x3dc007e0u, /* ldr q0,[sp,#16] */
        0x398003feu, /* ldrsb x30,[sp] */
        0x5800001eu, /* ldr x30,[pc] */
        0xf9800000u, /* prfm pldl1keep,[x0] */
        0xf8800000u, /* prfum pldl1keep,[x0] */
        0xf8a16800u, /* prfm pldl1keep,[x0,x1] */
        0xd8000020u, /* prfm pldl1keep,pc+4 */
    };
    size_t offsets[sizeof(words) / sizeof(words[0])];
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    for (size_t index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        offsets[index] = hl_a64_assembler_size(&assembler);
        uint64_t pc = index == 7 ? base : 0x4000 + index * 4;
        CHECK(hl_a64_single_emit(&assembler, words[index], pc));
    }
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    hl_native_aarch64_cpu cpu = {0};
    cpu.stack = base;
    cpu.flags = UINT64_C(0x60000000);
    cpu.memory_first = (uint64_t)(uintptr_t)stack;
    cpu.memory_last = (uint64_t)(uintptr_t)(stack + sizeof(stack));
    cpu.memory_permissions = HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE;
    cpu.registers[30] = UINT64_C(0x1122334455667788);
    execute(&cpu, code + offsets[0]);
    CHECK(*(uint64_t *)(uintptr_t)(base + 8) == UINT64_C(0x1122334455667788));
    CHECK(cpu.stack == base && cpu.flags == UINT64_C(0x60000000));
    cpu.registers[30] = 0;
    execute(&cpu, code + offsets[1]);
    CHECK(cpu.registers[30] == UINT64_C(0x1122334455667788));
    cpu.registers[30] = UINT64_C(0xaabbccddeeff0011);
    execute(&cpu, code + offsets[2]);
    CHECK(cpu.stack == base - 8 && *(uint64_t *)(uintptr_t)(base - 8) == UINT64_C(0xaabbccddeeff0011));
    cpu.registers[30] = 0;
    execute(&cpu, code + offsets[3]);
    CHECK(cpu.registers[30] == UINT64_C(0xaabbccddeeff0011) && cpu.stack == base);
    cpu.registers[1] = 1;
    execute(&cpu, code + offsets[4]);
    CHECK(cpu.registers[30] == UINT64_C(0x1122334455667788));
    *(uint64_t *)(uintptr_t)(base + 16) = UINT64_C(0x0102030405060708);
    *(uint64_t *)(uintptr_t)(base + 24) = UINT64_C(0x1112131415161718);
    execute(&cpu, code + offsets[5]);
    CHECK(cpu.vectors[0] == UINT64_C(0x0102030405060708));
    CHECK(cpu.vectors[1] == UINT64_C(0x1112131415161718));
    stack[64] = 0x80;
    execute(&cpu, code + offsets[6]);
    CHECK(cpu.registers[30] == UINT64_C(0xffffffffffffff80));
    *(uint64_t *)(uintptr_t)base = UINT64_C(0x8899aabbccddeeff);
    execute(&cpu, code + offsets[7]);
    CHECK(cpu.registers[30] == UINT64_C(0x8899aabbccddeeff));
    cpu.registers[0] = UINT64_MAX;
    for (size_t index = 8; index < sizeof(words) / sizeof(words[0]); index++) {
        execute(&cpu, code + offsets[index]);
        CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == 0x4004 + index * 4);
    }

    memset(stack + 64, 0x5a, 16);
    cpu.stack = base;
    cpu.memory_last = base + 12; /* access [base+8,base+16) crosses the window */
    cpu.registers[30] = UINT64_C(0xdeadbeefdeadbeef);
    execute(&cpu, code + offsets[0]);
    CHECK(cpu.reason == HL_NATIVE_EXIT_FALLBACK && cpu.program == 0x4000 && cpu.stack == base);
    CHECK(cpu.registers[30] == UINT64_C(0xdeadbeefdeadbeef));
    for (unsigned index = 0; index < 16; index++) CHECK(stack[64 + index] == 0x5a);
    CHECK(munmap(code, capacity) == 0);

    uint8_t short_buffer[HL_A64_SINGLE_MAX_BYTES - 1];
    memset(short_buffer, 0xa5, sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler, short_buffer, short_buffer, sizeof(short_buffer)));
    CHECK(!hl_a64_single_emit(&assembler, words[0], 0x5000));
    CHECK(hl_a64_assembler_size(&assembler) == 0);
    return 0;
#endif
}
