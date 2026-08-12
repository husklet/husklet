#ifndef HL_HOST_CPU_H
#define HL_HOST_CPU_H

/* Names the host-CPU axis, separately from the host OS (src/host/<os>/), so the
 * two compose. Prefer HL_HOST_CPU_* over compiler predefines: those are spelled
 * per compiler, and a bare `defined(__x86_64__)` says nothing about which OS's
 * context applies -- how an Apple-shaped `__ss.__rip` ended up under a plain
 * `#elif defined(__x86_64__)` in linux_abi/signal.c.  Distinct from
 * HL_GUEST_ISA_* (the ISA being run) and HL_HOST_ISA_* (translator/identity.h,
 * hashed into the persistent-cache key). */

#if defined(__aarch64__) || defined(__arm64__) || defined(_M_ARM64)
#define HL_HOST_CPU_AARCH64 1
#define HL_HOST_CPU_NAME "aarch64"
#elif defined(__x86_64__) || defined(__amd64__) || defined(_M_X64)
#define HL_HOST_CPU_X86_64 1
#define HL_HOST_CPU_NAME "x86_64"
#else
#error "hl engine has no host-CPU definition for this target"
#endif

/* HL_HOST_ISA_* values for code that must name the host ISA at preprocessor
 * time; pinned to the enum by the _Static_assert in translator/identity.c. */
#define HL_HOST_CPU_ISA_AARCH64 1
#define HL_HOST_CPU_ISA_X86_64 2

#if defined(HL_HOST_CPU_AARCH64)
#define HL_HOST_CPU_ISA HL_HOST_CPU_ISA_AARCH64
#else
#define HL_HOST_CPU_ISA HL_HOST_CPU_ISA_X86_64
#endif

#endif
