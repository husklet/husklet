#ifndef _GNU_SOURCE
#define _GNU_SOURCE 1
#endif

#include "fault.h"

#include "entry.h"

#include <string.h>

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

static int width_valid(uint32_t width) {
  return width == 1 || width == 2 || width == 4 || width == 8 || width == 16 ||
         width == 32 || width == 64;
}

static int record_valid(const hl_native_provenance *record) {
  if (record == NULL || record->code_size != sizeof(uint32_t) ||
      (record->guest & 3u) != 0 ||
      (record->access != HL_NATIVE_ACCESS_READ &&
       record->access != HL_NATIVE_ACCESS_WRITE) ||
      !width_valid(record->width))
    return 0;
  const hl_native_address *address = &record->address;
  return address->kind == HL_NATIVE_ADDRESS_BASE && address->bits == 64 &&
         address->base == 16 && address->index == 0 && address->shift == 0 &&
         address->extend == HL_NATIVE_EXTEND_U64 && address->constant == 0 &&
         address->reserved == 0;
}

int hl_a64_fault_reconstruct(hl_native_aarch64_cpu *cpu,
                             const hl_a64_host_context *host,
                             const hl_native_provenance *record) {
  if (cpu == NULL || host == NULL || host->vectors_valid != 1 ||
      host->reserved != 0 || !record_valid(record) ||
      host->registers[28] != (uint64_t)(uintptr_t)cpu)
    return 0;

  /* Faultable opcodes are emitted only after the guard has restored every
   * non-stolen register. x16 is the projected EA, x17/x18 are staging, x28
   * is the CPU owner and x30 carries the remaining budget. Their guest values
   * remain authoritative in the CPU record. A faulting load and every
   * manually de-indexed writeback leave architectural destinations/base
   * unchanged, so copying the remaining live host state yields pre-op state. */
  for (unsigned reg = 0; reg <= 30; ++reg)
    if (!stolen(reg))
      cpu->registers[reg] = host->registers[reg];
  cpu->budget = host->registers[30];
  cpu->stack = host->stack;
  cpu->program = record->guest;
  cpu->flags = host->pstate & UINT64_C(0xf0000000);
  memcpy(cpu->vectors, host->vectors, sizeof(cpu->vectors));
  cpu->fpcr = host->fpcr;
  cpu->fpsr = host->fpsr;
  return 1;
}

int hl_a64_fault_prepare(hl_native_aarch64_cpu *cpu,
                         const hl_a64_host_context *host,
                         const hl_native_provenance *record,
                         uint64_t host_fault) {
  if (cpu == NULL || host == NULL || record == NULL || host_fault == 0 ||
      cpu->host_stack == 0 || (cpu->host_stack & 15u) != 0)
    return 0;
  uint64_t access_first;
  if (record->address.displacement >= 0) {
    uint64_t displacement = (uint64_t)record->address.displacement;
    if (host->registers[16] > UINT64_MAX - displacement)
      return 0;
    access_first = host->registers[16] + displacement;
  } else {
    uint64_t displacement = (uint64_t)(-(record->address.displacement + 1)) + 1;
    if (host->registers[16] < displacement)
      return 0;
    access_first = host->registers[16] - displacement;
  }
  if (!width_valid(record->width) ||
      access_first > UINT64_MAX - record->width || host_fault < access_first ||
      host_fault >= access_first + record->width)
    return 0;
  if (!hl_a64_fault_reconstruct(cpu, host, record))
    return 0;

  cpu->reason = HL_NATIVE_EXIT_FAULT;
  cpu->fault_address = host_fault - cpu->memory_delta;
  cpu->fault_access = record->access;
  cpu->fault_size = record->width;
  return 1;
}

#if defined(__linux__) && defined(__aarch64__)
#include <asm/sigcontext.h>
#include <ucontext.h>

int hl_a64_linux_context(const void *opaque, hl_a64_host_context *output) {
  if (opaque == NULL || output == NULL)
    return 0;
  const ucontext_t *context = opaque;
  hl_a64_host_context captured = {0};
  memcpy(captured.registers, context->uc_mcontext.regs,
         sizeof(captured.registers));
  captured.stack = context->uc_mcontext.sp;
  captured.program = context->uc_mcontext.pc;
  captured.pstate = context->uc_mcontext.pstate;

  const unsigned char *cursor = context->uc_mcontext.__reserved;
  const unsigned char *end = cursor + sizeof(context->uc_mcontext.__reserved);
  while ((size_t)(end - cursor) >= sizeof(struct _aarch64_ctx)) {
    struct _aarch64_ctx header;
    memcpy(&header, cursor, sizeof(header));
    if (header.magic == 0 && header.size == 0)
      break;
    if (header.size < sizeof(header) || (header.size & 15u) != 0 ||
        header.size > (size_t)(end - cursor))
      return 0;
    if (header.magic == FPSIMD_MAGIC) {
      if (header.size < sizeof(struct fpsimd_context))
        return 0;
      struct fpsimd_context fpsimd;
      memcpy(&fpsimd, cursor, sizeof(fpsimd));
      memcpy(captured.vectors, fpsimd.vregs, sizeof(captured.vectors));
      captured.fpcr = fpsimd.fpcr;
      captured.fpsr = fpsimd.fpsr;
      captured.vectors_valid = 1;
      *output = captured;
      return 1;
    }
    cursor += header.size;
  }
  return 0;
}
#else
int hl_a64_linux_context(const void *opaque, hl_a64_host_context *output) {
  (void)opaque;
  (void)output;
  return 0;
}
#endif

int hl_a64_linux_fault_reconstruct(hl_native_aarch64_cpu *cpu,
                                   const void *context,
                                   const hl_native_provenance *record) {
  hl_a64_host_context host;
  return hl_a64_linux_context(context, &host) &&
         hl_a64_fault_reconstruct(cpu, &host, record);
}

#if defined(__linux__) && defined(__aarch64__)
int hl_a64_linux_fault_return(hl_native_aarch64_cpu *cpu, void *opaque,
                              const hl_native_provenance *record,
                              uint64_t host_fault) {
  if (cpu == NULL || opaque == NULL || record == NULL)
    return 0;
  hl_a64_host_context host;
  if (!hl_a64_linux_context(opaque, &host) ||
      !hl_a64_fault_prepare(cpu, &host, record, host_fault))
    return 0;
  ucontext_t *context = opaque;
  context->uc_mcontext.regs[0] = (uint64_t)(uintptr_t)cpu;
  context->uc_mcontext.pc = (uint64_t)(uintptr_t)hl_native_aarch64_fault_return;
  return 1;
}
#else
int hl_a64_linux_fault_return(hl_native_aarch64_cpu *cpu, void *context,
                              const hl_native_provenance *record,
                              uint64_t host_fault) {
  (void)cpu;
  (void)context;
  (void)record;
  (void)host_fault;
  return 0;
}
#endif
