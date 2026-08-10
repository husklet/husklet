#ifndef HL_SYSCALL_TRAP_H
#define HL_SYSCALL_TRAP_H

#include <stdint.h>

#define HL_SYSCALL_TRAP_ABI 1u

enum {
    HL_SYSCALL_TRAP_DECLINED = 0,
    HL_SYSCALL_TRAP_CONTINUE = 1,
    HL_SYSCALL_TRAP_EXIT = 2,
    HL_SYSCALL_TRAP_FAULT = 3,
    HL_SYSCALL_TRAP_REPLACE_IMAGE = 4
};

typedef struct hl_syscall_cpu_aarch64 {
    uint32_t abi;
    uint32_t size;
    uint64_t x[31];
    uint64_t sp;
    uint64_t pc;
    uint64_t tls;
    uint64_t nzcv;
} hl_syscall_cpu_aarch64;

typedef struct hl_syscall_trap_result {
    uint32_t abi;
    uint32_t size;
    uint32_t outcome;
    int32_t exit_status;
    uint64_t image_generation;
} hl_syscall_trap_result;

_Static_assert(sizeof(hl_syscall_cpu_aarch64) == 288, "AArch64 syscall snapshot ABI drifted");
_Static_assert(sizeof(hl_syscall_trap_result) == 24, "syscall trap result ABI drifted");

typedef int32_t (*hl_syscall_trap_fn)(void *context, uint32_t guest_isa, hl_syscall_cpu_aarch64 *cpu,
                                     hl_syscall_trap_result *result);

void hl_target_syscall_trap_install(void *context, hl_syscall_trap_fn dispatch);

#endif
