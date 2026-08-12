// translator/guest/aarch64/interp.c -- an AArch64 decoder + executor, selected by the unity build on every
// host CPU that is NOT AArch64; an AArch64 host picks translate.c + stubs.c.
// translate.c transliterates guest instructions into host code, so it assumes the guest register file IS the
// host register file -- true only on the ARM64 diagonal. Substituting this is legal because core/dispatch.c's
// run_guest asks only for translate_block(pc) -> something callable, run_block(c, code) to call it, and
// c->reason/c->pc/all guest state in *c on return; block cache, linux_abi, stop-the-world and checkpoint are
// reused unchanged, and struct cpu keeps the JIT's layout (its sizeof is checked on restore). Extension
// points: interp_step (top-level decoder), interp_read_guest/interp_write_guest (every guest access),
// interp_block_ends (the pre-scan's terminator set), interp_undefined (the diagnostic exit every
// unimplemented class reaches). Performance is not a goal: every instruction is re-decoded.

#include <setjmp.h>
// Guest FP runs on the HOST FPU with guest FPCR.RMode projected onto host rounding control: <fenv.h> is that
// projection, <math.h> has fma -- the only correctly-rounded FMADD -- and sqrt.
#include <fenv.h>
#include <math.h>

#include "../../../guest_fetch.h"
#include "../../../identity.h"
#include "../../../digest.h"
#include "../../../../host/cpu.h"
#include "../../../../host/native_context.h" // ucontext_t: the fault path restores uc_sigmask by hand
#include "../../../../host/range.h"
#include "../../../../linux_abi/logical_vma.h"

// Engine-wide debug/identity state that stubs.c owns for the JIT; the trace hook, syscall tracer, /proc
// synthesis, ELF loader and checkpoint writer all read it, so this backend must own it too.
static int g_trace;       // G_TRACE_DUMP: per-block guest PC + register dump
static int g_systrace;    // syscall tracer
static int g_dbg_nochain; // suppress inter-block chaining so every block re-enters the dispatcher
static int g_dbg_gprdump; // dump all guest GPRs per block, for a register-value differential
// What /proc/self/exe must report; a literal, not NULL, so an early reader cannot deref it.
static const char *g_exe_path = "";

// Non-PIE geometry -- the one place a guest address is not a host address. Really defined (by load_elf) in
// elf.c + container/vfs.c, LATER in this unity TU; these tentative definitions merge with that one, as in
// translate.c. All 0 for PIE and static-PIE images.
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;

// Guest-signal delivery, really defined later in this unity TU by signal.c; used only by the trapping
// classes (UDF, BRK, HLT, HVC/SMC). raise_guest_signal owns the DISPOSITION (SIG_DFL -> guest_group_fatal,
// 128+signo with the core-dump flag); sigq_flush makes it THREAD-directed.
static void raise_guest_signal(struct cpu *c, int sig);
static void sigq_flush(int sig);

// PC-relative base for ADR/ADRP and BL/BLR's x30. A non-PIE ET_EXEC is mapped HIGH, but its baked absolute
// pointers are LOW and real code compares an ADR/ADRP result against them, so PC-relative VALUES use the LOW
// link address while control flow keeps the HIGH pc. Must stay identical to translate.c's pcrel_base:
// signal.c hands the guest a PC through this function.
static uint64_t pcrel_base(uint64_t gpc) {
    if (g_nonpie_lo && gpc >= g_nonpie_lo + g_nonpie_bias && gpc < g_nonpie_hi + g_nonpie_bias)
        return gpc - g_nonpie_bias;
    return gpc;
}

// Guest VA -> host VA, equal but for a non-PIE ET_EXEC's low link range, served at +g_nonpie_bias: the whole
// address translation layer. Mirrors hl_x86_guest_pointer.
static uint64_t interp_guest_pointer(uint64_t address) {
    return g_nonpie_lo && address >= g_nonpie_lo && address < g_nonpie_hi ? address + g_nonpie_bias : address;
}

// The fault model. struct cpu is authoritative at every instruction boundary, so nothing needs reconstructing
// as in the JIT; the handler only has to ABANDON the in-flight memcpy in the guest accessors, which would
// otherwise re-execute and fault forever. A marker is armed around every guest access, run_block sigsetjmps
// at its top (savemask = 0; interp_restore_handler_mask below owns the mask), the handler siglongjmps
// back. Sound because nothing in between holds a lock, and a load commits its destination only after the
// marked memcpy returns while a store reads its sources into locals first: the abandoned instruction made no
// partial architectural change and cpu->pc still names it.
static __thread struct cpu *g_interp_marker_cpu;  // the cpu whose run_block armed the marker
static __thread sigjmp_buf g_interp_marker_jmp;   // where interp_signal_resume goes
static __thread int g_interp_marker_armed;        // 1 while the buffer is valid
static __thread int g_interp_access_active;       // 1 while a guest access is in flight
static __thread uint64_t g_interp_access_address; // its effective guest address
static __thread uint64_t g_interp_access_bytes;   // its size
static __thread int g_interp_access_write;        // 1 for a store

// The past-EOF SIGBUS ledger. mem.c re-maps the past-EOF tail of a MAP_PRIVATE file mapping as anonymous
// zero, so that SIGBUS is owed by the TRANSLATOR, from mem.c's ledger via core/bus.h; interp_access_begin is
// the single chokepoint. The ledger holds HOST addresses (differing by g_nonpie_bias for a folded non-PIE
// access) and reports the FIRST past-EOF byte, not the access base.
static void interp_bus_ledger_check(uint64_t address, uint64_t bytes) {
    if (!jit_guest_bus_active()) return; // inert unless some guest maps a file past its end
    uint64_t host = interp_guest_pointer(address);
    uint64_t host_fault = jit_guest_bus_fault(host, bytes);
    if (host_fault == 0) return;
    struct cpu *cpu = g_interp_marker_cpu;
    if (cpu == NULL || !g_interp_marker_armed) return; // no landing pad: cannot happen from run_block
    uint64_t guest_fault = host_fault - (host - address);
    cpu->fault_addr = guest_fault;
    cpu->bus_ea = guest_fault;
    cpu->reason = R_BUS;
    g_interp_access_active = 0;
    // The route into the pad that owes no mask restore: no signal was raised, so the host mask is already
    // what run_block will resume on. (interp_signal_resume is the other route and it does owe one.)
    siglongjmp(g_interp_marker_jmp, 1);
}

static void interp_access_begin(uint64_t address, uint64_t bytes, int write) {
    interp_bus_ledger_check(address, bytes);
    g_interp_access_address = address;
    g_interp_access_bytes = bytes;
    g_interp_access_write = write;
    // Publish LAST: the handler reads this flag to decide the fault is a guest access, so the description
    // must already be readable when it turns true.
    __atomic_store_n(&g_interp_access_active, 1, __ATOMIC_RELEASE);
}

static void interp_access_end(void) {
    __atomic_store_n(&g_interp_access_active, 0, __ATOMIC_RELEASE);
}

// Called (via sigframe_capture_fault) from the host SIGSEGV/SIGBUS guard. 1 = the fault was inside a marked
// guest access and *c is already correct; 0 = an engine bug in our own C, which must reach the crash report
// rather than become a guest signal. The host PC, the JIT's discriminator, cannot separate the two here.
static int interp_signal_capture(struct cpu *c, void *ucontext) {
    (void)ucontext;
    if (c == NULL) return 0;
    if (!__atomic_load_n(&g_interp_access_active, __ATOMIC_ACQUIRE)) return 0;
    if (g_interp_marker_cpu != c) return 0; // a fault on another thread's cpu is not this thread's to own
    // si_addr is the HOST address: it differs for a folded non-PIE access, and some hosts are imprecise.
    c->fault_addr = g_interp_access_address;
    return 1;
}

// WHY THIS EXISTS, AND WHY IT IS NOT sigsetjmp(pad, 1).
//
// Leaving a host handler by long jump instead of returning means the kernel never runs rt_sigreturn, so the
// mask restore rt_sigreturn would have done is OWED BY US. Skip it and the fault signal stays blocked in this
// guest thread forever: the SECOND guest SIGSEGV never arrives, and the failure is silent and looks like a
// hang. The JIT backend has no such debt -- it rewrites the ucontext PC to block_return and RETURNS, so
// sigreturn restores the mask for it. This function is the interpreter's hand-rolled sigreturn.
//
// The mask to install is exactly ucontext->uc_sigmask: the kernel wrote the pre-signal mask there, and it is
// by definition the value sigreturn would restore. That is stronger than snapshotting the run loop's mask,
// because it assumes NO invariance -- it stays correct across a nested guest signal, an SA_ONSTACK handler,
// a guest rt_sigprocmask that mirrored SIGTSTP/SIGTTIN/SIGTTOU onto the real host mask (syscall/signal.c
// case 135), a clone/fork child that inherited a different mask, and a host-service window that blocked
// signals around a write (host/linux/host.c). Restoring BEFORE the jump matches glibc's own siglongjmp
// ordering (mask first, __longjmp second).
//
// The savemask=1 this replaces cost an rt_sigprocmask syscall on EVERY guest basic block -- 271.7 ns, 44% of
// compute CPU and 99.96% of the process's host syscalls -- to
// save a mask that only this rare path ever reads.
static void interp_restore_handler_mask(void *ucontext) {
    if (ucontext != NULL) {
        pthread_sigmask(SIG_SETMASK, &((ucontext_t *)ucontext)->uc_sigmask, NULL);
        return;
    }
    // No context (no caller does this today: deliver_guest_fault_hint rejects a NULL ucontext up front).
    // Unblock the classes the kernel could have auto-blocked at handler entry, so a repeat fault is still
    // deliverable -- the failure mode this guards is exactly the silent one described above.
    sigset_t fault;
    sigemptyset(&fault);
    sigaddset(&fault, SIGSEGV);
    sigaddset(&fault, SIGBUS);
    sigaddset(&fault, SIGILL);
    sigaddset(&fault, SIGFPE);
    sigaddset(&fault, SIGTRAP);
    pthread_sigmask(SIG_UNBLOCK, &fault, NULL);
}

// Called (via sigframe_resume_dispatch) once the handler has set c->sync_signal/sync_code/tpending/reason.
static void interp_signal_resume(struct cpu *c, void *ucontext) {
    if (!g_interp_marker_armed || g_interp_marker_cpu != c) {
        // Unreachable via capture-then-resume; returning re-faults visibly, better than a stale-buffer jump.
        // Returning also hands the mask back to rt_sigreturn, so nothing is owed on this path.
        return;
    }
    interp_restore_handler_mask(ucontext);
    siglongjmp(g_interp_marker_jmp, 1);
}

// AArch64 permits unaligned access and real guests rely on it, so memcpy rather than cast-and-deref: a host
// that traps unaligned would fault where the guest expects success, and a misaligned deref is UB anyway.
#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error "the aarch64 interpreter backend assumes a little-endian host"
#endif

// ONE host access per guest access. With a RUNTIME size, memcpy is a CALL into glibc, whose 4..7 and 8..15
// paths copy head and tail separately -- `mov %ecx,(%rdi)` then `mov %esi,-4(%rdi,%rdx)`, THE SAME ADDRESS
// at n == 4, and likewise the 8-byte pair at n == 8. A guest store therefore landed TWICE, and a value a
// peer guest thread committed to that word between the two writes was resurrected by the second: measured
// directly on the host, ~1e-4 of the CASes that raced such a copy were silently undone. Packed structs, not
// casts: a guest access need not be aligned, and this must stay one instruction at every -O level, not only
// where the optimiser happens to inline a constant-size memcpy.
struct interp_una8 {
    uint8_t v;
} __attribute__((packed));

struct interp_una16 {
    uint16_t v;
} __attribute__((packed));

struct interp_una32 {
    uint32_t v;
} __attribute__((packed));

struct interp_una64 {
    uint64_t v;
} __attribute__((packed));

static void interp_copy_indivisible(void *destination, const void *source, unsigned bytes) {
    switch (bytes) {
    case 1: *(struct interp_una8 *)destination = *(const struct interp_una8 *)source; return;
    case 2: *(struct interp_una16 *)destination = *(const struct interp_una16 *)source; return;
    case 4: *(struct interp_una32 *)destination = *(const struct interp_una32 *)source; return;
    case 8: *(struct interp_una64 *)destination = *(const struct interp_una64 *)source; return;
    default: memcpy(destination, source, bytes); return; // 3/5/6/7: no guest access is this width
    }
}

static int interp_read_guest(uint64_t address, void *destination, unsigned bytes) {
    uint64_t host = interp_guest_pointer(address);
    interp_access_begin(address, bytes, 0);
    interp_copy_indivisible(destination, (const void *)(uintptr_t)host, bytes);
    interp_access_end();
    return 1;
}

static int interp_write_guest(uint64_t address, const void *source, unsigned bytes) {
    uint64_t host = interp_guest_pointer(address);
    interp_access_begin(address, bytes, 1);
    interp_copy_indivisible((void *)(uintptr_t)host, source, bytes);
    interp_access_end();
    return 1;
}

static uint64_t interp_load_bits(uint64_t address, unsigned bytes) {
    uint64_t value = 0;
    interp_read_guest(address, &value, bytes);
    return value;
}

static void interp_store_bits(uint64_t address, uint64_t value, unsigned bytes) {
    interp_write_guest(address, &value, bytes);
}

// The host pointer an atomic RMW works on directly. An unaligned atomic or exclusive access is a guest
// fault, so NULL lets the caller raise it rather than invoke a misaligned host atomic, which on some hosts
// silently loses atomicity.
// A vector load/store of 1..16 bytes on the fault-marked path; a short load zeroes the rest of the register.
static void interp_vec_load(struct cpu *cpu, int reg, uint64_t address, unsigned bytes);
static void interp_vec_store(struct cpu *cpu, int reg, uint64_t address, unsigned bytes);

static void *interp_atomic_pointer(uint64_t address, unsigned bytes) {
    uint64_t host = interp_guest_pointer(address);
    if (host & (bytes - 1u)) return NULL;
    return (void *)(uintptr_t)host;
}

// Register 31 is XZR in most encodings and SP in a few (add/sub immediate and extended-register, and a
// non-flag-setting logical immediate's destination): a silent wrong-answer bug, hence two named accessors.
static uint64_t interp_gpr(const struct cpu *cpu, int reg) {
    return reg == 31 ? UINT64_C(0) : cpu->x[reg];
}

static uint64_t interp_gpr_sp(const struct cpu *cpu, int reg) {
    return reg == 31 ? cpu->sp : cpu->x[reg];
}

static void interp_set_gpr(struct cpu *cpu, int reg, uint64_t value) {
    if (reg != 31) cpu->x[reg] = value;
}

static void interp_set_gpr_sp(struct cpu *cpu, int reg, uint64_t value) {
    if (reg == 31)
        cpu->sp = value;
    else
        cpu->x[reg] = value;
}

// A 32-bit result always zero-extends into the 64-bit register; every 32-bit destination goes through here.
static void interp_set_gpr32(struct cpu *cpu, int reg, uint32_t value) {
    interp_set_gpr(cpu, reg, (uint64_t)value);
}

static void interp_set_gpr32_sp(struct cpu *cpu, int reg, uint32_t value) {
    interp_set_gpr_sp(cpu, reg, (uint64_t)value);
}

// cpu->nzcv holds the flags EXACTLY as `mrs Xt, nzcv` reads them: N at 31, Z at 30, C at 29, V at 28, rest
// zero -- the JIT's form, and what signal.c stores into the sigcontext pstate word at mc+272 and reads back.
#define INTERP_NZCV_N (UINT64_C(1) << 31)
#define INTERP_NZCV_Z (UINT64_C(1) << 30)
#define INTERP_NZCV_C (UINT64_C(1) << 29)
#define INTERP_NZCV_V (UINT64_C(1) << 28)

static void interp_set_flags(struct cpu *cpu, unsigned n, unsigned z, unsigned c, unsigned v) {
    cpu->nzcv = (n ? INTERP_NZCV_N : 0) | (z ? INTERP_NZCV_Z : 0) | (c ? INTERP_NZCV_C : 0) | (v ? INTERP_NZCV_V : 0);
}

// FPCR/FPSR. Per-thread, and NOT in struct cpu -- that layout is the checkpoint format shared with the JIT,
// which does not model them. Guest-visible by MRS/MSR and by signal.c's FPSIMD record; the two must agree.
static __thread uint64_t g_interp_fpcr;
static __thread uint64_t g_interp_fpsr;

// FPCR fields; only these are modelled.
#define INTERP_FPCR_FZ16(f) (((f) >> 19) & 1u)  // flush-to-zero for HALF-precision operations
#define INTERP_FPCR_RMODE(f) (((f) >> 22) & 3u) // 00 nearest-even, 01 +inf, 10 -inf, 11 zero
#define INTERP_FPCR_FZ(f) (((f) >> 24) & 1u)    // flush-to-zero for single/double
#define INTERP_FPCR_DN(f) (((f) >> 25) & 1u)    // default-NaN mode
#define INTERP_FPCR_AHP(f) (((f) >> 26) & 1u)   // alternative (non-IEEE) half-precision format

// The bits MSR FPCR may set. The rest -- notably the trap-enable bits at [15:8] -- reads back zero, the
// architectural spelling of "no trapped FP exceptions": nothing here traps.
#define INTERP_FPCR_WRITABLE                                                                                           \
    ((UINT64_C(1) << 19) | (UINT64_C(3) << 22) | (UINT64_C(1) << 24) | (UINT64_C(1) << 25) | (UINT64_C(1) << 26))

// FPSR cumulative-exception bits, at their architectural positions.
#define INTERP_FPSR_IOC 0x01u      // invalid operation
#define INTERP_FPSR_DZC 0x02u      // divide by zero
#define INTERP_FPSR_OFC 0x04u      // overflow
#define INTERP_FPSR_UFC 0x08u      // underflow
#define INTERP_FPSR_IXC 0x10u      // inexact
#define INTERP_FPSR_IDC 0x80u      // input denormal: only when FPCR.FZ flushed one
#define INTERP_FPSR_QC 0x08000000u // AdvSIMD saturation, not an IEEE exception: set by saturating integer ops
#define INTERP_FPSR_WRITABLE (0x9Fu | INTERP_FPSR_QC)

static void interp_fpsr_raise(unsigned bits) {
    g_interp_fpsr |= bits;
}

static unsigned interp_flag_n(const struct cpu *cpu) {
    return (cpu->nzcv & INTERP_NZCV_N) != 0;
}

static unsigned interp_flag_z(const struct cpu *cpu) {
    return (cpu->nzcv & INTERP_NZCV_Z) != 0;
}

static unsigned interp_flag_c(const struct cpu *cpu) {
    return (cpu->nzcv & INTERP_NZCV_C) != 0;
}

static unsigned interp_flag_v(const struct cpu *cpu) {
    return (cpu->nzcv & INTERP_NZCV_V) != 0;
}

// ConditionHolds(). The low bit inverts the test, except for 0b1111 (NV), which is AL, not "never".
static int interp_cond_holds(const struct cpu *cpu, unsigned cond) {
    unsigned n = interp_flag_n(cpu), z = interp_flag_z(cpu), c = interp_flag_c(cpu), v = interp_flag_v(cpu);
    int result;
    switch ((cond >> 1) & 7) {
    case 0: result = z; break;              // EQ / NE
    case 1: result = c; break;              // CS(HS) / CC(LO)
    case 2: result = n; break;              // MI / PL
    case 3: result = v; break;              // VS / VC
    case 4: result = c && !z; break;        // HI / LS
    case 5: result = (n == v); break;       // GE / LT
    case 6: result = (n == v) && !z; break; // GT / LE
    default: result = 1; break;             // AL / NV
    }
    if ((cond & 1) && (cond & 0xE) != 0xE) result = !result;
    return result;
}

// AddWithCarry(). Subtraction is AddWithCarry(a, ~b, 1), which is why SUBS leaves C set on "no borrow".
static uint64_t interp_add_with_carry64(uint64_t a, uint64_t b, unsigned carry_in, struct cpu *cpu, int set) {
    uint64_t partial, result;
    int carry_a = __builtin_add_overflow(a, b, &partial);
    int carry_b = __builtin_add_overflow(partial, (uint64_t)carry_in, &result);
    if (set)
        interp_set_flags(cpu, (result >> 63) & 1, result == 0, (unsigned)(carry_a | carry_b),
                         (unsigned)(((a ^ result) & (b ^ result)) >> 63) & 1u);
    return result;
}

static uint32_t interp_add_with_carry32(uint32_t a, uint32_t b, unsigned carry_in, struct cpu *cpu, int set) {
    uint32_t partial, result;
    int carry_a = __builtin_add_overflow(a, b, &partial);
    int carry_b = __builtin_add_overflow(partial, (uint32_t)carry_in, &result);
    if (set)
        interp_set_flags(cpu, (result >> 31) & 1, result == 0, (unsigned)(carry_a | carry_b),
                         (unsigned)(((a ^ result) & (b ^ result)) >> 31) & 1u);
    return result;
}

// Logical operations set N and Z from the result and always CLEAR C and V.
static void interp_set_logical_flags(struct cpu *cpu, uint64_t result, unsigned sf) {
    unsigned negative = sf ? (unsigned)((result >> 63) & 1) : (unsigned)((result >> 31) & 1);
    uint64_t masked = sf ? result : (uint32_t)result;
    interp_set_flags(cpu, negative, masked == 0, 0, 0);
}

static int64_t interp_sext(uint64_t value, unsigned bits) {
    return (int64_t)(value << (64 - bits)) >> (64 - bits);
}

static uint64_t interp_ror64(uint64_t value, unsigned amount) {
    amount &= 63;
    return amount ? ((value >> amount) | (value << (64 - amount))) : value;
}

// CRC32/CRC32C in the architecture's REFLECTED form, which this table-free loop and zlib/ACLE `__crc32*`
// agree on. The constants reflect 0x04C11DB7 and 0x1EDC6F41 (Castagnoli). The data operand is consumed LOW
// BYTE FIRST; high byte first gives checksums that never match, with no other symptom.
static uint64_t interp_crc32(uint32_t accumulator, uint64_t data, unsigned bytes, int castagnoli) {
    uint32_t polynomial = castagnoli ? 0x82F63B78u : 0xEDB88320u;
    for (unsigned index = 0; index < bytes; index++) {
        accumulator ^= (uint32_t)((data >> (8u * index)) & 0xFFu);
        for (unsigned bit = 0; bit < 8u; bit++)
            accumulator = (accumulator >> 1) ^ (polynomial & (uint32_t)(-(int32_t)(accumulator & 1u)));
    }
    return (uint64_t)accumulator;
}

static uint32_t interp_ror32(uint32_t value, unsigned amount) {
    amount &= 31;
    return amount ? ((value >> amount) | (value << (32 - amount))) : value;
}

// DecodeBitMasks(). immediate = 1 (logical-immediate group) forbids the all-ones element, 0 (bitfield group)
// permits it. Returns 0 for an encoding the architecture leaves UNDEFINED, so the caller can refuse it.
static int interp_bit_masks(unsigned sf, unsigned immn, unsigned imms, unsigned immr, int immediate,
                            uint64_t *wmask_out, uint64_t *tmask_out) {
    uint32_t combined = (immn << 6) | ((~imms) & 0x3Fu); // N : NOT(imms)
    int length = -1;
    for (int bit = 6; bit >= 0; --bit)
        if (combined & (1u << bit)) {
            length = bit;
            break;
        }
    if (length < 1) return 0;
    if (!sf && immn) return 0; // a 64-bit-only element size in a 32-bit instruction
    unsigned levels = (1u << (unsigned)length) - 1u;
    if (immediate && (imms & levels) == levels) return 0;
    unsigned s = imms & levels, r = immr & levels;
    unsigned diff = (s - r) & levels;
    unsigned esize = 1u << (unsigned)length;
    uint64_t element_mask = esize == 64 ? UINT64_MAX : ((UINT64_C(1) << esize) - 1u);
    uint64_t welem = s + 1u >= 64u ? UINT64_MAX : ((UINT64_C(1) << (s + 1u)) - 1u);
    uint64_t telem = diff + 1u >= 64u ? UINT64_MAX : ((UINT64_C(1) << (diff + 1u)) - 1u);
    uint64_t rotated = welem & element_mask;
    if (r) rotated = ((rotated >> r) | (rotated << (esize - r))) & element_mask;
    uint64_t wmask = 0, tmask = 0;
    for (unsigned offset = 0; offset < 64; offset += esize) {
        wmask |= rotated << offset;
        tmask |= (telem & element_mask) << offset;
    }
    if (wmask_out) *wmask_out = wmask;
    if (tmask_out) *tmask_out = tmask;
    return 1;
}

// A trap the ARCHITECTURE defines -- UDF and the RESERVED group, BRK, HLT, HVC/SMC/DCPS -- not a gap in this
// backend: a guest event the program may well survive, where interp_undefined below must stop the run; every
// decode site picks one of the two deliberately. Not plain raise_guest_signal(), the mask-honouring SI_USER
// route: Linux delivers a fault-class signal with full siginfo and FORCES it past the mask, since a
// synchronous fault whose handler never runs re-executes forever. Hence the sync siginfo fields, the
// unmasking, and sigq_flush to make the queued instance THREAD-directed -- else another thread could claim it
// AND this one would deliver it again via tpending. Gap: under SIG_IGN Linux kills the process while this
// drops the signal and loops (needs a raise_guest_sync_signal() in signal.c).
static void interp_raise_sync_signal(struct cpu *cpu, int signo, int signal_code, uint64_t fault_address) {
    cpu->sync_signal = signo;
    cpu->sync_code = signal_code;
    cpu->sync_address = fault_address;
    cpu->sigmask &= ~(1ull << (signo - 1)); // forced: delivered even if the guest blocked it
    raise_guest_signal(cpu, signo);         // owns the disposition; does not return for SIG_DFL
    sigq_flush(signo);                      // drop the process-directed instance it queued
    __atomic_or_fetch(&cpu->tpending, 1ull << signo, __ATOMIC_SEQ_CST);
    // cpu->pc is left ON the trapping instruction: what the guest's frame must name, and what makes a
    // handler that returns without advancing re-take the trap, as Linux does here.
    cpu->reason = R_BRANCH;
}

// Backend gaps are fatal; architecturally undefined guest instructions use the signal path instead.
static int interp_undefined(struct cpu *cpu, uint32_t insn, const char *class_name) {
    char message[320];
    int written = snprintf(message, sizeof message,
                           "[interp] TODO(amd64-host) unimplemented aarch64 encoding 0x%08x at guest pc 0x%llx "
                           "class=\"%s\" op0=0x%x sf=%u Rd=%u Rn=%u Rm=%u Rt2=%u",
                           insn, (unsigned long long)cpu->pc, class_name, (unsigned)((insn >> 25) & 0xF),
                           (unsigned)((insn >> 31) & 1), (unsigned)(insn & 31), (unsigned)((insn >> 5) & 31),
                           (unsigned)((insn >> 16) & 31), (unsigned)((insn >> 10) & 31));
    if (written < 0) written = 0;
    if ((size_t)written >= sizeof message) written = (int)sizeof message - 1;
    (void)jit_fail(HL_STATUS_NOT_SUPPORTED, message, (size_t)written);
    // Leave cpu->pc ON the offending instruction so message and state agree; the dispatcher's fatal check
    // ends the run next iteration, and R_BRANCH is the only exit that does not misread it.
    cpu->reason = R_BRANCH;
    return 1;
}

// Decode and execute.
#define INTERP_NEXT 0 // instruction done, cpu->pc advanced; continue the block
#define INTERP_END 1  // block ends here; cpu->reason and cpu->pc are final

// Data processing -- immediate; sub-class from insn[25:23].
static int interp_exec_dp_immediate(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned group = (insn >> 23) & 7;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31);

    switch (group) {
    case 0:
    case 1: { // PC-relative addressing: ADR / ADRP
        int64_t immediate = interp_sext((((insn >> 5) & 0x7FFFFu) << 2) | ((insn >> 29) & 3u), 21);
        uint64_t value;
        if (insn & 0x80000000u) // ADRP: page base, immediate scaled by 4 KiB
            value = (pcrel_base(gpc) & ~UINT64_C(0xFFF)) + ((uint64_t)immediate << 12);
        else
            value = pcrel_base(gpc) + (uint64_t)immediate;
        interp_set_gpr(cpu, rd, value);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 2: { // Add/subtract (immediate)
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1, shift = (insn >> 22) & 1;
        uint64_t immediate = (insn >> 10) & 0xFFFu;
        if (shift) immediate <<= 12;
        // Rn is <Xn|SP>; Rd is <Xd|SP> for ADD/SUB but XZR for ADDS/SUBS, so `cmp` discards its result.
        if (sf) {
            uint64_t a = interp_gpr_sp(cpu, rn);
            uint64_t result = op ? interp_add_with_carry64(a, ~immediate, 1, cpu, (int)setflags)
                                 : interp_add_with_carry64(a, immediate, 0, cpu, (int)setflags);
            if (setflags)
                interp_set_gpr(cpu, rd, result);
            else
                interp_set_gpr_sp(cpu, rd, result);
        } else {
            uint32_t a = (uint32_t)interp_gpr_sp(cpu, rn);
            uint32_t result = op ? interp_add_with_carry32(a, ~(uint32_t)immediate, 1, cpu, (int)setflags)
                                 : interp_add_with_carry32(a, (uint32_t)immediate, 0, cpu, (int)setflags);
            if (setflags)
                interp_set_gpr32(cpu, rd, result);
            else
                interp_set_gpr32_sp(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 3: // Add/subtract (immediate, with tags): MTE
        return interp_undefined(cpu, insn, "data-processing immediate -- ADDG/SUBG (memory tagging)");
    case 4: { // Logical (immediate)
        unsigned opc = (insn >> 29) & 3, immn = (insn >> 22) & 1;
        unsigned immr = (insn >> 16) & 0x3Fu, imms = (insn >> 10) & 0x3Fu;
        uint64_t wmask;
        if (!interp_bit_masks(sf, immn, imms, immr, 1, &wmask, NULL))
            return interp_undefined(cpu, insn, "data-processing immediate -- undefined logical-immediate mask");
        uint64_t operand = interp_gpr(cpu, rn), result;
        switch (opc) {
        case 0: result = operand & wmask; break;  // AND
        case 1: result = operand | wmask; break;  // ORR
        case 2: result = operand ^ wmask; break;  // EOR
        default: result = operand & wmask; break; // ANDS
        }
        if (!sf) result = (uint32_t)result;
        if (opc == 3) { // ANDS: flag-setting, so Rd is XZR when 31
            interp_set_logical_flags(cpu, result, sf);
            if (sf)
                interp_set_gpr(cpu, rd, result);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)result);
        } else { // AND/ORR/EOR: Rd is <Xd|SP>
            if (sf)
                interp_set_gpr_sp(cpu, rd, result);
            else
                interp_set_gpr32_sp(cpu, rd, (uint32_t)result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 5: { // Move wide (immediate): MOVN / MOVZ / MOVK
        unsigned opc = (insn >> 29) & 3, hw = (insn >> 21) & 3;
        uint64_t imm16 = (insn >> 5) & 0xFFFFu;
        if (opc == 1) return interp_undefined(cpu, insn, "data-processing immediate -- unallocated move-wide opc");
        if (!sf && (hw & 2)) return interp_undefined(cpu, insn, "data-processing immediate -- 32-bit move-wide hw>1");
        unsigned shift = hw * 16u;
        uint64_t field = imm16 << shift;
        uint64_t result;
        if (opc == 0)
            result = ~field; // MOVN
        else if (opc == 2)
            result = field; // MOVZ
        else                // MOVK: keep the other halfwords
            result = (interp_gpr(cpu, rd) & ~(UINT64_C(0xFFFF) << shift)) | field;
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 6: { // Bitfield: SBFM / BFM / UBFM and every alias
        unsigned opc = (insn >> 29) & 3, immn = (insn >> 22) & 1;
        unsigned immr = (insn >> 16) & 0x3Fu, imms = (insn >> 10) & 0x3Fu;
        uint64_t wmask, tmask;
        if (opc == 3 || immn != sf)
            return interp_undefined(cpu, insn, "data-processing immediate -- unallocated bitfield encoding");
        if (!interp_bit_masks(sf, immn, imms, immr, 0, &wmask, &tmask))
            return interp_undefined(cpu, insn, "data-processing immediate -- undefined bitfield mask");
        uint64_t source = interp_gpr(cpu, rn);
        uint64_t rotated = sf ? interp_ror64(source, immr) : (uint64_t)interp_ror32((uint32_t)source, immr);
        uint64_t result;
        if (opc == 1) { // BFM: keep Rd's bits outside the field
            uint64_t destination = interp_gpr(cpu, rd);
            uint64_t bottom = (destination & ~wmask) | (rotated & wmask);
            result = (destination & ~tmask) | (bottom & tmask);
        } else {
            uint64_t bottom = rotated & wmask;
            // SBFM replicates bit S of the source above the field; UBFM zeroes it.
            uint64_t top = (opc == 0 && ((source >> imms) & 1)) ? UINT64_MAX : UINT64_C(0);
            result = (top & ~tmask) | (bottom & tmask);
        }
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    default: { // 7: Extract -- EXTR (ROR when Rn == Rm)
        unsigned immn = (insn >> 22) & 1, imms = (insn >> 10) & 0x3Fu;
        int rm = (int)((insn >> 16) & 31);
        if (((insn >> 29) & 3) != 0 || ((insn >> 21) & 1) != 0 || immn != sf)
            return interp_undefined(cpu, insn, "data-processing immediate -- unallocated extract encoding");
        if (!sf && (imms & 0x20u))
            return interp_undefined(cpu, insn, "data-processing immediate -- 32-bit EXTR lsb>31");
        uint64_t high = interp_gpr(cpu, rn), low = interp_gpr(cpu, rm);
        // Result is the low datasize bits of (Rn:Rm) >> lsb, so the low half comes from Rm.
        if (sf) {
            uint64_t result = imms ? ((low >> imms) | (high << (64 - imms))) : low;
            interp_set_gpr(cpu, rd, result);
        } else {
            uint32_t result = imms ? (((uint32_t)low >> imms) | ((uint32_t)high << (32 - imms))) : (uint32_t)low;
            interp_set_gpr32(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    }
}

// ShiftReg(). `amount` is already masked to the operand size by the encoding.
static uint64_t interp_shift_operand(uint64_t value, unsigned shift_type, unsigned amount, unsigned sf) {
    if (sf) {
        switch (shift_type) {
        case 0: return amount ? (value << amount) : value;                               // LSL
        case 1: return amount ? (value >> amount) : value;                               // LSR
        case 2: return (uint64_t)(amount ? ((int64_t)value >> amount) : (int64_t)value); // ASR
        default: return interp_ror64(value, amount);                                     // ROR
        }
    }
    uint32_t narrow = (uint32_t)value;
    switch (shift_type) {
    case 0: return (uint64_t)(uint32_t)(amount ? (narrow << amount) : narrow);
    case 1: return (uint64_t)(uint32_t)(amount ? (narrow >> amount) : narrow);
    case 2: return (uint64_t)(uint32_t)(amount ? (uint32_t)((int32_t)narrow >> amount) : narrow);
    default: return (uint64_t)interp_ror32(narrow, amount);
    }
}

// ExtendReg(): Rm sign/zero-extended per `option`, then shifted. UXTX/SXTX read the whole register.
static uint64_t interp_extend_operand(const struct cpu *cpu, int rm, unsigned option, unsigned shift, unsigned sf) {
    uint64_t value = interp_gpr(cpu, rm);
    uint64_t extended;
    switch (option) {
    case 0: extended = (uint8_t)value; break;                    // UXTB
    case 1: extended = (uint16_t)value; break;                   // UXTH
    case 2: extended = (uint32_t)value; break;                   // UXTW
    case 3: extended = value; break;                             // UXTX
    case 4: extended = (uint64_t)(int64_t)(int8_t)value; break;  // SXTB
    case 5: extended = (uint64_t)(int64_t)(int16_t)value; break; // SXTH
    case 6: extended = (uint64_t)(int64_t)(int32_t)value; break; // SXTW
    default: extended = value; break;                            // SXTX
    }
    extended <<= shift;
    return sf ? extended : (uint64_t)(uint32_t)extended;
}

// Add, logical, and multiply register forms.
static int interp_exec_dp_register_arithmetic(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);

    // Add/subtract, shifted and extended register; insn[21] separates them.
    if ((insn & 0x1F000000u) == 0x0B000000u) {
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1;
        uint64_t operand;
        int destination_is_sp;
        if (insn & 0x00200000u) { // extended register
            unsigned option = (insn >> 13) & 7, shift = (insn >> 10) & 7;
            if (shift > 4) return interp_undefined(cpu, insn, "data-processing register -- extend shift > 4");
            operand = interp_extend_operand(cpu, rm, option, shift, sf);
            destination_is_sp = 1; // Rn is <Xn|SP>; Rd too unless flag-setting
        } else {                   // shifted register
            unsigned shift_type = (insn >> 22) & 3, amount = (insn >> 10) & 0x3Fu;
            if (shift_type == 3) return interp_undefined(cpu, insn, "data-processing register -- add/sub ROR");
            if (!sf && (amount & 0x20u))
                return interp_undefined(cpu, insn, "data-processing register -- 32-bit add/sub shift > 31");
            operand = interp_shift_operand(interp_gpr(cpu, rm), shift_type, amount, sf);
            destination_is_sp = 0; // this form names no SP; 31 is XZR throughout
        }
        if (sf) {
            uint64_t a = destination_is_sp ? interp_gpr_sp(cpu, rn) : interp_gpr(cpu, rn);
            uint64_t result = op ? interp_add_with_carry64(a, ~operand, 1, cpu, (int)setflags)
                                 : interp_add_with_carry64(a, operand, 0, cpu, (int)setflags);
            if (destination_is_sp && !setflags)
                interp_set_gpr_sp(cpu, rd, result);
            else
                interp_set_gpr(cpu, rd, result);
        } else {
            uint32_t a = (uint32_t)(destination_is_sp ? interp_gpr_sp(cpu, rn) : interp_gpr(cpu, rn));
            uint32_t result = op ? interp_add_with_carry32(a, ~(uint32_t)operand, 1, cpu, (int)setflags)
                                 : interp_add_with_carry32(a, (uint32_t)operand, 0, cpu, (int)setflags);
            if (destination_is_sp && !setflags)
                interp_set_gpr32_sp(cpu, rd, result);
            else
                interp_set_gpr32(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Logical (shifted register). opc selects AND/ORR/EOR/ANDS and N inverts Rm.
    if ((insn & 0x1F000000u) == 0x0A000000u) {
        unsigned opc = (insn >> 29) & 3, shift_type = (insn >> 22) & 3, negate = (insn >> 21) & 1;
        unsigned amount = (insn >> 10) & 0x3Fu;
        if (!sf && (amount & 0x20u))
            return interp_undefined(cpu, insn, "data-processing register -- 32-bit logical shift > 31");
        uint64_t operand = interp_shift_operand(interp_gpr(cpu, rm), shift_type, amount, sf);
        if (negate) operand = sf ? ~operand : (uint64_t)(uint32_t)~(uint32_t)operand;
        uint64_t a = interp_gpr(cpu, rn), result;
        switch (opc) {
        case 0: result = a & operand; break;  // AND / BIC
        case 1: result = a | operand; break;  // ORR / ORN  (MOV reg is ORR with Rn == XZR)
        case 2: result = a ^ operand; break;  // EOR / EON
        default: result = a & operand; break; // ANDS / BICS
        }
        if (!sf) result = (uint32_t)result;
        if (opc == 3) interp_set_logical_flags(cpu, result, sf);
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x1F000000u) == 0x1B000000u) { // Data-processing (3 source)
        unsigned op31 = (insn >> 21) & 7, o0 = (insn >> 15) & 1;
        int ra = (int)((insn >> 10) & 31);
        uint64_t addend = interp_gpr(cpu, ra);
        switch (op31) {
        case 0: { // MADD / MSUB (MUL / MNEG with Ra == XZR)
            if (sf) {
                uint64_t product = interp_gpr(cpu, rn) * interp_gpr(cpu, rm);
                interp_set_gpr(cpu, rd, o0 ? addend - product : addend + product);
            } else {
                uint32_t product = (uint32_t)interp_gpr(cpu, rn) * (uint32_t)interp_gpr(cpu, rm);
                uint32_t base = (uint32_t)addend;
                interp_set_gpr32(cpu, rd, o0 ? base - product : base + product);
            }
            break;
        }
        case 1: { // SMADDL / SMSUBL (SMULL / SMNEGL with Ra == XZR)
            if (!sf) return interp_undefined(cpu, insn, "data-processing register -- 32-bit widening multiply");
            int64_t product = (int64_t)(int32_t)interp_gpr(cpu, rn) * (int64_t)(int32_t)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, o0 ? addend - (uint64_t)product : addend + (uint64_t)product);
            break;
        }
        case 2: { // SMULH
            if (!sf || o0) return interp_undefined(cpu, insn, "data-processing register -- unallocated SMULH form");
            __int128 product = (__int128)(int64_t)interp_gpr(cpu, rn) * (__int128)(int64_t)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, (uint64_t)(product >> 64));
            break;
        }
        case 5: { // UMADDL / UMSUBL (UMULL / UMNEGL with Ra == XZR)
            if (!sf) return interp_undefined(cpu, insn, "data-processing register -- 32-bit widening multiply");
            uint64_t product = (uint64_t)(uint32_t)interp_gpr(cpu, rn) * (uint64_t)(uint32_t)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, o0 ? addend - product : addend + product);
            break;
        }
        case 6: { // UMULH
            if (!sf || o0) return interp_undefined(cpu, insn, "data-processing register -- unallocated UMULH form");
            unsigned __int128 product = (unsigned __int128)interp_gpr(cpu, rn) * (unsigned __int128)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, (uint64_t)(product >> 64));
            break;
        }
        default: return interp_undefined(cpu, insn, "data-processing register -- unallocated 3-source op31");
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "data-processing register -- unallocated arithmetic encoding");
}

// Carry and conditional-compare forms, which update NZCV.
static int interp_exec_dp_register_flags(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);

    if ((insn & 0x1FE00000u) == 0x1A000000u) { // ADC / ADCS / SBC / SBCS
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1;
        if ((insn & 0x0000FC00u) != 0)
            return interp_undefined(cpu, insn, "data-processing register -- rotate/flag ops");
        unsigned carry = interp_flag_c(cpu);
        if (sf) {
            uint64_t a = interp_gpr(cpu, rn), b = interp_gpr(cpu, rm);
            uint64_t result = op ? interp_add_with_carry64(a, ~b, carry, cpu, (int)setflags)
                                 : interp_add_with_carry64(a, b, carry, cpu, (int)setflags);
            interp_set_gpr(cpu, rd, result);
        } else {
            uint32_t a = (uint32_t)interp_gpr(cpu, rn), b = (uint32_t)interp_gpr(cpu, rm);
            uint32_t result = op ? interp_add_with_carry32(a, ~b, carry, cpu, (int)setflags)
                                 : interp_add_with_carry32(a, b, carry, cpu, (int)setflags);
            interp_set_gpr32(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x1FE00000u) == 0x1A400000u) { // Conditional compare: CCMN / CCMP
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1;
        unsigned cond = (insn >> 12) & 0xFu, immediate_form = (insn >> 11) & 1;
        unsigned nzcv = insn & 0xFu;
        if (!setflags || ((insn >> 10) & 1) != 0 || ((insn >> 4) & 1) != 0)
            return interp_undefined(cpu, insn, "data-processing register -- unallocated conditional-compare form");
        if (!interp_cond_holds(cpu, cond)) {
            // Condition failed: NZCV is REPLACED by the encoded nzcv literal, not left alone.
            interp_set_flags(cpu, (nzcv >> 3) & 1, (nzcv >> 2) & 1, (nzcv >> 1) & 1, nzcv & 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        uint64_t operand = immediate_form ? (uint64_t)((insn >> 16) & 0x1Fu) : interp_gpr(cpu, rm);
        if (sf) {
            uint64_t a = interp_gpr(cpu, rn);
            if (op)
                (void)interp_add_with_carry64(a, ~operand, 1, cpu, 1); // CCMP
            else
                (void)interp_add_with_carry64(a, operand, 0, cpu, 1); // CCMN
        } else {
            uint32_t a = (uint32_t)interp_gpr(cpu, rn);
            if (op)
                (void)interp_add_with_carry32(a, ~(uint32_t)operand, 1, cpu, 1);
            else
                (void)interp_add_with_carry32(a, (uint32_t)operand, 0, cpu, 1);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "data-processing register -- unallocated flag encoding");
}

// Conditional selection and unary/binary register forms.
static int interp_exec_dp_register_select(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);

    if ((insn & 0x1FE00000u) == 0x1A800000u) { // CSEL / CSINC / CSINV / CSNEG
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1, op2 = (insn >> 10) & 3;
        if (setflags || (op2 & 2))
            return interp_undefined(cpu, insn, "data-processing register -- unallocated conditional-select form");
        unsigned cond = (insn >> 12) & 0xFu;
        uint64_t result;
        if (interp_cond_holds(cpu, cond)) {
            result = interp_gpr(cpu, rn);
        } else {
            uint64_t other = interp_gpr(cpu, rm);
            if (!op)
                result = op2 ? other + 1 : other; // CSINC : CSEL
            else
                result = op2 ? (uint64_t)0 - other : ~other; // CSNEG : CSINV
        }
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x1FE00000u) == 0x1AC00000u) { // Data-processing (1 source) and (2 source)
        if (insn & 0x40000000u) {              // insn[30] == 1: 1 source
            unsigned opcode2 = (insn >> 16) & 0x1Fu, opcode = (insn >> 10) & 0x3Fu;
            if (((insn >> 29) & 1) || opcode2 != 0)
                return interp_undefined(cpu, insn, "data-processing register -- PAC/1-source extension");
            uint64_t value = interp_gpr(cpu, rn), result;
            switch (opcode) {
            case 0: { // RBIT
                uint64_t wide = value;
                wide = ((wide & UINT64_C(0x5555555555555555)) << 1) | ((wide >> 1) & UINT64_C(0x5555555555555555));
                wide = ((wide & UINT64_C(0x3333333333333333)) << 2) | ((wide >> 2) & UINT64_C(0x3333333333333333));
                wide = ((wide & UINT64_C(0x0F0F0F0F0F0F0F0F)) << 4) | ((wide >> 4) & UINT64_C(0x0F0F0F0F0F0F0F0F));
                wide = __builtin_bswap64(wide);
                result = sf ? wide : (uint64_t)(uint32_t)(wide >> 32);
                break;
            }
            case 1: { // REV16: byte-swap within each halfword
                uint64_t wide = value;
                result = ((wide & UINT64_C(0x00FF00FF00FF00FF)) << 8) | ((wide >> 8) & UINT64_C(0x00FF00FF00FF00FF));
                break;
            }
            case 2: // REV (32-bit form) / REV32 (64-bit form)
                if (sf) {
                    uint64_t wide = value;
                    result =
                        ((wide & UINT64_C(0x000000FF000000FF)) << 24) | ((wide & UINT64_C(0x0000FF000000FF00)) << 8) |
                        ((wide >> 8) & UINT64_C(0x0000FF000000FF00)) | ((wide >> 24) & UINT64_C(0x000000FF000000FF));
                } else {
                    result = (uint64_t)__builtin_bswap32((uint32_t)value);
                }
                break;
            case 3: // REV (64-bit form)
                if (!sf) return interp_undefined(cpu, insn, "data-processing register -- 32-bit REV64");
                result = __builtin_bswap64(value);
                break;
            case 4: // CLZ
                if (sf)
                    result = value ? (uint64_t)__builtin_clzll(value) : 64u;
                else
                    result = (uint32_t)value ? (uint64_t)__builtin_clz((uint32_t)value) : 32u;
                break;
            case 5: { // CLS
                // CountLeadingSignBits is CLZ over bits [N-1:1] of (x ^ (x << 1)): the fold must be shifted
                // DOWN first, and the count is one less than a full-width CLZ. All-ones is the catching case.
                if (sf) {
                    uint64_t narrowed = (value ^ (value << 1)) >> 1;
                    result = narrowed ? (uint64_t)__builtin_clzll(narrowed) - 1u : 63u;
                } else {
                    uint32_t narrowed = (uint32_t)((uint32_t)value ^ ((uint32_t)value << 1)) >> 1;
                    result = narrowed ? (uint64_t)__builtin_clz(narrowed) - 1u : 31u;
                }
                break;
            }
            default: return interp_undefined(cpu, insn, "data-processing register -- unallocated 1-source opcode");
            }
            if (sf)
                interp_set_gpr(cpu, rd, result);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)result);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        unsigned opcode = (insn >> 10) & 0x3Fu;
        if ((insn >> 29) & 1) return interp_undefined(cpu, insn, "data-processing register -- flag-setting 2-source");
        uint64_t a = interp_gpr(cpu, rn), b = interp_gpr(cpu, rm), result;
        switch (opcode) {
        case 2: // UDIV: /0 yields 0, it does not trap
            if (sf)
                result = b ? a / b : 0;
            else
                result = (uint32_t)b ? (uint64_t)((uint32_t)a / (uint32_t)b) : 0;
            break;
        case 3: // SDIV: /0 yields 0, INT_MIN / -1 saturates to INT_MIN; neither traps
            if (sf) {
                int64_t x = (int64_t)a, y = (int64_t)b;
                result = y == 0 ? 0 : (y == -1 && x == INT64_MIN ? (uint64_t)x : (uint64_t)(x / y));
            } else {
                int32_t x = (int32_t)a, y = (int32_t)b;
                result = y == 0 ? 0 : (uint64_t)(uint32_t)(y == -1 && x == INT32_MIN ? x : x / y);
            }
            break;
        // The variable shifts mask their amount by the operand size, so LSLV by 64 is a no-op, not zero.
        case 8: result = interp_shift_operand(a, 0, (unsigned)(b & (sf ? 63u : 31u)), sf); break;  // LSLV
        case 9: result = interp_shift_operand(a, 1, (unsigned)(b & (sf ? 63u : 31u)), sf); break;  // LSRV
        case 10: result = interp_shift_operand(a, 2, (unsigned)(b & (sf ? 63u : 31u)), sf); break; // ASRV
        case 11:
            result = interp_shift_operand(a, 3, (unsigned)(b & (sf ? 63u : 31u)), sf);
            break; // RORV
        // CRC32B/H/W/X (10000..10011) and CRC32CB/H/W/X (10100..10111). sf names the DATA operand width
        // only, so it must be 1 for exactly the ..X forms; accumulator and result are always 32-bit.
        case 16:
        case 17:
        case 18:
        case 19:
        case 20:
        case 21:
        case 22:
        case 23: {
            unsigned data_bytes = 1u << (opcode & 3u);
            if ((data_bytes == 8) != (sf != 0))
                return interp_undefined(cpu, insn, "data-processing register -- CRC32 size/sf mismatch");
            result = interp_crc32((uint32_t)a, b, data_bytes, (opcode & 4u) != 0);
            // Always a W register, so not the sf-selected write below.
            interp_set_gpr32(cpu, rd, (uint32_t)result);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        default: return interp_undefined(cpu, insn, "data-processing register -- unallocated 2-source opcode");
        }
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "data-processing register -- unallocated selection encoding");
}

// Data processing -- register; dispatch sub-classes before executing one bounded encoding family.
static int interp_exec_dp_register(struct cpu *cpu, uint32_t insn) {
    uint32_t group = insn & 0x1FE00000u;
    if ((insn & 0x1F000000u) == 0x0B000000u || (insn & 0x1F000000u) == 0x0A000000u ||
        (insn & 0x1F000000u) == 0x1B000000u)
        return interp_exec_dp_register_arithmetic(cpu, insn);
    if (group == 0x1A000000u || group == 0x1A400000u) return interp_exec_dp_register_flags(cpu, insn);
    if (group == 0x1A800000u || group == 0x1AC00000u) return interp_exec_dp_register_select(cpu, insn);
    return interp_undefined(cpu, insn, "data-processing register -- unallocated encoding");
}

// Branches, exception generating and system instructions. Every form here ends the block.
static int interp_exec_branch_system(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;

    if ((insn & 0x7C000000u) == 0x14000000u) {
        int64_t offset = interp_sext(insn & 0x3FFFFFFu, 26) << 2;
        if (insn & 0x80000000u) {
            // pcrel_base: the guest's own view of the return address, un-biased for a non-PIE image.
            cpu->x[30] = pcrel_base(gpc) + 4;
        }
        cpu->pc = gpc + (uint64_t)offset;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    // B.cond, and BC.cond (the v8.8 hint: bit 4 apart, same architectural effect).
    if ((insn & 0xFF000010u) == 0x54000000u || (insn & 0xFF000010u) == 0x54000010u) {
        int64_t offset = interp_sext((insn >> 5) & 0x7FFFFu, 19) << 2;
        cpu->pc = interp_cond_holds(cpu, insn & 0xFu) ? gpc + (uint64_t)offset : gpc + 4;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    if ((insn & 0x7E000000u) == 0x34000000u) {
        unsigned sf = (insn >> 31) & 1, nonzero = (insn >> 24) & 1;
        int64_t offset = interp_sext((insn >> 5) & 0x7FFFFu, 19) << 2;
        uint64_t value = interp_gpr(cpu, (int)(insn & 31));
        int is_zero = sf ? value == 0 : (uint32_t)value == 0;
        cpu->pc = (nonzero ? !is_zero : is_zero) ? gpc + (uint64_t)offset : gpc + 4;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    // TBZ / TBNZ: the bit position is b5:b40, so insn[31] is its high bit and not an sf field.
    if ((insn & 0x7E000000u) == 0x36000000u) {
        unsigned nonzero = (insn >> 24) & 1;
        unsigned bit = (unsigned)(((insn >> 31) & 1) << 5) | ((insn >> 19) & 0x1Fu);
        int64_t offset = interp_sext((insn >> 5) & 0x3FFFu, 14) << 2;
        uint64_t value = interp_gpr(cpu, (int)(insn & 31));
        int set = (int)((value >> bit) & 1);
        cpu->pc = (nonzero ? set : !set) ? gpc + (uint64_t)offset : gpc + 4;
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    // BR / BLR / RET (the PAC/ERET forms are not modelled).
    if ((insn & 0xFE000000u) == 0xD6000000u) {
        unsigned opc = (insn >> 21) & 0xFu, op2 = (insn >> 16) & 0x1Fu, op3 = (insn >> 10) & 0x3Fu;
        int rn = (int)((insn >> 5) & 31);
        unsigned op4 = insn & 0x1Fu;
        if (op2 != 0x1F || op3 != 0 || op4 != 0)
            return interp_undefined(cpu, insn, "branch register -- pointer-authenticated branch (BRAA/BLRAA/RETAA)");
        switch (opc) {
        case 0: // BR
            cpu->pc = interp_gpr(cpu, rn);
            break;
        case 1: // BLR
        {
            uint64_t target = interp_gpr(cpu, rn);
            cpu->x[30] = pcrel_base(gpc) + 4;
            cpu->pc = target; // read the target BEFORE writing x30: `blr x30` must use the old value
            break;
        }
        case 2: // RET
            cpu->pc = interp_gpr(cpu, rn);
            break;
        default: return interp_undefined(cpu, insn, "branch register -- ERET/DRPS or unallocated opc");
        }
        cpu->reason = R_BRANCH;
        return INTERP_END;
    }

    if ((insn & 0xFF000000u) == 0xD4000000u) {
        unsigned opc = (insn >> 21) & 7, ll = insn & 3u;
        if (opc == 0 && ll == 1) {
            // The PC stays ON the svc; the dispatcher advances it unless the syscall set pc itself.
            cpu->pc = gpc;
            cpu->reason = R_SYSCALL;
            return INTERP_END;
        }
        // A GUEST event, not an engine gap: this must not reach interp_undefined (fatal, exit 70). BRK is
        // SIGTRAP/TRAP_BRKPT with the PC left ON it, so a handler that returns re-executes; HLT, HVC/SMC and
        // DCPS are UNDEFINED at EL0, so SIGILL.
        int signo = opc == 1 ? 5 /* SIGTRAP */ : 4 /* SIGILL */;
        int signal_code = opc == 1 ? 1 /* TRAP_BRKPT */ : 1 /* ILL_ILLOPC */;
        cpu->pc = gpc; // the faulting instruction the guest's frame must name
        // si_addr is pcrel_base(gpc): signal_canonicalize_pc treats the frame's own pc the same way.
        interp_raise_sync_signal(cpu, signo, signal_code, pcrel_base(gpc));
        return INTERP_END;
    }

    // Hints (the NOP space). Every member is a no-op at EL0; taking the whole space covers later hints.
    if ((insn & 0xFFFFF01Fu) == 0xD503201Fu) {
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Barriers. Rt is pinned to 11111: the constant is 0xD503301F, and 0xD5033000 would catch no barrier.
    if ((insn & 0xFFFFF01Fu) == 0xD503301Fu) {
        unsigned op2 = (insn >> 5) & 7;
        if (op2 == 6) {
            // ISB commits the icache dance: exit R_ICCOMMIT so smc_commit() drops rewritten blocks. New
            // BYTES come free from re-decoding, but the cached block EXTENT came from the old ones.
            cpu->pc = gpc + 4;
            cpu->reason = R_ICCOMMIT;
            return INTERP_END;
        }
        // DSB / DMB / SB / CLREX. Guest threads are host threads and guest accesses are ordinary C accesses
        // the host may reorder, so a guest barrier needs a real host one; SEQ_CST covers every ordering here.
        __atomic_thread_fence(__ATOMIC_SEQ_CST);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0xFFFFFFE0u) == 0xD50B7B20u) { // dc cvau, Xt
        // No-op: the host never instruction-fetches guest pages.
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    if ((insn & 0xFFFFFFE0u) == 0xD50B7520u) { // ic ivau, Xt
        // Record the line and exit R_ICFLUSH; smc_icflush() only QUEUES it, the drop happens at the ISB.
        cpu->smc_va = interp_gpr(cpu, (int)(insn & 31));
        cpu->pc = gpc + 4;
        cpu->reason = R_ICFLUSH;
        return INTERP_END;
    }

    // DC ZVA zeroes the block size advertised in DCZID_EL0 (== 4, so 64 bytes), never the host's.
    if ((insn & 0xFFFFFFE0u) == 0xD50B7420u) {
        uint64_t address = interp_gpr(cpu, (int)(insn & 31)) & ~UINT64_C(63);
        for (unsigned offset = 0; offset < 64u; offset += 8)
            interp_store_bits(address + offset, 0, 8);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // MSR (immediate): DAIF and the other PSTATE fields. Interrupt masking is meaningless at EL0.
    // op1 == 000 is excluded: it is CFINV / XAFLAG / AXFLAG (FEAT_FlagM, FlagM2), which REWRITE NZCV, plus
    // the EL1-only UAO/PAN/SPSel. Neither feature is advertised -- as with RMIF and SETF8/16 -- and running
    // them as no-ops left the flags silently wrong.
    if ((insn & 0xFFF8F01Fu) == 0xD500401Fu) {
        unsigned pstate_op1 = (insn >> 16) & 7u, pstate_op2 = (insn >> 5) & 7u;
        // op1:op2 == 011:011 is SVCR -- SMSTART/SMSTOP: no-oping it tells a guest streaming mode is on while
        // op0 == 0001 reports every SME instruction. Report, as the top-level decode already does.
        if (pstate_op1 == 0 || (pstate_op1 == 3 && pstate_op2 == 3))
            return interp_undefined(cpu, insn,
                                    "system -- MSR (immediate) CFINV/XAFLAG/AXFLAG, SMSTART/SMSTOP, or an EL1 "
                                    "PSTATE field");
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // MRS / MSR. Every value comes from g_aarch64_cpu_model (cpu.h) or emulated state, never the host.
    if ((insn & 0xFFD00000u) == 0xD5100000u) {
        int rt = (int)(insn & 31);
        uint32_t reg = insn & 0xFFFFFFE0u;
        int is_read = (insn & 0x00200000u) != 0; // bit 21 is L: 1 = MRS, 0 = MSR

        // EL1 ID-register space. HWCAP_CPUID is clear, so EL0 has no ID-register emulation and the access is
        // architecturally UNDEFINED -- a GUEST SIGILL, not an engine gap. translate.c emits a UDF word for the
        // same mask, which faults into the same guest signal; answering 0 here made the two backends disagree
        // and told a guest that had just been denied CPUID that its probe succeeded.
        if (is_read && (insn & 0xFFFF0000u) == 0xD5380000u && !g_aarch64_cpu_model.user_id_registers) {
            cpu->pc = gpc;
            interp_raise_sync_signal(cpu, 4 /* SIGILL */, 1 /* ILL_ILLOPC */, pcrel_base(gpc));
            return INTERP_END;
        }
        switch (reg) {
        case 0xD53B0020u: // MRS CTR_EL0. IDC=1/DIC=0: no clean needed before re-reading guest writes, but
                          // `ic ivau` must keep coming -- the engine's SMC interception point.
            interp_set_gpr(cpu, rt, g_aarch64_cpu_model.ctr_el0);
            break;
        case 0xD53B00E0u: // MRS DCZID_EL0 -- the DC ZVA block size
            interp_set_gpr(cpu, rt, g_aarch64_cpu_model.dczid_el0);
            break;
        case 0xD53BD040u: // MRS TPIDR_EL0 -- the thread pointer, emulated in cpu->tls
            interp_set_gpr(cpu, rt, cpu->tls);
            break;
        case 0xD51BD040u: // MSR TPIDR_EL0
            cpu->tls = interp_gpr(cpu, rt);
            break;
        case 0xD53BD060u: // MRS TPIDRRO_EL0 -- read-only alias
            interp_set_gpr(cpu, rt, cpu->tls);
            break;
        case 0xD53B4200u: // MRS NZCV
            interp_set_gpr(cpu, rt, cpu->nzcv & (INTERP_NZCV_N | INTERP_NZCV_Z | INTERP_NZCV_C | INTERP_NZCV_V));
            break;
        case 0xD51B4200u: // MSR NZCV -- only the four condition flags are writable
            cpu->nzcv = interp_gpr(cpu, rt) & (INTERP_NZCV_N | INTERP_NZCV_Z | INTERP_NZCV_C | INTERP_NZCV_V);
            break;
        case 0xD53B4220u: // MRS DAIF -- nothing is masked at an emulated EL0
            interp_set_gpr(cpu, rt, 0);
            break;
        case 0xD51B4220u: // MSR DAIF
            break;
        case 0xD53B4400u: // MRS FPCR
            interp_set_gpr(cpu, rt, g_interp_fpcr);
            break;
        case 0xD51B4400u: // MSR FPCR
            // The six trap-enable bits mask out and read back zero -- "no trapped FP exceptions", which
            // is what glibc's feenableexcept() probes for.
            g_interp_fpcr = interp_gpr(cpu, rt) & INTERP_FPCR_WRITABLE;
            break;
        case 0xD53B4420u: // MRS FPSR
            interp_set_gpr(cpu, rt, g_interp_fpsr);
            break;
        case 0xD51B4420u: // MSR FPSR. Only the six IEEE sticky bits and QC are writable; the rest are RES0.
            g_interp_fpsr = interp_gpr(cpu, rt) & INTERP_FPSR_WRITABLE;
            break;
        case 0xD53BE000u: // MRS CNTFRQ_EL0 -- 1 GHz, so the counter below IS a nanosecond count
            interp_set_gpr(cpu, rt, UINT64_C(1000000000));
            break;
        case 0xD53BE020u: // MRS CNTPCT_EL0 (physical counter)
        case 0xD53BE040u: // MRS CNTVCT_EL0 (virtual counter)
        case 0xD53BE0C0u: // MRS CNTVCTSS_EL0 (self-synchronising virtual counter)
            // The host monotonic clock clock_gettime is answered from: the two can never disagree.
            interp_set_gpr(cpu, rt, now_ns());
            break;
        default:
            // Unmodelled: report the encoding rather than answer 0 and fail far from the cause.
            return interp_undefined(cpu, insn, "system -- unmodelled system register (MRS/MSR)");
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0xFFC00000u) == 0xD5000000u)
        return interp_undefined(cpu, insn, "system -- SYS/SYSL maintenance operation");

    return interp_undefined(cpu, insn, "branches, exception generating and system -- unallocated encoding");
}
