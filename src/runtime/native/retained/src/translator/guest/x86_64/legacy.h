#ifndef HL_TRANSLATOR_GUEST_X86_64_LEGACY_H
#define HL_TRANSLATOR_GUEST_X86_64_LEGACY_H

#include <stdint.h>

struct cpu;

typedef int64_t (*hl_x86_time_fn)(void *context);
typedef int (*hl_x86_alarm_fn)(void *context, uint64_t seconds, uint64_t *remaining_seconds);
// Nonzero when [address,address+length) is a live guest span the engine may read (write==0) or store into
// (write!=0). `address` is already folded to storage coordinates. The translator may not ask linux_abi
// directly (DOCS.md 3.3), so the target injects this the way hl_x86_avx_state injects its memory ops.
typedef int (*hl_x86_access_fn)(void *context, uint64_t address, uint64_t length, int write);

typedef struct hl_x86_legacy_context {
    uint64_t nonpie_low;
    uint64_t nonpie_high;
    uint64_t nonpie_bias;
    hl_x86_time_fn time_seconds;
    hl_x86_alarm_fn set_alarm;
    hl_x86_access_fn access_ok;
    void *callback_context;
} hl_x86_legacy_context;

int hl_x86_legacy_normalize(struct cpu *cpu, const hl_x86_legacy_context *context);
void hl_x86_legacy_restore_fork(struct cpu *cpu);
// Nonzero when the syscall most recently normalized on this thread was the legacy dup2(2).
int hl_x86_legacy_is_dup2(void);

#endif
