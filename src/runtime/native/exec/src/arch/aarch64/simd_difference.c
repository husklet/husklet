#include "simd_difference.h"
#include "stub.h"

static int valid(uint32_t word) {
  unsigned size = (word >> 22) & 3u;
  if (size == 3u)
    return 0;
  if ((word & UINT32_C(0x9f200400)) == UINT32_C(0x0e200400)) {
    unsigned opcode = (word >> 11) & 31u;
    return opcode == 0x0eu || opcode == 0x0fu;
  }
  if ((word & UINT32_C(0x9f200c00)) == UINT32_C(0x0e200000)) {
    unsigned opcode = (word >> 12) & 15u;
    return opcode == 0x05u || opcode == 0x07u;
  }
  return 0;
}

int hl_a64_simd_difference_body(hl_a64_assembler *assembler, uint32_t word) {
  if (!assembler || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_simd_difference_emit(hl_a64_assembler *assembler, uint32_t word,
                                uint64_t pc) {
  if (!assembler ||
      hl_a64_assembler_remaining(assembler) <
          HL_A64_SIMD_DIFFERENCE_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_simd_difference_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
