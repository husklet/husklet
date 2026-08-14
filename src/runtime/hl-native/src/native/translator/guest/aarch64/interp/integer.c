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
#if defined(_WIN32)
    // A Windows vectored exception supplies a CONTEXT record but does not
    // modify a POSIX signal mask. Returning or long-jumping therefore owes no
    // mask restoration; uc_sigmask has no Windows analogue by design.
    (void)ucontext;
    return;
#else
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
#endif
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

#include "integer/immediate.c"
#include "integer/register.c"

#include "integer/control.c"
