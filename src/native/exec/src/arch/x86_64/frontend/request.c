#include "private.h"

#include <string.h>

#include "../decode.h"

int hl_x86_request_valid(const hl_x86_a64_request *request, const hl_x86_a64_result *result) {
    return request != NULL && result != NULL && request->abi == HL_X86_A64_FRONTEND_ABI &&
           request->size == sizeof *request && request->guest_bytes != NULL && request->host_words != NULL &&
           request->provenance != NULL && request->guest_size != 0 && request->max_instructions != 0 &&
           request->max_instructions <= HL_X86_A64_MAX_INSTRUCTIONS &&
           (request->flags & ~(HL_X86_A64_CHECKPOINTS | HL_X86_A64_CONDITIONAL_SELF_LOOP |
                               HL_X86_A64_LIVE_CHAIN | HL_X86_A64_LSE |
                               HL_X86_A64_DIAGNOSTICS)) == 0u &&
           ((request->flags & HL_X86_A64_LIVE_CHAIN) == 0u ||
            (request->flags & HL_X86_A64_CHECKPOINTS) != 0u) &&
           request->reserved == 0u;
}

hl_x86_a64_status hl_x86_result(const hl_x86_a64_request *request, const decode *block,
                                hl_x86_a64_result *result, uint32_t words) {
    memset(result, 0, sizeof *result);
    result->abi = HL_X86_A64_FRONTEND_ABI;
    result->size = sizeof *result;
    result->source_start = request->guest_pc;
    result->source_end = block->next_pc;
    result->exit_pc = block->next_pc;
    result->branch_target = block->target;
    result->instruction_count = block->count;
    result->word_count = words;
    result->provenance_count = block->count;
    result->exit = block->exit;
    return block->status;
}
