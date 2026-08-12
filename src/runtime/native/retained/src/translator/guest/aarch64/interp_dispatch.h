// The AArch64 guest's dispatch seam for the INTERPRETER backend -- every host CPU that is not AArch64;
// dispatch.h beside it is the JIT's. core/dispatch.c's contract is only `translate_block()` then
// `run_block()` and does not require `code` to be host machine code: here it is a decoded-block descriptor
// and run_block a C loop, over an untouched struct cpu.

// SMC: same model as the JIT -- `ic ivau` drops stale translations, since decoded blocks cache guest
// instruction bytes. Reason codes must match dispatch.h's; linux_abi and the checkpoint format see them.
#define R_ICFLUSH 4
#define R_ICCOMMIT 6
static int g_smc_seen;

static inline int smc_seen(void) {
    return __atomic_load_n(&g_smc_seen, __ATOMIC_ACQUIRE);
}

static uint64_t g_smc_flushes;

// x86-frontend feature only.
#define G_DISPATCH_DEBUG(c) ((void)0)

// ssp is in the checkpoint image: a stale one restored by the JIT is a foreign shadow stack.
#define G_SHADOW_CLEAR(c) ((c)->ssp = 0)

// No emitted code, so no entry alignment to tune.
#define G_BLOCK_ALIGN 0

// No inline branch-target cache: an indirect branch is a plain c->pc assignment.
#define G_IBTC_FILL(c) ((void)0)

// Structurally identical to dispatch.h's, including `pc += 4` past the SVC (an AArch64 GUEST ABI property).
// Soft-TLB reasons can only arrive from a JIT-written checkpoint, but handle them rather than fall into
// `else R_BRANCH` and resume at a bogus PC.
#define G_DISPATCH_REASON(c)                                                                                           \
    if ((c)->reason == R_SOFTMISS || (c)->reason == R_SOFTCOMMIT || (c)->reason == R_SOFTSPAN ||                       \
        (c)->reason == R_FETCHFAULT) {                                                                                 \
        if ((c)->reason != R_FETCHFAULT) (c)->fault_addr = (c)->soft_ea;                                               \
        if (raise_guest_fetch_fault(c)) {                                                                              \
            maybe_deliver_signal(c);                                                                                   \
            continue;                                                                                                  \
        }                                                                                                              \
        break;                                                                                                         \
    } else if ((c)->reason == R_BUS) {                                                                                 \
        if (raise_guest_bus(c)) {                                                                                      \
            maybe_deliver_signal(c);                                                                                   \
            continue;                                                                                                  \
        }                                                                                                              \
        break;                                                                                                         \
    } else if ((c)->reason == R_ICFLUSH) {                                                                             \
        uint64_t _line = (c)->smc_va & ~UINT64_C(0xfff);                                                               \
        filemap_refresh_emulated(_line, _line + UINT64_C(0x1000));                                                     \
        smc_icflush((c), (c)->smc_va);                                                                                 \
    } else if ((c)->reason == R_ICCOMMIT) {                                                                            \
        if ((c)->smc_range_overflow)                                                                                   \
            filemap_refresh_emulated(0, UINT64_MAX);                                                                   \
        else                                                                                                           \
            for (uint32_t _index = 0; _index < (c)->smc_range_count; ++_index)                                         \
                filemap_refresh_emulated((c)->smc_ranges[_index][0], (c)->smc_ranges[_index][1]);                      \
        if (smc_commit(c)) g_smc_flushes++;                                                                            \
    } else if ((c)->reason == R_SYSCALL) {                                                                             \
        if (g_prof) g_prof_sys++;                                                                                      \
        service(c);                                                                                                    \
        if ((c)->exited) break;                                                                                        \
        if ((c)->redirect)                                                                                             \
            (c)->redirect = 0;                                                                                         \
        else                                                                                                           \
            (c)->pc += 4; /* execve/sigreturn set pc directly */                                                       \
    }                                                                                                                  \
    /* else R_BRANCH: c->pc already holds the target */

// interp.c has its own run_block/block_return, so dispatch.c must not emit the shared AArch64 pair.
#define G_OWN_TRAMPOLINES 1
