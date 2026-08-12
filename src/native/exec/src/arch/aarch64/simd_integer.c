#include "simd_integer.h"

#include "stub.h"

static int valid(uint32_t word) {
  unsigned q = (word >> 30) & 1u;
  unsigned u = (word >> 29) & 1u;
  unsigned size = (word >> 22) & 3u;
  unsigned opcode = (word >> 11) & 31u;
  if ((word & UINT32_C(0x9f200400)) != UINT32_C(0x0e200400))
    return 0;
  if (!q && size == 3u)
    return 0;
  switch (opcode) {
  case 0x10u:
    return 1; /* ADD / SUB */
  case 0x0cu: /* SMAX / UMAX */
  case 0x0du: /* SMIN / UMIN */
  case 0x12u:
    return size != 3u; /* MLA / MLS */
  case 0x13u:
    return u ? size == 0u : size != 3u; /* PMUL / MUL */
  default:
    return 0;
  }
}

int hl_a64_simd_integer_body(hl_a64_assembler *assembler, uint32_t word) {
  if (assembler == NULL || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_integer_emit(hl_a64_assembler *assembler, uint32_t word,
                             uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_SIMD_INTEGER_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_integer_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
