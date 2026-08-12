#ifndef HL_TRANSLATOR_GUEST_X86_64_REP_RUNTIME_H
#define HL_TRANSLATOR_GUEST_X86_64_REP_RUNTIME_H

#include <stddef.h>
#include <stdint.h>

/*
 * Host-neutral half of the `rep movs`/`rep stos` idiom: the C helpers emitted
 * code calls at RUN time, plus the engine hooks that configure them.  Split out
 * of lower/repstr.c because that file's other half is an ARM64 emitter: sharing
 * one object made engine_global_init's calls to the two setters below drag the
 * emitter into every link, which is why a non-AArch64 host needed 21 aborting
 * emitter stubs to get past the linker.
 */

/* Access classes for cpu->soft_required. */
enum { X86_SOFT_READ = 1u, X86_SOFT_WRITE = 2u };

struct cpu;

typedef void (*hl_x86_rep_store_commit_fn)(uint64_t guest, uint64_t size);
typedef int (*hl_x86_rep_store_observation_active_fn)(void);
typedef int (*hl_x86_rep_access_fn)(uint64_t guest, size_t size);
typedef int (*hl_x86_rep_access_special_fn)(uint64_t guest, size_t size, int write);

void hl_x86_rep_set_store_commit(hl_x86_rep_store_commit_fn callback, hl_x86_rep_store_observation_active_fn active);
void hl_x86_rep_set_access_validators(hl_x86_rep_access_fn readable, hl_x86_rep_access_fn writable,
                                      hl_x86_rep_access_special_fn special);

/* The bound validators, published because the string ops are not the only guest
 * accesses the engine performs outside a backend's fault pad: avx.c's do_avx /
 * do_sse3b run from the dispatch loop and must prove an address the same way.
 * Unbound (standalone translator, no engine under it) answers "valid" -- one
 * flat address space, which is what the decoder's unit tests assume. */
int hl_x86_guest_readable(uint64_t guest, size_t length);
int hl_x86_guest_writable(uint64_t guest, size_t length);

uint64_t hl_x86_rep_movs(void *destination, const void *source, uint64_t bytes, int width, int backward,
                         struct cpu *cpu, uint64_t rip);
uint64_t hl_x86_rep_stos(void *destination, uint64_t value, uint64_t count, int width, int backward, struct cpu *cpu,
                         uint64_t rip);

void hl_x86_count_rep_movs(void);
void hl_x86_count_rep_stos(void);

#endif
