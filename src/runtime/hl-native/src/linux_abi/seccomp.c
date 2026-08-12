// hl/linux_abi -- seccomp: a real classic-BPF (cBPF) interpreter + syscall gating.
//
// Guests self-sandbox with seccomp(2) / prctl(PR_SET_SECCOMP): they install a cBPF program that the
// kernel runs against a `struct seccomp_data` on EVERY syscall and honours the program's return action
// (ALLOW / ERRNO / KILL_PROCESS / KILL_THREAD / TRAP / TRACE / LOG), or SECCOMP_MODE_STRICT (only
// read/write/exit/rt_sigreturn permitted). The engine previously accepted installation as a no-op and
// enforced nothing, so a guest believed a syscall was blocked/trapped/killed while it was still serviced (a
// compatibility AND security fail-open). This module closes that: it stores the installed program(s) and
// runs a small cBPF virtual machine on the syscall entry path (service()), applying the resulting action.
//
// Storage is PER-THREAD (Linux seccomp is per-thread) via __thread, stacked newest-first (multiple
// installs compose; the most restrictive action wins). It is inherited across fork(2) for free (a real
// host fork COW-copies the calling thread's __thread head + the malloc'd program nodes) and preserved
// across execve(2) for free (the engine's execve re-enters in-process rather than calling host execve, so thread
// state survives, exactly as the kernel keeps filters across a non-setuid exec).
//
// HOT PATH: g_seccomp_active is a plain global that stays 0 until the first install anywhere, so a guest
// that never touches seccomp pays a single predicted-not-taken load per syscall and nothing else. Only
// once a filter/strict mode is installed does the per-thread mode gate the interpreter.
//
// Included from syscall/dispatch.c before fs.c/proc.c/rare.c (so proc.c's PR_SET_SECCOMP and rare.c's
// seccomp(2) can call the installers) and after signal.c in the unity TU (so it can use
// raise_guest_signal / sigexit_record / sig_coredumps / svc_core_rlimit_cur / g_sigcode) and thread.c
// (gna_hit, the guest-PROT_NONE readability probe). The G_SECCOMP_ARCH / G_SECCOMP_NR seam is provided
// per guest frontend in its ABI header (native AUDIT_ARCH + the raw guest syscall number).

#include "seccomp_vm.h"

// seccomp(2) operations
#define HL_LINUX_SECCOMP_SET_MODE_STRICT 0u
#define HL_LINUX_SECCOMP_SET_MODE_FILTER 1u
#define HL_LINUX_SECCOMP_FILTER_FLAG_TSYNC 0x01u
#define HL_LINUX_SECCOMP_FILTER_FLAG_LOG 0x02u
#define HL_LINUX_SECCOMP_FILTER_FLAG_SPEC_ALLOW 0x04u
#define HL_LINUX_SECCOMP_FILTER_FLAG_NEW_LISTENER 0x08u
#define HL_LINUX_SECCOMP_FILTER_FLAG_TSYNC_ESRCH 0x10u
#define HL_LINUX_SECCOMP_FILTER_FLAGS_KNOWN                                                                            \
    (HL_LINUX_SECCOMP_FILTER_FLAG_TSYNC | HL_LINUX_SECCOMP_FILTER_FLAG_LOG | HL_LINUX_SECCOMP_FILTER_FLAG_SPEC_ALLOW | \
     HL_LINUX_SECCOMP_FILTER_FLAG_NEW_LISTENER | HL_LINUX_SECCOMP_FILTER_FLAG_TSYNC_ESRCH)
#define HL_LINUX_SECCOMP_MODE_STRICT 1u
#define HL_LINUX_SECCOMP_MODE_FILTER 2u

#ifndef CAP_SYS_ADMIN
#define CAP_SYS_ADMIN 21
#endif

struct hl_linux_bpf_filter {
    struct hl_linux_sock_filter *insns;
    uint16_t len;
    struct hl_linux_bpf_filter *prev;
};

static volatile int g_seccomp_active;
static __thread unsigned char t_seccomp_mode;
static __thread struct hl_linux_bpf_filter *t_seccomp_filters;

// Run every installed filter and return the highest-precedence (most restrictive) action word.
static uint32_t hl_linux_seccomp_evaluate(const struct hl_linux_seccomp_data *sd) {
    uint32_t best = HL_LINUX_SECCOMP_RET_ALLOW;
    int best_prec = 7;
    for (struct hl_linux_bpf_filter *f = t_seccomp_filters; f; f = f->prev) {
        uint32_t r = hl_seccomp_run(f->insns, f->len, sd);
        int p = hl_seccomp_precedence(r);
        if (p < best_prec) {
            best_prec = p;
            best = r;
        }
    }
    return best;
}

// Uncatchable termination for a seccomp KILL action / STRICT-mode violation: the process dies as if by an
// unblockable signal, bypassing any guest handler (a seccomp kill is not deliverable to userspace). A
// filter KILL_PROCESS/KILL_THREAD action reports SIGSYS (WTERMSIG=SIGSYS, coredump per RLIMIT_CORE); a
// SECCOMP_MODE_STRICT violation reports SIGKILL, matching the kernel. Mirrors signal.c's fatal-default
// path: run_guest unwinds on c->exited and run_loaded returns c->exit_code (128+signo), so the parent's
// wait4/waitid reconstructs WIFSIGNALED/WTERMSIG.
static void hl_linux_seccomp_kill(struct cpu *c, int signo) {
    guest_group_fatal(c, signo);
}

// The syscall-entry gate. Returns 1 if the syscall was intercepted (its result is already set in G_RET, a
// signal was queued, or the process was killed -- the dispatcher must NOT service it), 0 to allow it. Only
// reached when g_seccomp_active (see seccomp_gate below), so the no-seccomp case never runs any of this.
static int hl_linux_seccomp_apply(struct cpu *c) {
    unsigned mode = t_seccomp_mode;
    if (mode == 1) { // SECCOMP_MODE_STRICT: only read/write/exit/rt_sigreturn (canonical aarch64 numbers)
        uint64_t nr = G_NR(c);
        if (nr == 63 /*read*/ || nr == 64 /*write*/ || nr == 93 /*exit*/ || nr == 139 /*rt_sigreturn*/) return 0;
        hl_linux_seccomp_kill(c, 9 /*SIGKILL -- strict-mode violations are SIGKILL, not SIGSYS*/);
        return 1;
    }
    if (mode != 2) return 0; // no filter on this thread

    struct hl_linux_seccomp_data sd;
    sd.nr = (int32_t)G_SECCOMP_NR(c); // RAW native syscall number the guest issued (pre-normalization)
    sd.arch = G_SECCOMP_ARCH;
    // Guest ABI state: SIGSYS handlers compare this address with the interrupted PC in ucontext.
    sd.instruction_pointer = G_SECCOMP_IP(c);
    sd.args[0] = G_A0(c);
    sd.args[1] = G_A1(c);
    sd.args[2] = G_A2(c);
    sd.args[3] = G_A3(c);
    sd.args[4] = G_A4(c);
    sd.args[5] = G_A5(c);

    uint32_t action = hl_linux_seccomp_evaluate(&sd);
    switch (action & HL_LINUX_SECCOMP_RET_ACTION_FULL) {
    case HL_LINUX_SECCOMP_RET_ALLOW: return 0;
    case HL_LINUX_SECCOMP_RET_LOG: // treated as ALLOW (we don't emit an audit log); the syscall proceeds
        return 0;
    case HL_LINUX_SECCOMP_RET_ERRNO: {
        uint32_t e = action & HL_LINUX_SECCOMP_RET_DATA;
        if (e > 4095) e = 4095; // MAX_ERRNO clamp (kernel does the same)
        G_RET(c) = (uint64_t)(-(int64_t)e);
        return 1;
    }
    case HL_LINUX_SECCOMP_RET_TRACE:
        // SECCOMP_RET_TRACE with no ptrace supervisor attached: the kernel skips the syscall and returns
        // -ENOSYS. The engine has no seccomp-TRACE supervisor wiring, so this is the correct no-tracer path.
        G_RET(c) = (uint64_t)(-(int64_t)ENOSYS);
        return 1;
    case HL_LINUX_SECCOMP_RET_USER_NOTIF:
        // No SECCOMP_FILTER_FLAG_NEW_LISTENER supervisor exists (we reject that flag at install), so a
        // USER_NOTIF at runtime cannot be serviced; fail the syscall with -ENOSYS rather than hang.
        G_RET(c) = (uint64_t)(-(int64_t)ENOSYS);
        return 1;
    case HL_LINUX_SECCOMP_RET_TRAP:
        // Deliver SIGSYS (si_code = SYS_SECCOMP) to the guest and skip the syscall. If the guest installed a
        // SIGSYS handler it runs; otherwise SIGSYS default-terminates the process (raise_guest_signal). The
        // kernel's TRAP path skips the syscall WITHOUT writing the return register (unlike ERRNO/TRACE), so
        // we leave G_RET untouched -- matching Linux, where a returning handler observes the unmodified reg.
        // Linux _sigsys: call_addr@16, syscall@24, arch@28; RET_DATA is carried in si_errno.
        raise_guest_signal_info(c, 31, (int)(action & HL_LINUX_SECCOMP_RET_DATA), 1 /*SYS_SECCOMP*/,
                                (uint64_t)(uint32_t)sd.nr | ((uint64_t)sd.arch << 32), 0, 0, sd.instruction_pointer);
        return 1;
    case HL_LINUX_SECCOMP_RET_KILL_PROCESS:
    case HL_LINUX_SECCOMP_RET_KILL_THREAD: // modeled as process death (faithful for a single-threaded guest)
    default: hl_linux_seccomp_kill(c, 31 /*SIGSYS -- a filter KILL action reports WTERMSIG=SIGSYS*/); return 1;
    }
}

// Fast inline gate called from service() on EVERY syscall. One predicted-not-taken global load in the
// common (no-seccomp) case; the real work is out-of-line in hl_linux_seccomp_apply.
static inline int seccomp_gate(struct cpu *c) {
    if (__builtin_expect(!g_seccomp_active, 1)) return 0;
    if (__builtin_expect(t_seccomp_mode == 0, 1)) return 0;
    return hl_linux_seccomp_apply(c);
}

// ---- install paths (called from the seccomp(2) and prctl(PR_SET_SECCOMP) handlers) ----

// SECCOMP_SET_MODE_STRICT / PR_SET_SECCOMP(SECCOMP_MODE_STRICT). Returns 0 or -errno.
static long seccomp_set_strict(void) {
    if (t_seccomp_mode == 2) return -EINVAL; // cannot go strict after a filter is installed
    t_seccomp_mode = 1;
    g_seccomp_active = 1;
    return 0;
}

// SECCOMP_SET_MODE_FILTER / PR_SET_SECCOMP(SECCOMP_MODE_FILTER). `fprog_ptr` is the guest pointer to a
// `struct sock_fprog { unsigned short len; struct sock_filter *filter; }`; `flags` are the seccomp(2)
// filter flags (0 for the prctl entry point). Copies the program into engine memory and pushes it onto
// this thread's stacked chain. Returns 0 or -errno, matching the kernel's argument validation.
static long seccomp_install_filter(uint64_t fprog_ptr, uint32_t flags) {
    if (flags & ~HL_LINUX_SECCOMP_FILTER_FLAGS_KNOWN) return -EINVAL;
    // NEW_LISTENER would have us return a userspace-notification fd and run a supervisor protocol we do not
    // implement; reject it honestly rather than hand back a listener that never delivers notifications.
    if (flags & HL_LINUX_SECCOMP_FILTER_FLAG_NEW_LISTENER) return -EINVAL;
    // struct sock_fprog on LP64: u16 len at +0, 8-byte filter pointer at +8.
    uint16_t len;
    uint64_t insn_ptr;
    if (guest_copy_from(&len, fprog_ptr, sizeof len) != sizeof len ||
        guest_copy_from(&insn_ptr, fprog_ptr + 8, sizeof insn_ptr) != sizeof insn_ptr)
        return -EFAULT;
    /* sock_fprog contains a second guest pointer.  Static ET_EXEC guests may
     * embed its low link address even after the outer syscall argument was
     * rebased, so translate the nested pointer by the same image model. */
    insn_ptr = nonpie_fold(insn_ptr);
    if (len == 0 || len > HL_LINUX_BPF_MAXINSNS) return -EINVAL;
    if (!insn_ptr) return -EFAULT;
    size_t bytes = (size_t)len * sizeof(struct hl_linux_sock_filter);
    // Linux copies and validates the user program before checking installation
    // authority.  Consequently an unreadable program is EFAULT even when the
    // caller also lacks CAP_SYS_ADMIN/no_new_privs (LTP prctl02).  Keeping this
    // ordering also guarantees no guest pointer reaches memcpy unchecked.
    struct hl_linux_bpf_filter *node = (struct hl_linux_bpf_filter *)malloc(sizeof *node);
    if (!node) return -ENOMEM;
    node->insns = (struct hl_linux_sock_filter *)malloc(bytes);
    if (!node->insns) {
        free(node);
        return -ENOMEM;
    }
    if (guest_copy_from(node->insns, insn_ptr, bytes) != (ssize_t)bytes) {
        free(node->insns);
        free(node);
        return -EFAULT;
    }
    if (!g_nnp && !(g_cap_eff & (1ull << CAP_SYS_ADMIN))) {
        free(node->insns);
        free(node);
        return -EACCES;
    }
    node->len = len;
    node->prev = t_seccomp_filters;
    t_seccomp_filters = node;
    t_seccomp_mode = 2;
    g_seccomp_active = 1;
    return 0;
}

// SECCOMP_GET_ACTION_AVAIL (op 2): probe whether a filter return action is one the kernel recognises.
// libseccomp/runc call this before installing the docker profile to learn which actions (KILL_PROCESS, LOG,
// ...) they may use. `flags` must be 0; `act_ptr` points to a u32 action word that must exactly equal one of
// the defined SECCOMP_RET_* constants (any data bits set -> the action is "unavailable"). Returns 0 for a
// recognised action, -EOPNOTSUPP for anything else -- matching native, which reports every canonical action
// (including USER_NOTIF) available. Previously this op fell through to -EINVAL, so a runtime that probes
// action support saw the whole seccomp facility as broken.
static long seccomp_get_action_avail(uint64_t flags, uint64_t act_ptr) {
    if (flags) return -EINVAL;
    uint32_t action;
    if (guest_copy_from(&action, act_ptr, sizeof action) != sizeof action) return -EFAULT;
    switch (action) {
    case HL_LINUX_SECCOMP_RET_KILL_PROCESS:
    case HL_LINUX_SECCOMP_RET_KILL_THREAD:
    case HL_LINUX_SECCOMP_RET_TRAP:
    case HL_LINUX_SECCOMP_RET_ERRNO:
    case HL_LINUX_SECCOMP_RET_USER_NOTIF:
    case HL_LINUX_SECCOMP_RET_TRACE:
    case HL_LINUX_SECCOMP_RET_LOG:
    case HL_LINUX_SECCOMP_RET_ALLOW: return 0;
    default: return -EOPNOTSUPP; // recognised action words carry no data bits
    }
}

// SECCOMP_GET_NOTIF_SIZES (op 3): report the sizes of the user-notification ABI structs so a supervisor can
// size its buffers. `flags` must be 0; `sz_ptr` points to a `struct seccomp_notif_sizes { u16 seccomp_notif;
// u16 seccomp_notif_resp; u16 seccomp_data; }`. We report the current canonical sizes (matching native) even
// though NEW_LISTENER is rejected at install: the sizes are a stable query, and a probe must succeed as on
// Linux rather than see -EINVAL. seccomp_data is our own 64-byte struct; notif/resp mirror the kernel ABI.
static long seccomp_get_notif_sizes(uint64_t flags, uint64_t sz_ptr) {
    if (flags) return -EINVAL;
    uint16_t sizes[3] = {80 /*seccomp_notif*/, 24 /*seccomp_notif_resp*/,
                         (uint16_t)sizeof(struct hl_linux_seccomp_data) /*seccomp_data = 64*/};
    if (guest_copy_to(sz_ptr, sizes, sizeof sizes) != sizeof sizes) return -EFAULT;
    return 0;
}
