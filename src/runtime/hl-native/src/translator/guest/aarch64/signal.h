#ifndef HL_TRANSLATOR_GUEST_AARCH64_SIGNAL_H
#define HL_TRANSLATOR_GUEST_AARCH64_SIGNAL_H

#include <stdint.h>

struct cpu;

typedef uint64_t (*hl_aarch64_signal_pc_fn)(void *context, uint64_t pc);
typedef int (*hl_aarch64_signal_cache_fn)(void *context, uint64_t pc);

typedef struct hl_aarch64_signal_state {
    uint64_t handler;
    uint64_t flags;
    uint64_t mask;
    int *error;
    int *code;
    uint64_t *value;
    uint64_t *address;
    int *pid;
    int *uid;
    uint64_t sigreturn_pc;
    int trace;
    hl_aarch64_signal_pc_fn canonicalize_pc;
    void *callback_context;
} hl_aarch64_signal_state;

void hl_aarch64_signal_build(struct cpu *cpu, int signal_number, const hl_aarch64_signal_state *state);
void hl_aarch64_signal_restore(struct cpu *cpu);
int hl_aarch64_signal_capture(struct cpu *cpu, void *native_context, hl_aarch64_signal_cache_fn cache_contains,
                              void *callback_context);
void hl_aarch64_signal_resume(struct cpu *cpu, void *native_context, uintptr_t dispatcher_return);

#endif
