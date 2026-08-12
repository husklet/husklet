#ifndef HL_NATIVE_AARCH64_FAULT_H
#define HL_NATIVE_AARCH64_FAULT_H

#include "../../../include/executor.h"

/* Signal-safe POD copy of the AArch64 host state needed to reconstruct one
 * faulting guest instruction. `vectors` stores v0..v31 as low/high u64 words.
 */
typedef struct hl_a64_host_context {
  uint64_t registers[31];
  uint64_t stack;
  uint64_t program;
  uint64_t pstate;
  uint64_t vectors[64];
  uint32_t fpcr;
  uint32_t fpsr;
  uint32_t vectors_valid;
  uint32_t reserved;
} hl_a64_host_context;

_Static_assert(sizeof(hl_a64_host_context) == 800,
               "aarch64 host context ABI drifted");
_Static_assert(_Alignof(hl_a64_host_context) == 8,
               "aarch64 host context alignment drifted");
_Static_assert(offsetof(hl_a64_host_context, stack) == 248,
               "aarch64 host stack offset drifted");
_Static_assert(offsetof(hl_a64_host_context, vectors) == 272,
               "aarch64 host vector offset drifted");
_Static_assert(offsetof(hl_a64_host_context, fpcr) == 784,
               "aarch64 host FP offset drifted");

int hl_a64_fault_reconstruct(hl_native_aarch64_cpu *,
                             const hl_a64_host_context *,
                             const hl_native_provenance *);
int hl_a64_fault_prepare(hl_native_aarch64_cpu *, const hl_a64_host_context *,
                         const hl_native_provenance *, uint64_t);
int hl_a64_linux_context(const void *, hl_a64_host_context *);
int hl_a64_linux_fault_reconstruct(hl_native_aarch64_cpu *, const void *,
                                   const hl_native_provenance *);
int hl_a64_linux_fault_return(hl_native_aarch64_cpu *, void *,
                              const hl_native_provenance *, uint64_t);
int hl_a64_darwin_context(const void *, hl_a64_host_context *);
int hl_a64_darwin_fault_return(hl_native_aarch64_cpu *, void *,
                               const hl_native_provenance *, uint64_t);

#endif
