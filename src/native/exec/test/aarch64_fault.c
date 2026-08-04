#ifndef _GNU_SOURCE
#define _GNU_SOURCE 1
#endif

#include "../src/arch/aarch64/fault.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/trace.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__linux__) && defined(__aarch64__)
#include <asm/sigcontext.h>
#include <ucontext.h>
#elif defined(__APPLE__) && defined(__aarch64__)
#include <sys/ucontext.h>
#endif

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "a64-fault:%d: %s\n", __LINE__, #x); return 1; } } while (0)

typedef struct fault_case {
    uint32_t word;
    uint32_t access;
    uint32_t width;
} fault_case;

static void seed(hl_native_aarch64_cpu *cpu, hl_a64_host_context *host) {
    memset(cpu, 0, sizeof(*cpu));
    memset(host, 0, sizeof(*host));
    for (unsigned reg = 0; reg <= 30; ++reg) {
        cpu->registers[reg] = UINT64_C(0xc000000000000000) | reg;
        host->registers[reg] = UINT64_C(0xa000000000000000) | reg;
    }
    host->registers[28] = (uint64_t)(uintptr_t)cpu;
    host->stack = UINT64_C(0x700000001000);
    host->program = UINT64_C(0xdeadbeef);
    host->pstate = UINT64_C(0xffffffffffffffff);
    for (unsigned lane = 0; lane < 64; ++lane)
        host->vectors[lane] = UINT64_C(0xb000000000000000) | lane;
    host->fpcr = 0x1234;
    host->fpsr = 0x5678;
    host->vectors_valid = 1;
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    static const fault_case cases[] = {
        {0xb9000462u, HL_NATIVE_ACCESS_WRITE, 4},  /* scalar unsigned */
        {0xf81f8c62u, HL_NATIVE_ACCESS_WRITE, 8}, /* scalar pre-index */
        {0xf8408462u, HL_NATIVE_ACCESS_READ, 8},  /* scalar post-index */
        {0xf8627820u, HL_NATIVE_ACCESS_READ, 8},  /* register LSL */
        {0xf8624820u, HL_NATIVE_ACCESS_READ, 8},  /* register UXTW */
        {0xf862c820u, HL_NATIVE_ACCESS_READ, 8},  /* register SXTW */
        {0x58000000u, HL_NATIVE_ACCESS_READ, 8},  /* literal */
        {0x3dc00020u, HL_NATIVE_ACCESS_READ, 16}, /* scalar SIMD */
        {0xa9bf7bfdu, HL_NATIVE_ACCESS_WRITE, 16},/* pair/stolen targets */
        {0x88df7c22u, HL_NATIVE_ACCESS_READ, 4},  /* ordered */
        {0x4c408020u, HL_NATIVE_ACCESS_READ, 32}, /* structure */
        {0xd50b7421u, HL_NATIVE_ACCESS_WRITE, 16},/* DC ZVA */
    };
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page * 4;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    for (size_t current = 0; current < sizeof(cases) / sizeof(cases[0]); ++current) {
        const uint32_t words[] = {cases[current].word, 0xd4000001u};
        uint64_t guest = UINT64_C(0x4000) + current * 8;
        const hl_a64_source_span span = {guest, (const uint8_t *)words, sizeof(words), 7, 8};
        const hl_a64_source source = {&span, 1, 7, 8};
        hl_a64_trace_result trace;
        CHECK(hl_a64_trace_build(&source, guest, 2, code, capacity, &trace));
        unsigned found = 0;
        for (uint32_t index = 0; index < trace.provenance_count; ++index) {
            const hl_native_provenance *record = &trace.provenance[index];
            if (record->access == HL_NATIVE_ACCESS_UNKNOWN) continue;
            CHECK(record->access == cases[current].access && record->width == cases[current].width);
            hl_native_aarch64_cpu cpu;
            hl_a64_host_context host;
            seed(&cpu, &host);
            CHECK(hl_a64_fault_reconstruct(&cpu, &host, record));
            for (unsigned reg = 0; reg <= 30; ++reg) {
                int stolen = reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
                uint64_t expected = stolen ? (UINT64_C(0xc000000000000000) | reg)
                                           : (UINT64_C(0xa000000000000000) | reg);
                CHECK(cpu.registers[reg] == expected);
            }
            CHECK(cpu.stack == host.stack && cpu.program == guest);
            CHECK(cpu.flags == UINT64_C(0xf0000000));
            CHECK(memcmp(cpu.vectors, host.vectors, sizeof(cpu.vectors)) == 0);
            CHECK(cpu.fpcr == host.fpcr && cpu.fpsr == host.fpsr);
            ++found;
        }
        CHECK(found == (cases[current].word == 0xd50b7421u ? 4u : 1u));
    }

    hl_native_aarch64_cpu cpu;
    hl_a64_host_context host;
    seed(&cpu, &host);
    hl_native_provenance valid = {
        .code_size = 4, .guest = 0x8000,
        .address = {.kind = HL_NATIVE_ADDRESS_BASE, .bits = 64, .base = 16},
        .access = HL_NATIVE_ACCESS_READ, .width = 8};
    hl_native_aarch64_cpu before = cpu;
    host.vectors_valid = 0;
    CHECK(!hl_a64_fault_reconstruct(&cpu, &host, &valid));
    CHECK(memcmp(&cpu, &before, sizeof(cpu)) == 0);
    seed(&cpu, &host);
    before = cpu;
    host.registers[28]++;
    CHECK(!hl_a64_fault_reconstruct(&cpu, &host, &valid));
    CHECK(memcmp(&cpu, &before, sizeof(cpu)) == 0);
    seed(&cpu, &host);
    valid.address.base = 1;
    CHECK(!hl_a64_fault_reconstruct(&cpu, &host, &valid));
    valid.address.base = 16;
    valid.access = HL_NATIVE_ACCESS_EXECUTE;
    CHECK(!hl_a64_fault_reconstruct(&cpu, &host, &valid));
    valid.access = HL_NATIVE_ACCESS_READ;
    valid.width = 3;
    CHECK(!hl_a64_fault_reconstruct(&cpu, &host, &valid));
#if defined(__linux__) && defined(__aarch64__)
    ucontext_t linux_context;
    memset(&linux_context, 0, sizeof(linux_context));
    for (unsigned reg = 0; reg <= 30; ++reg)
        linux_context.uc_mcontext.regs[reg] = UINT64_C(0xd000000000000000) | reg;
    linux_context.uc_mcontext.sp = UINT64_C(0x710000001000);
    linux_context.uc_mcontext.pc = UINT64_C(0x720000002000);
    linux_context.uc_mcontext.pstate = UINT64_C(0xa0000000);
    struct fpsimd_context *fpsimd =
        (struct fpsimd_context *)(void *)linux_context.uc_mcontext.__reserved;
    fpsimd->head.magic = FPSIMD_MAGIC;
    fpsimd->head.size = sizeof(*fpsimd);
    fpsimd->fpcr = 0x2468;
    fpsimd->fpsr = 0x1357;
    for (unsigned vector = 0; vector < 32; ++vector)
        fpsimd->vregs[vector] = ((__uint128_t)(UINT64_C(0xe000000000000000) | vector) << 64) |
                                (UINT64_C(0xf000000000000000) | vector);
    hl_a64_host_context extracted;
    CHECK(hl_a64_linux_context(&linux_context, &extracted));
    CHECK(extracted.registers[7] == UINT64_C(0xd000000000000007));
    CHECK(extracted.stack == linux_context.uc_mcontext.sp &&
          extracted.program == linux_context.uc_mcontext.pc &&
          extracted.pstate == linux_context.uc_mcontext.pstate);
    CHECK(extracted.fpcr == fpsimd->fpcr && extracted.fpsr == fpsimd->fpsr);
    CHECK(extracted.vectors[0] == UINT64_C(0xf000000000000000));
    CHECK(extracted.vectors[1] == UINT64_C(0xe000000000000000));
    seed(&cpu, &host);
    linux_context.uc_mcontext.regs[28] = (uint64_t)(uintptr_t)&cpu;
    valid.width = 8;
    CHECK(hl_a64_linux_fault_reconstruct(&cpu, &linux_context, &valid));
    CHECK(cpu.registers[7] == UINT64_C(0xd000000000000007));
    fpsimd->head.size = sizeof(struct _aarch64_ctx) - 1;
    CHECK(!hl_a64_linux_context(&linux_context, &extracted));

    memset(&linux_context, 0, sizeof(linux_context));
    fpsimd = (struct fpsimd_context *)(void *)linux_context.uc_mcontext.__reserved;
    fpsimd->head.magic = FPSIMD_MAGIC;
    fpsimd->head.size = sizeof(*fpsimd);
    seed(&cpu, &host);
    cpu.host_stack = UINT64_C(0x1000);
    cpu.memory_delta = UINT64_C(0x1000);
    linux_context.uc_mcontext.regs[16] = UINT64_C(0x9000);
    linux_context.uc_mcontext.regs[28] = (uint64_t)(uintptr_t)&cpu;
    linux_context.uc_mcontext.sp = UINT64_C(0x700000001000);
    valid.width = 8;
    valid.guest = UINT64_C(0x8120);
    CHECK(hl_a64_linux_fault_return(&cpu, &linux_context, &valid, UINT64_C(0x9004)));
    CHECK(cpu.reason == HL_NATIVE_EXIT_FAULT && cpu.program == valid.guest);
    CHECK(cpu.fault_address == UINT64_C(0x8004));
    CHECK(cpu.fault_access == HL_NATIVE_ACCESS_READ && cpu.fault_size == 8);
    CHECK(linux_context.uc_mcontext.regs[0] == (uint64_t)(uintptr_t)&cpu);
    CHECK(linux_context.uc_mcontext.pc == (uint64_t)(uintptr_t)hl_native_aarch64_fault_return);

    seed(&cpu, &host);
    cpu.host_stack = UINT64_C(0x1000);
    cpu.memory_delta = UINT64_C(0x1000);
    linux_context.uc_mcontext.regs[16] = UINT64_C(0x9000);
    linux_context.uc_mcontext.regs[28] = (uint64_t)(uintptr_t)&cpu;
    hl_native_aarch64_cpu unchanged = cpu;
    CHECK(!hl_a64_linux_fault_return(&cpu, &linux_context, &valid, UINT64_C(0x9010)));
    CHECK(memcmp(&cpu, &unchanged, sizeof(cpu)) == 0);
#endif

#if defined(__APPLE__) && defined(__aarch64__)
    _STRUCT_MCONTEXT64 machine;
    ucontext_t darwin_context;
    memset(&machine, 0, sizeof(machine));
    memset(&darwin_context, 0, sizeof(darwin_context));
    darwin_context.uc_mcontext = &machine;
    for (unsigned reg = 0; reg < 29; ++reg)
        machine.__ss.__x[reg] = UINT64_C(0xd000000000000000) | reg;
    machine.__ss.__fp = UINT64_C(0xd00000000000001d);
    machine.__ss.__lr = UINT64_C(0xd00000000000001e);
    machine.__ss.__sp = UINT64_C(0x710000001000);
    machine.__ss.__pc = UINT64_C(0x720000002000);
    machine.__ss.__cpsr = UINT32_C(0xa0000000);
    for (unsigned vector = 0; vector < 32; ++vector)
        machine.__ns.__v[vector] = ((__uint128_t)(UINT64_C(0xe000000000000000) | vector) << 64) |
                                   (UINT64_C(0xf000000000000000) | vector);
    machine.__ns.__fpcr = 0x1234;
    machine.__ns.__fpsr = 0x5678;
    hl_a64_host_context extracted;
    CHECK(hl_a64_darwin_context(&darwin_context, &extracted));
    for (unsigned reg = 0; reg <= 30; ++reg)
        CHECK(extracted.registers[reg] == (UINT64_C(0xd000000000000000) | reg));
    CHECK(extracted.stack == machine.__ss.__sp && extracted.program == machine.__ss.__pc);
    CHECK(extracted.pstate == machine.__ss.__cpsr && extracted.vectors_valid == 1);
    CHECK(extracted.vectors[0] == UINT64_C(0xf000000000000000));
    CHECK(extracted.vectors[1] == UINT64_C(0xe000000000000000));
    CHECK(extracted.fpcr == machine.__ns.__fpcr && extracted.fpsr == machine.__ns.__fpsr);

    darwin_context.uc_mcontext = NULL;
    CHECK(!hl_a64_darwin_context(&darwin_context, &extracted));
    darwin_context.uc_mcontext = &machine;
    seed(&cpu, &host);
    cpu.host_stack = UINT64_C(0x1000);
    cpu.memory_delta = UINT64_C(0x1000);
    machine.__ss.__x[16] = UINT64_C(0x9000);
    machine.__ss.__x[28] = (uint64_t)(uintptr_t)&cpu;
    valid.width = 8;
    valid.guest = UINT64_C(0x8120);
    CHECK(hl_a64_darwin_fault_return(&cpu, &darwin_context, &valid, UINT64_C(0x9007)));
    CHECK(cpu.reason == HL_NATIVE_EXIT_FAULT && cpu.program == valid.guest);
    CHECK(cpu.fault_address == UINT64_C(0x8007));
    CHECK(cpu.fault_access == HL_NATIVE_ACCESS_READ && cpu.fault_size == 8);
    CHECK(machine.__ss.__x[0] == (uint64_t)(uintptr_t)&cpu);
    CHECK(machine.__ss.__pc == (uint64_t)(uintptr_t)hl_native_aarch64_fault_return);

    seed(&cpu, &host);
    cpu.host_stack = UINT64_C(0x1000);
    cpu.memory_delta = UINT64_C(0x1000);
    machine.__ss.__x[16] = UINT64_C(0x9000);
    machine.__ss.__x[28] = (uint64_t)(uintptr_t)&cpu;
    hl_native_aarch64_cpu unchanged = cpu;
    CHECK(!hl_a64_darwin_fault_return(&cpu, &darwin_context, &valid, UINT64_C(0x9008)));
    CHECK(memcmp(&cpu, &unchanged, sizeof(cpu)) == 0);
#endif

    /* The dedicated path consumes the already reconstructed record directly;
     * it reaches the normal host-frame restore without running a guest spill. */
    memset(&cpu, 0, sizeof(cpu));
    cpu.program = UINT64_C(0xfeedface);
    cpu.reason = HL_NATIVE_EXIT_FAULT;
    cpu.fault_address = UINT64_C(0x12345678);
    cpu.fault_access = HL_NATIVE_ACCESS_WRITE;
    cpu.fault_size = 4;
    cpu.registers[16] = UINT64_C(0x1616161616161616);
    hl_native_aarch64_enter(&cpu, hl_native_aarch64_fault_return);
    CHECK(cpu.program == UINT64_C(0xfeedface) && cpu.reason == HL_NATIVE_EXIT_FAULT);
    CHECK(cpu.fault_address == UINT64_C(0x12345678));
    CHECK(cpu.fault_access == HL_NATIVE_ACCESS_WRITE && cpu.fault_size == 4);
    CHECK(cpu.registers[16] == UINT64_C(0x1616161616161616));
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
