#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../word.h"
#include "cpu.h"

void hl_x86_spill(uint32_t *words, uint32_t *cursor, uint32_t dirty) {
  unsigned index;

  for (index = 0; index < 16; ++index) {
    if ((dirty & (UINT32_C(1) << index)) != 0)
      words[(*cursor)++] =
          store_word(index, offsetof(hl_native_x86_64_cpu, registers) +
                                index * sizeof(uint64_t));
  }
}

void hl_x86_spill_all(uint32_t *words, uint32_t *cursor) {
  hl_x86_spill(words, cursor, UINT32_C(0xffff));
}

void hl_x86_checkpoint(uint32_t *words, uint32_t *cursor, uint32_t delta,
                       int live_chain) {
  if (live_chain) {
    /* A decoded block contains at most 64 instructions, so its checkpoint
     * delta always fits AArch64's immediate form.  Keep the generic
     * register form as a fail-closed encoding path if that bound grows. */
    if (delta < UINT32_C(4096)) {
      words[(*cursor)++] =
          UINT32_C(0xd100035a) | delta << 10; /* sub x26,x26,#delta */
    } else {
      emit_constant(words, cursor, 17, delta);
      words[(*cursor)++] = UINT32_C(0xcb11035a); /* sub x26,x26,x17 */
    }
    return;
  }
  words[(*cursor)++] = load_word(16, offsetof(hl_native_x86_64_cpu, scratch));
  if (delta < UINT32_C(4096)) {
    words[(*cursor)++] =
        UINT32_C(0x91000210) | delta << 10; /* add x16,x16,#delta */
  } else {
    emit_constant(words, cursor, 17, delta);
    words[(*cursor)++] = UINT32_C(0x8b110210); /* add x16,x16,x17 */
  }
  words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, scratch));
}

void hl_x86_finish_chain(uint32_t *words, uint32_t *cursor) {
  words[(*cursor)++] = load_word(16, offsetof(hl_native_x86_64_cpu, budget));
  words[(*cursor)++] = UINT32_C(0xcb1a0210); /* sub x16,x16,x26 */
  words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, scratch));
}

void hl_x86_emit_exit(const hl_x86_a64_request *request, const decode *block,
                      uint32_t *cursor) {
  if (block->exit == HL_X86_A64_DYNAMIC_BRANCH ||
      block->exit == HL_X86_A64_DIRECT_CALL)
    return;
  if (block->exit == HL_X86_A64_CONDITIONAL_BRANCH) {
    const instruction *control = &block->instructions[block->count - 1u];
    int self_loop = (request->flags & HL_X86_A64_CONDITIONAL_SELF_LOOP) != 0u &&
                    block->target == request->guest_pc;

    if (control->condition == 0xau || control->condition == 0xbu) {
      request->host_words[(*cursor)++] =
          load_word(20, offsetof(hl_native_x86_64_cpu, flags));
      request->host_words[(*cursor)++] = hl_x86_bit(20, 20, 2);
      request->host_words[(*cursor)++] = UINT32_C(0xf100001f) | 20u << 5;
    } else {
      hl_x86_emit_rflags(request->host_words, cursor);
    }
    if (self_loop) {
      uint32_t condition = control->condition == 0xau ? 1u
                           : control->condition == 0xbu
                               ? 0u
                               : arm_condition(control->condition);
      uint32_t taken_branch = (*cursor)++;
      emit_constant(request->host_words, cursor, 16, block->next_pc);
      request->host_words[(*cursor)++] =
          store_word(16, offsetof(hl_native_x86_64_cpu, program));
      request->host_words[(*cursor)++] =
          load_word(17, offsetof(hl_native_x86_64_cpu, loop_completed));
      request->host_words[(*cursor)++] = UINT32_C(0x91000400) | 17u << 5 | 17u;
      request->host_words[(*cursor)++] =
          store_word(17, offsetof(hl_native_x86_64_cpu, loop_completed));
      if ((request->flags & HL_X86_A64_LIVE_CHAIN) != 0u) {
        hl_x86_finish_chain(request->host_words, cursor);
        hl_x86_spill_all(request->host_words, cursor);
      }
      request->host_words[(*cursor)++] = UINT32_C(0xd65f03c0);
      uint32_t taken = *cursor;
      request->host_words[taken_branch] =
          UINT32_C(0x54000000) |
          ((taken - taken_branch) & UINT32_C(0x7ffff)) << 5 | condition;
      request->host_words[(*cursor)++] =
          load_word(17, offsetof(hl_native_x86_64_cpu, loop_completed));
      request->host_words[(*cursor)++] = UINT32_C(0x91000400) | 17u << 5 | 17u;
      request->host_words[(*cursor)++] =
          store_word(17, offsetof(hl_native_x86_64_cpu, loop_completed));
      request->host_words[(*cursor)++] =
          load_word(17, offsetof(hl_native_x86_64_cpu, loop_remaining));
      request->host_words[(*cursor)++] = UINT32_C(0xd1000400) | 17u << 5 | 17u;
      request->host_words[(*cursor)++] =
          store_word(17, offsetof(hl_native_x86_64_cpu, loop_remaining));
      uint32_t zero_branch = (*cursor)++;
      request->host_words[(*cursor)++] =
          load_word(16, offsetof(hl_native_x86_64_cpu, executable_written));
      uint32_t executable_branch = (*cursor)++;
      request->host_words[(*cursor)++] =
          load_word(16, offsetof(hl_native_x86_64_cpu, interrupt));
      uint32_t irq_branch = (*cursor)++;
      int64_t body_delta = -(int64_t)(*cursor);
      request->host_words[(*cursor)++] =
          UINT32_C(0x14000000) | ((uint32_t)body_delta & UINT32_C(0x03ffffff));
      uint32_t done = *cursor;
      request->host_words[zero_branch] =
          UINT32_C(0xb4000000) |
          ((done - zero_branch) & UINT32_C(0x7ffff)) << 5 | 17u;
      request->host_words[irq_branch] =
          UINT32_C(0xb5000000) |
          ((done - irq_branch) & UINT32_C(0x7ffff)) << 5 | 16u;
      request->host_words[executable_branch] =
          UINT32_C(0x37000000) |
          ((done - executable_branch) & UINT32_C(0x3fff)) << 5 | 2u << 19 | 16u;
      if ((request->flags & HL_X86_A64_LIVE_CHAIN) != 0u) {
        hl_x86_finish_chain(request->host_words, cursor);
        hl_x86_spill_all(request->host_words, cursor);
      }
      request->host_words[(*cursor)++] = UINT32_C(0xd65f03c0);
      return;
    }
    emit_constant(request->host_words, cursor, 16, block->target);
    emit_constant(request->host_words, cursor, 17, block->next_pc);
    request->host_words[(*cursor)++] =
        UINT32_C(0x9a800000) | 17u << 16 |
        (control->condition == 0xau   ? 1u
         : control->condition == 0xbu ? 0u
                                      : arm_condition(control->condition))
            << 12 |
        16u << 5 | 16u;
    if (control->condition != 0xau && control->condition != 0xbu)
      hl_x86_emit_nzcv(request->host_words, cursor);
  } else {
    emit_constant(request->host_words, cursor, 16,
                  block->exit == HL_X86_A64_DIRECT_BRANCH ? block->target
                                                          : block->next_pc);
  }
  request->host_words[(*cursor)++] =
      store_word(16, offsetof(hl_native_x86_64_cpu, program));
}
