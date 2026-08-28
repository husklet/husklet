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
_Static_assert(TL_MAX_INSNS < 256,
               "hook marker's source-page delta must represent every monotonic block instruction");
#ifndef TL_STITCH_MAX_JMPS
#define TL_STITCH_MAX_JMPS 8
#endif
#define TL_MAX_OWNER_RANGES (TL_MAX_INSNS + 3 * TL_STITCH_MAX_JMPS + 1)

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
static _Atomic uint64_t translit_jcc_link_admitted;
static _Atomic uint64_t translit_jcc_link_taken;
static _Atomic uint64_t translit_jcc_link_irq_fallback;
static _Atomic uint64_t translit_jcc_link_dispatcher;
static _Atomic uint64_t translit_jcc_ibtc_emitted;
static _Atomic uint64_t translit_jcc_ibtc_hit;
static _Atomic uint64_t translit_jcc_ibtc_miss;
static _Atomic uint64_t translit_jcc_ibtc_irq;
static _Atomic uint64_t translit_jcc_ibtc_fill_count;
static _Atomic uint64_t translit_jcc_ibtc_collision;
static _Atomic uint64_t translit_jcc_ibtc_suppressed;
static _Atomic uint64_t translit_jcc_ibtc_invalid_refusal;
#endif
enum translit_jcc_ibtc_counter {
    TL_JCC_IBTC_COUNT_EMITTED,
    TL_JCC_IBTC_COUNT_HIT,
    TL_JCC_IBTC_COUNT_MISS,
    TL_JCC_IBTC_COUNT_IRQ,
    TL_JCC_IBTC_COUNT_FILL,
    TL_JCC_IBTC_COUNT_SUPPRESSED,
    TL_JCC_IBTC_COUNT_INVALID_REFUSAL,
};
#if !defined(HL_NATIVE_TEST_HOOKS)
_Static_assert(TL_JCC_IBTC_COUNT_EMITTED == HL_BACKEND_JCC_IBTC_EMITTED &&
                   TL_JCC_IBTC_COUNT_HIT == HL_BACKEND_JCC_IBTC_HIT &&
                   TL_JCC_IBTC_COUNT_MISS == HL_BACKEND_JCC_IBTC_MISS &&
                   TL_JCC_IBTC_COUNT_IRQ == HL_BACKEND_JCC_IBTC_IRQ &&
                   TL_JCC_IBTC_COUNT_FILL == HL_BACKEND_JCC_IBTC_FILL &&
                   TL_JCC_IBTC_COUNT_SUPPRESSED == HL_BACKEND_JCC_IBTC_SUPPRESSED &&
                   TL_JCC_IBTC_COUNT_INVALID_REFUSAL == HL_BACKEND_JCC_IBTC_INVALID_REFUSAL,
               "JCC IBTC local and shared counter kinds must remain identical");
#endif

static uintptr_t translit_jcc_ibtc_dynamic_counter(enum translit_jcc_ibtc_counter kind) {
#if defined(HL_NATIVE_TEST_HOOKS)
    switch (kind) {
    case TL_JCC_IBTC_COUNT_HIT: return (uintptr_t)&translit_jcc_ibtc_hit;
    case TL_JCC_IBTC_COUNT_MISS: return (uintptr_t)&translit_jcc_ibtc_miss;
    case TL_JCC_IBTC_COUNT_IRQ: return (uintptr_t)&translit_jcc_ibtc_irq;
    default: return 0;
    }
#else
    return hl_backend_tree_jcc_ibtc_dynamic_counter_address((enum hl_backend_jcc_ibtc_counter)kind);
#endif
}

static void translit_jcc_ibtc_count(enum translit_jcc_ibtc_counter kind, uint64_t count) {
#if defined(HL_NATIVE_TEST_HOOKS)
    _Atomic uint64_t *counter = NULL;
    switch (kind) {
    case TL_JCC_IBTC_COUNT_EMITTED: counter = &translit_jcc_ibtc_emitted; break;
    case TL_JCC_IBTC_COUNT_HIT: counter = &translit_jcc_ibtc_hit; break;
    case TL_JCC_IBTC_COUNT_MISS: counter = &translit_jcc_ibtc_miss; break;
    case TL_JCC_IBTC_COUNT_IRQ: counter = &translit_jcc_ibtc_irq; break;
    case TL_JCC_IBTC_COUNT_FILL: counter = &translit_jcc_ibtc_fill_count; break;
    case TL_JCC_IBTC_COUNT_SUPPRESSED: counter = &translit_jcc_ibtc_suppressed; break;
    case TL_JCC_IBTC_COUNT_INVALID_REFUSAL: counter = &translit_jcc_ibtc_invalid_refusal; break;
    }
    if (counter != NULL && count != 0) atomic_fetch_add_explicit(counter, count, memory_order_relaxed);
#else
    hl_backend_tree_jcc_ibtc_add((enum hl_backend_jcc_ibtc_counter)kind, count);
#endif
}
static uint64_t translit_displaced_blocks;
static uint64_t translit_rip_indirect_lowered;
static uint64_t translit_provenance_fallback;
static uint64_t translit_body_owner_recovered;
// Production diagnostics for the same-binary mixed normal/SSE control. Encounters are scan telemetry,
// not a comparable policy denominator: the disabled builder stops at its first boundary while the enabled
// builder can encounter more. Admission advances only after the complete ordered recovery batch publishes;
// execution advances only after a diagnostic terminal marker completes.
static uint64_t translit_mixed_sse_encounters;
static uint64_t translit_mixed_sse_admitted;
static uint64_t translit_mixed_sse_transitions;
static uint64_t translit_mixed_sse_executed;
static uint64_t translit_mixed_sse_executed_transitions;
static uint64_t translit_mixed_sse_disabled_boundaries;
#if defined(HL_NATIVE_TEST_HOOKS)
static uint64_t translit_body_owner_published;
static uint64_t translit_sse2_runs_admitted;
static uint64_t translit_sse2_instructions_admitted;
static uint64_t translit_sse2_target_runs;
static uint64_t translit_sse2_next_family_runs;
static uint64_t translit_sse2_store_instructions;
static uint64_t translit_sse2_store_movups;
static uint64_t translit_sse2_store_movaps;
static uint64_t translit_sse2_store_movdqu;
static uint64_t translit_sse2_store_family_runs;
static uint64_t translit_sse2_pxor_admitted;
static uint64_t translit_sse2_pxor_runs_admitted;
static uint64_t translit_sse2_punpcklqdq_admitted;
static uint64_t translit_sse2_punpcklqdq_runs_admitted;
static uint64_t translit_sse2_movd_admitted;
static uint64_t translit_sse2_movd_runs_admitted;
static uint64_t translit_sse2_movhlps_admitted;
static uint64_t translit_fs_mem_admitted;
#if defined(HL_NATIVE_TEST_HOOKS)
static const uint8_t *translit_fall_fixture_bytes;
static uint64_t translit_fall_fixture_guest;
static size_t translit_fall_fixture_length;
static uint32_t translit_test_last_fall_marker;
static int translit_test_fall_marker_seen;
static int translit_fall_fixture_test(unsigned scenario);
static int translit_mixed_fixture_test(unsigned scenario);
static int translit_jcc_ibtc_fixture_test(unsigned scenario);
static int translit_jcc_ibtc_signal_stage_test(void);
static void run_block(hl_x86_hot_context *context, struct cpu *cpu, void *code);
#endif
static uint64_t translit_fs_fixture_admitted;
#define TRANSLIT_PUNPCKLQDQ_PROFILE                                                                    \
    " sse2_punpcklqdq_admitted=%llu sse2_punpcklqdq_runs_admitted=%llu sse2_movhlps_admitted=%llu"
#define TRANSLIT_PUNPCKLQDQ_ARGUMENTS                                                                  \
    , (unsigned long long)sse2_punpcklqdq_admitted, (unsigned long long)sse2_punpcklqdq_runs_admitted,   \
        (unsigned long long)sse2_movhlps_admitted
#define TRANSLIT_MOVD_PROFILE " sse2_movd_admitted=%llu sse2_movd_runs_admitted=%llu"
#define TRANSLIT_MOVD_ARGUMENTS                                                                        \
    , (unsigned long long)sse2_movd_admitted, (unsigned long long)sse2_movd_runs_admitted
#define TRANSLIT_FS_PROFILE " fs_mem_admitted=%llu fs_fixture_admitted=%llu"
#define TRANSLIT_FS_ARGUMENTS                                                                         \
    , (unsigned long long)fs_mem_admitted, (unsigned long long)fs_fixture_admitted

#define TL_UNSUPPORTED_FORM_N 4096u
#define TL_UNSUPPORTED_SITE_N 8192u
// Process-local diagnostic state. fork() inherits a snapshot and each exiting process reports only its
// own subsequent execution; consumers must not sum parent/child lines as though this were shared aggregation.
typedef struct {
    _Atomic uint32_t state;
    uint64_t key;
    _Atomic uint64_t count;
} translit_unsupported_form;
typedef struct {
    _Atomic uint32_t state;
    uint64_t pc, key;
    _Atomic uint64_t count;
} translit_unsupported_site;
static translit_unsupported_form translit_unsupported_forms[TL_UNSUPPORTED_FORM_N];
static translit_unsupported_site translit_unsupported_sites[TL_UNSUPPORTED_SITE_N];
static _Atomic uint64_t translit_unsupported_keyed;
static _Atomic uint64_t translit_unsupported_form_unique, translit_unsupported_form_collisions;
static _Atomic uint64_t translit_unsupported_form_overflow;
static _Atomic uint64_t translit_unsupported_site_unique;
static _Atomic uint64_t translit_unsupported_site_repeats, translit_unsupported_site_collisions;
static _Atomic uint64_t translit_unsupported_site_overflow;
static _Atomic uint64_t translit_unsupported_deferred_excluded;
static _Atomic uint32_t translit_unsupported_pause_stage, translit_unsupported_pause_ready,
    translit_unsupported_pause_release;
#else
#define TRANSLIT_PUNPCKLQDQ_PROFILE ""
#define TRANSLIT_PUNPCKLQDQ_ARGUMENTS
#define TRANSLIT_MOVD_PROFILE ""
#define TRANSLIT_MOVD_ARGUMENTS
#define TRANSLIT_FS_PROFILE ""
#define TRANSLIT_FS_ARGUMENTS
#endif
static uint64_t translit_displaced_entries;
static uint64_t translit_displaced_operand_declined;
static uint64_t translit_displaced_sse2_memory_declined;
static uint64_t translit_displaced_riprel_projected;
static uint64_t translit_displaced_lea_low;
static uint64_t translit_displaced_riprel_unreachable;
static uint64_t translit_displaced_riprel_lowered;
static uint64_t translit_displaced_scratch_lowered;

#if defined(HL_NATIVE_TEST_HOOKS)
static FILE *translit_perf_map_file;
static pid_t translit_perf_map_pid;
static FILE *translit_jitdump_file;
static void *translit_jitdump_marker;
static pid_t translit_jitdump_pid;
static uint64_t translit_jitdump_index;
extern const char *hl_c_backend_build_fingerprint(void);

typedef struct __attribute__((packed)) translit_jitdump_header {
    uint32_t magic, version, total_size, elf_mach, pad1, pid;
    uint64_t timestamp, flags;
} translit_jitdump_header;

typedef struct __attribute__((packed)) translit_jitdump_load {
    uint32_t id, total_size;
    uint64_t timestamp;
    uint32_t pid, tid;
    uint64_t vma, code_addr, code_size, code_index;
} translit_jitdump_load;

static uint64_t translit_jitdump_now(void) {
    struct timespec now;
    return clock_gettime(CLOCK_MONOTONIC, &now) == 0
               ? (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec
               : 0;
}

static void translit_jitdump_open(const char *directory, pid_t pid) {
    if (translit_jitdump_marker != NULL) (void)munmap(translit_jitdump_marker, 4096);
    if (translit_jitdump_file != NULL) fclose(translit_jitdump_file);
    translit_jitdump_file = NULL;
    translit_jitdump_marker = NULL;
    char path[PATH_MAX];
    int length = snprintf(path, sizeof path, "%s/jit-%lld.dump", directory, (long long)pid);
    if (length <= 0 || (size_t)length >= sizeof path) return;
    translit_jitdump_file = fopen(path, "w+b");
    if (translit_jitdump_file == NULL) return;
    translit_jitdump_header header = {UINT32_C(0x4A695444), 1, sizeof header, 62, 0, (uint32_t)pid,
                                      translit_jitdump_now(), 0};
    if (fwrite(&header, sizeof header, 1, translit_jitdump_file) != 1 || fflush(translit_jitdump_file) != 0) {
        fclose(translit_jitdump_file);
        translit_jitdump_file = NULL;
        return;
    }
    translit_jitdump_marker = mmap(NULL, 4096, PROT_READ | PROT_EXEC, MAP_PRIVATE,
                                   fileno(translit_jitdump_file), 0);
    if (translit_jitdump_marker == MAP_FAILED) {
        translit_jitdump_marker = NULL;
        fclose(translit_jitdump_file);
        translit_jitdump_file = NULL;
        return;
    }
    (void)setvbuf(translit_jitdump_file, NULL, _IONBF, 0);
    translit_jitdump_pid = pid;
    translit_jitdump_index = 0;
}

static void translit_perf_map_publish(const struct interp_block *block, const uint8_t *start, uint32_t host_len,
                                      uint32_t instruction_count) {
    const char *directory = hl_option_get("HL_TRANSLIT_PERF_MAP");
    if (!g_prof || directory == NULL || directory[0] == '\0' || host_len == 0) return;
    pid_t pid = getpid();
    if (translit_perf_map_file == NULL || translit_perf_map_pid != pid) {
        if (translit_perf_map_file != NULL) fclose(translit_perf_map_file);
        char path[PATH_MAX];
        int length = snprintf(path, sizeof path, "%s/perf-%lld.map", directory, (long long)pid);
        if (length <= 0 || (size_t)length >= sizeof path) return;
        translit_perf_map_file = fopen(path, "a");
        if (translit_perf_map_file == NULL) return;
        (void)setvbuf(translit_perf_map_file, NULL, _IONBF, 0);
        translit_perf_map_pid = pid;
    }
    const char *fingerprint = hl_c_backend_build_fingerprint();
    uint64_t guest_length = block->guest_end >= block->gpc ? block->guest_end - block->gpc : 0;
    char name[256];
    int name_length = snprintf(name, sizeof name, "hl_tl_%s_g%llx_gl%llu_i%u", fingerprint,
                               (unsigned long long)block->gpc, (unsigned long long)guest_length,
                               instruction_count);
    if (name_length <= 0 || (size_t)name_length >= sizeof name) return;
    (void)fprintf(translit_perf_map_file, "%llx %x %s\n",
                  (unsigned long long)(uintptr_t)J_RX(start), host_len, name);
    if (translit_jitdump_file == NULL || translit_jitdump_pid != pid) translit_jitdump_open(directory, pid);
    if (translit_jitdump_file == NULL) return;
    translit_jitdump_load load = {0,
                                  (uint32_t)(sizeof load + (size_t)name_length + 1u + host_len),
                                  translit_jitdump_now(),
                                  (uint32_t)pid,
                                  (uint32_t)syscall(SYS_gettid),
                                  (uint64_t)(uintptr_t)J_RX(start),
                                  (uint64_t)(uintptr_t)J_RX(start),
                                  host_len,
                                  ++translit_jitdump_index};
    (void)fwrite(&load, sizeof load, 1, translit_jitdump_file);
    (void)fwrite(name, (size_t)name_length + 1u, 1, translit_jitdump_file);
    (void)fwrite(start, host_len, 1, translit_jitdump_file);
}
#endif
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
#if defined(HL_NATIVE_TEST_HOOKS)
    if (translit_fall_fixture_bytes != NULL) return 1;
#endif
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
    TL_FS_MEM,   // narrow FS:disp32 64-bit register load/sub; guest FS comes from cpu state
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
    if (!insn->p66 && !insn->repne && !insn->lock && !insn->addr32 && !insn->seg && !insn->vex &&
        !insn->evex && !insn->map3 && insn->has_modrm) {
        // MOVDQU uses mandatory F3; MOVUPS/MOVAPS use no mandatory prefix. Stores remain eligible only
        // while translit_image_ok() proves no executable/shared alias observation is active.
        if (insn->rep && (op == 0x6F || (op == 0x7F && insn->is_mem))) return TL_SSE2;
        if (!insn->rep &&
            (op == 0x10 || op == 0x28 || ((op == 0x11 || op == 0x29) && insn->is_mem)))
            return TL_SSE2;
        if (!insn->rep && op == 0x12 && insn->mod == 3) return TL_SSE2; // MOVHLPS xmm,xmm
    }
    if (insn->p66 && !insn->rep && !insn->repne && !insn->lock && !insn->addr32 && !insn->seg &&
        !insn->vex && !insn->evex && !insn->map3 && insn->has_modrm) {
        if (op == 0x6F ||
            ((op == 0x6C || op == 0x6E || op == 0xDB || op == 0xDF || op == 0xEB || op == 0xEF) &&
             insn->mod == 3))
            return TL_SSE2;
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
    if (insn->len <= 0 || insn->vex || insn->evex || insn->map3 || insn->addr32) return TL_NO;
    // First TLS family: exactly `64 REX.W 8b|2b mod00 r/m=100 sib=25 disp32`.  This is the dominant
    // compiler TLS form and has no guest base/index register to project.  Everything wider (GS, an
    // address-size or other prefix, a real SIB operand, stores) remains in the authoritative interpreter.
    if (insn->seg) {
        if (insn->seg == 1 && insn->has_rex && insn->rexW && !insn->rexX && !insn->rexB &&
            !insn->p66 && !insn->lock && !insn->rep && !insn->repne && !insn->two &&
            (insn->op == 0x8B || insn->op == 0x2B) && insn->has_modrm && insn->is_mem &&
            insn->mod == 0 && insn->rm == 4 && !insn->m_hasbase && !insn->m_hasindex && !insn->rip_rel)
            return TL_FS_MEM;
        return TL_NO;
    }
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

#if defined(HL_NATIVE_TEST_HOOKS)
static unsigned translit_unsupported_width(const struct insn *insn) {
    return insn->opsize == 1 ? 0u : insn->opsize == 2 ? 1u : insn->opsize == 4 ? 2u : insn->opsize == 8 ? 3u : 4u;
}

static uint64_t translit_unsupported_key(const struct insn *insn) {
    unsigned map = insn->evex ? 4u : insn->vex ? 3u : insn->map3 ? 2u : insn->two ? 1u : 0u;
    unsigned immediate = insn->imm_bytes == 0 ? 0u : insn->imm_bytes == 1 ? 1u : insn->imm_bytes <= 4 ? 2u : 3u;
    return (uint64_t)insn->op | (uint64_t)map << 8 | (uint64_t)(insn->p66 != 0) << 11 |
           (uint64_t)(insn->rep != 0) << 12 | (uint64_t)(insn->repne != 0) << 13 |
           (uint64_t)(insn->lock != 0) << 14 | (uint64_t)(insn->addr32 != 0) << 15 |
           (uint64_t)(insn->seg & 7) << 16 | (uint64_t)(insn->has_rex != 0) << 19 |
           (uint64_t)(insn->rexW != 0) << 20 | (uint64_t)(insn->rexR != 0) << 21 |
           (uint64_t)(insn->rexX != 0) << 22 | (uint64_t)(insn->rexB != 0) << 23 |
           (uint64_t)(insn->has_modrm != 0) << 24 | (uint64_t)insn->modrm << 25 |
           (uint64_t)(insn->is_mem != 0) << 33 | (uint64_t)(insn->rip_rel != 0) << 34 |
           (uint64_t)translit_unsupported_width(insn) << 35 | (uint64_t)immediate << 38 |
           (uint64_t)(insn->vex_map & 31) << 40 | (uint64_t)(insn->vex_pp & 3) << 45 |
           (uint64_t)(insn->vex_l & 3) << 47 | (uint64_t)(insn->vex_w != 0) << 49 |
           (uint64_t)(insn->vvvv & 31) << 50 | (uint64_t)(insn->evex_mask & 7) << 55 |
           (uint64_t)(insn->evex_z != 0) << 58 | (uint64_t)(insn->evex_b != 0) << 59;
}

static uint64_t translit_unsupported_mix(uint64_t value) {
    value ^= value >> 33;
    value *= UINT64_C(0xff51afd7ed558ccd);
    value ^= value >> 33;
    value *= UINT64_C(0xc4ceb9fe1a85ec53);
    return value ^ (value >> 33);
}

static uint32_t translit_unsupported_published(const _Atomic uint32_t *state) {
    uint32_t value = atomic_load_explicit(state, memory_order_acquire);
    while (value == 1) value = atomic_load_explicit(state, memory_order_acquire);
    return value;
}

static void translit_unsupported_record(const struct insn *insn, uint64_t pc) {
    uint64_t key = translit_unsupported_key(insn);
    unsigned start = (unsigned)translit_unsupported_mix(key) & (TL_UNSUPPORTED_FORM_N - 1u);
    int keyed = 0;
    for (unsigned probe = 0; probe < TL_UNSUPPORTED_FORM_N; probe++) {
        translit_unsupported_form *slot = &translit_unsupported_forms[(start + probe) & (TL_UNSUPPORTED_FORM_N - 1u)];
        uint32_t state = translit_unsupported_published(&slot->state);
        if (state == 0) {
            uint32_t empty = 0;
            if (atomic_compare_exchange_strong_explicit(&slot->state, &empty, 1, memory_order_acquire,
                                                        memory_order_relaxed)) {
                if (atomic_load_explicit(&translit_unsupported_pause_stage, memory_order_relaxed) == 1) {
                    atomic_store_explicit(&translit_unsupported_pause_ready, 1, memory_order_release);
                    while (!atomic_load_explicit(&translit_unsupported_pause_release, memory_order_acquire))
                        sched_yield();
                }
                slot->key = key;
                atomic_store_explicit(&slot->count, 1, memory_order_relaxed);
                atomic_store_explicit(&slot->state, 2, memory_order_release);
                atomic_fetch_add_explicit(&translit_unsupported_form_unique, 1, memory_order_relaxed);
                keyed = 1;
                break;
            }
            // A competing publisher won this exact slot after our initial load. Wait for that slot's
            // metadata rather than probing onward and creating a duplicate key.
            state = translit_unsupported_published(&slot->state);
        }
        if (state == 2 && slot->key == key) {
            atomic_fetch_add_explicit(&slot->count, 1, memory_order_relaxed);
            keyed = 1;
            break;
        }
        atomic_fetch_add_explicit(&translit_unsupported_form_collisions, 1, memory_order_relaxed);
    }
    atomic_fetch_add_explicit(keyed ? &translit_unsupported_keyed : &translit_unsupported_form_overflow, 1,
                              memory_order_relaxed);

    uint64_t mixed = translit_unsupported_mix(pc ^ (key * UINT64_C(0x9e3779b97f4a7c15)));
    start = (unsigned)mixed & (TL_UNSUPPORTED_SITE_N - 1u);
    for (unsigned probe = 0; probe < TL_UNSUPPORTED_SITE_N; probe++) {
        translit_unsupported_site *slot = &translit_unsupported_sites[(start + probe) & (TL_UNSUPPORTED_SITE_N - 1u)];
        uint32_t state = translit_unsupported_published(&slot->state);
        if (state == 0) {
            uint32_t empty = 0;
            if (atomic_compare_exchange_strong_explicit(&slot->state, &empty, 1, memory_order_acquire,
                                                        memory_order_relaxed)) {
                if (atomic_load_explicit(&translit_unsupported_pause_stage, memory_order_relaxed) == 2) {
                    atomic_store_explicit(&translit_unsupported_pause_ready, 1, memory_order_release);
                    while (!atomic_load_explicit(&translit_unsupported_pause_release, memory_order_acquire))
                        sched_yield();
                }
                slot->pc = pc;
                slot->key = key;
                atomic_store_explicit(&slot->count, 1, memory_order_relaxed);
                atomic_store_explicit(&slot->state, 2, memory_order_release);
                atomic_fetch_add_explicit(&translit_unsupported_site_unique, 1, memory_order_relaxed);
                return;
            }
            // As above, a failed empty-slot CAS must retry the slot that became reserved.
            state = translit_unsupported_published(&slot->state);
        }
        if (state == 2 && slot->pc == pc && slot->key == key) {
            atomic_fetch_add_explicit(&slot->count, 1, memory_order_relaxed);
            atomic_fetch_add_explicit(&translit_unsupported_site_repeats, 1, memory_order_relaxed);
            return;
        }
        atomic_fetch_add_explicit(&translit_unsupported_site_collisions, 1, memory_order_relaxed);
    }
    atomic_fetch_add_explicit(&translit_unsupported_site_overflow, 1, memory_order_relaxed);
}

static void translit_unsupported_record_completed(const struct insn *insn, uint64_t pc, int completed) {
    if (translit_classify(insn) != TL_NO) return;
    if (completed)
        translit_unsupported_record(insn, pc);
    else
        atomic_fetch_add_explicit(&translit_unsupported_deferred_excluded, 1, memory_order_relaxed);
}

static void translit_unsupported_top(const translit_unsupported_form *forms, unsigned form_count,
                                     uint64_t top_key[8], uint64_t top_count[8]);
#endif

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

// Shared constant-JCC IBTC entry has already saved guest RAX in cpu->r[0] and
// uses live RAX only to hand over the constant target.  Spill every remaining
// architectural register without overwriting that canonical saved value.
static void translit_emit_spill_after_rax(hl_x64_asm *a, uint8_t **rsp_canonical,
                                          uint8_t **flags_canonical) {
    static const int order[14] = {1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15};
    for (int i = 0; i < 14; i++)
        hl_x64_store_gs(a, order[i], (int32_t)(order[i] * 8));
    hl_x64_store_gs(a, HL_X64_RSP, 4 * 8);
    hl_x64_load_gs(a, HL_X64_RSP, OFF_HSP);
    *rsp_canonical = a->cursor;
    hl_x64_pushfq(a);
    hl_x64_pop(a, HL_X64_RAX);
    hl_x64_store_gs(a, HL_X64_RAX, TL_MM_FLAGS);
    *flags_canonical = a->cursor;
    hl_x64_cld(a);
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

// Reload the canonical image produced by translit_emit_spill without creating a second host frame. The
// ordering is the entry trampoline's ordering: flags before GPRs and guest RSP last. Every instruction
// stays in the JIT arena, so a fault after the final jump is attributed by the target block's ordinary
// provenance map; there is no out-of-cache trampoline window.
static void translit_emit_reload(hl_x64_asm *a) {
    hl_x64_push_gs(a, TL_MM_FLAGS);
    hl_x64_popfq(a);
    static const int order[15] = {0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15};
    for (int i = 0; i < 15; i++)
        hl_x64_load_gs(a, order[i], (int32_t)(order[i] * 8));
    hl_x64_load_gs(a, HL_X64_RSP, 4 * 8);
}

// Select only a target whose code was already mapped and published in the current immutable arena. The
// target's entire source hull must fit on the JCC's page: the source descriptor will own that whole page,
// so any SMC/invalidation that can stale the bypassed target also removes the source link.
static struct interp_block *translit_jcc_link_target(uint64_t source, uint64_t target) {
#if defined(HL_NATIVE_TEST_HOOKS)
    const char *disabled = hl_option_get("HL_TRANSLIT_JCC_LINK_DISABLE");
    if (disabled != NULL && disabled[0] != '0' && disabled[0] != 0) return NULL;
#endif
    uint64_t page = source & ~UINT64_C(0xfff);
    if (page > UINT64_MAX - UINT64_C(0x1000) || (target & ~UINT64_C(0xfff)) != page) return NULL;
    struct interp_block *target_block = (struct interp_block *)map_host(target);
    if (target_block == NULL || target_block->magic != INTERP_BLOCK_MAGIC ||
        target_block->generation != g_cache_gen || target_block->host_entry_off == 0 ||
        target_block->host_len == 0 || target_block->guest_start < page ||
        target_block->guest_end > page + UINT64_C(0x1000))
        return NULL;
    uintptr_t block_address = (uintptr_t)target_block;
    uintptr_t entry_address = block_address + target_block->host_entry_off;
    if (block_address < (uintptr_t)g_cache || entry_address < block_address || entry_address < (uintptr_t)g_cache ||
        entry_address > (uintptr_t)g_cp || target_block->host_len > (uintptr_t)g_cp - entry_address)
        return NULL;
    return target_block;
}

#if defined(HL_NATIVE_TEST_HOOKS)
/* Classify at source publication, before map_put makes this source visible.  The stored first refusal is
   immutable: run_block must never repeat map_host and accidentally credit a target published later. */
static uint8_t translit_would_link_classify(struct interp_block *source_block, const uint8_t *source_start,
                                            const uint8_t *source_end, uint64_t source, uint64_t target,
                                            int force_rel32_refusal) {
    uintptr_t source_start_rw = (uintptr_t)source_start, source_end_rw = (uintptr_t)source_end;
    void *source_block_rx = NULL, *source_rx_lo = NULL, *source_rx_last = NULL;
    uint64_t source_block_generation = UINT64_MAX, source_generation = UINT64_MAX;
    uint64_t source_last_generation = UINT64_MAX;
    if (source_block == NULL || source_start_rw == 0 || source_end_rw <= source_start_rw ||
        !jit_resolve_rw_code(source_block, &source_block_rx, &source_block_generation) ||
        !jit_resolve_rw_code((void *)source_start_rw, &source_rx_lo, &source_generation) ||
        !jit_resolve_rw_code((void *)(source_end_rw - 1), &source_rx_last, &source_last_generation))
        return HL_BACKEND_WOULD_LINK_SOURCE_UNRESOLVED;
    (void)source_block_rx;
    uint64_t page = source & ~UINT64_C(0xfff);
    if (page > UINT64_MAX - UINT64_C(0x1000) || (target & ~UINT64_C(0xfff)) != page)
        return HL_BACKEND_WOULD_LINK_CROSS_PAGE;
    struct interp_block *target_block = (struct interp_block *)map_host(target);
    if (target_block == NULL) return HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED;
    if (target_block->magic != INTERP_BLOCK_MAGIC)
        return HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED;
    void *target_rx = NULL;
    uint64_t target_generation = UINT64_MAX;
    if (!jit_resolve_rw_code(target_block, &target_rx, &target_generation))
        return HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED;
    if (source_block->generation != g_cache_gen || source_block_generation != g_cache_gen ||
        source_generation != g_cache_gen || source_last_generation != g_cache_gen ||
        target_generation != g_cache_gen || target_block->generation != g_cache_gen)
        return HL_BACKEND_WOULD_LINK_GENERATION;
    uintptr_t target_block_rx = (uintptr_t)target_rx;
    uintptr_t target_entry = target_block_rx + target_block->host_entry_off;
    uintptr_t target_block_rw = (uintptr_t)target_block;
    uintptr_t target_entry_rw = target_block_rw + target_block->host_entry_off;
    if (target_block->host_entry_off == 0 || target_block->host_len == 0 || target_entry < target_block_rx ||
        target_entry_rw < target_block_rw || target_block_rw < (uintptr_t)g_cache ||
        target_entry_rw > (uintptr_t)g_cp || target_block->host_len > (uintptr_t)g_cp - target_entry_rw)
        return HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED;
    if (target_block->guest_start < page || target_block->guest_end > page + UINT64_C(0x1000))
        return HL_BACKEND_WOULD_LINK_TARGET_PAGE;
    uintptr_t source_lo = (uintptr_t)source_rx_lo;
    uintptr_t source_hi = (uintptr_t)source_rx_last + 1;
    if (source_hi == 0 || force_rel32_refusal ||
        !interp_backend_shape_rel32_reachable(1, source_lo, source_hi, target_entry))
        return HL_BACKEND_WOULD_LINK_REL32;
    return HL_BACKEND_WOULD_LINK_ELIGIBLE;
}
#endif

static void translit_jcc_link_own_page(uint64_t source, uint64_t *lo, uint64_t *hi) {
    uint64_t page = source & ~UINT64_C(0xfff);
    if (page < *lo) *lo = page;
    if (page <= UINT64_MAX - UINT64_C(0x1000) && page + UINT64_C(0x1000) > *hi)
        *hi = page + UINT64_C(0x1000);
}

// The guest JCC has committed before this stub. Spill first (including RCX and the exact flags), then
// poll the async/STW/checkpoint kick without touching guest-visible state. A pending kick writes RIP and
// returns through the ordinary dispatcher boundary. The clear path reloads the exact image and jumps to
// an already-published block; cpu->rip is deliberately NOT changed on that path, so a signal/fault uses
// the target code's instruction provenance rather than a premature architectural store.
static void translit_emit_jcc_link(hl_x64_asm *a, struct interp_block *target_block, uint64_t target) {
    translit_emit_spill(a);
    hl_x64_cmp_gs_imm8(a, OFF_IRQ, 0);
    uint8_t *irq = hl_x64_jcc_rel32(a, 5); // JNE: dispatcher fallback
#if defined(HL_NATIVE_TEST_HOOKS)
    hl_x64_atomic_inc_abs(a, (uintptr_t)&translit_jcc_link_taken);
    uintptr_t shape_counter = hl_backend_tree_jcc_link_counter_address();
    if (shape_counter != 0) hl_x64_atomic_inc_abs(a, shape_counter);
#endif
    translit_emit_reload(a);
    hl_x64_jmp_rel32(a, (uint8_t *)target_block + target_block->host_entry_off);
    hl_x64_patch_rel32(a, irq, a->cursor);
#if defined(HL_NATIVE_TEST_HOOKS)
    hl_x64_atomic_inc_abs(a, (uintptr_t)&translit_jcc_link_irq_fallback);
#endif
    hl_x64_mov_imm64(a, HL_X64_RAX, target);
    hl_x64_store_gs(a, HL_X64_RAX, OFF_RIP);
    translit_emit_return(a, R_BRANCH);
}

static void translit_emit_jcc_dispatcher(hl_x64_asm *a, uint64_t target) {
    translit_emit_spill(a);
#if defined(HL_NATIVE_TEST_HOOKS)
    hl_x64_atomic_inc_abs(a, (uintptr_t)&translit_jcc_link_dispatcher);
#endif
    hl_x64_mov_imm64(a, HL_X64_RAX, target);
    hl_x64_store_gs(a, HL_X64_RAX, OFF_RIP);
    translit_emit_return(a, R_BRANCH);
}

enum { TL_JCC_IBTC_MISS = 1 };

#define TL_JCC_IBTC_STUB_MAGIC UINT64_C(0x4a43434942544331) // "JCCIBTC1"
typedef struct {
    uint64_t magic;
    uint32_t entry_off;
    uint32_t rsp_canonical_off;
    uint32_t flags_canonical_off;
    uint32_t end_off;
} translit_jcc_ibtc_stub_header;

static uint8_t *translit_jcc_ibtc_stub_entry;
static uint8_t *translit_jcc_ibtc_stub_rsp_canonical;
static uint8_t *translit_jcc_ibtc_stub_flags_canonical;
static uint8_t *translit_jcc_ibtc_stub_end;

typedef struct {
    uint8_t *save_end;
    uint8_t *target_end;
    uint8_t *rip_end;
    uint8_t *end;
} translit_jcc_ibtc_site_ranges;

#if defined(HL_NATIVE_TEST_HOOKS)
static uint8_t *translit_test_jcc_ibtc_instruction_start;
static translit_jcc_ibtc_site_ranges translit_test_jcc_ibtc_site;
static uint64_t translit_test_jcc_ibtc_source_guest;
#endif

static int translit_jcc_ibtc_candidate(uint64_t source, uint64_t target) {
    return translit_jcc_ibtc_stub_entry != NULL && (source >> 12) == (target >> 12);
}

// One generation-owned shared probe.  Every source hands it a target in RAX
// after saving guest RAX and publishing the already-committed architectural
// target in cpu->rip.  Keeping the large spill/probe/reload sequence out of each
// descriptor is load-bearing: eight legal stitched JCCs must fit inside the
// unchanged 3072-byte block budget.
static int translit_jcc_ibtc_stub_init(void) {
    translit_jcc_ibtc_stub_header *header = (translit_jcc_ibtc_stub_header *)g_cache;
    if (g_cp != g_cache && header->magic == TL_JCC_IBTC_STUB_MAGIC &&
        header->entry_off >= sizeof *header && header->entry_off < header->rsp_canonical_off &&
        header->rsp_canonical_off < header->flags_canonical_off &&
        header->flags_canonical_off < header->end_off && header->end_off <= CACHE_EMIT_HEADROOM) {
        translit_jcc_ibtc_stub_entry = g_cache + header->entry_off;
        translit_jcc_ibtc_stub_rsp_canonical = g_cache + header->rsp_canonical_off;
        translit_jcc_ibtc_stub_flags_canonical = g_cache + header->flags_canonical_off;
        translit_jcc_ibtc_stub_end = g_cache + header->end_off;
        return 1;
    }
    if (g_cp != g_cache) return 0;

    // A failed first attempt must never leave a stale pointer from the previous
    // generation eligible at a new source. The same-ISA backend's pcache_load
    // is a categorical MISS, so these bytes are never restored without their
    // generation-owned provenance. The offset-only header nevertheless makes
    // fork-preserve rebinding independent of process-local pointers.
    translit_jcc_ibtc_stub_entry = NULL;
    translit_jcc_ibtc_stub_rsp_canonical = NULL;
    translit_jcc_ibtc_stub_flags_canonical = NULL;
    translit_jcc_ibtc_stub_end = NULL;

    uint32_t owner_token;
    if (!jit_body_owner_reserve_n(g_cache_gen, 3, &owner_token)) return 0;
    uint8_t *entry = g_cache + ((sizeof *header + 15u) & ~15u);
    hl_x64_asm a = {entry, g_cache + CACHE_EMIT_HEADROOM, 0};
    uint8_t *rsp_canonical = NULL, *flags_canonical = NULL;
    translit_emit_spill_after_rax(&a, &rsp_canonical, &flags_canonical);
    hl_x64_cmp_gs_imm8(&a, OFF_IRQ, 0);
    uint8_t *irq = hl_x64_jcc_rel32(&a, 5);
    hl_x64_load_gs(&a, HL_X64_RAX, OFF_IBTC);
    // RAX no longer carries the target; the source published it in cpu->rip.
    // Hashing remains the established direct-map formula, so load the exact
    // slot selected by the target constant at each source.  A shared stub cannot
    // bake that displacement: compute it from target in RCX.
    hl_x64_load_gs(&a, HL_X64_RCX, OFF_RIP);
    hl_x64_mov_reg(&a, HL_X64_RDX, HL_X64_RCX);
    hl_x64_shr_imm8(&a, HL_X64_RDX, 2);
    hl_x64_and_imm32(&a, HL_X64_RDX, IBTC_N - 1);
    hl_x64_shl_imm8(&a, HL_X64_RDX, 4);
    hl_x64_add_reg(&a, HL_X64_RAX, HL_X64_RDX);
    hl_x64_load_xmm_aligned_disp32(&a, 0, HL_X64_RAX, 0);
    hl_x64_movq_xmm_to_reg(&a, HL_X64_RDX, 0);
    hl_x64_cmp_reg(&a, HL_X64_RDX, HL_X64_RCX);
    uint8_t *miss_key = hl_x64_jcc_rel32(&a, 5);
    hl_x64_psrldq(&a, 0, 8);
    hl_x64_movq_xmm_to_reg(&a, HL_X64_RAX, 0);
    hl_x64_test_reg(&a, HL_X64_RAX);
    uint8_t *miss_body = hl_x64_jcc_rel32(&a, 4);
    hl_x64_store_gs(&a, HL_X64_RAX, TL_MM_ENTRY);
    uintptr_t hit_counter = translit_jcc_ibtc_dynamic_counter(TL_JCC_IBTC_COUNT_HIT);
    if (hit_counter != 0) hl_x64_atomic_inc_abs(&a, hit_counter);
#if defined(HL_NATIVE_TEST_HOOKS)
    uintptr_t shape_counter = hl_backend_tree_jcc_link_counter_address();
    if (shape_counter != 0) hl_x64_atomic_inc_abs(&a, shape_counter);
#endif
    translit_emit_reload(&a);
    hl_x64_jmp_gs_indirect(&a, TL_MM_ENTRY);

    hl_x64_patch_rel32(&a, miss_key, a.cursor);
    hl_x64_patch_rel32(&a, miss_body, a.cursor);
    uintptr_t miss_counter = translit_jcc_ibtc_dynamic_counter(TL_JCC_IBTC_COUNT_MISS);
    if (miss_counter != 0) hl_x64_atomic_inc_abs(&a, miss_counter);
    hl_x64_store_gs_imm32(&a, TL_JCC_IBTC_MISS, OFF_JCC_IBTC_MISS);
    translit_emit_return(&a, R_BRANCH); // cpu->rip already names the target

    hl_x64_patch_rel32(&a, irq, a.cursor);
    uintptr_t irq_counter = translit_jcc_ibtc_dynamic_counter(TL_JCC_IBTC_COUNT_IRQ);
    if (irq_counter != 0) hl_x64_atomic_inc_abs(&a, irq_counter);
    translit_emit_return(&a, R_BRANCH);
    if (a.overflow) return 0;

    jit_body_owner_range ranges[3] = {
        {(uint64_t)(uintptr_t)entry, (uint64_t)(uintptr_t)rsp_canonical,
         JIT_BODY_OWNER_DYNAMIC_RIP, 1u << HL_X64_RAX},
        {(uint64_t)(uintptr_t)rsp_canonical, (uint64_t)(uintptr_t)flags_canonical,
         JIT_BODY_OWNER_DYNAMIC_RIP, (1u << HL_X64_RAX) | (1u << HL_X64_RSP)},
        {(uint64_t)(uintptr_t)flags_canonical, (uint64_t)(uintptr_t)a.cursor,
         JIT_BODY_OWNER_DYNAMIC_RIP_FLAGS, UINT16_MAX},
    };
    if (!jit_body_owner_publish_n(g_cache_gen, owner_token, ranges, 3)) return 0;
    for (unsigned i = 0; i < 3; i++)
        jit_instruction_map_put_preserve(ranges[i].lo, ranges[i].hi, ranges[i].guest,
                                         ranges[i].preserve_registers);
    *header = (translit_jcc_ibtc_stub_header){
        TL_JCC_IBTC_STUB_MAGIC,
        (uint32_t)(entry - g_cache),
        (uint32_t)(rsp_canonical - g_cache),
        (uint32_t)(flags_canonical - g_cache),
        (uint32_t)(a.cursor - g_cache),
    };
    g_cp = a.cursor;
    translit_jcc_ibtc_stub_entry = entry;
    translit_jcc_ibtc_stub_rsp_canonical = rsp_canonical;
    translit_jcc_ibtc_stub_flags_canonical = flags_canonical;
    translit_jcc_ibtc_stub_end = a.cursor;
    return 1;
}

// Immutable unresolved taken-JCC source handoff.  The typed OFF option still
// suppresses only publication, so ON and OFF execute byte-identical bytes.
static translit_jcc_ibtc_site_ranges translit_emit_jcc_ibtc(hl_x64_asm *a, uint64_t target) {
    translit_jcc_ibtc_site_ranges ranges = {0};
    hl_x64_store_gs(a, HL_X64_RAX, R_OFF(HL_X64_RAX));
    ranges.save_end = a->cursor;
    hl_x64_mov_imm64(a, HL_X64_RAX, target);
    ranges.target_end = a->cursor;
    hl_x64_store_gs(a, HL_X64_RAX, OFF_RIP);
    ranges.rip_end = a->cursor;
    hl_x64_jmp_rel32(a, translit_jcc_ibtc_stub_entry);
    ranges.end = a->cursor;
    return ranges;
}

// Runs at the existing post-map dispatcher seam. Under threads the caller still holds g_jit_lock; the
// pair publish remains atomic because emitted readers do not take that lock. The RX entry is derived from
// the current descriptor and generation, never from a persisted or inherited heap/code pointer.
static void translit_jcc_ibtc_fill(struct cpu *cpu, void *code) {
    if (cpu->jcc_ibtc_miss != TL_JCC_IBTC_MISS) {
        cpu->jcc_ibtc_miss = 0;
        return;
    }
    cpu->jcc_ibtc_miss = 0;
    if (hl_option_get("HL_TRANSLIT_JCC_IBTC_DISABLE") != NULL) {
        translit_jcc_ibtc_count(TL_JCC_IBTC_COUNT_SUPPRESSED, 1);
        return;
    }
    struct interp_block *block = code;
    void *rx = NULL;
    uint64_t generation = UINT64_MAX;
    if (block == NULL || block->magic != INTERP_BLOCK_MAGIC || block->gpc != cpu->rip ||
        block->generation != g_cache_gen || block->host_entry_off == 0 || block->host_len == 0 ||
        !jit_resolve_rw_code(block, &rx, &generation) || generation != g_cache_gen) {
        translit_jcc_ibtc_count(TL_JCC_IBTC_COUNT_INVALID_REFUSAL, 1);
        return;
    }
    uintptr_t entry_address = (uintptr_t)rx + block->host_entry_off;
    if (entry_address < (uintptr_t)rx) {
        translit_jcc_ibtc_count(TL_JCC_IBTC_COUNT_INVALID_REFUSAL, 1);
        return;
    }
    ibtc_ent *entry = &g_ibtc[ibtc_index(cpu->rip)];
#if defined(HL_NATIVE_TEST_HOOKS)
    ibtc_ent previous = ibtc_snapshot(entry);
    if (previous.target != 0 && previous.target != cpu->rip)
        (void)__atomic_fetch_add(&translit_jcc_ibtc_collision, 1, __ATOMIC_RELAXED);
#endif
    ibtc_publish(entry, cpu->rip, (void *)entry_address);
    translit_jcc_ibtc_count(TL_JCC_IBTC_COUNT_FILL, 1);
}

// Every dynamic miss is consumed once by fill/suppress/refusal. A fatal or ordinary teardown can interrupt
// between the emitted miss and the dispatcher seam; classify that still-typed miss as an invalid refusal
// before the process publishes its final lifecycle state, preserving misses=fills+suppressed+refusals.
static void translit_jcc_ibtc_finalize_cpu(struct cpu *cpu) {
    if (cpu != NULL && cpu->jcc_ibtc_miss == TL_JCC_IBTC_MISS) {
        cpu->jcc_ibtc_miss = 0;
        translit_jcc_ibtc_count(TL_JCC_IBTC_COUNT_INVALID_REFUSAL, 1);
    }
}

enum {
    TL_MIXED_PROFILE_MAGIC = 0x4d530000u, // "MS", version 0
    TL_MIXED_PROFILE_MAGIC_MASK = 0xffff0000u,
    TL_MIXED_PROFILE_TRANSITIONS_MASK = 0xffu,
    TL_MIXED_PROFILE_DISABLED_BOUNDARY = 1u << 8,
};

_Static_assert(TL_MAX_INSNS <= TL_MIXED_PROFILE_TRANSITIONS_MASK,
               "profile marker must represent every mixed transition in one block");
_Static_assert(TL_MAX_OWNER_RANGES == JIT_BODY_OWNER_BLOCK_HEADROOM,
               "dispatcher owner rotation headroom must cover one worst-case block");

// Production diagnostics need execution proof without growing the stable descriptor. Diagnostic builds
// emit this flag-neutral GS store into existing CPU alignment padding; every terminal pays the same store,
// while the value says whether this completed body crossed a mixed boundary or stopped at typed OFF.
static void translit_mixed_profile_terminal(hl_x64_asm *a, uint64_t transitions, int disabled_boundary) {
    if (g_prof) {
        uint32_t marker = TL_MIXED_PROFILE_MAGIC |
                          ((uint32_t)transitions & TL_MIXED_PROFILE_TRANSITIONS_MASK) |
                          (disabled_boundary ? TL_MIXED_PROFILE_DISABLED_BOUNDARY : 0u);
        hl_x64_store_gs_imm32(a, (int32_t)marker, OFF_MIXED_PROFILE);
    }
}

static void translit_mixed_profile_completed(uint32_t marker) {
    if ((marker & TL_MIXED_PROFILE_MAGIC_MASK) != TL_MIXED_PROFILE_MAGIC) return;
    uint64_t transitions = marker & TL_MIXED_PROFILE_TRANSITIONS_MASK;
    int disabled_boundary = (marker & TL_MIXED_PROFILE_DISABLED_BOUNDARY) != 0;
    hl_backend_tree_mixed_sse_completed(transitions, disabled_boundary);
    if (disabled_boundary) {
        if (transitions == 0)
            (void)__atomic_fetch_add(&translit_mixed_sse_disabled_boundaries, 1, __ATOMIC_RELAXED);
        return;
    }
    if (transitions == 0) return;
    (void)__atomic_fetch_add(&translit_mixed_sse_executed, 1, __ATOMIC_RELAXED);
    (void)__atomic_fetch_add(&translit_mixed_sse_executed_transitions, transitions, __ATOMIC_RELAXED);
}

enum {
    TL_SHAPE_KIND_SHIFT = 0,
    TL_SHAPE_COND_FALL_SHIFT = 4,
    TL_SHAPE_JMP_SHIFT = 8,
    TL_SHAPE_SOURCE_PAGE_SHIFT = 12,
    TL_SHAPE_WOULD_FAMILY_SHIFT = 20,
    TL_SHAPE_WOULD_DISPOSITION_SHIFT = 22,
    TL_SHAPE_FALL_STOP_SHIFT = 25,
    TL_SHAPE_KIND_MASK = 0xf,
    TL_SHAPE_STITCH_MASK = 0xf,
    TL_SHAPE_SOURCE_PAGE_MASK = 0xff,
    TL_SHAPE_WOULD_FAMILY_MASK = 0x3,
    TL_SHAPE_WOULD_DISPOSITION_MASK = 0x7,
    TL_SHAPE_FALL_STOP_MASK = 0x7f,
};

_Static_assert(TL_STITCH_MAX_JMPS <= TL_SHAPE_STITCH_MASK,
               "hook marker must represent every admitted stitch");
#if defined(HL_NATIVE_TEST_HOOKS)
_Static_assert(HL_BACKEND_SHAPE_T_COUNT <= TL_SHAPE_KIND_MASK + 1,
               "hook marker must represent every translated terminator");
_Static_assert(HL_BACKEND_WOULD_LINK_FAMILY_COUNT <= TL_SHAPE_WOULD_FAMILY_MASK,
               "hook marker reserves its final family value for no would-link terminal");
_Static_assert(HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT <= TL_SHAPE_WOULD_DISPOSITION_MASK + 1,
               "hook marker must represent every would-link disposition");
_Static_assert(HL_BACKEND_FALL_COUNT <= TL_SHAPE_FALL_STOP_MASK + 1,
               "hook marker must represent every fall-stop reason");
_Static_assert(TL_SHAPE_KIND_SHIFT + 4 <= TL_SHAPE_COND_FALL_SHIFT &&
                   TL_SHAPE_COND_FALL_SHIFT + 4 <= TL_SHAPE_JMP_SHIFT &&
                   TL_SHAPE_JMP_SHIFT + 4 <= TL_SHAPE_SOURCE_PAGE_SHIFT &&
                   TL_SHAPE_SOURCE_PAGE_SHIFT + 8 <= TL_SHAPE_WOULD_FAMILY_SHIFT &&
                   TL_SHAPE_WOULD_FAMILY_SHIFT + 2 <= TL_SHAPE_WOULD_DISPOSITION_SHIFT &&
                   TL_SHAPE_WOULD_DISPOSITION_SHIFT + 3 <= TL_SHAPE_FALL_STOP_SHIFT &&
                   TL_SHAPE_FALL_STOP_SHIFT + 7 == 32,
               "hook marker fields must be disjoint and fill exactly 32 bits");
#endif

static uint8_t *translit_shape_exit(hl_x64_asm *a, unsigned kind, unsigned stitched_jmp,
                                    unsigned stitched_cond_fall, unsigned source_page_delta) {
#if defined(HL_NATIVE_TEST_HOOKS)
    /* pc is monotonic: sequential decode can advance at most one page per instruction, while admitted
       JMP stitching stays forward on its current page. TL_MAX_INSNS therefore bounds this immutable
       source-page delta to the marker's byte. The remaining high bits carry the emitted terminal owner's
       immutable publication classification, so a linked JCC ingress cannot attribute the target's exit to
       the dispatcher-entry descriptor. run_block combines the page delta with block->gpc's page; admitted
       JCC links require both descriptors on that same page. */
    uint32_t packed = (kind & TL_SHAPE_KIND_MASK) << TL_SHAPE_KIND_SHIFT |
                      (stitched_cond_fall & TL_SHAPE_STITCH_MASK) << TL_SHAPE_COND_FALL_SHIFT |
                      (stitched_jmp & TL_SHAPE_STITCH_MASK) << TL_SHAPE_JMP_SHIFT |
                      (source_page_delta & TL_SHAPE_SOURCE_PAGE_MASK) << TL_SHAPE_SOURCE_PAGE_SHIFT |
                      (HL_BACKEND_WOULD_LINK_FAMILY_COUNT & TL_SHAPE_WOULD_FAMILY_MASK)
                          << TL_SHAPE_WOULD_FAMILY_SHIFT;
    hl_x64_store_gs_imm32(a, (int32_t)packed, OFF_BACKEND_SHAPE);
    return a->overflow ? NULL : a->cursor - sizeof(uint32_t);
#else
    (void)a;
    (void)kind;
    (void)stitched_jmp;
    (void)stitched_cond_fall;
    (void)source_page_delta;
    return NULL;
#endif
}

#if defined(HL_NATIVE_TEST_HOOKS)
static void translit_shape_would_link_patch(uint8_t *marker, unsigned family, unsigned disposition) {
    if (marker == NULL || family >= HL_BACKEND_WOULD_LINK_FAMILY_COUNT ||
        disposition >= HL_BACKEND_WOULD_LINK_DISPOSITION_COUNT)
        return;
    uint32_t packed = (uint32_t)marker[0] | (uint32_t)marker[1] << 8 | (uint32_t)marker[2] << 16 |
                      (uint32_t)marker[3] << 24;
    packed &= ~((uint32_t)TL_SHAPE_WOULD_FAMILY_MASK << TL_SHAPE_WOULD_FAMILY_SHIFT |
                (uint32_t)TL_SHAPE_WOULD_DISPOSITION_MASK << TL_SHAPE_WOULD_DISPOSITION_SHIFT);
    packed |= (family & TL_SHAPE_WOULD_FAMILY_MASK) << TL_SHAPE_WOULD_FAMILY_SHIFT |
              (disposition & TL_SHAPE_WOULD_DISPOSITION_MASK) << TL_SHAPE_WOULD_DISPOSITION_SHIFT;
    marker[0] = (uint8_t)packed;
    marker[1] = (uint8_t)(packed >> 8);
    marker[2] = (uint8_t)(packed >> 16);
    marker[3] = (uint8_t)(packed >> 24);
}

static uint8_t *translit_shape_fall_exit(hl_x64_asm *a, unsigned stitched_jmp,
                                         unsigned stitched_cond_fall, unsigned source_page_delta,
                                         unsigned stop_reason) {
    uint8_t *marker = translit_shape_exit(a, HL_BACKEND_SHAPE_T_FALLTHROUGH, stitched_jmp,
                                          stitched_cond_fall, source_page_delta);
    if (marker == NULL) return NULL;
    uint32_t packed = (uint32_t)marker[0] | (uint32_t)marker[1] << 8 | (uint32_t)marker[2] << 16 |
                      (uint32_t)marker[3] << 24;
    packed &= ~((uint32_t)TL_SHAPE_FALL_STOP_MASK << TL_SHAPE_FALL_STOP_SHIFT);
    packed |= (stop_reason & TL_SHAPE_FALL_STOP_MASK) << TL_SHAPE_FALL_STOP_SHIFT;
    marker[0] = (uint8_t)packed;
    marker[1] = (uint8_t)(packed >> 8);
    marker[2] = (uint8_t)(packed >> 16);
    marker[3] = (uint8_t)(packed >> 24);
    return marker;
}
#else
#define translit_shape_fall_exit(a, stitched_jmp, stitched_cond_fall, source_page_delta, stop_reason) \
    translit_shape_exit((a), HL_BACKEND_SHAPE_T_FALLTHROUGH, (stitched_jmp),                      \
                        (stitched_cond_fall), (source_page_delta))
#endif

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

static int translit_owner_append(jit_body_owner_range ranges[TL_MAX_OWNER_RANGES], uint32_t *count,
                                 const uint8_t *lo, const uint8_t *hi, uint64_t guest,
                                 uint32_t preserve_registers) {
    if (hi <= lo) return 1; // a stitched direct jump can advance guest PC without emitting host bytes
    if (*count >= TL_MAX_OWNER_RANGES) return 0;
    ranges[(*count)++] = (jit_body_owner_range){(uint64_t)(uintptr_t)lo, (uint64_t)(uintptr_t)hi,
                                                guest, preserve_registers};
    return 1;
}

// Ordinary copied instructions use the exact staged instruction map for normal fault provenance. Their
// persistent fallback owner may cover one maximal contiguous zero-preserve span, matching the historical
// block-granular fallback without consuming one 16-byte sidecar entry per one-byte NOP. The caller clears
// `pending` at every semantic boundary (SSE commit, scratch interval, stitch side region and terminal).
static int translit_owner_append_normal(jit_body_owner_range ranges[TL_MAX_OWNER_RANGES], uint32_t *count,
                                        int *pending, const uint8_t *lo, const uint8_t *hi,
                                        uint64_t guest) {
    if (hi <= lo) return 1;
    if (*pending && *count != 0 && ranges[*count - 1].hi == (uint64_t)(uintptr_t)lo) {
        ranges[*count - 1].hi = (uint64_t)(uintptr_t)hi;
        return 1;
    }
    if (!translit_owner_append(ranges, count, lo, hi, guest, 0)) return 0;
    *pending = 1;
    return 1;
}

static int translit_owner_append_ibtc(jit_body_owner_range ranges[TL_MAX_OWNER_RANGES], uint32_t *count,
                                      const uint8_t *instruction_start, const uint8_t *instruction_end,
                                      const translit_jcc_ibtc_site_ranges *site, uint64_t guest) {
#if defined(HL_NATIVE_TEST_HOOKS)
    translit_test_jcc_ibtc_instruction_start = (uint8_t *)instruction_start;
    translit_test_jcc_ibtc_site = *site;
    translit_test_jcc_ibtc_source_guest = guest;
#endif
    return translit_owner_append(ranges, count, instruction_start, site->save_end, guest, 0) &&
           translit_owner_append(ranges, count, site->save_end, site->rip_end, guest,
                                 1u << HL_X64_RAX) &&
           translit_owner_append(ranges, count, site->rip_end, site->end,
                                 JIT_BODY_OWNER_DYNAMIC_RIP, 1u << HL_X64_RAX) &&
           translit_owner_append(ranges, count, site->end, instruction_end, guest, 0);
}

static void translit_instruction_ranges_publish(const jit_body_owner_range *ranges, uint32_t count) {
    for (uint32_t i = 0; i < count; i++)
        jit_instruction_map_put_preserve(ranges[i].lo, ranges[i].hi, ranges[i].guest,
                                         ranges[i].preserve_registers);
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
typedef struct {
    struct insn insn;
    uint64_t pc;
    _Atomic uint32_t done;
} translit_unsupported_record_test;

static void *translit_unsupported_record_thread(void *opaque) {
    translit_unsupported_record_test *test = opaque;
    translit_unsupported_record(&test->insn, test->pc);
    atomic_store_explicit(&test->done, 1, memory_order_release);
    return NULL;
}

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
        uint32_t preserve = 0;
        jit_body_owner_drop_generation(generation);
        jit_body_owner_set *set = jit_body_owner_set_for(generation, 1);
        if (set == NULL) return 49;
        set->rw = (uint8_t *)(uintptr_t)rw;
        set->rw2rx = CACHE_SZ * 2u;
        if (!jit_body_owner_reserve(generation, &token) ||
            !jit_body_owner_publish(generation, token, rw, rw + 8, 0))
            return 49;
        jit_body_owner_range range[] = {{rw + 0x10, rw + 0x18, 1, 1u << 10},
                                        {rw + 0x18, rw + 0x20, 2, 1u << 11},
                                        {rw + 0x20, rw + 0x28, 3, 0}};
        int ok = jit_body_owner_reserve_n(generation, 3, &token) && token == 1 &&
                 jit_body_owner_publish_n(generation, token, range, 3) &&
                 jit_body_owner_lookup_preserve(rw + 0x17, &owner, &preserve) && owner == 1 &&
                 preserve == (1u << 10) &&
                 jit_body_owner_lookup_preserve(rw + CACHE_SZ * 2u + 0x18, &owner, &preserve) && owner == 2 &&
                 preserve == (1u << 11) && jit_body_owner_lookup_preserve(rw + 0x27, &owner, &preserve) &&
                 owner == 3 && preserve == 0 &&
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
        jit_body_owner_range unsorted[] = {{rw + 0x20, rw + 0x28, 1, 0}, {rw + 0x10, rw + 0x18, 2, 0}};
        jit_body_owner_range overlap[] = {{rw + 0x10, rw + 0x20, 1, 0}, {rw + 0x18, rw + 0x28, 2, 0}};
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
            generation, token, {{rw + 0x10, rw + 0x18, 1, 0}, {rw + 0x18, rw + 0x20, 2, 0},
                                {rw + 0x20, rw + 0x28, 3, 0}}};
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
        jit_body_owner_range range[] = {{rw + 0x10, rw + 0x18, 1, 0}, {rw + 0x18, rw + 0x20, 2, 0}};
        int rejected = !jit_body_owner_publish_n(generation, token, range, 2);
        int unchanged = atomic_load_explicit(&set->count, memory_order_acquire) == token;
        jit_body_owner_drop_generation(generation);
        return rejected && unchanged ? 0 : 52;
    }
    if (scenario == 39) {
        static const uint8_t admitted[][6] = {{0x66, 0x0F, 0xDB, 0xC1}, {0x66, 0x0F, 0xDF, 0xC1},
                                               {0x66, 0x0F, 0x6C, 0xC1}, {0x66, 0x45, 0x0F, 0x6C, 0xC1},
                                               {0x66, 0x0F, 0x6E, 0xC1}, {0x66, 0x4D, 0x0F, 0x6E, 0xD9},
                                               {0x66, 0x0F, 0xEF, 0xC1}, {0x66, 0x45, 0x0F, 0xEF, 0xC1},
                                               {0x66, 0x0F, 0x6F, 0x00}, {0xF3, 0x0F, 0x6F, 0x01},
                                               {0x0F, 0x10, 0xC1}, {0x0F, 0x28, 0x01}, {0x0F, 0x12, 0xC1},
                                               {0x45, 0x0F, 0x12, 0xFF},
                                               {0x66, 0x0F, 0x73, 0xDA, 0}, {0x66, 0x0F, 0xEB, 0xC1},
                                               {0x66, 0x0F, 0x7E, 0xC0}, {0xF3, 0x0F, 0x7F, 0x00},
                                               {0x0F, 0x11, 0x00}, {0x0F, 0x29, 0x00},
                                               {0x44, 0x0F, 0x11, 0x4F, 1}, {0x44, 0x0F, 0x29, 0x0C, 0x24},
                                               {0xF3, 0x44, 0x0F, 0x7F, 0x4F, 0x13}};
        static const uint8_t refused[][6] = {{0xF0, 0x66, 0x0F, 0xDB, 0xC1}, // LOCK
                                              {0xF0, 0x66, 0x0F, 0x6C, 0xC1},
                                              {0xF0, 0x66, 0x0F, 0x6E, 0xC1},
                                              {0xF2, 0x0F, 0xDB, 0xC1},       // wrong mandatory prefix
                                              {0xF3, 0x0F, 0x6C, 0xC1},
                                              {0xF3, 0x0F, 0x6E, 0xC1},
                                              {0xF2, 0x0F, 0x6F, 0xC1},
                                              {0x67, 0x66, 0x0F, 0xEB, 0xC1}, // address-size override
                                              {0x66, 0x0F, 0x73, 0x18, 1},    // group form requires mod=3
                                              {0x66, 0x0F, 0xDB, 0x00},       // aligned-memory contract deferred
                                              {0x66, 0x0F, 0x6C, 0x00},       // PUNPCKLQDQ memory source deferred
                                              {0x66, 0x0F, 0x6E, 0x00},       // MOVD/Q memory source deferred
                                              {0x66, 0x0F, 0xEF, 0x00},       // PXOR memory source deferred
                                              {0xF3, 0x0F, 0xEF, 0xC1},       // wrong mandatory prefix
                                              {0x0F, 0x12, 0x00},             // MOVLPS memory form
                                              {0x66, 0x0F, 0x12, 0xC1},       // wrong MOVHLPS prefix
                                              {0xF2, 0x0F, 0x12, 0xC1}, {0xF3, 0x0F, 0x12, 0xC1},
                                              {0x66, 0x0F, 0x7E, 0x00},       // memory store deferred
                                              {0x66, 0x0F, 0xDF, 0x00},       // PANDN memory source deferred
                                              {0xF3, 0x0F, 0x7F, 0xC0},       // MOVDQU store requires memory
                                              {0x0F, 0x11, 0xC0},             // MOVUPS store requires memory
                                              {0x0F, 0x29, 0xC0}};            // MOVAPS store requires memory
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
    if (scenario == 41) {
#if defined(HL_HOST_CPU_X86_64)
        int saved_option = translit_selected;
        int saved_shared = filemap_emulated_shared_active();
        translit_selected = 1;
        filemap_test_set_emulated_shared(0);
        int admitted_without_observer = translit_image_ok();
        filemap_test_set_emulated_shared(1);
        int refused_with_observer = !translit_image_ok();
        filemap_test_set_emulated_shared(saved_shared);
        translit_selected = saved_option;
        return admitted_without_observer && refused_with_observer ? 0 : 55;
#else
        return 0;
#endif
    }
    if (scenario == 42) {
        translit_unsupported_form forms[3] = {0};
        atomic_store_explicit(&forms[0].state, 2, memory_order_relaxed);
        atomic_store_explicit(&forms[1].state, 2, memory_order_relaxed);
        atomic_store_explicit(&forms[2].state, 2, memory_order_relaxed);
        forms[0].key = 0x30;
        forms[1].key = 0x10;
        forms[2].key = 0x20;
        atomic_store_explicit(&forms[0].count, 2, memory_order_relaxed);
        atomic_store_explicit(&forms[1].count, 5, memory_order_relaxed);
        atomic_store_explicit(&forms[2].count, 3, memory_order_relaxed);
        uint64_t keys[8] = {0}, counts[8] = {0};
        translit_unsupported_top(forms, 3, keys, counts);
        return keys[0] == 0x10 && counts[0] == 5 && keys[1] == 0x20 && counts[1] == 3 &&
                       keys[2] == 0x30 && counts[2] == 2
                   ? 0
                   : 56;
    }
    if (scenario == 43) {
        struct insn forms[20] = {0};
        for (unsigned i = 0; i < 20; i++) forms[i].op = 0x12;
        forms[1].p66 = 1;
        forms[2].rep = 1;
        forms[3].repne = 1;
        forms[4].lock = 1;
        forms[5].addr32 = 1;
        forms[6].seg = 2;
        forms[7].has_rex = forms[7].rexW = 1;
        forms[8].has_rex = forms[8].rexR = 1;
        forms[9].has_rex = forms[9].rexX = 1;
        forms[10].has_rex = forms[10].rexB = 1;
        forms[11].has_modrm = 1;
        forms[11].modrm = 0xc1;
        forms[12].opsize = 8;
        forms[13].vex = 1;
        forms[13].vex_map = 2;
        forms[13].vex_pp = 3;
        forms[14].vex = 1;
        forms[14].vex_l = 2;
        forms[14].vex_w = 1;
        forms[15].vex = 1;
        forms[15].vvvv = 17;
        forms[16].vex = forms[16].evex = 1;
        forms[16].evex_mask = 5;
        forms[17].vex = forms[17].evex = 1;
        forms[17].evex_z = 1;
        forms[18].vex = forms[18].evex = 1;
        forms[18].evex_b = 1;
        forms[19] = forms[13];
        forms[19].vex_map = 10;
        uint64_t keys[20];
        for (unsigned i = 0; i < 20; i++) keys[i] = translit_unsupported_key(&forms[i]);
        for (unsigned i = 0; i < 20; i++)
            for (unsigned j = i + 1; j < 20; j++)
                if (keys[i] == keys[j]) return 57;
        return 0;
    }
    if (scenario == 44) {
        // Pause the winning publisher after the real CAS in each table. A second recorder of the same
        // key/site must wait for that slot rather than probing onward and publishing a duplicate.
        for (unsigned stage = 1; stage <= 2; stage++) {
            translit_unsupported_record_test first = {
                .insn = {.op = (uint8_t)(0xe0 + stage),
                         .vex = 1,
                         .vex_map = (uint8_t)(28 + stage),
                         .vex_pp = 3,
                         .has_modrm = 1,
                         .modrm = 0xc1,
                         .mod = 3},
                .pc = UINT64_C(0xfedcba9876500000) + stage,
            };
            translit_unsupported_record_test second = {.insn = first.insn, .pc = first.pc};
            uint64_t key = translit_unsupported_key(&first.insn);
            uint64_t keyed = atomic_load_explicit(&translit_unsupported_keyed, memory_order_relaxed);
            uint64_t form_unique = atomic_load_explicit(&translit_unsupported_form_unique, memory_order_relaxed);
            uint64_t site_unique = atomic_load_explicit(&translit_unsupported_site_unique, memory_order_relaxed);
            uint64_t repeats = atomic_load_explicit(&translit_unsupported_site_repeats, memory_order_relaxed);
            uint64_t form_overflow = atomic_load_explicit(&translit_unsupported_form_overflow, memory_order_relaxed);
            uint64_t site_overflow = atomic_load_explicit(&translit_unsupported_site_overflow, memory_order_relaxed);
            atomic_store_explicit(&translit_unsupported_pause_ready, 0, memory_order_relaxed);
            atomic_store_explicit(&translit_unsupported_pause_release, 0, memory_order_relaxed);
            atomic_store_explicit(&translit_unsupported_pause_stage, stage, memory_order_release);
            pthread_t publisher, waiter;
            if (pthread_create(&publisher, NULL, translit_unsupported_record_thread, &first) != 0) return 58;
            while (!atomic_load_explicit(&translit_unsupported_pause_ready, memory_order_acquire)) sched_yield();
            if (pthread_create(&waiter, NULL, translit_unsupported_record_thread, &second) != 0) {
                atomic_store_explicit(&translit_unsupported_pause_release, 1, memory_order_release);
                (void)pthread_join(publisher, NULL);
                return 58;
            }
            for (unsigned spin = 0; spin < 1000; spin++) sched_yield();
            if (atomic_load_explicit(&first.done, memory_order_acquire) ||
                atomic_load_explicit(&second.done, memory_order_acquire)) {
                atomic_store_explicit(&translit_unsupported_pause_release, 1, memory_order_release);
                (void)pthread_join(publisher, NULL);
                (void)pthread_join(waiter, NULL);
                return 58;
            }
            atomic_store_explicit(&translit_unsupported_pause_release, 1, memory_order_release);
            if (pthread_join(publisher, NULL) != 0 || pthread_join(waiter, NULL) != 0) return 58;
            atomic_store_explicit(&translit_unsupported_pause_stage, 0, memory_order_release);
            unsigned form_matches = 0, site_matches = 0;
            uint64_t form_count = 0, site_count = 0;
            for (unsigned i = 0; i < TL_UNSUPPORTED_FORM_N; i++)
                if (atomic_load_explicit(&translit_unsupported_forms[i].state, memory_order_acquire) == 2 &&
                    translit_unsupported_forms[i].key == key) {
                    form_matches++;
                    form_count += atomic_load_explicit(&translit_unsupported_forms[i].count, memory_order_relaxed);
                }
            for (unsigned i = 0; i < TL_UNSUPPORTED_SITE_N; i++)
                if (atomic_load_explicit(&translit_unsupported_sites[i].state, memory_order_acquire) == 2 &&
                    translit_unsupported_sites[i].key == key && translit_unsupported_sites[i].pc == first.pc) {
                    site_matches++;
                    site_count += atomic_load_explicit(&translit_unsupported_sites[i].count, memory_order_relaxed);
                }
            if (form_matches != 1 || site_matches != 1 || form_count != 2 || site_count != 2 ||
                atomic_load_explicit(&translit_unsupported_keyed, memory_order_relaxed) != keyed + 2 ||
                atomic_load_explicit(&translit_unsupported_form_unique, memory_order_relaxed) != form_unique + 1 ||
                atomic_load_explicit(&translit_unsupported_site_unique, memory_order_relaxed) != site_unique + 1 ||
                atomic_load_explicit(&translit_unsupported_site_repeats, memory_order_relaxed) != repeats + 1 ||
                atomic_load_explicit(&translit_unsupported_form_overflow, memory_order_relaxed) != form_overflow ||
                atomic_load_explicit(&translit_unsupported_site_overflow, memory_order_relaxed) != site_overflow)
                return 58;
        }
        return 0;
    }
    if (scenario == 45) {
        struct insn insn = {.op = 0x12, .two = 1, .has_modrm = 1, .modrm = 0xc1, .mod = 3};
        uint64_t key = translit_unsupported_key(&insn);
        for (unsigned i = 0; i < TL_UNSUPPORTED_FORM_N; i++) {
            translit_unsupported_forms[i].key = key ^ 1;
            atomic_store_explicit(&translit_unsupported_forms[i].state, 2, memory_order_release);
        }
        for (unsigned i = 0; i < TL_UNSUPPORTED_SITE_N; i++) {
            translit_unsupported_sites[i].pc = i;
            translit_unsupported_sites[i].key = key ^ 1;
            atomic_store_explicit(&translit_unsupported_sites[i].state, 2, memory_order_release);
        }
        uint64_t form_overflow = atomic_load_explicit(&translit_unsupported_form_overflow, memory_order_relaxed);
        uint64_t site_overflow = atomic_load_explicit(&translit_unsupported_site_overflow, memory_order_relaxed);
        translit_unsupported_record(&insn, UINT64_MAX);
        return atomic_load_explicit(&translit_unsupported_form_overflow, memory_order_relaxed) == form_overflow + 1 &&
                       atomic_load_explicit(&translit_unsupported_site_overflow, memory_order_relaxed) == site_overflow + 1
                   ? 0
                   : 59;
    }
    if (scenario == 46) {
        struct insn insn = {.op = 0x12, .two = 1, .has_modrm = 1, .modrm = 0xc1, .mod = 3};
        uint64_t keyed = atomic_load_explicit(&translit_unsupported_keyed, memory_order_relaxed);
        uint64_t overflow = atomic_load_explicit(&translit_unsupported_form_overflow, memory_order_relaxed);
        uint64_t deferred = atomic_load_explicit(&translit_unsupported_deferred_excluded, memory_order_relaxed);
        translit_unsupported_record_completed(&insn, 0x1234, 0);
        return atomic_load_explicit(&translit_unsupported_keyed, memory_order_relaxed) == keyed &&
                       atomic_load_explicit(&translit_unsupported_form_overflow, memory_order_relaxed) == overflow &&
                       atomic_load_explicit(&translit_unsupported_deferred_excluded, memory_order_relaxed) == deferred + 1
                   ? 0
                   : 60;
    }
    if (scenario == 47 || scenario == 48) {
        struct insn fs = {.len = 9,
                          .has_rex = 1,
                          .rexW = 1,
                          .opsize = 8,
                          .seg = 1,
                          .op = scenario == 47 ? 0x8B : 0x2B,
                          .has_modrm = 1,
                          .is_mem = 1,
                          .mod = 0,
                          .rm = 4,
                          .reg = scenario == 47 ? 15 : 2,
                          .disp = 0x12345678};
        if (translit_classify(&fs) != TL_FS_MEM) return 61;
        struct insn rejected = fs;
#define FS_MUST_REFUSE(field, value) \
    do {                               \
        rejected = fs;                \
        rejected.field = (value);     \
        if (translit_classify(&rejected) != TL_NO) return 62; \
    } while (0)
        FS_MUST_REFUSE(seg, 2);
        FS_MUST_REFUSE(rexW, 0);
        FS_MUST_REFUSE(addr32, 1);
        FS_MUST_REFUSE(rep, 1);
        FS_MUST_REFUSE(lock, 1);
        FS_MUST_REFUSE(m_hasbase, 1);
        FS_MUST_REFUSE(m_hasindex, 1);
        FS_MUST_REFUSE(op, 0x89);
#undef FS_MUST_REFUSE
        return 0;
    }
    if (scenario == 49) {
        uint8_t emitted[16] = {0};
        hl_x64_asm output = {emitted, emitted + sizeof emitted, 0};
        hl_x64_reg_mem_disp32(&output, 0x8B, 15, 11, INT32_C(0x12345678));
        static const uint8_t expected[] = {0x4D, 0x8B, 0xBB, 0x78, 0x56, 0x34, 0x12};
        return !output.overflow && output.cursor - emitted == (ptrdiff_t)sizeof expected &&
                       memcmp(emitted, expected, sizeof expected) == 0
                   ? 0
                   : 63;
    }
    if (scenario == 50) {
        uint64_t base = UINT64_C(0x12345000);
        uint64_t guest = 0;
        uint32_t preserve = 0;
        jit_instruction_map_put_preserve(base, base + 10, UINT64_C(0x11110000), 1u << 11);
        jit_instruction_map_put_preserve(base + 10, base + 20, UINT64_C(0x22220000), 1u << 11);
        int source = jit_instruction_map_lookup(base + 9, &guest, &preserve) &&
                     guest == UINT64_C(0x11110000) && preserve == (1u << 11);
        int postcommit = jit_instruction_map_lookup(base + 10, &guest, &preserve) &&
                         guest == UINT64_C(0x22220000) && preserve == (1u << 11);
        return source && postcommit ? 0 : 64;
    }
    if (scenario == 51) {
        uint8_t *saved_cache = g_cache;
        ptrdiff_t saved_rw2rx = g_rw2rx;
        uint64_t saved_generation = g_cache_gen;
        int saved_nretired = g_nretired;
        __typeof__(g_retired[0]) saved_retired = g_retired[0];
        uintptr_t current_rw = UINT64_C(0x100000000);
        ptrdiff_t current_delta = (ptrdiff_t)(CACHE_SZ * 8u);
        uintptr_t retired_rw = UINT64_C(0x200000000);
        ptrdiff_t retired_delta = (ptrdiff_t)(CACHE_SZ * 12u);
        g_cache = (uint8_t *)current_rw;
        g_rw2rx = current_delta;
        g_cache_gen = 51;
        g_nretired = 1;
        g_retired[0].rw = (uint8_t *)retired_rw;
        g_retired[0].rw2rx = retired_delta;
        g_retired[0].gen = 50;
        void *current_from_rw = NULL, *current_from_rx = NULL;
        void *retired_from_rw = NULL, *retired_from_rx = NULL;
        uint64_t current_rw_gen = 0, current_rx_gen = 0, retired_rw_gen = 0, retired_rx_gen = 0;
        int ok = jit_resolve_host_rx_code((void *)(current_rw + 0x100), &current_from_rw, &current_rw_gen) &&
                 jit_resolve_host_rx_code((void *)(current_rw + (uintptr_t)current_delta + 0x100),
                                          &current_from_rx, &current_rx_gen) &&
                 jit_resolve_host_rx_code((void *)(retired_rw + 0x200), &retired_from_rw, &retired_rw_gen) &&
                 jit_resolve_host_rx_code((void *)(retired_rw + (uintptr_t)retired_delta + 0x200),
                                          &retired_from_rx, &retired_rx_gen) &&
                 current_from_rw == (void *)(current_rw + (uintptr_t)current_delta + 0x100) &&
                 current_from_rx == current_from_rw && current_rw_gen == 51 && current_rx_gen == 51 &&
                 retired_from_rw == (void *)(retired_rw + (uintptr_t)retired_delta + 0x200) &&
                 retired_from_rx == retired_from_rw && retired_rw_gen == 50 && retired_rx_gen == 50;
        g_cache = saved_cache;
        g_rw2rx = saved_rw2rx;
        g_cache_gen = saved_generation;
        g_nretired = saved_nretired;
        g_retired[0] = saved_retired;
        return ok ? 0 : 65;
    }
    if (scenario == 52)
        return !interp_backend_shape_rel32_reachable(0, 0, 0, UINT64_C(0x1000)) &&
                       interp_backend_shape_rel32_reachable(1, UINT64_C(0x1000), UINT64_C(0x1100),
                                                            UINT64_C(0x1200))
                   ? 0
                   : 66;
    if (scenario == 53) {
        uint64_t source_lo = UINT64_C(0x1800), source_hi = UINT64_C(0x1900);
        translit_jcc_link_own_page(UINT64_C(0x1abc), &source_lo, &source_hi);
        return source_lo == UINT64_C(0x1000) && source_hi == UINT64_C(0x2000) ? 0 : 67;
    }
    if (scenario == 54) {
        hl_x64_cmp_gs_imm8(&output, INT32_C(0x12345678), 0);
        const uint8_t expected[] = {0x65, 0x48, 0x83, 0x3c, 0x25, 0x78, 0x56, 0x34, 0x12, 0x00};
        return output.cursor - bytes == (ptrdiff_t)sizeof expected && memcmp(bytes, expected, sizeof expected) == 0
                   ? 0
                   : 68;
    }
    if (scenario == 55) {
        uint8_t reload[192] = {0};
        hl_x64_asm reload_output = {reload, reload + sizeof reload, 0};
        translit_emit_reload(&reload_output);
        // push %gs:flags; popfq; load RAX then RCX; load guest RSP last.
        const uint8_t prefix[] = {0x65, 0xff, 0x34, 0x25, 0x90, 0x02, 0x00, 0x00, 0x9d,
                                  0x65, 0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00,
                                  0x65, 0x48, 0x8b, 0x0c, 0x25, 0x08, 0x00, 0x00, 0x00};
        const uint8_t suffix[] = {0x65, 0x48, 0x8b, 0x24, 0x25, 0x20, 0x00, 0x00, 0x00};
        size_t length = (size_t)(reload_output.cursor - reload);
        return !reload_output.overflow && length == 153 && memcmp(reload, prefix, sizeof prefix) == 0 &&
                       memcmp(reload + length - sizeof suffix, suffix, sizeof suffix) == 0
                   ? 0
                   : 69;
    }
    if (scenario == 56) {
        uint64_t source_lo = UINT64_C(0x1800), source_hi = UINT64_C(0x1900);
        const uint64_t dirty[1][2] = {{UINT64_C(0x1abc), UINT64_C(0x1abd)}};
        translit_jcc_link_own_page(UINT64_C(0x1abc), &source_lo, &source_hi);
        map_clear();
        map_put(UINT64_C(0x1800), source_lo, source_hi, (void *)(uintptr_t)UINT64_C(0x111000),
                (void *)(uintptr_t)UINT64_C(0x111001));
        map_put(UINT64_C(0x2800), UINT64_C(0x2800), UINT64_C(0x2900),
                (void *)(uintptr_t)UINT64_C(0x222000), (void *)(uintptr_t)UINT64_C(0x222001));
        uint32_t removed = map_invalidate_source_ranges(dirty, 1);
        int ok = removed == 1 && map_host(UINT64_C(0x1800)) == NULL &&
                 (uintptr_t)map_host(UINT64_C(0x2800)) == UINT64_C(0x222000);
        map_clear();
        return ok ? 0 : 70;
    }
    if (scenario == 57) {
        _Alignas(struct interp_block) uint8_t arena[sizeof(struct interp_block) + 1] = {0};
        uint8_t *saved_cache = g_cache;
        uint8_t *saved_cp = g_cp;
        uint64_t saved_generation = g_cache_gen;
        g_cache = arena;
        g_cp = arena + sizeof arena;
        g_cache_gen = 57;
        struct interp_block *target_block = (struct interp_block *)arena;
        target_block->magic = INTERP_BLOCK_MAGIC;
        target_block->generation = g_cache_gen;
        target_block->host_entry_off = sizeof *target_block;
        target_block->host_len = 1;
        target_block->guest_start = UINT64_C(0x1000);
        target_block->guest_end = UINT64_C(0x2000);
        map_clear();
        map_put(UINT64_C(0x1100), UINT64_C(0x1000), UINT64_C(0x2000), target_block, target_block);
        int before_reset = translit_jcc_link_target(UINT64_C(0x1200), UINT64_C(0x1100)) == target_block;
        map_clear();
        int absent_after_reset = translit_jcc_link_target(UINT64_C(0x1200), UINT64_C(0x1100)) == NULL;
        map_put(UINT64_C(0x1100), UINT64_C(0x1000), UINT64_C(0x2000), target_block, target_block);
        int readmitted = translit_jcc_link_target(UINT64_C(0x1200), UINT64_C(0x1100)) == target_block;
        map_clear();
        g_cache = saved_cache;
        g_cp = saved_cp;
        g_cache_gen = saved_generation;
        return before_reset && absent_after_reset && readmitted ? 0 : 72;
    }
    if (scenario >= 58 && scenario <= 66) {
        _Alignas(struct interp_block) uint8_t arena[1024] = {0};
        uint8_t *saved_cache = g_cache;
        uint8_t *saved_cp = g_cp;
        uint64_t saved_generation = g_cache_gen;
        struct interp_block *target_block = (struct interp_block *)arena;
        struct interp_block *source_block = (struct interp_block *)(arena + 128);
        const uint8_t *source_start = arena + 256;
        const uint8_t *source_end = arena + 320;
        uint64_t source = UINT64_C(0x1200), target = UINT64_C(0x1100);
        g_cache = arena;
        g_cp = arena + sizeof arena;
        g_cache_gen = 66;
        target_block->magic = INTERP_BLOCK_MAGIC;
        target_block->generation = g_cache_gen;
        target_block->host_entry_off = sizeof *target_block;
        target_block->host_len = 1;
        target_block->guest_start = UINT64_C(0x1000);
        target_block->guest_end = UINT64_C(0x2000);
        source_block->generation = g_cache_gen;
        map_clear();
        if (scenario != 61 && scenario != 66)
            map_put(target, UINT64_C(0x1000), UINT64_C(0x2000), target_block, target_block);
        if (scenario == 59) {
            source_start = (const uint8_t *)(uintptr_t)UINT64_C(0x1000);
            source_end = (const uint8_t *)(uintptr_t)UINT64_C(0x1100);
        }
        if (scenario == 60) target = UINT64_C(0x2100);
        if (scenario == 62) target_block->host_entry_off = 0;
        if (scenario == 63) target_block->generation--;
        if (scenario == 64) target_block->guest_end = UINT64_C(0x2001);
        uint8_t disposition =
            translit_would_link_classify(source_block, source_start, source_end, source, target, scenario == 65);
        int ok;
        if (scenario == 66) {
            uint8_t publication_disposition = disposition;
            map_put(target, UINT64_C(0x1000), UINT64_C(0x2000), target_block, target_block);
            uint8_t after_mapping =
                translit_would_link_classify(source_block, source_start, source_end, source, target, 0);
            ok = disposition == HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED &&
                 publication_disposition == HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED &&
                 after_mapping == HL_BACKEND_WOULD_LINK_ELIGIBLE;
        } else {
            static const uint8_t expected[] = {
                HL_BACKEND_WOULD_LINK_ELIGIBLE,          HL_BACKEND_WOULD_LINK_SOURCE_UNRESOLVED,
                HL_BACKEND_WOULD_LINK_CROSS_PAGE,        HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED,
                HL_BACKEND_WOULD_LINK_TARGET_UNTRANSLATED, HL_BACKEND_WOULD_LINK_GENERATION,
                HL_BACKEND_WOULD_LINK_TARGET_PAGE,       HL_BACKEND_WOULD_LINK_REL32,
            };
            ok = disposition == expected[scenario - 58];
        }
        map_clear();
        g_cache = saved_cache;
        g_cp = saved_cp;
        g_cache_gen = saved_generation;
        return ok ? 0 : 73;
    }
    if (scenario == 67) {
        uint8_t shape[32] = {0};
        hl_x64_asm shape_output = {shape, shape + sizeof shape, 0};
        uint8_t *marker = translit_shape_exit(&shape_output, HL_BACKEND_SHAPE_T_DIRECT_JUMP, 2, 3, 4);
        translit_shape_would_link_patch(marker, HL_BACKEND_WOULD_LINK_DIRECT_JUMP,
                                        HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED);
        uint32_t packed = marker == NULL ? 0 : (uint32_t)marker[0] | (uint32_t)marker[1] << 8 |
                                                   (uint32_t)marker[2] << 16 | (uint32_t)marker[3] << 24;
        int ok = !shape_output.overflow &&
                 ((packed >> TL_SHAPE_KIND_SHIFT) & TL_SHAPE_KIND_MASK) ==
                     HL_BACKEND_SHAPE_T_DIRECT_JUMP &&
                 ((packed >> TL_SHAPE_JMP_SHIFT) & TL_SHAPE_STITCH_MASK) == 2 &&
                 ((packed >> TL_SHAPE_COND_FALL_SHIFT) & TL_SHAPE_STITCH_MASK) == 3 &&
                 ((packed >> TL_SHAPE_SOURCE_PAGE_SHIFT) & TL_SHAPE_SOURCE_PAGE_MASK) == 4 &&
                 ((packed >> TL_SHAPE_WOULD_FAMILY_SHIFT) & TL_SHAPE_WOULD_FAMILY_MASK) ==
                     HL_BACKEND_WOULD_LINK_DIRECT_JUMP &&
                 ((packed >> TL_SHAPE_WOULD_DISPOSITION_SHIFT) & TL_SHAPE_WOULD_DISPOSITION_MASK) ==
                     HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED;
        return ok ? 0 : 74;
    }
    if (scenario == 68) {
        uint8_t shapes[64] = {0};
        hl_x64_asm shape_output = {shapes, shapes + sizeof shapes, 0};
        uint8_t *source_marker =
            translit_shape_exit(&shape_output, HL_BACKEND_SHAPE_T_DIRECT_JUMP, 0, 0, 0);
        uint8_t *target_marker =
            translit_shape_exit(&shape_output, HL_BACKEND_SHAPE_T_DIRECT_CALL, 0, 0, 0);
        translit_shape_would_link_patch(source_marker, HL_BACKEND_WOULD_LINK_DIRECT_JUMP,
                                        HL_BACKEND_WOULD_LINK_ELIGIBLE);
        translit_shape_would_link_patch(target_marker, HL_BACKEND_WOULD_LINK_DIRECT_CALL,
                                        HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED);
        /* A linked ingress leaves the target terminal's emitted store as cpu->backend_shape. */
        uint32_t executed = target_marker == NULL ? 0 : (uint32_t)target_marker[0] |
                                                        (uint32_t)target_marker[1] << 8 |
                                                        (uint32_t)target_marker[2] << 16 |
                                                        (uint32_t)target_marker[3] << 24;
        int ok = !shape_output.overflow &&
                 ((executed >> TL_SHAPE_KIND_SHIFT) & TL_SHAPE_KIND_MASK) ==
                     HL_BACKEND_SHAPE_T_DIRECT_CALL &&
                 ((executed >> TL_SHAPE_WOULD_FAMILY_SHIFT) & TL_SHAPE_WOULD_FAMILY_MASK) ==
                     HL_BACKEND_WOULD_LINK_DIRECT_CALL &&
                 ((executed >> TL_SHAPE_WOULD_DISPOSITION_SHIFT) & TL_SHAPE_WOULD_DISPOSITION_MASK) ==
                     HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED;
        return ok ? 0 : 75;
    }
    if (scenario == 69) {
        hl_backend_tree_begin(1, effective_host_services());
        if (g_backend_tree_self == NULL) return 76;
        struct cpu family_cpu = {0};
        struct insn family = {.len = 2, .op = 0xFF, .has_modrm = 1, .reg = 4, .is_mem = 1};
        family_cpu.reason = R_BRANCH;
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        struct hl_backend_tree_summary summary;
        hl_backend_tree_summary(&summary);
        if (summary.family_jmem != 1) return 77;
        family.rip_rel = 1; // translated RIP-indirect control is not Jmem
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        family.rip_rel = 0;
        family.reg = 2; // memory-indirect CALL is not Jmem
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        family.reg = 4;
        family.is_mem = 0; // register-indirect JMP is translated, not Jmem
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        family.is_mem = 1;
        family_cpu.reason = R_DIV; // an uncommitted/other exit is not a completed Jmem
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        hl_backend_tree_summary(&summary);
        if (summary.family_jmem != 1) return 78;

        family = (struct insn){.len = 2, .op = 0xF7, .has_modrm = 1, .reg = 6, .opsize = 4};
        interp_backend_family_completed(&family_cpu, &family, 0 /* STEP_NEXT */);
        hl_backend_tree_summary(&summary);
        if (summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_INLINE] != 1 ||
            summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] != 0 ||
            summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_DE] != 0 ||
            summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_INLINE] != 0)
            return 79;
        family.opsize = 8;
        family_cpu.reason = R_DIV;
        family_cpu.divop = 7;
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        hl_backend_tree_summary(&summary);
        if (summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] != 1 ||
            summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_UNSIGNED] != 0)
            return 80;
        hl_backend_tree_family_div_service64_completed(HL_BACKEND_FAMILY_DIV_UNSIGNED);
        hl_backend_tree_summary(&summary);
        if (summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_UNSIGNED] != 1) return 81;
        family.op = 0xF6;
        family.opsize = 1;
        family_cpu.divop = 0;
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        hl_backend_tree_summary(&summary);
        if (summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_DE] != 1 ||
            summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_DE] != 0)
            return 82;

        family.op = 0xF7;
        family.opsize = 4;
        family.reg = 7;
        interp_backend_family_completed(&family_cpu, &family, 0 /* STEP_NEXT */);
        hl_backend_tree_summary(&summary);
        if (summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_INLINE] != 1 ||
            summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] != 0 ||
            summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_DE] != 0)
            return 83;
        family.opsize = 8;
        family_cpu.reason = R_IDIV;
        family_cpu.divop = 7;
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        hl_backend_tree_summary(&summary);
        if (summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] != 1 ||
            summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_SIGNED] != 0)
            return 84;
        hl_backend_tree_family_div_service64_completed(HL_BACKEND_FAMILY_DIV_SIGNED);
        hl_backend_tree_summary(&summary);
        if (summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_SIGNED] != 1) return 85;
        family.op = 0xF6;
        family.opsize = 1;
        family_cpu.divop = 0;
        interp_backend_family_completed(&family_cpu, &family, 1 /* STEP_END */);
        hl_backend_tree_summary(&summary);
        if (summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_DE] != 1) return 86;

        hl_backend_tree_summary(&summary);
        int ok = summary.family_jmem == 1 &&
                 summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_INLINE] == 1 &&
                 summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] == 1 &&
                 summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_UNSIGNED] == 1 &&
                 summary.family_div[HL_BACKEND_FAMILY_DIV_UNSIGNED][HL_BACKEND_FAMILY_DIV_DE] == 1 &&
                 summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_INLINE] == 1 &&
                 summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_SERVICE64] == 1 &&
                 summary.family_div_service64_completed[HL_BACKEND_FAMILY_DIV_SIGNED] == 1 &&
                 summary.family_div[HL_BACKEND_FAMILY_DIV_SIGNED][HL_BACKEND_FAMILY_DIV_DE] == 1;
        return ok ? 0 : 87;
    }
    if (scenario == 70) {
        uint8_t emitted[32] = {0};
        hl_x64_asm marker_output = {emitted, emitted + sizeof emitted, 0};
        uint8_t *marker = translit_shape_fall_exit(&marker_output, 3, 2, 1, HL_BACKEND_FALL_FETCH);
        if (marker == NULL || marker_output.overflow) return 88;
        translit_shape_would_link_patch(marker, HL_BACKEND_WOULD_LINK_FALLTHROUGH,
                                        HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED);
        uint32_t packed = (uint32_t)marker[0] | (uint32_t)marker[1] << 8 | (uint32_t)marker[2] << 16 |
                          (uint32_t)marker[3] << 24;
        return ((packed >> TL_SHAPE_KIND_SHIFT) & TL_SHAPE_KIND_MASK) == HL_BACKEND_SHAPE_T_FALLTHROUGH &&
                       ((packed >> TL_SHAPE_JMP_SHIFT) & TL_SHAPE_STITCH_MASK) == 3 &&
                       ((packed >> TL_SHAPE_COND_FALL_SHIFT) & TL_SHAPE_STITCH_MASK) == 2 &&
                       ((packed >> TL_SHAPE_SOURCE_PAGE_SHIFT) & TL_SHAPE_SOURCE_PAGE_MASK) == 1 &&
                       ((packed >> TL_SHAPE_WOULD_FAMILY_SHIFT) & TL_SHAPE_WOULD_FAMILY_MASK) ==
                           HL_BACKEND_WOULD_LINK_FALLTHROUGH &&
                       ((packed >> TL_SHAPE_WOULD_DISPOSITION_SHIFT) & TL_SHAPE_WOULD_DISPOSITION_MASK) ==
                           HL_BACKEND_WOULD_LINK_TARGET_UNMAPPED &&
                       ((packed >> TL_SHAPE_FALL_STOP_SHIFT) & TL_SHAPE_FALL_STOP_MASK) == HL_BACKEND_FALL_FETCH
                   ? 0
                   : 89;
    }
    if (scenario >= 71 && scenario <= 88) {
        pid_t child = fork();
        if (child < 0) return 97;
        if (child == 0)
            _exit(scenario <= 77 ? translit_fall_fixture_test(scenario)
                                 : translit_mixed_fixture_test(scenario));
        int status = 0;
        return waitpid(child, &status, 0) == child && WIFEXITED(status) ? WEXITSTATUS(status) : 99;
    }
    if (scenario >= 89 && scenario <= 99) {
        pid_t child = fork();
        if (child < 0) return 98;
        if (child == 0) _exit(translit_jcc_ibtc_fixture_test(scenario));
        int status = 0;
        if (waitpid(child, &status, 0) != child) return 99;
        if (WIFEXITED(status)) return WEXITSTATUS(status);
        return WIFSIGNALED(status) ? 144 + WTERMSIG(status) : 99;
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

static void *translit_build(hl_x86_hot_context *context, struct interp_block *block, uint64_t gpc,
                            uint64_t *jcc_ibtc_sites_out) {
    if (jcc_ibtc_sites_out != NULL) *jcc_ibtc_sites_out = 0;
    if (!translit_image_ok()) {
        if (translit_enabled()) (void)__atomic_fetch_add(&translit_declined, 1, __ATOMIC_RELAXED);
#if defined(HL_NATIVE_TEST_HOOKS)
        block->profile_fallback_kind = translit_enabled() ? HL_BACKEND_SHAPE_I_IMAGE : HL_BACKEND_SHAPE_I_DISABLED;
#endif
        return NULL;
    }
    struct insn first_insn;
    int first_decoded = hl_x86_decode_context(context, gpc, &first_insn);
    int first_kind = first_decoded > 0 ? translit_classify(&first_insn) : TL_NO;
#if defined(HL_NATIVE_TEST_HOOKS)
    if (first_decoded <= 0)
        block->profile_fallback_kind = HL_BACKEND_SHAPE_I_DECODE;
    else if (first_kind == TL_NO) {
        block->profile_fallback_kind = HL_BACKEND_SHAPE_I_UNSUPPORTED;
        block->profile_fallback_form = translit_unsupported_key(&first_insn);
    }
#endif
    int fs_run = first_kind == TL_FS_MEM;
    int mixed_sse_enabled = hl_option_get("HL_TRANSLIT_MIXED_SSE_DISABLE") == NULL;
    // Direct host memory is correct only in the identity mapping mode.  The soft/logical and bus-ledger
    // paths require an authoritative guard that this deliberately tiny same-ISA lowering does not fake.
    int fs_authority = g_nonpie_lo || hl_guest_memory_indirect() || jit_guest_soft_active() || jit_guest_bus_active();
#if defined(HL_NATIVE_TEST_HOOKS)
    fs_authority = fs_authority || hl_option_get("HL_TRANSLIT_FS_AUTHORITY_TEST") != NULL;
#endif
    if (fs_run && fs_authority) {
#if defined(HL_NATIVE_TEST_HOOKS)
        block->profile_fallback_kind = HL_BACKEND_SHAPE_I_AUTHORITY;
#endif
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
    // Reserve the worst case before emitting a byte.  Publication advances only the exact populated
    // prefix; conservative capacity refusal falls back to the interpreter with no reachable partial body.
    // FS remains one isolated transaction (two recovery spans plus its tail bound); every other body may
    // need one exact range per retired instruction plus the final fall-through tail.
    uint32_t owner_reservation = fs_run ? 3u : TL_MAX_OWNER_RANGES;
    if (!jit_body_owner_reserve_n(g_cache_gen, owner_reservation, &owner_token)) {
#if defined(HL_NATIVE_TEST_HOOKS)
        block->profile_fallback_kind = HL_BACKEND_SHAPE_I_RESOURCE;
#endif
        return NULL;
    }
    uint8_t *arena_limit = (uint8_t *)g_cache + CACHE_SZ - CACHE_EMIT_HEADROOM;
    uint8_t *start = g_cp;
    if (start + TL_MAX_BYTES > arena_limit) {
#if defined(HL_NATIVE_TEST_HOOKS)
        block->profile_fallback_kind = HL_BACKEND_SHAPE_I_RESOURCE;
#endif
        return NULL;
    }
    struct translit_builder builder = {{start, start + TL_MAX_BYTES, 0}, start, g_rw2rx};
    hl_x64_asm *a = &builder.asm_state;

    uint64_t pc = gpc;
    int count = 0, terminated = 0, stitched = 0;
    int previous_kind = TL_NO;
    uint64_t mixed_sse_transition_count = 0;
    int mixed_sse_boundary_stopped = 0;
#if defined(HL_NATIVE_TEST_HOOKS)
    uint64_t sse2_instruction_count = 0;
    unsigned fall_stop = HL_BACKEND_FALL_OTHER;
    uint64_t sse2_signature = 0;
    uint64_t sse2_store_count = 0;
    uint64_t sse2_store_movups = 0, sse2_store_movaps = 0, sse2_store_movdqu = 0;
    uint64_t sse2_pxor_count = 0;
    uint64_t sse2_punpcklqdq_count = 0;
    uint64_t sse2_movd_count = 0;
    uint64_t sse2_movhlps_count = 0;
    int sse2_next_family_seen = 0;
    int sse2_store_family_seen = 0;
    int sse2_pxor_seen = 0;
    int sse2_punpcklqdq_seen = 0;
    int sse2_movd_seen = 0;
    unsigned would_link_family = HL_BACKEND_WOULD_LINK_FAMILY_COUNT;
    uint64_t would_link_source = 0, would_link_target = 0;
    uint8_t *would_link_marker = NULL;
#endif
    jit_body_owner_range owners[TL_MAX_OWNER_RANGES];
    jit_body_owner_range instruction_ranges[TL_MAX_OWNER_RANGES];
    uint32_t owner_count = 0;
    uint32_t instruction_range_count = 0;
    int owner_normal_pending = 0;
    int jcc_fall_stitched = 0;
    int direct_jmp_stitched = 0;
    unsigned jcc_link_admitted = 0;
    uint64_t jcc_ibtc_sites = 0;
    uint64_t linked_page_lo = UINT64_MAX, linked_page_hi = 0;
    uint8_t bytes[16];

    while (count < TL_MAX_INSNS && !a->overflow) {
        struct insn insn;
        if (hl_x86_decode_context(context, pc, &insn) <= 0) {
#if defined(HL_NATIVE_TEST_HOOKS)
            fall_stop = HL_BACKEND_FALL_DECODE;
#endif
            break;
        }
        int kind = translit_classify(&insn);
        if (kind == TL_NO) {
#if defined(HL_NATIVE_TEST_HOOKS)
            fall_stop = HL_BACKEND_FALL_TL_NO;
#endif
            break;
        }
        // FS is still an isolated one-instruction transaction.  Only the normal/SSE ownership-ledger
        // boundary is removable in phase one.
        if ((fs_run && kind != TL_FS_MEM) || (!fs_run && kind == TL_FS_MEM)) {
#if defined(HL_NATIVE_TEST_HOOKS)
            if (fs_run)
                fall_stop = kind == TL_SSE2 ? HL_BACKEND_FALL_FS_TO_SSE2 : HL_BACKEND_FALL_FS_TO_NORMAL;
            else
                fall_stop = previous_kind == TL_SSE2 ? HL_BACKEND_FALL_SSE2_TO_FS
                                                      : HL_BACKEND_FALL_NORMAL_TO_FS;
#endif
            break;
        }
        int previous_sse2 = previous_kind == TL_SSE2;
        int current_sse2 = kind == TL_SSE2;
        if (previous_kind != TL_NO && previous_sse2 != current_sse2) {
            (void)__atomic_fetch_add(&translit_mixed_sse_encounters, 1, __ATOMIC_RELAXED);
            if (!mixed_sse_enabled) {
#if defined(HL_NATIVE_TEST_HOOKS)
                fall_stop = current_sse2 ? HL_BACKEND_FALL_NORMAL_TO_SSE2
                                         : HL_BACKEND_FALL_SSE2_TO_NORMAL;
#endif
                mixed_sse_boundary_stopped = 1;
                break;
            }
            mixed_sse_transition_count++;
        }
        if (!translit_displaced_safe(&insn, kind)) {
            (void)__atomic_fetch_add(&translit_declined, 1, __ATOMIC_RELAXED);
            (void)__atomic_fetch_add(&translit_displaced_operand_declined, 1, __ATOMIC_RELAXED);
            translit_count_displaced_refusal(&insn);
#if defined(HL_NATIVE_TEST_HOOKS)
            fall_stop = HL_BACKEND_FALL_DISPLACED_UNSAFE;
#endif
            break;
        }
        if (x86_guest_fetch_exec(pc, bytes, (size_t)insn.len) != 0) {
#if defined(HL_NATIVE_TEST_HOOKS)
            fall_stop = HL_BACKEND_FALL_FETCH;
#endif
            break;
        }

        uint64_t next = pc + (uint64_t)insn.len;
        unsigned source_page_delta = (unsigned)((pc >> 12) - (gpc >> 12));
        uint8_t *instruction_start = a->cursor;
        translit_jcc_ibtc_site_ranges ibtc_site = {0};
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
#if defined(HL_NATIVE_TEST_HOOKS)
                    fall_stop = HL_BACKEND_FALL_RIPREL_LOWER;
#endif
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
            uint32_t preserve = translit_preserve_mask(preserve_register);
            int instruction_ok = translit_owner_append(instruction_ranges, &instruction_range_count,
                                                        instruction_start, a->cursor, pc, preserve);
            int owner_ok;
            if (preserve == 0) {
                owner_ok = translit_owner_append_normal(owners, &owner_count, &owner_normal_pending,
                                                        instruction_start, a->cursor, pc);
            } else {
                owner_normal_pending = 0;
                owner_ok = translit_owner_append(owners, &owner_count, instruction_start, a->cursor, pc,
                                                  preserve);
            }
            if (!instruction_ok || !owner_ok) {
                a->overflow = 1;
                a->cursor = instruction_start;
                break;
            }
            pc = next;
            count++;
            previous_kind = kind;
            continue;
        }

        if (kind == TL_FS_MEM) {
            int scratch = insn.reg == HL_X64_R11 ? 10 : HL_X64_R11;
            uint32_t preserve = translit_preserve_mask(scratch);
            hl_x64_store_gs(a, scratch, R_OFF(scratch));
            hl_x64_load_gs(a, scratch, OFF_FS);
            hl_x64_reg_mem_disp32(a, insn.op, insn.reg, scratch, (int32_t)insn.disp);
            uint8_t *commit_end = a->cursor;
            hl_x64_load_gs(a, scratch, R_OFF(scratch));
            if (a->overflow) {
                a->cursor = instruction_start;
                break;
            }
            owner_normal_pending = 0;
            if (!translit_owner_append(owners, &owner_count, instruction_start, commit_end, pc, preserve) ||
                !translit_owner_append(owners, &owner_count, commit_end, a->cursor, next, preserve) ||
                !translit_owner_append(instruction_ranges, &instruction_range_count, instruction_start,
                                       commit_end, pc, preserve) ||
                !translit_owner_append(instruction_ranges, &instruction_range_count, commit_end,
                                       a->cursor, next, preserve)) {
                a->overflow = 1;
                a->cursor = instruction_start;
                break;
            }
            pc = next;
            count++;
            previous_kind = kind;
#if defined(HL_NATIVE_TEST_HOOKS)
            fall_stop = HL_BACKEND_FALL_FS_TRANSACTION;
#endif
            break; // one transaction per descriptor keeps the persistent owner map exact and bounded
        }

        if (kind == TL_SSE2) {
            int sse2_store = insn.is_mem &&
                             (insn.op == 0x11 || insn.op == 0x29 || (insn.rep && insn.op == 0x7F));
            int sse2_gpr_source = insn.op == 0x6E;
            int destination = insn.op == 0x73 ? insn.rm_reg : insn.reg;
            // MOVD/MOVQ sources a canonical guest GPR and overwrites the complete XMM destination.
            if (sse2_gpr_source) {
                // No XMM preload.
            } else if (sse2_store) {
                translit_xmm_cpu(a, insn.reg, 1);
            } else if (insn.op == 0x12 || insn.op == 0x6C || insn.op == 0xDB || insn.op == 0xDF || insn.op == 0xEB ||
                       insn.op == 0xEF) {
                translit_xmm_cpu(a, insn.reg, 1);
                if (!insn.is_mem && insn.rm_reg != insn.reg) translit_xmm_cpu(a, insn.rm_reg, 1);
            } else if (insn.op == 0x6F || insn.op == 0x10 || insn.op == 0x28) {
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
#if defined(HL_NATIVE_TEST_HOOKS)
                fall_stop = HL_BACKEND_FALL_SSE_RIPREL_LOWER;
#endif
                break;
            }
            if (!sse2_store && insn.op != 0x7E) translit_xmm_cpu(a, destination, 0);
            if (a->overflow) {
                a->cursor = instruction_start;
                break;
            }
            uint8_t *commit_end = a->cursor;
            owner_normal_pending = 0;
            if (!translit_owner_append(owners, &owner_count, instruction_start, commit_end, pc, 0) ||
                !translit_owner_append(instruction_ranges, &instruction_range_count, instruction_start,
                                       commit_end, pc, 0)) {
                a->overflow = 1;
                a->cursor = instruction_start;
                break;
            }
            pc = next;
            count++;
            previous_kind = kind;
#if defined(HL_NATIVE_TEST_HOOKS)
            sse2_instruction_count++;
            sse2_signature = (sse2_signature << 8) | insn.op;
            sse2_store_count += (uint64_t)sse2_store;
            sse2_store_movups += (uint64_t)(sse2_store && insn.op == 0x11);
            sse2_store_movaps += (uint64_t)(sse2_store && insn.op == 0x29);
            sse2_store_movdqu += (uint64_t)(sse2_store && insn.op == 0x7F);
            sse2_pxor_count += (uint64_t)(insn.op == 0xEF);
            sse2_punpcklqdq_count += (uint64_t)(insn.op == 0x6C);
            sse2_movd_count += (uint64_t)(insn.op == 0x6E);
            sse2_movhlps_count += (uint64_t)(insn.op == 0x12);
            if ((sse2_signature & UINT64_C(0xFFFFFF)) == UINT64_C(0x6F6FDF))
                sse2_next_family_seen = 1;
            if ((sse2_signature & UINT64_C(0xFFFFFFFF)) == UINT64_C(0x6F11297F))
                sse2_store_family_seen = 1;
            if (insn.op == 0xEF) sse2_pxor_seen = 1;
            if (insn.op == 0x6C) sse2_punpcklqdq_seen = 1;
            if (insn.op == 0x6E) sse2_movd_seen = 1;
#endif
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
                    uint64_t target = next + (uint64_t)insn.imm;
                    struct interp_block *target_block = translit_jcc_link_target(pc, target);
                    uint8_t *fall = hl_x64_jcc_rel32(a, translit_condition(&insn) ^ 1);
                    translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
                    translit_shape_exit(a, HL_BACKEND_SHAPE_T_COND_TAKEN, (unsigned)direct_jmp_stitched,
                                        (unsigned)jcc_fall_stitched, source_page_delta);
                    if (target_block != NULL) {
                        translit_emit_jcc_link(a, target_block, target);
                        translit_jcc_link_own_page(pc, &linked_page_lo, &linked_page_hi);
                        jcc_link_admitted++;
                    } else if (translit_jcc_ibtc_candidate(pc, target)) {
                        ibtc_site = translit_emit_jcc_ibtc(a, target);
                        jcc_ibtc_sites++;
                    } else {
                        translit_emit_jcc_dispatcher(a, target);
                    }
                    hl_x64_patch_rel32(a, fall, a->cursor);
                    translit_shape_exit(a, HL_BACKEND_SHAPE_T_OTHER, (unsigned)direct_jmp_stitched,
                                        (unsigned)(jcc_fall_stitched + 1), source_page_delta);
                    if (a->overflow) {
                        a->cursor = instruction_start;
                        break;
                    }
                    owner_normal_pending = 0;
                    int owner_exact = ibtc_site.end != NULL
                                          ? translit_owner_append_ibtc(owners, &owner_count,
                                                                       instruction_start, a->cursor,
                                                                       &ibtc_site, pc)
                                          : translit_owner_append(owners, &owner_count, instruction_start,
                                                                  a->cursor, pc, 0);
                    int instruction_exact =
                        ibtc_site.end != NULL
                            ? translit_owner_append_ibtc(instruction_ranges, &instruction_range_count,
                                                         instruction_start, a->cursor, &ibtc_site, pc)
                            : translit_owner_append(instruction_ranges, &instruction_range_count,
                                                    instruction_start, a->cursor, pc, 0);
                    if (!owner_exact || !instruction_exact) {
                        a->overflow = 1;
                        a->cursor = instruction_start;
                        break;
                    }
                    pc = next;
                    count++;
                    stitched++;
                    jcc_fall_stitched++;
                    previous_kind = kind;
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
                    translit_shape_exit(a, HL_BACKEND_SHAPE_T_OTHER, (unsigned)(direct_jmp_stitched + 1),
                                        (unsigned)jcc_fall_stitched, source_page_delta);
                    if (a->overflow) {
                        a->cursor = instruction_start;
                        break;
                    }
                    // Nohooks emits no bytes here; hooks emits a four-byte terminal-shape store whose
                    // asynchronous provenance must still name this stitched JMP. Empty appends are
                    // intentionally ignored, preserving production layout and owner density.
                    owner_normal_pending = 0;
                    if (!translit_owner_append(owners, &owner_count, instruction_start, a->cursor, pc, 0) ||
                        !translit_owner_append(instruction_ranges, &instruction_range_count,
                                               instruction_start, a->cursor, pc, 0)) {
                        a->overflow = 1;
                        a->cursor = instruction_start;
                        break;
                    }
                    pc = target;
                    count++;
                    stitched++;
                    direct_jmp_stitched++;
                    previous_kind = kind;
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
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
            translit_shape_exit(a, HL_BACKEND_SHAPE_T_SYSCALL, (unsigned)direct_jmp_stitched,
                                (unsigned)jcc_fall_stitched, source_page_delta);
            translit_emit_exit_const(a, next, R_SYSCALL);
            break;
        case TL_JMP:
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
#if defined(HL_NATIVE_TEST_HOOKS)
            would_link_marker =
                translit_shape_exit(a, HL_BACKEND_SHAPE_T_DIRECT_JUMP, (unsigned)direct_jmp_stitched,
                                    (unsigned)jcc_fall_stitched, source_page_delta);
#else
            translit_shape_exit(a, HL_BACKEND_SHAPE_T_DIRECT_JUMP, (unsigned)direct_jmp_stitched,
                                (unsigned)jcc_fall_stitched, source_page_delta);
#endif
            translit_emit_exit_const(a, next + (uint64_t)insn.imm, R_BRANCH);
#if defined(HL_NATIVE_TEST_HOOKS)
            would_link_family = HL_BACKEND_WOULD_LINK_DIRECT_JUMP;
            would_link_source = pc;
            would_link_target = next + (uint64_t)insn.imm;
#endif
            break;
        case TL_JCC: {
            uint64_t target = next + (uint64_t)insn.imm;
            struct interp_block *target_block = translit_jcc_link_target(pc, target);
            uint8_t *taken = hl_x64_jcc_rel32(a, translit_condition(&insn));
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
            translit_shape_exit(a, HL_BACKEND_SHAPE_T_COND_NOT_TAKEN, (unsigned)direct_jmp_stitched,
                                (unsigned)jcc_fall_stitched, source_page_delta);
            translit_emit_exit_const(a, next, R_BRANCH);
            hl_x64_patch_rel32(a, taken, a->cursor);
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
            translit_shape_exit(a, HL_BACKEND_SHAPE_T_COND_TAKEN, (unsigned)direct_jmp_stitched,
                                (unsigned)jcc_fall_stitched, source_page_delta);
            if (target_block != NULL) {
                translit_emit_jcc_link(a, target_block, target);
                translit_jcc_link_own_page(pc, &linked_page_lo, &linked_page_hi);
                jcc_link_admitted++;
            } else if (translit_jcc_ibtc_candidate(pc, target)) {
                ibtc_site = translit_emit_jcc_ibtc(a, target);
                jcc_ibtc_sites++;
            } else {
                translit_emit_jcc_dispatcher(a, target);
            }
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
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
#if defined(HL_NATIVE_TEST_HOOKS)
            uint8_t *call_marker = translit_shape_exit(a,
                                    kind == TL_CALL ? HL_BACKEND_SHAPE_T_DIRECT_CALL
                                                    : HL_BACKEND_SHAPE_T_INDIRECT_CALL,
                                    (unsigned)direct_jmp_stitched, (unsigned)jcc_fall_stitched, source_page_delta);
            if (kind == TL_CALL) would_link_marker = call_marker;
#else
            translit_shape_exit(a,
                                kind == TL_CALL ? HL_BACKEND_SHAPE_T_DIRECT_CALL
                                                : HL_BACKEND_SHAPE_T_INDIRECT_CALL,
                                (unsigned)direct_jmp_stitched, (unsigned)jcc_fall_stitched, source_page_delta);
#endif
            if (kind == TL_CALL)
                translit_emit_exit_const(a, next + (uint64_t)insn.imm, R_BRANCH);
            else
                translit_emit_exit_preset(a, R_BRANCH);
#if defined(HL_NATIVE_TEST_HOOKS)
            if (kind == TL_CALL) {
                would_link_family = HL_BACKEND_WOULD_LINK_DIRECT_CALL;
                would_link_source = pc;
                would_link_target = next + (uint64_t)insn.imm;
            }
#endif
            break;
        }
        case TL_JMP_REG:
            hl_x64_store_gs(a, insn.rm_reg, OFF_RIP);
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
            translit_shape_exit(a, HL_BACKEND_SHAPE_T_INDIRECT_BRANCH, (unsigned)direct_jmp_stitched,
                                (unsigned)jcc_fall_stitched, source_page_delta);
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
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
            translit_shape_exit(a, HL_BACKEND_SHAPE_T_INDIRECT_BRANCH, (unsigned)direct_jmp_stitched,
                                (unsigned)jcc_fall_stitched, source_page_delta);
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
            translit_mixed_profile_terminal(a, mixed_sse_transition_count, 0);
            translit_shape_exit(a, HL_BACKEND_SHAPE_T_RETURN, (unsigned)direct_jmp_stitched,
                                (unsigned)jcc_fall_stitched, source_page_delta);
            translit_emit_exit_preset(a, R_BRANCH);
            break;
        }
        default: break;
        }
        if (a->overflow) {
            a->cursor = instruction_start;
            break;
        }
        owner_normal_pending = 0;
        int owner_ok;
        if (preserve_start) {
            owner_ok = translit_owner_append(owners, &owner_count, instruction_start, preserve_end, pc,
                                              translit_preserve_mask(preserve_register)) &&
                       translit_owner_append(owners, &owner_count, preserve_end, a->cursor, pc, 0) &&
                       translit_owner_append(instruction_ranges, &instruction_range_count,
                                              instruction_start, preserve_end, pc,
                                              translit_preserve_mask(preserve_register)) &&
                       translit_owner_append(instruction_ranges, &instruction_range_count,
                                              preserve_end, a->cursor, pc, 0);
            (void)__atomic_fetch_add(&translit_rip_indirect_lowered, 1, __ATOMIC_RELAXED);
        } else if (ibtc_site.end != NULL) {
            owner_ok = translit_owner_append_ibtc(owners, &owner_count, instruction_start,
                                                  a->cursor, &ibtc_site, pc) &&
                       translit_owner_append_ibtc(instruction_ranges, &instruction_range_count,
                                                  instruction_start, a->cursor, &ibtc_site, pc);
        } else {
            owner_ok = translit_owner_append(owners, &owner_count, instruction_start, a->cursor, pc, 0) &&
                       translit_owner_append(instruction_ranges, &instruction_range_count,
                                              instruction_start, a->cursor, pc, 0);
        }
        if (!owner_ok) {
            a->overflow = 1;
            a->cursor = instruction_start;
            break;
        }
        pc = next;
        count++;
        previous_kind = kind;
        terminated = 1;
        break;
    }

    if (count == 0 || a->overflow) {
#if defined(HL_NATIVE_TEST_HOOKS)
        if (block->profile_fallback_kind == HL_BACKEND_SHAPE_I_OTHER)
            block->profile_fallback_kind = HL_BACKEND_SHAPE_I_EMIT;
#endif
        return NULL;
    }
    if (!terminated) {
#if defined(HL_NATIVE_TEST_HOOKS)
        if (count == TL_MAX_INSNS) fall_stop = HL_BACKEND_FALL_CAP;
#endif
        uint8_t *tail = a->cursor;
        translit_mixed_profile_terminal(a, mixed_sse_transition_count, mixed_sse_boundary_stopped);
#if defined(HL_NATIVE_TEST_HOOKS)
        would_link_marker = translit_shape_fall_exit(a, (unsigned)direct_jmp_stitched,
                                                     (unsigned)jcc_fall_stitched,
                                                     (unsigned)((pc >> 12) - (gpc >> 12)), fall_stop);
#else
        translit_shape_fall_exit(a, (unsigned)direct_jmp_stitched, (unsigned)jcc_fall_stitched,
                                 (unsigned)((pc >> 12) - (gpc >> 12)), 0);
#endif
        translit_emit_exit_const(a, pc, R_BRANCH);
#if defined(HL_NATIVE_TEST_HOOKS)
        would_link_family = HL_BACKEND_WOULD_LINK_FALLTHROUGH;
        would_link_source = pc - 1;
        would_link_target = pc;
#endif
        if (a->overflow) {
#if defined(HL_NATIVE_TEST_HOOKS)
            block->profile_fallback_kind = HL_BACKEND_SHAPE_I_EMIT;
#endif
            return NULL;
        }
        owner_normal_pending = 0;
        if (!translit_owner_append(owners, &owner_count, tail, a->cursor, pc, 0) ||
            !translit_owner_append(instruction_ranges, &instruction_range_count, tail, a->cursor, pc, 0)) {
#if defined(HL_NATIVE_TEST_HOOKS)
            block->profile_fallback_kind = HL_BACKEND_SHAPE_I_RESOURCE;
#endif
            return NULL;
        }
    }

    int owner_published = jit_body_owner_publish_n(g_cache_gen, owner_token, owners, owner_count);
    if (!owner_published) {
#if defined(HL_NATIVE_TEST_HOOKS)
        block->profile_fallback_kind = HL_BACKEND_SHAPE_I_RESOURCE;
#endif
        return NULL;
    }
    // The instruction map is deliberately staged behind the all-or-nothing owner publication.  Failed
    // emission and capacity refusal therefore cannot leave stale provenance naming unreachable bytes.
    translit_instruction_ranges_publish(instruction_ranges, instruction_range_count);
    if (mixed_sse_transition_count != 0) {
        (void)__atomic_fetch_add(&translit_mixed_sse_admitted, 1, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_mixed_sse_transitions, mixed_sse_transition_count,
                                 __ATOMIC_RELAXED);
    }
#if defined(HL_NATIVE_TEST_HOOKS)
    (void)__atomic_fetch_add(&translit_body_owner_published, 1, __ATOMIC_RELAXED);
    (void)__atomic_fetch_add(&translit_jcc_link_admitted, jcc_link_admitted, __ATOMIC_RELAXED);
    if (would_link_family < HL_BACKEND_WOULD_LINK_FAMILY_COUNT) {
        unsigned disposition =
            translit_would_link_classify(block, start, a->cursor, would_link_source, would_link_target, 0);
        translit_shape_would_link_patch(would_link_marker, would_link_family, disposition);
        translit_test_last_fall_marker = (uint32_t)would_link_marker[0] |
                                         (uint32_t)would_link_marker[1] << 8 |
                                         (uint32_t)would_link_marker[2] << 16 |
                                         (uint32_t)would_link_marker[3] << 24;
        translit_test_fall_marker_seen = 1;
    }
    if (sse2_instruction_count != 0) {
        (void)__atomic_fetch_add(&translit_sse2_runs_admitted, 1, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_instructions_admitted, sse2_instruction_count,
                                 __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_store_instructions, sse2_store_count, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_store_movups, sse2_store_movups, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_store_movaps, sse2_store_movaps, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_store_movdqu, sse2_store_movdqu, __ATOMIC_RELAXED);
        if (sse2_instruction_count == 6 && sse2_signature == UINT64_C(0x6FDB6F73EB7E))
            (void)__atomic_fetch_add(&translit_sse2_target_runs, 1, __ATOMIC_RELAXED);
        if (sse2_next_family_seen)
            (void)__atomic_fetch_add(&translit_sse2_next_family_runs, 1, __ATOMIC_RELAXED);
        if (sse2_store_family_seen)
            (void)__atomic_fetch_add(&translit_sse2_store_family_runs, 1, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_pxor_admitted, sse2_pxor_count, __ATOMIC_RELAXED);
        if (sse2_pxor_seen) (void)__atomic_fetch_add(&translit_sse2_pxor_runs_admitted, 1, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_punpcklqdq_admitted, sse2_punpcklqdq_count, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_movhlps_admitted, sse2_movhlps_count, __ATOMIC_RELAXED);
        if (sse2_punpcklqdq_seen)
            (void)__atomic_fetch_add(&translit_sse2_punpcklqdq_runs_admitted, 1, __ATOMIC_RELAXED);
        (void)__atomic_fetch_add(&translit_sse2_movd_admitted, sse2_movd_count, __ATOMIC_RELAXED);
        if (sse2_movd_seen) (void)__atomic_fetch_add(&translit_sse2_movd_runs_admitted, 1, __ATOMIC_RELAXED);
    }
    if (fs_run) {
        (void)__atomic_fetch_add(&translit_fs_mem_admitted, 1, __ATOMIC_RELAXED);
        if (first_insn.disp == INT64_C(0x12340) || first_insn.disp == INT64_C(0x12348) ||
            first_insn.disp == INT64_C(0x12350) || first_insn.disp == INT64_C(0x12358) ||
            first_insn.disp == -INT64_C(0x2340))
            (void)__atomic_fetch_add(&translit_fs_fixture_admitted, 1, __ATOMIC_RELAXED);
    }
#else
    (void)jcc_link_admitted;
#endif
    block->host_entry_off = (uint32_t)(start - (uint8_t *)block);
    block->host_len = (uint32_t)(a->cursor - start);
    block->guest_start = linked_page_lo < gpc ? linked_page_lo : gpc;
    block->guest_end = linked_page_hi > pc ? linked_page_hi : pc;
#if defined(HL_NATIVE_TEST_HOOKS)
    translit_perf_map_publish(block, start, block->host_len, (uint32_t)count);
#endif
#if defined(HL_NATIVE_TEST_HOOKS)
    block->profile_jcc_fall_stitches = (uint8_t)jcc_fall_stitched;
    block->profile_insns = (uint16_t)count;
#endif
    g_cp = a->cursor;
    if (jcc_ibtc_sites_out != NULL) *jcc_ibtc_sites_out = jcc_ibtc_sites;
    (void)__atomic_fetch_add(&translit_blocks, 1, __ATOMIC_RELAXED);
    if (g_nonpie_lo) (void)__atomic_fetch_add(&translit_displaced_blocks, 1, __ATOMIC_RELAXED);
    return start;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int translit_fall_fixture_fetch(void *opaque, uint64_t guest, void *destination, size_t length) {
    (void)opaque;
    if (translit_fall_fixture_bytes == NULL || guest < translit_fall_fixture_guest ||
        guest - translit_fall_fixture_guest >= translit_fall_fixture_length)
        return -1;
    size_t offset = (size_t)(guest - translit_fall_fixture_guest);
    size_t copied = translit_fall_fixture_length - offset;
    if (copied > length) copied = length;
    memcpy(destination, translit_fall_fixture_bytes + offset, copied);
    memset((uint8_t *)destination + copied, 0, length - copied);
    return 0;
}

static int translit_fall_fixture_test(unsigned scenario) {
    const size_t arena_size = 8192;
    uint8_t *arena = mmap(NULL, arena_size, PROT_READ | PROT_WRITE | PROT_EXEC,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 98;
    static const uint8_t normal_to_sse[] = {0x90, 0x66, 0x0f, 0xef, 0xc0, 0x0f, 0x0b};
    static const uint8_t sse_to_normal[] = {0x66, 0x0f, 0xef, 0xc0, 0x90, 0x0f, 0x0b};
    static const uint8_t normal_to_fs[] = {0x90, 0x64, 0x48, 0x8b, 0x04, 0x25, 0x40, 0x23, 0x01, 0x00};
    static const uint8_t fs_transaction[] = {0x64, 0x48, 0x8b, 0x04, 0x25, 0x40, 0x23, 0x01, 0x00};
    static const uint8_t tl_no[] = {0x90, 0x0f, 0x0b};
    static const uint8_t sse_to_tl_no[] = {0x66, 0x0f, 0xef, 0xc0, 0x0f, 0x0b};
    uint8_t cap[TL_MAX_INSNS + 1];
    memset(cap, 0x90, TL_MAX_INSNS);
    cap[TL_MAX_INSNS] = 0x0f;
    const uint8_t *bytes = NULL;
    size_t length = 0;
    unsigned expected = HL_BACKEND_FALL_OTHER;
    switch (scenario) {
    case 71: bytes = normal_to_sse; length = sizeof normal_to_sse; expected = HL_BACKEND_FALL_TL_NO; break;
    case 72: bytes = sse_to_normal; length = sizeof sse_to_normal; expected = HL_BACKEND_FALL_TL_NO; break;
    case 73: bytes = normal_to_fs; length = sizeof normal_to_fs; expected = HL_BACKEND_FALL_NORMAL_TO_FS; break;
    case 74: bytes = fs_transaction; length = sizeof fs_transaction; expected = HL_BACKEND_FALL_FS_TRANSACTION; break;
    case 75: bytes = tl_no; length = sizeof tl_no; expected = HL_BACKEND_FALL_TL_NO; break;
    case 76: bytes = cap; length = sizeof cap; expected = HL_BACKEND_FALL_CAP; break;
    case 77: bytes = sse_to_tl_no; length = sizeof sse_to_tl_no; expected = HL_BACKEND_FALL_TL_NO; break;
    default: return 90;
    }
    uint8_t *saved_cache = g_cache;
    uint8_t *saved_cp = g_cp;
    ptrdiff_t saved_rw2rx = g_rw2rx;
    uint64_t saved_generation = g_cache_gen;
    g_cache = arena;
    g_cp = arena + sizeof(struct interp_block);
    g_rw2rx = 0;
    g_cache_gen = UINT64_MAX - scenario;
    struct interp_block *block = (struct interp_block *)arena;
    translit_fall_fixture_bytes = bytes;
    translit_fall_fixture_guest = (uint64_t)(uintptr_t)bytes;
    translit_fall_fixture_length = length;
    translit_test_last_fall_marker = 0;
    translit_test_fall_marker_seen = 0;
    hl_x86_hot_context *context = hl_x86_hot_context_create(translit_fall_fixture_fetch, NULL);
    void *body = context == NULL ? NULL : translit_build(context, block, translit_fall_fixture_guest, NULL);
    unsigned actual = (translit_test_last_fall_marker >> TL_SHAPE_FALL_STOP_SHIFT) & TL_SHAPE_FALL_STOP_MASK;
    unsigned kind = (translit_test_last_fall_marker >> TL_SHAPE_KIND_SHIFT) & TL_SHAPE_KIND_MASK;
    int built = body != NULL && translit_test_fall_marker_seen && kind == HL_BACKEND_SHAPE_T_FALLTHROUGH &&
                actual == expected;
    int cap_exact = scenario != 76 ||
                    (block->profile_insns == TL_MAX_INSNS && block->guest_end == translit_fall_fixture_guest + TL_MAX_INSNS);
    struct cpu fixture_cpu = {0};
    fixture_cpu.rip = translit_fall_fixture_guest;
    fixture_cpu.fs_base = (uint64_t)(uintptr_t)(arena + arena_size - 8) - UINT64_C(0x12340);
    block->magic = INTERP_BLOCK_MAGIC;
    block->gpc = translit_fall_fixture_guest;
    hl_backend_tree_begin(1, effective_host_services());
    if (built && cap_exact) run_block(context, &fixture_cpu, block);
    struct hl_backend_tree_summary summary;
    hl_backend_tree_summary(&summary);
    int executed = fixture_cpu.irq == 0 &&
                   ((fixture_cpu.backend_shape >> TL_SHAPE_FALL_STOP_SHIFT) & TL_SHAPE_FALL_STOP_MASK) == expected &&
                   summary.translated_exit[HL_BACKEND_SHAPE_T_FALLTHROUGH] == 1 &&
                   summary.translated_fall_stop[expected] == 1;
    for (unsigned reason = 0; reason < HL_BACKEND_FALL_COUNT; reason++)
        if (reason != expected && summary.translated_fall_stop[reason] != 0) executed = 0;
    translit_fall_fixture_bytes = NULL;
    hl_x86_hot_context_destroy(context);
    jit_body_owner_drop_generation(g_cache_gen);
    g_cache = saved_cache;
    g_cp = saved_cp;
    g_rw2rx = saved_rw2rx;
    g_cache_gen = saved_generation;
    munmap(arena, arena_size);
    return built && cap_exact && executed ? 0 : 100 + (int)(scenario - 71);
}

static int translit_mixed_fixture_test(unsigned scenario) {
    const size_t arena_size = 8192;
    uint8_t *arena = mmap(NULL, arena_size, PROT_READ | PROT_WRITE | PROT_EXEC,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 120;
    static const uint8_t alternating[] = {
        0x90,                         // normal
        0x66, 0x0f, 0xef, 0xc0,       // pxor xmm0,xmm0
        0x4d, 0x89, 0xd2,             // mov r10,r10
        0x66, 0x0f, 0xef, 0xc9,       // pxor xmm1,xmm1
        0x90,                         // normal
        0x0f, 0x0b,                   // unsupported terminator for a deterministic tail
    };
    static const uint8_t direct_stitch[] = {
        0x90,                         // normal
        0x66, 0x0f, 0xef, 0xc0,       // pxor xmm0,xmm0
        0xeb, 0x02,                   // forward direct stitch to offset 9
        0x0f, 0x0b,                   // skipped bytes
        0x90,                         // normal successor
        0x66, 0x0f, 0xef, 0xc9,       // pxor xmm1,xmm1
        0x0f, 0x0b,                   // deterministic tail
    };
    uint8_t overflow[96 * 5 + 2];
    uint8_t normal_cap[TL_MAX_INSNS + 2];
    for (unsigned i = 0; i < 96; i++) {
        overflow[i * 5] = 0x90;
        memcpy(overflow + i * 5 + 1, (uint8_t[]){0x66, 0x0f, 0xef, 0xc1}, 4);
    }
    overflow[sizeof overflow - 2] = 0x0f;
    overflow[sizeof overflow - 1] = 0x0b;
    memset(normal_cap, 0x90, TL_MAX_INSNS);
    normal_cap[TL_MAX_INSNS] = 0x0f;
    normal_cap[TL_MAX_INSNS + 1] = 0x0b;
    const uint8_t *bytes = scenario == 81 ? overflow : scenario == 84 ? normal_cap
                                                      : scenario == 85 ? direct_stitch
                                                                       : alternating;
    size_t length = scenario == 81 ? sizeof overflow : scenario == 84 ? sizeof normal_cap
                                                           : scenario == 85 ? sizeof direct_stitch
                                                                            : sizeof alternating;
    uint8_t *saved_cache = g_cache;
    uint8_t *saved_cp = g_cp;
    ptrdiff_t saved_rw2rx = g_rw2rx;
    uint64_t saved_generation = g_cache_gen;
    int saved_prof = g_prof;
    uint64_t saved_nonpie_lo = g_nonpie_lo;
    g_cache = arena;
    g_cp = arena + sizeof(struct interp_block);
    g_rw2rx = 0;
    g_cache_gen = UINT64_MAX - scenario;
    map_clear();
    struct interp_block *block = (struct interp_block *)arena;
    translit_fall_fixture_bytes = bytes;
    translit_fall_fixture_guest = (uint64_t)(uintptr_t)bytes;
    translit_fall_fixture_length = length;
    hl_x86_hot_context *context = hl_x86_hot_context_create(translit_fall_fixture_fetch, NULL);
    int result = 121;
    if (context == NULL) goto done;

    jit_body_owner_set *set = jit_body_owner_set_for(g_cache_gen, 1);
    if (set == NULL) goto done;
    if (scenario == 79) g_prof = 1;
    if (scenario == 80)
        atomic_store_explicit(&set->count, JIT_BODY_OWNER_N - (TL_MAX_OWNER_RANGES - 1), memory_order_relaxed);
    uint64_t admitted_before = __atomic_load_n(&translit_mixed_sse_admitted, __ATOMIC_RELAXED);
    uint64_t transitions_before = __atomic_load_n(&translit_mixed_sse_transitions, __ATOMIC_RELAXED);
    uint64_t stitches_before = __atomic_load_n(&translit_stitch_admitted, __ATOMIC_RELAXED);
    void *body = translit_build(context, block, translit_fall_fixture_guest, NULL);
    jit_body_owner_entry *entries = atomic_load_explicit(&set->entry, memory_order_acquire);

    if (scenario == 78) {
        static const uint64_t offsets[] = {0, 1, 5, 8, 12, 13};
        uint32_t published = atomic_load_explicit(&set->count, memory_order_acquire);
        int exact = body != NULL && block->profile_insns == 5 && published == 6 && entries != NULL;
        for (unsigned i = 0; exact && i < sizeof offsets / sizeof offsets[0]; i++)
            exact = entries[i].guest == translit_fall_fixture_guest + offsets[i] &&
                    entries[i].rw_start < entries[i].rw_end &&
                    (i == 0 || entries[i - 1].rw_end <= entries[i].rw_start);
        exact = exact && __atomic_load_n(&translit_mixed_sse_admitted, __ATOMIC_RELAXED) == admitted_before + 1 &&
                __atomic_load_n(&translit_mixed_sse_transitions, __ATOMIC_RELAXED) == transitions_before + 4;
        result = exact ? 0 : 122;
    } else if (scenario == 79) {
        struct cpu cpu = {0};
        uint64_t executed_before = __atomic_load_n(&translit_mixed_sse_executed, __ATOMIC_RELAXED);
        uint64_t transitions_before =
            __atomic_load_n(&translit_mixed_sse_executed_transitions, __ATOMIC_RELAXED);
        uint64_t boundaries_before =
            __atomic_load_n(&translit_mixed_sse_disabled_boundaries, __ATOMIC_RELAXED);
        block->magic = INTERP_BLOCK_MAGIC;
        block->gpc = translit_fall_fixture_guest;
        cpu.rip = translit_fall_fixture_guest;
        cpu.irq = 1;
        hl_backend_tree_begin(1, effective_host_services());
        if (body != NULL) run_block(context, &cpu, block);
        struct hl_backend_tree_summary summary;
        hl_backend_tree_summary(&summary);
        result = body != NULL && cpu.irq == 1 && cpu.reason == R_BRANCH && cpu.v[0] == 0 && cpu.v[1] == 0 &&
                         cpu.v[2] == 0 && cpu.v[3] == 0 &&
                         summary.translated_exit[HL_BACKEND_SHAPE_T_IRQ] == 1 &&
                         summary.mixed_sse_executed == 0 && summary.mixed_sse_executed_transitions == 0 &&
                         summary.mixed_sse_disabled_boundaries == 0 &&
                         __atomic_load_n(&translit_mixed_sse_executed, __ATOMIC_RELAXED) == executed_before &&
                         __atomic_load_n(&translit_mixed_sse_executed_transitions, __ATOMIC_RELAXED) ==
                             transitions_before &&
                         __atomic_load_n(&translit_mixed_sse_disabled_boundaries, __ATOMIC_RELAXED) ==
                             boundaries_before
                     ? 0
                     : 123;
    } else if (scenario == 80) {
        uint64_t guest = 0;
        uint32_t before = JIT_BODY_OWNER_N - (TL_MAX_OWNER_RANGES - 1);
        int refused = body == NULL && atomic_load_explicit(&set->count, memory_order_acquire) == before &&
                      !jit_instruction_map_lookup((uint64_t)(uintptr_t)(arena + sizeof *block), &guest, NULL);
        memset(block, 0, sizeof *block);
        g_cp = arena + sizeof *block;
        atomic_store_explicit(&set->count, JIT_BODY_OWNER_N - TL_MAX_OWNER_RANGES, memory_order_relaxed);
        body = translit_build(context, block, translit_fall_fixture_guest, NULL);
        uint32_t after = atomic_load_explicit(&set->count, memory_order_acquire);
        result = refused && body != NULL && after == JIT_BODY_OWNER_N - TL_MAX_OWNER_RANGES + 6 ? 0 : 124;
    } else if (scenario == 81) {
        uint64_t guest = 0;
        result = body == NULL && atomic_load_explicit(&set->count, memory_order_acquire) == 0 &&
                         !jit_instruction_map_lookup((uint64_t)(uintptr_t)(arena + sizeof *block), &guest, NULL)
                     ? 0
                     : 125;
    } else if (scenario == 82) {
        uint64_t guest = 0;
        int before = body != NULL && jit_body_owner_lookup((uint64_t)(uintptr_t)body, &guest) &&
                     guest == translit_fall_fixture_guest;
        jit_body_owner_after_fork(1);
        int preserved = jit_body_owner_lookup((uint64_t)(uintptr_t)body, &guest) &&
                        guest == translit_fall_fixture_guest;
        jit_body_owner_after_fork(0);
        int rebuilt = !jit_body_owner_lookup((uint64_t)(uintptr_t)body, &guest);
        result = before && preserved && rebuilt ? 0 : 126;
    } else if (scenario == 83) {
        uint64_t guest = 0;
        const uint64_t dirty[1][2] = {{translit_fall_fixture_guest + 5,
                                       translit_fall_fixture_guest + 6}};
        int published = body != NULL && block->guest_start <= dirty[0][0] &&
                        block->guest_end >= dirty[0][1] &&
                        jit_body_owner_lookup((uint64_t)(uintptr_t)body, &guest) &&
                        guest == translit_fall_fixture_guest;
        if (published)
            map_put(translit_fall_fixture_guest, block->guest_start, block->guest_end, block, block);
        int reachable = published && map_host(translit_fall_fixture_guest) == block;
        uint32_t invalidated = map_invalidate_source_ranges(dirty, 1);
        int retained_for_fault = invalidated == 1 && map_host(translit_fall_fixture_guest) == NULL &&
                                 jit_body_owner_lookup((uint64_t)(uintptr_t)body, &guest) &&
                                 guest == translit_fall_fixture_guest;
        jit_body_owner_drop_generation(g_cache_gen);
        int retired = !jit_body_owner_lookup((uint64_t)(uintptr_t)body, &guest);
        result = reachable && retained_for_fault && retired ? 0 : 127;
    } else if (scenario == 84) {
        uint32_t published = atomic_load_explicit(&set->count, memory_order_acquire);
        int exact = body != NULL && block->profile_insns == TL_MAX_INSNS && published == 2;
        for (uint32_t offset = 0; exact && offset < block->host_len; offset++) {
            uint64_t guest = 0;
            exact = jit_body_owner_lookup((uint64_t)(uintptr_t)body + offset, &guest) &&
                    guest >= translit_fall_fixture_guest && guest <= translit_fall_fixture_guest + TL_MAX_INSNS;
        }
        for (uint32_t offset = 0; exact && offset < TL_MAX_INSNS; offset++) {
            uint64_t guest = 0;
            exact = jit_instruction_map_lookup((uint64_t)(uintptr_t)body + offset, &guest, NULL) &&
                    guest == translit_fall_fixture_guest + offset;
        }
        result = exact ? 0 : 128;
    } else if (scenario == 85) {
        uint32_t published = atomic_load_explicit(&set->count, memory_order_acquire);
        int exact = body != NULL && block->profile_insns == 5 && published == 6 &&
                    __atomic_load_n(&translit_stitch_admitted, __ATOMIC_RELAXED) == stitches_before + 1;
        for (uint32_t offset = 0; exact && offset < block->host_len; offset++) {
            uint64_t owner_guest = 0, instruction_guest = 0;
            exact = jit_body_owner_lookup((uint64_t)(uintptr_t)body + offset, &owner_guest) &&
                    jit_instruction_map_lookup((uint64_t)(uintptr_t)body + offset, &instruction_guest, NULL) &&
                    owner_guest == instruction_guest;
        }
        result = exact ? 0 : 129;
    } else if (scenario == 86) {
        uint64_t old_generation = g_cache_gen;
        atomic_store_explicit(&set->count, JIT_BODY_OWNER_N - (TL_MAX_OWNER_RANGES - 1), memory_order_relaxed);
        int low = jit_cache_needs_rotation();
        atomic_store_explicit(&set->count, JIT_BODY_OWNER_N - TL_MAX_OWNER_RANGES, memory_order_relaxed);
        int exact = !jit_cache_needs_rotation();
        jit_body_owner_drop_generation(old_generation);
        g_cache_gen = old_generation - 1;
        int fresh = !jit_body_owner_needs_rotation(g_cache_gen) &&
                    jit_body_owner_set_for(old_generation, 0) == NULL;
        result = low && exact && fresh ? 0 : 130;
    } else if (scenario == 87) {
        uint64_t executed_before = __atomic_load_n(&translit_mixed_sse_executed, __ATOMIC_RELAXED);
        uint64_t transitions_before =
            __atomic_load_n(&translit_mixed_sse_executed_transitions, __ATOMIC_RELAXED);
        uint64_t boundaries_before =
            __atomic_load_n(&translit_mixed_sse_disabled_boundaries, __ATOMIC_RELAXED);
        translit_mixed_profile_completed(0);
        translit_mixed_profile_completed(TL_MIXED_PROFILE_MAGIC | 191u);
        translit_mixed_profile_completed(TL_MIXED_PROFILE_MAGIC | TL_MIXED_PROFILE_DISABLED_BOUNDARY);
        translit_mixed_profile_completed(TL_MIXED_PROFILE_MAGIC | TL_MIXED_PROFILE_DISABLED_BOUNDARY | 1u);
        result = __atomic_load_n(&translit_mixed_sse_executed, __ATOMIC_RELAXED) == executed_before + 1 &&
                         __atomic_load_n(&translit_mixed_sse_executed_transitions, __ATOMIC_RELAXED) ==
                             transitions_before + 191 &&
                         __atomic_load_n(&translit_mixed_sse_disabled_boundaries, __ATOMIC_RELAXED) ==
                             boundaries_before + 1
                     ? 0
                     : 131;
    } else if (scenario == 88) {
        uint64_t entries_before = __atomic_load_n(&translit_entries, __ATOMIC_RELAXED);
        uint64_t displaced_before = __atomic_load_n(&translit_displaced_entries, __ATOMIC_RELAXED);
        block->magic = INTERP_BLOCK_MAGIC;
        block->gpc = translit_fall_fixture_guest;
        g_nonpie_lo = 1;

        struct cpu off = {0};
        off.rip = translit_fall_fixture_guest;
        off.irq = 1;
        off.v[0] = off.v[1] = off.v[2] = off.v[3] = UINT64_MAX;
        g_prof = 0;
        if (body != NULL) run_block(context, &off, block);
        int off_exact = body != NULL && off.reason == R_BRANCH && off.v[0] == 0 && off.v[1] == 0 &&
                        off.v[2] == 0 && off.v[3] == 0 &&
                        __atomic_load_n(&translit_entries, __ATOMIC_RELAXED) == entries_before &&
                        __atomic_load_n(&translit_displaced_entries, __ATOMIC_RELAXED) == displaced_before;

        struct cpu on = {0};
        on.rip = translit_fall_fixture_guest;
        on.irq = 1;
        on.v[0] = on.v[1] = on.v[2] = on.v[3] = UINT64_MAX;
        g_prof = 1;
        if (body != NULL) run_block(context, &on, block);
        result = off_exact && on.reason == R_BRANCH && on.v[0] == 0 && on.v[1] == 0 && on.v[2] == 0 &&
                         on.v[3] == 0 &&
                         __atomic_load_n(&translit_entries, __ATOMIC_RELAXED) == entries_before + 1 &&
                         __atomic_load_n(&translit_displaced_entries, __ATOMIC_RELAXED) == displaced_before + 1
                     ? 0
                     : 132;
    }

done:
    map_clear();
    translit_fall_fixture_bytes = NULL;
    if (context != NULL) hl_x86_hot_context_destroy(context);
    jit_body_owner_drop_generation(g_cache_gen);
    g_cache = saved_cache;
    g_cp = saved_cp;
    g_rw2rx = saved_rw2rx;
    g_cache_gen = saved_generation;
    g_prof = saved_prof;
    g_nonpie_lo = saved_nonpie_lo;
    munmap(arena, arena_size);
    return result;
}

static struct interp_block *translit_jcc_ibtc_test_build(hl_x86_hot_context *context, uint64_t guest) {
    if (!translit_jcc_ibtc_stub_init()) return NULL;
    while ((uintptr_t)g_cp & 15)
        g_cp++;
    struct interp_block *block = (struct interp_block *)g_cp;
    g_cp += sizeof *block;
    memset(block, 0, sizeof *block);
    block->magic = INTERP_BLOCK_MAGIC;
    block->gpc = guest;
    block->generation = g_cache_gen;
    block->guest_start = guest;
    block->guest_end = guest + 1;
    uint64_t jcc_ibtc_sites = 0;
    (void)translit_build(context, block, guest, &jcc_ibtc_sites);
    map_put(guest, block->guest_start, block->guest_end, block, block);
    translit_jcc_ibtc_count(TL_JCC_IBTC_COUNT_EMITTED, jcc_ibtc_sites);
    return block;
}

static int translit_jcc_ibtc_fixture_test(unsigned scenario) {
    if (scenario == 89) {
        uint8_t on[2048] = {0}, off[2048] = {0};
        hl_x64_asm a_on = {on, on + sizeof on, 0}, a_off = {off, off + sizeof off, 0};
        (void)hl_option_unset("HL_TRANSLIT_JCC_IBTC_DISABLE");
        translit_jcc_ibtc_stub_entry = on + 1024;
        translit_jcc_ibtc_site_ranges on_ranges =
            translit_emit_jcc_ibtc(&a_on, UINT64_C(0x12345678));
        if (hl_option_set("HL_TRANSLIT_JCC_IBTC_DISABLE", "1", 1) != 0) return 132;
        translit_jcc_ibtc_stub_entry = off + 1024;
        translit_jcc_ibtc_site_ranges off_ranges =
            translit_emit_jcc_ibtc(&a_off, UINT64_C(0x12345678));
        (void)hl_option_unset("HL_TRANSLIT_JCC_IBTC_DISABLE");
        size_t on_len = (size_t)(a_on.cursor - on), off_len = (size_t)(a_off.cursor - off);
        return !a_on.overflow && !a_off.overflow && on_len == 33 && off_len == 33 &&
                       memcmp(on, off, on_len) == 0 && on_ranges.save_end == on + 9 &&
                       on_ranges.target_end == on + 19 && on_ranges.rip_end == on + 28 &&
                       on_ranges.end == on + 33 && off_ranges.save_end == off + 9 &&
                       off_ranges.target_end == off + 19 && off_ranges.rip_end == off + 28 &&
                       off_ranges.end == off + 33 && on[28] == 0xe9
                   ? 0
                   : 133;
    }

    const size_t arena_size = 65536;
    uint8_t *arena = mmap(NULL, arena_size, PROT_READ | PROT_WRITE | PROT_EXEC,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 134;
    static _Alignas(4096) uint8_t guest_bytes[4096];
    memset(guest_bytes, 0x0f, sizeof guest_bytes);
    const size_t target_offset = 0x40;
    if (scenario == 98) {
        // Eight legal taken-side sites followed by enough one-byte ordinary
        // instructions to hit the architectural instruction cap. The rejected
        // 412-byte inline probe exceeded TL_MAX_BYTES here; compact sources fit
        // without changing either cap.
        for (unsigned i = 0; i < TL_STITCH_MAX_JMPS; i++) {
            guest_bytes[i * 2] = 0x74;     // je next
            guest_bytes[i * 2 + 1] = 0x00;
        }
        memset(guest_bytes + TL_STITCH_MAX_JMPS * 2, 0x90,
               TL_MAX_INSNS - TL_STITCH_MAX_JMPS);
    } else {
        guest_bytes[0] = 0x31; // xor eax,eax -> ZF=1
        guest_bytes[1] = 0xc0;
        guest_bytes[2] = 0x74; // je target
        guest_bytes[3] = (uint8_t)(target_offset - 4);
        guest_bytes[4] = 0x0f; // deterministic unsupported fall successor
        guest_bytes[5] = 0x0b;
        guest_bytes[target_offset + 0] = 0xb8; // mov $42,%eax
        guest_bytes[target_offset + 1] = 42;
        guest_bytes[target_offset + 2] = 0;
        guest_bytes[target_offset + 3] = 0;
        guest_bytes[target_offset + 4] = 0;
        guest_bytes[target_offset + 5] = 0x0f;
        guest_bytes[target_offset + 6] = 0x0b;
    }

    g_cache = arena;
    g_cp = arena;
    g_rw2rx = 0;
    g_cache_gen = UINT64_MAX - scenario;
    map_clear();
    translit_fall_fixture_bytes = guest_bytes;
    translit_fall_fixture_guest = (uint64_t)(uintptr_t)guest_bytes;
    translit_fall_fixture_length = sizeof guest_bytes;
    hl_x86_hot_context *context = hl_x86_hot_context_create(translit_fall_fixture_fetch, NULL);
    if (context == NULL) return 135;
    hl_backend_tree_begin(1, effective_host_services());
    uint64_t source = translit_fall_fixture_guest;
    uint64_t target = source + target_offset;
    uint64_t emitted_before = __atomic_load_n(&translit_jcc_ibtc_emitted, __ATOMIC_RELAXED);
    uint64_t hit_before = __atomic_load_n(&translit_jcc_ibtc_hit, __ATOMIC_RELAXED);
    uint64_t miss_before = __atomic_load_n(&translit_jcc_ibtc_miss, __ATOMIC_RELAXED);
    uint64_t irq_before = __atomic_load_n(&translit_jcc_ibtc_irq, __ATOMIC_RELAXED);
    uint64_t fill_before = __atomic_load_n(&translit_jcc_ibtc_fill_count, __ATOMIC_RELAXED);
    uint64_t collision_before = __atomic_load_n(&translit_jcc_ibtc_collision, __ATOMIC_RELAXED);
    uint64_t suppressed_before = __atomic_load_n(&translit_jcc_ibtc_suppressed, __ATOMIC_RELAXED);
    uint64_t invalid_before = __atomic_load_n(&translit_jcc_ibtc_invalid_refusal, __ATOMIC_RELAXED);
    int result = 136;

    if (scenario == 91) {
        struct interp_block *target_block = translit_jcc_ibtc_test_build(context, target);
        struct interp_block *source_block = translit_jcc_ibtc_test_build(context, source);
        struct cpu cpu = {0};
        cpu.rip = source;
        cpu.r[4] = (uint64_t)(uintptr_t)(arena + arena_size - 64);
        cpu.ibtc_base = (uint64_t)g_ibtc;
        run_block(context, &cpu, source_block);
        result = target_block->host_entry_off != 0 && source_block->host_entry_off != 0 && cpu.r[0] == 42 &&
                         cpu.rip == target + 5 &&
                         __atomic_load_n(&translit_jcc_ibtc_emitted, __ATOMIC_RELAXED) == emitted_before
                     ? 0
                     : 137;
        goto done_ibtc;
    }

    struct interp_block *source_block = translit_jcc_ibtc_test_build(context, source);
    if (source_block == NULL) {
        result = 150;
        goto done_ibtc;
    }
    if (source_block->host_entry_off == 0) goto done_ibtc;
    if (scenario == 98) {
        uint32_t published = 0;
        jit_body_owner_set *set = jit_body_owner_set_for(g_cache_gen, 0);
        if (set != NULL) published = atomic_load_explicit(&set->count, memory_order_acquire);
        int covered = source_block->host_len < TL_MAX_BYTES && published == 37;
        uint8_t *body = (uint8_t *)source_block + source_block->host_entry_off;
        for (uint32_t offset = 0; covered && offset < source_block->host_len; offset++) {
            uint64_t owner = 0;
            covered = jit_body_owner_lookup((uint64_t)(uintptr_t)(body + offset), &owner);
        }
        result = source_block->profile_insns == TL_MAX_INSNS && covered &&
                         __atomic_load_n(&translit_jcc_ibtc_emitted, __ATOMIC_RELAXED) ==
                             emitted_before + TL_STITCH_MAX_JMPS
                     ? 0
                     : 145;
        goto done_ibtc;
    }
    if (scenario == 99) {
        result = translit_jcc_ibtc_signal_stage_test() ? 0 : 146;
        goto done_ibtc;
    }
    struct cpu cpu = {0};
    cpu.rip = source;
    cpu.r[1] = UINT64_C(0x1122334455667788);
    cpu.r[4] = (uint64_t)(uintptr_t)(arena + arena_size - 64);
    cpu.v[0] = UINT64_C(0x8877665544332211);
    cpu.ibtc_base = (uint64_t)g_ibtc;
    run_block(context, &cpu, source_block);
    uint64_t first_flags = cpu.nzcv;
    if (cpu.rip != target || cpu.jcc_ibtc_miss != TL_JCC_IBTC_MISS ||
        __atomic_load_n(&translit_jcc_ibtc_miss, __ATOMIC_RELAXED) != miss_before + 1)
        goto done_ibtc;
    struct interp_block *target_block = translit_jcc_ibtc_test_build(context, target);

    if (scenario == 97) {
        target_block->generation--;
        translit_jcc_ibtc_fill(&cpu, target_block);
        cpu = (struct cpu){0};
        cpu.rip = source;
        cpu.r[4] = (uint64_t)(uintptr_t)(arena + arena_size - 64);
        cpu.ibtc_base = (uint64_t)g_ibtc;
        run_block(context, &cpu, source_block);
        translit_jcc_ibtc_finalize_cpu(&cpu);
        result = cpu.jcc_ibtc_miss == 0 &&
                         __atomic_load_n(&translit_jcc_ibtc_miss, __ATOMIC_RELAXED) == miss_before + 2 &&
                         __atomic_load_n(&translit_jcc_ibtc_invalid_refusal, __ATOMIC_RELAXED) == invalid_before + 2
                     ? 0
                     : 144;
        goto done_ibtc;
    }

    if (scenario == 96) {
        if (hl_option_set("HL_TRANSLIT_JCC_IBTC_DISABLE", "1", 1) != 0) goto done_ibtc;
        translit_jcc_ibtc_fill(&cpu, target_block);
        (void)hl_option_unset("HL_TRANSLIT_JCC_IBTC_DISABLE");
        ibtc_ent empty = ibtc_snapshot(&g_ibtc[ibtc_index(target)]);
        result = empty.target == 0 && empty.body == NULL &&
                         __atomic_load_n(&translit_jcc_ibtc_suppressed, __ATOMIC_RELAXED) == suppressed_before + 1
                     ? 0
                     : 138;
        goto done_ibtc;
    }

    translit_jcc_ibtc_fill(&cpu, target_block);
    ibtc_ent *cell = &g_ibtc[ibtc_index(target)];
    ibtc_ent published = ibtc_snapshot(cell);
    if (published.target != target || published.body != (uint8_t *)target_block + target_block->host_entry_off ||
        __atomic_load_n(&translit_jcc_ibtc_fill_count, __ATOMIC_RELAXED) != fill_before + 1)
        goto done_ibtc;

    if (scenario == 92) {
        struct interp_block *collision_block = (struct interp_block *)g_cp;
        g_cp += sizeof *collision_block + 16;
        memset(collision_block, 0, sizeof *collision_block);
        uint64_t collision_target = target + ((uint64_t)IBTC_N << 2);
        collision_block->magic = INTERP_BLOCK_MAGIC;
        collision_block->gpc = collision_target;
        collision_block->generation = g_cache_gen;
        collision_block->host_entry_off = sizeof *collision_block;
        collision_block->host_len = 1;
        cpu.rip = collision_target;
        cpu.jcc_ibtc_miss = TL_JCC_IBTC_MISS;
        translit_jcc_ibtc_fill(&cpu, collision_block);
        ibtc_ent collided = ibtc_snapshot(cell);
        cpu = (struct cpu){0};
        cpu.rip = source;
        cpu.r[4] = (uint64_t)(uintptr_t)(arena + arena_size - 64);
        cpu.ibtc_base = (uint64_t)g_ibtc;
        run_block(context, &cpu, source_block);
        result = collided.target == collision_target && cpu.rip == target &&
                         cpu.jcc_ibtc_miss == TL_JCC_IBTC_MISS &&
                         __atomic_load_n(&translit_jcc_ibtc_miss, __ATOMIC_RELAXED) == miss_before + 2 &&
                         __atomic_load_n(&translit_jcc_ibtc_collision, __ATOMIC_RELAXED) == collision_before + 1
                     ? 0
                     : 139;
        goto done_ibtc;
    }

    if (scenario == 93) {
        cpu = (struct cpu){0};
        cpu.rip = source;
        cpu.r[1] = UINT64_C(0x1122334455667788);
        cpu.r[4] = (uint64_t)(uintptr_t)(arena + arena_size - 64);
        cpu.v[0] = UINT64_C(0x8877665544332211);
        cpu.ibtc_base = (uint64_t)g_ibtc;
        cpu.irq = 1;
        run_block(context, &cpu, source_block);
        result = cpu.rip == target && cpu.r[1] == UINT64_C(0x1122334455667788) &&
                         cpu.r[4] == (uint64_t)(uintptr_t)(arena + arena_size - 64) &&
                         cpu.v[0] == UINT64_C(0x8877665544332211) && cpu.jcc_ibtc_miss == 0 &&
                         __atomic_load_n(&translit_jcc_ibtc_irq, __ATOMIC_RELAXED) == irq_before + 1 &&
                         __atomic_load_n(&translit_jcc_ibtc_hit, __ATOMIC_RELAXED) == hit_before
                     ? 0
                     : 140;
        goto done_ibtc;
    }

    if (scenario == 94) {
        const uint64_t dirty[1][2] = {{target, target + 1}};
        uint32_t invalidated = map_invalidate_source_ranges(dirty, 1);
        ibtc_ent cleared = ibtc_snapshot(cell);
        int source_live = map_host(source) == source_block;
        struct interp_block *replacement = translit_jcc_ibtc_test_build(context, target);
        cpu.rip = target;
        cpu.jcc_ibtc_miss = TL_JCC_IBTC_MISS;
        translit_jcc_ibtc_fill(&cpu, replacement);
        ibtc_ent repaired = ibtc_snapshot(cell);
        map_clear();
        ibtc_ent epoch_cleared = ibtc_snapshot(cell);
        result = invalidated == 1 && source_live && cleared.target == 0 && cleared.body == NULL &&
                         repaired.target == target && repaired.body != NULL && epoch_cleared.target == 0 &&
                         epoch_cleared.body == NULL
                     ? 0
                     : 141;
        goto done_ibtc;
    }

    if (scenario == 95) {
        uint64_t collision_target = target + ((uint64_t)IBTC_N << 2);
        translit_jcc_ibtc_stub_header *header = (translit_jcc_ibtc_stub_header *)g_cache;
        pid_t child = fork();
        if (child == 0) {
            // Fork-preserve reconstructs process-local pointers solely from the
            // generation-owned offset header before any inherited source runs.
            translit_jcc_ibtc_stub_entry = NULL;
            translit_jcc_ibtc_stub_rsp_canonical = NULL;
            translit_jcc_ibtc_stub_flags_canonical = NULL;
            translit_jcc_ibtc_stub_end = NULL;
            if (!translit_jcc_ibtc_stub_init() ||
                translit_jcc_ibtc_stub_entry != g_cache + header->entry_off ||
                translit_jcc_ibtc_stub_rsp_canonical != g_cache + header->rsp_canonical_off ||
                translit_jcc_ibtc_stub_flags_canonical != g_cache + header->flags_canonical_off ||
                translit_jcc_ibtc_stub_end != g_cache + header->end_off)
                _exit(1);
            struct cpu child_cpu = {0};
            child_cpu.rip = source;
            child_cpu.r[4] = (uint64_t)(uintptr_t)(arena + arena_size - 64);
            child_cpu.ibtc_base = (uint64_t)g_ibtc;
            run_block(context, &child_cpu, source_block);
            if (child_cpu.r[0] != 42 || child_cpu.rip != target + 5) _exit(2);
            ibtc_publish(cell, collision_target, (void *)UINT64_C(0x12345000));
            ibtc_ent child_pair = ibtc_snapshot(cell);
            _exit(child_pair.target == collision_target ? 0 : 3);
        }
        int status = 0;
        ibtc_ent parent_pair = ibtc_snapshot(cell);
        result = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0 &&
                         parent_pair.target == target && parent_pair.body == published.body
                     ? 0
                     : 142;
        goto done_ibtc;
    }

    cpu.rip = source;
    cpu.jcc_ibtc_miss = 0;
    run_block(context, &cpu, source_block);
    result = cpu.r[0] == 42 && cpu.r[1] == UINT64_C(0x1122334455667788) &&
                     cpu.v[0] == UINT64_C(0x8877665544332211) && cpu.rip == target + 5 &&
                     cpu.nzcv == first_flags &&
                     __atomic_load_n(&translit_jcc_ibtc_emitted, __ATOMIC_RELAXED) == emitted_before + 1 &&
                     __atomic_load_n(&translit_jcc_ibtc_hit, __ATOMIC_RELAXED) == hit_before + 1 &&
                     __atomic_load_n(&translit_jcc_ibtc_miss, __ATOMIC_RELAXED) == miss_before + 1 &&
                     __atomic_load_n(&translit_jcc_ibtc_irq, __ATOMIC_RELAXED) == irq_before
                 ? 0
                 : 143;

done_ibtc:
    (void)hl_option_unset("HL_TRANSLIT_JCC_IBTC_DISABLE");
    translit_fall_fixture_bytes = NULL;
    hl_x86_hot_context_destroy(context);
    jit_body_owner_drop_generation(g_cache_gen);
    munmap(arena, arena_size);
    return result;
}
#endif

// Enter a transliterated block. The sigsetjmp pad run_block already arms covers this call: a guest fault
// inside emitted code is captured by translit_signal_capture and leaves by the same siglongjmp the
// interpreter uses, which restores the host callee-saved registers the block is currently holding guest
// values in.
static void translit_run(struct cpu *cpu, struct interp_block *block) {
    // These counters exist only in the profile report. Normal launches must not serialize every translated
    // entry on their shared cache lines to maintain telemetry nobody can observe.
    if (g_prof) {
        (void)__atomic_fetch_add(&translit_entries, 1, __ATOMIC_RELAXED);
        if (g_nonpie_lo) (void)__atomic_fetch_add(&translit_displaced_entries, 1, __ATOMIC_RELAXED);
    }
    cpu->mmscratch[0] = translit_flags_in(cpu);
    hl_x86_translit_enter(cpu, (uint8_t *)block + block->host_entry_off);
    translit_flags_out(cpu, cpu->mmscratch[0]);
#if defined(HL_NATIVE_TEST_HOOKS)
    if (block->profile_jcc_fall_stitches != 0)
        (void)__atomic_fetch_add(&translit_jcc_fall_executed, block->profile_jcc_fall_stitches, __ATOMIC_RELAXED);
#endif
}

#if defined(HL_NATIVE_TEST_HOOKS)
static void translit_unsupported_top(const translit_unsupported_form *forms, unsigned form_count,
                                     uint64_t top_key[8], uint64_t top_count[8]) {
    for (unsigned slot = 0; slot < form_count; slot++) {
        if (atomic_load_explicit(&forms[slot].state, memory_order_acquire) != 2) continue;
        uint64_t key = forms[slot].key;
        uint64_t count = atomic_load_explicit(&forms[slot].count, memory_order_relaxed);
        unsigned rank = 0;
        while (rank < 8 &&
               (top_count[rank] > count || (top_count[rank] == count && top_key[rank] <= key)))
            rank++;
        if (rank == 8) continue;
        for (unsigned move = 7; move > rank; move--) {
            top_key[move] = top_key[move - 1];
            top_count[move] = top_count[move - 1];
        }
        top_key[rank] = key;
        top_count[rank] = count;
    }
}

static int translit_unsupported_report(char *out, size_t size) {
    uint64_t top_key[8] = {0}, top_count[8] = {0};
    uint64_t site_top_pc = 0, site_top_key = 0, site_top_count = 0;
    translit_unsupported_top(translit_unsupported_forms, TL_UNSUPPORTED_FORM_N, top_key, top_count);
    for (unsigned slot = 0; slot < TL_UNSUPPORTED_SITE_N; slot++) {
        if (atomic_load_explicit(&translit_unsupported_sites[slot].state, memory_order_acquire) != 2) continue;
        uint64_t count = atomic_load_explicit(&translit_unsupported_sites[slot].count, memory_order_relaxed);
        if (count > site_top_count) {
            site_top_pc = translit_unsupported_sites[slot].pc;
            site_top_key = translit_unsupported_sites[slot].key;
            site_top_count = count;
        }
    }
    uint64_t keyed = atomic_load_explicit(&translit_unsupported_keyed, memory_order_relaxed);
    uint64_t overflow = atomic_load_explicit(&translit_unsupported_form_overflow, memory_order_relaxed);
    uint64_t site_unique = atomic_load_explicit(&translit_unsupported_site_unique, memory_order_relaxed);
    uint64_t site_repeats = atomic_load_explicit(&translit_unsupported_site_repeats, memory_order_relaxed);
    uint64_t site_overflow = atomic_load_explicit(&translit_unsupported_site_overflow, memory_order_relaxed);
    int used = snprintf(out, size,
                        "[diag] unsupported total=%llu keyed=%llu overflow=%llu forms=%llu collisions=%llu"
                        " site_events=%llu sites=%llu repeats=%llu site_overflow=%llu site_collisions=%llu"
                        " deferred_excluded=%llu top=",
                        (unsigned long long)(keyed + overflow), (unsigned long long)keyed,
                        (unsigned long long)overflow,
                        (unsigned long long)atomic_load_explicit(&translit_unsupported_form_unique,
                                                                memory_order_relaxed),
                        (unsigned long long)atomic_load_explicit(&translit_unsupported_form_collisions,
                                                                memory_order_relaxed),
                        (unsigned long long)(site_unique + site_repeats + site_overflow),
                        (unsigned long long)site_unique, (unsigned long long)site_repeats,
                        (unsigned long long)site_overflow,
                        (unsigned long long)atomic_load_explicit(&translit_unsupported_site_collisions,
                                                                memory_order_relaxed),
                        (unsigned long long)atomic_load_explicit(&translit_unsupported_deferred_excluded,
                                                                memory_order_relaxed));
    if (used < 0 || (size_t)used >= size) return -1;
    for (unsigned rank = 0; rank < 8 && top_count[rank] != 0; rank++) {
        int added = snprintf(out + used, size - (size_t)used, "%s%llx:%llu", rank == 0 ? "" : ",",
                             (unsigned long long)top_key[rank], (unsigned long long)top_count[rank]);
        if (added < 0 || (size_t)added >= size - (size_t)used) return -1;
        used += added;
    }
    int site_added = snprintf(out + used, size - (size_t)used, " site_top=%llx:%llx:%llu",
                              (unsigned long long)site_top_pc, (unsigned long long)site_top_key,
                              (unsigned long long)site_top_count);
    if (site_added < 0 || (size_t)site_added >= size - (size_t)used) return -1;
    used += site_added;
    if ((size_t)used + 1u >= size) return -1;
    out[used++] = '\n';
    out[used] = '\0';
    return used;
}
#endif

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
    uint64_t jcc_link_admitted = __atomic_load_n(&translit_jcc_link_admitted, __ATOMIC_RELAXED);
    uint64_t jcc_link_taken = __atomic_load_n(&translit_jcc_link_taken, __ATOMIC_RELAXED);
    uint64_t jcc_link_irq_fallback = __atomic_load_n(&translit_jcc_link_irq_fallback, __ATOMIC_RELAXED);
    uint64_t jcc_link_dispatcher = __atomic_load_n(&translit_jcc_link_dispatcher, __ATOMIC_RELAXED);
#else
    uint64_t jcc_fall_executed = 0;
    uint64_t jcc_link_admitted = 0, jcc_link_taken = 0, jcc_link_irq_fallback = 0, jcc_link_dispatcher = 0;
#endif
    uint64_t rip_indirect_lowered = __atomic_load_n(&translit_rip_indirect_lowered, __ATOMIC_RELAXED);
    uint64_t provenance_fallback = __atomic_load_n(&translit_provenance_fallback, __ATOMIC_RELAXED);
    uint64_t body_owner_recovered = __atomic_load_n(&translit_body_owner_recovered, __ATOMIC_RELAXED);
    uint64_t mixed_sse_encounters = __atomic_load_n(&translit_mixed_sse_encounters, __ATOMIC_RELAXED);
    uint64_t mixed_sse_admitted = __atomic_load_n(&translit_mixed_sse_admitted, __ATOMIC_RELAXED);
    uint64_t mixed_sse_transitions = __atomic_load_n(&translit_mixed_sse_transitions, __ATOMIC_RELAXED);
    uint64_t mixed_sse_executed = __atomic_load_n(&translit_mixed_sse_executed, __ATOMIC_RELAXED);
    uint64_t mixed_sse_executed_transitions =
        __atomic_load_n(&translit_mixed_sse_executed_transitions, __ATOMIC_RELAXED);
    uint64_t mixed_sse_disabled_boundaries =
        __atomic_load_n(&translit_mixed_sse_disabled_boundaries, __ATOMIC_RELAXED);
#if defined(HL_NATIVE_TEST_HOOKS)
    uint64_t body_owner_published = __atomic_load_n(&translit_body_owner_published, __ATOMIC_RELAXED);
    uint64_t body_owner_low_rotations = g_body_owner_low_test_rotations;
    uint64_t body_owner_low_retranslations = g_body_owner_low_test_retranslations;
    uint64_t sse2_runs_admitted = __atomic_load_n(&translit_sse2_runs_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_instructions_admitted =
        __atomic_load_n(&translit_sse2_instructions_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_target_runs = __atomic_load_n(&translit_sse2_target_runs, __ATOMIC_RELAXED);
    uint64_t sse2_next_family_runs = __atomic_load_n(&translit_sse2_next_family_runs, __ATOMIC_RELAXED);
    uint64_t sse2_store_instructions = __atomic_load_n(&translit_sse2_store_instructions, __ATOMIC_RELAXED);
    uint64_t sse2_store_movups = __atomic_load_n(&translit_sse2_store_movups, __ATOMIC_RELAXED);
    uint64_t sse2_store_movaps = __atomic_load_n(&translit_sse2_store_movaps, __ATOMIC_RELAXED);
    uint64_t sse2_store_movdqu = __atomic_load_n(&translit_sse2_store_movdqu, __ATOMIC_RELAXED);
    uint64_t sse2_store_family_runs = __atomic_load_n(&translit_sse2_store_family_runs, __ATOMIC_RELAXED);
    uint64_t sse2_pxor_admitted = __atomic_load_n(&translit_sse2_pxor_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_pxor_runs_admitted = __atomic_load_n(&translit_sse2_pxor_runs_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_punpcklqdq_admitted =
        __atomic_load_n(&translit_sse2_punpcklqdq_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_punpcklqdq_runs_admitted =
        __atomic_load_n(&translit_sse2_punpcklqdq_runs_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_movd_admitted = __atomic_load_n(&translit_sse2_movd_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_movd_runs_admitted = __atomic_load_n(&translit_sse2_movd_runs_admitted, __ATOMIC_RELAXED);
    uint64_t sse2_movhlps_admitted = __atomic_load_n(&translit_sse2_movhlps_admitted, __ATOMIC_RELAXED);
    uint64_t fs_mem_admitted = __atomic_load_n(&translit_fs_mem_admitted, __ATOMIC_RELAXED);
    uint64_t fs_fixture_admitted = __atomic_load_n(&translit_fs_fixture_admitted, __ATOMIC_RELAXED);
#else
    uint64_t body_owner_published = 0;
    uint64_t body_owner_low_rotations = 0, body_owner_low_retranslations = 0;
    uint64_t sse2_runs_admitted = 0, sse2_instructions_admitted = 0, sse2_target_runs = 0,
             sse2_next_family_runs = 0, sse2_store_instructions = 0;
    uint64_t sse2_store_movups = 0, sse2_store_movaps = 0, sse2_store_movdqu = 0;
    uint64_t sse2_store_family_runs = 0;
    uint64_t sse2_pxor_admitted = 0, sse2_pxor_runs_admitted = 0;
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
#if defined(HL_NATIVE_TEST_HOOKS)
    if (hl_option_get("HL_TRANSLIT_PROFILE_WIDE_TEST") != NULL) {
        blocks = entries = declined = stitch_candidates = stitch_admitted = UINT64_MAX;
        rip_indirect_lowered = provenance_fallback = body_owner_recovered = body_owner_published = UINT64_MAX;
        body_owner_low_rotations = body_owner_low_retranslations = UINT64_MAX;
        mixed_sse_encounters = mixed_sse_admitted = mixed_sse_transitions = UINT64_MAX;
        mixed_sse_executed = mixed_sse_executed_transitions = mixed_sse_disabled_boundaries = UINT64_MAX;
        sse2_runs_admitted = sse2_instructions_admitted = sse2_target_runs = sse2_next_family_runs = UINT64_MAX;
        sse2_store_instructions = sse2_store_movups = sse2_store_movaps = sse2_store_movdqu = UINT64_MAX;
        sse2_store_family_runs = sse2_pxor_admitted = sse2_pxor_runs_admitted = UINT64_MAX;
        sse2_punpcklqdq_admitted = sse2_punpcklqdq_runs_admitted = UINT64_MAX;
        sse2_movd_admitted = sse2_movd_runs_admitted = UINT64_MAX;
        sse2_movhlps_admitted = UINT64_MAX;
        fs_mem_admitted = UINT64_MAX;
        fs_fixture_admitted = UINT64_MAX;
        displaced_blocks = displaced_entries = displaced_operand_declined = UINT64_MAX;
        displaced_sse2_memory_declined = displaced_riprel_projected = displaced_lea_low = UINT64_MAX;
        displaced_riprel_unreachable = displaced_riprel_lowered = displaced_scratch_lowered = UINT64_MAX;
        displaced_lea_lowered = natural_abs32_lowered = natural_lea_lowered = UINT64_MAX;
        displaced_refusal_count = displaced_refusal_prefix = displaced_refusal_width = UINT64_MAX;
        displaced_refusal_rsp = displaced_refusal_opcode_other = UINT64_MAX;
        jcc_fall_candidates = jcc_fall_admitted = jcc_fall_page_refused = UINT64_MAX;
        jcc_fall_successor_page_refused = jcc_fall_decode_refused = jcc_fall_executed = UINT64_MAX;
        jcc_link_admitted = jcc_link_taken = jcc_link_irq_fallback = jcc_link_dispatcher = UINT64_MAX;
    }
#endif
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
                        " jcc_fall_decode_refused=%llu jcc_fall_executed=%llu"
                        " jcc_link_admitted=%llu jcc_link_taken=%llu jcc_link_irq_fallback=%llu"
                        " jcc_link_dispatcher=%llu mixed_sse_encounters=%llu"
                        " mixed_sse_admitted=%llu mixed_sse_transitions=%llu"
                        " mixed_sse_executed=%llu mixed_sse_executed_transitions=%llu"
                        " mixed_sse_disabled_boundaries=%llu body_owner_low_rotations=%llu"
                        " body_owner_low_retranslations=%llu\n",
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
                        (unsigned long long)jcc_fall_decode_refused, (unsigned long long)jcc_fall_executed,
                        (unsigned long long)jcc_link_admitted, (unsigned long long)jcc_link_taken,
                        (unsigned long long)jcc_link_irq_fallback, (unsigned long long)jcc_link_dispatcher,
                        (unsigned long long)mixed_sse_encounters, (unsigned long long)mixed_sse_admitted,
                        (unsigned long long)mixed_sse_transitions, (unsigned long long)mixed_sse_executed,
                        (unsigned long long)mixed_sse_executed_transitions,
                        (unsigned long long)mixed_sse_disabled_boundaries,
                        (unsigned long long)body_owner_low_rotations,
                        (unsigned long long)body_owner_low_retranslations);
    if (jit86_store_alias_observation_active())
        return snprintf(out, size,
                        "[prof] translit: declined, guest executable mapping or shared alias observed"
                        " blocks=%llu entries=%llu declined=%llu stitch_candidates=%llu stitch_admitted=%llu"
                        " mixed_sse_encounters=%llu mixed_sse_admitted=%llu mixed_sse_transitions=%llu"
                        " mixed_sse_executed=%llu mixed_sse_executed_transitions=%llu"
                        " mixed_sse_disabled_boundaries=%llu body_owner_low_rotations=%llu"
                        " body_owner_low_retranslations=%llu\n",
                        (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined,
                        (unsigned long long)stitch_candidates, (unsigned long long)stitch_admitted,
                        (unsigned long long)mixed_sse_encounters, (unsigned long long)mixed_sse_admitted,
                        (unsigned long long)mixed_sse_transitions, (unsigned long long)mixed_sse_executed,
                        (unsigned long long)mixed_sse_executed_transitions,
                        (unsigned long long)mixed_sse_disabled_boundaries,
                        (unsigned long long)body_owner_low_rotations,
                        (unsigned long long)body_owner_low_retranslations);
    return snprintf(out, size,
                    "[prof] translit: blocks=%llu entries=%llu declined=%llu stitch_candidates=%llu"
                    " stitch_admitted=%llu rip_indirect_lowered=%llu provenance_fallback=%llu"
                    " body_owner_recovered=%llu body_owner_published=%llu"
                    " sse2_runs_admitted=%llu sse2_instructions_admitted=%llu sse2_target_runs=%llu"
                    " sse2_next_family_runs=%llu"
                    " sse2_store_instructions=%llu"
                    " sse2_store_movups=%llu sse2_store_movaps=%llu sse2_store_movdqu=%llu"
                    " sse2_store_family_runs=%llu"
                    " sse2_pxor_admitted=%llu sse2_pxor_runs_admitted=%llu"
                    TRANSLIT_PUNPCKLQDQ_PROFILE
                    TRANSLIT_MOVD_PROFILE
                    TRANSLIT_FS_PROFILE
                    " riprel_lowered=%llu scratch_lowered=%llu lea_lowered=%llu abs32_lowered=%llu"
                    " natural_lea_lowered=%llu"
                    " jcc_fall_candidates=%llu jcc_fall_admitted=%llu jcc_fall_page_refused=%llu"
                    " jcc_fall_successor_page_refused=%llu jcc_fall_decode_refused=%llu"
                    " jcc_fall_executed=%llu jcc_link_admitted=%llu jcc_link_taken=%llu"
                    " jcc_link_irq_fallback=%llu jcc_link_dispatcher=%llu"
                    " mixed_sse_encounters=%llu mixed_sse_admitted=%llu mixed_sse_transitions=%llu"
                    " mixed_sse_executed=%llu mixed_sse_executed_transitions=%llu"
                    " mixed_sse_disabled_boundaries=%llu body_owner_low_rotations=%llu"
                    " body_owner_low_retranslations=%llu\n",
                    (unsigned long long)blocks, (unsigned long long)entries, (unsigned long long)declined,
                    (unsigned long long)stitch_candidates, (unsigned long long)stitch_admitted,
                    (unsigned long long)rip_indirect_lowered, (unsigned long long)provenance_fallback,
                    (unsigned long long)body_owner_recovered, (unsigned long long)body_owner_published,
                    (unsigned long long)sse2_runs_admitted, (unsigned long long)sse2_instructions_admitted,
                    (unsigned long long)sse2_target_runs, (unsigned long long)sse2_next_family_runs,
                    (unsigned long long)sse2_store_instructions,
                    (unsigned long long)sse2_store_movups, (unsigned long long)sse2_store_movaps,
                    (unsigned long long)sse2_store_movdqu,
                    (unsigned long long)sse2_store_family_runs,
                    (unsigned long long)sse2_pxor_admitted, (unsigned long long)sse2_pxor_runs_admitted
                        TRANSLIT_PUNPCKLQDQ_ARGUMENTS TRANSLIT_MOVD_ARGUMENTS TRANSLIT_FS_ARGUMENTS,
                    (unsigned long long)displaced_riprel_lowered,
                    (unsigned long long)displaced_scratch_lowered,
                    (unsigned long long)displaced_lea_lowered,
                    (unsigned long long)natural_abs32_lowered,
                    (unsigned long long)natural_lea_lowered,
                    (unsigned long long)jcc_fall_candidates, (unsigned long long)jcc_fall_admitted,
                    (unsigned long long)jcc_fall_page_refused,
                    (unsigned long long)jcc_fall_successor_page_refused,
                    (unsigned long long)jcc_fall_decode_refused, (unsigned long long)jcc_fall_executed,
                    (unsigned long long)jcc_link_admitted, (unsigned long long)jcc_link_taken,
                    (unsigned long long)jcc_link_irq_fallback, (unsigned long long)jcc_link_dispatcher,
                    (unsigned long long)mixed_sse_encounters, (unsigned long long)mixed_sse_admitted,
                    (unsigned long long)mixed_sse_transitions, (unsigned long long)mixed_sse_executed,
                    (unsigned long long)mixed_sse_executed_transitions,
                    (unsigned long long)mixed_sse_disabled_boundaries,
                    (unsigned long long)body_owner_low_rotations,
                    (unsigned long long)body_owner_low_retranslations);
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
    int flags_from_cpu = 0;
    int exact_pc = jit_instruction_guest_pc_preserve(host_pc, &guest_pc, &preserve_registers);
#if defined(HL_NATIVE_TEST_HOOKS)
    const char *force_miss = hl_option_get("HL_TRANSLIT_PROVENANCE_FALLBACK");
    if (force_miss != NULL && force_miss[0] != '0' && force_miss[0] != 0) {
        exact_pc = 0;
        preserve_registers = 0;
        (void)__atomic_fetch_add(&translit_provenance_fallback, 1, __ATOMIC_RELAXED);
    }
#endif
    if (!exact_pc && jit_body_owner_lookup_preserve(host_pc, &guest_pc, &preserve_registers)) {
        exact_pc = 1;
        (void)__atomic_fetch_add(&translit_body_owner_recovered, 1, __ATOMIC_RELAXED);
    }
    if (exact_pc && (guest_pc == JIT_BODY_OWNER_DYNAMIC_RIP ||
                     guest_pc == JIT_BODY_OWNER_DYNAMIC_RIP_FLAGS)) {
        flags_from_cpu = guest_pc == JIT_BODY_OWNER_DYNAMIC_RIP_FLAGS;
        guest_pc = cpu->rip;
    }
    for (int i = 0; i < 16; i++) {
        if ((preserve_registers & (1u << i)) == 0) cpu->r[i] = (uint64_t)gregs[g_translit_greg[i]];
    }
    translit_flags_out(cpu, flags_from_cpu ? cpu->mmscratch[0]
                                           : (uint64_t)gregs[HL_HOST_UC_REG_EFL]);
    if (exact_pc) cpu->rip = guest_pc;
    cpu->reason = R_BRANCH;
    return 1;
}

#if defined(HL_NATIVE_TEST_HOOKS)
// Deterministic signal-context reconstruction across the source handoff and
// each canonicalization stage of the shared stub. Distinct context/cpu values
// make every preserve decision, the static-to-dynamic RIP transition, and the
// post-flags cpu source observable; no asynchronous timing is involved.
static int translit_jcc_ibtc_signal_stage_test(void) {
    if (translit_jcc_ibtc_stub_entry == NULL || translit_jcc_ibtc_stub_rsp_canonical == NULL ||
        translit_jcc_ibtc_stub_flags_canonical == NULL || translit_test_jcc_ibtc_instruction_start == NULL ||
        translit_test_jcc_ibtc_site.save_end == NULL || translit_test_jcc_ibtc_site.target_end == NULL ||
        translit_test_jcc_ibtc_site.rip_end == NULL)
        return 0;
    const uint64_t dynamic_target = UINT64_C(0x123456789abcdef0);
    struct {
        uint8_t *pc;
        uint32_t preserve;
        uint64_t cpu_rip;
        uint64_t expected_rip;
        int flags_from_cpu;
    } stages[] = {
        {translit_test_jcc_ibtc_instruction_start, 0, dynamic_target,
         translit_test_jcc_ibtc_source_guest, 0},
        {translit_test_jcc_ibtc_site.save_end, 1u << HL_X64_RAX,
         translit_test_jcc_ibtc_source_guest, translit_test_jcc_ibtc_source_guest, 0},
        {translit_test_jcc_ibtc_site.target_end, 1u << HL_X64_RAX,
         translit_test_jcc_ibtc_source_guest, translit_test_jcc_ibtc_source_guest, 0},
        {translit_test_jcc_ibtc_site.rip_end, 1u << HL_X64_RAX, dynamic_target, dynamic_target, 0},
        {translit_jcc_ibtc_stub_entry, 1u << HL_X64_RAX, dynamic_target, dynamic_target, 0},
        {translit_jcc_ibtc_stub_rsp_canonical, (1u << HL_X64_RAX) | (1u << HL_X64_RSP),
         dynamic_target, dynamic_target, 0},
        {translit_jcc_ibtc_stub_flags_canonical, UINT16_MAX, dynamic_target, dynamic_target, 1},
    };
    int saved_selected = translit_selected;
    translit_selected = 1;
    int exact = 1;
    for (unsigned stage = 0; exact && stage < sizeof stages / sizeof stages[0]; stage++) {
        struct cpu cpu = {0}, expected_flags_cpu = {0};
        ucontext_t context;
        memset(&context, 0, sizeof context);
        greg_t *gregs = HL_HOST_UC_GREGS(&context);
        for (unsigned reg = 0; reg < 16; reg++) {
            cpu.r[reg] = UINT64_C(0xc000000000000000) + reg;
            gregs[g_translit_greg[reg]] = (greg_t)(UINT64_C(0xa000000000000000) + reg);
        }
        uint64_t context_flags = UINT64_C(0xad7);
        uint64_t canonical_flags = UINT64_C(0x853);
        cpu.rip = stages[stage].cpu_rip;
        cpu.mmscratch[0] = canonical_flags;
        gregs[HL_HOST_UC_REG_EFL] = (greg_t)context_flags;
        HL_HOST_UC_PC(&context) = (greg_t)(uintptr_t)stages[stage].pc;
        exact = translit_signal_capture(&cpu, &context) && cpu.rip == stages[stage].expected_rip &&
                cpu.reason == R_BRANCH;
        for (unsigned reg = 0; exact && reg < 16; reg++) {
            uint64_t expected = stages[stage].preserve & (1u << reg)
                                    ? UINT64_C(0xc000000000000000) + reg
                                    : UINT64_C(0xa000000000000000) + reg;
            exact = cpu.r[reg] == expected;
        }
        translit_flags_out(&expected_flags_cpu, stages[stage].flags_from_cpu ? canonical_flags : context_flags);
        exact = exact && translit_flags_in(&cpu) == translit_flags_in(&expected_flags_cpu);
    }
    translit_selected = saved_selected;
    return exact;
}
#endif

#else // not a Linux x86-64 host: no diagonal exists, so the transliterator is inert.

static int translit_image_ok(void) {
    return 0;
}

static int translit_bind_cpu(struct cpu *cpu) {
    (void)cpu;
    return 0;
}

static void *translit_build(hl_x86_hot_context *context, struct interp_block *block, uint64_t gpc,
                            uint64_t *jcc_ibtc_sites_out) {
    (void)context;
    (void)block;
    (void)gpc;
    if (jcc_ibtc_sites_out != NULL) *jcc_ibtc_sites_out = 0;
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
