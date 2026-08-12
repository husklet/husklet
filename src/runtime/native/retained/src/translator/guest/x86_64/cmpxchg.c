#include "cmpxchg.h"

#include <stdatomic.h>
#include <stdint.h>

#include "cpu.h"

// cmpxchg16b (R_CMPXCHG16): atomic 128-bit compare-exchange under a hashed spinlock. The operand's host EA
// is in cpu->x87_ea (rip already advanced past the insn). A hashed array of 64-bit-atomic locks makes this
// livelock-free (unlike a hardware CASPAL, which store-forwarding-replays forever on Apple Silicon) while
// still serialising two guest threads that target the same 16 bytes (same address -> same lock). x86:
// compare RDX:RAX with [m]; if equal store RCX:RBX and set ZF=1; else load [m] into RDX:RAX and clear ZF.
// Only ZF is affected -- the other flags (already materialized into cpu->nzcv / pf / af) are left untouched.
#define DWCAS_NLOCK 256
static _Atomic unsigned g_dwcas_lock[DWCAS_NLOCK];

void hl_x86_cmpxchg16(struct cpu *cpu) {
    uint64_t ea = cpu->x87_ea;
    volatile uint64_t *memory = (volatile uint64_t *)ea; // memory[0]=lo, memory[1]=hi
    unsigned hash = (unsigned)((ea >> 4) & (DWCAS_NLOCK - 1));
    _Atomic unsigned *lock = &g_dwcas_lock[hash];
    while (atomic_exchange_explicit(lock, 1u, memory_order_acquire))
        ; // spin (64-bit atomic exchange -> replay-immune, and a spinlock always makes forward progress)
    uint64_t low = memory[0], high = memory[1];
    int equal = (low == cpu->r[RAX] && high == cpu->r[RDX]);
    if (equal) {
        memory[0] = cpu->r[RBX];
        memory[1] = cpu->r[RCX];
    } else {
        cpu->r[RAX] = low; // Intel: on mismatch RDX:RAX <- [m]
        cpu->r[RDX] = high;
    }
    atomic_store_explicit(lock, 0u, memory_order_release);
    if (equal) // x86 ZF is stored in cpu->nzcv bit 30 (ARM Z); only ZF changes.
        cpu->nzcv |= (UINT64_C(1) << 30);
    else
        cpu->nzcv &= ~(UINT64_C(1) << 30);
}
