// translator/guest/x86_64/abi.h -- the x86-64 guest's implementation of the cpu interface os/linux/ is written
// against (the contract documented in frontend/aarch64/abi.h). With this + sysmap.h + a per-arch
// fill_linux_stat, the x86 target can drop its own service.c/container.c and share os/linux/.
//
// x86-64 cpu: r[16] = rax,rcx,rdx,rbx,rsp,rbp,rsi,rdi,r8..r15. Linux x86-64 syscall ABI:
//   number = rax ; args = rdi,rsi,rdx,r10,r8,r9 ; return = rax.
#include "../../../linux_abi/number.h"
#include "legacy.h"

#define G_NR(c) hl_linux_syscall_number(HL_LINUX_GUEST_X86_64, (c)->r[0])
#define CANON_X86ONLY HL_LINUX_SYSCALL_X86_ONLY
#define G_A0(c) ((c)->r[7])  // rdi
#define G_A1(c) ((c)->r[6])  // rsi
#define G_A2(c) ((c)->r[2])  // rdx
#define G_A3(c) ((c)->r[10]) // r10
#define G_A4(c) ((c)->r[8])  // r8
#define G_A5(c) ((c)->r[9])  // r9
#define G_RET(c) ((c)->r[0]) // rax

// seccomp seam: the NATIVE audit arch + the RAW guest syscall number the seccomp cBPF program is run
// against (os/linux/seccomp.c). The filter expects the x86-64 syscall number (rax), not the engine's canonical
// mapping -- a guest's own filter is written against its ISA's numbers. AUDIT_ARCH_X86_64 = EM_X86_64(62)
// | __AUDIT_ARCH_64BIT | _LE = 0xC000003E.
#define G_SECCOMP_ARCH 0xC000003Eu
#define G_SECCOMP_NR(c) ((int)(uint32_t)(c)->r[0])
#define G_SECCOMP_IP(c) ((c)->rip)

// Engine seam: the shared jit/cache.c hashes the guest PC as (gpc >> G_GPC_HASH_SHIFT). x86 PCs are
// byte-granular, so do not shift (>>0) -- matches the original frontend/x86_64/cache.c hash.
#define G_GPC_HASH_SHIFT 0

#define G_PC(c) ((c)->rip)
#define G_SP(c) ((c)->r[4])     // rsp
#define G_TLS(c) ((c)->fs_base) // x86 TLS base (arch_prctl SET_FS)
// A JIT guest unmapped / remapped an executable VA range [lo,hi) -> drop stale cached translations for it
// (jit86_drop_range_translations, defined in translate.c). Expanded in the shared os/linux munmap / MAP_FIXED
// / mremap paths. Inert unless a JIT guest is present (g_rwx_guest). See the "stale translation after
// unmap/remap" bug. (aarch64 keeps its own smc_icflush model, so its seam is a no-op.)
#define G_SMC_UNMAP(lo, hi) jit86_drop_range_translations((lo), (hi))
/* Kernel copyout can modify bytes through a writable alias whose guest VA is
   different from an executable alias. Until alias ranges are enumerated,
   conservatively retire translated code whenever SMC tracking is armed. */
#define G_SMC_COPYOUT(lo, hi) jit86_store_alias_changed((lo), (hi) - (lo))
#define G_SHADOW_RESET(c) ((void)0) // no §B shadow stack on the x86 frontend
/* x86 has no per-CPU shadow stack, but its second-level indirect target cache
   also contains host code pointers and must not survive an arena rotation. */
#define G_ACTIVATION_CLEAR_CPU(c) ((void)(c))
#define G_ACTIVATION_CLEAR_GLOBAL() memset(g_xibtc, 0, sizeof g_xibtc)

// Child thread resume PC: x86 pre-advances rip past `syscall` before servicing, so the copy is correct.
#define G_THREAD_RESUME(child, parent) ((void)0)

// Clone/thread-start hook: flush the code cache on the single->multi-threaded transition so x86-TSO-
// barrier-elided (single-threaded) blocks are re-translated WITH barriers before any peer runs. Evaluates
// to nonzero on success, 0 on host W^X failure. See hl_x86_flush_for_thread_start (translate.c).
#define G_THREAD_START_FLUSH() hl_x86_flush_for_thread_start()
// Guest established a MAP_SHARED mapping: force x86-TSO barriers (a peer PROCESS can now observe us).
#define G_SHARED_MAP_BARRIERS() hl_x86_force_barriers_for_shared()

// Syscall normalization: x86 rewrites legacy syscalls to their *at form (frontend/x86_64/legacy.c).
#define G_NORMALIZE(c)                                                                                                 \
    hl_x86_legacy_normalize((c),                                                                                       \
                            &(hl_x86_legacy_context){g_nonpie_lo, g_nonpie_hi, g_nonpie_bias, legacy_time_seconds,     \
                                                     legacy_set_alarm, legacy_access_ok, NULL})
#define G_FORK_PRESERVE(c) hl_x86_legacy_restore_fork(c)
// The canonical dup3 handler serves x86 legacy dup2 too; this reports which one the guest issued.
#define G_IS_DUP2_COMPAT() hl_x86_legacy_is_dup2()

// Zero the integer register file (execve). x86 = r[16].
#define G_RESET_REGS(c) memset((c)->r, 0, sizeof(c)->r)

// uname(2) `machine` field — per guest ISA, so an x86-64 guest reports "x86_64" (not the host "aarch64").
#define G_UNAME_MACHINE "x86_64"

// brk policy: on the macOS VM x86 reports a fixed break so glibc uses its mmap allocator -- a brk heap the
// guest then mmap/mprotects cannot be split under the 16 KB host page (jit86 learned this the hard way). A
// native Linux host has matching page granularity and the pre-reserved brk arena grows purely logically
// (brk_cur bump into already-mapped RW memory), so a guest that calls brk/sbrk directly -- rather than
// through glibc's mmap fallback -- must observe real growth exactly like the aarch64 guest does.
#if defined(__linux__)
#define G_BRK_GROWABLE 1
#else
#define G_BRK_GROWABLE 0
#endif

// Open-flag bits that DIFFER by guest arch (the high O_* group): x86-64 has its own values for the
// O_DIRECTORY/O_NOFOLLOW group, distinct from aarch64's asm-generic ones (see frontend/aarch64/abi.h).
#define G_O_DIRECTORY 0x10000 // x86-64 O_DIRECTORY = 0200000
#define G_O_NOFOLLOW 0x20000  // x86-64 O_NOFOLLOW  = 0400000

// W5B tier-2: extra PROF line at exit_group (x86 engine only; aarch64 leaves G_PROF_EXTRA undefined).
// g_prof_t2 lives in the shared cache, while g_prof_t2fold lives in the independently compiled x86 glue state.
#define G_PROF_EXTRA                                                                                                   \
    do {                                                                                                               \
    } while (0)
