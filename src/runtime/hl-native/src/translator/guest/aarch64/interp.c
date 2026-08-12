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

#include "../../guest_fetch.h"
#include "../../identity.h"
#include "../../digest.h"
#include "../../../host/host_cpu.h"
#include "../../../host/native_context.h" // ucontext_t: the fault path restores uc_sigmask by hand
#include "../../../host/range.h"
#include "../../../linux_abi/logical_vma.h"

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

// Data processing -- register; sub-class from insn[28:21] plus insn[30], in ARM ARM table order.
static int interp_exec_dp_register(struct cpu *cpu, uint32_t insn) {
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

// The vector register file. cpu->v[] holds V0..V31 as {low, high} uint64 pairs -- the layout
// guest/aarch64/signal.c memcpy's into the sigframe's fpsimd_context, so guest-visible ABI.
typedef struct {
    uint8_t byte[16];
} interp_vec;

static interp_vec interp_vec_read(const struct cpu *cpu, int reg) {
    interp_vec value;
    memcpy(value.byte, &cpu->v[2 * reg], 16);
    return value;
}

// THE RULE THAT IS EASY TO GET WRONG: a D-form (Q == 0) write must ZERO the upper 64 bits of the
// destination, for every AdvSIMD and scalar-FP write; keeping the old half is invisible until it is read.
static void interp_vec_write(struct cpu *cpu, int reg, interp_vec value, unsigned q) {
    if (!q) memset(value.byte + 8, 0, 8);
    memcpy(&cpu->v[2 * reg], value.byte, 16);
    // Nothing to spill here, but vdirty is in the checkpoint image the JIT shares; keep it truthful.
    cpu->vdirty = (uint64_t)(uintptr_t)cpu;
}

// `size` is the architecture's log2 element width: 0 = B, 1 = H, 2 = S, 3 = D.
static uint64_t interp_vec_element(const interp_vec *value, unsigned size, unsigned index) {
    uint64_t element = 0;
    memcpy(&element, value->byte + (index << size), (size_t)1u << size);
    return element;
}

static void interp_vec_set_element(interp_vec *value, unsigned size, unsigned index, uint64_t element) {
    memcpy(value->byte + (index << size), &element, (size_t)1u << size);
}

static unsigned interp_vec_lanes(unsigned size, unsigned q) {
    return (q ? 16u : 8u) >> size;
}

static uint64_t interp_element_mask(unsigned size) {
    return size >= 3 ? UINT64_MAX : ((UINT64_C(1) << (8u << size)) - 1u);
}

static uint64_t interp_element_sext(uint64_t element, unsigned size) {
    return (uint64_t)interp_sext(element, 8u << size);
}

// The byte dot product shared by FEAT_DotProd (SDOT/UDOT) and FEAT_I8MM (USDOT, SMMLA/UMMLA/USMMLA): four
// byte products summed modulo 2^32, exactly as the ARM ARM writes them. MMLA calls it twice per lane for its
// eight-element rows. Signedness is per SOURCE, which is what the mixed US/SU forms need.
static uint32_t interp_dot4(const interp_vec *left, const interp_vec *right, unsigned left_base, unsigned right_base,
                            int left_signed, int right_signed) {
    uint32_t sum = 0;
    for (unsigned i = 0; i < 4u; i++) {
        uint8_t a = left->byte[left_base + i], b = right->byte[right_base + i];
        int32_t x = left_signed ? (int32_t)(int8_t)a : (int32_t)a;
        int32_t y = right_signed ? (int32_t)(int8_t)b : (int32_t)b;
        sum += (uint32_t)(x * y);
    }
    return sum;
}

// Maps the three-same-extra opcode + U to how each source's bytes are read; 0 means "not a dot/MMLA form".
// 0010/0100 are the same-signedness pairs (S with U=0, U with U=1); 0011/0101 are the mixed unsigned-by-signed
// forms, for which U=1 is unallocated.
static int interp_dot_signedness(unsigned opcode, unsigned u, int *left_signed, int *right_signed) {
    if (opcode == 2u || opcode == 4u) {
        *left_signed = *right_signed = !u;
        return 1;
    }
    if ((opcode == 3u || opcode == 5u) && !u) {
        *left_signed = 0;
        *right_signed = 1;
        return 1;
    }
    return 0;
}

// AdvSIMDExpandImm(), for MOVI/MVNI and immediate ORR/BIC. Returns 0 for a reserved cmode/op.
static int interp_advsimd_expand_imm(unsigned op, unsigned cmode, unsigned o2, unsigned q, uint64_t imm8,
                                     uint64_t *out) {
    unsigned selector = (cmode >> 1) & 7, low = cmode & 1;
    uint64_t imm64;
    if (selector <= 3 && !low) { // 32-bit element, imm8 shifted by 0/8/16/24
        uint32_t narrow = (uint32_t)(imm8 << (8u * selector));
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else if (selector <= 3) { // the same shifts, but the ORR/BIC-immediate spelling
        uint32_t narrow = (uint32_t)(imm8 << (8u * selector));
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else if (selector == 4 || selector == 5) { // 16-bit element, imm8 shifted by 0 or 8
        uint16_t narrow = (uint16_t)(imm8 << (8u * (selector & 1u)));
        uint32_t doubled = ((uint32_t)narrow << 16) | narrow;
        imm64 = ((uint64_t)doubled << 32) | doubled;
    } else if (selector == 6) { // 32-bit element with a "moving ones" low field (MSL)
        uint32_t narrow = low ? (uint32_t)((imm8 << 16) | 0xFFFFu) : (uint32_t)((imm8 << 8) | 0xFFu);
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else if (!low && !op) { // 8-bit element replicated: MOVI Vd.8B/16B, #imm8
        if (o2) return 0;
        imm64 = imm8 * UINT64_C(0x0101010101010101);
    } else if (!low) {
        // cmode == 1110 with op == 1: each BIT of imm8 becomes a whole BYTE of the element (imm8<0> lowest),
        // so `movi v0.2d, #0xffffffff` is 0x00000000ffffffff, not the op == 0 arm's byte replication.
        if (o2) return 0;
        imm64 = 0;
        for (unsigned byte = 0; byte < 8u; byte++)
            if ((imm8 >> byte) & 1u) imm64 |= UINT64_C(0xFF) << (8u * byte);
    } else if (!op && o2) { // half-precision float expansion, replicated (FMOV Vd.4H/8H, #imm)
        uint32_t sign = (uint32_t)((imm8 >> 7) & 1), exponent = (uint32_t)((imm8 >> 4) & 7);
        uint32_t fraction = (uint32_t)(imm8 & 0xFu);
        uint32_t narrow =
            (sign << 15) | ((exponent & 4u) ? 0x3000u : 0x4000u) | ((exponent & 3u) << 10) | (fraction << 6);
        imm64 = (uint64_t)narrow * UINT64_C(0x0001000100010001);
    } else if (!op) { // single-precision float expansion, replicated
        uint32_t sign = (uint32_t)((imm8 >> 7) & 1), exponent = (uint32_t)((imm8 >> 4) & 7);
        uint32_t fraction = (uint32_t)(imm8 & 0xFu);
        uint32_t narrow =
            (sign << 31) | ((exponent & 4u) ? 0x3E000000u : 0x40000000u) | ((exponent & 3u) << 23) | (fraction << 19);
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else { // double-precision float expansion (Q must be 1)
        if (!q || o2) return 0;
        uint64_t sign = (imm8 >> 7) & 1, exponent = (imm8 >> 4) & 7, fraction = imm8 & 0xFu;
        imm64 = (sign << 63) | ((exponent & 4u) ? UINT64_C(0x3FC0000000000000) : UINT64_C(0x4000000000000000)) |
                (exponent & 3u) << 52 | (fraction << 48);
    }
    *out = imm64;
    return 1;
}

// imm5 encodes the element size in its lowest set bit and the lane index above it; none set is reserved.
static int interp_imm5_element(unsigned imm5, unsigned *size_out, unsigned *index_out) {
    if (imm5 & 1u) {
        *size_out = 0;
        *index_out = (imm5 >> 1) & 0xFu;
    } else if (imm5 & 2u) {
        *size_out = 1;
        *index_out = (imm5 >> 2) & 7u;
    } else if (imm5 & 4u) {
        *size_out = 2;
        *index_out = (imm5 >> 3) & 3u;
    } else if (imm5 & 8u) {
        *size_out = 3;
        *index_out = (imm5 >> 4) & 1u;
    } else {
        return 0;
    }
    return 1;
}

// The by-element operand of the "vector x indexed element" box, keyed on the ELEMENT size (log2), not on the
// encoding's `size` field -- half-precision FP spells H as size 00 but indexes like any 16-bit element.
// THE PART THAT IS SILENT WHEN WRONG: the index field GROWS as the element shrinks, at Rm's expense.
//   16-bit: index = H:L:M, so Rm is 4 bits -- only V0..V15 are addressable.
//   32-bit: index = H:L,   Rm = M:Rm.
//   64-bit: index = H,     Rm = M:Rm, and L must be 0.
// Reading H:L for a 16-bit element takes the right value from the wrong lane, which most tests never see.
static int interp_elem_index(uint32_t decode, unsigned size, unsigned *index_out, int *reg_out) {
    unsigned l = (decode >> 21) & 1u, m = (decode >> 20) & 1u, h = (decode >> 11) & 1u;
    unsigned low = (decode >> 16) & 0xFu;
    if (size == 1u) {
        *index_out = (h << 2) | (l << 1) | m;
        *reg_out = (int)low;
        return 1;
    }
    *reg_out = (int)(low | (m << 4));
    if (size == 2u) {
        *index_out = (h << 1) | l;
        return 1;
    }
    if (size != 3u || l) return 0;
    *index_out = h;
    return 1;
}

static void interp_vec_load(struct cpu *cpu, int reg, uint64_t address, unsigned bytes) {
    interp_vec value;
    memset(value.byte, 0, sizeof value.byte);
    if (bytes <= 8) {
        uint64_t chunk = interp_load_bits(address, bytes);
        memcpy(value.byte, &chunk, bytes);
    } else {
        uint64_t low = interp_load_bits(address, 8), high = interp_load_bits(address + 8, 8);
        memcpy(value.byte, &low, 8);
        memcpy(value.byte + 8, &high, 8);
    }
    interp_vec_write(cpu, reg, value, bytes > 8);
}

static void interp_vec_store(struct cpu *cpu, int reg, uint64_t address, unsigned bytes) {
    interp_vec value = interp_vec_read(cpu, reg);
    uint64_t low, high;
    memcpy(&low, value.byte, 8);
    memcpy(&high, value.byte + 8, 8);
    if (bytes <= 8) {
        interp_store_bits(address, low, bytes);
    } else {
        interp_store_bits(address, low, 8);
        interp_store_bits(address + 8, high, 8);
    }
}

// SIMD&FP access width, spelled opc<1>:size and NOT plain size. 0 means unallocated.
static unsigned interp_simd_access_bytes(unsigned size, unsigned opc) {
    if (opc & 2u) return size == 0 ? 16u : 0u;
    return 1u << size;
}

// Loads and stores. Three rules, each enforced in one place:
//   * Rn == 31 IS SP, NOT XZR, in every addressing mode here (interp_gpr_sp / interp_set_gpr_sp). Rt/Rt2/Rm
//     keep the ordinary meaning where 31 is XZR.
//   * Every guest access goes through interp_load_bits / interp_store_bits, which memcpy, so unaligned
//     accesses work. The atomic/exclusive family REQUIRES natural alignment instead.
//   * Read sources into locals, then access, then write the base back: the fault path has nothing to undo.

// The local exclusive monitor for LDXR/STXR: it records the address AND the value LDXR observed, and STXR
// compare-and-swaps against it. ABA is NOT reproduced -- more permissive, invisible to lock/refcount code.
static __thread int g_interp_monitor_valid;
static __thread uint64_t g_interp_monitor_address;
static __thread unsigned g_interp_monitor_bytes;
static __thread uint64_t g_interp_monitor_value;
static __thread uint64_t g_interp_monitor_value2; // second register of an LDXP

static void interp_monitor_clear(void) {
    g_interp_monitor_valid = 0;
}

// A misaligned atomic: an alignment fault, reported through the JIT's soft-TLB-probe reason so signal.c
// raises it as an ordinary synchronous SIGBUS.
static int interp_alignment_fault(struct cpu *cpu, uint64_t address) {
    cpu->fault_addr = address;
    cpu->bus_ea = address;
    cpu->reason = R_BUS;
    return INTERP_END;
}

static int interp_exec_load_store(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rt = (int)(insn & 31), rn = (int)((insn >> 5) & 31);
    int rt2 = (int)((insn >> 10) & 31), rm = (int)((insn >> 16) & 31);
    unsigned vector = (insn >> 26) & 1;
    unsigned q = (insn >> 30) & 1;

    // AdvSIMD load/store multiple structures. `opcode` names both the register count and whether they
    // INTERLEAVE: LD1 x4 is four whole-register loads, LD4 walks memory one element at a time across four.
    if ((insn & 0xBF200000u) == 0x0C000000u) {
        unsigned load = (insn >> 22) & 1u, opcode = (insn >> 12) & 0xFu, esize_code = (insn >> 10) & 3u;
        int post_index = (insn & 0x00800000u) != 0;
        unsigned registers, interleaved = 1;
        switch (opcode) {
        case 0x0: registers = 4; break; // LD4/ST4
        case 0x2:
            registers = 4;
            interleaved = 0;
            break;                      // LD1/ST1, four registers
        case 0x4: registers = 3; break; // LD3/ST3
        case 0x6:
            registers = 3;
            interleaved = 0;
            break; // LD1/ST1, three registers
        case 0x7:
            registers = 1;
            interleaved = 0;
            break;                      // LD1/ST1, one register
        case 0x8: registers = 2; break; // LD2/ST2
        case 0xA:
            registers = 2;
            interleaved = 0;
            break; // LD1/ST1, two registers
        default: return interp_undefined(cpu, insn, "AdvSIMD load/store -- unallocated multi-structure opcode");
        }
        unsigned bytes = q ? 16u : 8u;
        uint64_t base = interp_gpr_sp(cpu, rn);
        uint64_t address = base;
        // The 1D arrangement exists only for the one-register LD1/ST1 form.
        if (esize_code == 3 && !q && registers > 1)
            return interp_undefined(cpu, insn, "AdvSIMD load/store -- 1D arrangement with several registers");
        if (interleaved) {
            unsigned lanes = interp_vec_lanes(esize_code, q), element_bytes = 1u << esize_code;
            for (unsigned lane = 0; lane < lanes; lane++)
                for (unsigned index = 0; index < registers; index++) {
                    int reg = (rt + (int)index) % 32; // the register list wraps at V31
                    if (load) {
                        uint64_t element = interp_load_bits(address, element_bytes);
                        // Lane 0 starts from zero: unwritten lanes must end up zero, not stale.
                        interp_vec value;
                        if (lane == 0)
                            memset(value.byte, 0, sizeof value.byte);
                        else
                            value = interp_vec_read(cpu, reg);
                        interp_vec_set_element(&value, esize_code, lane, element);
                        interp_vec_write(cpu, reg, value, 1);
                    } else {
                        interp_vec value = interp_vec_read(cpu, reg);
                        interp_store_bits(address, interp_vec_element(&value, esize_code, lane), element_bytes);
                    }
                    address += element_bytes;
                }
        } else {
            for (unsigned index = 0; index < registers; index++) {
                int reg = (rt + (int)index) % 32;
                if (load) {
                    interp_vec value;
                    memset(value.byte, 0, sizeof value.byte);
                    for (unsigned offset = 0; offset < bytes; offset += 8) {
                        uint64_t chunk = interp_load_bits(address + offset, 8);
                        memcpy(value.byte + offset, &chunk, 8);
                    }
                    interp_vec_write(cpu, reg, value, q);
                } else {
                    interp_vec value = interp_vec_read(cpu, reg);
                    for (unsigned offset = 0; offset < bytes; offset += 8) {
                        uint64_t chunk;
                        memcpy(&chunk, value.byte + offset, 8);
                        interp_store_bits(address + offset, chunk, 8);
                    }
                }
                address += bytes;
            }
        }
        if (post_index) {
            // Rm == 31: the increment is the whole transfer size.
            uint64_t increment = rm == 31 ? (uint64_t)registers * bytes : interp_gpr(cpu, rm);
            interp_set_gpr_sp(cpu, rn, base + increment);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD load/store SINGLE structure, and the LD1R..LD4R replicating loads. Register count is
    // (R ? 2 : 1) + (opcode<0> ? 2 : 0); the lane INDEX is Q:S:size for 8-bit, Q:S:size<1> for 16-bit, Q:S
    // for 32-bit, Q alone for 64-bit -- `size` is NOT a lane count. The mask covers bits[29:24] only, since
    // bit23 is post-index and bit21 is R and either one would reject every LD2R/LD4R.
    if ((insn & 0xBF000000u) == 0x0D000000u) {
        unsigned load = (insn >> 22) & 1u, replicate_group = (insn >> 21) & 1u;
        unsigned opcode = (insn >> 13) & 7u, selector = (insn >> 12) & 1u, size_field = (insn >> 10) & 3u;
        int post_index = (insn & 0x00800000u) != 0;
        unsigned registers = (replicate_group ? 2u : 1u) + ((opcode & 1u) ? 2u : 0u);
        uint64_t base = interp_gpr_sp(cpu, rn), address = base;
        unsigned element_size, index;
        if ((opcode >> 1) == 3u) { // LD1R / LD2R / LD3R / LD4R
            if (!load || selector) return interp_undefined(cpu, insn, "AdvSIMD load single -- unallocated replicate");
            element_size = size_field;
            unsigned bytes = 1u << element_size;
            for (unsigned entry = 0; entry < registers; entry++) {
                uint64_t element = interp_load_bits(address, bytes);
                interp_vec value;
                memset(value.byte, 0, sizeof value.byte);
                for (unsigned lane = 0; lane < interp_vec_lanes(element_size, q); lane++)
                    interp_vec_set_element(&value, element_size, lane, element);
                interp_vec_write(cpu, (rt + (int)entry) % 32, value, q);
                address += bytes;
            }
        } else {
            switch (opcode >> 1) {
            case 0: // 8-bit: the index uses Q, S and both size bits
                element_size = 0;
                index = (q << 3) | (selector << 2) | size_field;
                break;
            case 1: // 16-bit: size<0> is RES0 and does not participate
                if (size_field & 1u) return interp_undefined(cpu, insn, "AdvSIMD load single -- 16-bit size<0> set");
                element_size = 1;
                index = (q << 2) | (selector << 1) | (size_field >> 1);
                break;
            default: // 32-bit when size == 00, 64-bit when size == 01 (S must then be 0)
                if (size_field == 0) {
                    element_size = 2;
                    index = (q << 1) | selector;
                } else if (size_field == 1 && selector == 0) {
                    element_size = 3;
                    index = q;
                } else {
                    return interp_undefined(cpu, insn, "AdvSIMD load single -- unallocated 32/64-bit form");
                }
                break;
            }
            unsigned bytes = 1u << element_size;
            for (unsigned entry = 0; entry < registers; entry++) {
                int reg = (rt + (int)entry) % 32;
                if (load) {
                    uint64_t element = interp_load_bits(address, bytes);
                    // A single-lane LOAD leaves every other lane unchanged, [127:64] included.
                    interp_vec value = interp_vec_read(cpu, reg);
                    interp_vec_set_element(&value, element_size, index, element);
                    interp_vec_write(cpu, reg, value, 1);
                } else {
                    interp_vec value = interp_vec_read(cpu, reg);
                    interp_store_bits(address, interp_vec_element(&value, element_size, index), bytes);
                }
                address += bytes;
            }
        }
        if (post_index) {
            uint64_t increment = rm == 31 ? (address - base) : interp_gpr(cpu, rm);
            interp_set_gpr_sp(cpu, rn, base + increment);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x3B000000u) == 0x18000000u) {
        unsigned opc = (insn >> 30) & 3;
        int64_t offset = interp_sext((insn >> 5) & 0x7FFFFu, 19) << 2;
        // pcrel_base, not the raw PC: a non-PIE image's architectural PC is its low link address.
        uint64_t address = pcrel_base(gpc) + (uint64_t)offset;
        if (vector) { // LDR St/Dt/Qt, literal
            if (opc == 3) return interp_undefined(cpu, insn, "loads and stores -- unallocated SIMD literal size");
            interp_vec_load(cpu, rt, address, 4u << opc);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opc == 3) { // PRFM (literal): a hint
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opc == 2) // LDRSW
            interp_set_gpr(cpu, rt, (uint64_t)interp_sext(interp_load_bits(address, 4), 32));
        else if (opc == 1) // LDR Xt
            interp_set_gpr(cpu, rt, interp_load_bits(address, 8));
        else // LDR Wt
            interp_set_gpr32(cpu, rt, (uint32_t)interp_load_bits(address, 4));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x3A000000u) == 0x28000000u) {
        unsigned opc = (insn >> 30) & 3, load = (insn >> 22) & 1, mode = (insn >> 23) & 3;
        if (vector) { // STP/LDP of two S, D or Q registers
            if (opc == 3) return interp_undefined(cpu, insn, "loads and stores -- unallocated SIMD pair opc");
            unsigned element = 4u << opc; // opc 0/1/2 -> 4, 8, 16 bytes per register
            int64_t vector_offset = interp_sext((insn >> 15) & 0x7Fu, 7) * (int64_t)element;
            uint64_t vector_base = interp_gpr_sp(cpu, rn);
            int vector_writeback = mode == 1 || mode == 3;
            uint64_t vector_address = mode == 1 ? vector_base : vector_base + (uint64_t)vector_offset;
            if (load) {
                interp_vec_load(cpu, rt, vector_address, element);
                interp_vec_load(cpu, rt2, vector_address + element, element);
            } else {
                interp_vec_store(cpu, rt, vector_address, element);
                interp_vec_store(cpu, rt2, vector_address + element, element);
            }
            if (vector_writeback) interp_set_gpr_sp(cpu, rn, vector_base + (uint64_t)vector_offset);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opc == 3) return interp_undefined(cpu, insn, "loads and stores -- unallocated pair opc");
        if (opc == 1 && !load) return interp_undefined(cpu, insn, "loads and stores -- STGP (memory tagging)");
        unsigned bytes = opc == 2 ? 8u : 4u;
        unsigned scale = opc == 2 ? 3u : 2u;
        int64_t offset = interp_sext((insn >> 15) & 0x7Fu, 7) << scale;
        uint64_t base = interp_gpr_sp(cpu, rn);
        // mode 0 = LDNP/STNP, 1 = post-index, 2 = signed offset, 3 = pre-index. Only 1 uses the OLD base.
        int writeback = mode == 1 || mode == 3;
        uint64_t address = mode == 1 ? base : base + (uint64_t)offset;
        if (load) {
            uint64_t first = interp_load_bits(address, bytes);
            uint64_t second = interp_load_bits(address + bytes, bytes);
            if (opc == 1) { // LDPSW: two 32-bit loads, sign-extended
                interp_set_gpr(cpu, rt, (uint64_t)interp_sext(first, 32));
                interp_set_gpr(cpu, rt2, (uint64_t)interp_sext(second, 32));
            } else if (bytes == 8) {
                interp_set_gpr(cpu, rt, first);
                interp_set_gpr(cpu, rt2, second);
            } else {
                interp_set_gpr32(cpu, rt, (uint32_t)first);
                interp_set_gpr32(cpu, rt2, (uint32_t)second);
            }
        } else {
            uint64_t first = interp_gpr(cpu, rt), second = interp_gpr(cpu, rt2);
            interp_store_bits(address, first, bytes);
            interp_store_bits(address + bytes, second, bytes);
        }
        // Writeback LAST: Rn as a transfer register too is CONSTRAINED UNPREDICTABLE; last is what cores do.
        if (writeback) interp_set_gpr_sp(cpu, rn, base + (uint64_t)offset);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Load/store exclusive, plus the ordered (LDAR/STLR) and CAS members of the box.
    if ((insn & 0x3F000000u) == 0x08000000u) {
        unsigned size = (insn >> 30) & 3, o2 = (insn >> 23) & 1, load = (insn >> 22) & 1;
        unsigned o1 = (insn >> 21) & 1, o0 = (insn >> 15) & 1;
        int rs = rm;
        unsigned bytes = 1u << size;

        if (o2 && o1) { // CAS / CASA / CASL / CASAL (Rt2 is 11111)
            if (rt2 != 31) return interp_undefined(cpu, insn, "loads and stores -- unallocated CAS encoding");
            uint64_t address = interp_gpr_sp(cpu, rn);
            void *pointer = interp_atomic_pointer(address, bytes);
            if (pointer == NULL) return interp_alignment_fault(cpu, address);
            uint64_t compare = interp_gpr(cpu, rs), swap = interp_gpr(cpu, rt);
            // Comparand and returned value are the ACCESS width, not the register width.
            uint64_t mask = bytes == 8 ? UINT64_MAX : ((UINT64_C(1) << (bytes * 8)) - 1u);
            uint64_t expected = compare & mask, observed;
            interp_access_begin(address, bytes, 1);
            switch (bytes) {
            case 1: {
                uint8_t narrow = (uint8_t)expected;
                __atomic_compare_exchange_n((uint8_t *)pointer, &narrow, (uint8_t)swap, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed = narrow;
                break;
            }
            case 2: {
                uint16_t narrow = (uint16_t)expected;
                __atomic_compare_exchange_n((uint16_t *)pointer, &narrow, (uint16_t)swap, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed = narrow;
                break;
            }
            case 4: {
                uint32_t narrow = (uint32_t)expected;
                __atomic_compare_exchange_n((uint32_t *)pointer, &narrow, (uint32_t)swap, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed = narrow;
                break;
            }
            default: {
                uint64_t wide = expected;
                __atomic_compare_exchange_n((uint64_t *)pointer, &wide, swap, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                observed = wide;
                break;
            }
            }
            interp_access_end();
            // CAS returns the PRE-EXISTING value in Rs, whether or not the swap happened.
            if (bytes == 8)
                interp_set_gpr(cpu, rs, observed);
            else
                interp_set_gpr32(cpu, rs, (uint32_t)observed);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (o2) { // LDAR / LDLAR / STLR / STLLR: ordered access, no monitor
            if (o1 || rs != 31 || rt2 != 31)
                return interp_undefined(cpu, insn, "loads and stores -- unallocated ordered-access encoding");
            uint64_t address = interp_gpr_sp(cpu, rn);
            // Free on x86-TSO, but this backend is not x86-only and the compiler must be stopped anyway.
            if (load) {
                uint64_t value = interp_load_bits(address, bytes);
                __atomic_thread_fence(__ATOMIC_ACQUIRE);
                if (bytes == 8)
                    interp_set_gpr(cpu, rt, value);
                else
                    interp_set_gpr32(cpu, rt, (uint32_t)value);
            } else {
                uint64_t value = interp_gpr(cpu, rt);
                __atomic_thread_fence(__ATOMIC_RELEASE);
                interp_store_bits(address, value, bytes);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        uint64_t address = interp_gpr_sp(cpu, rn);

        // CASP shares o1 == 1 with LDXP/STXP and is separated by BIT 31, not by size: `size < 2` alone
        // rejects every CASP as an unallocated pair size.
        if (o1 && !(insn & 0x80000000u)) {
            if (rt2 != 31) return interp_undefined(cpu, insn, "loads and stores -- unallocated CASP encoding");
            // Rs and Rt must both be even: each names the first of a register PAIR.
            if ((rs & 1) || (rt & 1))
                return interp_undefined(cpu, insn, "loads and stores -- CASP with an odd register pair");
            unsigned element = ((insn >> 30) & 1u) ? 8u : 4u; // bit30 selects a 32-bit or 64-bit pair
            unsigned total = element * 2u;
            void *pointer = interp_atomic_pointer(address, total);
            if (pointer == NULL) return interp_alignment_fault(cpu, address);
            uint64_t compare_low = interp_gpr(cpu, rs), compare_high = interp_gpr(cpu, rs + 1);
            uint64_t swap_low = interp_gpr(cpu, rt), swap_high = interp_gpr(cpu, rt + 1);
            uint64_t observed_low, observed_high;
            interp_access_begin(address, total, 1);
            if (element == 4) {
                // A 32-bit pair is one aligned 64-bit location; low register first (little-endian guest).
                uint64_t expected = (compare_low & 0xFFFFFFFFu) | ((compare_high & 0xFFFFFFFFu) << 32);
                uint64_t replacement = (swap_low & 0xFFFFFFFFu) | ((swap_high & 0xFFFFFFFFu) << 32);
                __atomic_compare_exchange_n((uint64_t *)pointer, &expected, replacement, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed_low = expected & 0xFFFFFFFFu;
                observed_high = expected >> 32;
            } else {
                unsigned __int128 expected = (unsigned __int128)compare_low | ((unsigned __int128)compare_high << 64);
                unsigned __int128 replacement = (unsigned __int128)swap_low | ((unsigned __int128)swap_high << 64);
                __atomic_compare_exchange_n((unsigned __int128 *)pointer, &expected, replacement, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed_low = (uint64_t)expected;
                observed_high = (uint64_t)(expected >> 64);
            }
            interp_access_end();
            // Like CAS, CASP returns the PRE-EXISTING pair whether or not it swapped.
            if (element == 8) {
                interp_set_gpr(cpu, rs, observed_low);
                interp_set_gpr(cpu, rs + 1, observed_high);
            } else {
                interp_set_gpr32(cpu, rs, (uint32_t)observed_low);
                interp_set_gpr32(cpu, rs + 1, (uint32_t)observed_high);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (o1 && size < 2) return interp_undefined(cpu, insn, "loads and stores -- unallocated exclusive-pair size");
        unsigned access_bytes = o1 ? bytes * 2u : bytes;
        if (load) { // LDXR / LDAXR / LDXP / LDAXP
            if (rs != 31 || (!o1 && rt2 != 31))
                return interp_undefined(cpu, insn, "loads and stores -- unallocated load-exclusive encoding");
            if (interp_atomic_pointer(address, bytes) == NULL) return interp_alignment_fault(cpu, address);
            uint64_t first = interp_load_bits(address, bytes);
            uint64_t second = o1 ? interp_load_bits(address + bytes, bytes) : 0;
            if (o0) __atomic_thread_fence(__ATOMIC_ACQUIRE); // LDAXR/LDAXP
            g_interp_monitor_address = address;
            g_interp_monitor_bytes = access_bytes;
            g_interp_monitor_value = first;
            g_interp_monitor_value2 = second;
            g_interp_monitor_valid = 1;
            if (bytes == 8) {
                interp_set_gpr(cpu, rt, first);
                if (o1) interp_set_gpr(cpu, rt2, second);
            } else {
                interp_set_gpr32(cpu, rt, (uint32_t)first);
                if (o1) interp_set_gpr32(cpu, rt2, (uint32_t)second);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // STXR / STLXR / STXP / STLXP: Rs receives 0 on success, 1 on failure.
        if (!o1 && rt2 != 31)
            return interp_undefined(cpu, insn, "loads and stores -- unallocated store-exclusive encoding");
        void *pointer = interp_atomic_pointer(address, bytes);
        if (pointer == NULL) return interp_alignment_fault(cpu, address);
        unsigned failed = 1;
        if (g_interp_monitor_valid && g_interp_monitor_address == address && g_interp_monitor_bytes == access_bytes) {
            uint64_t desired = interp_gpr(cpu, rt);
            if (o0) __atomic_thread_fence(__ATOMIC_RELEASE); // STLXR/STLXP
            interp_access_begin(address, access_bytes, 1);
            if (!o1) {
                switch (bytes) {
                case 1: {
                    uint8_t expected = (uint8_t)g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint8_t *)pointer, &expected, (uint8_t)desired, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                    break;
                }
                case 2: {
                    uint16_t expected = (uint16_t)g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint16_t *)pointer, &expected, (uint16_t)desired, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                    break;
                }
                case 4: {
                    uint32_t expected = (uint32_t)g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint32_t *)pointer, &expected, (uint32_t)desired, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                    break;
                }
                default: {
                    uint64_t expected = g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint64_t *)pointer, &expected, desired, 0, __ATOMIC_SEQ_CST,
                                                          __ATOMIC_SEQ_CST);
                    break;
                }
                }
            } else {
                // STXP: the pair must commit indivisibly -- a 64-bit pair is one 128-bit CAS, a 32-bit
                // pair one 64-bit CAS. __atomic on 16 bytes may lower to a libatomic lock, consistent only
                // because every access to the location goes through this code.
                uint64_t desired2 = interp_gpr(cpu, rt2);
                if (bytes == 4) {
                    uint64_t expected =
                        (g_interp_monitor_value & 0xFFFFFFFFu) | ((g_interp_monitor_value2 & 0xFFFFFFFFu) << 32);
                    uint64_t replacement = (desired & 0xFFFFFFFFu) | ((desired2 & 0xFFFFFFFFu) << 32);
                    failed = !__atomic_compare_exchange_n((uint64_t *)pointer, &expected, replacement, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                } else {
                    unsigned __int128 expected =
                        (unsigned __int128)g_interp_monitor_value | ((unsigned __int128)g_interp_monitor_value2 << 64);
                    unsigned __int128 replacement = (unsigned __int128)desired | ((unsigned __int128)desired2 << 64);
                    failed = !__atomic_compare_exchange_n((unsigned __int128 *)pointer, &expected, replacement, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                }
            }
            interp_access_end();
        }
        // ANY store-exclusive clears the monitor, or a retry loop could succeed without re-reading.
        interp_monitor_clear();
        interp_set_gpr32(cpu, rs, failed);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // LDAPR (FEAT_LRCPC) sits inside the LSE atomic box below, so it must be recognised BEFORE that
    // decode. RCpc and RCsc come out the same here: SEQ_CST is stronger than either.
    if ((insn & 0x3FFFFC00u) == 0x38BFC000u && !vector) {
        unsigned bytes = 1u << ((insn >> 30) & 3);
        uint64_t value = interp_load_bits(interp_gpr_sp(cpu, rn), bytes);
        __atomic_thread_fence(__ATOMIC_SEQ_CST);
        if (bytes == 8)
            interp_set_gpr(cpu, rt, value);
        else
            interp_set_gpr32(cpu, rt, (uint32_t)value);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Atomic memory operations (LSE), sharing the register-offset box; bits[11:10] == 00 selects them.
    if ((insn & 0x3B200C00u) == 0x38200000u) {
        unsigned size = (insn >> 30) & 3, opc = (insn >> 12) & 7, o3 = (insn >> 15) & 1;
        int rs = rm;
        unsigned bytes = 1u << size;
        if (vector) return interp_undefined(cpu, insn, "loads and stores -- SIMD/FP atomic");
        uint64_t address = interp_gpr_sp(cpu, rn);
        void *pointer = interp_atomic_pointer(address, bytes);
        if (pointer == NULL) return interp_alignment_fault(cpu, address);
        uint64_t operand = interp_gpr(cpu, rs), old = 0;
        // Real host read-modify-writes, not load-then-store: an interleaved peer would lose an update.
        interp_access_begin(address, bytes, 1);
#define INTERP_LSE_RMW(type, expression)                                                                               \
    do {                                                                                                               \
        type *slot = (type *)pointer;                                                                                  \
        type argument = (type)operand;                                                                                 \
        (void)argument;                                                                                                \
        old = (uint64_t)(expression);                                                                                  \
    } while (0)
#define INTERP_LSE_WIDTHS(expression8, expression16, expression32, expression64)                                       \
    do {                                                                                                               \
        switch (bytes) {                                                                                               \
        case 1: INTERP_LSE_RMW(uint8_t, expression8); break;                                                           \
        case 2: INTERP_LSE_RMW(uint16_t, expression16); break;                                                         \
        case 4: INTERP_LSE_RMW(uint32_t, expression32); break;                                                         \
        default: INTERP_LSE_RMW(uint64_t, expression64); break;                                                        \
        }                                                                                                              \
    } while (0)
        if (o3) { // SWP
            if (opc != 0) {
                interp_access_end();
                return interp_undefined(cpu, insn, "loads and stores -- unallocated LSE swap/op3 encoding");
            }
            INTERP_LSE_WIDTHS(__atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST),
                              __atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST),
                              __atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST),
                              __atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST));
        } else {
            switch (opc) {
            case 0: // LDADD
                INTERP_LSE_WIDTHS(__atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST));
                break;
            case 1: // LDCLR: bit CLEAR, so the operand is complemented
                INTERP_LSE_WIDTHS(__atomic_fetch_and(slot, (uint8_t)~argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_and(slot, (uint16_t)~argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_and(slot, (uint32_t)~argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_and(slot, (uint64_t)~argument, __ATOMIC_SEQ_CST));
                break;
            case 2: // LDEOR
                INTERP_LSE_WIDTHS(__atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST));
                break;
            case 3: // LDSET
                INTERP_LSE_WIDTHS(__atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST));
                break;
            case 4:   // LDSMAX
            case 5:   // LDSMIN
            case 6:   // LDUMAX
            case 7: { // LDUMIN
                // No __atomic_fetch_max, so these are a CAS retry loop: a load-compare-store would let a
                // peer's update land in between. Comparison is at the ACCESS width and signedness.
                unsigned want_max = opc == 4 || opc == 6;
                unsigned is_signed = opc < 6;
#define INTERP_LSE_MINMAX(type, signed_type)                                                                           \
    do {                                                                                                               \
        type *slot = (type *)pointer;                                                                                  \
        type argument = (type)operand;                                                                                 \
        type current = __atomic_load_n(slot, __ATOMIC_SEQ_CST);                                                        \
        for (;;) {                                                                                                     \
            int argument_greater = is_signed ? ((signed_type)argument > (signed_type)current) : (argument > current);  \
            type chosen = (argument_greater == (int)(want_max != 0)) ? argument : current;                             \
            /* Already correct: nothing to store, and `current` is still the pre-existing value to return. */          \
            if (chosen == current) break;                                                                              \
            if (__atomic_compare_exchange_n(slot, &current, chosen, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) break;     \
            /* A peer won the race; `current` now holds its value, so re-decide against that. */                       \
        }                                                                                                              \
        old = (uint64_t)current;                                                                                       \
    } while (0)
                switch (bytes) {
                case 1: INTERP_LSE_MINMAX(uint8_t, int8_t); break;
                case 2: INTERP_LSE_MINMAX(uint16_t, int16_t); break;
                case 4: INTERP_LSE_MINMAX(uint32_t, int32_t); break;
                default: INTERP_LSE_MINMAX(uint64_t, int64_t); break;
                }
#undef INTERP_LSE_MINMAX
                break;
            }
            default:
                interp_access_end();
                return interp_undefined(cpu, insn, "loads and stores -- unallocated LSE atomic opcode");
            }
        }
#undef INTERP_LSE_WIDTHS
#undef INTERP_LSE_RMW
        interp_access_end();
        // Rt receives the PRE-operation value; Rt == 31 is the ST<op> alias, which discards it.
        if (bytes == 8)
            interp_set_gpr(cpu, rt, old);
        else
            interp_set_gpr32(cpu, rt, (uint32_t)old);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // The three single-register integer addressing modes, sharing one size/opc layout:
    //   opc == 0  store of (1 << size) bytes        1  zero-extending load
    //   opc == 2  sign-extending load into Xt       3  sign-extending load into Wt   (2 with size 3 is PRFM)
    unsigned size = (insn >> 30) & 3;
    unsigned opc = (insn >> 22) & 3;
    int scaled = (insn & 0x3B000000u) == 0x39000000u;
    int register_offset = (insn & 0x3B200C00u) == 0x38200800u;
    int unscaled = (insn & 0x3B200000u) == 0x38000000u;
    if (!scaled && !register_offset && !unscaled)
        return interp_undefined(cpu, insn, "loads and stores -- AdvSIMD structure or unallocated encoding");
    if (!vector && ((insn & 0x3B200C00u) == 0x38200400u || (insn & 0x3B200C00u) == 0x38200C00u))
        return interp_undefined(cpu, insn, "loads and stores -- LDRAA/LDRAB (pointer authentication)");

    unsigned bytes = vector ? interp_simd_access_bytes(size, opc) : (1u << size);
    if (vector && bytes == 0) return interp_undefined(cpu, insn, "loads and stores -- unallocated SIMD/FP access size");
    unsigned scale = vector ? (opc & 2u ? 4u : size) : size;
    uint64_t base = interp_gpr_sp(cpu, rn);
    uint64_t address;
    int writeback = 0;
    uint64_t writeback_value = 0;
    if (scaled) {
        address = base + (((uint64_t)((insn >> 10) & 0xFFFu)) << scale);
    } else if (register_offset) {
        unsigned option = (insn >> 13) & 7, s = (insn >> 12) & 1;
        // S scales the index by the access size; only options 010/011/110/111 are allocated here.
        if ((option & 3u) < 2u)
            return interp_undefined(cpu, insn, "loads and stores -- unallocated register-offset extend option");
        address = base + interp_extend_operand(cpu, rm, option, s ? scale : 0u, 1);
    } else {
        unsigned mode = (insn >> 10) & 3;
        int64_t offset = interp_sext((insn >> 12) & 0x1FFu, 9);
        // mode 0 = LDUR/STUR, 1 = post-index, 2 = LDTR/STTR (at EL0 the same as 0), 3 = pre-index.
        writeback = mode == 1 || mode == 3;
        writeback_value = base + (uint64_t)offset;
        address = mode == 1 ? base : base + (uint64_t)offset;
    }

    if (vector) {
        if (opc & 1u)
            interp_vec_load(cpu, rt, address, bytes);
        else
            interp_vec_store(cpu, rt, address, bytes);
    } else if (opc == 0) {                    // store
        uint64_t value = interp_gpr(cpu, rt); // source read before the access
        interp_store_bits(address, value, bytes);
    } else if (opc == 2 && size == 3) { // PRFM / PRFUM: a hint
        (void)0;
    } else if (opc == 1) { // zero-extending load
        uint64_t value = interp_load_bits(address, bytes);
        if (size == 3)
            interp_set_gpr(cpu, rt, value);
        else
            interp_set_gpr32(cpu, rt, (uint32_t)value); // a 32-bit destination zero-extends to 64 anyway
    } else {                                            // sign-extending load: LDRSB / LDRSH / LDRSW
        if (size == 3 || (size == 2 && opc == 3))
            return interp_undefined(cpu, insn, "loads and stores -- unallocated sign-extending load size");
        uint64_t value = (uint64_t)interp_sext(interp_load_bits(address, bytes), bytes * 8u);
        if (opc == 2) // 64-bit destination
            interp_set_gpr(cpu, rt, value);
        else // 32-bit destination
            interp_set_gpr32(cpu, rt, (uint32_t)value);
    }
    if (writeback) interp_set_gpr_sp(cpu, rn, writeback_value);
    cpu->pc = gpc + 4;
    return INTERP_NEXT;
}

// Floating point: FPCR/FPSR, and how a guest FP result is computed.
// Project FPCR onto the host FP environment (<fenv.h>; this file backs every non-AArch64 host) and let the
// host compute: for FADD/FSUB/FMUL/FDIV/FSQRT/FMADD and single <-> double both ISAs define the same
// correctly-rounded IEEE-754 result. guest/x86_64/x87state.c is this same map read right to left.
//
// Handled below instead, because the ISAs differ: NaN propagation (AArch64 prefers a SIGNALLING operand, in
// operand order; x86 the first), the default NaN (AArch64's sign bit is CLEAR, x86's SET), FPCR.DN and
// FPCR.FZ/FZ16, FMIN/FMAX (signs ANDed for max(+0,-0)), and the integer and half conversions, which x86-64
// has no baseline correctly-rounded instruction for. AArch64 also detects tininess BEFORE rounding and
// x86-64 after, so FPSR.UFC can differ for a value tiny before rounding that rounds up to normal.

// The three formats, ordered so (fmt + 1) is the AdvSIMD element-size code the vector accessors take.
#define INTERP_FP_H 0u
#define INTERP_FP_S 1u
#define INTERP_FP_D 2u

static unsigned interp_fp_width(unsigned fmt) {
    return fmt == INTERP_FP_H ? 16u : (fmt == INTERP_FP_S ? 32u : 64u);
}

static unsigned interp_fp_mant(unsigned fmt) {
    return fmt == INTERP_FP_H ? 10u : (fmt == INTERP_FP_S ? 23u : 52u);
}

static int interp_fp_bias(unsigned fmt) {
    return fmt == INTERP_FP_H ? 15 : (fmt == INTERP_FP_S ? 127 : 1023);
}

static unsigned interp_fp_inf_exp(unsigned fmt) {
    return fmt == INTERP_FP_H ? 0x1Fu : (fmt == INTERP_FP_S ? 0xFFu : 0x7FFu);
}

static uint64_t interp_fp_mant_mask(unsigned fmt) {
    return (UINT64_C(1) << interp_fp_mant(fmt)) - 1u;
}

static uint64_t interp_fp_sign_mask(unsigned fmt) {
    return UINT64_C(1) << (interp_fp_width(fmt) - 1u);
}

// ptype: 10 is unallocated as a format (only FMOV to/from Vn.D[1] spells it, and carries no format).
static int interp_fp_type_fmt(unsigned type, unsigned *fmt) {
    switch (type) {
    case 0: *fmt = INTERP_FP_S; return 1;
    case 1: *fmt = INTERP_FP_D; return 1;
    case 3: *fmt = INTERP_FP_H; return 1;
    default: return 0;
    }
}

// QNAN/SNAN last, so "is this a NaN" is one comparison.
#define INTERP_FPC_ZERO 0u
#define INTERP_FPC_DENORM 1u
#define INTERP_FPC_NORM 2u
#define INTERP_FPC_INF 3u
#define INTERP_FPC_QNAN 4u
#define INTERP_FPC_SNAN 5u

static unsigned interp_fp_class(uint64_t bits, unsigned fmt) {
    unsigned mant = interp_fp_mant(fmt);
    uint64_t frac = bits & interp_fp_mant_mask(fmt);
    unsigned biased = (unsigned)((bits >> mant) & (uint64_t)interp_fp_inf_exp(fmt));
    if (biased == 0) return frac == 0 ? INTERP_FPC_ZERO : INTERP_FPC_DENORM;
    if (biased != interp_fp_inf_exp(fmt)) return INTERP_FPC_NORM;
    if (frac == 0) return INTERP_FPC_INF;
    return ((frac >> (mant - 1u)) & 1u) ? INTERP_FPC_QNAN : INTERP_FPC_SNAN;
}

// The first four are FPCR.RMode verbatim; RA (ties away) has no FPCR encoding -- FCVTA*/FRINTA name it.
#define INTERP_RM_RN 0u // to nearest, ties to even
#define INTERP_RM_RP 1u // toward +infinity
#define INTERP_RM_RM 2u // toward -infinity
#define INTERP_RM_RZ 3u // toward zero
#define INTERP_RM_RA 4u // to nearest, ties away from zero

static int interp_fp_host_round(unsigned rmode) {
    switch (rmode) {
    case INTERP_RM_RP: return FE_UPWARD;
    case INTERP_RM_RM: return FE_DOWNWARD;
    case INTERP_RM_RZ: return FE_TOWARDZERO;
    default: return FE_TONEAREST;
    }
}

static int interp_fp_round_away(unsigned rmode, unsigned sign, int round_bit, int sticky, unsigned lsb) {
    switch (rmode) {
    case INTERP_RM_RN: return round_bit && (sticky || lsb);
    case INTERP_RM_RA: return round_bit != 0;
    case INTERP_RM_RP: return !sign && (round_bit || sticky);
    case INTERP_RM_RM: return sign && (round_bit || sticky);
    default: return 0; // RZ truncates
    }
}

// Enter installs the guest rounding mode and clears the host's sticky flags; leave harvests them onto FPSR
// and RESTORES the host mode, so the engine's own C cannot inherit it. Barriers and volatiles stop GCC
// (FENV_ACCESS off) hoisting FP work out.
typedef struct {
    int host_round;
} interp_fpenv;

static void interp_fp_env_enter(interp_fpenv *env) {
    int want = interp_fp_host_round(INTERP_FPCR_RMODE(g_interp_fpcr));
    env->host_round = fegetround();
    if (env->host_round != want) (void)fesetround(want);
    (void)feclearexcept(FE_ALL_EXCEPT);
    __asm__ __volatile__("" ::: "memory");
}

static unsigned interp_fp_env_leave(interp_fpenv *env) {
    __asm__ __volatile__("" ::: "memory");
    int raised = fetestexcept(FE_ALL_EXCEPT);
    if (env->host_round != interp_fp_host_round(INTERP_FPCR_RMODE(g_interp_fpcr))) (void)fesetround(env->host_round);
    unsigned bits = 0;
    if (raised & FE_INVALID) bits |= INTERP_FPSR_IOC;
    if (raised & FE_DIVBYZERO) bits |= INTERP_FPSR_DZC;
    if (raised & FE_OVERFLOW) bits |= INTERP_FPSR_OFC;
    if (raised & FE_UNDERFLOW) bits |= INTERP_FPSR_UFC;
    if (raised & FE_INEXACT) bits |= INTERP_FPSR_IXC;
    return bits;
}

static double interp_fp_to_double(uint64_t bits) {
    double value;
    memcpy(&value, &bits, sizeof value);
    return value;
}

static uint64_t interp_fp_from_double(double value) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    return bits;
}

static float interp_fp_to_float(uint32_t bits) {
    float value;
    memcpy(&value, &bits, sizeof value);
    return value;
}

static uint64_t interp_fp_from_float(float value) {
    uint32_t bits;
    memcpy(&bits, &value, sizeof bits);
    return (uint64_t)bits;
}

// Exact re-encoding, so it raises nothing. A half denormal becomes a double NORMAL.
static double interp_fp_half_to_double(uint64_t bits) {
    uint64_t sign = (bits & 0x8000u) ? (UINT64_C(1) << 63) : 0;
    unsigned biased = (unsigned)((bits >> 10) & 0x1Fu);
    uint64_t frac = bits & 0x3FFu;
    if (biased == 0x1Fu) return interp_fp_to_double(sign | (UINT64_C(0x7FF) << 52) | (frac << 42));
    if (biased == 0) {
        if (frac == 0) return interp_fp_to_double(sign);
        int exponent = 1 - 15;
        while (!(frac & 0x400u)) {
            frac <<= 1;
            exponent--;
        }
        frac &= 0x3FFu;
        return interp_fp_to_double(sign | ((uint64_t)(exponent + 1023) << 52) | (frac << 42));
    }
    return interp_fp_to_double(sign | ((uint64_t)((int)biased - 15 + 1023) << 52) | (frac << 42));
}

// Widening to double is EXACT here, so comparisons and half arithmetic add no rounding. Screen NaNs first.
static double interp_fp_widen(uint64_t bits, unsigned fmt) {
    if (fmt == INTERP_FP_D) return interp_fp_to_double(bits);
    if (fmt == INTERP_FP_S) return (double)interp_fp_to_float((uint32_t)bits);
    return interp_fp_half_to_double(bits);
}

static uint64_t interp_fp_default_nan(unsigned fmt) {
    unsigned mant = interp_fp_mant(fmt);
    return ((uint64_t)interp_fp_inf_exp(fmt) << mant) | (UINT64_C(1) << (mant - 1u));
}

static uint64_t interp_fp_process_nan(uint64_t bits, unsigned fmt) {
    if (interp_fp_class(bits, fmt) == INTERP_FPC_SNAN) interp_fpsr_raise(INTERP_FPSR_IOC);
    if (INTERP_FPCR_DN(g_interp_fpcr)) return interp_fp_default_nan(fmt);
    return bits | (UINT64_C(1) << (interp_fp_mant(fmt) - 1u));
}

static int interp_fp_process_nans(unsigned fmt, unsigned count, const uint64_t *operands, uint64_t *result) {
    for (unsigned pass = 0; pass < 2; pass++)
        for (unsigned index = 0; index < count; index++) {
            unsigned cls = interp_fp_class(operands[index], fmt);
            if (cls == (pass == 0 ? INTERP_FPC_SNAN : INTERP_FPC_QNAN)) {
                *result = interp_fp_process_nan(operands[index], fmt);
                return 1;
            }
        }
    return 0;
}

// On an INPUT: FPCR.FZ governs single/double, FPCR.FZ16 half. FPSR.IDC means "denormal AND discarded" --
// but only at single and double: FPUnpack's half-precision branch flushes with no InputDenorm exception,
// so an FZ16 flush is SILENT. Measured across 15 half-precision forms this file already implemented:
// every FZ16 flush reported IDC here and none did under qemu-aarch64.
static uint64_t interp_fp_flush_input(uint64_t bits, unsigned fmt) {
    unsigned flush = fmt == INTERP_FP_H ? INTERP_FPCR_FZ16(g_interp_fpcr) : INTERP_FPCR_FZ(g_interp_fpcr);
    if (!flush || interp_fp_class(bits, fmt) != INTERP_FPC_DENORM) return bits;
    if (fmt != INTERP_FP_H) interp_fpsr_raise(INTERP_FPSR_IDC);
    return bits & interp_fp_sign_mask(fmt);
}

static uint64_t interp_fp_postprocess(unsigned fmt, uint64_t bits, unsigned raised) {
    unsigned cls = interp_fp_class(bits, fmt);
    if (cls >= INTERP_FPC_QNAN) {
        // No operand was a NaN, so this is an invalid operation and the host's NaN is not FPDefaultNaN.
        bits = interp_fp_default_nan(fmt);
        // A result the host already underflowed all the way to zero is just as tiny as a denormal one, and
        // FZ reports it the same way; the host's Precision flag for it is not AArch64's.
    } else if (cls == INTERP_FPC_DENORM || (cls == INTERP_FPC_ZERO && (raised & INTERP_FPSR_UFC))) {
        unsigned flush = fmt == INTERP_FP_H ? INTERP_FPCR_FZ16(g_interp_fpcr) : INTERP_FPCR_FZ(g_interp_fpcr);
        if (flush) {
            bits &= interp_fp_sign_mask(fmt);
            // Underflow ALONE: FPRoundBase returns the flushed zero before the Inexact test, so the host's
            // IXC for the denormal it actually computed must not survive.
            raised = (raised & ~(unsigned)INTERP_FPSR_IXC) | INTERP_FPSR_UFC;
        }
    }
    interp_fpsr_raise(raised);
    return bits;
}

// Round (-1)^sign * significand * 2^exponent into `fmt` under `rmode`; OFC/UFC/IXC are OR-ed into *raised.
// The only soft-float rounder here: unsigned-64 -> FP, conversion to half and the fixed-point forms have no
// correctly-rounded baseline x86-64 instruction. Clamping `lsb_exp` to the subnormal ulp is AArch64's
// BEFORE-rounding tininess rule.
static uint64_t interp_fp_pack(unsigned sign, uint64_t significand, int exponent, unsigned fmt, unsigned rmode,
                               unsigned *raised) {
    unsigned mant = interp_fp_mant(fmt);
    uint64_t sign_bit = sign ? interp_fp_sign_mask(fmt) : 0;
    if (significand == 0) return sign_bit; // exact zero keeps its sign

    int value_exp = exponent + (63 - __builtin_clzll(significand));
    int min_exp = 1 - interp_fp_bias(fmt) - (int)mant; // exponent of the smallest subnormal's last bit
    int lsb_exp = value_exp - (int)mant;
    int tiny = lsb_exp < min_exp;
    if (tiny) lsb_exp = min_exp;

    // A shift of 64 or more arises only in the clamped (tiny) case.
    int shift = lsb_exp - exponent;
    uint64_t quotient;
    int round_bit = 0, sticky = 0;
    if (shift <= 0) {
        // Cannot overflow: unclamped the leading one lands exactly at bit `mant`, clamped strictly below.
        quotient = significand << (unsigned)(-shift);
    } else if (shift >= 64) {
        quotient = 0;
        round_bit = shift == 64 ? (int)((significand >> 63) & 1u) : 0;
        sticky = (significand & (shift == 64 ? ~(UINT64_C(1) << 63) : UINT64_MAX)) != 0;
    } else {
        quotient = significand >> (unsigned)shift;
        round_bit = (int)((significand >> (unsigned)(shift - 1)) & 1u);
        sticky = shift > 1 && (significand & ((UINT64_C(1) << (unsigned)(shift - 1)) - 1u)) != 0;
    }
    int inexact = round_bit | sticky;
    if (interp_fp_round_away(rmode, sign, round_bit, sticky, (unsigned)(quotient & 1u))) quotient++;
    if (inexact) *raised |= INTERP_FPSR_IXC;

    if ((quotient >> mant) == 0) {
        if (tiny && inexact) *raised |= INTERP_FPSR_UFC;
        return sign_bit | quotient;
    }
    // A round-up carries into the next binade; in the subnormal branch such a carry IS the smallest normal.
    if (quotient >> (mant + 1u)) {
        quotient >>= 1;
        lsb_exp++;
    }
    if (tiny && inexact) *raised |= INTERP_FPSR_UFC; // tiny before rounding, normal after: still Underflow
    int biased = lsb_exp + (int)mant + interp_fp_bias(fmt);
    if (biased >= (int)interp_fp_inf_exp(fmt)) {
        // IEEE-754: infinity if the mode rounds AWAY from zero at this sign, else the largest finite.
        *raised |= INTERP_FPSR_OFC | INTERP_FPSR_IXC;
        int away = rmode == INTERP_RM_RN || rmode == INTERP_RM_RA || (rmode == INTERP_RM_RP && !sign) ||
                   (rmode == INTERP_RM_RM && sign);
        if (away) return sign_bit | ((uint64_t)interp_fp_inf_exp(fmt) << mant);
        return sign_bit | ((uint64_t)(interp_fp_inf_exp(fmt) - 1u) << mant) | interp_fp_mant_mask(fmt);
    }
    return sign_bit | ((uint64_t)(unsigned)biased << mant) | (quotient & interp_fp_mant_mask(fmt));
}

static uint64_t interp_fp_half_from_double(double value, unsigned rmode, unsigned *raised) {
    uint64_t bits = interp_fp_from_double(value);
    unsigned sign = (unsigned)(bits >> 63);
    unsigned biased = (unsigned)((bits >> 52) & 0x7FFu);
    uint64_t frac = bits & ((UINT64_C(1) << 52) - 1u);
    if (biased == 0x7FFu)
        return frac == 0 ? (sign ? 0xFC00u : 0x7C00u) : 0x7E00u; // postprocess substitutes the default NaN
    if (biased == 0 && frac == 0) return sign ? 0x8000u : 0u;
    uint64_t significand = biased == 0 ? frac : (frac | (UINT64_C(1) << 52));
    int exponent = (int)(biased == 0 ? 1u : biased) - 1023 - 52;
    return interp_fp_pack(sign, significand, exponent, INTERP_FP_H, rmode, raised);
}

#define INTERP_FPOP_ADD 0u
#define INTERP_FPOP_SUB 1u
#define INTERP_FPOP_MUL 2u
#define INTERP_FPOP_DIV 3u

// Half computes in the host's DOUBLE unit then rounds to half: exact, since 53 bits exceed the 24 that
// binary16 needs to survive a second rounding.
static uint64_t interp_fp_arith(unsigned fmt, unsigned op, uint64_t a, uint64_t b) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float left = interp_fp_to_float((uint32_t)a), right = interp_fp_to_float((uint32_t)b);
        interp_fp_env_enter(&env);
        volatile float x = left, y = right, r = 0;
        switch (op) {
        case INTERP_FPOP_ADD: r = x + y; break;
        case INTERP_FPOP_SUB: r = x - y; break;
        case INTERP_FPOP_MUL: r = x * y; break;
        default: r = x / y; break;
        }
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double left = interp_fp_widen(a, fmt), right = interp_fp_widen(b, fmt);
        interp_fp_env_enter(&env);
        volatile double x = left, y = right, r = 0;
        switch (op) {
        case INTERP_FPOP_ADD: r = x + y; break;
        case INTERP_FPOP_SUB: r = x - y; break;
        case INTERP_FPOP_MUL: r = x * y; break;
        default: r = x / y; break;
        }
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

static uint64_t interp_fp_sqrt(unsigned fmt, uint64_t a) {
    a = interp_fp_flush_input(a, fmt);
    uint64_t nan;
    if (interp_fp_process_nans(fmt, 1, &a, &nan)) return nan;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float operand = interp_fp_to_float((uint32_t)a);
        interp_fp_env_enter(&env);
        volatile float x = operand, r;
        r = sqrtf(x);
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double operand = interp_fp_widen(a, fmt);
        interp_fp_env_enter(&env);
        volatile double x = operand, r;
        r = sqrt(x);
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

// addend + a*b with a SINGLE rounding, which is what C's fma() is defined to be; `x*y + z` rounds twice.
static uint64_t interp_fp_muladd(unsigned fmt, uint64_t addend, uint64_t a, uint64_t b) {
    addend = interp_fp_flush_input(addend, fmt);
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    int zero_times_inf = (class_a == INTERP_FPC_INF && class_b == INTERP_FPC_ZERO) ||
                         (class_a == INTERP_FPC_ZERO && class_b == INTERP_FPC_INF);
    uint64_t operands[3] = {addend, a, b}, nan;
    int have_nan = interp_fp_process_nans(fmt, 3, operands, &nan);
    // A QUIET NaN addend does NOT win over an invalid multiply (inf*0 gives the default NaN); a SIGNALLING
    // one does.
    if (zero_times_inf && interp_fp_class(addend, fmt) == INTERP_FPC_QNAN) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_default_nan(fmt);
    }
    if (have_nan) return nan;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float left = interp_fp_to_float((uint32_t)a), right = interp_fp_to_float((uint32_t)b),
              extra = interp_fp_to_float((uint32_t)addend);
        interp_fp_env_enter(&env);
        volatile float x = left, y = right, z = extra, r;
        r = fmaf(x, y, z);
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double left = interp_fp_widen(a, fmt), right = interp_fp_widen(b, fmt), extra = interp_fp_widen(addend, fmt);
        interp_fp_env_enter(&env);
        volatile double x = left, y = right, z = extra, r;
        r = fma(x, y, z);
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

// FMULX is FMUL except at 0 * inf, which is the ONLY reason the instruction exists: it yields +-2.0 with no
// Invalid, so a reciprocal-estimate refinement step stays finite at the extremes instead of turning into a NaN.
static uint64_t interp_fp_mulx(unsigned fmt, uint64_t a, uint64_t b) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    if ((class_a == INTERP_FPC_INF && class_b == INTERP_FPC_ZERO) ||
        (class_a == INTERP_FPC_ZERO && class_b == INTERP_FPC_INF))
        return ((a ^ b) & interp_fp_sign_mask(fmt)) | ((uint64_t)(interp_fp_bias(fmt) + 1) << interp_fp_mant(fmt));
    return interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b);
}

// The NM forms swap a QUIET NaN for the losing infinity; a SIGNALLING NaN still propagates with Invalid.
static uint64_t interp_fp_minmax(unsigned fmt, uint64_t a, uint64_t b, int want_max, int numeric) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    if (numeric) {
        uint64_t losing = ((uint64_t)interp_fp_inf_exp(fmt) << interp_fp_mant(fmt)) |
                          (want_max ? interp_fp_sign_mask(fmt) : UINT64_C(0));
        int a_quiet = interp_fp_class(a, fmt) == INTERP_FPC_QNAN, b_quiet = interp_fp_class(b, fmt) == INTERP_FPC_QNAN;
        if (a_quiet && !b_quiet)
            a = losing;
        else if (b_quiet && !a_quiet)
            b = losing;
    }
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    double x = interp_fp_widen(a, fmt), y = interp_fp_widen(b, fmt);
    uint64_t chosen = (want_max ? (x > y) : (x < y)) ? a : b;
    if (interp_fp_class(chosen, fmt) == INTERP_FPC_ZERO) {
        // +0 and -0 compare EQUAL: FPMax ANDs the operand signs (+0 wins), FPMin ORs them (-0 wins).
        unsigned sign_a = (a & interp_fp_sign_mask(fmt)) != 0, sign_b = (b & interp_fp_sign_mask(fmt)) != 0;
        unsigned sign = want_max ? (sign_a & sign_b) : (sign_a | sign_b);
        return sign ? interp_fp_sign_mask(fmt) : UINT64_C(0);
    }
    return chosen;
}

// `quiet_signals` selects FCMPE/FCCMPE, which raise Invalid for a quiet NaN too.
static void interp_fp_compare(struct cpu *cpu, unsigned fmt, uint64_t a, uint64_t b, int quiet_signals) {
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    if (class_a >= INTERP_FPC_QNAN || class_b >= INTERP_FPC_QNAN) {
        if (quiet_signals || class_a == INTERP_FPC_SNAN || class_b == INTERP_FPC_SNAN)
            interp_fpsr_raise(INTERP_FPSR_IOC);
        interp_set_flags(cpu, 0, 0, 1, 1); // unordered
        return;
    }
    double x = interp_fp_widen(a, fmt), y = interp_fp_widen(b, fmt);
    if (x == y)
        interp_set_flags(cpu, 0, 1, 1, 0);
    else if (x < y)
        interp_set_flags(cpu, 1, 0, 0, 0);
    else
        interp_set_flags(cpu, 0, 0, 1, 0);
}

// `exact` selects FRINTX, which raises Inexact when the value moves. Same format in and out.
static uint64_t interp_fp_round_integral(unsigned fmt, uint64_t bits, unsigned rmode, int exact) {
    bits = interp_fp_flush_input(bits, fmt);
    uint64_t nan;
    if (interp_fp_process_nans(fmt, 1, &bits, &nan)) return nan;
    unsigned cls = interp_fp_class(bits, fmt);
    if (cls == INTERP_FPC_INF || cls == INTERP_FPC_ZERO) return bits;

    unsigned mant = interp_fp_mant(fmt);
    unsigned sign = (bits & interp_fp_sign_mask(fmt)) != 0;
    uint64_t frac = bits & interp_fp_mant_mask(fmt);
    unsigned biased = (unsigned)((bits >> mant) & (uint64_t)interp_fp_inf_exp(fmt));
    uint64_t significand = biased == 0 ? frac : (frac | (UINT64_C(1) << mant));
    int exponent = (int)(biased == 0 ? 1u : biased) - interp_fp_bias(fmt) - (int)mant;
    if (exponent >= 0) return bits; // already integral

    unsigned shift = (unsigned)(-exponent);
    uint64_t magnitude;
    int round_bit, sticky;
    if (shift >= 64) {
        magnitude = 0;
        round_bit = shift == 64 ? (int)((significand >> 63) & 1u) : 0;
        sticky = (significand & (shift == 64 ? ~(UINT64_C(1) << 63) : UINT64_MAX)) != 0;
    } else {
        magnitude = significand >> shift;
        round_bit = (int)((significand >> (shift - 1u)) & 1u);
        sticky = shift > 1 && (significand & ((UINT64_C(1) << (shift - 1u)) - 1u)) != 0;
    }
    if (interp_fp_round_away(rmode, sign, round_bit, sticky, (unsigned)(magnitude & 1u))) magnitude++;
    if (exact && (round_bit || sticky)) interp_fpsr_raise(INTERP_FPSR_IXC);
    // A zero result keeps the operand's sign: FRINTM of -0.4 is -1.0, FRINTZ of -0.4 is -0.0.
    unsigned raised = 0;
    return interp_fp_pack(sign, magnitude, 0, fmt, INTERP_RM_RZ, &raised);
}

static uint64_t interp_fp_convert_nan(uint64_t bits, unsigned from, unsigned to) {
    if (interp_fp_class(bits, from) == INTERP_FPC_SNAN) interp_fpsr_raise(INTERP_FPSR_IOC);
    if (INTERP_FPCR_DN(g_interp_fpcr)) return interp_fp_default_nan(to);
    unsigned from_mant = interp_fp_mant(from), to_mant = interp_fp_mant(to);
    uint64_t payload = bits & interp_fp_mant_mask(from);
    payload = to_mant >= from_mant ? payload << (to_mant - from_mant) : payload >> (from_mant - to_mant);
    uint64_t sign = (bits & interp_fp_sign_mask(from)) ? interp_fp_sign_mask(to) : 0;
    return sign | ((uint64_t)interp_fp_inf_exp(to) << to_mant) | payload | (UINT64_C(1) << (to_mant - 1u));
}

// Widening is exact; double -> single is the one narrowing the host does identically.
static uint64_t interp_fp_convert(unsigned from, unsigned to, uint64_t bits) {
    if (interp_fp_class(bits, from) >= INTERP_FPC_QNAN) return interp_fp_convert_nan(bits, from, to);
    bits = interp_fp_flush_input(bits, from);
    if (interp_fp_width(to) > interp_fp_width(from)) {
        double wide = interp_fp_widen(bits, from);
        if (to == INTERP_FP_D) return interp_fp_postprocess(to, interp_fp_from_double(wide), 0);
        return interp_fp_postprocess(to, interp_fp_from_float((float)wide), 0);
    }
    unsigned raised = 0;
    uint64_t out;
    if (to == INTERP_FP_H) {
        out = interp_fp_half_from_double(interp_fp_widen(bits, from), INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    } else {
        double wide = interp_fp_to_double(bits); // from == INTERP_FP_D, to == INTERP_FP_S
        interp_fpenv env;
        interp_fp_env_enter(&env);
        volatile double x = wide;
        volatile float r = (float)x;
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    }
    return interp_fp_postprocess(to, out, raised);
}

// FCVTXN's FPRounding_ODD: truncate, then force the low bit whenever anything was lost, so a narrowing
// never lands on a value a second narrowing could round the wrong way. Overflow gives the largest finite.
// ODD replaces FPCR.RMode rather than composing with it, hence the (thread-local) override around the one
// host conversion; DN and FZ still come from the guest's, so it is restored before postprocess.
static uint64_t interp_fp_convert_odd(uint64_t bits) {
    if (interp_fp_class(bits, INTERP_FP_D) >= INTERP_FPC_QNAN)
        return interp_fp_convert_nan(bits, INTERP_FP_D, INTERP_FP_S);
    bits = interp_fp_flush_input(bits, INTERP_FP_D);
    double wide = interp_fp_to_double(bits);
    uint64_t saved_fpcr = g_interp_fpcr;
    g_interp_fpcr = (saved_fpcr & ~(UINT64_C(3) << 22)) | ((uint64_t)INTERP_RM_RZ << 22);
    interp_fpenv env;
    interp_fp_env_enter(&env);
    volatile double x = wide;
    volatile float r = (float)x;
    unsigned raised = interp_fp_env_leave(&env);
    g_interp_fpcr = saved_fpcr;
    uint64_t out = interp_fp_from_float(r);
    if (raised & INTERP_FPSR_IXC) out |= 1u;
    return interp_fp_postprocess(INTERP_FP_S, out, raised);
}

// x86's RCPPS is an implementation-defined approximation, so this engine answers it exactly. AArch64's
// estimates are the opposite: the ARM ARM specifies an 8-bit-mantissa table and the exact extraction, so
// two conforming implementations agree bit for bit and an approximation -- however close -- is a wrong
// answer. These two are the DDI 0487 shared pseudocode RecipEstimate/RecipSqrtEstimate transcribed; every
// entry of the 256- and 384-entry tables they generate was read back off qemu-aarch64 through six
// instruction paths (FRECPE.s/.d, URECPE, FRSQRTE.s/.d, URSQRTE), which agreed with each other and with
// these, and tests/compat/completeness/aarch64/neon_recip.c re-checks all 640 on whatever CPU runs it.
static unsigned interp_recip_estimate(unsigned a) {
    a = a * 2u + 1u; // to nearest, in units of 1/512
    unsigned b = (1u << 19) / a;
    return (b + 1u) / 2u; // to nearest
}

static unsigned interp_recip_sqrt_estimate(unsigned a) {
    a = a < 256u ? a * 2u + 1u : (((a >> 1) << 1) + 1u) * 2u; // 0.25..0.5 keeps its low bit, 0.5..1.0 drops it
    uint64_t b = 512;
    while ((uint64_t)a * (b + 1u) * (b + 1u) < (UINT64_C(1) << 28))
        b++;
    return (unsigned)((b + 1u) / 2u);
}

#define INTERP_FRAC52 ((UINT64_C(1) << 52) - 1u)

// Both estimates renormalise the operand into a 52-bit fraction, take 8 bits of it through the table and
// rebuild; the pseudocode spells the exponent arithmetic per format, and every constant is a bias multiple.
static uint64_t interp_fp_recip_estimate(unsigned fmt, uint64_t a) {
    a = interp_fp_flush_input(a, fmt);
    unsigned mant = interp_fp_mant(fmt), cls = interp_fp_class(a, fmt), inf_exp = interp_fp_inf_exp(fmt);
    int bias = interp_fp_bias(fmt);
    uint64_t sign = a & interp_fp_sign_mask(fmt);
    if (cls >= INTERP_FPC_QNAN) return interp_fp_process_nan(a, fmt);
    if (cls == INTERP_FPC_INF) return sign;
    if (cls == INTERP_FPC_ZERO) {
        interp_fpsr_raise(INTERP_FPSR_DZC);
        return sign | ((uint64_t)inf_exp << mant);
    }
    uint64_t frac = a & interp_fp_mant_mask(fmt);
    int exp = (int)((a >> mant) & inf_exp);
    // |x| < 2^-(bias+1) -- a denormal with its top two fraction bits clear -- reciprocates to an overflow.
    if (exp == 0 && (frac >> (mant - 2u)) == 0) {
        unsigned rmode = INTERP_FPCR_RMODE(g_interp_fpcr);
        int to_inf = rmode == INTERP_RM_RN || (rmode == INTERP_RM_RP && !sign) || (rmode == INTERP_RM_RM && sign);
        interp_fpsr_raise(INTERP_FPSR_OFC | INTERP_FPSR_IXC);
        return sign |
               (to_inf ? ((uint64_t)inf_exp << mant) : (((uint64_t)(inf_exp - 1u) << mant) | interp_fp_mant_mask(fmt)));
    }
    // The mirror case: FZ turns a reciprocal that would be denormal into zero, Underflow and no Inexact.
    if ((fmt == INTERP_FP_H ? INTERP_FPCR_FZ16(g_interp_fpcr) : INTERP_FPCR_FZ(g_interp_fpcr)) && exp >= 2 * bias - 1) {
        interp_fpsr_raise(INTERP_FPSR_UFC);
        return sign;
    }
    uint64_t fraction = frac << (52u - mant);
    if (exp == 0) { // at most two shifts: a third would have been the overflow case above
        if ((fraction >> 51) == 0) {
            exp = -1;
            fraction = (fraction << 2) & INTERP_FRAC52;
        } else {
            fraction = (fraction << 1) & INTERP_FRAC52;
        }
    }
    unsigned estimate = interp_recip_estimate(256u + (unsigned)((fraction >> 44) & 0xFFu));
    int result_exp = 2 * bias - 1 - exp;
    fraction = (uint64_t)(estimate & 0xFFu) << 44;
    // A result_exp the estimate pushed below the normal range comes back as an explicit leading one.
    if (result_exp == 0) {
        fraction = (UINT64_C(1) << 51) | (fraction >> 1);
    } else if (result_exp == -1) {
        fraction = (UINT64_C(1) << 50) | (fraction >> 2);
        result_exp = 0;
    }
    return sign | ((uint64_t)(unsigned)result_exp << mant) | (fraction >> (52u - mant));
}

static uint64_t interp_fp_rsqrt_estimate(unsigned fmt, uint64_t a) {
    a = interp_fp_flush_input(a, fmt);
    unsigned mant = interp_fp_mant(fmt), cls = interp_fp_class(a, fmt), inf_exp = interp_fp_inf_exp(fmt);
    uint64_t sign = a & interp_fp_sign_mask(fmt);
    if (cls >= INTERP_FPC_QNAN) return interp_fp_process_nan(a, fmt);
    if (cls == INTERP_FPC_ZERO) {
        interp_fpsr_raise(INTERP_FPSR_DZC);
        return sign | ((uint64_t)inf_exp << mant);
    }
    if (sign) { // -0 took the branch above; every other negative is Invalid, not a signed result
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_default_nan(fmt);
    }
    if (cls == INTERP_FPC_INF) return 0;
    uint64_t fraction = (a & interp_fp_mant_mask(fmt)) << (52u - mant);
    int exp = (int)((a >> mant) & inf_exp);
    if (exp == 0) { // no bound needed: a denormal has a set bit to normalise onto
        while ((fraction >> 51) == 0) {
            fraction = (fraction << 1) & INTERP_FRAC52;
            exp--;
        }
        fraction = (fraction << 1) & INTERP_FRAC52;
    }
    // The exponent's PARITY survives the scaling, because a square root halves it: an odd one scales into
    // 0.25..0.5 and indexes the table's lower half.
    unsigned scaled = ((unsigned)exp & 1u) ? 128u + (unsigned)((fraction >> 45) & 0x7Fu)
                                           : 256u + (unsigned)((fraction >> 44) & 0xFFu);
    int result_exp = (3 * interp_fp_bias(fmt) - 1 - exp) / 2;
    unsigned estimate = interp_recip_sqrt_estimate(scaled);
    return ((uint64_t)(unsigned)result_exp << mant) | ((uint64_t)(estimate & 0xFFu) << (mant - 8u));
}

// FRECPX is NOT FRECPE: no table, no estimate. It reflects the exponent exactly and zeroes the mantissa,
// which is what a range-reduction step needs. FZ never changes the answer -- the exponent comes from the
// raw operand -- but flushing a denormal input still reports it at single and double.
static uint64_t interp_fp_recpx(unsigned fmt, uint64_t a) {
    unsigned mant = interp_fp_mant(fmt), inf_exp = interp_fp_inf_exp(fmt);
    if (interp_fp_class(a, fmt) >= INTERP_FPC_QNAN) return interp_fp_process_nan(a, fmt);
    (void)interp_fp_flush_input(a, fmt);
    unsigned exp = (unsigned)((a >> mant) & inf_exp);
    return (a & interp_fp_sign_mask(fmt)) | ((uint64_t)(exp == 0 ? inf_exp - 1u : (~exp & inf_exp)) << mant);
}

// URECPE / URSQRTE: the same two tables read as unsigned Q0.32 fixed point. No FPCR, no FPSR.
static uint64_t interp_uint_recip_estimate(uint64_t a, int sqrt_form) {
    uint32_t value = (uint32_t)a;
    if (sqrt_form ? (value >> 30) == 0 : (value >> 31) == 0) return UINT64_C(0xFFFFFFFF);
    unsigned estimate = sqrt_form ? interp_recip_sqrt_estimate(value >> 23) : interp_recip_estimate(value >> 23);
    return (uint64_t)estimate << 23;
}

// FRECPS / FRSQRTS, the Newton-Raphson steps. Three things do not follow from the arithmetic: op1 is
// NEGATED before the unpack, so a NaN operand propagates with its sign FLIPPED; 0*inf yields 2.0 or 1.5
// rather than the Invalid a bare multiply would raise; and inf*finite yields a signed infinity directly.
static uint64_t interp_fp_recip_step(unsigned fmt, uint64_t a, uint64_t b, int sqrt_form) {
    unsigned mant = interp_fp_mant(fmt);
    a ^= interp_fp_sign_mask(fmt);
    a = interp_fp_flush_input(a, fmt);
    b = interp_fp_flush_input(b, fmt);
    uint64_t operands[2] = {a, b}, nan;
    if (interp_fp_process_nans(fmt, 2, operands, &nan)) return nan;
    unsigned class_a = interp_fp_class(a, fmt), class_b = interp_fp_class(b, fmt);
    int inf_a = class_a == INTERP_FPC_INF, inf_b = class_b == INTERP_FPC_INF;
    if ((inf_a && class_b == INTERP_FPC_ZERO) || (class_a == INTERP_FPC_ZERO && inf_b))
        return sqrt_form ? (((uint64_t)interp_fp_bias(fmt) << mant) | (UINT64_C(1) << (mant - 1u)))
                         : ((uint64_t)(interp_fp_bias(fmt) + 1) << mant);
    if (inf_a || inf_b) return ((a ^ b) & interp_fp_sign_mask(fmt)) | ((uint64_t)interp_fp_inf_exp(fmt) << mant);

    // (3 - a*b)/2 is ONE rounding, so the halving must stay out of it: 1.5 + (a/2)*b is a single fma when
    // a/2 is exact. When it is not, |a| is below 2^(2-bias) and |a*b| below 8, so halving afterwards
    // neither overflows nor lands in the subnormals -- which halving 3 - a*b on its own can both do.
    int prehalve = sqrt_form && ((a >> mant) & interp_fp_inf_exp(fmt)) >= 2u;
    if (prehalve) a -= UINT64_C(1) << mant;
    interp_fpenv env;
    unsigned raised;
    uint64_t out;
    if (fmt == INTERP_FP_S) {
        float left = interp_fp_to_float((uint32_t)a), right = interp_fp_to_float((uint32_t)b);
        interp_fp_env_enter(&env);
        volatile float x = left, y = right, r;
        r = fmaf(x, y, sqrt_form ? (prehalve ? 1.5f : 3.0f) : 2.0f);
        if (sqrt_form && !prehalve) r = r * 0.5f;
        raised = interp_fp_env_leave(&env);
        out = interp_fp_from_float(r);
    } else {
        double left = interp_fp_widen(a, fmt), right = interp_fp_widen(b, fmt);
        interp_fp_env_enter(&env);
        volatile double x = left, y = right, r;
        r = fma(x, y, sqrt_form ? (prehalve ? 1.5 : 3.0) : 2.0);
        if (sqrt_form && !prehalve) r = r * 0.5;
        raised = interp_fp_env_leave(&env);
        out = fmt == INTERP_FP_D ? interp_fp_from_double(r)
                                 : interp_fp_half_from_double(r, INTERP_FPCR_RMODE(g_interp_fpcr), &raised);
    }
    return interp_fp_postprocess(fmt, out, raised);
}

// SatQ()'s out-of-range value; a 32-bit destination comes back sign-extended to 64.
static uint64_t interp_fp_int_saturate(unsigned sign, unsigned dest_bits, int is_signed) {
    if (!is_signed) return sign ? UINT64_C(0) : (dest_bits == 64 ? UINT64_MAX : ((UINT64_C(1) << dest_bits) - 1u));
    uint64_t limit = UINT64_C(1) << (dest_bits - 1u);
    return sign ? (UINT64_C(0) - limit) : (limit - 1u);
}

// FP to a `dest_bits`-wide integer, keeping `fbits` fractional bits (a scale is only an exponent addend).
// ORDER matters: round to an integer FIRST, then range-check -- FCVTZU of -0.5 is 0 with Inexact and no
// Invalid, and a NaN becomes 0 before the check. The two exceptions are exclusive (FPToFixed's if/elsif):
// -1.5 saturates with Invalid ALONE, Inexact suppressed.
static uint64_t interp_fp_to_int(unsigned fmt, uint64_t bits, unsigned dest_bits, int is_signed, unsigned rmode,
                                 unsigned fbits) {
    bits = interp_fp_flush_input(bits, fmt);
    unsigned cls = interp_fp_class(bits, fmt);
    unsigned sign = (bits & interp_fp_sign_mask(fmt)) != 0;
    if (cls >= INTERP_FPC_QNAN) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return 0;
    }
    if (cls == INTERP_FPC_INF) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_int_saturate(sign, dest_bits, is_signed);
    }
    if (cls == INTERP_FPC_ZERO) return 0;

    unsigned mant = interp_fp_mant(fmt);
    uint64_t frac = bits & interp_fp_mant_mask(fmt);
    unsigned biased = (unsigned)((bits >> mant) & (uint64_t)interp_fp_inf_exp(fmt));
    uint64_t significand = biased == 0 ? frac : (frac | (UINT64_C(1) << mant));
    int exponent = (int)(biased == 0 ? 1u : biased) - interp_fp_bias(fmt) - (int)mant + (int)fbits;

    uint64_t magnitude = 0;
    int inexact = 0, too_large = 0;
    if (exponent >= 0) {
        // Already an integer, so nothing is inexact -- but it may not fit 64 bits, let alone the destination.
        // exponent == 0 must be spelled out: `significand >> 64` is masked to a no-op by the host and made
        // every scaled conversion whose result lands exactly at 2^0 saturate.
        too_large = exponent >= 64 || (exponent > 0 && (significand >> (64 - (unsigned)exponent)) != 0);
        if (!too_large) magnitude = significand << (unsigned)exponent;
    } else {
        unsigned shift = (unsigned)(-exponent);
        int round_bit, sticky;
        if (shift >= 64) {
            round_bit = shift == 64 ? (int)((significand >> 63) & 1u) : 0;
            sticky = (significand & (shift == 64 ? ~(UINT64_C(1) << 63) : UINT64_MAX)) != 0;
        } else {
            magnitude = significand >> shift;
            round_bit = (int)((significand >> (shift - 1u)) & 1u);
            sticky = shift > 1 && (significand & ((UINT64_C(1) << (shift - 1u)) - 1u)) != 0;
        }
        inexact = round_bit | sticky;
        if (interp_fp_round_away(rmode, sign, round_bit, sticky, (unsigned)(magnitude & 1u))) magnitude++;
    }
    // FPToFixed's exceptions are an if/elsif: a conversion that saturates raises Invalid and NOT Inexact.
    if (too_large) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_int_saturate(sign, dest_bits, is_signed);
    }
    if (!is_signed) {
        if (sign && magnitude != 0) {
            interp_fpsr_raise(INTERP_FPSR_IOC);
            return 0;
        }
        if (!sign && dest_bits < 64 && magnitude > ((UINT64_C(1) << dest_bits) - 1u)) {
            interp_fpsr_raise(INTERP_FPSR_IOC);
            return interp_fp_int_saturate(0, dest_bits, 0);
        }
        if (inexact) interp_fpsr_raise(INTERP_FPSR_IXC);
        return sign ? UINT64_C(0) : magnitude;
    }
    uint64_t limit = UINT64_C(1) << (dest_bits - 1u);
    if (sign) {
        if (magnitude > limit) {
            interp_fpsr_raise(INTERP_FPSR_IOC);
            return interp_fp_int_saturate(1, dest_bits, 1);
        }
        if (inexact) interp_fpsr_raise(INTERP_FPSR_IXC);
        return UINT64_C(0) - magnitude;
    }
    if (magnitude >= limit) {
        interp_fpsr_raise(INTERP_FPSR_IOC);
        return interp_fp_int_saturate(0, dest_bits, 1);
    }
    if (inexact) interp_fpsr_raise(INTERP_FPSR_IXC);
    return magnitude;
}

static uint64_t interp_fp_from_int(unsigned fmt, uint64_t value, unsigned source_bits, int is_signed, unsigned rmode,
                                   unsigned fbits) {
    unsigned sign = 0;
    uint64_t magnitude;
    // Any width, not just 32: a 16-bit source (SCVTF Vd.8H, Vn.8H, #fbits) arrives zero-extended, and
    // testing only for 32 made every negative half-width input convert as if it were unsigned.
    uint64_t width_mask = source_bits >= 64 ? UINT64_MAX : ((UINT64_C(1) << source_bits) - 1u);
    value &= width_mask;
    if (is_signed) {
        uint64_t sign_bit = UINT64_C(1) << (source_bits - 1u);
        int64_t signed_value = (int64_t)((value ^ sign_bit) - sign_bit);
        sign = signed_value < 0;
        magnitude = sign ? (UINT64_C(0) - (uint64_t)signed_value) : (uint64_t)signed_value;
    } else {
        magnitude = value;
    }
    // Two statements: as two arguments of one call, `raised` may be read before the pack that fills it.
    unsigned raised = 0;
    uint64_t packed = interp_fp_pack(sign, magnitude, -(int)fbits, fmt, rmode, &raised);
    return interp_fp_postprocess(fmt, packed, raised);
}

static uint64_t interp_fp_expand_imm(unsigned fmt, uint64_t imm8) {
    unsigned exp_bits = interp_fp_width(fmt) - interp_fp_mant(fmt) - 1u;
    unsigned mant = interp_fp_mant(fmt);
    uint64_t sign = (imm8 >> 7) & 1u;
    uint64_t b = (imm8 >> 6) & 1u;
    uint64_t exponent = (b ? 0u : 1u) << (exp_bits - 1u);
    if (b) exponent |= (((UINT64_C(1) << (exp_bits - 3u)) - 1u) << 2);
    exponent |= (imm8 >> 4) & 3u;
    return (sign << (interp_fp_width(fmt) - 1u)) | (exponent << mant) | ((imm8 & 0xFu) << (mant - 4u));
}

// AdvSIMD saturation. FPSR.QC is cumulative and sticky: any clamped result sets it, only MSR FPSR clears.

// The 64-bit element form is real, so the wide case detects overflow and clamps by the WRAPPED sign rather
// than computing in a wider type.
static uint64_t interp_sqadd_element(uint64_t a, uint64_t b, unsigned size, int subtract) {
    unsigned esize = 8u << size;
    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
    if (esize < 64) {
        int64_t result = subtract ? x - y : x + y;
        int64_t max = (int64_t)((UINT64_C(1) << (esize - 1u)) - 1u), min = -max - 1;
        if (result > max) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            result = max;
        } else if (result < min) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            result = min;
        }
        return (uint64_t)result & interp_element_mask(size);
    }
    int64_t result;
    int overflow = subtract ? __builtin_sub_overflow(x, y, &result) : __builtin_add_overflow(x, y, &result);
    if (overflow) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        result = result < 0 ? INT64_MAX : INT64_MIN;
    }
    return (uint64_t)result;
}

static uint64_t interp_uqadd_element(uint64_t a, uint64_t b, unsigned size, int subtract) {
    uint64_t mask = interp_element_mask(size);
    a &= mask;
    b &= mask;
    if (subtract) {
        if (a < b) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            return 0;
        }
        return a - b;
    }
    uint64_t sum;
    // A 64-bit element can carry out of the type itself, so the carry is tested as well as the mask.
    if (__builtin_add_overflow(a, b, &sum) || sum > mask) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        return mask;
    }
    return sum;
}

// Saturating NARROWING. `size` is the DESTINATION width and the source element is twice it. SQXTN is
// signed->signed, UQXTN unsigned->unsigned, SQXTUN signed->UNSIGNED.
static uint64_t interp_sat_narrow(uint64_t element, unsigned size, int source_signed, int dest_signed) {
    unsigned esize = 8u << size;
    uint64_t mask = interp_element_mask(size);
    if (source_signed) {
        int64_t value = (int64_t)interp_element_sext(element, size + 1u);
        if (dest_signed) {
            int64_t max = (int64_t)((UINT64_C(1) << (esize - 1u)) - 1u), min = -max - 1;
            if (value > max) {
                interp_fpsr_raise(INTERP_FPSR_QC);
                value = max;
            } else if (value < min) {
                interp_fpsr_raise(INTERP_FPSR_QC);
                value = min;
            }
            return (uint64_t)value & mask;
        }
        if (value < 0) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            return 0;
        }
        if ((uint64_t)value > mask) {
            interp_fpsr_raise(INTERP_FPSR_QC);
            return mask;
        }
        return (uint64_t)value;
    }
    uint64_t value = element & interp_element_mask(size + 1u);
    if (value > mask) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        return mask;
    }
    return value;
}

// Saturating DOUBLING multiply. All three forms compute 2*a*b and differ only in what they keep, so they
// share one 128-bit product: at esize 32 the doubling itself overflows int64 for INT32_MIN * INT32_MIN
// (2 * 2^62 == 2^63), which is exactly the input that must saturate rather than wrap.
static __int128 interp_sqdmul_wide(uint64_t a, uint64_t b, unsigned size) {
    return (__int128)2 * (int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size);
}

static uint64_t interp_signed_sat(__int128 value, unsigned size) {
    __int128 max = ((__int128)1 << ((8u << size) - 1u)) - 1, min = -max - 1;
    if (value > max) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        value = max;
    } else if (value < min) {
        interp_fpsr_raise(INTERP_FPSR_QC);
        value = min;
    }
    return (uint64_t)value & interp_element_mask(size);
}

// SQDMULL: the doubled product kept at DOUBLE width. SQDMLAL/SQDMLSL saturate this, then saturate the
// accumulate separately -- two independent chances to set QC.
static uint64_t interp_sqdmull_element(uint64_t a, uint64_t b, unsigned size) {
    return interp_signed_sat(interp_sqdmul_wide(a, b, size), size + 1u);
}

// SQDMULH keeps the HIGH half; SQRDMULH adds half an element first, so its tie rounds away from zero.
static uint64_t interp_sqdmulh_element(uint64_t a, uint64_t b, unsigned size, int rounding) {
    unsigned esize = 8u << size;
    __int128 product = interp_sqdmul_wide(a, b, size);
    if (rounding) product += (__int128)1 << (esize - 1u);
    return interp_signed_sat(product >> esize, size);
}

// SQRDMLAH/SQRDMLSH (FEAT_RDM): the accumulator joins the doubled product SHIFTED UP by esize, so the
// rounding constant and the single saturation see the whole expression -- not a saturated product plus a
// saturated add, which is what SQDMLAL does and what gives the two forms different results.
static uint64_t interp_sqrdmlah_element(uint64_t accumulator, uint64_t a, uint64_t b, unsigned size, int subtract) {
    unsigned esize = 8u << size;
    __int128 product = interp_sqdmul_wide(a, b, size);
    __int128 total = ((__int128)(int64_t)interp_element_sext(accumulator, size) << esize) +
                     (subtract ? -product : product) + ((__int128)1 << (esize - 1u));
    return interp_signed_sat(total >> esize, size);
}

static void interp_poly_mul(uint64_t a, uint64_t b, unsigned bits, uint64_t *low, uint64_t *high) {
    uint64_t result_low = 0, result_high = 0;
    for (unsigned bit = 0; bit < bits; bit++) {
        if (!((b >> bit) & 1u)) continue;
        // A shift by 0 must not become a shift of the high word by 64, which is undefined in C.
        result_low ^= a << bit;
        if (bit) result_high ^= a >> (64u - bit);
    }
    *low = result_low;
    *high = result_high;
}

// AES, SHA1 and SHA256. hwcap 0x1fb advertises HWCAP_AES/PMULL/SHA1/SHA2, so guest ifunc resolvers pick
// these paths. The tables are GENERATED (GF(2^8) inverse then the FIPS-197 affine transform).
static const uint8_t interp_aes_sbox[256] = {
    0x63, 0x7C, 0x77, 0x7B, 0xF2, 0x6B, 0x6F, 0xC5, 0x30, 0x01, 0x67, 0x2B, 0xFE, 0xD7, 0xAB, 0x76, 0xCA, 0x82, 0xC9,
    0x7D, 0xFA, 0x59, 0x47, 0xF0, 0xAD, 0xD4, 0xA2, 0xAF, 0x9C, 0xA4, 0x72, 0xC0, 0xB7, 0xFD, 0x93, 0x26, 0x36, 0x3F,
    0xF7, 0xCC, 0x34, 0xA5, 0xE5, 0xF1, 0x71, 0xD8, 0x31, 0x15, 0x04, 0xC7, 0x23, 0xC3, 0x18, 0x96, 0x05, 0x9A, 0x07,
    0x12, 0x80, 0xE2, 0xEB, 0x27, 0xB2, 0x75, 0x09, 0x83, 0x2C, 0x1A, 0x1B, 0x6E, 0x5A, 0xA0, 0x52, 0x3B, 0xD6, 0xB3,
    0x29, 0xE3, 0x2F, 0x84, 0x53, 0xD1, 0x00, 0xED, 0x20, 0xFC, 0xB1, 0x5B, 0x6A, 0xCB, 0xBE, 0x39, 0x4A, 0x4C, 0x58,
    0xCF, 0xD0, 0xEF, 0xAA, 0xFB, 0x43, 0x4D, 0x33, 0x85, 0x45, 0xF9, 0x02, 0x7F, 0x50, 0x3C, 0x9F, 0xA8, 0x51, 0xA3,
    0x40, 0x8F, 0x92, 0x9D, 0x38, 0xF5, 0xBC, 0xB6, 0xDA, 0x21, 0x10, 0xFF, 0xF3, 0xD2, 0xCD, 0x0C, 0x13, 0xEC, 0x5F,
    0x97, 0x44, 0x17, 0xC4, 0xA7, 0x7E, 0x3D, 0x64, 0x5D, 0x19, 0x73, 0x60, 0x81, 0x4F, 0xDC, 0x22, 0x2A, 0x90, 0x88,
    0x46, 0xEE, 0xB8, 0x14, 0xDE, 0x5E, 0x0B, 0xDB, 0xE0, 0x32, 0x3A, 0x0A, 0x49, 0x06, 0x24, 0x5C, 0xC2, 0xD3, 0xAC,
    0x62, 0x91, 0x95, 0xE4, 0x79, 0xE7, 0xC8, 0x37, 0x6D, 0x8D, 0xD5, 0x4E, 0xA9, 0x6C, 0x56, 0xF4, 0xEA, 0x65, 0x7A,
    0xAE, 0x08, 0xBA, 0x78, 0x25, 0x2E, 0x1C, 0xA6, 0xB4, 0xC6, 0xE8, 0xDD, 0x74, 0x1F, 0x4B, 0xBD, 0x8B, 0x8A, 0x70,
    0x3E, 0xB5, 0x66, 0x48, 0x03, 0xF6, 0x0E, 0x61, 0x35, 0x57, 0xB9, 0x86, 0xC1, 0x1D, 0x9E, 0xE1, 0xF8, 0x98, 0x11,
    0x69, 0xD9, 0x8E, 0x94, 0x9B, 0x1E, 0x87, 0xE9, 0xCE, 0x55, 0x28, 0xDF, 0x8C, 0xA1, 0x89, 0x0D, 0xBF, 0xE6, 0x42,
    0x68, 0x41, 0x99, 0x2D, 0x0F, 0xB0, 0x54, 0xBB, 0x16,
};

static const uint8_t interp_aes_inv_sbox[256] = {
    0x52, 0x09, 0x6A, 0xD5, 0x30, 0x36, 0xA5, 0x38, 0xBF, 0x40, 0xA3, 0x9E, 0x81, 0xF3, 0xD7, 0xFB, 0x7C, 0xE3, 0x39,
    0x82, 0x9B, 0x2F, 0xFF, 0x87, 0x34, 0x8E, 0x43, 0x44, 0xC4, 0xDE, 0xE9, 0xCB, 0x54, 0x7B, 0x94, 0x32, 0xA6, 0xC2,
    0x23, 0x3D, 0xEE, 0x4C, 0x95, 0x0B, 0x42, 0xFA, 0xC3, 0x4E, 0x08, 0x2E, 0xA1, 0x66, 0x28, 0xD9, 0x24, 0xB2, 0x76,
    0x5B, 0xA2, 0x49, 0x6D, 0x8B, 0xD1, 0x25, 0x72, 0xF8, 0xF6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xD4, 0xA4, 0x5C, 0xCC,
    0x5D, 0x65, 0xB6, 0x92, 0x6C, 0x70, 0x48, 0x50, 0xFD, 0xED, 0xB9, 0xDA, 0x5E, 0x15, 0x46, 0x57, 0xA7, 0x8D, 0x9D,
    0x84, 0x90, 0xD8, 0xAB, 0x00, 0x8C, 0xBC, 0xD3, 0x0A, 0xF7, 0xE4, 0x58, 0x05, 0xB8, 0xB3, 0x45, 0x06, 0xD0, 0x2C,
    0x1E, 0x8F, 0xCA, 0x3F, 0x0F, 0x02, 0xC1, 0xAF, 0xBD, 0x03, 0x01, 0x13, 0x8A, 0x6B, 0x3A, 0x91, 0x11, 0x41, 0x4F,
    0x67, 0xDC, 0xEA, 0x97, 0xF2, 0xCF, 0xCE, 0xF0, 0xB4, 0xE6, 0x73, 0x96, 0xAC, 0x74, 0x22, 0xE7, 0xAD, 0x35, 0x85,
    0xE2, 0xF9, 0x37, 0xE8, 0x1C, 0x75, 0xDF, 0x6E, 0x47, 0xF1, 0x1A, 0x71, 0x1D, 0x29, 0xC5, 0x89, 0x6F, 0xB7, 0x62,
    0x0E, 0xAA, 0x18, 0xBE, 0x1B, 0xFC, 0x56, 0x3E, 0x4B, 0xC6, 0xD2, 0x79, 0x20, 0x9A, 0xDB, 0xC0, 0xFE, 0x78, 0xCD,
    0x5A, 0xF4, 0x1F, 0xDD, 0xA8, 0x33, 0x88, 0x07, 0xC7, 0x31, 0xB1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xEC, 0x5F, 0x60,
    0x51, 0x7F, 0xA9, 0x19, 0xB5, 0x4A, 0x0D, 0x2D, 0xE5, 0x7A, 0x9F, 0x93, 0xC9, 0x9C, 0xEF, 0xA0, 0xE0, 0x3B, 0x4D,
    0xAE, 0x2A, 0xF5, 0xB0, 0xC8, 0xEB, 0xBB, 0x3C, 0x83, 0x53, 0x99, 0x61, 0x17, 0x2B, 0x04, 0x7E, 0xBA, 0x77, 0xD6,
    0x26, 0xE1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0C, 0x7D,
};

// GF(2^8) multiply by x, modulo the AES polynomial 0x11B.
static uint8_t interp_aes_xtime(uint8_t a) {
    return (uint8_t)((a & 0x80u) ? (uint8_t)((a << 1) ^ 0x1Bu) : (uint8_t)(a << 1));
}

static uint8_t interp_aes_mul(uint8_t a, uint8_t b) {
    uint8_t result = 0;
    for (unsigned bit = 0; bit < 8u; bit++) {
        if (b & 1u) result ^= a;
        b = (uint8_t)(b >> 1);
        a = interp_aes_xtime(a);
    }
    return result;
}

// The state is column-major (byte 4*column + row), so (column, row) reads from (column + row) mod 4. A
// reversed direction still round-trips through AESE/AESD: only test vectors catch it.
static void interp_aes_shift_rows(const uint8_t *in, uint8_t *out, int inverse) {
    for (unsigned column = 0; column < 4u; column++)
        for (unsigned row = 0; row < 4u; row++) {
            unsigned source = 4u * ((column + row) & 3u) + row, destination = 4u * column + row;
            if (inverse)
                out[source] = in[destination];
            else
                out[destination] = in[source];
        }
}

static void interp_aes_mix_columns(const uint8_t *in, uint8_t *out, int inverse) {
    static const uint8_t forward[4] = {2, 3, 1, 1}, backward[4] = {14, 11, 13, 9};
    const uint8_t *coefficient = inverse ? backward : forward;
    for (unsigned column = 0; column < 4u; column++)
        for (unsigned row = 0; row < 4u; row++) {
            uint8_t value = 0;
            for (unsigned term = 0; term < 4u; term++)
                value ^= interp_aes_mul(in[4u * column + ((row + term) & 3u)], coefficient[term]);
            out[4u * column + row] = value;
        }
}

// ---- SHA helpers ----
static uint32_t interp_ror32_bits(uint32_t value, unsigned amount) {
    amount &= 31u;
    return amount ? ((value >> amount) | (value << (32u - amount))) : value;
}

static uint32_t interp_rol32_bits(uint32_t value, unsigned amount) {
    return interp_ror32_bits(value, (32u - (amount & 31u)) & 31u);
}

static uint32_t interp_sha_choose(uint32_t x, uint32_t y, uint32_t z) {
    return (((y ^ z) & x) ^ z);
}

static uint32_t interp_sha_majority(uint32_t x, uint32_t y, uint32_t z) {
    return ((x & y) | ((x | y) & z));
}

static uint32_t interp_sha_parity(uint32_t x, uint32_t y, uint32_t z) {
    return (x ^ y ^ z);
}

// The low element as a scalar of `fmt`. A scalar FP write ZEROES bits [127:N] (interp_vec_write's q == 0).
static uint64_t interp_fp_read(const struct cpu *cpu, int reg, unsigned fmt) {
    interp_vec value = interp_vec_read(cpu, reg);
    return interp_vec_element(&value, fmt + 1u, 0);
}

static void interp_fp_write(struct cpu *cpu, int reg, unsigned fmt, uint64_t bits) {
    interp_vec result;
    memset(result.byte, 0, sizeof result.byte);
    interp_vec_set_element(&result, fmt + 1u, 0, bits);
    interp_vec_write(cpu, reg, result, 0);
}

// The scalar FP encoding space: bits[30:29] == 00, bits[28:24] == 11110 (11111 for the three-source
// multiply-adds). bit30 separates it from the AdvSIMD SCALAR boxes (bits[31:30] == 01); bit31 is `sf` in the
// conversion boxes and M, which must be 0, elsewhere.
static int interp_exec_fp_scalar(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned type = (insn >> 22) & 3u, sf = (insn >> 31) & 1u;
    unsigned fmt = INTERP_FP_S;

    // ---- 3-source: FMADD / FMSUB / FNMADD / FNMSUB ----
    if ((insn & 0x7F000000u) == 0x1F000000u) {
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- 3-source with M set");
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- 3-source unallocated ptype");
        unsigned o1 = (insn >> 21) & 1u, o0 = (insn >> 15) & 1u;
        int ra = (int)((insn >> 10) & 31);
        uint64_t addend = interp_fp_read(cpu, ra, fmt);
        uint64_t left = interp_fp_read(cpu, rn, fmt), right = interp_fp_read(cpu, rm, fmt);
        //   FMADD =  Ra + Rn*Rm    FMSUB  =  Ra - Rn*Rm    FNMADD = -Ra - Rn*Rm    FNMSUB = -Ra + Rn*Rm
        // One FPMulAdd; the flip is a literal sign-bit toggle (FPNeg), so a propagated NaN's sign flips.
        uint64_t sign = interp_fp_sign_mask(fmt);
        if (o1) addend ^= sign;
        if (o1 != o0) left ^= sign;
        interp_fp_write(cpu, rd, fmt, interp_fp_muladd(fmt, addend, left, right));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if (!((insn >> 21) & 1u)) {
        // ---- FP <-> fixed-point ----
        // fbits = 64 - scale, and a 32-bit general register cannot name more than 32 fractional bits.
        unsigned rmode = (insn >> 19) & 3u, opcode = (insn >> 16) & 7u, scale = (insn >> 10) & 0x3Fu;
        unsigned fbits = 64u - scale;
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- fixed-point conversion unallocated ptype");
        if (!sf && scale < 32u)
            return interp_undefined(cpu, insn, "scalar FP -- 32-bit fixed-point conversion with scale < 32");
        if (rmode == 0 && (opcode == 2 || opcode == 3)) { // SCVTF / UCVTF (fixed-point)
            uint64_t value = interp_gpr(cpu, rn);
            interp_fp_write(
                cpu, rd, fmt,
                interp_fp_from_int(fmt, value, sf ? 64u : 32u, opcode == 2, INTERP_FPCR_RMODE(g_interp_fpcr), fbits));
        } else if (rmode == 3 && opcode <= 1) { // FCVTZS / FCVTZU (fixed-point)
            uint64_t out =
                interp_fp_to_int(fmt, interp_fp_read(cpu, rn, fmt), sf ? 64u : 32u, opcode == 0, INTERP_RM_RZ, fbits);
            if (sf)
                interp_set_gpr(cpu, rd, out);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)out);
        } else {
            return interp_undefined(cpu, insn, "scalar FP -- unallocated fixed-point conversion");
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // bit21 == 1. The rest are selected by bits[11:10], the four 00 boxes most specific first.
    unsigned op_low = (insn >> 10) & 3u;

    if (op_low == 1) { // ---- FCCMP / FCCMPE ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- FCCMP with M set");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- FCCMP ptype");
        unsigned cond = (insn >> 12) & 0xFu, quiet_signals = (insn >> 4) & 1u;
        if (interp_cond_holds(cpu, cond))
            interp_fp_compare(cpu, fmt, interp_fp_read(cpu, rn, fmt), interp_fp_read(cpu, rm, fmt), (int)quiet_signals);
        else
            // No comparison happens and no exception can be raised; NZCV comes from the insn's nzcv field.
            cpu->nzcv = ((uint64_t)(insn & 0xFu)) << 28;
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if (op_low == 2) { // ---- 2-source ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- 2-source with M set");
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- 2-source unallocated ptype");
        unsigned opcode = (insn >> 12) & 0xFu;
        uint64_t a = interp_fp_read(cpu, rn, fmt), b = interp_fp_read(cpu, rm, fmt), out;
        switch (opcode) {
        case 0: out = interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b); break; // FMUL
        case 1: out = interp_fp_arith(fmt, INTERP_FPOP_DIV, a, b); break; // FDIV
        case 2: out = interp_fp_arith(fmt, INTERP_FPOP_ADD, a, b); break; // FADD
        case 3: out = interp_fp_arith(fmt, INTERP_FPOP_SUB, a, b); break; // FSUB
        case 4: out = interp_fp_minmax(fmt, a, b, 1, 0); break;           // FMAX
        case 5: out = interp_fp_minmax(fmt, a, b, 0, 0); break;           // FMIN
        case 6: out = interp_fp_minmax(fmt, a, b, 1, 1); break;           // FMAXNM
        case 7: out = interp_fp_minmax(fmt, a, b, 0, 1); break;           // FMINNM
        case 8:
            // FNMUL negates the PRODUCT, after rounding and NaN propagation, so a propagated NaN flips too.
            out = interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b) ^ interp_fp_sign_mask(fmt);
            break;
        default: return interp_undefined(cpu, insn, "scalar FP -- unallocated 2-source opcode");
        }
        interp_fp_write(cpu, rd, fmt, out);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if (op_low == 3) { // ---- FCSEL ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- FCSEL with M set");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- FCSEL ptype");
        unsigned cond = (insn >> 12) & 0xFu;
        // A pure register copy: no flushing, no exceptions -- a signalling NaN passes through.
        interp_fp_write(cpu, rd, fmt,
                        interp_cond_holds(cpu, cond) ? interp_fp_read(cpu, rn, fmt) : interp_fp_read(cpu, rm, fmt));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x0000FC00u) == 0) { // ---- FP <-> integer ----
        unsigned rmode = (insn >> 19) & 3u, opcode = (insn >> 16) & 7u;
        if (rmode == 0 && opcode == 6) { // FMOV to a general register (Vn's low element -> Rd)
            interp_vec source = interp_vec_read(cpu, rn);
            if (type == 0 && !sf) // FMOV Wd, Sn
                interp_set_gpr32(cpu, rd, (uint32_t)interp_vec_element(&source, 2, 0));
            else if (type == 1 && sf) // FMOV Xd, Dn
                interp_set_gpr(cpu, rd, interp_vec_element(&source, 3, 0));
            else if (type == 3 && !sf) // FMOV Wd, Hn
                interp_set_gpr32(cpu, rd, (uint32_t)interp_vec_element(&source, 1, 0));
            else
                return interp_undefined(cpu, insn, "scalar FP -- unallocated FMOV to general register");
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 0 && opcode == 7) { // FMOV from a general register (Rn -> Vd's low element)
            if (type == 0 && !sf)        // FMOV Sd, Wn
                interp_fp_write(cpu, rd, INTERP_FP_S, interp_gpr(cpu, rn) & 0xFFFFFFFFu);
            else if (type == 1 && sf) // FMOV Dd, Xn
                interp_fp_write(cpu, rd, INTERP_FP_D, interp_gpr(cpu, rn));
            else if (type == 3 && !sf) // FMOV Hd, Wn
                interp_fp_write(cpu, rd, INTERP_FP_H, interp_gpr(cpu, rn) & 0xFFFFu);
            else
                return interp_undefined(cpu, insn, "scalar FP -- unallocated FMOV from general register");
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 1 && opcode == 6 && type == 2 && sf) { // FMOV Xd, Vn.D[1]
            interp_vec source = interp_vec_read(cpu, rn);
            interp_set_gpr(cpu, rd, interp_vec_element(&source, 3, 1));
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 1 && opcode == 7 && type == 2 && sf) { // FMOV Vd.D[1], Xn
            interp_vec destination = interp_vec_read(cpu, rd);
            interp_vec_set_element(&destination, 3, 1, interp_gpr(cpu, rn));
            interp_vec_write(cpu, rd, destination, 1); // single-lane write: keep the low half
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (rmode == 3 && opcode == 6 && type == 1 && !sf) {
            // FJCVTZS: double -> int32 with JavaScript ToInt32 semantics. It WRAPS modulo 2^32 rather than
            // saturating, and Z reports exactness (1 only if nothing was lost).
            uint64_t bits = interp_fp_flush_input(interp_fp_read(cpu, rn, INTERP_FP_D), INTERP_FP_D);
            unsigned cls = interp_fp_class(bits, INTERP_FP_D);
            unsigned exact = 1;
            uint64_t result = 0;
            if (cls >= INTERP_FPC_INF) { // NaN or infinity: Invalid, result 0, not exact
                interp_fpsr_raise(INTERP_FPSR_IOC);
                exact = 0;
            } else if (cls != INTERP_FPC_ZERO) {
                // A 64-bit signed destination so nothing saturates; exactness then comes off the flags.
                uint64_t before = g_interp_fpsr;
                uint64_t wide = interp_fp_to_int(INTERP_FP_D, bits, 64u, 1, INTERP_RM_RZ, 0);
                unsigned raised = (unsigned)((g_interp_fpsr & ~before) & (INTERP_FPSR_IXC | INTERP_FPSR_IOC));
                if (raised) exact = 0;
                if ((int64_t)wide != (int64_t)(int32_t)(uint32_t)wide) {
                    interp_fpsr_raise(INTERP_FPSR_IOC);
                    exact = 0;
                }
                result = (uint64_t)(uint32_t)wide;
            } else if (bits & interp_fp_sign_mask(INTERP_FP_D)) {
                exact = 0; // -0.0 converts to +0, which ToInt32 does not consider exact
            }
            interp_set_gpr32(cpu, rd, (uint32_t)result);
            interp_set_flags(cpu, 0, exact, 0, 0);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- integer conversion unallocated ptype");
        if (opcode == 2 || opcode == 3) { // SCVTF / UCVTF
            if (rmode != 0) return interp_undefined(cpu, insn, "scalar FP -- unallocated SCVTF/UCVTF rmode");
            interp_fp_write(cpu, rd, fmt,
                            interp_fp_from_int(fmt, interp_gpr(cpu, rn), sf ? 64u : 32u, opcode == 2,
                                               INTERP_FPCR_RMODE(g_interp_fpcr), 0));
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opcode <= 1 || opcode == 4 || opcode == 5) {
            // rmode picks the rounding for opcode 0/1; opcode 4/5 is FCVTA, whose ties-away has no FPCR code.
            unsigned convert_mode;
            if (opcode >= 4) {
                if (rmode != 0) return interp_undefined(cpu, insn, "scalar FP -- unallocated FCVTA rmode");
                convert_mode = INTERP_RM_RA;
            } else {
                static const unsigned by_rmode[4] = {INTERP_RM_RN, INTERP_RM_RP, INTERP_RM_RM, INTERP_RM_RZ};
                convert_mode = by_rmode[rmode];
            }
            uint64_t out = interp_fp_to_int(fmt, interp_fp_read(cpu, rn, fmt), sf ? 64u : 32u, (opcode & 1u) == 0,
                                            convert_mode, 0);
            if (sf)
                interp_set_gpr(cpu, rd, out);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)out);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        return interp_undefined(cpu, insn, "scalar FP -- unallocated integer conversion");
    }

    if ((insn & 0x00007C00u) == 0x00004000u) { // ---- 1-source ----
        if (sf) return interp_undefined(cpu, insn, "scalar FP -- 1-source with M set");
        unsigned opcode = (insn >> 15) & 0x3Fu;
        if (!interp_fp_type_fmt(type, &fmt))
            return interp_undefined(cpu, insn, "scalar FP -- 1-source unallocated ptype");
        // FCVT names its DESTINATION in the low two opcode bits and its source in ptype, so split it first.
        if ((opcode & 0x3Cu) == 0x04u) {
            // Opcode 000110 shares this box but is BFCVT (FEAT_BF16), single -> BFloat16, not an FCVT.
            if (opcode == 0x06u) {
                // (ftype 01, opcode 000110) IS the encoding; the operand is V[n,32] anyway, so `fmt` above
                // does not apply. ARM ARM FPConvertBF: bf16 is the TOP HALF of the binary32 encoding, so the
                // whole conversion is a rounding of the discarded low 16 bits -- exact for normals,
                // subnormals and the overflow-to-infinity carry alike. FPCR.RMode selects it; only the
                // default tie-to-even is implemented, so the other three report rather than guess.
                if (type != 1u) return interp_undefined(cpu, insn, "scalar FP -- BFCVT unallocated ptype");
                if (INTERP_FPCR_RMODE(g_interp_fpcr) != 0u)
                    return interp_undefined(cpu, insn, "scalar FP -- BFCVT with a non-default FPCR.RMode");
                uint32_t bits = (uint32_t)interp_fp_flush_input(interp_fp_read(cpu, rn, INTERP_FP_S), INTERP_FP_S);
                unsigned cls = interp_fp_class(bits, INTERP_FP_S);
                uint64_t out;
                if (cls >= INTERP_FPC_QNAN) {
                    out = UINT64_C(0x7FC0);
                } else if (cls == INTERP_FPC_INF || cls == INTERP_FPC_ZERO) {
                    out = bits >> 16;
                } else {
                    // Tie-to-even: add half an ulp, plus one more when the kept bit is already odd.
                    uint32_t rounded = bits + 0x7FFFu + ((bits >> 16) & 1u);
                    if (bits & 0xFFFFu) {
                        unsigned raised = INTERP_FPSR_IXC;
                        if ((rounded & 0x7F800000u) == 0x7F800000u) raised |= INTERP_FPSR_OFC;
                        // bf16 shares binary32's exponent field, so the result is tiny exactly when the source
                        // was -- tested BEFORE rounding, which is AArch64's tininess rule.
                        if ((bits & 0x7F800000u) == 0) raised |= INTERP_FPSR_UFC;
                        interp_fpsr_raise(raised);
                    }
                    out = rounded >> 16;
                }
                interp_fp_write(cpu, rd, INTERP_FP_H, out);
                cpu->pc = gpc + 4;
                return INTERP_NEXT;
            }
            unsigned to;
            if (!interp_fp_type_fmt(opcode & 3u, &to) || to == fmt)
                return interp_undefined(cpu, insn, "scalar FP -- unallocated FCVT destination");
            if ((to == INTERP_FP_H || fmt == INTERP_FP_H) && INTERP_FPCR_AHP(g_interp_fpcr))
                return interp_undefined(cpu, insn, "scalar FP -- FCVT with FPCR.AHP (alternative half format)");
            interp_fp_write(cpu, rd, to, interp_fp_convert(fmt, to, interp_fp_read(cpu, rn, fmt)));
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        uint64_t a = interp_fp_read(cpu, rn, fmt), out;
        switch (opcode) {
        case 0x00: out = a; break;                             // FMOV (register): a pure bit copy
        case 0x01: out = a & ~interp_fp_sign_mask(fmt); break; // FABS: clears the sign bit, nothing else
        case 0x02: out = a ^ interp_fp_sign_mask(fmt); break;  // FNEG: a sign-bit toggle
        case 0x03:
            out = interp_fp_sqrt(fmt, a);
            break; // FSQRT
        // The FRINT family differs only in the mode and in whether a change is Inexact; only FRINTX is.
        case 0x08: out = interp_fp_round_integral(fmt, a, INTERP_RM_RN, 0); break;                     // FRINTN
        case 0x09: out = interp_fp_round_integral(fmt, a, INTERP_RM_RP, 0); break;                     // FRINTP
        case 0x0A: out = interp_fp_round_integral(fmt, a, INTERP_RM_RM, 0); break;                     // FRINTM
        case 0x0B: out = interp_fp_round_integral(fmt, a, INTERP_RM_RZ, 0); break;                     // FRINTZ
        case 0x0C: out = interp_fp_round_integral(fmt, a, INTERP_RM_RA, 0); break;                     // FRINTA
        case 0x0E: out = interp_fp_round_integral(fmt, a, INTERP_FPCR_RMODE(g_interp_fpcr), 1); break; // FRINTX
        case 0x0F: out = interp_fp_round_integral(fmt, a, INTERP_FPCR_RMODE(g_interp_fpcr), 0); break; // FRINTI
        default: return interp_undefined(cpu, insn, "scalar FP -- unimplemented 1-source opcode");
        }
        interp_fp_write(cpu, rd, fmt, out);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x00003C00u) == 0x00002000u) { // ---- FCMP / FCMPE ----
        if (sf || ((insn >> 14) & 3u) != 0) return interp_undefined(cpu, insn, "scalar FP -- unallocated compare");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- compare ptype");
        unsigned opcode2 = insn & 0x1Fu;
        if (opcode2 & 7u) return interp_undefined(cpu, insn, "scalar FP -- unallocated compare opcode2");
        // opcode2<4> is E (Invalid for a quiet NaN too); opcode2<3> selects the compare-with-zero form.
        int quiet_signals = (opcode2 >> 4) & 1;
        uint64_t b = (opcode2 & 8u) ? UINT64_C(0) : interp_fp_read(cpu, rm, fmt);
        interp_fp_compare(cpu, fmt, interp_fp_read(cpu, rn, fmt), b, quiet_signals);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x00001C00u) == 0x00001000u) { // ---- FMOV (immediate) ----
        if (sf || ((insn >> 5) & 0x1Fu) != 0)
            return interp_undefined(cpu, insn, "scalar FP -- unallocated FMOV immediate");
        if (!interp_fp_type_fmt(type, &fmt)) return interp_undefined(cpu, insn, "scalar FP -- FMOV immediate ptype");
        interp_fp_write(cpu, rd, fmt, interp_fp_expand_imm(fmt, (insn >> 13) & 0xFFu));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "scalar FP -- unallocated encoding");
}

// Scalar floating-point and Advanced SIMD.
// The subset guests actually reach. Reported, not implemented: BFloat16, reciprocal estimates,
// saturating-doubling multiplies, by-element forms, SVE.
static int interp_exec_simd(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned q = (insn >> 30) & 1, u = (insn >> 29) & 1;

    // crypto: AES and two-register SHA
    // Before the scalar normalisation, which would turn bits[31:30] == 01 into another encoding.
    if ((insn & 0xFF3E0C00u) == 0x4E280800u || (insn & 0xFF3E0C00u) == 0x5E280800u) {
        unsigned opcode = (insn >> 12) & 0x1Fu, size = (insn >> 22) & 3u;
        interp_vec source = interp_vec_read(cpu, rn), destination = interp_vec_read(cpu, rd), result;
        if ((insn & 0xFF000000u) == 0x4E000000u) { // ---- AES ----
            if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD AES -- size must be 00");
            uint8_t stage[16], mixed[16];
            switch (opcode) {
            case 0x04:   // AESE
            case 0x05: { // AESD
                int inverse = opcode == 0x05;
                uint8_t combined[16];
                for (unsigned index = 0; index < 16u; index++)
                    combined[index] = (uint8_t)(destination.byte[index] ^ source.byte[index]);
                interp_aes_shift_rows(combined, stage, inverse);
                for (unsigned index = 0; index < 16u; index++)
                    stage[index] = inverse ? interp_aes_inv_sbox[stage[index]] : interp_aes_sbox[stage[index]];
                memcpy(result.byte, stage, 16);
                break;
            }
            case 0x06: // AESMC
            case 0x07: // AESIMC
                interp_aes_mix_columns(source.byte, mixed, opcode == 0x07);
                memcpy(result.byte, mixed, 16);
                break;
            default: return interp_undefined(cpu, insn, "AdvSIMD AES -- unallocated opcode");
            }
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // two-register SHA
        if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD SHA -- size must be 00");
        uint32_t d[4], n[4];
        for (unsigned index = 0; index < 4u; index++) {
            d[index] = (uint32_t)interp_vec_element(&destination, 2, index);
            n[index] = (uint32_t)interp_vec_element(&source, 2, index);
        }
        uint32_t out[4] = {0, 0, 0, 0};
        switch (opcode) {
        case 0x00: // SHA1H
            out[0] = interp_rol32_bits(n[0], 30);
            break;
        case 0x01: { // SHA1SU1
            uint32_t t[4];
            // T = Vd EOR (Vn >> 32), zero fill.
            for (unsigned index = 0; index < 4u; index++)
                t[index] = d[index] ^ (index < 3u ? n[index + 1u] : 0u);
            for (unsigned index = 0; index < 4u; index++)
                out[index] = interp_rol32_bits(t[index], 1);
            out[3] ^= interp_rol32_bits(t[0], 2);
            break;
        }
        case 0x02: { // SHA256SU0
            uint32_t t[4];
            for (unsigned index = 0; index < 4u; index++)
                t[index] = index < 3u ? d[index + 1u] : n[0];
            for (unsigned index = 0; index < 4u; index++) {
                uint32_t element = t[index];
                element = interp_ror32_bits(element, 7) ^ interp_ror32_bits(element, 18) ^ (element >> 3);
                out[index] = element + d[index];
            }
            break;
        }
        default: return interp_undefined(cpu, insn, "AdvSIMD SHA -- unallocated two-register opcode");
        }
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned index = 0; index < 4u; index++)
            interp_vec_set_element(&result, 2, index, out[index]);
        interp_vec_write(cpu, rd, result, 1);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // three-register SHA
    // Fixed bits are 31:24, 21, 15 and 11:10 ONLY: masking 15:10 as one field pins the opcode to zero.
    if ((insn & 0xFF208C00u) == 0x5E000000u) {
        unsigned opcode = (insn >> 12) & 7u;
        interp_vec vd = interp_vec_read(cpu, rd), vn = interp_vec_read(cpu, rn), vm = interp_vec_read(cpu, rm);
        uint32_t x[4], y[4], w[4], result_words[4];
        for (unsigned index = 0; index < 4u; index++) {
            x[index] = (uint32_t)interp_vec_element(&vd, 2, index);
            y[index] = (uint32_t)interp_vec_element(&vn, 2, index);
            w[index] = (uint32_t)interp_vec_element(&vm, 2, index);
        }
        if (opcode <= 2u) {
            // SHA1C/P/M: FOUR SHA-1 rounds; K is folded into Vm by the caller.
            uint32_t e = y[0];
            for (unsigned round = 0; round < 4u; round++) {
                uint32_t t = opcode == 0   ? interp_sha_choose(x[1], x[2], x[3])
                             : opcode == 1 ? interp_sha_parity(x[1], x[2], x[3])
                                           : interp_sha_majority(x[1], x[2], x[3]);
                uint32_t next = e + interp_rol32_bits(x[0], 5) + t + w[round];
                x[1] = interp_rol32_bits(x[1], 30);
                e = x[3];
                x[3] = x[2];
                x[2] = x[1];
                x[1] = x[0];
                x[0] = next;
            }
            memcpy(result_words, x, sizeof result_words);
        } else if (opcode == 3u) {
            // SHA1SU0
            uint32_t t[4] = {x[2], x[3], y[0], y[1]};
            for (unsigned index = 0; index < 4u; index++)
                result_words[index] = t[index] ^ x[index] ^ w[index];
        } else if (opcode == 4u || opcode == 5u) {
            // SHA256H -> x, SHA256H2 -> y, halves swapped.
            int part1 = opcode == 4u;
            uint32_t a[4], b[4];
            if (part1) {
                memcpy(a, x, sizeof a);
                memcpy(b, y, sizeof b);
            } else {
                memcpy(a, y, sizeof a);
                memcpy(b, x, sizeof b);
            }
            for (unsigned round = 0; round < 4u; round++) {
                uint32_t chs = interp_sha_choose(b[0], b[1], b[2]);
                uint32_t maj = interp_sha_majority(a[0], a[1], a[2]);
                uint32_t sigma1 =
                    interp_ror32_bits(b[0], 6) ^ interp_ror32_bits(b[0], 11) ^ interp_ror32_bits(b[0], 25);
                uint32_t sigma0 =
                    interp_ror32_bits(a[0], 2) ^ interp_ror32_bits(a[0], 13) ^ interp_ror32_bits(a[0], 22);
                uint32_t t = b[3] + sigma1 + chs + w[round];
                uint32_t new_a3 = t + a[3];
                uint32_t new_b3 = t + sigma0 + maj;
                // <y, x> = ROL(y : x, 32).
                uint32_t carry = new_a3;
                a[3] = a[2];
                a[2] = a[1];
                a[1] = a[0];
                a[0] = new_b3;
                b[3] = b[2];
                b[2] = b[1];
                b[1] = b[0];
                b[0] = carry;
            }
            memcpy(result_words, part1 ? a : b, sizeof result_words);
        } else if (opcode == 6u) {
            // SHA256SU1
            uint32_t t0[4] = {y[1], y[2], y[3], w[0]};
            uint32_t t1[2] = {w[2], w[3]};
            for (unsigned index = 0; index < 2u; index++) {
                uint32_t element = t1[index];
                element = interp_ror32_bits(element, 17) ^ interp_ror32_bits(element, 19) ^ (element >> 10);
                result_words[index] = element + x[index] + t0[index];
            }
            for (unsigned index = 2; index < 4u; index++) {
                uint32_t element = result_words[index - 2u];
                element = interp_ror32_bits(element, 17) ^ interp_ror32_bits(element, 19) ^ (element >> 10);
                result_words[index] = element + x[index] + t0[index];
            }
        } else {
            return interp_undefined(cpu, insn, "AdvSIMD SHA -- unallocated three-register opcode");
        }
        interp_vec result;
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned index = 0; index < 4u; index++)
            interp_vec_set_element(&result, 2, index, result_words[index]);
        interp_vec_write(cpu, rd, result, 1);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // scalar FP (bit28 == 1, bit30 == 0)
    if ((insn & 0x7F000000u) == 0x1E000000u || (insn & 0x7F000000u) == 0x1F000000u)
        return interp_exec_fp_scalar(cpu, insn);

    // AdvSIMD SCALAR forms, normalised into their vector spelling
    // Clearing bits 30 and 28 gives the vector encoding at Q == 0, but a scalar form has ONE lane and zeroes
    // [127:esize]: `scalar` overrides interp_vec_lanes and the "1D is reserved" checks. Diagnostics use `insn`.
    unsigned scalar = 0;
    uint32_t decode = insn;
    if ((insn & 0xDE000000u) == 0x5E000000u) {
        scalar = 1;
        decode &= ~UINT32_C(0x50000000);
        q = 0;
    }

    // AdvSIMD copy: DUP, INS, SMOV, UMOV
    // bits[23:21] must ALL be 000: leaving 23:22 unconstrained swallowed the whole three-same-FP16 box
    // below, which shares bit21 == 0 and bit10 == 1, and ran it as INS/DUP with imm5 = Rm.
    if ((decode & 0x9FE08400u) == 0x0E000400u) {
        unsigned op = (insn >> 29) & 1, imm4 = (insn >> 11) & 0xFu, imm5 = (insn >> 16) & 0x1Fu;
        unsigned size, index;
        if (op) { // INS (element)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            unsigned source_index = imm4 >> size; // imm4 is the source lane, scaled by the element size
            interp_vec source = interp_vec_read(cpu, rn), destination = interp_vec_read(cpu, rd);
            interp_vec_set_element(&destination, size, index, interp_vec_element(&source, size, source_index));
            // Single-lane write: must NOT zero the upper half.
            interp_vec_write(cpu, rd, destination, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        switch (imm4) {
        case 0: { // DUP (element)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            // The SCALAR spelling is DUP Dd, Vn.D[index]: do not reject 1D.
            if (size == 3 && !q && !scalar) return interp_undefined(cpu, insn, "AdvSIMD copy -- DUP 1D is reserved");
            interp_vec source = interp_vec_read(cpu, rn), result;
            uint64_t element = interp_vec_element(&source, size, index);
            memset(result.byte, 0, sizeof result.byte);
            // The scalar spelling (MOV Bd/Hd/Sd/Dd, Vn.T[index]) writes ONE element and zeroes the rest;
            // only the D form coincides with filling the 64-bit half.
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++)
                interp_vec_set_element(&result, size, lane, element);
            interp_vec_write(cpu, rd, result, q);
            break;
        }
        case 1: { // DUP (general)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            if (size == 3 && !q) return interp_undefined(cpu, insn, "AdvSIMD copy -- DUP 1D is reserved");
            uint64_t element = interp_gpr(cpu, rn) & interp_element_mask(size);
            interp_vec result;
            memset(result.byte, 0, sizeof result.byte);
            for (unsigned lane = 0; lane < interp_vec_lanes(size, q); lane++)
                interp_vec_set_element(&result, size, lane, element);
            interp_vec_write(cpu, rd, result, q);
            break;
        }
        case 3: { // INS (general)
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            interp_vec destination = interp_vec_read(cpu, rd);
            interp_vec_set_element(&destination, size, index, interp_gpr(cpu, rn) & interp_element_mask(size));
            interp_vec_write(cpu, rd, destination, 1);
            break;
        }
        case 5:   // SMOV
        case 7: { // UMOV
            if (!interp_imm5_element(imm5, &size, &index))
                return interp_undefined(cpu, insn, "AdvSIMD copy -- reserved imm5");
            interp_vec source = interp_vec_read(cpu, rn);
            uint64_t element = interp_vec_element(&source, size, index);
            if (imm4 == 5) element = interp_element_sext(element, size);
            // Here Q selects the destination GPR width, not the vector length.
            if (q)
                interp_set_gpr(cpu, rd, element);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)element);
            break;
        }
        default: return interp_undefined(cpu, insn, "AdvSIMD copy -- unallocated imm4");
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD modified immediate
    // Must precede shift-by-immediate: same box, separated only by immh (22:19).
    if ((decode & 0x9FF80400u) == 0x0F000400u) {
        unsigned op = (insn >> 29) & 1, cmode = (insn >> 12) & 0xFu, o2 = (insn >> 11) & 1;
        uint64_t imm8 = (uint64_t)(((insn >> 16) & 7u) << 5) | ((insn >> 5) & 0x1Fu);
        uint64_t pattern;
        if (!interp_advsimd_expand_imm(op, cmode, o2, q, imm8, &pattern))
            return interp_undefined(cpu, insn, "AdvSIMD modified immediate -- reserved cmode");
        // ORR/BIC is cmode 0xx1 and 10x1 only: cmode<0> == 1 with cmode<3:2> != 11. Testing cmode<3:1>
        // instead let 1101 -- MOVI/MVNI with MSL #16 -- read-modify the destination instead of replacing it.
        int read_modify = (cmode & 1u) && ((cmode >> 2) & 3u) != 3u;
        interp_vec result = interp_vec_read(cpu, rd);
        uint64_t low, high;
        memcpy(&low, result.byte, 8);
        memcpy(&high, result.byte + 8, 8);
        if (read_modify) {
            if (op) { // BIC
                low &= ~pattern;
                high &= ~pattern;
            } else { // ORR
                low |= pattern;
                high |= pattern;
            }
        } else if (op && ((cmode >> 1) & 7u) != 7u) { // MVNI
            low = high = ~pattern;
        } else { // MOVI
            low = high = pattern;
        }
        memcpy(result.byte, &low, 8);
        memcpy(result.byte + 8, &high, 8);
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD shift by immediate
    if ((decode & 0x9F800400u) == 0x0F000400u) {
        unsigned immh = (insn >> 19) & 0xFu, immb = (insn >> 16) & 7u, opcode = (insn >> 11) & 0x1Fu;
        unsigned size;
        if (immh & 8u)
            size = 3;
        else if (immh & 4u)
            size = 2;
        else if (immh & 2u)
            size = 1;
        else
            size = 0;
        unsigned esize = 8u << size;
        unsigned combined = (immh << 3) | immb;
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        // The scalar spelling has one lane, at the vector group's reserved 1D width.
        unsigned lanes = scalar ? 1u : interp_vec_lanes(size, q);
        uint64_t mask = interp_element_mask(size);

        // fixed-point conversions: fbits = 2*esize - (immh:immb), a SCALE not a shift
        if (opcode == 0x1C || opcode == 0x1F) {
            if (size < 1) return interp_undefined(cpu, insn, "AdvSIMD shift -- fixed-point conversion needs immh != 0");
            unsigned fmt = size == 3 ? INTERP_FP_D : (size == 2 ? INTERP_FP_S : INTERP_FP_H);
            unsigned fbits = 2u * esize - combined;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                uint64_t value;
                if (opcode == 0x1C) // SCVTF / UCVTF
                    value = interp_fp_from_int(fmt, element, esize, !u, INTERP_FPCR_RMODE(g_interp_fpcr), fbits);
                else // FCVTZS / FCVTZU
                    value = interp_fp_to_int(fmt, element, esize, !u, INTERP_RM_RZ, fbits);
                interp_vec_set_element(&result, size, lane, value & mask);
            }
            interp_vec_write(cpu, rd, result, scalar ? 0u : q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (opcode == 0x10 || opcode == 0x11 || opcode == 0x12 || opcode == 0x13) {
            // NARROWING right shifts: sources are TWICE the destination width, the 64-bit result goes in the
            // half Q selects. SHRN/RSHRN truncate; SQSHRUN saturates signed -> unsigned, SQSHRN/UQSHRN
            // signed/unsigned. Odd opcodes round.
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD shift -- narrowing shift with a 64-bit result");
            unsigned shift = 2u * esize - combined;
            unsigned narrow_lanes = 64u / esize;
            uint64_t wide_mask = interp_element_mask(size + 1u);
            int saturating = opcode >= 0x12 || u;
            int source_signed = opcode >= 0x12 ? !u : 1;
            int dest_signed = opcode >= 0x12 ? !u : 0;
            int rounding = (opcode & 1u) != 0;
            interp_vec packed;
            memset(packed.byte, 0, sizeof packed.byte);
            for (unsigned lane = 0; lane < (scalar ? 1u : narrow_lanes); lane++) {
                uint64_t element = interp_vec_element(&source, size + 1u, lane) & wide_mask;
                uint64_t shifted;
                // The rounding constant can carry out of the wide element (0xFFFF..FF + half at a 64-bit
                // source), so add in 128 bits and clamp: a carry-out is always a saturation, and letting it
                // wrap gave 0 with QC CLEAR where the answer is the maximum with QC SET.
                if (saturating && source_signed) {
                    // Shift the SIGN-EXTENDED value so saturation sees the true magnitude.
                    __int128 wide = (__int128)(int64_t)interp_element_sext(element, size + 1u);
                    if (rounding && shift > 0) wide += (__int128)1 << (shift - 1u);
                    __int128 value = wide >> shift;
                    __int128 wide_max = (__int128)(wide_mask >> 1);
                    shifted = (uint64_t)(value > wide_max ? wide_max : value) & wide_mask;
                } else {
                    unsigned __int128 wide = element;
                    if (rounding && shift > 0) wide += (unsigned __int128)1 << (shift - 1u);
                    unsigned __int128 value = wide >> shift;
                    shifted = value > (unsigned __int128)wide_mask ? wide_mask : (uint64_t)value;
                }
                interp_vec_set_element(&packed, size, lane,
                                       saturating ? interp_sat_narrow(shifted, size, source_signed, dest_signed)
                                                  : (shifted & mask));
            }
            if (!q || scalar) {
                // Unsuffixed: write the low 64 bits, ZERO the upper half.
                interp_vec_write(cpu, rd, packed, 0);
            } else {
                // "2": write the UPPER 64 bits, leave the lower half untouched.
                interp_vec destination = interp_vec_read(cpu, rd);
                memcpy(destination.byte + 8, packed.byte, 8);
                interp_vec_write(cpu, rd, destination, 1);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (opcode == 0x14) {
            // SSHLL / USHLL (SXTL/UXTL at zero shift): widen; Q picks the source half.
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD shift -- SSHLL/USHLL with a 64-bit source");
            unsigned shift = combined - esize;
            unsigned wide_lanes = 64u / esize;
            uint64_t wide_mask = interp_element_mask(size + 1u);
            for (unsigned lane = 0; lane < wide_lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, q ? lane + wide_lanes : lane);
                if (!u) element = interp_element_sext(element, size);
                interp_vec_set_element(&result, size + 1u, lane, (element << shift) & wide_mask);
            }
            // Full 128-bit destination either way.
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (size == 3 && !q && !scalar)
            return interp_undefined(cpu, insn, "AdvSIMD shift -- 64-bit element requires Q");
        if (opcode == 0x0A && !u) { // SHL
            unsigned shift = combined - esize;
            for (unsigned lane = 0; lane < lanes; lane++)
                interp_vec_set_element(&result, size, lane, (interp_vec_element(&source, size, lane) << shift) & mask);
        } else if (opcode == 0x08 || (opcode == 0x0A && u)) {
            // SRI / SLI: the shifted-in bits come from the DESTINATION, not zeroes.
            interp_vec destination = interp_vec_read(cpu, rd);
            unsigned shift = opcode == 0x08 ? 2u * esize - combined : combined - esize;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane) & mask;
                uint64_t base = interp_vec_element(&destination, size, lane) & mask;
                uint64_t moved, keep;
                if (opcode == 0x08) { // SRI: keep the destination's TOP bits
                    moved = shift >= esize ? 0 : (element >> shift);
                    keep = shift == 0 ? 0 : (mask << (esize - shift)) & mask;
                } else { // SLI: keep the destination's BOTTOM bits
                    moved = (element << shift) & mask;
                    keep = shift == 0 ? 0 : ((UINT64_C(1) << shift) - 1u);
                }
                interp_vec_set_element(&result, size, lane, (moved & ~keep) | (base & keep));
            }
        } else if (opcode == 0x0C || opcode == 0x0E) {
            // SQSHLU, SQSHL, UQSHL: repeated saturating doubling, matching SQADD's QC.
            if (opcode == 0x0C && !u) return interp_undefined(cpu, insn, "AdvSIMD shift -- unallocated SQSHLU");
            unsigned shift = combined - esize;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                uint64_t value;
                if (opcode == 0x0E && u) { // UQSHL
                    value = element & mask;
                    for (unsigned step = 0; step < shift; step++)
                        value = interp_uqadd_element(value, value, size, 0);
                } else if (opcode == 0x0E) { // SQSHL
                    value = element & mask;
                    for (unsigned step = 0; step < shift; step++)
                        value = interp_sqadd_element(value, value, size, 0);
                } else { // SQSHLU: UNSIGNED saturation
                    int64_t signed_element = (int64_t)interp_element_sext(element, size);
                    if (signed_element < 0) {
                        interp_fpsr_raise(INTERP_FPSR_QC);
                        value = 0;
                    } else {
                        value = (uint64_t)signed_element & mask;
                        for (unsigned step = 0; step < shift; step++)
                            value = interp_uqadd_element(value, value, size, 0);
                    }
                }
                interp_vec_set_element(&result, size, lane, value & mask);
            }
        } else if (opcode == 0x00 || opcode == 0x02 || opcode == 0x04 || opcode == 0x06) {
            // SSHR/USHR, SSRA/USRA, and the rounding SRSHR/URSHR, SRSRA/URSRA.
            unsigned shift = 2u * esize - combined;
            int rounding = opcode == 0x04 || opcode == 0x06;
            int accumulating = opcode == 0x02 || opcode == 0x06;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                // A full-element-width right shift is defined here but UB in C.
                uint64_t shifted;
                if (u) {
                    uint64_t value = element & mask;
                    uint64_t round = rounding && shift > 0 ? ((value >> (shift - 1u)) & 1u) : 0u;
                    shifted = (shift >= esize ? 0 : (value >> shift)) + round;
                } else {
                    int64_t signed_element = (int64_t)interp_element_sext(element, size);
                    uint64_t round = rounding && shift > 0
                                         ? (uint64_t)((signed_element >> (shift > esize ? esize - 1u : shift - 1u)) & 1)
                                         : 0u;
                    shifted = (uint64_t)(shift >= esize ? (signed_element >> (esize - 1)) : (signed_element >> shift));
                    shifted += round;
                }
                shifted &= mask;
                if (accumulating) shifted = (shifted + interp_vec_element(&accumulate, size, lane)) & mask;
                interp_vec_set_element(&result, size, lane, shifted);
            }
        } else {
            return interp_undefined(cpu, insn, "AdvSIMD shift by immediate -- unimplemented opcode");
        }
        interp_vec_write(cpu, rd, result, scalar ? 0u : q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD EXT
    if ((decode & 0xBFE08400u) == 0x2E000000u) {
        unsigned position = (insn >> 11) & 0xFu;
        unsigned bytes = q ? 16u : 8u;
        if (!q && (position & 8u)) return interp_undefined(cpu, insn, "AdvSIMD EXT -- imm4 out of range for 8B");
        interp_vec first = interp_vec_read(cpu, rn), second = interp_vec_read(cpu, rm), result;
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned index = 0; index < bytes; index++) {
            unsigned source = position + index;
            result.byte[index] = source < bytes ? first.byte[source] : second.byte[source - bytes];
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD table lookup: TBL / TBX
    if ((decode & 0xBF208C00u) == 0x0E000000u) {
        unsigned length = (insn >> 13) & 3u, extend = (insn >> 12) & 1u;
        unsigned bytes = q ? 16u : 8u;
        interp_vec index_vector = interp_vec_read(cpu, rm), result = interp_vec_read(cpu, rd);
        interp_vec table[4];
        for (unsigned entry = 0; entry <= length; entry++)
            table[entry] = interp_vec_read(cpu, (rn + (int)entry) % 32);
        for (unsigned index = 0; index < bytes; index++) {
            unsigned selector = index_vector.byte[index];
            if (selector < (length + 1u) * 16u)
                result.byte[index] = table[selector / 16u].byte[selector % 16u];
            else if (!extend)
                result.byte[index] = 0; // TBL zeroes an out-of-range index; TBX keeps the destination byte
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD permute: ZIP / UZP / TRN
    // Same mask as TBL/TBX, separated by bits[11:10] (00 there, 10 here).
    if ((decode & 0xBF208C00u) == 0x0E000800u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 7u;
        if (size == 3 && !q) return interp_undefined(cpu, insn, "AdvSIMD permute -- 1D form is reserved");
        interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned lanes = interp_vec_lanes(size, q), half = lanes / 2u;
        for (unsigned lane = 0; lane < lanes; lane++) {
            uint64_t element;
            switch (opcode) {
            case 1:   // UZP1 (even lanes of Vn:Vm)
            case 5: { // UZP2 (odd)
                unsigned offset = opcode == 5 ? 1u : 0u;
                const interp_vec *source = lane < half ? &left : &right;
                unsigned index = (lane < half ? lane : lane - half) * 2u + offset;
                element = interp_vec_element(source, size, index);
                break;
            }
            case 2:   // TRN1 (even)
            case 6: { // TRN2 (odd)
                unsigned offset = opcode == 6 ? 1u : 0u;
                const interp_vec *source = (lane & 1u) ? &right : &left;
                element = interp_vec_element(source, size, (lane & ~1u) + offset);
                break;
            }
            case 3:   // ZIP1 (lower halves)
            case 7: { // ZIP2 (upper)
                unsigned base = opcode == 7 ? half : 0u;
                const interp_vec *source = (lane & 1u) ? &right : &left;
                element = interp_vec_element(source, size, base + lane / 2u);
                break;
            }
            default: return interp_undefined(cpu, insn, "AdvSIMD permute -- unallocated opcode");
            }
            interp_vec_set_element(&result, size, lane, element);
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD across lanes
    // Before two-register-misc: bits[21:17] == 10000/11000, differ in bit 20.
    if ((decode & 0x9F3E0C00u) == 0x0E300800u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0x1Fu;
        // FP reductions (U == 1): bit23 selects max vs min, bit22 is sz
        // Folded left to right; the FPMax/FPMin NaN and zero-sign rules are symmetric.
        if (u && (opcode == 0x0Cu || opcode == 0x0Fu)) {
            unsigned fmt = (size & 1u) ? INTERP_FP_D : INTERP_FP_S, high = (size >> 1) & 1u;
            unsigned element = fmt + 1u, lanes = interp_vec_lanes(element, q);
            if (fmt == INTERP_FP_D || !q)
                return interp_undefined(cpu, insn, "AdvSIMD across lanes -- unallocated FP reduction size");
            interp_vec source = interp_vec_read(cpu, rn), result;
            memset(result.byte, 0, sizeof result.byte);
            uint64_t accumulator = interp_vec_element(&source, element, 0);
            for (unsigned lane = 1; lane < lanes; lane++)
                accumulator = interp_fp_minmax(fmt, accumulator, interp_vec_element(&source, element, lane), !high,
                                               opcode == 0x0Cu);
            interp_vec_set_element(&result, element, 0, accumulator);
            interp_vec_write(cpu, rd, result, 0);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // AdvSIMD SCALAR pairwise, sharing this box
        // These combine the TWO source lanes and bypass the vector size/Q reservations.
        if (scalar) {
            interp_vec source = interp_vec_read(cpu, rn), result;
            memset(result.byte, 0, sizeof result.byte);
            if (!u && opcode == 0x1Bu) { // ADDP (scalar): 2D only
                if (size != 3) return interp_undefined(cpu, insn, "AdvSIMD scalar pairwise -- ADDP needs 2D");
                interp_vec_set_element(&result, 3, 0,
                                       interp_vec_element(&source, 3, 0) + interp_vec_element(&source, 3, 1));
            } else if (u && (opcode == 0x0Cu || opcode == 0x0Du || opcode == 0x0Fu)) {
                unsigned fmt = (size & 1u) ? INTERP_FP_D : INTERP_FP_S, high = (size >> 1) & 1u;
                unsigned element = fmt + 1u;
                uint64_t a = interp_vec_element(&source, element, 0), b = interp_vec_element(&source, element, 1);
                uint64_t value;
                if (opcode == 0x0Du) { // FADDP (scalar)
                    if (high) return interp_undefined(cpu, insn, "AdvSIMD scalar pairwise -- unallocated FADDP");
                    value = interp_fp_arith(fmt, INTERP_FPOP_ADD, a, b);
                } else { // FMAXNMP/FMINNMP, FMAXP/FMINP
                    value = interp_fp_minmax(fmt, a, b, !high, opcode == 0x0Cu);
                }
                interp_vec_set_element(&result, element, 0, value);
            } else {
                return interp_undefined(cpu, insn, "AdvSIMD scalar pairwise -- unimplemented opcode");
            }
            interp_vec_write(cpu, rd, result, 0);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (size == 3 || (size == 2 && !q))
            return interp_undefined(cpu, insn, "AdvSIMD across lanes -- reserved size/Q combination");
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned lanes = interp_vec_lanes(size, q);
        uint64_t accumulator = interp_vec_element(&source, size, 0);
        switch (opcode) {
        case 0x03: { // SADDLV / UADDLV (DOUBLE width)
            uint64_t total = 0;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                total += u ? element : interp_element_sext(element, size);
            }
            interp_vec_set_element(&result, size + 1u, 0, total & interp_element_mask(size + 1u));
            break;
        }
        case 0x0A: // SMAXV / UMAXV
        case 0x1A: // SMINV / UMINV
        case 0x1B: // ADDV
            for (unsigned lane = 1; lane < lanes; lane++) {
                uint64_t element = interp_vec_element(&source, size, lane);
                if (opcode == 0x1B) {
                    accumulator = (accumulator + element) & interp_element_mask(size);
                } else if (u) {
                    int greater = element > accumulator;
                    if (opcode == 0x0A ? greater : !greater && element != accumulator) accumulator = element;
                } else {
                    int64_t left = (int64_t)interp_element_sext(accumulator, size);
                    int64_t right = (int64_t)interp_element_sext(element, size);
                    if (opcode == 0x0A ? right > left : right < left) accumulator = element;
                }
            }
            interp_vec_set_element(&result, size, 0, accumulator);
            break;
        default: return interp_undefined(cpu, insn, "AdvSIMD across lanes -- unimplemented opcode");
        }
        // A reduction is a SCALAR: only the low element is defined.
        interp_vec_write(cpu, rd, result, 0);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD two-register miscellaneous (FP16): the same operations at half precision, in a box of their
    // own at bits[23:17] == 1111100 that the mask below does not reach. Only the members this file
    // implements are decoded; the rest of the box (FABS/FNEG/FSQRT/FRINT*/FCVT*/FCMxx at .4H/.8H/H) has
    // never been decoded here and keeps reporting rather than being guessed at.
    if ((decode & 0x9FFE0C00u) == 0x0EF80800u) {
        unsigned opcode = (insn >> 12) & 0x1Fu;
        if (opcode != 0x1Du && !(opcode == 0x1Fu && !u && scalar))
            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc (FP16) -- unimplemented opcode");
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        for (unsigned lane = 0; lane < (scalar ? 1u : (q ? 8u : 4u)); lane++) {
            uint64_t a = interp_vec_element(&source, INTERP_FP_H + 1u, lane), value;
            if (opcode == 0x1Fu)
                value = interp_fp_recpx(INTERP_FP_H, a); // FRECPX
            else
                value = u ? interp_fp_rsqrt_estimate(INTERP_FP_H, a) : interp_fp_recip_estimate(INTERP_FP_H, a);
            interp_vec_set_element(&result, INTERP_FP_H + 1u, lane, value);
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD two-register misc
    if ((decode & 0x9F3E0C00u) == 0x0E200800u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0x1Fu;
        interp_vec source = interp_vec_read(cpu, rn), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned bytes = q ? 16u : 8u;

        // the floating-point members (opcodes 01100..01111, >= 10110)
        // `size` is not an element width: bit23 is an operation selector, bit22 is `sz`.
        if ((opcode >= 0x0Cu && opcode <= 0x0Fu) || opcode >= 0x16u) {
            unsigned fmt = (size & 1u) ? INTERP_FP_D : INTERP_FP_S, high = (size >> 1) & 1u;
            unsigned element = fmt + 1u;
            uint64_t saved_nzcv = cpu->nzcv; // see the note in the three-same FP block
            // FCVTL/FCVTN change the element width; sz names the NARROW format (0 half, 1 single).
            if (opcode == 0x16u || opcode == 0x17u) {
                // FCVTXN/FCVTXN2 is FCVTN with FPRounding_ODD, and exists only D -> S. U elsewhere in this
                // pair spells the FEAT_FP8 widenings (F1CVTL/F2CVTL/BF1CVTL) and bit23 the BF16 narrowings,
                // which were reaching FCVTL's code; there is no scalar FCVTL/FCVTN.
                unsigned odd = u && opcode == 0x16u && (size & 1u);
                if (high || (u && !odd) || (scalar && !odd))
                    return interp_undefined(cpu, insn,
                                            "AdvSIMD two-reg misc -- BFCVTN/F1CVTL/F2CVTL/BF1CVTL or "
                                            "unallocated FCVTL/FCVTN/FCVTXN form");
                unsigned narrow = (size & 1u) ? INTERP_FP_S : INTERP_FP_H, wide = narrow + 1u;
                // The narrow side is 64 bits of elements; Q picks the half.
                unsigned narrow_lanes = narrow == INTERP_FP_S ? 2u : 4u;
                if (opcode == 0x17u) { // FCVTL / FCVTL2
                    for (unsigned lane = 0; lane < narrow_lanes; lane++) {
                        uint64_t element_bits =
                            interp_vec_element(&source, narrow + 1u, q ? lane + narrow_lanes : lane);
                        interp_vec_set_element(&result, wide + 1u, lane, interp_fp_convert(narrow, wide, element_bits));
                    }
                    interp_vec_write(cpu, rd, result, 1);
                } else { // FCVTN / FCVTN2 / FCVTXN / FCVTXN2
                    interp_vec packed;
                    memset(packed.byte, 0, sizeof packed.byte);
                    for (unsigned lane = 0; lane < (scalar ? 1u : narrow_lanes); lane++) {
                        uint64_t element_bits = interp_vec_element(&source, wide + 1u, lane);
                        interp_vec_set_element(&packed, narrow + 1u, lane,
                                               odd ? interp_fp_convert_odd(element_bits)
                                                   : interp_fp_convert(wide, narrow, element_bits));
                    }
                    if (!q) {
                        interp_vec_write(cpu, rd, packed, 0);
                    } else {
                        interp_vec destination = interp_vec_read(cpu, rd);
                        memcpy(destination.byte + 8, packed.byte, 8);
                        interp_vec_write(cpu, rd, destination, 1);
                    }
                }
                cpu->pc = gpc + 4;
                return INTERP_NEXT;
            }
            if (fmt == INTERP_FP_D && !q && !scalar)
                return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- 2D form requires Q");
            // Per width, not interp_vec_lanes(element, q): a derived `element` makes the optimiser warn.
            unsigned fp_lanes = scalar ? 1u : (element == 3u ? (q ? 2u : 1u) : (q ? 4u : 2u));
            for (unsigned lane = 0; lane < fp_lanes; lane++) {
                uint64_t a = interp_vec_element(&source, element, lane), value;
                uint64_t all_ones = interp_element_mask(element);
                if (opcode >= 0x0Cu && opcode <= 0x0Fu) {
                    // Compare-against-zero; FABS/FNEG at 01111.
                    if (opcode == 0x0Fu) {
                        value = u ? (a ^ interp_fp_sign_mask(fmt)) : (a & ~interp_fp_sign_mask(fmt));
                    } else {
                        // Only FCMEQ is FPCompareEQ; FPCompareGE/GT/LE/LT raise Invalid for a QUIET NaN too.
                        interp_fp_compare(cpu, fmt, a, 0, !(opcode == 0x0Du && !u));
                        int ordered = !(interp_flag_c(cpu) && interp_flag_v(cpu));
                        int zero = interp_flag_z(cpu) != 0, negative = interp_flag_n(cpu) != 0;
                        int holds;
                        if (opcode == 0x0Cu)
                            holds = ordered && (u ? (!negative) : (!negative && !zero)); // FCMGE / FCMGT
                        else if (opcode == 0x0Du)
                            holds = ordered && (u ? (negative || zero) : zero); // FCMLE / FCMEQ
                        else
                            holds = ordered && negative && !u; // FCMLT
                        if (opcode == 0x0Eu && u)
                            return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated FP compare");
                        value = holds ? all_ones : UINT64_C(0);
                    }
                } else {
                    switch (opcode) {
                    case 0x18: // FRINTN / FRINTP; FRINTA / FRINTX under U
                        value = interp_fp_round_integral(fmt, a,
                                                         u ? INTERP_RM_RA : (high ? INTERP_RM_RP : INTERP_RM_RN), 0);
                        if (u && high) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated FRINT");
                        break;
                    case 0x19:
                        // FRINTM/FRINTZ (U == 0), FRINTX/FRINTI (U == 1). Only FRINTX reports Inexact.
                        if (u)
                            value = interp_fp_round_integral(fmt, a, INTERP_FPCR_RMODE(g_interp_fpcr), high ? 0 : 1);
                        else
                            value = interp_fp_round_integral(fmt, a, high ? INTERP_RM_RZ : INTERP_RM_RM, 0);
                        break;
                    case 0x1A: // FCVTNS/FCVTNU or FCVTPS/FCVTPU
                        value =
                            interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, high ? INTERP_RM_RP : INTERP_RM_RN, 0);
                        break;
                    case 0x1B: // FCVTMS/FCVTMU or FCVTZS/FCVTZU
                        value =
                            interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, high ? INTERP_RM_RZ : INTERP_RM_RM, 0);
                        break;
                    case 0x1C: // FCVTAS/FCVTAU at bit23 clear, URECPE/URSQRTE at set (.2S/.4S only)
                        if (high) {
                            if (fmt != INTERP_FP_S || scalar)
                                return interp_undefined(cpu, insn,
                                                        "AdvSIMD two-reg misc -- unallocated URECPE/URSQRTE form");
                            value = interp_uint_recip_estimate(a, u);
                        } else {
                            value = interp_fp_to_int(fmt, a, interp_fp_width(fmt), !u, INTERP_RM_RA, 0);
                        }
                        break;
                    case 0x1D: // SCVTF/UCVTF at bit23 clear, FRECPE/FRSQRTE at set
                        if (high)
                            value = u ? interp_fp_rsqrt_estimate(fmt, a) : interp_fp_recip_estimate(fmt, a);
                        else
                            value = interp_fp_from_int(fmt, a, interp_fp_width(fmt), !u,
                                                       INTERP_FPCR_RMODE(g_interp_fpcr), 0);
                        break;
                    case 0x1F: // FSQRT is the VECTOR U == 1 form, FRECPX the SCALAR U == 0 one: allocated
                               // exactly when u != scalar. bit23 clear is FRINT32Z/FRINT64Z/FRINT32X/FRINT64X
                               // (FEAT_FRINTTS), which shares this opcode and was being executed as FSQRT.
                        if (!high || (u != 0) == (scalar != 0))
                            return interp_undefined(cpu, insn,
                                                    "AdvSIMD two-reg misc -- FRINT32Z/FRINT64Z/FRINT32X/FRINT64X "
                                                    "or unallocated opcode 11111");
                        value = u ? interp_fp_sqrt(fmt, a) : interp_fp_recpx(fmt, a);
                        break;
                    default: return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unimplemented FP opcode");
                    }
                }
                interp_vec_set_element(&result, element, lane, value);
            }
            cpu->nzcv = saved_nzcv;
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        switch (opcode) {
        case 0x02:   // SADDLP / UADDLP (DOUBLE width)
        case 0x06: { // SADALP / UADALP: accumulating
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated ADDLP size");
            unsigned wide = size + 1u, wide_lanes = scalar ? 1u : interp_vec_lanes(wide, q);
            uint64_t wide_mask = interp_element_mask(wide);
            interp_vec accumulate = interp_vec_read(cpu, rd);
            for (unsigned lane = 0; lane < wide_lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane * 2u);
                uint64_t b = interp_vec_element(&source, size, lane * 2u + 1u);
                if (!u) {
                    a = interp_element_sext(a, size);
                    b = interp_element_sext(b, size);
                }
                uint64_t total = (a + b) & wide_mask;
                if (opcode == 0x06) total = (total + interp_vec_element(&accumulate, wide, lane)) & wide_mask;
                interp_vec_set_element(&result, wide, lane, total);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x03: {
            // SUQADD / USQADD: the accumulator in Vd and the operand in Vn have OPPOSITE signedness and the
            // saturation follows the accumulator's, so neither SQADD nor UQADD applies. 128-bit intermediate
            // because a 64-bit element's sum does not fit either operand type.
            interp_vec accumulate = interp_vec_read(cpu, rd);
            unsigned esize = 8u << size, misc_lanes = scalar ? 1u : interp_vec_lanes(size, q);
            uint64_t emask = interp_element_mask(size);
            for (unsigned lane = 0; lane < misc_lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane) & emask;
                uint64_t d = interp_vec_element(&accumulate, size, lane) & emask;
                __int128 total, low, high;
                if (!u) { // SUQADD: signed accumulator + unsigned operand
                    total = (__int128)(int64_t)interp_element_sext(d, size) + (__int128)a;
                    high = ((__int128)1 << (esize - 1u)) - 1;
                    low = -((__int128)1 << (esize - 1u));
                } else { // USQADD: unsigned accumulator + signed operand
                    total = (__int128)d + (__int128)(int64_t)interp_element_sext(a, size);
                    high = ((__int128)1 << esize) - 1;
                    low = 0;
                }
                if (total > high || total < low) {
                    interp_fpsr_raise(INTERP_FPSR_QC);
                    total = total > high ? high : low;
                }
                interp_vec_set_element(&result, size, lane, (uint64_t)total & emask);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x04: { // CLS (U=0) / CLZ (U=1)
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated CLS/CLZ size");
            unsigned esize = 8u << size, lanes = scalar ? 1u : interp_vec_lanes(size, q);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane) & interp_element_mask(size);
                // CLS counts leading bits MATCHING the sign, excluding it: 0..esize-1.
                uint64_t folded = ((a >> 1) ^ a) & (interp_element_mask(size) >> 1);
                unsigned count;
                if (!u)
                    count =
                        folded == 0 ? esize - 1u : (unsigned)(esize - 2u - (unsigned)(63 - __builtin_clzll(folded)));
                else
                    count = a == 0 ? esize : (unsigned)(esize - 1u - (unsigned)(63 - __builtin_clzll(a)));
                interp_vec_set_element(&result, size, lane, count);
            }
            // Must return here, not `break`: this switch's break falls into the NEXT switch, whose default
            // reports -- CLS/CLZ and SQABS/SQNEG were computed and then thrown away as unimplemented.
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x07: { // SQABS (U=0) / SQNEG (U=1)
            unsigned lanes = scalar ? 1u : interp_vec_lanes(size, q);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, lane);
                // As 0 - a, so the one overflowing input saturates through the group's helper.
                int64_t x = (int64_t)interp_element_sext(a, size);
                uint64_t value;
                if (!u && x >= 0)
                    value = a & interp_element_mask(size);
                else
                    value = interp_sqadd_element(0, a, size, 1);
                interp_vec_set_element(&result, size, lane, value);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x12:   // XTN (U=0) / SQXTUN (U=1)
        case 0x14: { // SQXTN (U=0) / UQXTN (U=1)
            // Narrowing: `size` names the RESULT element and sources are twice as wide; Q picks the half.
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated XTN size");
            unsigned narrow_lanes = 64u / (8u << size);
            interp_vec packed;
            memset(packed.byte, 0, sizeof packed.byte);
            for (unsigned lane = 0; lane < (scalar ? 1u : narrow_lanes); lane++) {
                uint64_t wide_element = interp_vec_element(&source, size + 1u, lane);
                uint64_t value;
                if (opcode == 0x12 && !u)
                    value = wide_element & interp_element_mask(size); // XTN
                else if (opcode == 0x12)
                    value = interp_sat_narrow(wide_element, size, 1, 0); // SQXTUN
                else
                    value = interp_sat_narrow(wide_element, size, u ? 0 : 1, u ? 0 : 1); // UQXTN / SQXTN
                interp_vec_set_element(&packed, size, lane, value);
            }
            if (!q || scalar) {
                interp_vec_write(cpu, rd, packed, 0);
            } else {
                interp_vec destination = interp_vec_read(cpu, rd);
                memcpy(destination.byte + 8, packed.byte, 8);
                interp_vec_write(cpu, rd, destination, 1);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        case 0x13: { // SHLL / SHLL2 (U=1): shift by the FULL width
            if (!u || size == 3) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated SHLL");
            unsigned wide = size + 1u, wide_lanes = 64u / (8u << size);
            for (unsigned lane = 0; lane < wide_lanes; lane++) {
                uint64_t a = interp_vec_element(&source, size, q ? lane + wide_lanes : lane);
                interp_vec_set_element(&result, wide, lane, (a << (8u << size)) & interp_element_mask(wide));
            }
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        default: break;
        }

        switch (opcode) {
        case 0x00:   // REV64 (U=0) / REV32 (U=1)
        case 0x01: { // REV16 (U=0)
            // Reverse bytes within each container (8, 4 or 2 by opcode); `size` is the element width.
            unsigned container = opcode == 0x01 ? 2u : (u ? 4u : 8u);
            unsigned element = 1u << size;
            if (element >= container)
                return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- REV element wider than container");
            for (unsigned base = 0; base < bytes; base += container)
                for (unsigned offset = 0; offset < container; offset += element)
                    memcpy(result.byte + base + (container - element - offset), source.byte + base + offset, element);
            break;
        }
        case 0x05: {  // CNT / NOT (size=0) / RBIT (size=1)
            if (!u) { // CNT
                if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- CNT requires 8B/16B");
                for (unsigned index = 0; index < bytes; index++)
                    result.byte[index] = (uint8_t)__builtin_popcount(source.byte[index]);
            } else if (size == 0) { // NOT / MVN
                for (unsigned index = 0; index < bytes; index++)
                    result.byte[index] = (uint8_t)~source.byte[index];
            } else if (size == 1) { // RBIT
                for (unsigned index = 0; index < bytes; index++) {
                    uint8_t value = source.byte[index];
                    value = (uint8_t)(((value & 0x55u) << 1) | ((value >> 1) & 0x55u));
                    value = (uint8_t)(((value & 0x33u) << 2) | ((value >> 2) & 0x33u));
                    value = (uint8_t)(((value & 0x0Fu) << 4) | ((value >> 4) & 0x0Fu));
                    result.byte[index] = value;
                }
            } else {
                return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated CNT/NOT/RBIT size");
            }
            break;
        }
        case 0x08:   // CMGT / CMGE (zero)
        case 0x09:   // CMEQ / CMLE (zero)
        case 0x0A: { // CMLT (zero)
            if (size == 3 && !q && !scalar)
                return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- 1D compare is reserved");
            uint64_t mask = interp_element_mask(size);
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++) {
                int64_t element = (int64_t)interp_element_sext(interp_vec_element(&source, size, lane), size);
                int holds;
                if (opcode == 0x08)
                    holds = u ? element >= 0 : element > 0;
                else if (opcode == 0x09)
                    holds = u ? element <= 0 : element == 0;
                else {
                    if (u) return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- unallocated compare-zero");
                    holds = element < 0;
                }
                interp_vec_set_element(&result, size, lane, holds ? mask : UINT64_C(0));
            }
            break;
        }
        case 0x0B: { // ABS (U=0) / NEG (U=1)
            if (size == 3 && !q && !scalar)
                return interp_undefined(cpu, insn, "AdvSIMD two-reg misc -- 1D ABS/NEG reserved");
            uint64_t mask = interp_element_mask(size);
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++) {
                int64_t element = (int64_t)interp_element_sext(interp_vec_element(&source, size, lane), size);
                uint64_t value = u ? (uint64_t)(-element) : (uint64_t)(element < 0 ? -element : element);
                interp_vec_set_element(&result, size, lane, value & mask);
            }
            break;
        }
        default: return interp_undefined(cpu, insn, "AdvSIMD two-register misc -- unimplemented opcode");
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD three different (widening/narrowing)
    // bits[11:10] == 00 separates this from three-same and across-lanes. Source and destination widths differ
    // and `size` always names the NARROWER; Q selects WHICH HALF the "2" mnemonics read or write.
    if ((decode & 0x9F200C00u) == 0x0E200000u) {
        unsigned size = (insn >> 22) & 3u, opcode = (insn >> 12) & 0xFu;
        interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm);
        int narrowing = opcode == 0x4 || opcode == 0x6; // ADDHN/RADDHN and SUBHN/RSUBHN
        // PMULL 64x64 -> 128: a 128-bit result element, no element accessor fits.
        if (opcode == 0xE && size == 3) {
            uint64_t a, b, low, high;
            memcpy(&a, left.byte + (q ? 8 : 0), 8);
            memcpy(&b, right.byte + (q ? 8 : 0), 8);
            interp_poly_mul(a, b, 64, &low, &high);
            interp_vec result;
            memcpy(result.byte, &low, 8);
            memcpy(result.byte + 8, &high, 8);
            interp_vec_write(cpu, rd, result, 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (size == 3)
            return interp_undefined(cpu, insn, "AdvSIMD three different -- 64-bit narrow element is reserved");
        unsigned wide = size + 1u, lanes = scalar ? 1u : 64u / (8u << size);
        uint64_t narrow_mask = interp_element_mask(size), wide_mask = interp_element_mask(wide);
        interp_vec result;
        memset(result.byte, 0, sizeof result.byte);
        interp_vec destination = interp_vec_read(cpu, rd);

        for (unsigned lane = 0; lane < lanes; lane++) {
            // Widening forms take narrow operands from the upper half when Q is set.
            unsigned narrow_lane = q && !narrowing ? lane + lanes : lane;
            uint64_t a, b;
            if (narrowing) {
                a = interp_vec_element(&left, wide, lane);
                b = interp_vec_element(&right, wide, lane);
                uint64_t sum = (opcode == 0x4 ? a + b : a - b) & wide_mask;
                // RADDHN/RSUBHN round: add half the discarded field first.
                if (u) sum = (sum + (UINT64_C(1) << ((8u << size) - 1u))) & wide_mask;
                interp_vec_set_element(&result, size, lane, (sum >> (8u << size)) & narrow_mask);
                continue;
            }
            a = interp_vec_element(&left, opcode == 0x1 || opcode == 0x3 ? wide : size,
                                   opcode == 0x1 || opcode == 0x3 ? lane : narrow_lane);
            b = interp_vec_element(&right, size, narrow_lane);
            // Widening forms sign-extend at U == 0, zero-extend at U == 1; PMULL is polynomial.
            uint64_t extended_a =
                opcode == 0x1 || opcode == 0x3 ? a : (u ? a & narrow_mask : (interp_element_sext(a, size) & wide_mask));
            uint64_t extended_b = u ? (b & narrow_mask) : (interp_element_sext(b, size) & wide_mask);
            uint64_t value;
            switch (opcode) {
            case 0x0: value = extended_a + extended_b; break; // SADDL / UADDL
            case 0x1: value = extended_a + extended_b; break; // SADDW / UADDW (Rn wide)
            case 0x2: value = extended_a - extended_b; break; // SSUBL / USUBL
            case 0x3: value = extended_a - extended_b; break; // SSUBW / USUBW
            case 0x5:                                         // SABAL / UABAL
            case 0x7: {                                       // SABDL / UABDL
                uint64_t difference;
                if (u) {
                    uint64_t x = a & narrow_mask, y = b & narrow_mask;
                    difference = x > y ? x - y : y - x;
                } else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    difference = (uint64_t)(x > y ? x - y : y - x);
                }
                value = difference;
                if (opcode == 0x5) value += interp_vec_element(&destination, wide, lane);
                break;
            }
            case 0x8:   // SMLAL / UMLAL
            case 0xA:   // SMLSL / UMLSL
            case 0xC: { // SMULL / UMULL
                uint64_t product;
                if (u)
                    product = (a & narrow_mask) * (b & narrow_mask);
                else
                    product = (uint64_t)((int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size));
                uint64_t base = interp_vec_element(&destination, wide, lane);
                value = opcode == 0x8 ? base + product : (opcode == 0xA ? base - product : product);
                break;
            }
            case 0x9:   // SQDMLAL / SQDMLAL2
            case 0xB:   // SQDMLSL / SQDMLSL2
            case 0xD: { // SQDMULL / SQDMULL2
                // Signed only; U=1 is unallocated. Two saturations for the accumulating forms: the doubled
                // product first, then the accumulate -- either can set QC.
                if (u || size == 0)
                    return interp_undefined(cpu, insn, "AdvSIMD three different -- unallocated doubling form");
                uint64_t product = interp_sqdmull_element(a, b, size);
                value = opcode == 0xD ? product
                                      : interp_sqadd_element(interp_vec_element(&destination, wide, lane), product,
                                                             wide, opcode == 0xB);
                break;
            }
            case 0xE: { // PMULL 8x8 -> 16
                if (u || size != 0)
                    return interp_undefined(cpu, insn, "AdvSIMD three different -- unallocated PMULL form");
                uint64_t low, high;
                interp_poly_mul(a & narrow_mask, b & narrow_mask, 8, &low, &high);
                value = low;
                break;
            }
            default: return interp_undefined(cpu, insn, "AdvSIMD three different -- unallocated opcode");
            }
            interp_vec_set_element(&result, wide, lane, value & wide_mask);
        }
        if (!narrowing) {
            interp_vec_write(cpu, rd, result, 1); // widening: full 128-bit result
        } else if (!q) {
            interp_vec_write(cpu, rd, result, 0); // ADDHN: low 64 bits, ZERO the upper half
        } else {
            memcpy(destination.byte + 8, result.byte, 8); // ADDHN2: upper half, preserve the lower
            interp_vec_write(cpu, rd, destination, 1);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD three same, and the separate three-same-FP16 box (bit22 set, bit21 clear, bits[15:14] 00)
    // that spells the same FP operations at half precision with a 3-bit opcode under an implied 11.
    unsigned fp16_three_same = (decode & 0x9F60C400u) == 0x0E400400u;
    if (fp16_three_same || (decode & 0x9F200400u) == 0x0E200400u) {
        unsigned size = (insn >> 22) & 3u;
        unsigned opcode = fp16_three_same ? (0x18u | ((insn >> 11) & 7u)) : ((insn >> 11) & 0x1Fu);
        interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm), result;
        memset(result.byte, 0, sizeof result.byte);
        unsigned bytes = q ? 16u : 8u;
        unsigned lanes = scalar ? 1u : interp_vec_lanes(size, q);
        uint64_t mask = interp_element_mask(size);

        // FP members (opcode >= 11000): bit23 the operation, bit22 `sz`
        if (opcode >= 0x18) {
            unsigned fmt = fp16_three_same ? INTERP_FP_H : ((size & 1u) ? INTERP_FP_D : INTERP_FP_S);
            unsigned high = (size >> 1) & 1u;
            if (fmt == INTERP_FP_D && !q && !scalar)
                return interp_undefined(cpu, insn, "AdvSIMD three same -- 2D form requires Q");
            unsigned fp_lanes = scalar ? 1u : interp_vec_lanes(fmt + 1u, q);
            unsigned element = fmt + 1u;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            uint64_t sign = interp_fp_sign_mask(fmt);
            // interp_fp_compare writes NZCV as scalar FCMP must; a VECTOR compare must not.
            uint64_t saved_nzcv = cpu->nzcv;
            for (unsigned lane = 0; lane < fp_lanes; lane++) {
                // Pairwise forms take both operands from Vn:Vm, not from matching lanes.
                int pairwise = u && (opcode == 0x18 || opcode == 0x1A || opcode == 0x1E) && !(opcode == 0x1A && high);
                uint64_t a, b;
                if (pairwise) {
                    const interp_vec *source = lane < fp_lanes / 2u ? &left : &right;
                    unsigned base = (lane < fp_lanes / 2u ? lane : lane - fp_lanes / 2u) * 2u;
                    a = interp_vec_element(source, element, base);
                    b = interp_vec_element(source, element, base + 1u);
                } else {
                    a = interp_vec_element(&left, element, lane);
                    b = interp_vec_element(&right, element, lane);
                }
                uint64_t value;
                if (!u) {
                    switch (opcode) {
                    case 0x18: value = interp_fp_minmax(fmt, a, b, !high, 1); break; // FMAXNM / FMINNM
                    case 0x19: {                                                     // FMLA / FMLS
                        uint64_t addend = interp_vec_element(&accumulate, element, lane);
                        value = interp_fp_muladd(fmt, addend, high ? (a ^ sign) : a, b);
                        break;
                    }
                    case 0x1A:
                        value = interp_fp_arith(fmt, high ? INTERP_FPOP_SUB : INTERP_FPOP_ADD, a, b);
                        break; // FADD / FSUB
                    case 0x1B:
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        value = interp_fp_mulx(fmt, a, b);
                        break;   // FMULX
                    case 0x1C: { // FCMEQ (register)
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        interp_fp_compare(cpu, fmt, a, b, 0);
                        value = interp_flag_z(cpu) ? interp_element_mask(element) : UINT64_C(0);
                        break;
                    }
                    case 0x1E: value = interp_fp_minmax(fmt, a, b, !high, 0); break; // FMAX / FMIN
                    case 0x1F: value = interp_fp_recip_step(fmt, a, b, high); break; // FRECPS / FRSQRTS
                    default: return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                    }
                } else {
                    switch (opcode) {
                    case 0x18: value = interp_fp_minmax(fmt, a, b, !high, 1); break; // FMAXNMP / FMINNMP
                    case 0x1A:
                        // FADDP at bit23 clear, FABD at set; FABD is NOT pairwise, hence the exclusion.
                        value = high ? (interp_fp_arith(fmt, INTERP_FPOP_SUB, a, b) & ~sign)
                                     : interp_fp_arith(fmt, INTERP_FPOP_ADD, a, b);
                        break;
                    case 0x1B:
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        value = interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b);
                        break;   // FMUL
                    case 0x1C:   // FCMGE / FCMGT
                    case 0x1D: { // FACGE / FACGT (absolute)
                        uint64_t x = a, y = b;
                        if (opcode == 0x1D) {
                            x &= ~sign;
                            y &= ~sign;
                        }
                        // FPCompareGE/GT raise Invalid for a QUIET NaN too, unlike FPCompareEQ above.
                        interp_fp_compare(cpu, fmt, x, y, 1);
                        // "ge" is C set minus unordered; "gt" adds Z clear.
                        int ordered = !(interp_flag_c(cpu) && interp_flag_v(cpu));
                        int holds = ordered && interp_flag_c(cpu) && (!high || !interp_flag_z(cpu));
                        value = holds ? interp_element_mask(element) : UINT64_C(0);
                        break;
                    }
                    case 0x1E: value = interp_fp_minmax(fmt, a, b, !high, 0); break; // FMAXP / FMINP
                    case 0x1F:
                        if (high) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                        value = interp_fp_arith(fmt, INTERP_FPOP_DIV, a, b);
                        break; // FDIV
                    default: return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated FP opcode");
                    }
                }
                interp_vec_set_element(&result, element, lane, value);
            }
            if (opcode == 0x1C || opcode == 0x1D) cpu->nzcv = saved_nzcv;
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (opcode == 0x03) { // bitwise group: size is a sub-opcode, not an element width
            interp_vec destination = interp_vec_read(cpu, rd);
            for (unsigned index = 0; index < bytes; index++) {
                uint8_t a = left.byte[index], b = right.byte[index], d = destination.byte[index];
                uint8_t value;
                if (!u) {
                    switch (size) {
                    case 0: value = (uint8_t)(a & b); break;            // AND
                    case 1: value = (uint8_t)(a & ~b); break;           // BIC
                    case 2: value = (uint8_t)(a | b); break;            // ORR (MOV when Rn == Rm)
                    default: value = (uint8_t)(a | (uint8_t)~b); break; // ORN
                    }
                } else {
                    // Which register is the mask differs; backwards is invisible until a `?:` inverts.
                    //   BSL  mask is Vd:            Vd = Vd ? Vn : Vm
                    //   BIT  mask Vm, insert true:  Vd = Vm ? Vn : Vd
                    //   BIF  mask Vm, insert false: Vd = Vm ? Vd : Vn
                    switch (size) {
                    case 0: value = (uint8_t)(a ^ b); break;
                    case 1: value = (uint8_t)((a & d) | (b & (uint8_t)~d)); break;
                    case 2: value = (uint8_t)(d ^ ((d ^ a) & b)); break;
                    default: value = (uint8_t)(d ^ ((d ^ a) & (uint8_t)~b)); break;
                    }
                }
                result.byte[index] = value;
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // The vector group reserves 64-bit elements at Q == 0; the SCALAR spelling is the D form.
        if (size == 3 && !q && !scalar && opcode != 0x10)
            return interp_undefined(cpu, insn, "AdvSIMD three same -- reserved 1D form");

        switch (opcode) {
        case 0x00:   // SHADD / UHADD
        case 0x02:   // SRHADD / URHADD
        case 0x04: { // SHSUB / UHSUB
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                uint64_t value;
                if (u) {
                    a &= mask;
                    b &= mask;
                    if (opcode == 0x04)
                        value = (a - b) >> 1;
                    else
                        // (a + b) can carry out of a 64-bit element; (a & b) + ((a ^ b) >> 1) does not.
                        value = (a & b) + (((a ^ b) >> 1) & (mask >> 1)) + (opcode == 0x02 ? ((a ^ b) & 1u) : 0u);
                } else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    if (opcode == 0x04)
                        value = (uint64_t)((x - y) >> 1);
                    else
                        value = (uint64_t)((x & y) + ((x ^ y) >> 1) + (opcode == 0x02 ? ((x ^ y) & 1) : 0));
                }
                interp_vec_set_element(&result, size, lane, value & mask);
            }
            break;
        }
        case 0x01:   // SQADD / UQADD
        case 0x05: { // SQSUB / UQSUB
            int subtract = opcode == 0x05;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                interp_vec_set_element(&result, size, lane,
                                       u ? interp_uqadd_element(a, b, size, subtract)
                                         : interp_sqadd_element(a, b, size, subtract));
            }
            break;
        }
        case 0x09:   // SQSHL / UQSHL: variable shift, right when Rm's lane is negative
        case 0x0A:   // SRSHL / URSHL
        case 0x0B: { // SQRSHL / UQRSHL
            unsigned esize = 8u << size;
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane);
                int8_t amount = (int8_t)(interp_vec_element(&right, size, lane) & 0xFFu);
                uint64_t value;
                if (amount >= 0) {
                    unsigned shift = (unsigned)amount;
                    if (opcode == 0x0A) { // SRSHL/URSHL left: exact, like SSHL
                        value = shift >= esize ? 0 : (a << shift) & mask;
                    } else if (u) {
                        uint64_t saturated = (a & mask);
                        // shift == 0 must be spelled out: at esize 64 the else arm shifts by 64, which the
                        // host masks to 0 and so saturates every nonzero input on a no-op shift.
                        int overflow =
                            shift != 0 && (shift >= esize ? saturated != 0 : (saturated >> (esize - shift)) != 0);
                        if (overflow) {
                            interp_fpsr_raise(INTERP_FPSR_QC);
                            value = mask;
                        } else {
                            value = (saturated << shift) & mask;
                        }
                    } else {
                        int64_t x = (int64_t)interp_element_sext(a, size);
                        int64_t max = esize == 64 ? INT64_MAX : (int64_t)((UINT64_C(1) << (esize - 1u)) - 1u);
                        int64_t min = esize == 64 ? INT64_MIN : -max - 1;
                        int64_t shifted = x;
                        int overflowed = 0;
                        for (unsigned step = 0; step < shift && !overflowed; step++) {
                            if (shifted > (max >> 1) || shifted < (min >> 1) || (shifted << 1) >> 1 != shifted)
                                overflowed = 1;
                            else
                                shifted <<= 1;
                        }
                        if (overflowed || shifted > max || shifted < min) {
                            interp_fpsr_raise(INTERP_FPSR_QC);
                            shifted = x < 0 ? min : max;
                        }
                        value = (uint64_t)shifted & mask;
                    }
                } else {
                    // A negative amount is a right shift, never saturates; rounding adds half the field.
                    unsigned shift = (unsigned)(-amount);
                    int rounding = opcode == 0x0A || opcode == 0x0B;
                    if (u) {
                        uint64_t x = a & mask;
                        uint64_t round = rounding && shift <= 64u && shift > 0 ? (x >> (shift - 1u)) & 1u : 0u;
                        value = shift >= esize ? round : ((x >> shift) + round);
                    } else {
                        int64_t x = (int64_t)interp_element_sext(a, size);
                        uint64_t round =
                            rounding && shift > 0 ? (uint64_t)((x >> (shift >= 64u ? 63u : shift - 1u)) & 1) : 0u;
                        int64_t shifted = shift >= esize ? (x >> (esize - 1u)) : (x >> shift);
                        value = (uint64_t)shifted + round;
                    }
                    value &= mask;
                }
                interp_vec_set_element(&result, size, lane, value);
            }
            break;
        }
        case 0x0E:   // SABD / UABD
        case 0x0F: { // SABA / UABA
            interp_vec accumulate = interp_vec_read(cpu, rd);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                uint64_t difference;
                if (u) {
                    a &= mask;
                    b &= mask;
                    difference = a > b ? a - b : b - a;
                } else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    difference = (uint64_t)(x > y ? x - y : y - x);
                }
                if (opcode == 0x0F) difference += interp_vec_element(&accumulate, size, lane);
                interp_vec_set_element(&result, size, lane, difference & mask);
            }
            break;
        }
        case 0x16: { // SQDMULH / SQRDMULH
            if (size == 0 || size == 3)
                return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated SQDMULH element size");
            for (unsigned lane = 0; lane < lanes; lane++)
                interp_vec_set_element(&result, size, lane,
                                       interp_sqdmulh_element(interp_vec_element(&left, size, lane),
                                                              interp_vec_element(&right, size, lane), size, u));
            break;
        }
        case 0x06:   // CMGT (U=0) / CMHI (U=1)
        case 0x07:   // CMGE (U=0) / CMHS (U=1)
        case 0x11: { // CMTST (U=0) / CMEQ (U=1)
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                int holds;
                if (opcode == 0x11)
                    holds = u ? a == b : (a & b) != 0;
                else if (u)
                    holds = opcode == 0x06 ? a > b : a >= b;
                else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    holds = opcode == 0x06 ? x > y : x >= y;
                }
                interp_vec_set_element(&result, size, lane, holds ? mask : UINT64_C(0));
            }
            break;
        }
        case 0x08: { // SSHL / USHL: shift by Rm's LOW BYTE
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane);
                int8_t amount = (int8_t)(interp_vec_element(&right, size, lane) & 0xFFu);
                unsigned esize = 8u << size;
                uint64_t value;
                if (amount >= 0) {
                    value = (unsigned)amount >= esize ? 0 : (a << amount);
                } else {
                    unsigned shift = (unsigned)(-amount);
                    if (u)
                        value = shift >= esize ? 0 : (a >> shift);
                    else {
                        int64_t signed_a = (int64_t)interp_element_sext(a, size);
                        value = (uint64_t)(shift >= esize ? (signed_a >> (esize - 1)) : (signed_a >> shift));
                    }
                }
                interp_vec_set_element(&result, size, lane, value & mask);
            }
            break;
        }
        case 0x0C:   // SMAX / UMAX
        case 0x0D: { // SMIN / UMIN
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                uint64_t chosen;
                if (u)
                    chosen = opcode == 0x0C ? (a > b ? a : b) : (a < b ? a : b);
                else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    chosen = (opcode == 0x0C ? (x > y) : (x < y)) ? a : b;
                }
                interp_vec_set_element(&result, size, lane, chosen);
            }
            break;
        }
        case 0x10: { // ADD (U=0) / SUB (U=1)
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t a = interp_vec_element(&left, size, lane), b = interp_vec_element(&right, size, lane);
                interp_vec_set_element(&result, size, lane, (u ? a - b : a + b) & mask);
            }
            break;
        }
        case 0x17: { // ADDP: pairwise across Rn:Rm
            if (u) return interp_undefined(cpu, insn, "AdvSIMD three same -- unallocated ADDP U bit");
            for (unsigned lane = 0; lane < lanes; lane++) {
                const interp_vec *source = lane < lanes / 2 ? &left : &right;
                unsigned base = (lane < lanes / 2 ? lane : lane - lanes / 2) * 2u;
                uint64_t a = interp_vec_element(source, size, base);
                uint64_t b = interp_vec_element(source, size, base + 1u);
                interp_vec_set_element(&result, size, lane, (a + b) & mask);
            }
            break;
        }
        case 0x12: { // MLA (U=0) / MLS (U=1)
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD three same -- 64-bit element MLA/MLS");
            interp_vec accumulate = interp_vec_read(cpu, rd);
            for (unsigned lane = 0; lane < lanes; lane++) {
                uint64_t product = interp_vec_element(&left, size, lane) * interp_vec_element(&right, size, lane);
                uint64_t base = interp_vec_element(&accumulate, size, lane);
                interp_vec_set_element(&result, size, lane, (u ? base - product : base + product) & mask);
            }
            break;
        }
        case 0x13: { // MUL / PMUL (U=1, carry-less)
            if (u) {
                if (size != 0) return interp_undefined(cpu, insn, "AdvSIMD three same -- PMUL requires 8B/16B");
                for (unsigned lane = 0; lane < lanes; lane++) {
                    uint64_t low, high;
                    interp_poly_mul(interp_vec_element(&left, 0, lane), interp_vec_element(&right, 0, lane), 8, &low,
                                    &high);
                    interp_vec_set_element(&result, 0, lane, low & 0xFFu);
                }
                break;
            }
            if (size == 3) return interp_undefined(cpu, insn, "AdvSIMD three same -- 64-bit element MUL");
            for (unsigned lane = 0; lane < lanes; lane++)
                interp_vec_set_element(
                    &result, size, lane,
                    (interp_vec_element(&left, size, lane) * interp_vec_element(&right, size, lane)) & mask);
            break;
        }
        case 0x14: { // SMAXP / UMAXP
            for (unsigned lane = 0; lane < lanes; lane++) {
                const interp_vec *source = lane < lanes / 2 ? &left : &right;
                unsigned base = (lane < lanes / 2 ? lane : lane - lanes / 2) * 2u;
                uint64_t a = interp_vec_element(source, size, base);
                uint64_t b = interp_vec_element(source, size, base + 1u);
                uint64_t chosen;
                if (u)
                    chosen = a > b ? a : b;
                else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    chosen = x > y ? a : b;
                }
                interp_vec_set_element(&result, size, lane, chosen);
            }
            break;
        }
        case 0x15: { // SMINP / UMINP
            for (unsigned lane = 0; lane < lanes; lane++) {
                const interp_vec *source = lane < lanes / 2 ? &left : &right;
                unsigned base = (lane < lanes / 2 ? lane : lane - lanes / 2) * 2u;
                uint64_t a = interp_vec_element(source, size, base);
                uint64_t b = interp_vec_element(source, size, base + 1u);
                uint64_t chosen;
                if (u)
                    chosen = a < b ? a : b;
                else {
                    int64_t x = (int64_t)interp_element_sext(a, size), y = (int64_t)interp_element_sext(b, size);
                    chosen = x < y ? a : b;
                }
                interp_vec_set_element(&result, size, lane, chosen);
            }
            break;
        }
        default: return interp_undefined(cpu, insn, "AdvSIMD three same -- unimplemented opcode");
        }
        interp_vec_write(cpu, rd, result, q);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD three-same-EXTRA: FEAT_DotProd / FEAT_I8MM / FEAT_RDM / FCMLA-FCADD
    // Same mask as the copy group, separated only by bit15.
    if ((decode & 0x9F208400u) == 0x0E008400u) {
        unsigned opcode = (decode >> 11) & 0xFu, size = (decode >> 22) & 3u;
        int n_signed, m_signed;
        // !scalar: the normalisation above folds the scalar boxes into this spelling, and no dot/MMLA form has
        // a scalar variant, so a scalar encoding here is some other instruction.
        if (!scalar && size == 2u && interp_dot_signedness(opcode, u, &n_signed, &m_signed)) {
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm);
            interp_vec result = interp_vec_read(cpu, rd);
            if (opcode <= 3u) { // SDOT / UDOT / USDOT (vector): one 4-byte dot product per 32-bit lane
                for (unsigned lane = 0; lane < (q ? 4u : 2u); lane++)
                    interp_vec_set_element(&result, 2, lane,
                                           (uint32_t)interp_vec_element(&result, 2, lane) +
                                               interp_dot4(&left, &right, 4u * lane, 4u * lane, n_signed, m_signed));
            } else { // SMMLA / UMMLA / USMMLA: 2x8 by 8x2, one eight-element dot product per lane
                if (!q) return interp_undefined(cpu, insn, "AdvSIMD three-same-extra -- MMLA requires Q=1");
                for (unsigned i = 0; i < 2u; i++)
                    for (unsigned j = 0; j < 2u; j++) {
                        uint32_t sum = (uint32_t)interp_vec_element(&result, 2, 2u * i + j);
                        sum += interp_dot4(&left, &right, 8u * i, 8u * j, n_signed, m_signed);
                        sum += interp_dot4(&left, &right, 8u * i + 4u, 8u * j + 4u, n_signed, m_signed);
                        interp_vec_set_element(&result, 2, 2u * i + j, sum);
                    }
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // SQRDMLAH / SQRDMLSH (FEAT_RDM): opcode 0000/0001 at U=1, 16- or 32-bit elements, scalar forms too.
        if (u && opcode <= 1u && (size == 1u || size == 2u)) {
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, rm);
            interp_vec accumulate = interp_vec_read(cpu, rd), result;
            memset(result.byte, 0, sizeof result.byte);
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(size, q)); lane++)
                interp_vec_set_element(&result, size, lane,
                                       interp_sqrdmlah_element(interp_vec_element(&accumulate, size, lane),
                                                               interp_vec_element(&left, size, lane),
                                                               interp_vec_element(&right, size, lane), size, opcode));
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        // FCMLA/FCADD (FEAT_FCMA) and the BF16/FP8 forms share this box and stay an honest gap report.
        return interp_undefined(cpu, insn, "AdvSIMD three-same-extra (FCMLA/FCADD, BFDOT/BFMMLA, FP8)");
    }

    // AdvSIMD vector x indexed element -- the box every compiled `*_lane` intrinsic lands in. `size` names the
    // integer element (01 = H, 10 = S) but the FP FORMAT for FMLA/FMLS/FMUL/FMULX (00 = H, 10 = S, 11 = D);
    // interp_elem_index() is keyed on the resulting element size, which is what the index split follows.
    // Still reported: FEAT_FHM (FMLAL/FMLSL, opcode 0000/0100/1000/1100 at size 10), FEAT_FCMA (FCMLA, U=1 odd
    // opcodes), FEAT_BF16 (BFDOT/BFMLAL at opcode 1111, size 01/11) and the FEAT_FP8 forms at size 11.
    if ((decode & 0x9F000400u) == 0x0F000000u) {
        unsigned opcode = (decode >> 12) & 0xFu, size = (decode >> 22) & 3u;
        // The by-element spelling shifts the vector opcodes one nibble up: 1110 is SDOT/UDOT, and 1111 is
        // USDOT at size 10 / SUDOT at size 00 -- the same pair the vector box spells 0010 and 0011.
        if (!scalar && ((opcode == 0xEu && size == 2u) || (opcode == 0xFu && !u && (size == 2u || size == 0u)))) {
            int n_signed = opcode == 0xEu ? !u : (size == 0u), m_signed = opcode == 0xEu ? !u : !(size == 0u);
            // Rm is M:Rm here, and H:L indexes the 32-bit group of Vm broadcast to every lane.
            interp_vec left = interp_vec_read(cpu, rn);
            interp_vec right = interp_vec_read(cpu, (int)(((decode >> 16) & 15u) | (((decode >> 20) & 1u) << 4)));
            interp_vec result = interp_vec_read(cpu, rd);
            unsigned index = (((decode >> 11) & 1u) << 1) | ((decode >> 21) & 1u);
            for (unsigned lane = 0; lane < (q ? 4u : 2u); lane++)
                interp_vec_set_element(&result, 2, lane,
                                       (uint32_t)interp_vec_element(&result, 2, lane) +
                                           interp_dot4(&left, &right, 4u * lane, 4u * index, n_signed, m_signed));
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // FMLA / FMLS / FMUL (U=0, opcode 0001/0101/1001) and FMULX (U=1, opcode 1001). size 01 is the FEAT_FP8
        // FDOT/FMLALL box, not these.
        if (size != 1u && ((!u && (opcode == 0x1u || opcode == 0x5u || opcode == 0x9u)) || (u && opcode == 0x9u))) {
            unsigned fmt = size == 0u ? INTERP_FP_H : (size == 2u ? INTERP_FP_S : INTERP_FP_D);
            unsigned element = fmt + 1u, index;
            int vm;
            if (!interp_elem_index(decode, element, &index, &vm))
                return interp_undefined(cpu, insn, "AdvSIMD by element -- a 64-bit index needs L == 0");
            if (fmt == INTERP_FP_D && !q && !scalar)
                return interp_undefined(cpu, insn, "AdvSIMD by element -- 2D form requires Q");
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, vm), result;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            memset(result.byte, 0, sizeof result.byte);
            uint64_t b = interp_vec_element(&right, element, index);
            for (unsigned lane = 0; lane < (scalar ? 1u : interp_vec_lanes(element, q)); lane++) {
                uint64_t a = interp_vec_element(&left, element, lane), value;
                if (opcode == 0x9u)
                    value = u ? interp_fp_mulx(fmt, a, b) : interp_fp_arith(fmt, INTERP_FPOP_MUL, a, b);
                else
                    // FUSED: one rounding of Vd + (+-Vn[lane])*Vm[index]. Multiply-then-add is wrong in the
                    // last bit and a fixture that only checks a few digits will not notice.
                    value = interp_fp_muladd(fmt, interp_vec_element(&accumulate, element, lane),
                                             opcode == 0x5u ? (a ^ interp_fp_sign_mask(fmt)) : a, b);
                interp_vec_set_element(&result, element, lane, value);
            }
            interp_vec_write(cpu, rd, result, q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // The integer forms, all of them 16- or 32-bit elements only.
        int mla = u && opcode == 0x0u, mls = u && opcode == 0x4u, mul = !u && opcode == 0x8u;
        int mulh = !u && (opcode == 0xCu || opcode == 0xDu);                       // SQDMULH / SQRDMULH
        int rdm = u && (opcode == 0xDu || opcode == 0xFu);                         // SQRDMLAH / SQRDMLSH (FEAT_RDM)
        int wide_acc = opcode == 0x2u || opcode == 0x6u;                           // S/UMLAL, S/UMLSL
        int wide_mul = opcode == 0xAu;                                             // S/UMULL
        int wide_sat = !u && (opcode == 0x3u || opcode == 0x7u || opcode == 0xBu); // SQDML{A,S}L, SQDMULL
        if ((size == 1u || size == 2u) && (mla || mls || mul || mulh || rdm || wide_acc || wide_mul || wide_sat)) {
            // Only the SATURATING forms have scalar spellings; a scalar MUL/MLA/MLAL encoding is unallocated
            // and must not fall through the scalar normalisation into the vector one.
            if (scalar && !(mulh || rdm || wide_sat))
                return interp_undefined(cpu, insn, "AdvSIMD by element -- no scalar form for this opcode");
            unsigned index;
            int vm;
            interp_elem_index(decode, size, &index, &vm);
            interp_vec left = interp_vec_read(cpu, rn), right = interp_vec_read(cpu, vm), result;
            interp_vec accumulate = interp_vec_read(cpu, rd);
            memset(result.byte, 0, sizeof result.byte);
            uint64_t b = interp_vec_element(&right, size, index), mask = interp_element_mask(size);
            int widening = wide_acc || wide_mul || wide_sat;
            unsigned wide = size + 1u,
                     lanes = scalar ? 1u : (widening ? 64u / (8u << size) : interp_vec_lanes(size, q));
            for (unsigned lane = 0; lane < lanes; lane++) {
                // The "2" mnemonics: Q picks WHICH half of Vn the narrow operands come from.
                uint64_t a = interp_vec_element(&left, size, widening && q ? lane + lanes : lane);
                if (!widening) {
                    uint64_t value;
                    if (mulh)
                        value = interp_sqdmulh_element(a, b, size, opcode == 0xDu);
                    else if (rdm)
                        value = interp_sqrdmlah_element(interp_vec_element(&accumulate, size, lane), a, b, size,
                                                        opcode == 0xFu);
                    else {
                        uint64_t product =
                            (uint64_t)((int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size));
                        uint64_t base = interp_vec_element(&accumulate, size, lane);
                        value = mla ? base + product : (mls ? base - product : product);
                    }
                    interp_vec_set_element(&result, size, lane, value & mask);
                    continue;
                }
                uint64_t value;
                if (wide_sat) {
                    uint64_t product = interp_sqdmull_element(a, b, size);
                    value = opcode == 0xBu ? product
                                           : interp_sqadd_element(interp_vec_element(&accumulate, wide, lane), product,
                                                                  wide, opcode == 0x7u);
                } else {
                    uint64_t product =
                        u ? (a & mask) * (b & mask)
                          : (uint64_t)((int64_t)interp_element_sext(a, size) * (int64_t)interp_element_sext(b, size));
                    uint64_t base = interp_vec_element(&accumulate, wide, lane);
                    value = opcode == 0x2u ? base + product : (opcode == 0x6u ? base - product : product);
                }
                interp_vec_set_element(&result, wide, lane, value & interp_element_mask(wide));
            }
            // A widening result is always 128-bit; the scalar spelling zeroes above its one element either way.
            interp_vec_write(cpu, rd, result, widening ? 1u : q);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        return interp_undefined(cpu, insn,
                                "AdvSIMD vector x indexed element -- FMLAL/FMLSL, FCMLA, BFDOT/BFMLAL, FP8, "
                                "or unallocated");
    }

    return interp_undefined(cpu, insn, "scalar floating-point and Advanced SIMD");
}

// One guest instruction; cpu->pc ends on the NEXT instruction or the branch target. Returns INTERP_NEXT,
// or INTERP_END with cpu->reason set. The switch is the ARM ARM's op0 = insn[28:25] table, in order.
static int interp_step(struct cpu *cpu) {
    uint32_t insn = 0;
    if (hl_guest_fetch_u32(cpu->pc, &insn) != 0) {
        // Unreadable instruction: the JIT's R_FETCHFAULT; a guest SIGSEGV at this PC.
        cpu->fault_addr = cpu->pc;
        cpu->reason = R_FETCHFAULT;
        return INTERP_END;
    }
    switch ((insn >> 25) & 0xF) {
    case 0x0:
        // op0 == 0000 is RESERVED and its only member, UDF, is PERMANENTLY undefined -- not a gap here, so
        // deliver a guest SIGILL instead of stopping the engine (SME and SVE below ARE allocated, stay fatal).
        // cpu->pc stays ON the instruction so `pc += 4` in a handler steps over it; si_code ILL_ILLOPC and
        // si_addr the faulting PC via pcrel_base.
        interp_raise_sync_signal(cpu, 4 /* SIGILL */, 1 /* ILL_ILLOPC */, pcrel_base(cpu->pc));
        return INTERP_END;
    case 0x1: return interp_undefined(cpu, insn, "unallocated (SME)");
    case 0x2: return interp_undefined(cpu, insn, "SVE");
    case 0x3: return interp_undefined(cpu, insn, "unallocated");
    case 0x8:
    case 0x9: return interp_exec_dp_immediate(cpu, insn);
    case 0xA:
    case 0xB: return interp_exec_branch_system(cpu, insn);
    case 0x4:
    case 0x6:
    case 0xC:
    case 0xE: return interp_exec_load_store(cpu, insn);
    case 0x5:
    case 0xD: return interp_exec_dp_register(cpu, insn);
    default: return interp_exec_simd(cpu, insn);
    }
}

// Must answer 1 wherever interp_step ends a block; more is only wasteful. The recorded range must never
// be a SUBSET of the bytes executed -- map_put's range is what SMC tests.
static int interp_block_ends(uint32_t insn) {
    if ((insn & 0x7C000000u) == 0x14000000u) return 1;                           // B / BL
    if ((insn & 0xFF000010u) == 0x54000000u) return 1;                           // B.cond
    if ((insn & 0xFF000010u) == 0x54000010u) return 1;                           // BC.cond
    if ((insn & 0x7E000000u) == 0x34000000u) return 1;                           // CBZ / CBNZ
    if ((insn & 0x7E000000u) == 0x36000000u) return 1;                           // TBZ / TBNZ
    if ((insn & 0xFE000000u) == 0xD6000000u) return 1;                           // BR / BLR / RET / ERET
    if ((insn & 0xFF000000u) == 0xD4000000u) return 1;                           // SVC / BRK / HLT / ...
    if ((insn & 0xFFFFFFE0u) == 0xD50B7520u) return 1;                           // ic ivau -> R_ICFLUSH
    if ((insn & 0xFFFFF01Fu) == 0xD503301Fu && ((insn >> 5) & 7) == 6) return 1; // ISB -> R_ICCOMMIT
    return 0;
}

// Block descriptor + translate_block. Cap the block so the descriptor fits the dispatcher's arena headroom
// and one block stays preemptible by the c->irq poll; splitting at the cap is an ordinary chain exit.
#define INTERP_BLOCK_MAX_INSNS 4096u

// The descriptor only DELIMITS: no decoded instructions are cached, so re-decoding each execution keeps SMC
// coherent by construction and only the block EXTENT goes stale. Must be a distinct non-NULL pointer per
// guest PC, from the arena bump pointer (not malloc) so the arena-membership accounting keeps working.
#define INTERP_BLOCK_MAGIC UINT64_C(0x484C494E54455250) // "HLINTERP"

struct interp_block {
    uint64_t magic;       // foreign/stale descriptor guard
    uint64_t guest_start; // entry guest PC == the map key
    uint64_t guest_end;   // one past the last instruction the pre-scan decoded
    uint64_t insn_count;  // diagnostics only
};

// Unreachable (G_BLOCK_ALIGN is literal 0) but must exist: the call compiles inside `if (0)`, not `#if 0`.
static void emit32(uint32_t instruction) {
    memcpy(g_cp, &instruction, sizeof instruction);
    g_cp += sizeof instruction;
}

// Never reached (nothing to fold: this back-edge is just cpu->pc), but core/dispatch.c calls it after
// every block.
static void tier2_promote(uint64_t gpc) {
    (void)gpc;
}

static void *translate_block(uint64_t gpc) {
    HL_LOGF(&g_jit_log, HL_LOG_TAG_TRANSLATE, "isa=aarch64 backend=interp guest_pc=%#llx", (unsigned long long)gpc);
    // Observe MAP_SHARED alias writes before decoding, as translate.c does.
    uint64_t source_page = gpc & ~UINT64_C(0xFFF);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));

    // The range still covers an unfetchable instruction, so run_block raises R_FETCHFAULT at the right PC.
    uint64_t cursor = gpc;
    uint64_t count = 0;
    while (count < INTERP_BLOCK_MAX_INSNS) {
        uint32_t insn = 0;
        count++;
        cursor += 4;
        if (hl_guest_fetch_u32(cursor - 4, &insn) != 0) break;
        if (interp_block_ends(insn)) break;
    }

    // Cannot overflow: the dispatcher guaranteed CACHE_EMIT_HEADROOM. Checked anyway.
    while ((uintptr_t)g_cp & 15u)
        *g_cp++ = 0;
    if (g_cp + sizeof(struct interp_block) > g_cache + CACHE_SZ) {
        static const char message[] = "interpreter block descriptor does not fit the code arena";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        return NULL;
    }
    struct interp_block *block = (struct interp_block *)g_cp;
    g_cp += sizeof *block;
    block->magic = INTERP_BLOCK_MAGIC;
    block->guest_start = gpc;
    block->guest_end = cursor;
    block->insn_count = count;

    // Key = entry PC; [guest_start, guest_end) is the SOURCE interval map_invalidate_source_ranges() intersects.
    // `body` = the same address (no prologue); non-NULL map_body() means "live translation" to patch_links_to().
    map_put(gpc, gpc, cursor, block, block);
    // SMC precise gate: without the page marks and the 64-byte line set (what txln_flush_class() classifies
    // an `ic ivau` against), the cached block EXTENT survives a rewrite of the branch that determined it.
    txpg_mark(gpc, cursor);
    if (g_txln_active)
        for (uint64_t line = gpc >> 6; line <= (cursor - 1) >> 6; line++)
            txln_put(line);
    return block;
}

// run_block / block_return: the dispatcher's boundary; interp_dispatch.h defines G_OWN_TRAMPOLINES so
// core/dispatch.c calls these instead of emitting its AArch64 pair. `static` is load-bearing:
// visibility("hidden") leaves the symbol STB_GLOBAL in a static link, the
// dual archive links BOTH target objects, and namespace.h does not rename these two.
static void run_block(struct cpu *cpu, void *code);
static void block_return(void);

static void run_block(struct cpu *cpu, void *code) {
    const struct interp_block *block = (const struct interp_block *)code;
    if (block == NULL || block->magic != INTERP_BLOCK_MAGIC) {
        // Not this backend's descriptor: a JIT-written pcache/checkpoint that host-ISA identity
        // (pcache_engine_id) should have rejected.
        static const char message[] = "interpreter entered a block that it did not translate";
        (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
        cpu->reason = R_BRANCH;
        return;
    }

    // savemask=0 -- this is the hottest line in the engine (once per guest block) and savemask=1 makes glibc
    // issue a real rt_sigprocmask here. interp_restore_handler_mask does the restore on the fault path
    // instead, where it is paid once per fault rather than once per block. sigsetjmp/siglongjmp and NOT
    // setjmp/longjmp: on Darwin setjmp/longjmp are the mask-SAVING pair, so sigsetjmp(.,0) is the only
    // portable way to say "this pad does not touch the mask" (same idiom as linux_abi/thread.c's probe pad).
    if (sigsetjmp(g_interp_marker_jmp, 0) != 0) {
        // Both abandon paths already left cpu as the dispatcher needs it; no architectural state changed.
        g_interp_access_active = 0;
        g_interp_marker_armed = 0;
        g_interp_marker_cpu = NULL;
        return;
    }
    g_interp_marker_cpu = cpu;
    g_interp_marker_armed = 1;

    uint64_t executed = 0;
    for (;;) {
        // Poll AFTER one instruction retires: exiting with cpu->pc unchanged gets the same block forever.
        if (executed && cpu->irq) {
            cpu->reason = R_BRANCH;
            break;
        }
        // Ordinary chain exit.
        if (cpu->pc < block->guest_start || cpu->pc >= block->guest_end) {
            cpu->reason = R_BRANCH;
            break;
        }
        if (interp_step(cpu) == INTERP_END) break;
        executed++;
    }

    g_interp_marker_armed = 0;
    g_interp_marker_cpu = NULL;
}

// Nothing here is executable so nothing branches in, but the symbol must exist and be address-taken:
// sigframe_resume_dispatch bakes it. Abort, not return -- a silent return spins the dispatcher on a stale
// cpu->reason.
static void block_return(void) {
    static const char message[] = "interpreter received an invalid generated-code return";
    (void)jit_fail(HL_STATUS_CORRUPT, message, sizeof message - 1u);
    abort();
}

// Self-modifying guest code, same model as the JIT: the reason codes and queue are checkpoint-image state.
static void smc_queue_line(struct cpu *c, uint64_t address) {
    // ET_EXEC code sits at a collision-avoidance bias while its pointers stay link-time low; map source
    // intervals use the real executable address, so normalize as dispatch does.
    if (g_nonpie_lo && address >= g_nonpie_lo && address < g_nonpie_hi) address += g_nonpie_bias;
    uint64_t start = address & ~UINT64_C(63), end = start + 64;
    for (uint32_t i = 0; i < c->smc_range_count; i++) {
        if (end < c->smc_ranges[i][0] || start > c->smc_ranges[i][1]) continue;
        if (start < c->smc_ranges[i][0]) c->smc_ranges[i][0] = start;
        if (end > c->smc_ranges[i][1]) c->smc_ranges[i][1] = end;
        return;
    }
    if (c->smc_range_count == SMC_RANGE_CAP) {
        c->smc_range_overflow = 1;
        return;
    }
    c->smc_ranges[c->smc_range_count][0] = start;
    c->smc_ranges[c->smc_range_count][1] = end;
    c->smc_range_count++;
}

static void aarch64_smc_queue_range(uint64_t first, uint64_t last, void *opaque) {
    struct cpu *c = opaque;
    for (uint64_t line = first & ~UINT64_C(63); line < last;) {
        smc_queue_line(c, line);
        if (line > UINT64_MAX - 64) break;
        line += 64;
    }
}

// G_SMC_COPYOUT. NOT inert here: a syscall copying to user memory can move the branch that determined a
// cached block extent.
static void aarch64_smc_copyout(uint64_t first, uint64_t last) {
    if (last <= first) return;
    struct cpu *c = pthread_getspecific(g_cpu_key);
    if (c == NULL) return;
    aarch64_smc_queue_range(first, last, c);
    hl_logical_vma_visit_exec_aliases(first, last, aarch64_smc_queue_range, c);
}

// R_ICFLUSH: queue only; smc_commit() classifies under g_jit_lock so a changed line is classified once.
static void smc_icflush(struct cpu *c, uint64_t va) {
    // Latch even for a never-translated line: g_smc_seen means "this guest generates code" engine-wide.
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    smc_queue_line(c, va);
}

// R_ICCOMMIT: what must be dropped is not host code (there is none) but the gpc->descriptor lookup.
static int smc_commit(struct cpu *c) {
    pthread_mutex_lock(&g_jit_lock);
    txln_activate();                // arm eager line recording; may request a priming wholesale drop
    int force_whole = g_txln_prime; // first SMC after activation: no lines recorded -> cannot classify
    g_txln_prime = 0;
    if (!force_whole && !c->smc_range_count && !c->smc_range_overflow) {
        pthread_mutex_unlock(&g_jit_lock);
        return 1;
    }
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    if (!c->smc_range_overflow && !force_whole) {
        uint32_t retained = 0;
        for (uint32_t i = 0; i < c->smc_range_count; i++) {
            uint64_t dirty_start = UINT64_MAX, dirty_end = 0;
            for (uint64_t line = c->smc_ranges[i][0]; line < c->smc_ranges[i][1]; line += 64) {
                // class 0 = never translated (nothing stale), 1 = first flush or bytes changed (drop),
                // 2 = translated but unchanged, i.e. benign icache maintenance (skip).
                if (txln_flush_class(line) == 1) {
                    if (dirty_start == UINT64_MAX) dirty_start = line;
                    dirty_end = line + 64;
                }
            }
            if (dirty_start != UINT64_MAX) {
                c->smc_ranges[retained][0] = dirty_start;
                c->smc_ranges[retained][1] = dirty_end;
                retained++;
            }
        }
        c->smc_range_count = retained;
        if (!retained) {
            pthread_mutex_unlock(&g_jit_lock);
            c->smc_range_overflow = 0;
            return 1;
        }
    }
    pthread_mutex_unlock(&g_jit_lock);
    // Map readers are lock-free: a peer must not still be in run_block against a range about to go stale.
    stw_mapping_begin();
    uint32_t removed;
    if (force_whole || c->smc_range_overflow) {
        removed = g_live_map_count;
        map_clear();
        // Inert here (G_IBTC_FILL is a no-op); kept so a checkpoint written here can be restored by the JIT.
        memset(g_ibtc, 0, sizeof g_ibtc);
        txpg_clear();
    } else {
        removed = map_invalidate_source_ranges((const uint64_t (*)[2])c->smc_ranges, c->smc_range_count);
    }
    pend_reset();
    HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT, "smc invalidate backend=interp mode=%s ranges=%u removed=%u retained=%u",
            (force_whole || c->smc_range_overflow) ? "whole" : "targeted", c->smc_range_count, removed,
            g_live_map_count);
    stw_mapping_end();
    c->smc_range_count = 0;
    c->smc_range_overflow = 0;
    return 1;
}

// Contract stubs, each inert for a stated reason -- except this one, which publishes the logical-VMA hull.
static void aarch64_soft_filter_refresh(struct cpu *c) {
    uint64_t first = UINT64_MAX, last = 0;
    hl_logical_vma_snapshot *snapshot =
        atomic_load_explicit(hl_logical_vma_global_snapshot_source(), memory_order_acquire);
    if (snapshot != NULL && snapshot->count != 0) {
        first = snapshot->views[0].guest_first;
        last = snapshot->views[snapshot->count - 1].guest_last;
    }
    c->soft_filter_first = first;
    c->soft_filter_last = last;
}

// Never raised here (accesses resolve inline), but the codes are shared vocabulary: G_DISPATCH_REASON turns
// R_SOFTMISS/R_SOFTSPAN/R_SOFTCOMMIT into a guest fetch fault so a JIT-written checkpoint cannot mis-resume.
static int aarch64_soft_tlb_miss(struct cpu *c) {
    (void)c;
    return 0;
}

static int aarch64_soft_tlb_span(struct cpu *c) {
    (void)c;
    return 0;
}

static int aarch64_soft_bounce_commit(struct cpu *c) {
    // No bounce can be pending; 1 = "committed" is the JIT's answer for an unarmed bounce.
    (void)c;
    return 1;
}

// Never consulted (G_BLOCK_ALIGN is literal 0); 0 spells "§B on", the ordinary non-tuning path.
static int shadowgate(void) {
    return 0;
}

// The persistent cache stores HOST CODE; a JIT-written file holds AArch64 instructions this backend would
// have to execute, so load is a clean MISS and save a no-op. PCACHE_*_HOOK stay undefined so their #ifdef'd
// call sites vanish; the globals below are storage read from outside this file.
#define PC_IMG_BASE 0x0000040000000000ull    // fixed guest image base
#define PC_INTERP_BASE 0x0000048000000000ull // fixed interpreter (ld.so) base

static int g_pcache; // HL_PCACHE=1 requested (never hits)
static int g_coldprof;
static uint64_t g_force_base;   // one-shot fixed-VA request consumed by load_elf
static int g_force_base_failed; // a fixed-VA map fell back to a kernel base
static uint64_t g_pc_binid;     // binary + interp + argv0 + build + host ISA
static uint64_t g_pc_entry;     // initial guest pc
static int g_pcache_loaded;     // never set here
static int g_pcache_forked;     // never set here
static int g_nreloc;            // always zero here

// Engine-identity mix-in for the cache key. Must be right even though the cache never hits: host_isa is
// HL_HOST_CPU_ISA, not a hardcoded 1 -- passing 1 would collide an x86-64-host identity with a JIT-written
// cache for the same guest, resolved by executing AArch64 on x86-64. Same value keys the CHECKPOINT image.
static uint64_t pcache_engine_id(void) {
    static const char tag[] = __DATE__ " " __TIME__;
    uint64_t build = hl_digest_bytes(HL_DIGEST_SEED, tag, sizeof tag - 1);
    uint64_t self = hl_identity_source(&g_jit_services, g_self_path);
    build = hl_digest_bytes(build, &self, sizeof self);
    // Bit 0 marks "interpreter", so this identity can never equal the JIT's on a shared ISA number.
    uint64_t modes = 1u;
    return hl_identity_configuration(build, HL_HOST_CPU_ISA_AARCH64, HL_HOST_CPU_ISA, modes);
}

static uint64_t pcache_make_id(const char *prog_host, const char *interp_host, const char *argv0) {
    uint64_t program = hl_identity_source(&g_jit_services, prog_host);
    uint64_t interpreter = interp_host ? hl_identity_source(&g_jit_services, interp_host) : 0xABCDEFull;
    return hl_identity_mix(program, interpreter, pcache_engine_id(), hl_identity_name(argv0));
}

// Always a clean MISS.
static int pcache_load(uint64_t entry_jump) {
    (void)entry_jump;
    return 0;
}

// A no-op: descriptors are keyed to this process's mapping and hold no translated bytes.
static void pcache_save(void) {
}

// Nothing is persisted, so nothing to poison.
static void pcache_poison_check(void) {
}

// No cache directory is opened.
static void pcache_directory_close(void) {
}

static void pcache_note_fixed_img(uint64_t base, uint64_t span) {
    (void)base;
    (void)span;
}
