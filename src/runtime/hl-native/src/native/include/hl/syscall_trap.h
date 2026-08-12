#ifndef HL_SYSCALL_TRAP_H
#define HL_SYSCALL_TRAP_H

#include "hl/base.h"

#define HL_SYSCALL_TRAP_ABI 1u
#define HL_TASK_EVENT_CLONE_THREAD UINT64_MAX
#define HL_TASK_EVENT_FORK_PROCESS (UINT64_MAX - 1u)
#define HL_TASK_EVENT_EXIT_THREAD (UINT64_MAX - 2u)
#define HL_TASK_EVENT_PREPARE_FORK (UINT64_MAX - 3u)
#define HL_TASK_EVENT_CANCEL_FORK (UINT64_MAX - 4u)
#define HL_TASK_EVENT_EXEC_THREAD (UINT64_MAX - 5u)
#define HL_TASK_EVENT_REAP_PROCESS (UINT64_MAX - 6u)
#define HL_TASK_EVENT_CREDENTIALS_CHANGED (UINT64_MAX - 7u)

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
    uint64_t task;
} hl_syscall_cpu_aarch64;

typedef struct hl_syscall_trap_result {
    uint32_t abi;
    uint32_t size;
    uint32_t outcome;
    int32_t exit_status;
    uint64_t image_generation;
} hl_syscall_trap_result;

HL_STATIC_ASSERT(sizeof(hl_syscall_cpu_aarch64) == 296, "AArch64 syscall snapshot ABI drifted");
HL_STATIC_ASSERT(sizeof(hl_syscall_trap_result) == 24, "syscall trap result ABI drifted");

typedef int32_t (*hl_syscall_trap_fn)(void *context, uint32_t guest_isa, hl_syscall_cpu_aarch64 *cpu,
                                      hl_syscall_trap_result *result);

HL_EXTERN_C_BEGIN

HL_API void hl_target_syscall_trap_install(void *context, hl_syscall_trap_fn dispatch);

HL_EXTERN_C_END

#endif
