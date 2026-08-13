#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_SIMD_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_SIMD_H

#include <stdint.h>

void hl_x86_emit_pcmpistri_eqeach_byte(int, int, int);
void e_sse_var_shift(int, int, int, int, int, int);
void hl_x86_emit_dnan_pre(int, int, int, int);
void hl_x86_emit_dnan_post(int, int, int);
void hl_x86_emit_nan_input_gate(int, int, int, uint64_t);

#endif
