#ifndef HL_TRANSLATOR_GUEST_X86_64_OPERAND_H
#define HL_TRANSLATOR_GUEST_X86_64_OPERAND_H
#include <stdint.h>
uint64_t hl_x86_operand_read(uint64_t address, int width);
#endif
