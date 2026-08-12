#ifndef HL_NATIVE_X86_64_FLAGS_H
#define HL_NATIVE_X86_64_FLAGS_H

#include <stdint.h>

#define HL_X86_RFLAGS_CF (UINT64_C(1) << 0)
#define HL_X86_RFLAGS_PF (UINT64_C(1) << 2)
#define HL_X86_RFLAGS_AF (UINT64_C(1) << 4)
#define HL_X86_RFLAGS_ZF (UINT64_C(1) << 6)
#define HL_X86_RFLAGS_SF (UINT64_C(1) << 7)
#define HL_X86_RFLAGS_OF (UINT64_C(1) << 11)
#define HL_X86_RFLAGS_NZCV_MASK (HL_X86_RFLAGS_CF | HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF | HL_X86_RFLAGS_OF)

uint32_t hl_x86_rflags_to_nzcv(uint64_t flags);
uint64_t hl_x86_nzcv_to_rflags(uint32_t nzcv, uint64_t preserved);
int hl_x86_condition_holds(uint8_t condition, uint64_t flags);

#endif
