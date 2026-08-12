#include "fault.h"

#include "entry.h"

#if defined(__APPLE__) && defined(__aarch64__)
#include <string.h>
#include <sys/ucontext.h>

int hl_a64_darwin_context(const void *opaque, hl_a64_host_context *output) {
  if (opaque == NULL || output == NULL)
    return 0;
  const ucontext_t *context = opaque;
  if (context->uc_mcontext == NULL)
    return 0;
  const _STRUCT_MCONTEXT64 *machine = context->uc_mcontext;
  hl_a64_host_context captured = {0};
  memcpy(captured.registers, machine->__ss.__x, sizeof(machine->__ss.__x));
  captured.registers[29] = machine->__ss.__fp;
  captured.registers[30] = machine->__ss.__lr;
  captured.stack = machine->__ss.__sp;
  captured.program = machine->__ss.__pc;
  captured.pstate = machine->__ss.__cpsr;
  memcpy(captured.vectors, machine->__ns.__v, sizeof(captured.vectors));
  captured.fpcr = machine->__ns.__fpcr;
  captured.fpsr = machine->__ns.__fpsr;
  captured.vectors_valid = 1;
  *output = captured;
  return 1;
}

int hl_a64_darwin_fault_return(hl_native_aarch64_cpu *cpu, void *opaque,
                               const hl_native_provenance *record,
                               uint64_t host_fault) {
  if (cpu == NULL || opaque == NULL || record == NULL)
    return 0;
  ucontext_t *context = opaque;
  hl_a64_host_context host;
  if (!hl_a64_darwin_context(context, &host) ||
      !hl_a64_fault_prepare(cpu, &host, record, host_fault))
    return 0;
  context->uc_mcontext->__ss.__x[0] = (uint64_t)(uintptr_t)cpu;
  context->uc_mcontext->__ss.__pc =
      (uint64_t)(uintptr_t)hl_native_aarch64_fault_return;
  return 1;
}
#else
int hl_a64_darwin_context(const void *opaque, hl_a64_host_context *output) {
  (void)opaque;
  (void)output;
  return 0;
}

int hl_a64_darwin_fault_return(hl_native_aarch64_cpu *cpu, void *context,
                               const hl_native_provenance *record,
                               uint64_t host_fault) {
  (void)cpu;
  (void)context;
  (void)record;
  (void)host_fault;
  return 0;
}
#endif
