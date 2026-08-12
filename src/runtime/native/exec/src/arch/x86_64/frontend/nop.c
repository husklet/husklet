#include "private.h"

#include "../decode.h"

static int fail(decode *block, size_t *cursor, size_t start,
                hl_x86_a64_status status) {
  *cursor = start;
  block->status = status;
  block->exit = HL_X86_A64_INTERPRETER;
  return 0;
}

int hl_x86_decode_nop(const hl_x86_a64_request *request, decode *block,
                      instruction *item, size_t start, size_t *cursor) {
  uint8_t modrm;
  uint8_t mod;
  uint8_t rm;
  size_t displacement = 0;

  if (*cursor >= request->guest_size || *cursor - start >= 15u)
    return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
  modrm = request->guest_bytes[(*cursor)++];
  mod = modrm >> 6;
  rm = modrm & 7u;
  if (((modrm >> 3) & 7u) != 0u)
    return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
  if (mod != 3u && rm == 4u) {
    uint8_t sib;
    if (*cursor >= request->guest_size || *cursor - start >= 15u)
      return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    sib = request->guest_bytes[(*cursor)++];
    if (mod == 0u && (sib & 7u) == 5u)
      displacement = 4u;
  } else if (mod == 0u && rm == 5u) {
    displacement = 4u;
  }
  if (mod == 1u)
    displacement = 1u;
  if (mod == 2u)
    displacement = 4u;
  if (displacement > request->guest_size - *cursor ||
      displacement > 15u - (*cursor - start))
    return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
  *cursor += displacement;
  item->operation = OP_NOP;
  return 1;
}
