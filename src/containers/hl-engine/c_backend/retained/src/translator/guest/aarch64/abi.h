// translator/guest/aarch64/abi.h -- the aarch64 guest's implementation of the cpu interface that os/linux/ uses.
//
// THE CONTRACT. Every Linux-guest frontend provides these so the os/linux/ personality (syscalls,
// container, ELF) stays frontend-agnostic and is genuinely SHARED, not copied:
//   G_NR(c)            the CANONICAL syscall number  [rvalue]   (os/linux switches on the aarch64 ABI
//                                                                numbers; a frontend whose guest numbers
//                                                                differ maps them to canonical here)
//   G_A0(c)..G_A5(c)   the syscall argument registers [rvalue]
//   G_RET(c)           the syscall return register    [lvalue]
//
// Remaining per-arch seams are provided by the frontend when it wires into the
// shared service: struct layouts (stat/sigaction differ on aarch64 vs x86_64), the §B shadow
// stack (`ssp`, aarch64-only), TLS, and sigaltstack.
//
// aarch64 is the reference: syscall regs are x8 / x0..x5 / x0, and its ABI numbers ARE canonical.
#include "../../../linux_abi/number.h"
#define G_NR(c) hl_linux_syscall_number(HL_LINUX_GUEST_AARCH64, (c)->x[8])
#define G_A0(c) ((c)->x[0])
#define G_A1(c) ((c)->x[1])
#define G_A2(c) ((c)->x[2])
#define G_A3(c) ((c)->x[3])
#define G_A4(c) ((c)->x[4])
#define G_A5(c) ((c)->x[5])
#define G_RET(c) ((c)->x[0])

// seccomp seam: the NATIVE audit arch + the RAW guest syscall number the seccomp cBPF program is run
// against (os/linux/seccomp.c builds struct seccomp_data). aarch64's ABI numbers are already the native
// ones, so G_SECCOMP_NR is just x8. AUDIT_ARCH_AARCH64 = EM_AARCH64(183) | __AUDIT_ARCH_64BIT | _LE.
#define G_SECCOMP_ARCH 0xC00000B7u
#define G_SECCOMP_NR(c) ((int)(uint32_t)(c)->x[8])
// seccomp_data and the SIGSYS ucontext expose the architectural guest PC, never the engine's private
// high-map address used to execute non-PIE images. Linux enters seccomp after advancing ELR past SVC.
#define G_SECCOMP_IP(c) pcrel_base((c)->pc + 4)

// Engine seam: the shared jit/cache.c hashes the guest PC as (gpc >> G_GPC_HASH_SHIFT). aarch64 PCs are
// 4-byte aligned, so shifting out the low 2 bits spreads the map. (Pure tuning constant; value 2 is the
// historical aarch64 hash, so this keeps the aarch64 engine bit-identical.)
#define G_GPC_HASH_SHIFT 2

// Engine seam (engine-dedup PR2): the shared jit/dispatch.c run_guest() loop calls four hooks at the spots
// where the dispatcher diverges per guest arch. The aarch64 definitions expand to EXACTLY the code that was
// inline before, so the aarch64 engine stays bit-identical; the x86 frontend defines its own in a later PR.
//
// The seam is per (guest ISA, HOST CPU): dispatch.h patches AArch64 branch encodings and assumes guest
// registers live in the matching host registers, so it is the AArch64-host arm. Everything ABOVE this line
// is pure guest ABI, shared by both.
#include "../../../host/host_cpu.h"
#if defined(HL_HOST_CPU_AARCH64)
#include "dispatch.h"
#else
#include "interp_dispatch.h"
#endif

// PC / SP / TLS / §B shadow-stack reset — the remaining per-arch cpu state os/linux touches.
#define G_PC(c) ((c)->pc)
#define G_SP(c) ((c)->sp)
#define G_TLS(c) ((c)->tls)
// x86 uses this to drop stale translations on munmap/MAP_FIXED/mremap of an executable VA. The aarch64
// engine keeps its own guest-`ic ivau` (smc_icflush) coherence model, so the shared-path seam is a no-op.
#define G_SMC_UNMAP(lo, hi) ((void)(lo), (void)(hi))
static void aarch64_smc_copyout(uint64_t, uint64_t);
#define G_SMC_COPYOUT(lo, hi) aarch64_smc_copyout((lo), (hi))
#define G_SHADOW_RESET(c) ((c)->ssp = 0) // reset the §B shadow stack (fork/exec); no-op on engines without it
/* A BUS-guard activation retires every pre-guard translation.  Clear cached
   host return PCs while all registered CPUs are at the dispatcher so none can
   return directly into a retired, unguarded arena. */
#define G_ACTIVATION_CLEAR_CPU(c) ((c)->ssp = 0)
#define G_ACTIVATION_CLEAR_GLOBAL() ((void)0)
static void aarch64_soft_filter_refresh(struct cpu *);
#define G_SOFT_TLB_REFRESH(c) aarch64_soft_filter_refresh(c)
/* Mapping publication retires logical-VMA backing only after this STW clear. */
#define G_SOFT_TLB_CLEAR(c)                                                                                            \
    do {                                                                                                               \
        (c)->soft_page = UINT64_MAX;                                                                                   \
        (c)->soft_protection = 0;                                                                                      \
        (c)->soft_span_bytes = 0;                                                                                      \
        (c)->soft_span_protection = 0;                                                                                 \
    } while (0)

// Child thread resume PC: aarch64 services a syscall with pc still at the SVC, so advance +4.
#define G_THREAD_RESUME(child, parent) ((child)->pc = (parent)->pc + 4)

// Clone/thread-start hook: the aarch64 frontend does not elide memory-ordering barriers, so no
// transition flush is needed. Evaluates to nonzero (success) so the shared clone path is byte-identical.
#define G_THREAD_START_FLUSH() 1
#define G_SHARED_MAP_BARRIERS() 1 /* aarch64 frontend elides no barriers */

// aarch64 guests already use canonical (*at) syscalls -> nothing to normalize.
#define G_NORMALIZE(c) 0

// Zero the integer register file (execve). aarch64 = x[31].
#define G_RESET_REGS(c) memset((c)->x, 0, sizeof(c)->x)

// uname(2) `machine` field — per guest ISA, so an aarch64 guest reports "aarch64".
#define G_UNAME_MACHINE "aarch64"

// brk policy: aarch64 grows a real brk heap (works on macOS).
#define G_BRK_GROWABLE 1

// Open-flag bits that DIFFER by guest arch (the high O_* group). aarch64 uses the asm-generic values; the
// service must test the GUEST's bit, not a hardcoded one -- e.g. aarch64 O_LARGEFILE (0x20000, which musl
// ORs into every open) is the SAME bit as x86's O_NOFOLLOW, so a hardcoded 0x20000 check turned every
// symlink open into ELOOP. (Low flags O_CREAT/EXCL/TRUNC/APPEND/NONBLOCK + O_CLOEXEC match across arches.)
#define G_O_DIRECTORY 0x4000 // asm-generic O_DIRECTORY = 0040000
#define G_O_NOFOLLOW 0x8000  // asm-generic O_NOFOLLOW  = 0100000
