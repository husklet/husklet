#include "frontend.h"

#include <string.h>

enum {
  A64_WORD_BYTES = 4,
  A64_MOVCONST_WORDS = 4,
};

/* x16/x17 are dispatcher scratch, x18 is host-reserved on Darwin, x28 holds
 * the native CPU record, and x30 is the native block-return link. The retained
 * frontend rewrites these operands through CPU-side storage. This first slice
 * refuses them until that generated-layout spill path is imported. */
static int reserved_register(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int stack_operand(uint32_t word, unsigned fields) {
  if ((word & 0x1f000000u) != 0x11000000u &&
      (word & 0x1f200000u) != 0x0b200000u)
    return 0;
  if ((fields & 1u) != 0 && (word & 31u) == 31u)
    return 1;
  if ((fields & 2u) != 0 && ((word >> 5) & 31u) == 31u)
    return 1;
  return (fields & 4u) != 0 && ((word >> 16) & 31u) == 31u;
}

static int field_reserved(uint32_t word, unsigned fields) {
  if ((fields & 1u) != 0 && reserved_register(word & 31u))
    return 1;
  if ((fields & 2u) != 0 && reserved_register((word >> 5) & 31u))
    return 1;
  if ((fields & 4u) != 0 && reserved_register((word >> 16) & 31u))
    return 1;
  return (fields & 8u) != 0 && reserved_register((word >> 10) & 31u);
}

static unsigned immediate_fields(uint32_t word) {
  if ((word & 0x1f000000u) == 0x10000000u)
    return 0; /* ADR/ADRP need guest-PC relocation. */
  if ((word & 0x1f800000u) == 0x12800000u)
    return 1; /* move wide */
  if ((word & 0x1f800000u) == 0x13800000u)
    return 1 | 2 | 4; /* EXTR */
  return 1 | 2;
}

static unsigned register_fields(uint32_t word) {
  if ((word & 0x1f000000u) == 0x1b000000u)
    return 1 | 2 | 4 | 8; /* three source */
  if ((word & 0x5fe00000u) == 0x5ac00000u)
    return 1 | 2; /* one source */
  if ((word & 0x1fe00000u) == 0x1a400000u)
    return (word & 0x800u) != 0 ? 2 : 2 | 4; /* conditional compare */
  return 1 | 2 | 4;
}

static int integer_immediate(uint32_t word, unsigned *fields) {
  if ((word & 0x1f000000u) != 0x11000000u &&
      (word & 0x1f800000u) != 0x12000000u &&
      (word & 0x1f800000u) != 0x12800000u &&
      (word & 0x1f800000u) != 0x13000000u &&
      (word & 0x7fa00000u) != 0x13800000u)
    return 0;
  *fields = immediate_fields(word);
  return 1;
}

static int pcrel_integer(uint32_t word) {
  return (word & 0x1f000000u) == 0x10000000u;
}

static int immediate_branch(uint32_t word) {
  return (word & 0x7c000000u) == 0x14000000u;
}

static int compare_branch(uint32_t word) {
  return (word & 0x7e000000u) == 0x34000000u;
}

static int test_branch(uint32_t word) {
  return (word & 0x7e000000u) == 0x36000000u;
}

static int condition_branch(uint32_t word) {
  return (word & 0xff000010u) == 0x54000000u;
}

static hl_a64_control register_control(uint32_t word) {
  if ((word & 0xfffffc1fu) == 0xd61f0000u)
    return HL_A64_INDIRECT_BRANCH;
  if ((word & 0xfffffc1fu) == 0xd63f0000u)
    return HL_A64_INDIRECT_CALL;
  if ((word & 0xfffffc1fu) == 0xd65f0000u)
    return HL_A64_RETURN;
  return HL_A64_CONTROL_NONE;
}

static uint64_t sign_extend(uint64_t value, unsigned bits) {
  uint64_t sign = UINT64_C(1) << (bits - 1);
  return (value ^ sign) - sign;
}

static uint64_t branch_target(uint32_t word, uint64_t guest_pc) {
  uint64_t displacement = sign_extend(word & 0x03ffffffu, 26) << 2;
  return guest_pc + displacement;
}

static uint64_t predicate_target(uint32_t word, uint64_t guest_pc,
                                 unsigned bits) {
  uint32_t mask = (UINT32_C(1) << bits) - 1u;
  uint64_t displacement = sign_extend((word >> 5) & mask, bits) << 2;
  return guest_pc + displacement;
}

static uint64_t pcrel_value(uint32_t word, uint64_t guest_pc) {
  uint64_t immediate =
      ((uint64_t)((word >> 5) & 0x7ffffu) << 2) | ((word >> 29) & 3u);
  uint64_t displacement = sign_extend(immediate, 21);
  if ((word & 0x80000000u) != 0)
    return (guest_pc & ~UINT64_C(0xfff)) + (displacement << 12);
  return guest_pc + displacement;
}

static int integer_register(uint32_t word, unsigned *fields) {
  if ((word & 0x1f200000u) != 0x0b000000u &&
      (word & 0x1f200000u) != 0x0b200000u &&
      (word & 0x1fe0fc00u) != 0x1a000000u &&
      (word & 0x1f000000u) != 0x0a000000u &&
      (word & 0x1f000000u) != 0x1b000000u &&
      (word & 0x7ffffc00u) != 0x5ac00000u &&
      !((word & 0x7fff0000u) == 0x5ac00000u && ((word >> 10) & 0x3fu) >= 1u &&
        ((word >> 10) & 0x3fu) <= 3u) &&
      (word & 0x7ffffc00u) != 0x5ac01000u &&
      (word & 0x1fe0f000u) != 0x1ac02000u &&
      (word & 0x1fe0f800u) != 0x1ac00800u &&
      (word & 0x1fe00000u) != 0x1a400000u &&
      (word & 0x3fe00800u) != 0x1a800000u)
    return 0;
  *fields = register_fields(word);
  return 1;
}

static hl_a64_terminal classify(uint32_t word) {
  if ((word & 0xffe0001fu) == 0xd4000001u)
    return HL_A64_SYSCALL;
  return HL_A64_UNSUPPORTED;
}

static void emit_word(uint8_t *output, uint32_t word) {
  output[0] = (uint8_t)word;
  output[1] = (uint8_t)(word >> 8);
  output[2] = (uint8_t)(word >> 16);
  output[3] = (uint8_t)(word >> 24);
}

static void emit_movconst(uint8_t *output, unsigned destination,
                          uint64_t value) {
  unsigned half;
  emit_word(output, UINT32_C(0xd2800000) | (uint32_t)((value & 0xffffu) << 5) |
                        destination);
  for (half = 1; half < A64_MOVCONST_WORDS; half++) {
    uint32_t chunk = (uint32_t)((value >> (half * 16)) & 0xffffu);
    emit_word(output + half * A64_WORD_BYTES,
              UINT32_C(0xf2800000) | (half << 21) | (chunk << 5) | destination);
  }
}

int hl_a64_emit_block(const hl_a64_block_input *input,
                      hl_a64_block_output *output) {
  size_t index;
  size_t required_code = 0;
  if (output == NULL)
    return 0;
  memset(output, 0, sizeof(*output));
  output->terminal = HL_A64_UNSUPPORTED;
  if (input == NULL || input->instructions == NULL || input->code == NULL ||
      input->provenance == NULL || input->instruction_count == 0 ||
      input->instruction_count > HL_A64_BLOCK_MAX_INSTRUCTIONS ||
      input->instruction_count >
          SIZE_MAX / (A64_WORD_BYTES * A64_MOVCONST_WORDS) ||
      input->provenance_capacity < input->instruction_count ||
      input->instructions[0].guest_pc >
          UINT64_MAX - (input->instruction_count - 1) * A64_WORD_BYTES)
    return 0;
  for (index = 0; index < input->instruction_count; index++) {
    uint64_t expected =
        input->instructions[0].guest_pc + index * A64_WORD_BYTES;
    if (input->instructions[index].reserved != 0 ||
        input->instructions[index].guest_pc != expected ||
        (input->instructions[index].guest_pc & (A64_WORD_BYTES - 1)) != 0)
      return 0;
  }

  for (index = 0; index < input->instruction_count; index++) {
    uint32_t word = input->instructions[index].word;
    unsigned fields = 0;
    size_t words;
    if (pcrel_integer(word)) {
      fields = 1;
      words = A64_MOVCONST_WORDS;
    } else if (integer_immediate(word, &fields) ||
               integer_register(word, &fields)) {
      words = 1;
    } else {
      break;
    }
    if (field_reserved(word, fields) || stack_operand(word, fields))
      break;
    required_code += words * A64_WORD_BYTES;
  }
  if (input->code_capacity < required_code ||
      input->host_base > UINT64_MAX - required_code)
    return 0;

  for (index = 0; index < input->instruction_count; index++) {
    const hl_a64_instruction *instruction = &input->instructions[index];
    unsigned fields = 0;
    int pcrel = pcrel_integer(instruction->word);
    int copy = pcrel || integer_immediate(instruction->word, &fields) ||
               integer_register(instruction->word, &fields);
    size_t emitted =
        pcrel ? A64_WORD_BYTES * A64_MOVCONST_WORDS : A64_WORD_BYTES;
    if (pcrel)
      fields = 1;
    if (!copy || field_reserved(instruction->word, fields) ||
        stack_operand(instruction->word, fields)) {
      unsigned tested = instruction->word & 31u;
      hl_a64_control indirect = register_control(instruction->word);
      if (immediate_branch(instruction->word)) {
        output->terminal = HL_A64_CONTROL;
        output->control_target =
            branch_target(instruction->word, instruction->guest_pc);
        if ((instruction->word & 0x80000000u) != 0) {
          output->control = HL_A64_DIRECT_CALL;
          output->control_link = instruction->guest_pc + A64_WORD_BYTES;
        } else {
          output->control = HL_A64_DIRECT_BRANCH;
        }
      } else if (condition_branch(instruction->word)) {
        output->terminal = HL_A64_CONTROL;
        output->control = HL_A64_CONDITION_BRANCH;
        output->control_target =
            predicate_target(instruction->word, instruction->guest_pc, 19u);
        output->control_fallthrough = instruction->guest_pc + A64_WORD_BYTES;
        output->control_condition = (uint8_t)(instruction->word & 15u);
      } else if ((compare_branch(instruction->word) ||
                  test_branch(instruction->word)) &&
                 !reserved_register(tested)) {
        int compare = compare_branch(instruction->word);
        output->terminal = HL_A64_CONTROL;
        output->control = compare ? HL_A64_COMPARE_BRANCH : HL_A64_TEST_BRANCH;
        output->control_target = predicate_target(
            instruction->word, instruction->guest_pc, compare ? 19u : 14u);
        output->control_fallthrough = instruction->guest_pc + A64_WORD_BYTES;
        output->control_register = (uint8_t)tested;
        output->control_nonzero = (uint8_t)((instruction->word >> 24) & 1u);
        if (compare) {
          output->control_wide = (uint8_t)(instruction->word >> 31);
        } else {
          output->control_bit = (uint8_t)(((instruction->word >> 31) << 5) |
                                          ((instruction->word >> 19) & 31u));
          output->control_wide = (uint8_t)(output->control_bit >= 32u);
        }
      } else if (indirect != HL_A64_CONTROL_NONE &&
                 !reserved_register((instruction->word >> 5) & 31u)) {
        output->terminal = HL_A64_CONTROL;
        output->control = indirect;
        output->control_register = (uint8_t)((instruction->word >> 5) & 31u);
        if (indirect == HL_A64_INDIRECT_CALL)
          output->control_link = instruction->guest_pc + A64_WORD_BYTES;
      } else {
        output->terminal = classify(instruction->word);
      }
      output->terminal_index = index;
      output->terminal_pc = instruction->guest_pc;
      output->terminal_word = instruction->word;
      return 1;
    }
    if (input->host_base > UINT64_MAX - output->code_size ||
        emitted > UINT64_MAX - input->host_base - output->code_size)
      return 0;
    if (pcrel)
      emit_movconst(input->code + output->code_size, instruction->word & 31u,
                    pcrel_value(instruction->word, instruction->guest_pc));
    else
      emit_word(input->code + output->code_size, instruction->word);
    input->provenance[index].host_first = input->host_base + output->code_size;
    input->provenance[index].host_last =
        input->provenance[index].host_first + emitted;
    input->provenance[index].guest_pc = instruction->guest_pc;
    output->code_size += emitted;
    output->instruction_count++;
    output->provenance_count++;
  }
  output->terminal = HL_A64_COMPLETE;
  output->terminal_index = input->instruction_count;
  return 1;
}
