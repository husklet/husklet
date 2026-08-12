#include "add.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int immediate(uint32_t word) {
  return (word & 0x1F800000u) == 0x11000000u;
}

static int shifted(uint32_t word) {
  return (word & 0x1F200000u) == 0x0B000000u;
}

static int shifted_body(hl_a64_assembler *assembler, uint32_t word) {
  unsigned destination = word & 31u;
  unsigned left = (word >> 5) & 31u;
  unsigned right = (word >> 16) & 31u;
  unsigned native_left = stolen(left) ? 16u : left;
  unsigned native_right = stolen(right) ? 17u : right;
  unsigned native_destination = destination;
  if (stolen(left))
    hl_a64_ldr(assembler, 16, CPU, (int)left * 8);
  if (stolen(right))
    hl_a64_ldr(assembler, 17, CPU, (int)right * 8);
  if (destination != 31 && stolen(destination)) {
    if (destination == left)
      native_destination = native_left;
    else if (destination == right)
      native_destination = native_right;
    else if (!stolen(left))
      native_destination = 16u;
    else if (!stolen(right))
      native_destination = 17u;
    else
      return 0;
  }
  hl_a64_emit32(assembler, (word & ~0x1F03FFu) | (native_right << 16) |
                               (native_left << 5) | native_destination);
  if (destination != 31 && stolen(destination))
    hl_a64_str(assembler, (int)native_destination, CPU, (int)destination * 8);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_add_body(hl_a64_assembler *assembler, uint32_t word) {
  unsigned source = (word >> 5) & 31u;
  unsigned destination = word & 31u;
  unsigned native_source = stolen(source) ? 16u : source;
  unsigned native_destination =
      destination != 31 && stolen(destination) ? 17u : destination;
  if (assembler == NULL)
    return 0;
  if (shifted(word))
    return shifted_body(assembler, word);
  if (!immediate(word))
    return 0;
  if (stolen(source))
    hl_a64_ldr(assembler, 16, CPU, (int)source * 8);
  hl_a64_emit32(assembler,
                (word & ~0x3FFu) | (native_source << 5) | native_destination);
  if (destination != 31 && stolen(destination))
    hl_a64_str(assembler, 17, CPU, (int)destination * 8);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_add_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_ADD_MAX_BYTES ||
      (!immediate(word) && !shifted(word)))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_add_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
