#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_BRANCH_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_BRANCH_H

#include <stdint.h>

#include "trace.h"

typedef struct {
    uint64_t start;
    void *body;
    uint64_t *seen;
    int *seen_count;
    int *trace_blocks;
    int *conditional_stitches;
    int stitch_allowed;
    int tier_two;
    int (*tier_disabled)(void);
    int (*tier_slot)(uint64_t);
    void *(*body_mapped)(uint64_t);
} hl_x86_branch_region;

int hl_x86_lower_near_branch(struct insn *, uint64_t *, uint64_t, hl_x86_trace_state *, hl_x86_branch_region *);
int hl_x86_lower_short_branch(struct insn *, uint64_t *, uint64_t, hl_x86_trace_state *, hl_x86_branch_region *);
int hl_x86_lower_direct_jump(struct insn *, uint64_t *, uint64_t, hl_x86_trace_state *, hl_x86_branch_region *);
int hl_x86_lower_conditional_move(struct insn *, uint64_t, uint64_t, int);

#endif
