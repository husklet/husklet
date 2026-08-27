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
#ifndef TL_STITCH_MAX_JMPS
#define TL_STITCH_MAX_JMPS 8
#endif

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
static uint64_t translit_stitch_candidates;
static uint64_t translit_stitch_admitted;
static uint64_t translit_jcc_fall_candidates;
static uint64_t translit_jcc_fall_admitted;
static uint64_t translit_jcc_fall_page_refused;
static uint64_t translit_jcc_fall_successor_page_refused;
static uint64_t translit_jcc_fall_decode_refused;
#if defined(HL_NATIVE_TEST_HOOKS)
static uint64_t translit_jcc_fall_executed;
#endif
static uint64_t translit_displaced_blocks;
static uint64_t translit_rip_indirect_lowered;
static uint64_t translit_provenance_fallback;
static uint64_t translit_body_owner_recovered;
#if defined(HL_NATIVE_TEST_HOOKS)
static uint64_t translit_body_owner_published;
static uint64_t translit_sse2_runs_admitted;
static uint64_t translit_sse2_instructions_admitted;
static uint64_t translit_sse2_target_runs;
#endif
static uint64_t translit_displaced_entries;
static uint64_t translit_displaced_operand_declined;
static uint64_t translit_displaced_sse2_memory_declined;
static uint64_t translit_displaced_riprel_projected;
static uint64_t translit_displaced_lea_low;
static uint64_t translit_displaced_riprel_unreachable;
static uint64_t translit_displaced_riprel_lowered;
static uint64_t translit_displaced_scratch_lowered;
static uint64_t translit_displaced_lea_lowered;
static uint64_t translit_natural_abs32_lowered;
static uint64_t translit_natural_lea_lowered;
static uint64_t translit_displaced_refusal_opcode[512];
static uint64_t translit_displaced_refusal_prefix;
static uint64_t translit_displaced_refusal_width;
static uint64_t translit_displaced_refusal_rsp;
static uint64_t translit_displaced_refusal_opcode_other;
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

static void translit_count_displaced_refusal(const struct insn *insn) {
    unsigned key = (insn->two ? 256u : 0u) | insn->op;
    (void)__atomic_fetch_add(&translit_displaced_refusal_opcode[key], 1, __ATOMIC_RELAXED);
}

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
    TL_SSE2,     // mandatory-66 SSE2 run with deferred guest retirement
    TL_JCC,      // conditional branch: host jcc to a second epilogue
    TL_JMP,      // direct jmp rel
    TL_CALL,     // direct call rel32
    TL_RET,      // ret / ret imm16
    TL_JMP_REG,  // jmp *%reg
    TL_CALL_REG, // call *%reg
    TL_JMP_RIP,  // jmp *disp32(%rip)
    TL_CALL_RIP, // call *disp32(%rip)
    TL_SYSCALL
};

static int translit_classify_two(const struct insn *insn) {
    uint8_t op = insn->op;
    if (op == 0x05) return TL_SYSCALL;
    if (op >= 0x80 && op <= 0x8F) return TL_JCC;
    if (insn->p66 && !insn->rep && !insn->repne && !insn->lock && !insn->addr32 && !insn->seg &&
        !insn->vex && !insn->evex && !insn->map3 && insn->has_modrm) {
        if (op == 0x6F || ((op == 0xDB || op == 0xEB) && insn->mod == 3)) return TL_SSE2;
        // First landing keeps 7E to its register destination: a guest-memory store has separate SMC and
        // store-observer retirement requirements and is not silently treated as an ordinary host store.
        if (op == 0x7E && insn->mod == 3) return TL_SSE2;
        if (op == 0x73 && insn->mod == 3 && (insn->reg & 7) == 3 && insn->imm_bytes == 1) return TL_SSE2;
    }
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
        if (insn->rip_rel && insn->reg == 2) return TL_CALL_RIP;
        if (insn->rip_rel && insn->reg == 4) return TL_JMP_RIP;
    }
    return TL_NO;
}

// A displaced ET_EXEC has distinct guest/link and host/storage coordinates. Stage one admits only
// instructions whose copied bytes either touch no memory or carry a RIP-relative address that can be
// projected below. Everything with an implicit memory operand, a guest-stack effect, or a register-derived
// effective address stays in the interpreter until it has dedicated lowering.
static int translit_displaced_safe_for(const struct insn *insn, int kind, int displaced) {
    if (!displaced) return 1;
    // A displaced image keeps guest pointers low in GPRs while its bytes live at the high storage bias.
    // Only RIP-relative SSE memory can be projected; register-derived memory must stay interpreted.
    if (kind == TL_SSE2 && insn->is_mem) {
        if (!insn->rip_rel)
            (void)__atomic_fetch_add(&translit_displaced_sse2_memory_declined, 1, __ATOMIC_RELAXED);
        return insn->rip_rel;
    }
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
// Rel32 is cheapest when it reaches.  The code arena is normally far from a low ET_EXEC, so supported
// forms fall through to the absolute lowerer rather than treating placement as an execution-policy gate.
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

static uint32_t translit_preserve_mask(int reg) {
    return reg < 0 ? 0 : 1u << reg;
}

// Generated code owns no persistent XMM state.  Each selected instruction reloads its operands from
// cpu->v and commits its destination back with MOVDQU.  The persistent owner ranges map every byte up to
// and including that commit to the SOURCE guest instruction; an asynchronous interruption therefore
// restarts from the still-canonical old cpu->v.  Bytes after the commit map to NEXT.  This is a deferred
// retirement transaction, not a vector sidecar, and aliasing is safe because all sources are loaded first.
static void translit_xmm_cpu(hl_x64_asm *a, int reg, int load) {
    hl_x64_u8(a, 0x65); // %gs: cpu
    hl_x64_u8(a, 0xF3); // MOVDQU: cpu->v itself is not promised 16-byte alignment
    if (reg >= 8) hl_x64_u8(a, 0x44); // REX.R
    hl_x64_u8(a, 0x0F);
    hl_x64_u8(a, load ? 0x6F : 0x7F);
    hl_x64_u8(a, (uint8_t)(((reg & 7) << 3) | 4));
    hl_x64_u8(a, 0x25); // no base/index, disp32
    hl_x64_u32(a, (uint32_t)(OFF_V + 16 * reg));
}

// Rel32 is deliberately attempted first: a one-instruction copy is the cheapest exact spelling. When the
// host-only arena is out of reach, x86's accumulator moffs forms provide a bounded absolute dereference
// without borrowing a guest register, touching the stack, or adding fault-recovery state. LEA is address
// materialisation rather than a dereference, so an immediate move preserves its low guest identity.
static int translit_lower_absolute_riprel(hl_x64_asm *a, const struct insn *insn, uint64_t guest_target,
                                          uint64_t storage_target, int displaced, int *preserve_register) {
    *preserve_register = -1;
    if (insn->op == 0x8D && !insn->two) {
        if (insn->lock || insn->rep || insn->repne ||
            (insn->opsize != 2 && insn->opsize != 4 && insn->opsize != 8))
            return 0;
        if (!displaced && (insn->opsize != 8 || insn->addr32)) return 0;
        int reg = insn->reg;
        if (insn->opsize == 8) {
            if (!displaced && guest_target <= UINT32_MAX) {
                if (reg >= 8) hl_x64_u8(a, 0x41);
                hl_x64_u8(a, (uint8_t)(0xB8 + (reg & 7)));
                hl_x64_u32(a, (uint32_t)guest_target);
            } else {
                hl_x64_mov_imm64(a, reg, guest_target);
            }
        } else {
            if (insn->opsize == 2) hl_x64_u8(a, 0x66);
            if (reg >= 8) hl_x64_u8(a, 0x41);
            hl_x64_u8(a, (uint8_t)(0xB8 + (reg & 7)));
            if (insn->opsize == 2) {
                hl_x64_u8(a, (uint8_t)guest_target);
                hl_x64_u8(a, (uint8_t)(guest_target >> 8));
            } else {
                hl_x64_u32(a, (uint32_t)guest_target);
            }
        }
        return !a->overflow;
    }
    if (insn->lock || insn->rep || insn->repne) {
        if (g_nonpie_lo) (void)__atomic_fetch_add(&translit_displaced_refusal_prefix, 1, __ATOMIC_RELAXED);
        return 0;
    }
    // In 64-bit mode an address-size override plus a no-base/no-index SIB names an unsigned absolute disp32.
    // Natural ET_EXEC storage is low, so this is shorter than movabs+load and borrows no guest GPR.
    if (!displaced && !insn->two && insn->op == 0x8B && storage_target <= UINT32_MAX &&
        (insn->opsize == 4 || insn->opsize == 8) && !insn->p66 && !insn->addr32) {
        hl_x64_u8(a, 0x67);
        uint8_t rex = (uint8_t)(0x40 | (insn->opsize == 8 ? 8 : 0) | (insn->reg >= 8 ? 4 : 0));
        if (rex != 0x40) hl_x64_u8(a, rex);
        hl_x64_u8(a, 0x8B);
        hl_x64_u8(a, (uint8_t)(((insn->reg & 7) << 3) | 4));
        hl_x64_u8(a, 0x25);
        hl_x64_u32(a, (uint32_t)storage_target);
        (void)__atomic_fetch_add(&translit_natural_abs32_lowered, 1, __ATOMIC_RELAXED);
        return !a->overflow;
    }
    // Natural mode never falls into the displaced image's borrowed-register or moffs families.
    if (!displaced) return 0;
    uint8_t moffs = 0;
    if (!insn->two && insn->reg == HL_X64_RAX && insn->op == 0x8A)
        moffs = 0xA0;
    else if (!insn->two && insn->reg == HL_X64_RAX && insn->op == 0x8B)
        moffs = 0xA1;
    else if (!insn->two && insn->reg == HL_X64_RAX && insn->op == 0x88)
        moffs = 0xA2;
    else if (!insn->two && insn->reg == HL_X64_RAX && insn->op == 0x89)
        moffs = 0xA3;
    if (moffs == 0) {
        int family = (!insn->two && (insn->op == 0x8B || insn->op == 0x63)) ||
                     (insn->two &&
                      (insn->op == 0xB6 || insn->op == 0xB7 || insn->op == 0xBE || insn->op == 0xBF));
        int supported = family && (insn->opsize == 4 || insn->opsize == 8) &&
                        (insn->two || insn->op != 0x63 || insn->opsize == 8);
        int reg = insn->reg;
        if (!supported) {
            if (g_nonpie_lo)
                (void)__atomic_fetch_add(family ? &translit_displaced_refusal_width
                                                : &translit_displaced_refusal_opcode_other,
                                         1, __ATOMIC_RELAXED);
            return 0;
        }
        if (reg == HL_X64_RSP) {
            if (g_nonpie_lo) (void)__atomic_fetch_add(&translit_displaced_refusal_rsp, 1, __ATOMIC_RELAXED);
            return 0;
        }
        hl_x64_mov_imm64(a, reg, storage_target);
        uint8_t rex = (uint8_t)(0x40 | (insn->opsize == 8 ? 8 : 0) | (reg >= 8 ? 5 : 0));
        if (rex != 0x40) hl_x64_u8(a, rex);
        if (insn->two) hl_x64_u8(a, 0x0F);
        hl_x64_u8(a, insn->op);
        int low = reg & 7;
        uint8_t mod = low == 5 ? 0x40 : 0;
        hl_x64_u8(a, (uint8_t)(mod | ((reg & 7) << 3) | (low == 4 ? 4 : low)));
        if (low == 4) hl_x64_u8(a, 0x24);
        if (low == 5) hl_x64_u8(a, 0);
        if (a->overflow) return 0;
        *preserve_register = reg;
        return 1;
    }
    if ((moffs == 0xA1 || moffs == 0xA3) && insn->opsize == 2) hl_x64_u8(a, 0x66);
    if ((moffs == 0xA1 || moffs == 0xA3) && insn->opsize == 8) hl_x64_u8(a, 0x48);
    hl_x64_u8(a, moffs);
    hl_x64_u64(a, storage_target);
    return !a->overflow;
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

enum { TL_SUCCESSOR_OK, TL_SUCCESSOR_DECODE_REFUSED, TL_SUCCESSOR_PAGE_REFUSED };
#if defined(HL_NATIVE_TEST_HOOKS)
static uint64_t translit_test_decode_refuse_pc;

static void *translit_body_owner_publish_worker(void *opaque) {
    uint64_t generation = *(uint64_t *)opaque;
    return jit_body_owner_set_for(generation, 1);
}

typedef struct {
    uint64_t generation;
    uint32_t token;
    jit_body_owner_range range[3];
} translit_body_owner_batch;

static void *translit_body_owner_batch_worker(void *opaque) {
    translit_body_owner_batch *batch = opaque;
    return (void *)(uintptr_t)jit_body_owner_publish_n(batch->generation, batch->token, batch->range, 3);
}
#endif
static int translit_jcc_successor(hl_x86_hot_context *context, uint64_t source, uint64_t next, struct insn *successor);

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
    uint8_t bytes[32] = {0};
    hl_x64_asm output = {bytes, bytes + sizeof(bytes), 0};
    insn.opsize = 8;
    if (scenario == 10) {
        insn.op = 0x8D;
        insn.reg = 9;
        int preserve;
        if (!translit_lower_absolute_riprel(&output, &insn, lo + 0x123, lo + bias + 0x123, 1, &preserve)) return 13;
        const uint8_t expected[] = {0x49, 0xB9, 0x23, 0x01, 0x40, 0, 0, 0, 0, 0};
        return output.cursor - bytes == (ptrdiff_t)sizeof(expected) && memcmp(bytes, expected, sizeof(expected)) == 0
                   ? 0
                   : 14;
    }
    if (scenario == 11 || scenario == 12) {
        insn.op = scenario == 11 ? 0x8B : 0x89;
        insn.reg = 0;
        int preserve;
        if (!translit_lower_absolute_riprel(&output, &insn, lo + 0x123, lo + bias + 0x123, 1, &preserve)) return 15;
        return output.cursor - bytes == 10 && bytes[0] == 0x48 && bytes[1] == (scenario == 11 ? 0xA1 : 0xA3) &&
                       memcmp(bytes + 2, (uint8_t[]){0x23, 0x01, 0x40, 0, 0x01, 0, 0, 0}, 8) == 0
                   ? 0
                   : 16;
    }
    if (scenario == 13) {
        insn.op = 0x8B;
        insn.reg = 4;
        int preserve;
        return translit_lower_absolute_riprel(&output, &insn, lo + 0x123, lo + bias + 0x123, 1, &preserve) ? 17 : 0;
    }
    if (scenario >= 14 && scenario <= 16) {
        int preserve;
        insn.reg = scenario == 14 ? 1 : (scenario == 15 ? 13 : 2);
        insn.op = scenario == 16 ? 0xBE : 0x8B;
        insn.two = scenario == 16;
        insn.opsize = scenario == 15 ? 4 : 8;
        if (!translit_lower_absolute_riprel(&output, &insn, lo + 0x123, lo + bias + 0x123, 1, &preserve)) return 18;
        static const uint8_t suffix[][4] = {{0x48, 0x8B, 0x09, 0}, {0x45, 0x8B, 0x6D, 0},
                                             {0x48, 0x0F, 0xBE, 0x12}};
        size_t suffix_size = scenario == 14 ? 3 : 4;
        size_t length = 10 + suffix_size;
        if (preserve != insn.reg || output.cursor - bytes != (ptrdiff_t)length ||
            memcmp(bytes + 10, suffix[scenario - 14], suffix_size) != 0)
            return 19;
        return 0;
    }
    if (scenario == 17) return translit_preserve_mask(10) == (1u << 10) ? 0 : 20;
    if (scenario == 18 || scenario == 19) {
        int preserve;
        insn.reg = scenario == 18 ? 11 : 14;
        insn.op = scenario == 18 ? 0xB6 : 0x63;
        insn.two = scenario == 18;
        insn.opsize = scenario == 18 ? 4 : 8;
        if (!translit_lower_absolute_riprel(&output, &insn, lo + 0x123, lo + bias + 0x123, 1, &preserve)) return 21;
        const uint8_t *expected = scenario == 18 ? (uint8_t[]){0x45, 0x0F, 0xB6, 0x1B}
                                                 : (uint8_t[]){0x4D, 0x63, 0x36};
        size_t expected_size = scenario == 18 ? 4 : 3;
        if (preserve != insn.reg || output.cursor - bytes != (ptrdiff_t)(10 + expected_size) ||
            memcmp(bytes + 10, expected, expected_size) != 0)
            return 22;
        return 0;
    }
    if (scenario == 20) {
        uint8_t code[] = {0x90};
        uint64_t next = (uint64_t)(uintptr_t)code;
        struct insn successor;
        translit_test_decode_refuse_pc = next;
        int refused = translit_jcc_successor(NULL, next, next, &successor) == TL_SUCCESSOR_DECODE_REFUSED;
        translit_test_decode_refuse_pc = 0;
        return refused ? 0 : 23;
    }
    if (scenario == 21) {
        int preserve;
        insn.op = 0x8B;
        insn.reg = 10;
        int lowered = translit_lower_absolute_riprel(&output, &insn, lo + 0x123, lo + 0x123, 0, &preserve);
        const uint8_t expected[] = {0x67, 0x4C, 0x8B, 0x14, 0x25, 0x23, 0x01, 0x40, 0x00};
        return lowered && preserve == -1 && output.cursor - bytes == (ptrdiff_t)sizeof(expected) &&
                       memcmp(bytes, expected, sizeof(expected)) == 0
                   ? 0
                   : 24;
    }
    if (scenario == 22 || scenario == 23) {
        int preserve;
        insn.op = 0x8D;
        insn.reg = scenario == 22 ? 10 : 9;
        uint64_t target = scenario == 22 ? lo + 0x123 : UINT64_C(0x7f0012345678);
        int lowered = translit_lower_absolute_riprel(&output, &insn, target, target, 0, &preserve);
        const uint8_t *expected = scenario == 22 ? (uint8_t[]){0x41, 0xBA, 0x23, 0x01, 0x40, 0x00}
                                                 : (uint8_t[]){0x49, 0xB9, 0x78, 0x56, 0x34, 0x12,
                                                               0x00, 0x7F, 0x00, 0x00};
        size_t expected_size = scenario == 22 ? 6 : 10;
        return lowered && preserve == -1 && output.cursor - bytes == (ptrdiff_t)expected_size &&
                       memcmp(bytes, expected, expected_size) == 0
                   ? 0
                   : 25;
    }
    if (scenario == 24) {
        const uint64_t generation = UINT64_MAX - 24;
        uint64_t base = (uint64_t)(uintptr_t)g_cache + CACHE_SZ - 0x1000;
        uint32_t token;
        uint64_t owner = 0;
        jit_body_owner_drop_generation(generation);
        if (!jit_body_owner_reserve(generation, &token) || token != 0 ||
            !jit_body_owner_publish(generation, token, base, base + 0x20, UINT64_C(0x1111)) ||
            !jit_body_owner_reserve(generation, &token) || token != 1 ||
            !jit_body_owner_publish(generation, token, base + 0x20, base + 0x40, UINT64_C(0x2222)))
            return 26;
        int ok = jit_body_owner_lookup(base + g_rw2rx + 0x1f, &owner) && owner == UINT64_C(0x1111) &&
                 jit_body_owner_lookup(base + g_rw2rx + 0x20, &owner) && owner == UINT64_C(0x2222) &&
                 !jit_body_owner_lookup(base + g_rw2rx + 0x40, &owner);
        jit_body_owner_drop_generation(generation);
        return ok ? 0 : 27;
    }
    if (scenario == 25) {
        const uint64_t generation = UINT64_MAX - 25;
        uint32_t token;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 28;
        atomic_store_explicit(&set->count, JIT_BODY_OWNER_N, memory_order_relaxed);
        int refused = !jit_body_owner_reserve(generation, &token);
        jit_body_owner_drop_generation(generation);
        return refused ? 0 : 29;
    }
    if (scenario == 26) {
        const uint64_t generation = UINT64_MAX - 26;
        uint64_t base = (uint64_t)(uintptr_t)g_cache + CACHE_SZ - 0x800;
        uint32_t token;
        jit_body_owner_drop_generation(generation);
        int reserved = jit_body_owner_reserve(generation, &token);
        int rejected = reserved && !jit_body_owner_publish(generation, token + 1, base, base + 1, 3);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 0);
        int unchanged = set != NULL && atomic_load_explicit(&set->count, memory_order_relaxed) == 0;
        jit_body_owner_drop_generation(generation);
        return rejected && unchanged ? 0 : 30;
    }
    if (scenario == 27) {
        const uint64_t generation = UINT64_MAX - 27;
        const uint64_t rw = UINT64_C(0x100000000);
        const ptrdiff_t alias = (ptrdiff_t)(CACHE_SZ * 2u);
        uint32_t token;
        uint64_t owner = 0;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 31;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = alias;
        if (!jit_body_owner_reserve(generation, &token) ||
            !jit_body_owner_publish(generation, token, rw + 0x80, rw + 0x90, UINT64_C(0x3333)))
            return 32;
        int ok = jit_body_owner_lookup(rw + 0x88, &owner) && owner == UINT64_C(0x3333) &&
                 jit_body_owner_lookup(rw + alias + 0x88, &owner) && owner == UINT64_C(0x3333);
        jit_body_owner_drop_generation(generation);
        return ok ? 0 : 33;
    }
    if (scenario == 28) {
        const uint64_t generation = UINT64_MAX - 28;
        const uint64_t rw = UINT64_C(0x200000000);
        uint32_t token;
        uint64_t owner = 0;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 34;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = 0;
        if (!jit_body_owner_reserve(generation, &token) ||
            !jit_body_owner_publish(generation, token, rw + 1, rw + 2, UINT64_C(0x4444)) ||
            !jit_body_owner_lookup(rw + 1, &owner) || owner != UINT64_C(0x4444))
            return 35;
        // Arena retirement, unlike ordinary map invalidation, removes the immutable owner set.
        jit_body_owner_drop_generation(generation);
        return !jit_body_owner_lookup(rw + 1, &owner) ? 0 : 36;
    }
    if (scenario == 29 || scenario == 30) {
        uint64_t generation = UINT64_MAX - scenario;
        const uint64_t rw = UINT64_C(0x300000000);
        uint32_t token;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 37;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = 0;
        if (!jit_body_owner_reserve(generation, &token) ||
            !jit_body_owner_publish(generation, token, rw + 0x20, rw + 0x40, 1) ||
            !jit_body_owner_reserve(generation, &token))
            return 38;
        uint64_t lo = scenario == 29 ? rw + 0x30 : rw + 0x10;
        int rejected = !jit_body_owner_publish(generation, token, lo, lo + 0x10, 2);
        int unchanged = atomic_load_explicit(&set->count, memory_order_relaxed) == 1;
        jit_body_owner_drop_generation(generation);
        return rejected && unchanged ? 0 : 39;
    }
    if (scenario == 31 || scenario == 32) {
        uint64_t generation = UINT64_MAX - scenario;
        const uint64_t rw = UINT64_C(0x400000000);
        uint32_t token;
        uint64_t owner = 0;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 40;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = 0;
        if (!jit_body_owner_reserve(generation, &token) ||
            !jit_body_owner_publish(generation, token, rw + 1, rw + 2, 5))
            return 41;
        jit_body_owner_after_fork(scenario == 31);
        int found = jit_body_owner_lookup(rw + 1, &owner) && owner == 5;
        jit_body_owner_drop_generation(generation);
        return found == (scenario == 31) ? 0 : 42;
    }
    if (scenario == 33) {
        const uint64_t generation = UINT64_MAX - 33;
        const uint64_t rw = UINT64_C(0x500000000);
        uint32_t token;
        uint64_t owner = 0;
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 43;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = 0;
        if (!jit_body_owner_reserve(generation, &token) ||
            !jit_body_owner_publish(generation, token, rw + 1, rw + 2, 6))
            return 44;
        if (jit_body_owner_reclaim_if_unused(generation, 1, 0) ||
            !jit_body_owner_lookup(rw + 1, &owner) || owner != 6 ||
            jit_body_owner_reclaim_if_unused(generation, 0, 1) ||
            !jit_body_owner_lookup(rw + 1, &owner) || owner != 6)
            return 45;
        if (!jit_body_owner_reclaim_if_unused(generation, 0, 0)) return 46;
        return !jit_body_owner_lookup(rw + 1, &owner) ? 0 : 47;
    }
    if (scenario == 34) {
        uint64_t generation = UINT64_MAX - 34;
        pthread_t thread;
        jit_body_owner_drop_generation(generation);
        atomic_store_explicit(&g_body_owner_publish_slot, -1, memory_order_relaxed);
        atomic_store_explicit(&g_body_owner_publish_pause, 1, memory_order_release);
        if (pthread_create(&thread, NULL, translit_body_owner_publish_worker, &generation) != 0) {
            atomic_store_explicit(&g_body_owner_publish_pause, 0, memory_order_release);
            return 48;
        }
        unsigned spins = 0;
        while (atomic_load_explicit(&g_body_owner_publish_pause, memory_order_acquire) != 2 &&
               ++spins != 10000000u)
            sched_yield();
        int slot = atomic_load_explicit(&g_body_owner_publish_slot, memory_order_acquire);
        int hidden = spins != 10000000u && slot >= 0 && slot < STW_RETIRED_MAX + 1 &&
                     atomic_load_explicit(&g_body_owners[slot].entry, memory_order_acquire) == NULL;
        atomic_store_explicit(&g_body_owner_publish_pause, 0, memory_order_release);
        void *published = NULL;
        if (pthread_join(thread, &published) != 0) return 48;
        jit_body_owner_set *set = slot < 0 ? NULL : &g_body_owners[slot];
        int visible = published != NULL && set == published && set->generation == generation &&
                      set->rw == g_cache && set->rw2rx == g_rw2rx &&
                      atomic_load_explicit(&set->count, memory_order_acquire) == 0 &&
                      atomic_load_explicit(&set->entry, memory_order_acquire) != NULL;
        jit_body_owner_drop_generation(generation);
        int detached = set != NULL && atomic_load_explicit(&set->entry, memory_order_acquire) == NULL &&
                       set->generation == 0 && set->rw == NULL && set->rw2rx == 0;
        return hidden && visible && detached ? 0 : 48;
    }
    if (scenario == 35) {
        const uint64_t generation = UINT64_MAX - 35, rw = UINT64_C(0x600000000);
        uint32_t token;
        uint64_t owner = 0;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 49;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = CACHE_SZ * 2u;
        if (!jit_body_owner_reserve(generation, &token) ||
            !jit_body_owner_publish(generation, token, rw, rw + 8, 0))
            return 49;
        jit_body_owner_range range[] = {{rw + 0x10, rw + 0x18, 1}, {rw + 0x18, rw + 0x20, 2},
                                        {rw + 0x20, rw + 0x28, 3}};
        int ok = jit_body_owner_reserve_n(generation, 3, &token) && token == 1 &&
                 jit_body_owner_publish_n(generation, token, range, 3) &&
                 jit_body_owner_lookup(rw + 0x17, &owner) && owner == 1 &&
                 jit_body_owner_lookup(rw + CACHE_SZ * 2u + 0x18, &owner) && owner == 2 &&
                 jit_body_owner_lookup(rw + 0x27, &owner) && owner == 3 &&
                 !jit_body_owner_lookup(rw + 8, &owner) && !jit_body_owner_lookup(rw + 0x28, &owner);
        jit_body_owner_drop_generation(generation);
        return ok ? 0 : 49;
    }
    if (scenario == 36) {
        const uint64_t generation = UINT64_MAX - 36, rw = UINT64_C(0x610000000);
        uint32_t token;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 50;
        set->rw = (uint8_t *)(uintptr_t)rw;
        jit_body_owner_range unsorted[] = {{rw + 0x20, rw + 0x28, 1}, {rw + 0x10, rw + 0x18, 2}};
        jit_body_owner_range overlap[] = {{rw + 0x10, rw + 0x20, 1}, {rw + 0x18, rw + 0x28, 2}};
        int ok = jit_body_owner_reserve_n(generation, 2, &token) &&
                 !jit_body_owner_publish_n(generation, token, unsorted, 2) &&
                 atomic_load_explicit(&set->count, memory_order_acquire) == 0 &&
                 !jit_body_owner_publish_n(generation, token, overlap, 2) &&
                 atomic_load_explicit(&set->count, memory_order_acquire) == 0;
        atomic_store_explicit(&set->count, JIT_BODY_OWNER_N - 1, memory_order_relaxed);
        ok = ok && !jit_body_owner_reserve_n(generation, 2, &token) &&
             jit_body_owner_reserve_n(generation, 1, &token) && token == JIT_BODY_OWNER_N - 1;
        jit_body_owner_drop_generation(generation);
        return ok ? 0 : 50;
    }
    if (scenario == 37) {
        const uint64_t generation = UINT64_MAX - 37, rw = UINT64_C(0x620000000);
        pthread_t thread;
        uint64_t owner = 0;
        uint32_t token;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 51;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = 0;
        if (!jit_body_owner_reserve_n(generation, 3, &token)) return 51;
        translit_body_owner_batch batch = {
            generation, token, {{rw + 0x10, rw + 0x18, 1}, {rw + 0x18, rw + 0x20, 2},
                                {rw + 0x20, rw + 0x28, 3}}};
        atomic_store_explicit(&g_body_owner_batch_pause, 1, memory_order_release);
        if (pthread_create(&thread, NULL, translit_body_owner_batch_worker, &batch) != 0) {
            atomic_store_explicit(&g_body_owner_batch_pause, 0, memory_order_release);
            return 51;
        }
        unsigned spins = 0;
        while (atomic_load_explicit(&g_body_owner_batch_pause, memory_order_acquire) != 2 &&
               ++spins != 10000000u)
            sched_yield();
        int hidden = spins != 10000000u &&
                     atomic_load_explicit(&set->count, memory_order_acquire) == token &&
                     !jit_body_owner_lookup(rw + 0x10, &owner) &&
                     !jit_body_owner_lookup(rw + 0x27, &owner);
        atomic_store_explicit(&g_body_owner_batch_pause, 0, memory_order_release);
        void *published = NULL;
        if (pthread_join(thread, &published) != 0) return 51;
        int visible = published != NULL &&
                      atomic_load_explicit(&set->count, memory_order_acquire) == token + 3 &&
                      jit_body_owner_lookup(rw + 0x10, &owner) && owner == 1 &&
                      jit_body_owner_lookup(rw + 0x27, &owner) && owner == 3;
        jit_body_owner_drop_generation(generation);
        return hidden && visible ? 0 : 51;
    }
    if (scenario == 38) {
        const uint64_t generation = UINT64_MAX - 38, rw = UINT64_C(0x630000000);
        uint32_t token = JIT_BODY_OWNER_N - 1;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 52;
        set->rw = (uint8_t *)(uintptr_t)rw;
        atomic_store_explicit(&set->count, token, memory_order_relaxed);
        jit_body_owner_range range[] = {{rw + 0x10, rw + 0x18, 1}, {rw + 0x18, rw + 0x20, 2}};
        int rejected = !jit_body_owner_publish_n(generation, token, range, 2);
        int unchanged = atomic_load_explicit(&set->count, memory_order_acquire) == token;
        jit_body_owner_drop_generation(generation);
        return rejected && unchanged ? 0 : 52;
    }
    if (scenario == 39) {
        static const uint8_t admitted[][6] = {{0x66, 0x0F, 0xDB, 0xC1}, {0x66, 0x0F, 0x6F, 0x00},
                                               {0x66, 0x0F, 0x73, 0xDA, 0}, {0x66, 0x0F, 0xEB, 0xC1},
                                               {0x66, 0x0F, 0x7E, 0xC0}};
        static const uint8_t refused[][6] = {{0xF0, 0x66, 0x0F, 0xDB, 0xC1}, // LOCK
                                              {0xF2, 0x0F, 0xDB, 0xC1},       // wrong mandatory prefix
                                              {0xF3, 0x0F, 0x6F, 0xC1},
                                              {0x67, 0x66, 0x0F, 0xEB, 0xC1}, // address-size override
                                              {0x66, 0x0F, 0x73, 0x18, 1},    // group form requires mod=3
                                              {0x66, 0x0F, 0xDB, 0x00},       // aligned-memory contract deferred
                                              {0x66, 0x0F, 0x7E, 0x00}};      // memory store deferred
        struct insn decoded;
        for (size_t i = 0; i < sizeof admitted / sizeof admitted[0]; i++)
            if (hl_x86_decode((uint64_t)(uintptr_t)admitted[i], &decoded) <= 0 ||
                translit_classify(&decoded) != TL_SSE2)
                return 53;
        for (size_t i = 0; i < sizeof refused / sizeof refused[0]; i++)
            if (hl_x86_decode((uint64_t)(uintptr_t)refused[i], &decoded) <= 0 ||
                translit_classify(&decoded) != TL_NO)
                return 53;
        return 0;
    }
    if (scenario == 40) {
        uint8_t emitted[32] = {0};
        hl_x64_asm output = {emitted, emitted + sizeof emitted, 0};
        translit_xmm_cpu(&output, 9, 1);
        static const uint8_t load[] = {0x65, 0xF3, 0x44, 0x0F, 0x6F, 0x0C, 0x25,
                                       (uint8_t)(OFF_V + 16 * 9),
                                       (uint8_t)((OFF_V + 16 * 9) >> 8), 0, 0};
        if (output.overflow || output.cursor - emitted != (ptrdiff_t)sizeof load ||
            memcmp(emitted, load, sizeof load) != 0)
            return 54;
        return 0;
    }
    return 6;
}
#endif

// Both jcc encodings (7x rel8 and 0F 8x rel32) carry the condition in the opcode's low nibble.
static int translit_condition(const struct insn *insn) {
    return insn->op & 0xF;
}

static int translit_jcc_successor(hl_x86_hot_context *context, uint64_t source, uint64_t next,
                                  struct insn *successor) {
#if defined(HL_NATIVE_TEST_HOOKS)
    if (next == translit_test_decode_refuse_pc) return TL_SUCCESSOR_DECODE_REFUSED;
#endif
    int decoded = context != NULL ? hl_x86_decode_context(context, next, successor) : hl_x86_decode(next, successor);
    if (decoded <= 0 || successor->len <= 0) return TL_SUCCESSOR_DECODE_REFUSED;
    if (next > UINT64_MAX - (uint64_t)successor->len ||
        ((next + (uint64_t)successor->len - 1) >> 12) != (source >> 12))
        return TL_SUCCESSOR_PAGE_REFUSED;
    return TL_SUCCESSOR_OK;
}

static void *translit_build(hl_x86_hot_context *context, struct interp_block *block, uint64_t gpc) {
    if (!translit_image_ok()) {
        if (translit_enabled()) (void)__atomic_fetch_add(&translit_declined, 1, __ATOMIC_RELAXED);
        return NULL;
    }
    uint32_t owner_token;
#if defined(HL_NATIVE_TEST_HOOKS)
    const char *exhaust = hl_option_get("HL_TRANSLIT_BODY_OWNER_EXHAUST");
    if (exhaust != NULL && exhaust[0] != '0' && exhaust[0] != 0) {
        jit_body_owner_set *set = jit_body_owner_set_for(g_cache_gen, 1);
        if (set != NULL) atomic_store_explicit(&set->count, JIT_BODY_OWNER_N, memory_order_relaxed);
    }
#endif
    if (!jit_body_owner_reserve(g_cache_gen, &owner_token)) return NULL;
    struct insn first_insn;
    int sse2_run = hl_x86_decode_context(context, gpc, &first_insn) > 0 &&
                   translit_classify(&first_insn) == TL_SSE2;
    // Reserve the worst case before emitting a byte.  Publication advances only the exact populated
    // prefix; conservative capacity refusal falls back to the interpreter with no reachable partial body.
    if (sse2_run && !jit_body_owner_reserve_n(g_cache_gen, TL_MAX_INSNS + 1, &owner_token)) return NULL;
    uint8_t *arena_limit = (uint8_t *)g_cache + CACHE_SZ - CACHE_EMIT_HEADROOM;
    uint8_t *start = g_cp;
    if (start + TL_MAX_BYTES > arena_limit) return NULL;
    struct translit_builder builder = {{start, start + TL_MAX_BYTES, 0}, start, g_rw2rx};
    hl_x64_asm *a = &builder.asm_state;

    uint64_t pc = gpc;
    int count = 0, terminated = 0, stitched = 0;
    uint64_t sse2_signature = 0;
    jit_body_owner_range sse2_owner[TL_MAX_INSNS + 1];
    uint32_t sse2_owner_count = 0;
#if defined(HL_NATIVE_TEST_HOOKS)
    int jcc_fall_stitched = 0;
#endif
    uint8_t bytes[16];

    while (count < TL_MAX_INSNS && !a->overflow) {
        struct insn insn;
        if (hl_x86_decode_context(context, pc, &insn) <= 0) break;
        int kind = translit_classify(&insn);
        if ((sse2_run && kind != TL_SSE2) || (!sse2_run && kind == TL_SSE2)) break;
        if (kind == TL_NO) break;
        if (!translit_displaced_safe(&insn, kind)) {
            (void)__atomic_fetch_add(&translit_declined, 1, __ATOMIC_RELAXED);
            (void)__atomic_fetch_add(&translit_displaced_operand_declined, 1, __ATOMIC_RELAXED);
            translit_count_displaced_refusal(&insn);
            break;
        }
        if (x86_guest_fetch_exec(pc, bytes, (size_t)insn.len) != 0) break;

        uint64_t next = pc + (uint64_t)insn.len;
        uint8_t *instruction_start = a->cursor;
        int preserve_register = -1;
        uint8_t *preserve_start = NULL, *preserve_end = NULL;

        if (kind == TL_COPY) {
            hl_x64_copy(a, bytes, insn.len);
            if (a->overflow) break;
            if (insn.rip_rel && !translit_fix_riprel(&builder, &insn, next, instruction_start)) {
                a->cursor = instruction_start;
                uint64_t guest_target = next + (uint64_t)insn.disp;
                uint64_t storage_target = translit_riprel_target(insn.op, insn.two, guest_target, g_nonpie_lo,
                                                                 g_nonpie_hi, g_nonpie_bias);
                // Natural low ET_EXEC currently admits only the measured dominant form.  The wider helper
                // remains available to displaced images, whose complete family is covered separately.
                if ((!g_nonpie_lo && (insn.two || (insn.op != 0x8B && insn.op != 0x8D))) ||
                    !translit_lower_absolute_riprel(a, &insn, guest_target, storage_target, g_nonpie_lo != 0,
                                                    &preserve_register)) {
                    if (g_nonpie_lo)
                        (void)__atomic_fetch_add(&translit_displaced_riprel_unreachable, 1, __ATOMIC_RELAXED);
                    if (g_nonpie_lo) translit_count_displaced_refusal(&insn);
                    a->cursor = instruction_start; // un-emit: the block ends before this instruction
                    break;
                }
                uint64_t *counter = insn.op == 0x8D && !insn.two
                                        ? (g_nonpie_lo ? &translit_displaced_lea_lowered
                                                       : &translit_natural_lea_lowered)
                                        : &translit_displaced_riprel_lowered;
                (void)__atomic_fetch_add(counter, 1, __ATOMIC_RELAXED);
                if (preserve_register >= 0)
                    (void)__atomic_fetch_add(&translit_displaced_scratch_lowered, 1, __ATOMIC_RELAXED);
            }
            jit_instruction_map_put_preserve((uint64_t)(uintptr_t)instruction_start,
                                             (uint64_t)(uintptr_t)a->cursor, pc,
                                             translit_preserve_mask(preserve_register));
            pc = next;
            count++;
            continue;
        }

        if (kind == TL_SSE2) {
            int destination = insn.op == 0x73 ? insn.rm_reg : insn.reg;
            if (insn.op == 0xDB || insn.op == 0xEB) {
                translit_xmm_cpu(a, insn.reg, 1);
                if (!insn.is_mem && insn.rm_reg != insn.reg) translit_xmm_cpu(a, insn.rm_reg, 1);
            } else if (insn.op == 0x6F) {
                if (!insn.is_mem) translit_xmm_cpu(a, insn.rm_reg, 1);
            } else if (insn.op == 0x73) {
                translit_xmm_cpu(a, insn.rm_reg, 1);
            } else { // 66 0F 7E, register destination only
                translit_xmm_cpu(a, insn.reg, 1);
            }
            uint8_t *guest_instruction = a->cursor;
            hl_x64_copy(a, bytes, insn.len);
            if (a->overflow) break;
            if (insn.rip_rel && !translit_fix_riprel(&builder, &insn, next, guest_instruction)) {
                a->cursor = instruction_start;
                break;
            }
            if (insn.op != 0x7E) translit_xmm_cpu(a, destination, 0);
            if (a->overflow) {
                a->cursor = instruction_start;
                break;
            }
            uint8_t *commit_end = a->cursor;
            sse2_owner[sse2_owner_count++] = (jit_body_owner_range){
                (uint64_t)(uintptr_t)instruction_start, (uint64_t)(uintptr_t)commit_end, pc};
            jit_instruction_map_put((uint64_t)(uintptr_t)instruction_start,
                                    (uint64_t)(uintptr_t)commit_end, pc);
            pc = next;
            count++;
            sse2_signature = (sse2_signature << 8) | insn.op;
            continue;
        }

        // Keep the linear fall-through of a conditional branch inside this descriptor. The taken edge
        // still leaves through the ordinary spill/dispatcher epilogue, so no external host address is
        // baked and every non-fall-through edge remains an IRQ/STW/checkpoint boundary. Inverting the
        // host condition lets the overwhelmingly common fall-through skip that side exit without a
        // second branch. The same-page and decode checks keep the source hull linear and make an
        // inaccessible successor start a fresh interpreted/translated block instead of faulting here.
        if (kind == TL_JCC && (next >> 12) != (pc >> 12))
            (void)__atomic_fetch_add(&translit_jcc_fall_page_refused, 1, __ATOMIC_RELAXED);
        if (kind == TL_JCC && (next >> 12) == (pc >> 12)) {
            (void)__atomic_fetch_add(&translit_jcc_fall_candidates, 1, __ATOMIC_RELAXED);
            if (stitched < TL_STITCH_MAX_JMPS) {
                struct insn successor;
                int successor_status = translit_jcc_successor(context, pc, next, &successor);
                if (successor_status == TL_SUCCESSOR_DECODE_REFUSED) {
                    (void)__atomic_fetch_add(&translit_jcc_fall_decode_refused, 1, __ATOMIC_RELAXED);
                } else if (successor_status == TL_SUCCESSOR_PAGE_REFUSED) {
                    (void)__atomic_fetch_add(&translit_jcc_fall_successor_page_refused, 1, __ATOMIC_RELAXED);
                } else {
                    uint8_t *fall = hl_x64_jcc_rel32(a, translit_condition(&insn) ^ 1);
                    translit_emit_exit_const(a, next + (uint64_t)insn.imm, R_BRANCH);
                    hl_x64_patch_rel32(a, fall, a->cursor);
                    if (a->overflow) {
                        a->cursor = instruction_start;
                        break;
                    }
                    jit_instruction_map_put((uint64_t)(uintptr_t)instruction_start,
                                            (uint64_t)(uintptr_t)a->cursor, pc);
                    pc = next;
                    count++;
                    stitched++;
#if defined(HL_NATIVE_TEST_HOOKS)
                    jcc_fall_stitched++;
#endif
                    (void)__atomic_fetch_add(&translit_jcc_fall_admitted, 1, __ATOMIC_RELAXED);
                    continue;
                }
            }
        }

        // A bounded forward jump on the same source page can remain in this descriptor: no host address is
        // baked, the existing [gpc, guest_end) hull remains a conservative SMC interval, and the ordinary
        // instruction/byte caps still bound the distance between dispatcher safepoints. Validate the target
        // before admitting it so an inaccessible/corrupt successor takes the normal guest-fault path from a
        // fresh block rather than turning a translation-time read into an engine fault.
        if (kind == TL_JMP) {
            uint64_t target = next + (uint64_t)insn.imm;
            if (target > next && (target >> 12) == (pc >> 12)) {
                (void)__atomic_fetch_add(&translit_stitch_candidates, 1, __ATOMIC_RELAXED);
                struct insn successor;
                if (stitched < TL_STITCH_MAX_JMPS && hl_x86_decode_context(context, target, &successor) > 0 &&
                    successor.len > 0) {
                    pc = target;
                    count++;
                    stitched++;
                    (void)__atomic_fetch_add(&translit_stitch_admitted, 1, __ATOMIC_RELAXED);
                    continue;
                }
            }
        }

        // Terminators. Each writes cpu->rip (directly or through the exit) and returns to the dispatcher;
        // no external inter-block edge is emitted, so every exit is a dispatcher safepoint and the shared
        // loop's signal/checkpoint/stop-the-world polling keeps working unchanged.
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
        case TL_CALL_REG:
        case TL_CALL_RIP: {
            // The pushed return address is guest-visible (unwinders and dladdr read it), so it is exactly
            // interp_call_return_pc's value -- which is `next` here, since a biased image never gets this
            // far. Written as two 32-bit immediate stores because that clobbers no register: if the store
            // faults (a guest stack overflow), every host GPR still holds its guest value and the fault
            // path can restart the CALL exactly.
            uint64_t pushed = interp_call_return_pc(next);
            // Read the target from the LIVE host register, not from cpu->r[] -- nothing is spilled yet.
            if (kind == TL_CALL_REG) hl_x64_store_gs(a, insn.rm_reg, OFF_RIP);
            if (kind == TL_CALL_RIP) {
                uint64_t guest_target = next + (uint64_t)insn.disp;
                uint64_t storage_target = translit_riprel_target(insn.op, insn.two, guest_target, g_nonpie_lo,
                                                                 g_nonpie_hi, g_nonpie_bias);
                // Borrow r11 only for the pointer load, then spill the target and restore the guest value
                // before either stack store can fault. cpu->rip remains the source CALL until both stores
                // succeed. A wrapped provenance map therefore retains the block-source fallback with an
                // exact guest register file rather than publishing this CALL's target early.
                hl_x64_store_gs(a, HL_X64_R11, R_OFF(HL_X64_R11));
                hl_x64_mov_imm64(a, HL_X64_R11, storage_target);
                preserve_start = a->cursor;
                hl_x64_load_ind(a, HL_X64_R11, HL_X64_R11);
                preserve_end = a->cursor;
                preserve_register = HL_X64_R11;
                hl_x64_store_gs(a, HL_X64_R11, TL_MM_ENTRY);
                hl_x64_load_gs(a, HL_X64_R11, R_OFF(HL_X64_R11));
            }
            hl_x64_store_rsp_imm32(a, -8, (uint32_t)pushed);
            hl_x64_store_rsp_imm32(a, -4, (uint32_t)(pushed >> 32));
            hl_x64_lea_rsp(a, -8);
            if (kind == TL_CALL_RIP) {
                hl_x64_load_gs(a, HL_X64_R11, TL_MM_ENTRY);
                hl_x64_store_gs(a, HL_X64_R11, OFF_RIP);
                hl_x64_load_gs(a, HL_X64_R11, R_OFF(HL_X64_R11));
            }
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
        case TL_JMP_RIP: {
            uint64_t guest_target = next + (uint64_t)insn.disp;
            uint64_t storage_target = translit_riprel_target(insn.op, insn.two, guest_target, g_nonpie_lo,
                                                             g_nonpie_hi, g_nonpie_bias);
            hl_x64_store_gs(a, HL_X64_R11, R_OFF(HL_X64_R11));
            hl_x64_mov_imm64(a, HL_X64_R11, storage_target);
            preserve_start = a->cursor;
            hl_x64_load_ind(a, HL_X64_R11, HL_X64_R11);
            preserve_end = a->cursor;
            preserve_register = HL_X64_R11;
            hl_x64_store_gs(a, HL_X64_R11, OFF_RIP);
            hl_x64_load_gs(a, HL_X64_R11, R_OFF(HL_X64_R11));
            translit_emit_exit_preset(a, R_BRANCH);
            break;
        }
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
        if (preserve_start) {
            jit_instruction_map_put_preserve((uint64_t)(uintptr_t)preserve_start,
                                             (uint64_t)(uintptr_t)preserve_end, pc,
                                             translit_preserve_mask(preserve_register));
            (void)__atomic_fetch_add(&translit_rip_indirect_lowered, 1, __ATOMIC_RELAXED);
        }
        jit_instruction_map_put((uint64_t)(uintptr_t)(preserve_end ? preserve_end : instruction_start),
                                (uint64_t)(uintptr_t)a->cursor, pc);
        pc = next;
        count++;
        terminated = 1;
        break;
    }

    if (count == 0 || a->overflow) return NULL;
    if (!terminated) {
        uint8_t *tail = a->cursor;
        translit_emit_exit_const(a, pc, R_BRANCH);
        if (a->overflow) return NULL;
        if (sse2_run)
            sse2_owner[sse2_owner_count++] = (jit_body_owner_range){
                (uint64_t)(uintptr_t)tail, (uint64_t)(uintptr_t)a->cursor, pc};
    }

    int owner_published = sse2_run
                              ? jit_body_owner_publish_n(g_cache_gen, owner_token, sse2_owner, sse2_owner_count)
                              : jit_body_owner_publish(g_cache_gen, owner_token, (uint64_t)(uintptr_t)start,
                                                       (uint64_t)(uintptr_t)a->cursor, gpc);
    if (!owner_published)
        return NULL;
#if defined(HL_NATIVE_TEST_HOOKS)
    (void)__atomic_fetch_add(&translit_body_owner_published, 1, __ATOMIC_RELAXED);
    if (sse2_run) {
        (void)__atomic_fetch_add(&translit_sse2_runs_admitted, 1, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_instructions_admitted, (uint64_t)count, __ATOMIC_RELAXED);
        if (count == 5 && sse2_signature == UINT64_C(0xDB6F73EB7E))
            (void)__atomic_fetch_add(&translit_sse2_target_runs, 1, __ATOMIC_RELAXED);
    }
#endif
    block->host_entry_off = (uint32_t)(start - (uint8_t *)block);
    block->host_len = (uint32_t)(a->cursor - start);
    block->guest_end = pc;
#if defined(HL_NATIVE_TEST_HOOKS)
    block->profile_jcc_fall_stitches = (uint8_t)jcc_fall_stitched;
#endif
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
#if defined(HL_NATIVE_TEST_HOOKS)
    if (block->profile_jcc_fall_stitches != 0)
        (void)__atomic_fetch_add(&translit_jcc_fall_executed, block->profile_jcc_fall_stitches, __ATOMIC_RELAXED);
#endif
}

// One line of the [prof] report (engine/target/x86_64.c), and the only place an operator can learn that
// this backend was selected and then silently refused. Written here rather than there because the
// refusal predicates are this file's; the caller needs no #ifdef either way.
static int translit_report(char *out, size_t size) {
    uint64_t blocks = __atomic_load_n(&translit_blocks, __ATOMIC_RELAXED);
    uint64_t entries = __atomic_load_n(&translit_entries, __ATOMIC_RELAXED);
    uint64_t declined = __atomic_load_n(&translit_declined, __ATOMIC_RELAXED);
    uint64_t stitch_candidates = __atomic_load_n(&translit_stitch_candidates, __ATOMIC_RELAXED);
    uint64_t stitch_admitted = __atomic_load_n(&translit_stitch_admitted, __ATOMIC_RELAXED);
    uint64_t jcc_fall_candidates = __atomic_load_n(&translit_jcc_fall_candidates, __ATOMIC_RELAXED);
    uint64_t jcc_fall_admitted = __atomic_load_n(&translit_jcc_fall_admitted, __ATOMIC_RELAXED);
    uint64_t jcc_fall_page_refused = __atomic_load_n(&translit_jcc_fall_page_refused, __ATOMIC_RELAXED);
    uint64_t jcc_fall_successor_page_refused =
        __atomic_load_n(&translit_jcc_fall_successor_page_refused, __ATOMIC_RELAXED);
    uint64_t jcc_fall_decode_refused = __atomic_load_n(&translit_jcc_fall_decode_refused, __ATOMIC_RELAXED);
#if defined(HL_NATIVE_TEST_HOOKS)
    uint64_t jcc_fall_executed = __atomic_load_n(&translit_jcc_fall_executed, __ATOMIC_RELAXED);
#else
    uint64_t jcc_fall_executed = 0;
#endif
    uint64_t rip_indirect_lowered = __atomic_load_n(&translit_rip_indirect_lowered, __ATOMIC_RELAXED);
    uint64_t provenance_fallback = __atomic_load_n(&translit_provenance_fallback, __ATOMIC_RELAXED);
    uint64_t body_owner_recovered = __atomic_load_n(&translit_body_owner_recovered, __ATOMIC_RELAXED);
#if defined(HL_NATIVE_TEST_HOOKS)
    uint64_t body_owner_published = __atomic_load_n(&translit_body_owner_published, __ATOMIC_RELAXED);
    uint64_t sse2_runs_admitted = __atomic_load_n(&translit_sse2_runs_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_instructions_admitted =
        __atomic_load_n(&translit_sse2_instructions_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_target_runs = __atomic_load_n(&translit_sse2_target_runs, __ATOMIC_RELAXED);
#else
    uint64_t body_owner_published = 0;
    uint64_t sse2_runs_admitted = 0, sse2_instructions_admitted = 0, sse2_target_runs = 0;
#endif
    uint64_t displaced_blocks = __atomic_load_n(&translit_displaced_blocks, __ATOMIC_RELAXED);
    uint64_t displaced_entries = __atomic_load_n(&translit_displaced_entries, __ATOMIC_RELAXED);
    uint64_t displaced_operand_declined =
        __atomic_load_n(&translit_displaced_operand_declined, __ATOMIC_RELAXED);
    uint64_t displaced_sse2_memory_declined =
        __atomic_load_n(&translit_displaced_sse2_memory_declined, __ATOMIC_RELAXED);
    uint64_t displaced_riprel_projected =
        __atomic_load_n(&translit_displaced_riprel_projected, __ATOMIC_RELAXED);
    uint64_t displaced_lea_low = __atomic_load_n(&translit_displaced_lea_low, __ATOMIC_RELAXED);
    uint64_t displaced_riprel_unreachable =
        __atomic_load_n(&translit_displaced_riprel_unreachable, __ATOMIC_RELAXED);
    uint64_t displaced_riprel_lowered = __atomic_load_n(&translit_displaced_riprel_lowered, __ATOMIC_RELAXED);
    uint64_t displaced_scratch_lowered =
        __atomic_load_n(&translit_displaced_scratch_lowered, __ATOMIC_RELAXED);
    uint64_t displaced_lea_lowered = __atomic_load_n(&translit_displaced_lea_lowered, __ATOMIC_RELAXED);
    uint64_t natural_abs32_lowered = __atomic_load_n(&translit_natural_abs32_lowered, __ATOMIC_RELAXED);
    uint64_t natural_lea_lowered = __atomic_load_n(&translit_natural_lea_lowered, __ATOMIC_RELAXED);
    uint64_t displaced_refusal_prefix = __atomic_load_n(&translit_displaced_refusal_prefix, __ATOMIC_RELAXED);
    uint64_t displaced_refusal_width = __atomic_load_n(&translit_displaced_refusal_width, __ATOMIC_RELAXED);
    uint64_t displaced_refusal_rsp = __atomic_load_n(&translit_displaced_refusal_rsp, __ATOMIC_RELAXED);
    uint64_t displaced_refusal_opcode_other =
        __atomic_load_n(&translit_displaced_refusal_opcode_other, __ATOMIC_RELAXED);
    unsigned displaced_refusal_key = 0;
    uint64_t displaced_refusal_count = 0;
    for (unsigned key = 0; key < 512; key++) {
        uint64_t count = __atomic_load_n(&translit_displaced_refusal_opcode[key], __ATOMIC_RELAXED);
        if (count > displaced_refusal_count) {
            displaced_refusal_key = key;
            displaced_refusal_count = count;
        }
    }
    if (!translit_enabled()) return snprintf(out, size, "[prof] translit: not selected\n");
    if (g_nonpie_lo != 0)
        return snprintf(out, size,
                        "[prof] translit: displaced blocks=%llu entries=%llu declined=%llu displaced_blocks=%llu"
                        " displaced_entries=%llu operand_declined=%llu riprel_projected=%llu lea_low=%llu"
                        " sse2_memory_declined=%llu"
                        " riprel_unreachable=%llu riprel_lowered=%llu scratch_lowered=%llu lea_lowered=%llu"
                        " refusal_key=%u refusal_count=%llu refusal_prefix=%llu refusal_width=%llu"
                        " refusal_rsp=%llu refusal_opcode=%llu stitch_candidates=%llu stitch_admitted=%llu"
                        " rip_indirect_lowered=%llu jcc_fall_candidates=%llu jcc_fall_admitted=%llu"
                        " jcc_fall_page_refused=%llu jcc_fall_successor_page_refused=%llu"
                        " jcc_fall_decode_refused=%llu jcc_fall_executed=%llu\n",
                        (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined,
                        (unsigned long long)displaced_blocks, (unsigned long long)displaced_entries,
                        (unsigned long long)displaced_operand_declined,
                        (unsigned long long)displaced_riprel_projected, (unsigned long long)displaced_lea_low,
                        (unsigned long long)displaced_sse2_memory_declined,
                        (unsigned long long)displaced_riprel_unreachable,
                        (unsigned long long)displaced_riprel_lowered,
                        (unsigned long long)displaced_scratch_lowered,
                        (unsigned long long)displaced_lea_lowered, displaced_refusal_key,
                        (unsigned long long)displaced_refusal_count, (unsigned long long)displaced_refusal_prefix,
                        (unsigned long long)displaced_refusal_width, (unsigned long long)displaced_refusal_rsp,
                        (unsigned long long)displaced_refusal_opcode_other, (unsigned long long)stitch_candidates,
                        (unsigned long long)stitch_admitted, (unsigned long long)rip_indirect_lowered,
                        (unsigned long long)jcc_fall_candidates, (unsigned long long)jcc_fall_admitted,
                        (unsigned long long)jcc_fall_page_refused,
                        (unsigned long long)jcc_fall_successor_page_refused,
                        (unsigned long long)jcc_fall_decode_refused, (unsigned long long)jcc_fall_executed);
    if (jit86_store_alias_observation_active())
        return snprintf(out, size,
                        "[prof] translit: declined, guest executable mapping or shared alias observed"
                        " (blocks=%llu entries=%llu declined=%llu stitch_candidates=%llu stitch_admitted=%llu)\n",
                        (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined,
                        (unsigned long long)stitch_candidates, (unsigned long long)stitch_admitted);
    return snprintf(out, size,
                    "[prof] translit: blocks=%llu entries=%llu declined=%llu stitch_candidates=%llu"
                    " stitch_admitted=%llu rip_indirect_lowered=%llu provenance_fallback=%llu"
                    " body_owner_recovered=%llu body_owner_published=%llu"
                    " sse2_runs_admitted=%llu sse2_instructions_admitted=%llu sse2_target_runs=%llu"
                    " riprel_lowered=%llu scratch_lowered=%llu lea_lowered=%llu abs32_lowered=%llu"
                    " natural_lea_lowered=%llu"
                    " jcc_fall_candidates=%llu jcc_fall_admitted=%llu jcc_fall_page_refused=%llu"
                    " jcc_fall_successor_page_refused=%llu jcc_fall_decode_refused=%llu"
                    " jcc_fall_executed=%llu\n",
                    (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined,
                    (unsigned long long)stitch_candidates, (unsigned long long)stitch_admitted,
                    (unsigned long long)rip_indirect_lowered, (unsigned long long)provenance_fallback,
                    (unsigned long long)body_owner_recovered, (unsigned long long)body_owner_published,
                    (unsigned long long)sse2_runs_admitted, (unsigned long long)sse2_instructions_admitted,
                    (unsigned long long)sse2_target_runs,
                    (unsigned long long)displaced_riprel_lowered,
                    (unsigned long long)displaced_scratch_lowered,
                    (unsigned long long)displaced_lea_lowered,
                    (unsigned long long)natural_abs32_lowered,
                    (unsigned long long)natural_lea_lowered,
                    (unsigned long long)jcc_fall_candidates, (unsigned long long)jcc_fall_admitted,
                    (unsigned long long)jcc_fall_page_refused,
                    (unsigned long long)jcc_fall_successor_page_refused,
                    (unsigned long long)jcc_fall_decode_refused, (unsigned long long)jcc_fall_executed);
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
    uint32_t preserve_registers = 0;
    uint64_t guest_pc;
    int exact_pc = jit_instruction_guest_pc_preserve(host_pc, &guest_pc, &preserve_registers);
#if defined(HL_NATIVE_TEST_HOOKS)
    const char *force_miss = hl_option_get("HL_TRANSLIT_PROVENANCE_FALLBACK");
    if (force_miss != NULL && force_miss[0] != '0' && force_miss[0] != 0) {
        exact_pc = 0;
        preserve_registers = 0;
        (void)__atomic_fetch_add(&translit_provenance_fallback, 1, __ATOMIC_RELAXED);
    }
#endif
    if (!exact_pc && jit_body_owner_lookup(host_pc, &guest_pc)) {
        exact_pc = 1;
        preserve_registers = 0;
        (void)__atomic_fetch_add(&translit_body_owner_recovered, 1, __ATOMIC_RELAXED);
    }
    for (int i = 0; i < 16; i++) {
        if ((preserve_registers & (1u << i)) == 0) cpu->r[i] = (uint64_t)gregs[g_translit_greg[i]];
    }
    translit_flags_out(cpu, (uint64_t)gregs[HL_HOST_UC_REG_EFL]);
    if (exact_pc) cpu->rip = guest_pc;
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

static void *translit_build(hl_x86_hot_context *context, struct interp_block *block, uint64_t gpc) {
    (void)context;
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

#if defined(HL_NATIVE_TEST_HOOKS)
HL_API int hl_x86_64_translit_displaced_test(uint32_t scenario) {
    (void)scenario;
    return -1;
}
#endif

#endif
