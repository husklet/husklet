#include "logical.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
  uint64_t mask;
  return (word & 0x1F800000u) == 0x12000000u &&
         hl_a64_logical_mask(word, &mask);
}

int hl_a64_logical_mask(uint32_t word, uint64_t *output) {
  unsigned width = (word >> 31) ? 64u : 32u;
  unsigned n = (word >> 22) & 1u;
  unsigned immr = (word >> 16) & 63u;
  unsigned imms = (word >> 10) & 63u;
  unsigned pattern = (n << 6) | ((~imms) & 63u);
  unsigned length;
  unsigned levels;
  unsigned size;
  unsigned ones;
  unsigned rotation;
  uint64_t element;
  uint64_t mask = 0;
  if (output == NULL || (width == 32 && n) || pattern < 2)
    return 0;
  length = 31u - (unsigned)__builtin_clz(pattern);
  levels = (1u << length) - 1u;
  if ((imms & levels) == levels)
    return 0;
  size = 1u << length;
  ones = (imms & levels) + 1u;
  rotation = immr & levels;
  element = (UINT64_C(1) << ones) - 1u;
  if (rotation != 0)
    element = (element >> rotation) | (element << (size - rotation));
  if (size != 64)
    element &= (UINT64_C(1) << size) - 1u;
  for (unsigned offset = 0; offset < width; offset += size)
    mask |= element << offset;
  *output = mask;
  return 1;
}

int hl_a64_logical_body(hl_a64_assembler *assembler, uint32_t word) {
  unsigned source = (word >> 5) & 31u;
  unsigned destination = word & 31u;
  unsigned native_source = stolen(source) ? 16u : source;
  unsigned native_destination =
      destination != 31 && stolen(destination) ? 17u : destination;
  if (assembler == NULL || !valid(word))
    return 0;

  if (stolen(source))
    hl_a64_ldr(assembler, 16, CPU, (int)source * 8);
  hl_a64_emit32(assembler,
                (word & ~0x3FFu) | (native_source << 5) | native_destination);
  if (destination != 31 && stolen(destination))
    hl_a64_str(assembler, 17, CPU, (int)destination * 8);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_logical_emit(hl_a64_assembler *assembler, uint32_t word,
                        uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_LOGICAL_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_logical_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
