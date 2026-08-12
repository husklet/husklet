#include "direct.h"

#include "stub.h"

#define CPU 28

static int64_t sign_extend(uint64_t value, unsigned bits) {
  uint64_t sign = UINT64_C(1) << (bits - 1);
  return (int64_t)((value ^ sign) - sign);
}

static int valid(uint32_t word) {
  uint32_t opcode = word & 0xFC000000u;
  return opcode == 0x14000000u || opcode == 0x94000000u;
}

int hl_a64_direct_chain(hl_a64_assembler *assembler, uint32_t word, uint64_t pc,
                        uint32_t **patch, uint64_t *chain_target) {
  uint32_t opcode = word & 0xFC000000u;
  uint64_t target;
  int64_t displacement;
  if (assembler == NULL || !valid(word))
    return 0;
  displacement = sign_extend(word & 0x03FFFFFFu, 26) * 4;
  target = pc + (uint64_t)displacement;

  if (opcode == 0x94000000u) {
    hl_a64_movconst(assembler, 16, pc + 4);
    hl_a64_str(assembler, 16, CPU, 30 * 8);
  }
  uint32_t *reservation = hl_a64_stub_edge_reserve(assembler);
  if (patch != NULL)
    *patch = reservation;
  if (chain_target != NULL)
    *chain_target = target;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, target);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_direct_body(hl_a64_assembler *assembler, uint32_t word,
                       uint64_t pc) {
  return hl_a64_direct_chain(assembler, word, pc, NULL, NULL);
}

int hl_a64_direct_emit(hl_a64_assembler *assembler, uint32_t word,
                       uint64_t pc) {
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_DIRECT_MAX_BYTES ||
      !valid(word))
    return 0;
  hl_a64_stub_prologue(assembler);
  return hl_a64_direct_body(assembler, word, pc);
}
