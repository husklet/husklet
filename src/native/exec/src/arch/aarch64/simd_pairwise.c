#include "simd_pairwise.h"

#include "stub.h"

static int valid(uint32_t word) {
  unsigned u = (word >> 29) & 1u;
  unsigned size = (word >> 22) & 3u;
  unsigned opcode = (word >> 12) & 31u;
  if ((word & UINT32_C(0xdf3e0c00)) != UINT32_C(0x5e300800))
    return 0;
  if (!u)
    return opcode == 0x1bu && size == 3u;
  return opcode == 0x0du && size < 2u;
}

int hl_a64_simd_pairwise_body(hl_a64_assembler *assembler, uint32_t word) {
  if (assembler == NULL || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_pairwise_emit(hl_a64_assembler *assembler, uint32_t word,
                              uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_PAIRWISE_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_pairwise_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
