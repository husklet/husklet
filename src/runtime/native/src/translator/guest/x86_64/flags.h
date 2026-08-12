#ifndef HL_TRANSLATOR_GUEST_X86_64_FLAGS_H
#define HL_TRANSLATOR_GUEST_X86_64_FLAGS_H
#include <stdint.h>
uint64_t hl_x86_sub_nzcv(uint64_t left, uint64_t right, int width);
#endif
