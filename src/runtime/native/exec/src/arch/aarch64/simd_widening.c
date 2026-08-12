#include "simd_widening.h"
#include "stub.h"

static int valid(uint32_t word) {
  unsigned opcode = (word >> 12) & 15u;
  unsigned size = (word >> 22) & 3u;
  return (word & UINT32_C(0x9f200c00)) == UINT32_C(0x0e200000) && size != 3u &&
         opcode <= 3u;
}

int hl_a64_simd_widening_body(hl_a64_assembler *assembler, uint32_t word) {
  if (!assembler || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_widening_emit(hl_a64_assembler *assembler, uint32_t word,
                              uint64_t pc) {
  if (!assembler ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_WIDENING_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_widening_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
