#include "memory.h"

#include <string.h>

static int64_t sign_extend(uint64_t value, unsigned bits) {
  uint64_t sign = UINT64_C(1) << (bits - 1);
  return (int64_t)((value ^ sign) - sign);
}

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int casp(uint32_t word) { return (word & 0xBFA07C00u) == 0x08207C00u; }

static uint64_t memory_bytes(uint32_t word) {
  if (casp(word))
    return (word & (1u << 30)) ? 16 : 8;
  int pair = (word & 0x3A000000u) == 0x28000000u;
  int vector = (word >> 26) & 1;
  unsigned size = (word >> 30) & 3;
  uint64_t bytes;
  if (pair)
    bytes = vector ? (UINT64_C(4) << size)
                   : (size == 2 ? UINT64_C(8) : UINT64_C(4));
  else if (!vector)
    bytes = UINT64_C(1) << size;
  else {
    unsigned scale = ((((word >> 22) & 3u) >> 1) << 2) | size;
    bytes = UINT64_C(1) << scale;
  }
  if ((word & 0x3F000000u) == 0x08000000u && !(word & (1u << 23)) &&
      (word & (1u << 21)))
    bytes *= 2;
  return pair ? bytes * 2 : bytes;
}

static int prefetch(uint32_t word) {
  if ((word & 0xFF000000u) == 0xD8000000u)
    return 1;
  if ((word & 0x04000000u) != 0 || ((word >> 30) & 3) != 3 ||
      ((word >> 22) & 3) != 2)
    return 0;
  return (word & 0x3B000000u) == 0x39000000u ||
         (word & 0x3B200000u) == 0x38000000u ||
         (word & 0x3B200C00u) == 0x38200800u;
}

static uint64_t structure_bytes(uint32_t word) {
  int q = (word >> 30) & 1;
  if (!((word >> 24) & 1)) {
    int regs;
    switch ((word >> 12) & 15u) {
    case 0:
    case 2:
      regs = 4;
      break;
    case 4:
    case 6:
      regs = 3;
      break;
    case 8:
    case 10:
      regs = 2;
      break;
    case 7:
      regs = 1;
      break;
    default:
      regs = 0;
      break;
    }
    return (uint64_t)regs * (q ? 16u : 8u);
  }
  int opcode = (word >> 13) & 7u;
  int scale = (opcode >> 1) & 3;
  int elements = (int)((((opcode & 1) << 1) | ((word >> 21) & 1)) + 1);
  if (scale == 3)
    scale = (word >> 10) & 3u;
  return (uint64_t)elements << scale;
}

int hl_a64_memory_decode(uint32_t word, uint64_t pc, hl_a64_memory *output) {
  unsigned mode;
  if (output == NULL)
    return 0;
  memset(output, 0, sizeof(*output));
  output->word = word;
  output->pc = pc;
  output->base = (uint8_t)((word >> 5) & 31u);
  output->target = (uint8_t)(word & 31u);
  output->target2 = (uint8_t)((word >> 10) & 31u);
  output->index = (uint8_t)((word >> 16) & 31u);
  output->vector = (uint8_t)((word >> 26) & 1u);
  if ((word & 0x3B000000u) == 0x18000000u ||
      (word & 0xFF000000u) == 0x98000000u ||
      (word & 0x3F000000u) == 0x1C000000u ||
      (word & 0xFF000000u) == 0xD8000000u) {
    output->kind =
        prefetch(word) ? HL_A64_MEMORY_PREFETCH : HL_A64_MEMORY_LITERAL;
    output->offset = sign_extend((word >> 5) & 0x7ffffu, 19) << 2;
    output->base = 31;
  } else if ((word & 0x0A000000u) != 0x08000000u) {
    return 0;
  } else if ((word & 0xBE000000u) == 0x0C000000u) {
    output->kind = HL_A64_MEMORY_STRUCTURE;
    output->writeback = (uint8_t)((word >> 23) & 1u);
    output->bytes = structure_bytes(word);
  } else if ((word & 0x3A000000u) == 0x28000000u) {
    output->kind = HL_A64_MEMORY_PAIR;
    mode = (word >> 23) & 3u;
    output->postindex = mode == 1;
    output->writeback = mode == 1 || mode == 3;
    output->offset = output->postindex ? 0
                                       : sign_extend((word >> 15) & 0x7fu, 7) *
                                             (int64_t)(memory_bytes(word) / 2);
  } else if ((word & 0x3F000000u) == 0x08000000u) {
    output->kind = casp(word) || (word & (1u << 23)) ? HL_A64_MEMORY_ATOMIC
                                                     : HL_A64_MEMORY_EXCLUSIVE;
  } else if ((word & 0x3B200C00u) == 0x38200000u) {
    output->kind = HL_A64_MEMORY_ATOMIC;
  } else if ((word & 0x3B000000u) == 0x39000000u) {
    output->kind =
        prefetch(word) ? HL_A64_MEMORY_PREFETCH : HL_A64_MEMORY_UNSIGNED;
    output->offset = (int64_t)((word >> 10) & 0xfffu)
                     << __builtin_ctzll(memory_bytes(word));
  } else if ((word & 0x3B200C00u) == 0x38200800u) {
    output->kind =
        prefetch(word) ? HL_A64_MEMORY_PREFETCH : HL_A64_MEMORY_REGISTER;
  } else if ((word & 0x3B200000u) == 0x38000000u) {
    output->kind =
        prefetch(word) ? HL_A64_MEMORY_PREFETCH : HL_A64_MEMORY_UNSCALED;
    mode = (word >> 10) & 3u;
    output->postindex = mode == 1;
    output->writeback = mode == 1 || mode == 3;
    output->offset =
        output->postindex ? 0 : sign_extend((word >> 12) & 0x1ffu, 9);
  } else {
    return 0;
  }
  if (output->kind == HL_A64_MEMORY_PREFETCH) {
    output->bytes = 0;
  } else if (output->kind == HL_A64_MEMORY_LITERAL) {
    if ((word & 0xFF000000u) == 0x98000000u)
      output->bytes = 4;
    else if ((word & 0x3F000000u) == 0x1C000000u)
      output->bytes = UINT64_C(4) << ((word >> 30) & 3u);
    else
      output->bytes = (word & (1u << 30)) ? 8 : 4;
  } else if (output->kind != HL_A64_MEMORY_STRUCTURE) {
    output->bytes = memory_bytes(word);
  }
  if (output->kind != HL_A64_MEMORY_PREFETCH) {
    if (output->kind == HL_A64_MEMORY_LITERAL)
      output->read = 1;
    else if (output->kind == HL_A64_MEMORY_UNSIGNED ||
             output->kind == HL_A64_MEMORY_UNSCALED ||
             output->kind == HL_A64_MEMORY_REGISTER)
      /* SIMD/FP uses opc bit zero for direction: opc=2 stores Q and
       * opc=3 loads Q. Integer singles instead reserve opc=0 for stores. */
      output->read = output->vector ? (uint8_t)(((word >> 22) & 1u) != 0)
                                    : (uint8_t)(((word >> 22) & 3u) != 0);
    else
      output->read = (uint8_t)((word >> 22) & 1u);
    output->write = (uint8_t)!output->read;
    if ((word & 0x3B200C00u) == 0x38200000u || casp(word) ||
        (word & 0x3FA07C00u) == 0x08A07C00u)
      output->read = output->write = 1;
  }
  output->stolen = (uint8_t)(stolen(output->base) ||
                             (!output->vector && stolen(output->target)) ||
                             (output->kind == HL_A64_MEMORY_PAIR &&
                              !output->vector && stolen(output->target2)) ||
                             ((output->kind == HL_A64_MEMORY_REGISTER ||
                               (output->kind == HL_A64_MEMORY_STRUCTURE &&
                                output->writeback && output->index != 31)) &&
                              stolen(output->index)) ||
                             (output->kind == HL_A64_MEMORY_ATOMIC &&
                              stolen(output->index)));
  return 1;
}
