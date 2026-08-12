#include "simd_logic.h"

#include "stub.h"

static int valid(uint32_t word) {
  /* Baseline AdvSIMD three-register logical operations.  Bits 23:22 select
   * AND/BIC/ORR/ORN/EOR/BSL/BIT/BIF and Q selects the vector width. */
  return (word & UINT32_C(0x9f20fc00)) == UINT32_C(0x0e201c00);
}

int hl_a64_simd_logic_body(hl_a64_assembler *assembler, uint32_t word) {
  if (assembler == NULL || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_logic_emit(hl_a64_assembler *assembler, uint32_t word,
                           uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_LOGIC_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_logic_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
