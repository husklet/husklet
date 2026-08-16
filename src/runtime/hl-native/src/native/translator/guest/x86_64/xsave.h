#ifndef HL_TRANSLATOR_GUEST_X86_64_XSAVE_H
#define HL_TRANSLATOR_GUEST_X86_64_XSAVE_H

#include <stdint.h>

struct cpu;

#define HL_X86_XSAVE_XCR0 UINT64_C(3)
#define HL_X86_XSAVE_SPAN 576u

void hl_x86_xsave_legacy(struct cpu *cpu, uint8_t image[512]);
uint64_t hl_x86_xsave_xinuse(const uint8_t image[512]);
int hl_x86_xsave(struct cpu *cpu, uint64_t *fault_guest);

#endif
