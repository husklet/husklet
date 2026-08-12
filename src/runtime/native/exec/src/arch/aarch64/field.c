#include "field.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) {
  unsigned sf = word >> 31, n = (word >> 22) & 1u;
  unsigned immr = (word >> 16) & 63u, imms = (word >> 10) & 63u;
  unsigned opcode = (word >> 29) & 3u;
  int extract = (word & 0x7F800000u) == 0x13800000u;
  int bitfield = (word & 0x1F800000u) == 0x13000000u;
  return (extract || bitfield) && n == sf && (sf || (immr < 32 && imms < 32)) &&
         (!bitfield || opcode != 3);
}

int hl_a64_field_body(hl_a64_assembler *assembler, uint32_t word) {
  unsigned right = (word >> 16) & 31u;
  unsigned left = (word >> 5) & 31u;
  unsigned destination = word & 31u;
  unsigned opcode = (word >> 29) & 3u;
  int extract = (word & 0x7F800000u) == 0x13800000u;
  int bitfield = (word & 0x1F800000u) == 0x13000000u;
  uint32_t native;
  if (assembler == NULL || !valid(word))
    return 0;
  if (stolen(left))
    hl_a64_ldr(assembler, 16, CPU, (int)left * 8);
  if (extract && stolen(right))
    hl_a64_ldr(assembler, 17, CPU, (int)right * 8);
  if (bitfield && opcode == 1 && stolen(destination))
    hl_a64_ldr(assembler, 17, CPU, (int)destination * 8);
  native = word & ~((31u << 16) | (31u << 5) | 31u);
  if (extract) {
    native |= (stolen(right) ? 17u : right) << 16;
    native |= (stolen(left) ? 16u : left) << 5;
    native |= destination != 31 && stolen(destination) ? 16u : destination;
  } else {
    native |= word & (31u << 16); /* immr occupies the extract Rm field */
    native |= (stolen(left) ? 16u : left) << 5;
    native |= destination != 31 && stolen(destination) ? 17u : destination;
  }
  hl_a64_emit32(assembler, native);
  if (destination != 31 && stolen(destination))
    hl_a64_str(assembler, extract ? 16 : 17, CPU, (int)destination * 8);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_field_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_FIELD_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_field_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
