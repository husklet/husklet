#include "fp_reduce.h"

#include "stub.h"

static int valid(uint32_t word) {
  unsigned scalar = (word >> 28) & 1u;
  unsigned size = (word >> 22) & 3u;
  unsigned opcode = (word >> 12) & 31u;
  if ((word & UINT32_C(0xef3e0c00)) != UINT32_C(0x6e300800))
    return 0;
  if (opcode != 0x0cu && opcode != 0x0fu)
    return 0;
  return scalar || (size & 1u) == 0u;
}

int hl_a64_simd_fp_reduce_body(hl_a64_assembler *assembler, uint32_t word) {
  if (assembler == NULL || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_fp_reduce_emit(hl_a64_assembler *assembler, uint32_t word,
                               uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_FP_REDUCE_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_fp_reduce_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
