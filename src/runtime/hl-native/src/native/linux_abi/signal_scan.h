#ifndef HL_LINUX_ABI_SIGNAL_SCAN_H
#define HL_LINUX_ABI_SIGNAL_SCAN_H

// Target composition fragment: the signal domain includes this only after defining `struct cpu`, the
// pending words `g_pending`/`g_pending_hi`/`cpu->tpending`, `thread_pending_hi_load()`, and the
// per-signal predicate `signal_deliverable()`.
//
// THE DISPATCHER'S SIGNAL POLL, and why it is driven by the pending words rather than by the signal
// numbers. engine/dispatch.c's run_guest() asks this question twice per dispatcher crossing -- once to
// re-arm cpu->irq at the loop top, once to deliver at the bottom -- and a crossing is one guest basic
// block, so it is the most frequently evaluated predicate in the engine on both backends. On a process
// with no signal outstanding, which is nearly every process nearly all of the time, the answer is no.
//
// Asking it as `for (signal = 64; signal >= 1; --signal)` pays two seq_cst pending loads per signal
// NUMBER -- 128 of them for one no -- and signal_deliverable() rejects every signal whose process- and
// thread-pending bits are both clear before it looks at anything else. So the scan enumerates the set
// bits of the pending WORDS instead: by the signal_pending_bit() convention every writer uses, bit N of
// the low words is signal N, and signal 64 lives alone in the hi words. Same answer, no memory traffic
// beyond the words themselves, and the cost now scales with the signals actually outstanding.
//
// Order is preserved (64 first, then descending) although the result is a disjunction and cannot depend
// on it -- keeping it means this fragment and maybe_deliver_signal() walk the priority the same way.

#include <stdint.h>

// Non-zero when some signal is pending for this thread AND may be acted on now; see signal_deliverable()
// for why merely pending is insufficient.
static int signal_deliverable_for_cpu(const struct cpu *cpu) {
    if ((__atomic_load_n(&g_pending_hi, __ATOMIC_SEQ_CST) | thread_pending_hi_load(cpu)) != 0 &&
        signal_deliverable(cpu, 64))
        return 1;
    uint64_t pending =
        __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&cpu->tpending, __ATOMIC_SEQ_CST);
    while (pending != 0) {
        int signal = 63 - __builtin_clzll(pending);
        pending &= ~(UINT64_C(1) << signal);
        // Bit 0 is never set -- signal_pending_bit() starts at bit 1 -- and signal_deliverable() would
        // reject signal 0 anyway, so no guard is needed here.
        if (signal_deliverable(cpu, signal)) return 1;
    }
    return 0;
}

#endif
