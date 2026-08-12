#include "atomic.h"

#include "projection.h"
#include "stub.h"

#include <string.h>

#if defined(__linux__) && defined(__aarch64__)
#include <asm/hwcap.h>
#include <sys/auxv.h>
#elif defined(__APPLE__) && defined(__aarch64__)
#include <sys/sysctl.h>
#endif

#define CPU 28

typedef struct atomic_form {
  unsigned bytes;
  unsigned base;
  unsigned source;
  unsigned target;
  unsigned writes_source;
  unsigned writes_target;
} atomic_form;

static int stolen(unsigned reg) {
  return reg == 16 || reg == 17 || reg == 18 || reg == 28 || reg == 30;
}

/* CAS/CASA/CASL/CASAL at byte, halfword, word and doubleword width.  Bit 23
 * separates these from CASP, whose register-pair staging is not lowered. */
static int compare_swap(uint32_t word) {
  return (word & UINT32_C(0x3fa07c00)) == UINT32_C(0x08a07c00);
}

/* The integer atomic-memory group: LDADD/LDCLR/LDEOR/LDSET/LDSMAX/LDSMIN/
 * LDUMAX/LDUMIN with o3 clear, and SWP alone with o3 set.  LDAPR and the
 * 64-byte accelerator forms share the group and are rejected. */
static int atomic_memory(uint32_t word) {
  if ((word & UINT32_C(0x3f200c00)) != UINT32_C(0x38200000))
    return 0;
  return ((word >> 15) & 1u) == 0u || ((word >> 12) & 7u) == 0u;
}

static int decode(uint32_t word, atomic_form *form) {
  memset(form, 0, sizeof(*form));
  form->bytes = 1u << ((word >> 30) & 3u);
  form->source = (word >> 16) & 31u;
  form->base = (word >> 5) & 31u;
  form->target = word & 31u;
  if (compare_swap(word)) {
    /* CAS returns the pre-op value in Rs and only reads Rt. */
    form->writes_source = form->source != 31u;
    return 1;
  }
  if (atomic_memory(word)) {
    /* SWP and the LD<op> family read Rs and return the pre-op value in
     * Rt; Rt==31 is the ST<op> alias, which discards it. */
    form->writes_target = form->target != 31u;
    return 1;
  }
  return 0;
}

int hl_a64_atomic_host_supports(void) {
  static int cached;
  int value = cached;
  if (value != 0)
    return value > 0;
#if defined(__linux__) && defined(__aarch64__)
  value = (getauxval(AT_HWCAP) & HWCAP_ATOMICS) != 0 ? 1 : -1;
#elif defined(__APPLE__) && defined(__aarch64__)
  {
    int present = 0;
    size_t size = sizeof(present);
    value = sysctlbyname("hw.optional.arm.FEAT_LSE", &present, &size, NULL,
                         0) == 0 &&
                    size == sizeof(present) && present != 0
                ? 1
                : -1;
  }
#else
  value = -1;
#endif
  cached = value;
  return value > 0;
}

int hl_a64_atomic_definitions(uint32_t word, uint32_t *definitions) {
  atomic_form form;
  if (definitions == NULL || !decode(word, &form))
    return 0;
  *definitions = 0;
  if (form.writes_source)
    *definitions |= UINT32_C(1) << form.source;
  if (form.writes_target)
    *definitions |= UINT32_C(1) << form.target;
  return 1;
}

static void address(hl_a64_assembler *assembler, unsigned base) {
  if (base == 31)
    hl_a64_mov_from_sp(assembler, 16);
  else if (stolen(base))
    hl_a64_ldr(assembler, 16, CPU, (int)base * 8);
  else
    hl_a64_movr(assembler, 16, (int)base);
}

int hl_a64_atomic_body(hl_a64_assembler *assembler, uint32_t word, uint64_t pc,
                       hl_a64_guard *guard, hl_a64_memory_sites *sites) {
  atomic_form form;
  unsigned native_source;
  int source_stolen;
  if (assembler == NULL || guard == NULL || !decode(word, &form) ||
      !hl_a64_atomic_host_supports())
    return 0;
  /* Rt would need a third staged temporary; leave those words interpreted. */
  if (form.target != 31u && stolen(form.target))
    return 0;
  memset(guard, 0, sizeof(*guard));
  if (sites != NULL)
    memset(sites, 0, sizeof(*sites));
  guard->pc = pc;
  source_stolen = form.source != 31u && stolen(form.source);
  native_source = source_stolen ? 17u : form.source;
  address(assembler, form.base);
  /* A read-modify-write demands write permission and must journal, so the
   * guard's exact-value required mode is WRITE. */
  hl_a64_guard_begin(assembler, form.bytes, HL_A64_PERMISSION_WRITE, guard);
  hl_a64_guard_write_begin(assembler, form.bytes, pc, guard);
  if (source_stolen)
    hl_a64_ldr(assembler, 17, CPU, (int)form.source * 8);
  if (sites != NULL) {
    sites->count = 1;
    sites->entries[0] = (hl_a64_memory_site){
        .code_offset = hl_a64_assembler_size(assembler),
        .access = HL_NATIVE_ACCESS_WRITE,
        .width = form.bytes,
    };
  }
  hl_a64_emit32(assembler, (word & ~((31u << 16) | (31u << 5) | 31u)) |
                               (native_source << 16) | (16u << 5) |
                               form.target);
  /* x17 carries the result and the journal helper clobbers it. */
  if (source_stolen && form.writes_source)
    hl_a64_str(assembler, 17, CPU, (int)form.source * 8);
  hl_a64_guard_written(assembler, form.bytes);
  return hl_a64_assembler_ok(assembler);
}

int hl_a64_atomic_emit(hl_a64_assembler *assembler, uint32_t word,
                       uint64_t pc) {
  hl_a64_guard guard;
  atomic_form form;
  if (assembler == NULL ||
      hl_a64_assembler_remaining(assembler) < HL_A64_ATOMIC_MAX_BYTES ||
      !decode(word, &form) || !hl_a64_atomic_host_supports() ||
      (form.target != 31u && stolen(form.target)))
    return 0;
  hl_a64_stub_prologue(assembler);
  if (!hl_a64_atomic_body(assembler, word, pc, &guard, NULL))
    return 0;
  hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
  hl_a64_guard_finish(assembler, &guard);
  return hl_a64_assembler_ok(assembler);
}
