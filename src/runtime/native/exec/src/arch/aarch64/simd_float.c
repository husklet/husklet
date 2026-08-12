#include "simd_float.h"

#include "stub.h"

static int fused(uint32_t word) {
  uint32_t box = word & UINT32_C(0xbfe0fc00);
  unsigned q = (word >> 30) & 1u;
  unsigned size = (word >> 22) & 1u;
  if (box != UINT32_C(0x0e20cc00) && box != UINT32_C(0x0e60cc00) &&
      box != UINT32_C(0x0ea0cc00) && box != UINT32_C(0x0ee0cc00))
    return 0;
  return !size || q;
}

static int integer_conversion(uint32_t word) {
  unsigned q = (word >> 30) & 1u;
  unsigned size = (word >> 22) & 1u;
  if ((word & UINT32_C(0x9fbffc00)) != UINT32_C(0x0e21d800))
    return 0;
  return !size || q;
}

static int valid(uint32_t word) {
  /* Baseline vector FMLA/FMLS and signed/unsigned integer-to-FP conversion.
   * Half precision and optional BF16/I8MM boxes remain behind explicit
   * feature-aware owners rather than being admitted as same-ISA copies. */
  return fused(word) || integer_conversion(word);
}

int hl_a64_simd_float_body(hl_a64_assembler *assembler, uint32_t word) {
  if (assembler == NULL || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_float_emit(hl_a64_assembler *assembler, uint32_t word,
                           uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_FLOAT_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_float_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
