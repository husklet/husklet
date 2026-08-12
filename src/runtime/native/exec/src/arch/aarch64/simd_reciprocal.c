#include "simd_reciprocal.h"

#include "stub.h"

static int valid(uint32_t word) {
  unsigned q = (word >> 30) & 1u;
  unsigned u = (word >> 29) & 1u;
  unsigned scalar = (word >> 28) & 1u;
  unsigned size = (word >> 22) & 3u;
  unsigned opcode = (word >> 12) & 31u;
  if ((word & UINT32_C(0x8f3e0c00)) == UINT32_C(0x0e200800)) {
    if (opcode == 0x1cu)
      return !scalar && size == 2u;
    if (opcode != 0x1du || size < 2u)
      return 0;
    return scalar ? q != 0u : (size == 2u || q != 0u);
  }
  if ((word & UINT32_C(0x8f200400)) != UINT32_C(0x0e200400) ||
      ((word >> 11) & 31u) != 0x1fu || u)
    return 0;
  return scalar ? q != 0u : ((size & 1u) == 0u || q != 0u);
}

int hl_a64_simd_reciprocal_body(hl_a64_assembler *assembler, uint32_t word) {
  if (assembler == NULL || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_reciprocal_emit(hl_a64_assembler *assembler, uint32_t word,
                                uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) <
          HL_A64_SIMD_RECIPROCAL_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_reciprocal_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
