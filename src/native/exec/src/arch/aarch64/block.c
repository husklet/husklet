#include "block.h"

#include "add.h"
#include "arithmetic.h"
#include "bitwise.h"
#include "broadcast.h"
#include "compare.h"
#include "conditional.h"
#include "direct.h"
#include "divide.h"
#include "field.h"
#include "fp_move.h"
#include "floating.h"
#include "indirect.h"
#include "logical.h"
#include "move.h"
#include "multiply.h"
#include "pair.h"
#include "pcrel.h"
#include "reverse.h"
#include "select.h"
#include "shift.h"
#include "simd_compare.h"
#include "simd_narrow.h"
#include "single.h"
#include "structure.h"
#include "system.h"
#include "terminator.h"

#include <string.h>

static int emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    return hl_a64_move_emit(assembler, word, pc) || hl_a64_add_emit(assembler, word, pc) ||
           hl_a64_logical_emit(assembler, word, pc) || hl_a64_pcrel_emit(assembler, word, pc) ||
           hl_a64_arithmetic_emit(assembler, word, pc) || hl_a64_bitwise_emit(assembler, word, pc) ||
           hl_a64_broadcast_emit(assembler, word, pc) ||
           hl_a64_fp_move_emit(assembler, word, pc) ||
           hl_a64_floating_emit(assembler, word, pc) ||
           hl_a64_field_emit(assembler, word, pc) || hl_a64_multiply_emit(assembler, word, pc) ||
           hl_a64_divide_emit(assembler, word, pc) || hl_a64_shift_emit(assembler, word, pc) ||
           hl_a64_reverse_emit(assembler, word, pc) ||
           hl_a64_simd_compare_emit(assembler, word, pc) ||
           hl_a64_simd_narrow_emit(assembler, word, pc) ||
           hl_a64_select_emit(assembler, word, pc) || hl_a64_compare_emit(assembler, word, pc) ||
           hl_a64_single_emit(assembler, word, pc) || hl_a64_pair_emit(assembler, word, pc) ||
           hl_a64_structure_emit(assembler, word, pc) || hl_a64_direct_emit(assembler, word, pc) ||
           hl_a64_system_emit(assembler, word, pc) ||
           hl_a64_indirect_emit(assembler, word, pc) || hl_a64_conditional_emit(assembler, word, pc) ||
           hl_a64_terminator_emit(assembler, word, pc);
}

int hl_a64_block_build(const hl_a64_source *source, uint64_t pc, void *buffer, size_t capacity,
                       hl_a64_block_result *output) {
    hl_a64_fetch_result fetched;
    hl_a64_assembler assembler;
    if (output == NULL) return 0;
    memset(output, 0, sizeof(*output));
    output->state = HL_A64_BLOCK_FETCH;
    if (buffer == NULL || capacity < HL_A64_BLOCK_MAX_BYTES ||
        !hl_a64_source_fetch(source, pc, 1, &fetched))
        return 0;
    output->source_first = pc;
    output->source_last = pc + 4;
    if (!hl_a64_assembler_begin(&assembler, buffer, buffer, capacity)) return 0;
    if (!emit(&assembler, fetched.words[0], pc)) {
        memset(buffer, 0, capacity);
        output->state = HL_A64_BLOCK_FALLBACK;
        return 1;
    }
    output->state = HL_A64_BLOCK_BUILT;
    output->code_size = hl_a64_assembler_size(&assembler);
    output->provenance = (hl_native_provenance){.code_size = output->code_size, .guest = pc};
    return 1;
}

static hl_native_status block_cache(hl_native_executor *executor, hl_native_lookup_context *context,
                                    const hl_a64_source *source, uint64_t pc,
                                    void *buffer, size_t capacity, hl_native_code *code,
                                    hl_a64_block_state *state) {
    hl_native_translation_key key;
    hl_a64_block_result block;
    hl_native_emission emission;
    hl_native_lookup lookup;
    hl_native_status status;
    if (executor == NULL || source == NULL || code == NULL || state == NULL ||
        !hl_a64_source_validate(source) || pc > UINT64_MAX - 4)
        return HL_NATIVE_ARGUMENT;
    key = (hl_native_translation_key){
        .guest = pc,
        .mapping_incarnation = source->mapping_incarnation,
        .instruction_epoch = source->instruction_epoch,
        .source_first = pc,
        .source_last = pc + 4,
        .memory_mode = executor->memory_mode,
        .authority_generation = executor->authority_generation,
        .architecture = HL_NATIVE_AARCH64,
        .direct_token = executor->memory_mode == 0 ? 0 : (uint64_t)(uintptr_t)executor->direct_authority,
        .direct_generation = executor->memory_mode == 0 ? 0 : executor->direct_generation,
    };
    lookup = context == NULL ? hl_native_translation_lookup(executor, &key, code)
                             : hl_native_translation_lookup_inner(executor, context, &key, code);
    if (lookup == HL_NATIVE_HIT) {
        *state = HL_A64_BLOCK_HIT;
        return HL_NATIVE_OK;
    }
    if (lookup == HL_NATIVE_EPOCH) return HL_NATIVE_STATE;
    if (!hl_a64_block_build(source, pc, buffer, capacity, &block)) {
        *state = block.state;
        return HL_NATIVE_ARGUMENT;
    }
    *state = block.state;
    if (block.state != HL_A64_BLOCK_BUILT) return HL_NATIVE_OK;
    emission = (hl_native_emission){.bytes = buffer, .size = block.code_size,
                                    .body_offset = 0, .provenance = &block.provenance,
                                    .provenance_count = 1};
    status = hl_native_translation_publish(executor, &key, &emission);
    if (status != HL_NATIVE_OK) return status;
    lookup = context == NULL ? hl_native_translation_lookup(executor, &key, code)
                             : hl_native_translation_lookup_inner(executor, context, &key, code);
    return lookup == HL_NATIVE_HIT
        ? HL_NATIVE_OK : HL_NATIVE_STATE;
}

hl_native_status hl_a64_block_cache_inner(hl_native_executor *executor, hl_native_lookup_context *context,
                                          const hl_a64_source *source, uint64_t pc,
                                          void *buffer, size_t capacity, hl_native_code *code,
                                          hl_a64_block_state *state) {
    if (context == NULL) return HL_NATIVE_ARGUMENT;
    return block_cache(executor, context, source, pc, buffer, capacity, code, state);
}

hl_native_status hl_a64_block_cache(hl_native_executor *executor, const hl_a64_source *source, uint64_t pc,
                                    void *buffer, size_t capacity, hl_native_code *code,
                                    hl_a64_block_state *state) {
    return block_cache(executor, NULL, source, pc, buffer, capacity, code, state);
}
