#ifndef HL_NATIVE_AARCH64_ENTRY_H
#define HL_NATIVE_AARCH64_ENTRY_H

#include "../../../include/cpu.h"

/* entry.S embeds these offsets and must fail compilation if the generated CPU
 * frame moves independently of the assembly boundary. */
_Static_assert(offsetof(hl_native_aarch64_cpu, host_stack) == 280,
               "aarch64 host SP ABI drifted");
_Static_assert(offsetof(hl_native_aarch64_cpu, host_registers) == 288,
               "aarch64 host GPR ABI drifted");
_Static_assert(offsetof(hl_native_aarch64_cpu, host_vectors) == 896,
               "aarch64 host SIMD ABI drifted");
/* The preamble's view-table proof embeds these four too. Growing dirty_records
 * moves them, which silently leaves read_valid_count zero and every guard
 * falling back rather than failing to build. */
_Static_assert(offsetof(hl_native_aarch64_cpu, read_token) == 1688,
               "aarch64 view-table proof drifted");
_Static_assert(offsetof(hl_native_aarch64_cpu, read_incarnation) == 1696,
               "aarch64 view-table proof drifted");
_Static_assert(offsetof(hl_native_aarch64_cpu, read_count) == 1704,
               "aarch64 view-table proof drifted");
_Static_assert(offsetof(hl_native_aarch64_cpu, read_valid_count) == 2400,
               "aarch64 view-table proof drifted");

#if defined(__aarch64__)
void hl_native_aarch64_enter(hl_native_aarch64_cpu *, void (*)(void));
void hl_native_aarch64_return(void);
void hl_native_aarch64_fault_return(void);
void hl_native_aarch64_fallback(void);
#endif

#endif
