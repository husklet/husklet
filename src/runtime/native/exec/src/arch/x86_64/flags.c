#include "flags.h"

uint32_t hl_x86_rflags_to_nzcv(uint64_t flags) {
  uint32_t nzcv = 0;

  if ((flags & HL_X86_RFLAGS_SF) != 0)
    nzcv |= UINT32_C(1) << 31;
  if ((flags & HL_X86_RFLAGS_ZF) != 0)
    nzcv |= UINT32_C(1) << 30;
  if ((flags & HL_X86_RFLAGS_CF) == 0)
    nzcv |= UINT32_C(1) << 29;
  if ((flags & HL_X86_RFLAGS_OF) != 0)
    nzcv |= UINT32_C(1) << 28;
  return nzcv;
}

uint64_t hl_x86_nzcv_to_rflags(uint32_t nzcv, uint64_t preserved) {
  uint64_t flags = preserved & ~HL_X86_RFLAGS_NZCV_MASK;

  if ((nzcv & (UINT32_C(1) << 31)) != 0)
    flags |= HL_X86_RFLAGS_SF;
  if ((nzcv & (UINT32_C(1) << 30)) != 0)
    flags |= HL_X86_RFLAGS_ZF;
  if ((nzcv & (UINT32_C(1) << 29)) == 0)
    flags |= HL_X86_RFLAGS_CF;
  if ((nzcv & (UINT32_C(1) << 28)) != 0)
    flags |= HL_X86_RFLAGS_OF;
  return flags;
}

int hl_x86_condition_holds(uint8_t condition, uint64_t flags) {
  int carry = (flags & HL_X86_RFLAGS_CF) != 0;
  int parity = (flags & HL_X86_RFLAGS_PF) != 0;
  int zero = (flags & HL_X86_RFLAGS_ZF) != 0;
  int sign = (flags & HL_X86_RFLAGS_SF) != 0;
  int overflow = (flags & HL_X86_RFLAGS_OF) != 0;
  int value;

  switch ((condition >> 1) & 7u) {
  case 0:
    value = overflow;
    break;
  case 1:
    value = carry;
    break;
  case 2:
    value = zero;
    break;
  case 3:
    value = carry || zero;
    break;
  case 4:
    value = sign;
    break;
  case 5:
    value = parity;
    break;
  case 6:
    value = sign != overflow;
    break;
  default:
    value = zero || sign != overflow;
    break;
  }
  return value ^ (condition & 1u);
}
