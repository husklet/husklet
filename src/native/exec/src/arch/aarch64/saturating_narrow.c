#include "saturating_narrow.h"
#include "stub.h"

static int valid(uint32_t word) {
  unsigned q = (word >> 30) & 1u;
  unsigned u = (word >> 29) & 1u;
  unsigned scalar = (word >> 28) & 1u;
  unsigned size = (word >> 22) & 3u;
  unsigned opcode = (word >> 12) & 31u;
  if ((word & UINT32_C(0x8f3e0c00)) != UINT32_C(0x0e200800) || size == 3u)
    return 0;
  if (scalar && !q)
    return 0;
  return opcode == 0x14u || (opcode == 0x12u && u);
}

int hl_a64_saturating_narrow_body(hl_a64_assembler *assembler, uint32_t word) {
  if (!assembler || !valid(word))
    return 0;
  hl_a64_emit32(assembler, word);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_saturating_narrow_emit(hl_a64_assembler *assembler, uint32_t word,
                                  uint64_t pc) {
  if (!assembler ||
      hl_a64_assembler_remaining(assembler) <
          HL_A64_SATURATING_NARROW_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_saturating_narrow_body(assembler, word))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  return hl_a64_assembler_ok(assembler);
}
