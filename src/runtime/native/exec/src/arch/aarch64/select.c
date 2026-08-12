#include "select.h"

#include "stub.h"

#define CPU 28

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int valid(uint32_t word) { return (word & 0x1FE00800u) == 0x1A800000u; }

int hl_a64_select_body(hl_a64_assembler *assembler, uint32_t word) {
  unsigned alternative = (word >> 16) & 31u;
  unsigned preferred = (word >> 5) & 31u;
  unsigned destination = word & 31u;
  unsigned native_alternative = stolen(alternative) ? 17u : alternative;
  unsigned native_preferred = stolen(preferred) ? 16u : preferred;
  unsigned native_destination =
      destination != 31 && stolen(destination) ? 16u : destination;
  if (assembler == NULL || !valid(word))
    return 0;
  if (stolen(preferred))
    hl_a64_ldr(assembler, 16, CPU, (int)preferred * 8);
  if (stolen(alternative))
    hl_a64_ldr(assembler, 17, CPU, (int)alternative * 8);
  hl_a64_emit32(assembler, (word & ~((31u << 16) | (31u << 5) | 31u)) |
                               (native_alternative << 16) |
                               (native_preferred << 5) | native_destination);
  if (destination != 31 && stolen(destination))
    hl_a64_str(assembler, 16, CPU, (int)destination * 8);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_select_emit(hl_a64_assembler *assembler, uint32_t word,
                       uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SELECT_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_select_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
