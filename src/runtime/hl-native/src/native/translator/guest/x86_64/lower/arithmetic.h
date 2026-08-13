#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_ARITHMETIC_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_ARITHMETIC_H

#include <stdint.h>

void e_imul2(int destination, int left, int right, int width, int carry_overflow_live);
void e_mul_set_oc(int carry_register);
void e_mul_oc_narrow(int product, int kind, int width);
void e_rot_flags_const(int result, int kind, int width, int count);
void e_rot_flags_cl(int result, int kind, int width);
void e_nzcv_set_of(int overflow_register);
int alu_kind_primary(uint8_t opcode);
void alu_core(int kind, int output, int left, int right, int sixty_four_bit);

#endif
