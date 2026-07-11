// dd/runtime/os/linux -- seccomp: a real classic-BPF (cBPF) interpreter + syscall gating.
//
// Guests self-sandbox with seccomp(2) / prctl(PR_SET_SECCOMP): they install a cBPF program that the
// kernel runs against a `struct seccomp_data` on EVERY syscall and honours the program's return action
// (ALLOW / ERRNO / KILL_PROCESS / KILL_THREAD / TRAP / TRACE / LOG), or SECCOMP_MODE_STRICT (only
// read/write/exit/rt_sigreturn permitted). dd previously accepted the install as a no-op and enforced
// nothing -- a guest believed a syscall was blocked/trapped/killed while dd kept servicing it (a
// compatibility AND security fail-open). This module closes that: it stores the installed program(s) and
// runs a small cBPF virtual machine on the syscall entry path (service()), applying the resulting action.
//
// Storage is PER-THREAD (Linux seccomp is per-thread) via __thread, stacked newest-first (multiple
// installs compose; the most restrictive action wins). It is inherited across fork(2) for free (a real
// host fork COW-copies the calling thread's __thread head + the malloc'd program nodes) and preserved
// across execve(2) for free (dd's execve re-enters IN-process -- it never host-execve's -- so __thread
// state survives, exactly as the kernel keeps filters across a non-setuid exec).
//
// HOT PATH: g_seccomp_active is a plain global that stays 0 until the first install anywhere, so a guest
// that never touches seccomp pays a single predicted-not-taken load per syscall and nothing else. Only
// once a filter/strict mode is installed does the per-thread mode gate the interpreter.
//
// Included from os/linux/syscall/dispatch.c BEFORE fs.c/proc.c/rare.c (so proc.c's PR_SET_SECCOMP and
// rare.c's seccomp(2) can call the installers) and AFTER os/linux/signal.c in the unity TU (so it can use
// raise_guest_signal / sigexit_record / sig_coredumps / svc_core_rlimit_cur / g_sigcode) and thread.c
// (gna_hit, the guest-PROT_NONE readability probe). The G_SECCOMP_ARCH / G_SECCOMP_NR seam is provided
// per guest frontend in translate/<arch>/abi.h (native AUDIT_ARCH + the raw guest syscall number).

// ---- classic-BPF ISA (subset the kernel's seccomp accepts; we implement the full cBPF for robustness) --
#define DD_BPF_CLASS(code) ((code) & 0x07)
#define DD_BPF_LD 0x00
#define DD_BPF_LDX 0x01
#define DD_BPF_ST 0x02
#define DD_BPF_STX 0x03
#define DD_BPF_ALU 0x04
#define DD_BPF_JMP 0x05
#define DD_BPF_RET 0x06
#define DD_BPF_MISC 0x07

#define DD_BPF_SIZE(code) ((code) & 0x18)
#define DD_BPF_W 0x00
#define DD_BPF_H 0x08
#define DD_BPF_B 0x10

#define DD_BPF_MODE(code) ((code) & 0xe0)
#define DD_BPF_IMM 0x00
#define DD_BPF_ABS 0x20
#define DD_BPF_IND 0x40
#define DD_BPF_MEM 0x60
#define DD_BPF_LEN 0x80
#define DD_BPF_MSH 0xa0

#define DD_BPF_OP(code) ((code) & 0xf0)
#define DD_BPF_ADD 0x00
#define DD_BPF_SUB 0x10
#define DD_BPF_MUL 0x20
#define DD_BPF_DIV 0x30
#define DD_BPF_OR 0x40
#define DD_BPF_AND 0x50
#define DD_BPF_LSH 0x60
#define DD_BPF_RSH 0x70
#define DD_BPF_NEG 0x80
#define DD_BPF_MOD 0x90
#define DD_BPF_XOR 0xa0
#define DD_BPF_JA 0x00
#define DD_BPF_JEQ 0x10
#define DD_BPF_JGT 0x20
#define DD_BPF_JGE 0x30
#define DD_BPF_JSET 0x40

#define DD_BPF_SRC(code) ((code) & 0x08)
#define DD_BPF_K 0x00
#define DD_BPF_X 0x08

#define DD_BPF_RVAL(code) ((code) & 0x18)
#define DD_BPF_A 0x10

#define DD_BPF_MISCOP(code) ((code) & 0xf8)
#define DD_BPF_TAX 0x00
#define DD_BPF_TXA 0x80

#define DD_BPF_MAXINSNS 4096
#define DD_BPF_MEMWORDS 16

// ---- seccomp return actions (linux/seccomp.h) ----
#define DD_SECCOMP_RET_KILL_PROCESS 0x80000000u
#define DD_SECCOMP_RET_KILL_THREAD 0x00000000u
#define DD_SECCOMP_RET_TRAP 0x00030000u
#define DD_SECCOMP_RET_ERRNO 0x00050000u
#define DD_SECCOMP_RET_USER_NOTIF 0x7fc00000u
#define DD_SECCOMP_RET_TRACE 0x7ff00000u
#define DD_SECCOMP_RET_LOG 0x7ffc0000u
#define DD_SECCOMP_RET_ALLOW 0x7fff0000u
#define DD_SECCOMP_RET_ACTION_FULL 0xffff0000u
#define DD_SECCOMP_RET_DATA 0x0000ffffu

// seccomp(2) operations
#define DD_SECCOMP_SET_MODE_STRICT 0u
#define DD_SECCOMP_SET_MODE_FILTER 1u
// seccomp(2) filter flags
#define DD_SECCOMP_FILTER_FLAG_TSYNC 0x01u
#define DD_SECCOMP_FILTER_FLAG_LOG 0x02u
#define DD_SECCOMP_FILTER_FLAG_SPEC_ALLOW 0x04u
#define DD_SECCOMP_FILTER_FLAG_NEW_LISTENER 0x08u
#define DD_SECCOMP_FILTER_FLAG_TSYNC_ESRCH 0x10u
#define DD_SECCOMP_FILTER_FLAGS_KNOWN                                                                                   \
    (DD_SECCOMP_FILTER_FLAG_TSYNC | DD_SECCOMP_FILTER_FLAG_LOG | DD_SECCOMP_FILTER_FLAG_SPEC_ALLOW |                    \
     DD_SECCOMP_FILTER_FLAG_NEW_LISTENER | DD_SECCOMP_FILTER_FLAG_TSYNC_ESRCH)

// prctl PR_SET_SECCOMP modes (differ from seccomp(2) op numbers!)
#define DD_SECCOMP_MODE_STRICT 1u
#define DD_SECCOMP_MODE_FILTER 2u

#ifndef CAP_SYS_ADMIN
#define CAP_SYS_ADMIN 21
#endif

// The seccomp classic-BPF instruction, byte-identical to the guest's `struct sock_filter`.
struct dd_sock_filter {
    uint16_t code;
    uint8_t jt;
    uint8_t jf;
    uint32_t k;
};

// The read-only data buffer the cBPF program is run against, byte-identical to the kernel's
// `struct seccomp_data`: nr (native syscall number), arch (AUDIT_ARCH_*), the userspace IP, and the six
// syscall arguments -- all little-endian on both guest ISAs, so BPF_LD|W|ABS reads a native u32/u16/u8.
struct dd_seccomp_data {
    int32_t nr;
    uint32_t arch;
    uint64_t instruction_pointer;
    uint64_t args[6];
};

// One installed program on a thread's stacked filter chain (newest first).
struct dd_bpf_filter {
    struct dd_sock_filter *insns;
    uint16_t len;
    struct dd_bpf_filter *prev;
};

// 0 = no seccomp anywhere (never installed); flips to 1 on the first install and never resets. A plain
// global load short-circuits the whole gate for the common case (guests that never call seccomp).
static volatile int g_seccomp_active;

// Per-thread mode: 0 = none, 1 = SECCOMP_MODE_STRICT, 2 = SECCOMP_MODE_FILTER. Sticky (a thread cannot
// leave seccomp). Inherited across fork (COW) and preserved across dd's in-process execve.
static __thread unsigned char t_seccomp_mode;
static __thread struct dd_bpf_filter *t_seccomp_filters; // stacked, newest first

// Run one cBPF program against `sd`, returning its 32-bit action word. A malformed/out-of-bounds memory
// access aborts the program with 0 (== SECCOMP_RET_KILL_THREAD), exactly as classic BPF (sk_run_filter)
// returns 0 on an out-of-range load -- the conservative "deny" that a broken filter deserves.
static uint32_t dd_bpf_run(const struct dd_sock_filter *f, uint16_t flen, const struct dd_seccomp_data *sd) {
    const uint8_t *pkt = (const uint8_t *)sd;
    const uint32_t plen = (uint32_t)sizeof(*sd);
    uint32_t A = 0, X = 0;
    uint32_t mem[DD_BPF_MEMWORDS];
    memset(mem, 0, sizeof mem);
    uint32_t pc = 0;
    // cBPF jumps are forward-only unsigned offsets, so a well-formed program halts within flen steps; the
    // extra guard bounds any pathological (yet in-range) case at the ISA maximum.
    for (uint32_t steps = 0; pc < flen && steps <= DD_BPF_MAXINSNS; steps++, pc++) {
        const struct dd_sock_filter *in = &f[pc];
        uint16_t code = in->code;
        uint32_t k = in->k;
        switch (DD_BPF_CLASS(code)) {
        case DD_BPF_LD:
            switch (DD_BPF_MODE(code)) {
            case DD_BPF_IMM:
                A = k;
                break;
            case DD_BPF_LEN:
                A = plen;
                break;
            case DD_BPF_ABS:
            case DD_BPF_IND: {
                uint64_t off = (DD_BPF_MODE(code) == DD_BPF_IND) ? (uint64_t)X + k : (uint64_t)k;
                uint32_t sz = (DD_BPF_SIZE(code) == DD_BPF_B) ? 1 : (DD_BPF_SIZE(code) == DD_BPF_H) ? 2 : 4;
                if (off + sz > plen) return 0; // out of bounds -> deny (classic-BPF semantics)
                if (sz == 1)
                    A = pkt[off];
                else if (sz == 2)
                    A = (uint32_t)pkt[off] | ((uint32_t)pkt[off + 1] << 8);
                else
                    A = (uint32_t)pkt[off] | ((uint32_t)pkt[off + 1] << 8) | ((uint32_t)pkt[off + 2] << 16) |
                        ((uint32_t)pkt[off + 3] << 24);
                break;
            }
            case DD_BPF_MEM:
                if (k >= DD_BPF_MEMWORDS) return 0;
                A = mem[k];
                break;
            default:
                return 0;
            }
            break;
        case DD_BPF_LDX:
            switch (DD_BPF_MODE(code)) {
            case DD_BPF_IMM:
                X = k;
                break;
            case DD_BPF_LEN:
                X = plen;
                break;
            case DD_BPF_MEM:
                if (k >= DD_BPF_MEMWORDS) return 0;
                X = mem[k];
                break;
            case DD_BPF_MSH: // X = 4 * (pkt[k] & 0xf) -- IP-header-length idiom; harmless here
                if (k >= plen) return 0;
                X = 4 * (pkt[k] & 0xf);
                break;
            default:
                return 0;
            }
            break;
        case DD_BPF_ST:
            if (k >= DD_BPF_MEMWORDS) return 0;
            mem[k] = A;
            break;
        case DD_BPF_STX:
            if (k >= DD_BPF_MEMWORDS) return 0;
            mem[k] = X;
            break;
        case DD_BPF_ALU: {
            uint32_t src = (DD_BPF_SRC(code) == DD_BPF_X) ? X : k;
            switch (DD_BPF_OP(code)) {
            case DD_BPF_ADD:
                A += src;
                break;
            case DD_BPF_SUB:
                A -= src;
                break;
            case DD_BPF_MUL:
                A *= src;
                break;
            case DD_BPF_DIV:
                if (src == 0) return 0; // div by zero -> abort (deny)
                A /= src;
                break;
            case DD_BPF_MOD:
                if (src == 0) return 0;
                A %= src;
                break;
            case DD_BPF_OR:
                A |= src;
                break;
            case DD_BPF_AND:
                A &= src;
                break;
            case DD_BPF_XOR:
                A ^= src;
                break;
            case DD_BPF_LSH:
                A = (src < 32) ? (A << src) : 0;
                break;
            case DD_BPF_RSH:
                A = (src < 32) ? (A >> src) : 0;
                break;
            case DD_BPF_NEG:
                A = (uint32_t)(-(int32_t)A);
                break;
            default:
                return 0;
            }
            break;
        }
        case DD_BPF_JMP: {
            if (DD_BPF_OP(code) == DD_BPF_JA) {
                pc += k; // += k, then the loop's pc++ advances to the target
                break;
            }
            uint32_t cmp = (DD_BPF_SRC(code) == DD_BPF_X) ? X : k;
            int t;
            switch (DD_BPF_OP(code)) {
            case DD_BPF_JEQ:
                t = (A == cmp);
                break;
            case DD_BPF_JGT:
                t = (A > cmp);
                break;
            case DD_BPF_JGE:
                t = (A >= cmp);
                break;
            case DD_BPF_JSET:
                t = (A & cmp) != 0;
                break;
            default:
                return 0;
            }
            pc += t ? in->jt : in->jf; // += jt/jf, then loop pc++ advances past it
            break;
        }
        case DD_BPF_RET: {
            uint32_t rval = (DD_BPF_RVAL(code) == DD_BPF_A) ? A : k;
            return rval;
        }
        case DD_BPF_MISC:
            if (DD_BPF_MISCOP(code) == DD_BPF_TAX)
                X = A;
            else if (DD_BPF_MISCOP(code) == DD_BPF_TXA)
                A = X;
            else
                return 0;
            break;
        default:
            return 0;
        }
    }
    // Fell off the end without a RET (a malformed program). Classic BPF cannot; deny conservatively.
    return 0;
}

// Precedence rank of a seccomp action -- SMALLER = higher precedence (more restrictive wins), matching the
// kernel's documented order KILL_PROCESS > KILL_THREAD > TRAP > ERRNO > USER_NOTIF > TRACE > LOG > ALLOW.
// An unrecognized action is treated as KILL_THREAD (the kernel's default for an unknown action word).
static int dd_seccomp_prec(uint32_t action) {
    switch (action & DD_SECCOMP_RET_ACTION_FULL) {
    case DD_SECCOMP_RET_KILL_PROCESS:
        return 0;
    case DD_SECCOMP_RET_KILL_THREAD:
        return 1;
    case DD_SECCOMP_RET_TRAP:
        return 2;
    case DD_SECCOMP_RET_ERRNO:
        return 3;
    case DD_SECCOMP_RET_USER_NOTIF:
        return 4;
    case DD_SECCOMP_RET_TRACE:
        return 5;
    case DD_SECCOMP_RET_LOG:
        return 6;
    case DD_SECCOMP_RET_ALLOW:
        return 7;
    default:
        return 1; // unknown -> KILL_THREAD
    }
}

// Run every installed filter and return the highest-precedence (most restrictive) action word.
static uint32_t dd_seccomp_eval(const struct dd_seccomp_data *sd) {
    uint32_t best = DD_SECCOMP_RET_ALLOW;
    int best_prec = 7;
    for (struct dd_bpf_filter *f = t_seccomp_filters; f; f = f->prev) {
        uint32_t r = dd_bpf_run(f->insns, f->len, sd);
        int p = dd_seccomp_prec(r);
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
static void dd_seccomp_kill(struct cpu *c, int signo) {
    sig_diag_raise_default(c, signo);
    int core = sig_coredumps(signo) && svc_core_rlimit_cur() > 0;
    sigexit_record(signo, core);
    c->exited = 1;
    c->exit_code = 128 + signo;
}

// The syscall-entry gate. Returns 1 if the syscall was intercepted (its result is already set in G_RET, a
// signal was queued, or the process was killed -- the dispatcher must NOT service it), 0 to allow it. Only
// reached when g_seccomp_active (see seccomp_gate below), so the no-seccomp case never runs any of this.
static int dd_seccomp_apply(struct cpu *c) {
    unsigned mode = t_seccomp_mode;
    if (mode == 1) { // SECCOMP_MODE_STRICT: only read/write/exit/rt_sigreturn (canonical aarch64 numbers)
        uint64_t nr = G_NR(c);
        if (nr == 63 /*read*/ || nr == 64 /*write*/ || nr == 93 /*exit*/ || nr == 139 /*rt_sigreturn*/) return 0;
        dd_seccomp_kill(c, 9 /*SIGKILL -- strict-mode violations are SIGKILL, not SIGSYS*/);
        return 1;
    }
    if (mode != 2) return 0; // no filter on this thread

    struct dd_seccomp_data sd;
    sd.nr = (int32_t)G_SECCOMP_NR(c); // RAW native syscall number the guest issued (pre-normalization)
    sd.arch = G_SECCOMP_ARCH;
    sd.instruction_pointer = G_PC(c);
    sd.args[0] = G_A0(c);
    sd.args[1] = G_A1(c);
    sd.args[2] = G_A2(c);
    sd.args[3] = G_A3(c);
    sd.args[4] = G_A4(c);
    sd.args[5] = G_A5(c);

    uint32_t action = dd_seccomp_eval(&sd);
    switch (action & DD_SECCOMP_RET_ACTION_FULL) {
    case DD_SECCOMP_RET_ALLOW:
        return 0;
    case DD_SECCOMP_RET_LOG: // treated as ALLOW (we don't emit an audit log); the syscall proceeds
        return 0;
    case DD_SECCOMP_RET_ERRNO: {
        uint32_t e = action & DD_SECCOMP_RET_DATA;
        if (e > 4095) e = 4095; // MAX_ERRNO clamp (kernel does the same)
        G_RET(c) = (uint64_t)(-(int64_t)e);
        return 1;
    }
    case DD_SECCOMP_RET_TRACE:
        // SECCOMP_RET_TRACE with no ptrace supervisor attached: the kernel skips the syscall and returns
        // -ENOSYS. dd has no seccomp-TRACE supervisor wiring, so this is the always-correct no-tracer path.
        G_RET(c) = (uint64_t)(-(int64_t)ENOSYS);
        return 1;
    case DD_SECCOMP_RET_USER_NOTIF:
        // No SECCOMP_FILTER_FLAG_NEW_LISTENER supervisor exists (we reject that flag at install), so a
        // USER_NOTIF at runtime cannot be serviced; fail the syscall with -ENOSYS rather than hang.
        G_RET(c) = (uint64_t)(-(int64_t)ENOSYS);
        return 1;
    case DD_SECCOMP_RET_TRAP:
        // Deliver SIGSYS (si_code = SYS_SECCOMP) to the guest and skip the syscall. If the guest installed a
        // SIGSYS handler it runs; otherwise SIGSYS default-terminates the process (raise_guest_signal). The
        // kernel's TRAP path skips the syscall WITHOUT writing the return register (unlike ERRNO/TRACE), so
        // we leave G_RET untouched -- matching Linux, where a returning handler observes the unmodified reg.
        g_sigcode[31] = 1 /*SYS_SECCOMP*/;
        raise_guest_signal(c, 31);
        return 1;
    case DD_SECCOMP_RET_KILL_PROCESS:
    case DD_SECCOMP_RET_KILL_THREAD: // modeled as process death (faithful for a single-threaded guest)
    default:
        dd_seccomp_kill(c, 31 /*SIGSYS -- a filter KILL action reports WTERMSIG=SIGSYS*/);
        return 1;
    }
}

// Fast inline gate called from service() on EVERY syscall. One predicted-not-taken global load in the
// common (no-seccomp) case; the real work is out-of-line in dd_seccomp_apply.
static inline int seccomp_gate(struct cpu *c) {
    if (__builtin_expect(!g_seccomp_active, 1)) return 0;
    if (__builtin_expect(t_seccomp_mode == 0, 1)) return 0;
    return dd_seccomp_apply(c);
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
    // Installing a filter requires CAP_SYS_ADMIN or no_new_privs (kernel: else -EACCES). The container's
    // default cap set lacks CAP_SYS_ADMIN, so a well-behaved sandbox sets PR_SET_NO_NEW_PRIVS first.
    if (!g_nnp && !(g_cap_eff & (1ull << CAP_SYS_ADMIN))) return -EACCES;
    if (flags & ~DD_SECCOMP_FILTER_FLAGS_KNOWN) return -EINVAL;
    // NEW_LISTENER would have us return a userspace-notification fd and run a supervisor protocol we do not
    // implement; reject it honestly rather than hand back a listener that never delivers notifications.
    if (flags & DD_SECCOMP_FILTER_FLAG_NEW_LISTENER) return -EINVAL;
    if (!fprog_ptr) return -EFAULT;
    if (gna_hit(fprog_ptr, 16)) return -EFAULT;

    // struct sock_fprog on LP64: u16 len at +0, 8-byte filter pointer at +8.
    uint16_t len;
    uint64_t insn_ptr;
    memcpy(&len, (const void *)(uintptr_t)fprog_ptr, sizeof len);
    memcpy(&insn_ptr, (const void *)(uintptr_t)(fprog_ptr + 8), sizeof insn_ptr);
    if (len == 0 || len > DD_BPF_MAXINSNS) return -EINVAL;
    if (!insn_ptr) return -EFAULT;
    size_t bytes = (size_t)len * sizeof(struct dd_sock_filter);
    if (gna_hit(insn_ptr, bytes)) return -EFAULT;

    struct dd_bpf_filter *node = (struct dd_bpf_filter *)malloc(sizeof *node);
    if (!node) return -ENOMEM;
    node->insns = (struct dd_sock_filter *)malloc(bytes);
    if (!node->insns) {
        free(node);
        return -ENOMEM;
    }
    memcpy(node->insns, (const void *)(uintptr_t)insn_ptr, bytes);
    node->len = len;
    node->prev = t_seccomp_filters;
    t_seccomp_filters = node;
    t_seccomp_mode = 2;
    g_seccomp_active = 1;
    return 0;
}
