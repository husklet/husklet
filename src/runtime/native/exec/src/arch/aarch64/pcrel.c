#include "pcrel.h"

#include "stub.h"

#define CPU 28

static int64_t sign_extend(uint64_t value, unsigned bits) {
  uint64_t sign = UINT64_C(1) << (bits - 1);
  return (int64_t)((value ^ sign) - sign);
}

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) { return (word & 0x1F000000u) == 0x10000000u; }

int hl_a64_pcrel_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
  unsigned destination = word & 31u;
  unsigned native_destination = stolen(destination) ? 16u : destination;
  uint64_t encoded = (((uint64_t)word >> 5) & 0x7FFFFu) << 2;
  int64_t immediate;
  uint64_t value;
  if (assembler == NULL || !valid(word))
    return 0;
  encoded |= (word >> 29) & 3u;
  immediate = sign_extend(encoded, 21);
  if (word & (1u << 31))
    value = (pc & ~UINT64_C(0xFFF)) + (uint64_t)immediate * UINT64_C(4096);
  else
    value = pc + (uint64_t)immediate;

  if (destination != 31)
    hl_a64_movconst(assembler, (int)native_destination, value);
  if (stolen(destination))
    hl_a64_str(assembler, 16, CPU, (int)destination * 8);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_pcrel_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_PCREL_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_pcrel_body(assembler, word, pc))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
