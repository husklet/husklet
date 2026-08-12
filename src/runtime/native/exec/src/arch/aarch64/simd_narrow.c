#include "simd_narrow.h"

#include "stub.h"

static int valid(uint32_t word) {
  unsigned immediate_high = (word >> 19) & 15u;
  unsigned opcode = (word >> 11) & 31u;
  return (word & 0xBF800400u) == 0x0F000400u && immediate_high != 0 &&
         (immediate_high & 8u) == 0 && (opcode == 0x10u || opcode == 0x11u);
}

int hl_a64_simd_narrow_body(hl_a64_assembler *assembler, uint32_t word) {
  if (assembler == NULL || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_narrow_emit(hl_a64_assembler *assembler, uint32_t word,
                            uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_NARROW_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_narrow_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
