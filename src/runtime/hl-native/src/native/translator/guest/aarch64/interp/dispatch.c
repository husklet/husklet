#include "../../../cache_abi.h"

static int interp_step(struct cpu *cpu) {
    uint32_t insn = 0;
    if (hl_guest_fetch_u32(cpu->pc, &insn) != 0) {
        // Unreadable instruction: the JIT's R_FETCHFAULT; a guest SIGSEGV at this PC.
        cpu->fault_addr = cpu->pc;
        cpu->reason = R_FETCHFAULT;
        return INTERP_END;
    }
    switch ((insn >> 25) & 0xF) {
    case 0x0:
        // op0 == 0000 is RESERVED and its only member, UDF, is PERMANENTLY undefined -- not a gap here, so
        // deliver a guest SIGILL instead of stopping the engine (SME and SVE below ARE allocated, stay fatal).
        // cpu->pc stays ON the instruction so `pc += 4` in a handler steps over it; si_code ILL_ILLOPC and
        // si_addr the faulting PC via pcrel_base.
        interp_raise_sync_signal(cpu, 4 /* SIGILL */, 1 /* ILL_ILLOPC */, pcrel_base(cpu->pc));
        return INTERP_END;
    case 0x1: return interp_undefined(cpu, insn, "unallocated (SME)");
    case 0x2: return interp_undefined(cpu, insn, "SVE");
    case 0x3: return interp_undefined(cpu, insn, "unallocated");
    case 0x8:
    case 0x9: return interp_exec_dp_immediate(cpu, insn);
    case 0xA:
    case 0xB: return interp_exec_branch_system(cpu, insn);
    case 0x4:
    case 0x6:
    case 0xC:
    case 0xE: return interp_exec_load_store(cpu, insn);
    case 0x5:
    case 0xD: return interp_exec_dp_register(cpu, insn);
    default: return interp_exec_simd(cpu, insn);
    }
}

// Must answer 1 wherever interp_step ends a block; more is only wasteful. The recorded range must never
// be a SUBSET of the bytes executed -- map_put's range is what SMC tests.
static int interp_block_ends(uint32_t insn) {
    if ((insn & 0x7C000000u) == 0x14000000u) return 1;                           // B / BL
    if ((insn & 0xFF000010u) == 0x54000000u) return 1;                           // B.cond
    if ((insn & 0xFF000010u) == 0x54000010u) return 1;                           // BC.cond
    if ((insn & 0x7E000000u) == 0x34000000u) return 1;                           // CBZ / CBNZ
    if ((insn & 0x7E000000u) == 0x36000000u) return 1;                           // TBZ / TBNZ
    if ((insn & 0xFE000000u) == 0xD6000000u) return 1;                           // BR / BLR / RET / ERET
    if ((insn & 0xFF000000u) == 0xD4000000u) return 1;                           // SVC / BRK / HLT / ...
    if ((insn & 0xFFFFFFE0u) == 0xD50B7520u) return 1;                           // ic ivau -> R_ICFLUSH
    if ((insn & 0xFFFFF01Fu) == 0xD503301Fu && ((insn >> 5) & 7) == 6) return 1; // ISB -> R_ICCOMMIT
    return 0;
}

// Block descriptor + translate_block. Cap the block so the descriptor fits the dispatcher's arena headroom
// and one block stays preemptible by the c->irq poll; splitting at the cap is an ordinary chain exit.
#define INTERP_BLOCK_MAX_INSNS 4096u

// The descriptor only DELIMITS: no decoded instructions are cached, so re-decoding each execution keeps SMC
// coherent by construction and only the block EXTENT goes stale. Must be a distinct non-NULL pointer per
// guest PC, from the arena bump pointer (not malloc) so the arena-membership accounting keeps working.
#define INTERP_BLOCK_MAGIC UINT64_C(0x484C494E54455250) // "HLINTERP"

struct interp_block {
    uint64_t magic;       // foreign/stale descriptor guard
    uint64_t guest_start; // entry guest PC == the map key
    uint64_t guest_end;   // one past the last instruction the pre-scan decoded
    uint64_t insn_count;  // diagnostics only
};

// Unreachable (G_BLOCK_ALIGN is literal 0) but must exist: the call compiles inside `if (0)`, not `#if 0`.
static void emit32(uint32_t instruction) {
    memcpy(g_cp, &instruction, sizeof instruction);
    g_cp += sizeof instruction;
}

// Never reached (nothing to fold: this back-edge is just cpu->pc), but core/dispatch.c calls it after
// every block.
static void tier2_promote(uint64_t gpc) {
    (void)gpc;
}

static void *translate_block(uint64_t gpc) {
    HL_LOGF(&g_jit_log, HL_LOG_TAG_TRANSLATE, "isa=aarch64 backend=interp guest_pc=%#llx", (unsigned long long)gpc);
    // Observe MAP_SHARED alias writes before decoding, as translate.c does.
    uint64_t source_page = gpc & ~UINT64_C(0xFFF);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));

    // The range still covers an unfetchable instruction, so run_block raises R_FETCHFAULT at the right PC.
    uint64_t cursor = gpc;
    uint64_t count = 0;
    while (count < INTERP_BLOCK_MAX_INSNS) {
        uint32_t insn = 0;
        count++;
        cursor += 4;
        if (hl_guest_fetch_u32(cursor - 4, &insn) != 0) break;
        if (interp_block_ends(insn)) break;
    }

    // Cannot overflow: the dispatcher guaranteed CACHE_EMIT_HEADROOM. Checked anyway.
    while ((uintptr_t)g_cp & 15u)
        *g_cp++ = 0;
    if (g_cp + sizeof(struct interp_block) > g_cache + CACHE_SZ) {
        static const char message[] = "interpreter block descriptor does not fit the code arena";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        return NULL;
    }
    struct interp_block *block = (struct interp_block *)g_cp;
    g_cp += sizeof *block;
    block->magic = INTERP_BLOCK_MAGIC;
    block->guest_start = gpc;
    block->guest_end = cursor;
    block->insn_count = count;

    // Key = entry PC; [guest_start, guest_end) is the SOURCE interval map_invalidate_source_ranges() intersects.
    // `body` = the same address (no prologue); non-NULL map_body() means "live translation" to patch_links_to().
    map_put(gpc, gpc, cursor, block, block);
    // SMC precise gate: without the page marks and the 64-byte line set (what txln_flush_class() classifies
    // an `ic ivau` against), the cached block EXTENT survives a rewrite of the branch that determined it.
    txpg_mark(gpc, cursor);
    if (g_txln_active)
        for (uint64_t line = gpc >> 6; line <= (cursor - 1) >> 6; line++)
            txln_put(line);
    return block;
}

// run_block / block_return: the dispatcher's boundary; interp_dispatch.h defines G_OWN_TRAMPOLINES so
// core/dispatch.c calls these instead of emitting its AArch64 pair. `static` is load-bearing:
// visibility("hidden") leaves the symbol STB_GLOBAL in a static link, the
// dual archive links BOTH target objects, and namespace.h does not rename these two.
static void run_block(struct cpu *cpu, void *code);
static void block_return(void);

static void run_block(struct cpu *cpu, void *code) {
    const struct interp_block *block = (const struct interp_block *)code;
    if (block == NULL || block->magic != INTERP_BLOCK_MAGIC) {
        // Not this backend's descriptor: a JIT-written pcache/checkpoint that host-ISA identity
        // (pcache_engine_id) should have rejected.
        static const char message[] = "interpreter entered a block that it did not translate";
        (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
        cpu->reason = R_BRANCH;
        return;
    }

    // savemask=0 -- this is the hottest line in the engine (once per guest block) and savemask=1 makes glibc
    // issue a real rt_sigprocmask here. interp_restore_handler_mask does the restore on the fault path
    // instead, where it is paid once per fault rather than once per block. sigsetjmp/siglongjmp and NOT
    // setjmp/longjmp: on Darwin setjmp/longjmp are the mask-SAVING pair, so sigsetjmp(.,0) is the only
    // portable way to say "this pad does not touch the mask" (same idiom as linux_abi/thread.c's probe pad).
    if (sigsetjmp(g_interp_marker_jmp, 0) != 0) {
        // Both abandon paths already left cpu as the dispatcher needs it; no architectural state changed.
        g_interp_access_active = 0;
        g_interp_marker_armed = 0;
        g_interp_marker_cpu = NULL;
        return;
    }
    g_interp_marker_cpu = cpu;
    g_interp_marker_armed = 1;

    uint64_t executed = 0;
    for (;;) {
        // Poll AFTER one instruction retires: exiting with cpu->pc unchanged gets the same block forever.
        if (executed && cpu->irq) {
            cpu->reason = R_BRANCH;
            break;
        }
        // Ordinary chain exit.
        if (cpu->pc < block->guest_start || cpu->pc >= block->guest_end) {
            cpu->reason = R_BRANCH;
            break;
        }
        if (interp_step(cpu) == INTERP_END) break;
        executed++;
    }

    g_interp_marker_armed = 0;
    g_interp_marker_cpu = NULL;
}

// Nothing here is executable so nothing branches in, but the symbol must exist and be address-taken:
// sigframe_resume_dispatch bakes it. Abort, not return -- a silent return spins the dispatcher on a stale
// cpu->reason.
static void block_return(void) {
    static const char message[] = "interpreter received an invalid generated-code return";
    (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
    abort();
}

// Self-modifying guest code, same model as the JIT: the reason codes and queue are checkpoint-image state.
static void smc_queue_line(struct cpu *c, uint64_t address) {
    // ET_EXEC code sits at a collision-avoidance bias while its pointers stay link-time low; map source
    // intervals use the real executable address, so normalize as dispatch does.
    if (g_nonpie_lo && address >= g_nonpie_lo && address < g_nonpie_hi) address += g_nonpie_bias;
    uint64_t start = address & ~UINT64_C(63), end = start + 64;
    for (uint32_t i = 0; i < c->smc_range_count; i++) {
        if (end < c->smc_ranges[i][0] || start > c->smc_ranges[i][1]) continue;
        if (start < c->smc_ranges[i][0]) c->smc_ranges[i][0] = start;
        if (end > c->smc_ranges[i][1]) c->smc_ranges[i][1] = end;
        return;
    }
    if (c->smc_range_count == SMC_RANGE_CAP) {
        c->smc_range_overflow = 1;
        return;
    }
    c->smc_ranges[c->smc_range_count][0] = start;
    c->smc_ranges[c->smc_range_count][1] = end;
    c->smc_range_count++;
}

static void aarch64_smc_queue_range(uint64_t first, uint64_t last, void *opaque) {
    struct cpu *c = opaque;
    for (uint64_t line = first & ~UINT64_C(63); line < last;) {
        smc_queue_line(c, line);
        if (line > UINT64_MAX - 64) break;
        line += 64;
    }
}

// G_SMC_COPYOUT. NOT inert here: a syscall copying to user memory can move the branch that determined a
// cached block extent.
static void aarch64_smc_copyout(uint64_t first, uint64_t last) {
    if (last <= first) return;
    struct cpu *c = pthread_getspecific(g_cpu_key);
    if (c == NULL) return;
    aarch64_smc_queue_range(first, last, c);
    hl_logical_vma_visit_exec_aliases(first, last, aarch64_smc_queue_range, c);
}

// R_ICFLUSH: queue only; smc_commit() classifies under g_jit_lock so a changed line is classified once.
static void smc_icflush(struct cpu *c, uint64_t va) {
    // Latch even for a never-translated line: g_smc_seen means "this guest generates code" engine-wide.
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    smc_queue_line(c, va);
}

// R_ICCOMMIT: what must be dropped is not host code (there is none) but the gpc->descriptor lookup.
static int smc_commit(struct cpu *c) {
    pthread_mutex_lock(&g_jit_lock);
    txln_activate();                // arm eager line recording; may request a priming wholesale drop
    int force_whole = g_txln_prime; // first SMC after activation: no lines recorded -> cannot classify
    g_txln_prime = 0;
    if (!force_whole && !c->smc_range_count && !c->smc_range_overflow) {
        pthread_mutex_unlock(&g_jit_lock);
        return 1;
    }
    stw_mapping_begin_locked();
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    if (!c->smc_range_overflow && !force_whole) {
        uint32_t retained = 0;
        for (uint32_t i = 0; i < c->smc_range_count; i++) {
            uint64_t dirty_start = UINT64_MAX, dirty_end = 0;
            for (uint64_t line = c->smc_ranges[i][0]; line < c->smc_ranges[i][1]; line += 64) {
                // class 0 = never translated (nothing stale), 1 = first flush or bytes changed (drop),
                // 2 = translated but unchanged, i.e. benign icache maintenance (skip).
                if (txln_flush_class(line) == 1) {
                    if (dirty_start == UINT64_MAX) dirty_start = line;
                    dirty_end = line + 64;
                }
            }
            if (dirty_start != UINT64_MAX) {
                c->smc_ranges[retained][0] = dirty_start;
                c->smc_ranges[retained][1] = dirty_end;
                retained++;
            }
        }
        c->smc_range_count = retained;
        if (!retained) {
            c->smc_range_overflow = 0;
            stw_mapping_end();
            return 1;
        }
    }
    // Map readers are lock-free: a peer must not still be in run_block against a range about to go stale.
    uint32_t removed;
    if (force_whole || c->smc_range_overflow) {
        removed = g_live_map_count;
        map_clear();
        // Inert here (G_IBTC_FILL is a no-op); kept so a checkpoint written here can be restored by the JIT.
        memset(g_ibtc, 0, sizeof g_ibtc);
        txpg_clear();
    } else {
        removed = map_invalidate_source_ranges((const uint64_t (*)[2])c->smc_ranges, c->smc_range_count);
    }
    pend_reset();
    HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT, "smc invalidate backend=interp mode=%s ranges=%u removed=%u retained=%u",
            (force_whole || c->smc_range_overflow) ? "whole" : "targeted", c->smc_range_count, removed,
            g_live_map_count);
    stw_mapping_end();
    c->smc_range_count = 0;
    c->smc_range_overflow = 0;
    return 1;
}

// Contract stubs, each inert for a stated reason -- except this one, which publishes the logical-VMA hull.
static void aarch64_soft_filter_refresh(struct cpu *c) {
    uint64_t first = UINT64_MAX, last = 0;
    hl_logical_vma_snapshot *snapshot =
        atomic_load_explicit(hl_logical_vma_global_snapshot_source(), memory_order_acquire);
    if (snapshot != NULL && snapshot->count != 0) {
        first = snapshot->views[0].guest_first;
        last = snapshot->views[snapshot->count - 1].guest_last;
    }
    c->soft_filter_first = first;
    c->soft_filter_last = last;
}

// Never raised here (accesses resolve inline), but the codes are shared vocabulary: G_DISPATCH_REASON turns
// R_SOFTMISS/R_SOFTSPAN/R_SOFTCOMMIT into a guest fetch fault so a JIT-written checkpoint cannot mis-resume.
static int aarch64_soft_tlb_miss(struct cpu *c) {
    (void)c;
    return 0;
}

static int aarch64_soft_tlb_span(struct cpu *c) {
    (void)c;
    return 0;
}

static int aarch64_soft_bounce_commit(struct cpu *c) {
    // No bounce can be pending; 1 = "committed" is the JIT's answer for an unarmed bounce.
    (void)c;
    return 1;
}

// Never consulted (G_BLOCK_ALIGN is literal 0); 0 spells "§B on", the ordinary non-tuning path.
static int shadowgate(void) {
    return 0;
}
