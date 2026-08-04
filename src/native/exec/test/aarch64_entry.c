#include "../include/executor.h"
#include "../src/arch/aarch64/assembler.h"
#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/stub.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "entry:%d: %s\n", __LINE__, #x); return 1; } } while (0)

#if defined(__aarch64__)
hl_native_aarch64_cpu hl_test_inner_cpu;
extern void hl_test_nested(void);

__asm__(
    ".text\n"
    ".p2align 2\n"
    ".global hl_test_nested\n"
    ".type hl_test_nested,%function\n"
    "hl_test_nested:\n"
    "mov x20,x0\n"
    "adrp x0,hl_test_inner_cpu\n"
    "add x0,x0,:lo12:hl_test_inner_cpu\n"
    "adrp x1,hl_native_aarch64_fault_return\n"
    "add x1,x1,:lo12:hl_native_aarch64_fault_return\n"
    "bl hl_native_aarch64_enter\n"
    "mov x0,x20\n"
    "b hl_native_aarch64_fault_return\n"
    ".size hl_test_nested,.-hl_test_nested\n");

static uint64_t read_fpcr(void) {
    uint64_t value;
    __asm__ volatile("mrs %0,fpcr" : "=r"(value));
    return value;
}

static uint64_t read_fpsr(void) {
    uint64_t value;
    __asm__ volatile("mrs %0,fpsr" : "=r"(value));
    return value;
}

static void write_fp(uint64_t fpcr, uint64_t fpsr) {
    __asm__ volatile("msr fpcr,%0\nmsr fpsr,%1" : : "r"(fpcr), "r"(fpsr) : "memory");
}

static uint64_t read_sp(void) {
    uint64_t value;
    __asm__ volatile("mov %0,sp" : "=r"(value));
    return value;
}

static void enter(hl_native_aarch64_cpu *cpu, void *address) {
    void (*target)(void);
    memcpy(&target, &address, sizeof(target));
    hl_native_aarch64_enter(cpu, target);
}

static void seed(hl_native_aarch64_cpu *cpu, uint64_t stack, uint64_t fpcr, uint64_t fpsr) {
    memset(cpu, 0, sizeof(*cpu));
    cpu->stack = stack;
    cpu->fpcr = fpcr;
    cpu->fpsr = fpsr;
    uint32_t one = UINT32_C(0x3f800000), zero = 0;
    memcpy(&cpu->vectors[2], &one, sizeof(one));
    memcpy(&cpu->vectors[4], &zero, sizeof(zero));
}
#endif

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    const uint64_t host_fpcr = UINT64_C(0x00800000);
    const uint64_t host_fpsr = UINT64_C(0x00000010);
    const uint64_t guest_fpcr = UINT64_C(0x00400000);
    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > 0);
    size_t capacity = (size_t)page;
    uint8_t *code = mmap(NULL, capacity, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED);
    hl_a64_assembler assembler;
    CHECK(hl_a64_assembler_begin(&assembler, code, code, capacity));
    hl_a64_stub_prologue(&assembler);
    hl_a64_emit32(&assembler, UINT32_C(0x1e221820)); /* fdiv s0,s1,s2: 1.0 / 0.0 */
    hl_a64_stub_exit(&assembler, HL_NATIVE_EXIT_BRANCH, UINT64_C(0x5004));
    CHECK(hl_a64_assembler_ok(&assembler));
    __builtin___clear_cache((char *)code, (char *)code + hl_a64_assembler_size(&assembler));
    CHECK(mprotect(code, capacity, PROT_READ | PROT_EXEC) == 0);

    uint8_t guest_stack[256] __attribute__((aligned(16)));
    hl_native_aarch64_cpu cpu;
    seed(&cpu, (uint64_t)(uintptr_t)(guest_stack + sizeof(guest_stack)), guest_fpcr, 0);
    write_fp(host_fpcr, host_fpsr);
    uint64_t host_sp = read_sp();
    enter(&cpu, code);
    CHECK(read_sp() == host_sp);
    CHECK(read_fpcr() == host_fpcr && read_fpsr() == host_fpsr);
    CHECK(cpu.fpcr == guest_fpcr && (cpu.fpsr & UINT64_C(0x02)) != 0);
    CHECK(cpu.reason == HL_NATIVE_EXIT_BRANCH && cpu.program == UINT64_C(0x5004));
    uint32_t infinity;
    memcpy(&infinity, &cpu.vectors[0], sizeof(infinity));
    CHECK(infinity == UINT32_C(0x7f800000));

    for (unsigned iteration = 0; iteration < 3; iteration++) {
        seed(&cpu, (uint64_t)(uintptr_t)(guest_stack + sizeof(guest_stack)), guest_fpcr, iteration);
        write_fp(host_fpcr, host_fpsr);
        host_sp = read_sp();
        enter(&cpu, (void *)(uintptr_t)hl_native_aarch64_fallback);
        CHECK(read_sp() == host_sp);
        CHECK(read_fpcr() == host_fpcr && read_fpsr() == host_fpsr);
        CHECK(cpu.fpcr == guest_fpcr && cpu.fpsr == iteration);
    }

    seed(&cpu, (uint64_t)(uintptr_t)(guest_stack + sizeof(guest_stack)), guest_fpcr, 0);
    write_fp(host_fpcr, host_fpsr);
    host_sp = read_sp();
    enter(&cpu, (void *)(uintptr_t)hl_native_aarch64_fault_return);
    CHECK(read_sp() == host_sp);
    CHECK(read_fpcr() == host_fpcr && read_fpsr() == host_fpsr);

    memset(&hl_test_inner_cpu, 0, sizeof(hl_test_inner_cpu));
    seed(&cpu, (uint64_t)(uintptr_t)(guest_stack + sizeof(guest_stack)), guest_fpcr, 0);
    write_fp(host_fpcr, host_fpsr);
    host_sp = read_sp();
    enter(&cpu, (void *)(uintptr_t)hl_test_nested);
    CHECK(read_sp() == host_sp);
    CHECK(read_fpcr() == host_fpcr && read_fpsr() == host_fpsr);
    CHECK(hl_test_inner_cpu.host_stack != 0 && cpu.host_stack != 0 &&
          hl_test_inner_cpu.host_stack != cpu.host_stack);

    write_fp(0, 0);
    CHECK(munmap(code, capacity) == 0);
    return 0;
#endif
}
