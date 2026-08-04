#ifndef HL_NATIVE_AARCH64_ENTRY_H
#define HL_NATIVE_AARCH64_ENTRY_H

#include "../../../include/cpu.h"

/* entry.S embeds these offsets and must fail compilation if the generated CPU
 * frame moves independently of the assembly boundary. */
_Static_assert(offsetof(hl_native_aarch64_cpu, host_stack) == 280, "aarch64 host SP ABI drifted");
_Static_assert(offsetof(hl_native_aarch64_cpu, host_registers) == 288, "aarch64 host GPR ABI drifted");
_Static_assert(offsetof(hl_native_aarch64_cpu, host_vectors) == 896, "aarch64 host SIMD ABI drifted");

#if defined(__aarch64__)
void hl_native_aarch64_enter(hl_native_aarch64_cpu *, void (*)(void));
void hl_native_aarch64_return(void);
void hl_native_aarch64_fault_return(void);
void hl_native_aarch64_fallback(void);
#endif

#endif
