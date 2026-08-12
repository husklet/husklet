#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_TRACE_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_TRACE_H

#include <stdint.h>

#include "../decoder.h"

enum {
    HL_X86_TRACE_MAX_BLOCKS = 16,
    HL_X86_TRACE_MAX_BYTES = 16 * 1024,
    HL_X86_FLAG_CF = 0x01,
    HL_X86_FLAG_PF = 0x02,
    HL_X86_FLAG_AF = 0x04,
    HL_X86_FLAG_ZF = 0x08,
    HL_X86_FLAG_SF = 0x10,
    HL_X86_FLAG_OF = 0x20,
    HL_X86_FLAG_ALL = 0x3f,
    HL_X86_FLAG_NZCV = HL_X86_FLAG_CF | HL_X86_FLAG_ZF | HL_X86_FLAG_SF | HL_X86_FLAG_OF,
    HL_X86_PENDING_NONE = 0,
    HL_X86_PENDING_SUB = 1,
    HL_X86_PENDING_ADD = 2,
    HL_X86_PENDING_LOGIC = 3,
    // Per-edge spill kind reported by hl_x86_trace_jcc_flags via *save_taken/*save_fall: which finalizer
    // the caller must emit inside the (flag-live) edge stub. NONE = elide (successor overwrites first).
    HL_X86_JCC_SPILL_NONE = 0,
    HL_X86_JCC_SPILL_SUB = 1,   // e_nzcv_save   (SUBS: live NZCV already borrow-canonical)
    HL_X86_JCC_SPILL_LOGIC = 2, // e_nzcv_save_c1 (ANDS: canonicalize x86 CF=0/OF=0 before the store)
};

typedef struct hl_x86_trace_state {
    int *pending_flags;
    uint64_t *tier_counters;
    uint64_t *flag_elisions;
    uint64_t *flag_scans;
    uint64_t *tier_folds;
    void (*materialize_flags)(void);
    void (*fix_add_flags)(void);
    void (*fix_logic_flags)(void);
    void (*emit_chain_exit)(uint64_t target);
    int (*page_translated)(uint64_t address);
    int flag_elision;
    int tier_two;
} hl_x86_trace_state;

int hl_x86_trace_seen(const uint64_t *seen, int count, uint64_t value);
int hl_x86_trace_trap_head(uint64_t address);
int hl_x86_trace_loop_hazard(uint64_t body, uint64_t end);
int hl_x86_trace_flags_livein(hl_x86_trace_state *state, uint64_t pc, uint64_t anchor);
int hl_x86_trace_pfaf_dead(hl_x86_trace_state *state, const struct insn *insn, uint64_t pc, uint64_t anchor);
void hl_x86_trace_flags_edge(hl_x86_trace_state *state, uint64_t target, uint64_t anchor);
void hl_x86_trace_jcc_flags(hl_x86_trace_state *state, uint64_t taken, uint64_t fall, uint64_t anchor, int stitch_fall,
                            int arm_cc, int *save_taken, int *save_fall);
void hl_x86_trace_self_loop(hl_x86_trace_state *state, int condition, uint64_t start, uint64_t fall, void *body,
                            int slot);

#endif
