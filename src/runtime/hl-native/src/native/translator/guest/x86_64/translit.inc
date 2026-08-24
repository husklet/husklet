// translator/guest/x86_64/translit -- the x86-64 guest -> x86-64 host SAME-ISA TRANSLITERATOR: the
// diagonal of the (guest ISA x host CPU) matrix, and the mirror of guest/aarch64/translate.c on an ARM64
// host. Straight-line guest instructions are copied into the code cache verbatim; only block boundaries and
// the few operands whose meaning depends on WHERE the bytes live are rewritten.
//
// Textually included by interp.c, which stays the fallback: this file never has to be complete. Anything it
// declines leaves the block to the interpreter, per block, so a gap here is slow rather than wrong.
//
// THE REGISTER MODEL. An x86-64 host has 16 GPRs and an x86-64 guest wants all 16, so nothing is stolen.
// All sixteen guest GPRs live in the matching host GPRs for the whole block -- guest RSP is the host RSP --
// and `struct cpu` is reached through the %gs segment base, which is set once per guest thread with
// arch_prctl(ARCH_SET_GS, cpu). `mov %gs:OFF, %reg` therefore costs no register at all, which is the
// x86-64 spelling of the ARM64 host's mrs TPIDRRO_EL0 (host/aarch64/asm.c hl_a64_load_cpu). The engine
// consequently OWNS the real %gs, so a guest instruction with an %fs or %gs prefix is declined rather than
// executed; guest %fs (Linux TLS, ubiquitous) is left entirely to the interpreter for the same reason --
// swapping the host FS base per block would break every host signal handler, which reads its own TLS.
// Scratch is needed only at block boundaries, and comes from cpu->mmscratch through %gs, never from the
// guest stack: the guest red zone at [rsp-128, rsp) is never written.
//
// THE NON-PIE BIAS RULE. A non-PIE ET_EXEC is mapped HIGH at +g_nonpie_bias while its baked pointers stay
// LOW, so the JIT emits a runtime fold on EVERY memory access (guest/x86_64/address.c emit_bias: if the
// address has no bits above 32, add the bias) and un-biases address MATERIALISATION (lower/mov.c,
// interp_lea_value) in the opposite direction. Verbatim copying cannot express a register-derived fold
// without scratch state. Stage one therefore copies only memory-free instructions and RIP-relative
// dereferences (whose disp32 can be re-aimed at storage), while LEA remains aimed at the low guest address.
// Base/index, moffs, string, stack, call and return forms stay in the interpreter.

#include "../../host/x86_asm.h"
#include "cpu.h"
#include "frame.h"

struct interp_block;

#if defined(__linux__) && defined(HL_HOST_CPU_X86_64)
#include <sys/syscall.h>
#endif

// The offsets the entry trampoline below bakes as literals. cpu.h owns the numbers; these tie the two.
_Static_assert(OFF_HSP == 168 && OFF_MM == 656 && OFF_RIP == 128 && OFF_RSN == 160,
               "translit trampoline literals drifted from cpu.h");
_Static_assert(__builtin_offsetof(struct cpu, r) == 0, "translit bakes r[] at offset 0");

#define TL_MM_FLAGS OFF_MM       // guest EFLAGS image, handed across the boundary in C
#define TL_MM_ENTRY (OFF_MM + 8) // block entry pointer on the way in; RET's rax save on the way out

// Bound on one block. The host code cap matters more than the instruction count: the arena is shared with
// every other block and the dispatcher flushes wholesale when it fills.
#define TL_MAX_INSNS 192
#define TL_MAX_BYTES 3072

// ---- the host<->guest boundary
//
// Enters with the host stack live, leaves through the epilogue every block ends with. The six host
// callee-saved registers are pushed here rather than into cpu->host_save[]: those slots are the AArch64
// JIT's and are part of the checkpoint image, and the host stack is already the right place for them.
// cpu->host_sp is recorded AFTER the pushes, so the epilogue's `mov %gs:168,%rsp` lands exactly on them.
#if defined(__linux__) && defined(HL_HOST_CPU_X86_64)
extern void hl_x86_translit_enter(struct cpu *cpu, void *entry) __attribute__((visibility("hidden")));
__asm__(".pushsection .text\n.p2align 4\n.hidden hl_x86_translit_enter\n"
        ".type hl_x86_translit_enter,%function\nhl_x86_translit_enter:\n"
        "push %rbx\npush %rbp\npush %r12\npush %r13\npush %r14\npush %r15\n"
        "mov %rsp,%gs:168\n"    /* cpu->host_sp */
        "mov %rsi,%gs:664\n"    /* cpu->mmscratch[1] = entry */
        "push %gs:656\npopfq\n" /* guest EFLAGS, built in C */
        "mov %gs:0,%rax\nmov %gs:8,%rcx\nmov %gs:16,%rdx\nmov %gs:24,%rbx\n"
        "mov %gs:40,%rbp\nmov %gs:48,%rsi\nmov %gs:56,%rdi\n"
        "mov %gs:64,%r8\nmov %gs:72,%r9\nmov %gs:80,%r10\nmov %gs:88,%r11\n"
        "mov %gs:96,%r12\nmov %gs:104,%r13\nmov %gs:112,%r14\nmov %gs:120,%r15\n"
        "mov %gs:32,%rsp\n" /* guest RSP last: from here the host stack is the guest stack */
        "jmp *%gs:664\n"
        ".size hl_x86_translit_enter,.-hl_x86_translit_enter\n.popsection\n");
#endif

// ---- the flag substrate at the boundary
//
// Inside a block the guest's flags ARE the host's flags, which is most of why same-ISA transliteration is
// worth doing: no flag is materialised, and cmp->jcc costs one instruction. cpu->nzcv and its PF/AF/DF side
// lanes (interp.c) are the format everything else in the engine reads, so convert once per boundary in C
// rather than in emitted code -- PF is an even-parity of a stored byte, which is several instructions to
// compute and zero here.
static uint64_t translit_flags_in(const struct cpu *cpu) {
    uint64_t flags = hl_x86_signal_nzcv_to_eflags(cpu->nzcv); // CF/ZF/SF/OF + the reserved bit 1
    flags |= (cpu->df & 1) << 10;
    flags |= (uint64_t)((cpu->af >> 4) & 1) << 4;
    if (!__builtin_parity((unsigned)(cpu->pf & 0xFF))) flags |= 1u << 2; // even parity == x86 PF
    return flags;                                                        // TF/ID/AC deliberately 0
}

static void translit_flags_out(struct cpu *cpu, uint64_t flags) {
    cpu->nzcv = hl_x86_signal_eflags_to_nzcv(flags);
    cpu->df = (flags >> 10) & 1;
    cpu->pf = ((flags >> 2) & 1) ? 0 : 1; // PF=1 <-> the stored byte has even parity
    cpu->af = (uint64_t)((flags >> 4) & 1) << 4;
}

// ---- the switch
//
// HL_TRANSLIT selects the same-ISA backend. HL_TRANSLIT_DEFAULT is the build-time default; the
// HL_TRANSLIT launch option (engine/options.c) overrides it per launch, so one binary can be gated in
// both states -- a compile-time-only switch makes "with the transliterator" and "without it" two
// different binaries, and two binaries are never a differential test of one.
//
// Resolved ONCE, on the first call, and cached: translit_image_ok() runs on every block entry and
// hl_option_get is a linear scan of the registry. The first call is hl_x86_init's SA_ONSTACK decision
// (engine/target/x86_64.c), which is after the launch store is bound and before any guest code runs, so
// the cache can never be filled from an unbound store. Guest exec keeps the same immutable launch option
// store in the worker process, so the cached selection remains the selection for every later image.
//
// The environment fallback is a DELIBERATE, REVIEWED EXCEPTION to "ambient environment does not
// configure the guest engine", and it is not precedent for anything else. It exists because the
// standalone engine workers (hl-x86_64/hl-aarch64) build their option store from a RuntimePlan, and an
// explicit store never imports the host environment (engine/runtime.c: `if (source_options == NULL)
// hl_options_import_environment`) -- so without it this backend cannot be selected from a command line
// at all, and the only way to measure it is to rebuild with a different HL_TRANSLIT_DEFAULT.
//
// That is exactly the mechanism that let these 475 lines sit unexecuted by any gate for the whole
// history of this repository: `git log -S HL_TRANSLIT_DEFAULT` finds it has never been anything but 0,
// and a feature that can only be measured by rebuilding goes unmeasured. The exception buys a corpus
// run, a benchmark arm and a bisect that need one binary and two environments.
//
// The conditions it is granted under: the compiled default stays 0, so an unset environment changes
// nothing; and the gate (hl-engine/tests/translit_differential.rs) selects the backend through the
// LAUNCH OPTION, never through the environment, so the gate does not depend on this path existing.
// Same lazy self-contained shape as HL_SOFT_SHARED_RESOLVER in the aarch64 translator.
#ifndef HL_TRANSLIT_DEFAULT
#define HL_TRANSLIT_DEFAULT 0
#endif

#if defined(__linux__) && defined(HL_HOST_CPU_X86_64)
static int translit_selected = -1; // inside the gate: off a Linux/x86-64 host it would be an unused static

// Why a launch got the speedup, or did not. Both refusals below are SILENT and permanent, and an
// operator has no other way to tell "transliterated and fast" from "refused and slow" than by timing:
//   - a non-PIE ET_EXEC placed at a storage bias admits only the bounded stage-one subset;
//   - g_rwx_guest latches on the first anonymous PROT_EXEC mmap or mprotect and is NOT cleared by
//     execve, so one 4 KiB page of JIT arena disables this backend for the process and for every image
//     it later execs -- which is every JIT-hosting guest there is.
// Counted, not inferred: `blocks` is what the filter admitted, `entries` is what actually ran, and
// `declined` is builds stopped by an image or displaced-operand predicate. A gate that only knows the option was SET does
// not know the backend ran; these are what make that a measurement.
static uint64_t translit_blocks;
static uint64_t translit_entries;
static uint64_t translit_declined;
static uint64_t translit_displaced_blocks;
static uint64_t translit_displaced_entries;
static uint64_t translit_displaced_operand_declined;
static uint64_t translit_displaced_riprel_projected;
static uint64_t translit_displaced_lea_low;
static uint64_t translit_displaced_riprel_unreachable;
#endif

static int translit_enabled(void) {
#if defined(__linux__) && defined(HL_HOST_CPU_X86_64)
    if (translit_selected < 0) {
        const char *value = hl_option_get("HL_TRANSLIT");
        if (value == NULL) value = getenv("HL_TRANSLIT");
        translit_selected = value != NULL ? (value[0] != '0' && value[0] != 0) : HL_TRANSLIT_DEFAULT;
    }
    return translit_selected;
#else
    return 0;
#endif
}

#if defined(__linux__) && defined(HL_HOST_CPU_X86_64)

// %gs base == this thread's cpu. Per thread, so re-published whenever run_block sees a cpu it has not
// published for -- a cloned guest thread, a fork child and a restored checkpoint all land here.
#ifndef ARCH_SET_GS
#define ARCH_SET_GS 0x1001
#endif
static __thread struct cpu *g_translit_gs_cpu;

static int translit_bind_cpu(struct cpu *cpu) {
    if (g_translit_gs_cpu == cpu) return 1;
    if (syscall(SYS_arch_prctl, ARCH_SET_GS, (unsigned long)(uintptr_t)cpu) != 0) return 0;
    g_translit_gs_cpu = cpu;
    return 1;
}

// A block may only be transliterated while the whole-image preconditions hold. Both are runtime facts that
// can turn on mid-run, so run_block re-tests before entering emitted code and interprets instead.
//   - displaced images are filtered per instruction by translit_displaced_safe below.
//   - store-alias observation: an emulated MAP_SHARED mapping needs every guest store queued for writeback
//     (interp_store does that by hand), and a PROT_EXEC guest mapping needs SMC page protection the
//     interpreter does not carry. Verbatim stores do neither.
static int translit_image_ok(void) {
    return translit_enabled() && !jit86_store_alias_observation_active();
}

// ---- the instruction filter
//
// Deliberately a whitelist. Everything admitted is baseline x86-64 with no engine-visible side effect
// beyond its own registers/memory, so copying the bytes is the whole implementation. Anything not named
// ends the block, and the interpreter takes it from there.
enum {
    TL_NO = 0,   // not transliterable: end the block before it
    TL_COPY,     // copy the bytes verbatim
    TL_JCC,      // conditional branch: host jcc to a second epilogue
    TL_JMP,      // direct jmp rel
    TL_CALL,     // direct call rel32
    TL_RET,      // ret / ret imm16
    TL_JMP_REG,  // jmp *%reg
    TL_CALL_REG, // call *%reg
    TL_SYSCALL
};

static int translit_classify_two(const struct insn *insn) {
    uint8_t op = insn->op;
    if (op == 0x05) return TL_SYSCALL;
    if (op >= 0x80 && op <= 0x8F) return TL_JCC;
    if (op == 0x1E || op == 0x1F) return TL_COPY;                             // endbr64 / multi-byte nop
    if (op >= 0x40 && op <= 0x4F) return TL_COPY;                             // cmovcc
    if (op >= 0x90 && op <= 0x9F) return TL_COPY;                             // setcc
    if (op >= 0xC8 && op <= 0xCF) return TL_COPY;                             // bswap
    if (op == 0xA3 || op == 0xAB || op == 0xB3 || op == 0xBB) return TL_COPY; // bt/bts/btr/btc r/m,r
    if (op == 0xBA) return TL_COPY;                                           // bt group, imm8
    if (op == 0xA4 || op == 0xA5 || op == 0xAC || op == 0xAD) return TL_COPY; // shld/shrd
    if (op == 0xAF) return TL_COPY;                                           // imul r,r/m
    if (op == 0xB0 || op == 0xB1) return TL_COPY;                             // cmpxchg
    if (op == 0xC0 || op == 0xC1) return TL_COPY;                             // xadd
    if (op == 0xB6 || op == 0xB7 || op == 0xBE || op == 0xBF) return TL_COPY; // movzx/movsx
    // bsf/bsr only WITHOUT F3: the same bytes are tzcnt/lzcnt on a BMI1 host, and which one runs would then
    // depend on the host CPU rather than on the CPUID this engine advertises. That is the general rule for
    // anything added to this list -- an instruction may be copied only if it is in the intersection of what
    // hl_x86_cpuid() advertises and what every supported host actually implements, or a guest that probed
    // CPUID correctly still takes a #UD. Everything admitted here is baseline x86-64.
    if ((op == 0xBC || op == 0xBD) && !insn->rep) return TL_COPY;
    return TL_NO;
}

static int translit_classify(const struct insn *insn) {
    // Prefixes and maps the model cannot carry. %fs/%gs: the engine owns %gs and rewrites guest segment
    // accesses against cpu->fs_base/gs_base. 0x67: 32-bit addressing wraps, which a verbatim copy would
    // apply to a 64-bit host address. VEX/EVEX/0F38/0F3A and x87: guest xmm/ST live in cpu->v/cpu->st and
    // are never loaded into host registers by this backend.
    if (insn->len <= 0 || insn->vex || insn->evex || insn->map3 || insn->seg || insn->addr32) return TL_NO;
    if (insn->two) return translit_classify_two(insn);
    uint8_t op = insn->op;
    if (op < 0x40 && (op & 7) <= 5) return TL_COPY; // the eight ALU groups
    if (op >= 0x50 && op <= 0x5F) return TL_COPY;   // push/pop reg
    if (op == 0x63) return TL_COPY;                 // movsxd
    if (op == 0x68 || op == 0x6A) return TL_COPY;   // push imm
    if (op == 0x69 || op == 0x6B) return TL_COPY;   // imul imm
    if (op >= 0x70 && op <= 0x7F) return TL_JCC;
    if (op == 0x80 || op == 0x81 || op == 0x83) return TL_COPY;
    if (op >= 0x84 && op <= 0x8B) return TL_COPY;            // test/xchg/mov
    if (op == 0x8D) return TL_COPY;                          // lea
    if (op == 0x8F) return insn->reg == 0 ? TL_COPY : TL_NO; // pop r/m (/0 only; the rest are #UD)
    if (op >= 0x90 && op <= 0x99) return TL_COPY;            // nop/xchg rAX/cwtl/cltd
    if (op >= 0xA0 && op <= 0xA3) return TL_COPY;            // mov moffs
    if (op >= 0xA4 && op <= 0xAF) return TL_COPY;            // string ops + test imm
    if (op >= 0xB0 && op <= 0xBF) return TL_COPY;            // mov reg, imm
    if (op == 0xC0 || op == 0xC1) return TL_COPY;            // shift imm8
    if (op == 0xC2 || op == 0xC3) return TL_RET;
    if (op == 0xC6 || op == 0xC7) return TL_COPY; // mov r/m, imm
    if (op == 0xC9) return TL_COPY;               // leave
    if (op >= 0xD0 && op <= 0xD3) return TL_COPY; // shift 1/CL
    if (op == 0xE8) return TL_CALL;
    if (op == 0xE9 || op == 0xEB) return TL_JMP;
    if (op == 0xF5 || op == 0xF8 || op == 0xF9) return TL_COPY;            // cmc/clc/stc
    if (op == 0xFC || op == 0xFD) return TL_COPY;                          // cld/std -- DF is live in the host flags
    if (op == 0xF6 || op == 0xF7) return insn->reg <= 5 ? TL_COPY : TL_NO; // not div/idiv (#DE is an exit)
    if (op == 0xFE) return insn->reg <= 1 ? TL_COPY : TL_NO;               // inc/dec r/m8
    if (op == 0xFF) {
        if (insn->reg == 0 || insn->reg == 1 || insn->reg == 6) return TL_COPY; // inc/dec/push r/m
        if (insn->mod == 3 && insn->reg == 2) return TL_CALL_REG;
        if (insn->mod == 3 && insn->reg == 4) return TL_JMP_REG;
    }
    return TL_NO;
}

// A displaced ET_EXEC has distinct guest/link and host/storage coordinates. Stage one admits only
// instructions whose copied bytes either touch no memory or carry a RIP-relative address that can be
// projected below. Everything with an implicit memory operand, a guest-stack effect, or a register-derived
// effective address stays in the interpreter until it has dedicated lowering.
static int translit_displaced_safe_for(const struct insn *insn, int kind, int displaced) {
    if (!displaced) return 1;
    if (kind == TL_CALL || kind == TL_CALL_REG || kind == TL_RET) return 0;
    if (kind != TL_COPY) return 1; // direct control, register jmp, and syscall touch no guest memory here
    if (insn->is_mem) return insn->rip_rel;
    if (insn->two) return 1;
    uint8_t op = insn->op;
    if ((op >= 0x50 && op <= 0x5F) || op == 0x68 || op == 0x6A || op == 0x8F || op == 0xC9)
        return 0; // push/pop/leave: implicit guest stack
    if (op >= 0xA0 && op <= 0xA7) return 0; // moffs, movs, cmps
    if (op >= 0xAA && op <= 0xAF) return 0; // stos, lods, scas
    if (op == 0xFF && insn->reg == 6) return 0; // push r/m
    return 1;
}

static int translit_displaced_safe(const struct insn *insn, int kind) {
    return translit_displaced_safe_for(insn, kind, g_nonpie_lo != 0);
}

// ---- the block
//
// One header per translated guest PC, bump-allocated from the shared arena exactly as interp.c's
// descriptor is. host_entry_off == 0 means "interpret"; the two kinds coexist in one cache, which is what
// makes the transliterator additive.
struct translit_builder {
    hl_x64_asm asm_state;
    uint8_t *rw_base;   // RW address the emitted code starts at
    ptrdiff_t rx_delta; // add to an RW address for the address the code will execute at
};

static int64_t translit_rx(const struct translit_builder *b, const uint8_t *rw) {
    return (int64_t)(intptr_t)rw + (int64_t)b->rx_delta;
}

// The spill half of every exit. Sixteen stores plus the stack switch, all flag-neutral, so the guest's
// flags survive untouched into the pushfq that follows -- the reason nothing has to materialise them.
static void translit_emit_spill(hl_x64_asm *a) {
    static const int order[15] = {0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15};
    for (int i = 0; i < 15; i++)
        hl_x64_store_gs(a, order[i], (int32_t)(order[i] * 8));
    hl_x64_store_gs(a, HL_X64_RSP, 4 * 8);
    hl_x64_load_gs(a, HL_X64_RSP, OFF_HSP); // back onto the host stack, below the six saved registers
    hl_x64_pushfq(a);
    hl_x64_pop(a, HL_X64_RAX); // rax is already spilled
    hl_x64_store_gs(a, HL_X64_RAX, TL_MM_FLAGS);
    hl_x64_cld(a); // a guest `std` must not leak DF into host C code
}

static void translit_emit_return(hl_x64_asm *a, uint64_t reason) {
    hl_x64_store_gs_imm32(a, (int32_t)reason, OFF_RSN);
    hl_x64_pop(a, 15);
    hl_x64_pop(a, 14);
    hl_x64_pop(a, 13);
    hl_x64_pop(a, 12);
    hl_x64_pop(a, 5); // rbp
    hl_x64_pop(a, 3); // rbx
    hl_x64_ret(a);
}

// Exit with a guest PC known at translate time.
static void translit_emit_exit_const(hl_x64_asm *a, uint64_t rip, uint64_t reason) {
    translit_emit_spill(a);
    hl_x64_mov_imm64(a, HL_X64_RAX, rip);
    hl_x64_store_gs(a, HL_X64_RAX, OFF_RIP);
    translit_emit_return(a, reason);
}

// Exit with cpu->rip already written by the terminator sequence (RET, and the indirect forms).
static void translit_emit_exit_preset(hl_x64_asm *a, uint64_t reason) {
    translit_emit_spill(a);
    translit_emit_return(a, reason);
}

// ---- the one operand whose meaning depends on where the bytes live
//
// A rip-relative operand names `next_guest_rip + disp32`; after the move the same bytes name
// `next_host_rip + disp32`. Re-aim the displacement at the guest address. The guest EA is left alone
// otherwise -- it is an ACCESS, and accesses are not un-biased (and a biased image never reaches here).
// Out of int32 reach ends the block; that is a real limit, not a rarity to be assumed away, because
// nothing places the code arena near the guest image.
static uint64_t translit_riprel_target(uint8_t op, int two, uint64_t guest_target, uint64_t nonpie_lo,
                                       uint64_t nonpie_hi, uint64_t nonpie_bias) {
    if (op == 0x8D && !two) return guest_target;
    return nonpie_lo && guest_target >= nonpie_lo && guest_target < nonpie_hi ? guest_target + nonpie_bias
                                                                                 : guest_target;
}

static int translit_rel32(uint64_t target, int64_t host_next, uint32_t *encoded) {
    int64_t delta = (int64_t)target - host_next;
    if (delta < INT32_MIN || delta > INT32_MAX) return 0;
    *encoded = (uint32_t)(int32_t)delta;
    return 1;
}

static int translit_fix_riprel(struct translit_builder *b, const struct insn *insn, uint64_t guest_next,
                               uint8_t *copy_start) {
    int disp_offset = insn->len - insn->imm_bytes - 4;
    if (disp_offset < 2 || disp_offset + 4 > insn->len) return 0;
    uint64_t guest_target = guest_next + (uint64_t)insn->disp;
    // LEA materialises a guest-visible pointer and must remain LOW. Every other admitted RIP-relative
    // memory form dereferences the target, so a displaced image must name its HIGH storage projection.
    uint64_t projected_target = translit_riprel_target(insn->op, insn->two, guest_target, g_nonpie_lo,
                                                       g_nonpie_hi, g_nonpie_bias);
    int64_t host_next = translit_rx(b, copy_start + insn->len);
    uint32_t value;
    if (!translit_rel32(projected_target, host_next, &value)) return 0;
    if (g_nonpie_lo) {
        uint64_t *counter = insn->op == 0x8D && !insn->two ? &translit_displaced_lea_low
                                                            : &translit_displaced_riprel_projected;
        (void)__atomic_fetch_add(counter, 1, __ATOMIC_RELAXED);
    }
    for (int i = 0; i < 4; i++)
        copy_start[disp_offset + i] = (uint8_t)(value >> (8 * i));
    return 1;
}

#if defined(HL_NATIVE_TEST_HOOKS)
HL_API int hl_x86_64_translit_displaced_test(uint32_t scenario) {
    const uint64_t lo = UINT64_C(0x400000), hi = UINT64_C(0x500000), bias = UINT64_C(0x100000000);
    if (scenario == 1) return translit_riprel_target(0x8D, 0, lo + 0x123, lo, hi, bias) == lo + 0x123 ? 0 : 1;
    if (scenario == 2)
        return translit_riprel_target(0x8B, 0, lo + 0x123, lo, hi, bias) == lo + bias + 0x123 ? 0 : 2;
    if (scenario == 3) {
        uint32_t encoded = 0;
        if (!translit_rel32(lo + bias + 0x123, (int64_t)(lo + bias), &encoded)) return 3;
        if ((int32_t)encoded != 0x123) return 4;
        return translit_rel32(UINT64_C(0x8000000000), 0, &encoded) ? 5 : 0;
    }
    struct insn insn = {0};
    if (scenario == 4) {
        insn.is_mem = 1;
        return translit_displaced_safe_for(&insn, TL_COPY, 1) ? 7 : 0;
    }
    if (scenario == 5) {
        insn.is_mem = insn.rip_rel = 1;
        return translit_displaced_safe_for(&insn, TL_COPY, 1) ? 0 : 8;
    }
    if (scenario == 6) {
        insn.op = 0x50;
        return translit_displaced_safe_for(&insn, TL_COPY, 1) ? 9 : 0;
    }
    if (scenario == 7) {
        insn.op = 0xA4;
        return translit_displaced_safe_for(&insn, TL_COPY, 1) ? 10 : 0;
    }
    if (scenario == 8) return translit_displaced_safe_for(&insn, TL_CALL, 1) ? 11 : 0;
    if (scenario == 9) return translit_displaced_safe_for(&insn, TL_COPY, 1) ? 0 : 12;
    return 6;
}
#endif

// Both jcc encodings (7x rel8 and 0F 8x rel32) carry the condition in the opcode's low nibble.
static int translit_condition(const struct insn *insn) {
    return insn->op & 0xF;
}

static void *translit_build(struct interp_block *block, uint64_t gpc) {
    if (!translit_image_ok()) {
        if (translit_enabled()) (void)__atomic_fetch_add(&translit_declined, 1, __ATOMIC_RELAXED);
        return NULL;
    }
    uint8_t *arena_limit = (uint8_t *)g_cache + CACHE_SZ - CACHE_EMIT_HEADROOM;
    uint8_t *start = g_cp;
    if (start + TL_MAX_BYTES > arena_limit) return NULL;
    struct translit_builder builder = {{start, start + TL_MAX_BYTES, 0}, start, g_rw2rx};
    hl_x64_asm *a = &builder.asm_state;

    uint64_t pc = gpc;
    int count = 0, terminated = 0;
    uint8_t bytes[16];

    while (count < TL_MAX_INSNS && !a->overflow) {
        struct insn insn;
        if (hl_x86_decode(pc, &insn) <= 0) break;
        int kind = translit_classify(&insn);
        if (kind == TL_NO) break;
        if (!translit_displaced_safe(&insn, kind)) {
            (void)__atomic_fetch_add(&translit_declined, 1, __ATOMIC_RELAXED);
            (void)__atomic_fetch_add(&translit_displaced_operand_declined, 1, __ATOMIC_RELAXED);
            break;
        }
        if (x86_guest_fetch_exec(pc, bytes, (size_t)insn.len) != 0) break;

        uint64_t next = pc + (uint64_t)insn.len;
        uint8_t *instruction_start = a->cursor;

        if (kind == TL_COPY) {
            hl_x64_copy(a, bytes, insn.len);
            if (a->overflow) break;
            if (insn.rip_rel && !translit_fix_riprel(&builder, &insn, next, instruction_start)) {
                if (g_nonpie_lo)
                    (void)__atomic_fetch_add(&translit_displaced_riprel_unreachable, 1, __ATOMIC_RELAXED);
                a->cursor = instruction_start; // un-emit: the block ends before this instruction
                break;
            }
            jit_instruction_map_put((uint64_t)(uintptr_t)instruction_start, (uint64_t)(uintptr_t)a->cursor, pc);
            pc = next;
            count++;
            continue;
        }

        // Terminators. Each writes cpu->rip (directly or through the exit) and returns to the dispatcher;
        // no inter-block edge is emitted, so every exit is a dispatcher safepoint and the shared loop's
        // signal/checkpoint/stop-the-world polling keeps working unchanged.
        switch (kind) {
        case TL_SYSCALL:
            // The frontend convention is that rip is already past the 0F 05 when the block exits.
            translit_emit_exit_const(a, next, R_SYSCALL);
            break;
        case TL_JMP: translit_emit_exit_const(a, next + (uint64_t)insn.imm, R_BRANCH); break;
        case TL_JCC: {
            uint8_t *taken = hl_x64_jcc_rel32(a, translit_condition(&insn));
            translit_emit_exit_const(a, next, R_BRANCH);
            hl_x64_patch_rel32(a, taken, a->cursor);
            translit_emit_exit_const(a, next + (uint64_t)insn.imm, R_BRANCH);
            break;
        }
        case TL_CALL:
        case TL_CALL_REG: {
            // The pushed return address is guest-visible (unwinders and dladdr read it), so it is exactly
            // interp_call_return_pc's value -- which is `next` here, since a biased image never gets this
            // far. Written as two 32-bit immediate stores because that clobbers no register: if the store
            // faults (a guest stack overflow), every host GPR still holds its guest value and the fault
            // path can restart the CALL exactly.
            uint64_t pushed = interp_call_return_pc(next);
            // Read the target from the LIVE host register, not from cpu->r[] -- nothing is spilled yet.
            if (kind == TL_CALL_REG) hl_x64_store_gs(a, insn.rm_reg, OFF_RIP);
            hl_x64_store_rsp_imm32(a, -8, (uint32_t)pushed);
            hl_x64_store_rsp_imm32(a, -4, (uint32_t)(pushed >> 32));
            hl_x64_lea_rsp(a, -8);
            if (kind == TL_CALL)
                translit_emit_exit_const(a, next + (uint64_t)insn.imm, R_BRANCH);
            else
                translit_emit_exit_preset(a, R_BRANCH);
            break;
        }
        case TL_JMP_REG:
            hl_x64_store_gs(a, insn.rm_reg, OFF_RIP);
            translit_emit_exit_preset(a, R_BRANCH);
            break;
        case TL_RET: {
            // Save rax through %gs (never through the stack: the red zone is the guest's), read the return
            // address while every guest register is still live, then pop. A fault on the load leaves the
            // guest register file untouched, so the RET restarts.
            hl_x64_store_gs(a, HL_X64_RAX, TL_MM_ENTRY);
            hl_x64_load_rsp_ind(a, HL_X64_RAX);
            hl_x64_store_gs(a, HL_X64_RAX, OFF_RIP);
            hl_x64_load_gs(a, HL_X64_RAX, TL_MM_ENTRY);
            hl_x64_lea_rsp(a, insn.op == 0xC2 ? (int32_t)(8 + (uint16_t)insn.imm) : 8);
            translit_emit_exit_preset(a, R_BRANCH);
            break;
        }
        default: break;
        }
        if (a->overflow) {
            a->cursor = instruction_start;
            break;
        }
        jit_instruction_map_put((uint64_t)(uintptr_t)instruction_start, (uint64_t)(uintptr_t)a->cursor, pc);
        pc = next;
        count++;
        terminated = 1;
        break;
    }

    if (count == 0 || a->overflow) return NULL;
    if (!terminated) {
        translit_emit_exit_const(a, pc, R_BRANCH);
        if (a->overflow) return NULL;
    }

    block->host_entry_off = (uint32_t)(start - (uint8_t *)block);
    block->host_len = (uint32_t)(a->cursor - start);
    block->guest_end = pc;
    g_cp = a->cursor;
    (void)__atomic_fetch_add(&translit_blocks, 1, __ATOMIC_RELAXED);
    if (g_nonpie_lo) (void)__atomic_fetch_add(&translit_displaced_blocks, 1, __ATOMIC_RELAXED);
    return start;
}

// Enter a transliterated block. The sigsetjmp pad run_block already arms covers this call: a guest fault
// inside emitted code is captured by translit_signal_capture and leaves by the same siglongjmp the
// interpreter uses, which restores the host callee-saved registers the block is currently holding guest
// values in.
static void translit_run(struct cpu *cpu, struct interp_block *block) {
    (void)__atomic_fetch_add(&translit_entries, 1, __ATOMIC_RELAXED);
    if (g_nonpie_lo) (void)__atomic_fetch_add(&translit_displaced_entries, 1, __ATOMIC_RELAXED);
    cpu->mmscratch[0] = translit_flags_in(cpu);
    hl_x86_translit_enter(cpu, (uint8_t *)block + block->host_entry_off);
    translit_flags_out(cpu, cpu->mmscratch[0]);
}

// One line of the [prof] report (engine/target/x86_64.c), and the only place an operator can learn that
// this backend was selected and then silently refused. Written here rather than there because the
// refusal predicates are this file's; the caller needs no #ifdef either way.
static int translit_report(char *out, size_t size) {
    uint64_t blocks = __atomic_load_n(&translit_blocks, __ATOMIC_RELAXED);
    uint64_t entries = __atomic_load_n(&translit_entries, __ATOMIC_RELAXED);
    uint64_t declined = __atomic_load_n(&translit_declined, __ATOMIC_RELAXED);
    uint64_t displaced_blocks = __atomic_load_n(&translit_displaced_blocks, __ATOMIC_RELAXED);
    uint64_t displaced_entries = __atomic_load_n(&translit_displaced_entries, __ATOMIC_RELAXED);
    uint64_t displaced_operand_declined =
        __atomic_load_n(&translit_displaced_operand_declined, __ATOMIC_RELAXED);
    uint64_t displaced_riprel_projected =
        __atomic_load_n(&translit_displaced_riprel_projected, __ATOMIC_RELAXED);
    uint64_t displaced_lea_low = __atomic_load_n(&translit_displaced_lea_low, __ATOMIC_RELAXED);
    uint64_t displaced_riprel_unreachable =
        __atomic_load_n(&translit_displaced_riprel_unreachable, __ATOMIC_RELAXED);
    if (!translit_enabled()) return snprintf(out, size, "[prof] translit: not selected\n");
    if (g_nonpie_lo != 0)
        return snprintf(out, size,
                        "[prof] translit: displaced blocks=%llu entries=%llu declined=%llu displaced_blocks=%llu"
                        " displaced_entries=%llu operand_declined=%llu riprel_projected=%llu lea_low=%llu"
                        " riprel_unreachable=%llu\n",
                        (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined,
                        (unsigned long long)displaced_blocks, (unsigned long long)displaced_entries,
                        (unsigned long long)displaced_operand_declined,
                        (unsigned long long)displaced_riprel_projected, (unsigned long long)displaced_lea_low,
                        (unsigned long long)displaced_riprel_unreachable);
    if (jit86_store_alias_observation_active())
        return snprintf(out, size,
                        "[prof] translit: declined, guest executable mapping or shared alias observed"
                        " (blocks=%llu entries=%llu declined=%llu)\n",
                        (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined);
    return snprintf(out, size, "[prof] translit: blocks=%llu entries=%llu declined=%llu\n",
                    (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined);
}

// cpu->r[] index -> ucontext greg index, for reconstructing guest state from a fault inside emitted code.
static const int g_translit_greg[16] = {HL_HOST_UC_REG_RAX, HL_HOST_UC_REG_RCX, HL_HOST_UC_REG_RDX, HL_HOST_UC_REG_RBX,
                                        HL_HOST_UC_REG_RSP, HL_HOST_UC_REG_RBP, HL_HOST_UC_REG_RSI, HL_HOST_UC_REG_RDI,
                                        HL_HOST_UC_REG_R8,  HL_HOST_UC_REG_R9,  HL_HOST_UC_REG_R10, HL_HOST_UC_REG_R11,
                                        HL_HOST_UC_REG_R12, HL_HOST_UC_REG_R13, HL_HOST_UC_REG_R14, HL_HOST_UC_REG_R15};

// A fault whose host PC is inside the code cache came from a guest access in a transliterated block: guest
// GPRs are the host GPRs, so *cpu is reconstructed exactly, and the JIT's own per-instruction provenance
// map (cache.c) recovers the faulting guest RIP. Block-granular cpu->rip is the fallback when the bounded
// map has wrapped -- the same fallback the AArch64 JIT takes.
static int translit_signal_capture(struct cpu *cpu, void *native_context) {
    if (native_context == NULL || !translit_enabled()) return 0;
    ucontext_t *uc = (ucontext_t *)native_context;
    uint64_t host_pc = (uint64_t)HL_HOST_UC_PC(uc);
    if (!jit_pc_in_retained_cache(host_pc)) return 0;
    greg_t *gregs = HL_HOST_UC_GREGS(uc);
    for (int i = 0; i < 16; i++)
        cpu->r[i] = (uint64_t)gregs[g_translit_greg[i]];
    translit_flags_out(cpu, (uint64_t)gregs[HL_HOST_UC_REG_EFL]);
    uint64_t guest_pc;
    if (jit_instruction_guest_pc(host_pc, &guest_pc)) cpu->rip = guest_pc;
    cpu->reason = R_BRANCH;
    return 1;
}

#else // not a Linux x86-64 host: no diagonal exists, so the transliterator is inert.

static int translit_image_ok(void) {
    return 0;
}

static int translit_bind_cpu(struct cpu *cpu) {
    (void)cpu;
    return 0;
}

static void *translit_build(struct interp_block *block, uint64_t gpc) {
    (void)block;
    (void)gpc;
    return NULL;
}

static void translit_run(struct cpu *cpu, struct interp_block *block) {
    (void)cpu;
    (void)block;
}

static int translit_signal_capture(struct cpu *cpu, void *native_context) {
    (void)cpu;
    (void)native_context;
    return 0;
}

static int translit_report(char *out, size_t size) {
    return snprintf(out, size, "[prof] translit: absent, not a Linux x86-64 host\n");
}

#endif
