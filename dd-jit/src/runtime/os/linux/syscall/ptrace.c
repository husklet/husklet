// os/linux/syscall/ptrace.c -- real ptrace(2) support for dd. Included by dispatch.c AFTER the
// svc_* family + local helpers; the shared arena struct (struct pt_arena/pt_link) + the hook forward
// declarations live at the top of dispatch.c (before the family includes so mem.c/proc.c/rare.c can call
// them). See that header block for the architecture rationale. Summary:
//
//   dd runs each guest PROCESS as its own host process, so a guest tracer and its guest tracee are two
//   host processes. We do NOT touch the host macOS ptrace (no Linux semantics, cannot see guest regs).
//   Instead this file emulates the Linux ptrace relationship BETWEEN the two guest processes over a
//   shared-memory arena (g_pt) keyed on guest pids:
//     * the TRACEE, when it enters a ptrace-stop, marshals its guest register file into its slot and
//       blocks in ptrace_stop() until the tracer issues a resume command (CONT/SYSCALL/DETACH/...);
//     * the TRACER's ptrace() calls read/steer that slot (GETREGS/SETREGS/SETOPTIONS/CONT/...);
//     * PEEK/POKE and process_vm_readv of TRACEE memory are serviced by the (stopped) tracee against its
//       OWN address space over a request/response channel -- the tracer holds only a COW copy after fork
//       and cannot read the tracee's live memory directly;
//     * the TRACER's wait4() is pumped through ptrace_wait(): it surfaces tracee ptrace-stops (Linux wait
//       status: the 0x7f WIFSTOPPED marker, the 0x80 TRACESYSGOOD syscall bit, PTRACE_EVENT_* high bits)
//       and real child exits, and tears a link down when its tracee dies.
//
// WHAT IS IMPLEMENTED (the strace core, both guest arches):
//   TRACEME/ATTACH/SEIZE/DETACH/KILL/INTERRUPT; CONT/SYSCALL (syscall-entry+exit stops, the heart of
//   strace)/SINGLESTEP*/LISTEN; PEEK{TEXT,DATA,USER}/POKE{TEXT,DATA,USER}; GETREGS/SETREGS +
//   GETREGSET/SETREGSET(NT_PRSTATUS, per-arch user_regs_struct); GETSIGINFO/SETSIGINFO; SETOPTIONS
//   (TRACESYSGOOD/TRACEEXEC/EXITKILL...) + GETEVENTMSG; exec-stop (SIGTRAP or PTRACE_EVENT_EXEC);
//   signal-delivery/group stops for signals a traced process raises on itself; cross-process
//   process_vm_readv/writev against a stopped tracee.
//
// STAGED (returns a correct errno / documented approximation, never a lying success):
//   (*) SINGLESTEP is accepted and resumes, but dd translates whole basic blocks, so it is NOT yet
//       instruction-granular (it resumes like CONT). True single-step needs single-instruction blocks --
//       tracked for the gdb batch. GETFPREGS/SETFPREGS/GETREGSET(NT_PRFPREG/NT_X86_XSTATE) return -EIO
//       (FP/vector register-set marshalling is the next batch). TRACEFORK/VFORK/CLONE auto-attach of
//       children + PTRACE_EVENT_{FORK,CLONE,EXIT,VFORK_DONE} group-stops, PTRACE_O_TRACESECCOMP, and
//       async/cross-process signal-delivery stops (a kill(2) from a third process to a stopped tracee)
//       are staged; the interception point (maybe_deliver_signal) is identified in signal.c. Multi-
//       threaded tracees are tracked at process (not per-tid) granularity for now.

#include <limits.h>

// ---- ptrace request numbers (identical on x86-64 and aarch64 Linux) ----
enum {
    PTRACE_TRACEME = 0,
    PTRACE_PEEKTEXT = 1,
    PTRACE_PEEKDATA = 2,
    PTRACE_PEEKUSER = 3,
    PTRACE_POKETEXT = 4,
    PTRACE_POKEDATA = 5,
    PTRACE_POKEUSER = 6,
    PTRACE_CONT = 7,
    PTRACE_KILL = 8,
    PTRACE_SINGLESTEP = 9,
    PTRACE_GETREGS = 12,
    PTRACE_SETREGS = 13,
    PTRACE_GETFPREGS = 14,
    PTRACE_SETFPREGS = 15,
    PTRACE_ATTACH = 16,
    PTRACE_DETACH = 17,
    PTRACE_SYSCALL = 24,
    PTRACE_SETOPTIONS = 0x4200,
    PTRACE_GETEVENTMSG = 0x4201,
    PTRACE_GETSIGINFO = 0x4202,
    PTRACE_SETSIGINFO = 0x4203,
    PTRACE_GETREGSET = 0x4204,
    PTRACE_SETREGSET = 0x4205,
    PTRACE_SEIZE = 0x4206,
    PTRACE_INTERRUPT = 0x4207,
    PTRACE_LISTEN = 0x4208,
};

// ---- SETOPTIONS bits ----
enum {
    PT_O_TRACESYSGOOD = 0x01,
    PT_O_TRACEFORK = 0x02,
    PT_O_TRACEVFORK = 0x04,
    PT_O_TRACECLONE = 0x08,
    PT_O_TRACEEXEC = 0x10,
    PT_O_TRACEVFORKDONE = 0x20,
    PT_O_TRACEEXIT = 0x40,
    PT_O_TRACESECCOMP = 0x80,
    PT_O_EXITKILL = 0x100,
};

// ---- PTRACE_EVENT_* ----
enum {
    PTEV_FORK = 1,
    PTEV_VFORK = 2,
    PTEV_CLONE = 3,
    PTEV_EXEC = 4,
    PTEV_VFORK_DONE = 5,
    PTEV_EXIT = 6,
    PTEV_STOP = 128
};

// ---- resume commands ----
enum { PTC_NONE = 0, PTC_CONT = 1, PTC_SYSCALL = 2, PTC_SINGLESTEP = 3, PTC_DETACH = 4, PTC_KILL = 5, PTC_LISTEN = 6 };

// ---- stop kinds ----
enum { PTS_NONE = 0, PTS_SYSCALL_ENTRY = 1, PTS_SYSCALL_EXIT = 2, PTS_SIGNAL = 3, PTS_GROUP = 4, PTS_EXEC = 5 };

#define NT_PRSTATUS_ 1

// x86 vs aarch64 discriminant (matches the idiom os/linux/signal.c uses): x86-64 O_DIRECTORY == 0x10000.
#if G_O_DIRECTORY == 0x10000
#define PT_X86 1
#define PT_RAWNR(c) ((uint64_t)(c)->r[0]) // x86-64 syscall nr = rax (pre-normalization)

static int pt_is_execve(uint64_t nr) {
    return nr == 59 || nr == 322;
}

#define PT_REGWORDS 27
#define PT_REGBYTES (PT_REGWORDS * 8)
#else
#define PT_X86 0
#define PT_RAWNR(c) ((uint64_t)(c)->x[8]) // aarch64 syscall nr = x8

static int pt_is_execve(uint64_t nr) {
    return nr == 221 || nr == 281;
}

#define PT_REGWORDS 34
#define PT_REGBYTES (PT_REGWORDS * 8)
#endif

static void pt_usleep(long us) {
    struct timespec t = {us / 1000000, (us % 1000000) * 1000};
    nanosleep(&t, NULL);
}

// guest pid for a host pid (container init's host pid shows through as guest pid 1)
static int pt_gpid(int hostpid) {
    return (g_init_hostpid && hostpid == g_init_hostpid) ? 1 : hostpid;
}

// host pid for a guest pid (inverse; used to kill(2)/existence-check the target host process)
static int pt_hostpid(int gpid) {
    return (gpid == 1 && g_init_hostpid) ? g_init_hostpid : gpid;
}

static void pt_lock(void) {
    while (__atomic_exchange_n(&g_pt->lock, 1, __ATOMIC_ACQUIRE))
        pt_usleep(20);
}

static void pt_unlock(void) {
    __atomic_store_n(&g_pt->lock, 0, __ATOMIC_RELEASE);
}

// mmap the shared arena ONCE, before any guest fork (called from engine_global_init in each target).
static void ptrace_arena_init(void) {
    if (g_pt) return;
    void *p = mmap(NULL, sizeof(struct pt_arena), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (p == MAP_FAILED) {
        g_pt = NULL;
        return;
    } // ptrace unavailable -> requests EPERM/ESRCH, never crash
    memset(p, 0, sizeof(struct pt_arena));
    g_pt = (struct pt_arena *)p;
}

// ---- per-process cached self-link lookup (re-scans only when the arena generation or our pid changes) ----
static uint64_t g_pt_seen_gen = ~0ull;
static int g_pt_self_pid = -1;
static struct pt_link *g_pt_self;

static struct pt_link *ptrace_lookup_self(void) {
    if (!g_pt || __atomic_load_n(&g_pt->nactive, __ATOMIC_RELAXED) == 0) return NULL;
    int me = container_pid();
    uint64_t gen = __atomic_load_n(&g_pt->gen, __ATOMIC_ACQUIRE);
    if (gen == g_pt_seen_gen && me == g_pt_self_pid) return g_pt_self;
    g_pt_seen_gen = gen;
    g_pt_self_pid = me;
    g_pt_self = NULL;
    for (int i = 0; i < PT_MAXLINK; i++) {
        struct pt_link *L = &g_pt->link[i];
        if (L->used && L->attached && L->tracee_pid == me) {
            g_pt_self = L;
            break;
        }
    }
    return g_pt_self;
}

// find the link tracer `me` holds for tracee `tpid` (NULL if none)
static struct pt_link *pt_find_tracee(int me, int tpid) {
    if (!g_pt) return NULL;
    for (int i = 0; i < PT_MAXLINK; i++) {
        struct pt_link *L = &g_pt->link[i];
        if (L->used && L->tracer_pid == me && L->tracee_pid == tpid) return L;
    }
    return NULL;
}

static struct pt_link *pt_alloc(int tracer, int tracee) {
    pt_lock();
    struct pt_link *L = NULL;
    for (int i = 0; i < PT_MAXLINK; i++)
        if (!g_pt->link[i].used) {
            L = &g_pt->link[i];
            break;
        }
    if (L) {
        memset(L, 0, sizeof *L);
        L->used = 1;
        L->tracer_pid = tracer;
        L->tracee_pid = tracee;
        __atomic_add_fetch(&g_pt->nactive, 1, __ATOMIC_SEQ_CST);
        __atomic_add_fetch(&g_pt->gen, 1, __ATOMIC_SEQ_CST);
    }
    pt_unlock();
    return L;
}

// Is ptrace in use anywhere in this session? Gates ALL wait4 ptrace routing. 0 for the whole non-ptrace
// matrix -> wait4 runs its original host-wait4 path byte-identically (no handler armed, no restructure, no
// spurious EINTR). Only when a tracee link exists (nactive > 0) does the tracer's wait4 use the ptrace pump.
static int ptrace_wait_active(void) {
    return g_pt && __atomic_load_n(&g_pt->nactive, __ATOMIC_RELAXED) > 0;
}

// Notify a tracer that its tracee changed ptrace-stop state (mirrors Linux SIGCHLD-to-tracer). Wakes a
// tracer blocked in a plain host wait4 / nanosleep so it observes the stop. It is a real cross-process
// kill of the tracer's engine (exactly the SIGCHLD Linux raises to a parent on a child stop), never sent
// to anyone but this link's tracer.
static void pt_notify_tracer(struct pt_link *L) {
    int hp = pt_hostpid(L->tracer_pid);
    if (hp > 0 && hp != getpid()) kill(hp, SIGCHLD);
}

// tracer-wait race, scoped-and-transparent SIGCHLD wake. The classic strace ordering is: parent
// forks, then waitpid()s and BLOCKS in a plain host wait4 -- BEFORE the child runs PTRACE_TRACEME (so at
// wait entry the parent is not yet a tracer and nactive is 0). When the child then traces itself and
// stops, its stop must interrupt the parent's already-blocked wait4. We arm a benign SIGCHLD handler
// ONLY around the actual blocking wait4 (pt_wait_arm just before it, pt_wait_disarm right after,
// restoring the prior disposition), and ONLY if the guest has NOT installed its own SIGCHLD handler
// (if it has, that handler already interrupts wait4 and we must not clobber it). Because it is scoped to
// the wait4 syscall and restored immediately, NO other syscall's blocking is ever affected, and the
// guest's waitpid never returns a spurious EINTR (the do/while retries internally). A guest that never
// calls wait4 (e.g. the pipe-eof case) is never armed at all. The handler does nothing but exist (so
// wait4 EINTRs); it sets no pending bit -> no phantom signal reaches the guest.
static void pt_wait_wake_h(int s) {
    (void)s;
}

static int pt_wait_arm(struct sigaction *saved) {
    if (!g_pt) return 0;
    // Arm ONLY when the guest's SIGCHLD disposition is SIG_DFL (0). A guest custom handler (>1) already
    // interrupts wait4; SIG_IGN (1) means "auto-reap, don't wait" and must be left untouched so we don't
    // change zombie-reaping semantics. SIG_DFL is the common no-SIGCHLD-handling case (incl. strace).
    if (g_sigact[17].handler != 0) return 0;
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = pt_wait_wake_h; // no SA_RESTART -> a child-stop SIGCHLD interrupts the blocking wait4
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGCHLD, &sa, saved) != 0) return 0;
    return 1;
}

static void pt_wait_disarm(int armed, const struct sigaction *saved) {
    if (armed) sigaction(SIGCHLD, saved, NULL); // restore the prior disposition (default, or none)
}

static void pt_free(struct pt_link *L) {
    pt_lock();
    if (L->used) {
        L->used = 0;
        L->attached = 0;
        __atomic_sub_fetch(&g_pt->nactive, 1, __ATOMIC_SEQ_CST);
        __atomic_add_fetch(&g_pt->gen, 1, __ATOMIC_SEQ_CST);
    }
    pt_unlock();
}

// ---- register marshalling: guest cpu <-> Linux user_regs_struct (NT_PRSTATUS) ----
static void ptrace_publish_regs(struct cpu *c, struct pt_link *L, uint64_t orig_nr) {
    uint64_t g[PT_REGWORDS];
#if PT_X86
    const uint64_t *R = c->r; // rax,rcx,rdx,rbx,rsp,rbp,rsi,rdi,r8..r15
    g[0] = R[15];
    g[1] = R[14];
    g[2] = R[13];
    g[3] = R[12];
    g[4] = R[5];
    g[5] = R[3];
    g[6] = R[11];
    g[7] = R[10];
    g[8] = R[9];
    g[9] = R[8];
    g[10] = R[0];
    g[11] = R[1];
    g[12] = R[2];
    g[13] = R[6];
    g[14] = R[7];
    g[15] = orig_nr; // orig_rax
    g[16] = c->rip;
    g[17] = 0x33;
    g[18] = nzcv_to_eflags(c->nzcv);
    g[19] = R[4];
    g[20] = 0x2b;
    g[21] = c->fs_base;
    g[22] = c->gs_base;
    g[23] = 0;
    g[24] = 0;
    g[25] = 0;
    g[26] = 0;
    L->arch = 0;
#else
    (void)orig_nr;
    for (int i = 0; i < 31; i++)
        g[i] = c->x[i];
    g[31] = c->sp;
    g[32] = c->pc;
    g[33] = c->nzcv; // regs[31]=sp, regs[32]=pc, regs[33]=pstate
    L->arch = 1;
#endif
    for (int i = 0; i < PT_REGWORDS; i++)
        L->regs[i] = g[i];
    L->reglen = PT_REGBYTES;
}

static void ptrace_apply_regs(struct cpu *c, struct pt_link *L) {
    uint64_t g[PT_REGWORDS];
    for (int i = 0; i < PT_REGWORDS; i++)
        g[i] = L->regs[i];
#if PT_X86
    uint64_t *R = c->r;
    R[15] = g[0];
    R[14] = g[1];
    R[13] = g[2];
    R[12] = g[3];
    R[5] = g[4];
    R[3] = g[5];
    R[11] = g[6];
    R[10] = g[7];
    R[9] = g[8];
    R[8] = g[9];
    R[0] = g[10];
    R[1] = g[11];
    R[2] = g[12];
    R[6] = g[13];
    R[7] = g[14];
    c->rip = g[16];
    c->nzcv = eflags_to_nzcv(g[18]);
    R[4] = g[19];
    c->fs_base = g[21];
    c->gs_base = g[22];
#else
    for (int i = 0; i < 31; i++)
        c->x[i] = g[i];
    c->sp = g[31];
    c->pc = g[32];
    c->nzcv = g[33];
#endif
}

// ---- tracee-side memory servicing (runs only while the tracee is stopped in ptrace_stop) ----
static void ptrace_service_mem(struct pt_link *L) {
    unsigned s = __atomic_load_n(&L->mem_seq, __ATOMIC_ACQUIRE);
    if (s == L->mem_ack) return;
    uint64_t addr = L->mem_addr, len = L->mem_len;
    if (len > PT_MEMBUF) len = PT_MEMBUF;
    int err = 0;
    if (L->mem_dir == 1) { // read: tracee copies its own guest memory into the shared buffer
        if (host_range_mapped((uintptr_t)addr, (size_t)len))
            memcpy((void *)L->mem_buf, (void *)(uintptr_t)addr, len);
        else
            err = EIO;
    } else if (L->mem_dir == 2) { // write
        if (host_range_mapped((uintptr_t)addr, (size_t)len))
            memcpy((void *)(uintptr_t)addr, (void *)L->mem_buf, len);
        else
            err = EIO;
    }
    L->mem_err = err;
    __atomic_store_n(&L->mem_ack, s, __ATOMIC_RELEASE);
}

// tracer-side one-chunk memory transfer against a stopped tracee (<= PT_MEMBUF bytes)
static int pt_mem_chunk(struct pt_link *L, uint64_t addr, uint8_t *buf, uint64_t len, int is_write) {
    if (len > PT_MEMBUF) len = PT_MEMBUF;
    if (is_write) memcpy((void *)L->mem_buf, buf, len);
    L->mem_addr = addr;
    L->mem_len = len;
    L->mem_dir = is_write ? 2 : 1;
    unsigned s = L->mem_ack + 1;
    __atomic_store_n(&L->mem_seq, s, __ATOMIC_RELEASE);
    for (int i = 0; i < 40000; i++) { // ~2s cap; the stopped tracee services within a poll tick
        if (__atomic_load_n(&L->mem_ack, __ATOMIC_ACQUIRE) == s) {
            if (L->mem_err) return -L->mem_err;
            if (!is_write) memcpy(buf, (void *)L->mem_buf, len);
            return (int)len;
        }
        if (!L->used || L->stopstate == 0) return -ESRCH; // tracee resumed/died mid-transfer
        pt_usleep(50);
    }
    return -EIO;
}

static long pt_mem_xfer(struct pt_link *L, uint64_t addr, uint8_t *buf, uint64_t len, int is_write) {
    uint64_t done = 0;
    while (done < len) {
        uint64_t n = len - done;
        if (n > PT_MEMBUF) n = PT_MEMBUF;
        int r = pt_mem_chunk(L, addr + done, buf + done, n, is_write);
        if (r < 0) return done ? (long)done : r;
        done += (uint64_t)r;
        if ((uint64_t)r < n) break;
    }
    return (long)done;
}

// ---- the tracee stop: publish already done by caller; block until a resume command ----
// Returns the resume command the tracer issued (PTC_*).
static int ptrace_stop(struct cpu *c, struct pt_link *L, int kind, int status, int sig, int event, unsigned long emsg) {
    L->stopkind = kind;
    L->stopsig = sig;
    L->event = event;
    L->eventmsg = emsg;
    L->waitstatus = status;
    memset((void *)L->siginfo, 0, 128);
    *(int *)(L->siginfo + 0) = sig > 0 ? sig : 5;                                                  // si_signo
    *(int *)(L->siginfo + 8) = (kind == PTS_SYSCALL_ENTRY || kind == PTS_SYSCALL_EXIT) ? 0x85 : 0; // si_code hint
    __atomic_store_n(&L->reported, 0, __ATOMIC_SEQ_CST);
    __atomic_store_n(&L->stopstate, 1, __ATOMIC_SEQ_CST); // publish STOPPED after regs/status are set
    pt_notify_tracer(L); // SIGCHLD the tracer so a blocked host wait4 wakes and observes this stop
    int cmd = PTC_CONT;
    for (;;) {
        ptrace_service_mem(L);
        unsigned cs = __atomic_load_n(&L->cmd_seq, __ATOMIC_ACQUIRE);
        if (cs != L->ack_seq) {
            cmd = L->cmd;
            L->ack_seq = cs;
            if (cmd) break;
        }
        if (!L->used || !L->attached) {
            cmd = PTC_DETACH;
            break;
        }
        if (__atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) {
            cmd = PTC_KILL;
            break;
        }
        pt_usleep(120);
    }
    if (L->regs_dirty) {
        ptrace_apply_regs(c, L);
        L->regs_dirty = 0;
    }
    __atomic_store_n(&L->stopstate, 0, __ATOMIC_SEQ_CST);
    if (cmd == PTC_SYSCALL)
        L->syscall_mode = 1;
    else if (cmd == PTC_CONT || cmd == PTC_SINGLESTEP || cmd == PTC_LISTEN)
        L->syscall_mode = 0;
    if (cmd == PTC_DETACH) L->attached = 0;
    if (cmd == PTC_KILL) {
        c->exited = 1;
        if (c->exit_code == 0) c->exit_code = 128 + 9;
    }
    return cmd;
}

// ---- signal-delivery / group stop (called from raise_guest_signal in signal.c) ----
static int ptrace_intercept_signal(struct cpu *c, int sig, int *out_sig) {
    *out_sig = sig;
    if (!g_pt || __atomic_load_n(&g_pt->nactive, __ATOMIC_RELAXED) == 0) return 0;
    struct pt_link *L = ptrace_lookup_self();
    if (!L) return 0;
    if (sig == 9) return 0; // SIGKILL is not stoppable
    if (L->inject_pass == sig) {
        L->inject_pass = 0;
        return 0;
    } // a just-injected signal: deliver once
    int grp = (sig == 19 || sig == 20 || sig == 21 || sig == 22); // SIGSTOP/TSTP/TTIN/TTOU -> group-stop
    ptrace_publish_regs(c, L, PT_RAWNR(c));
    ptrace_stop(c, L, grp ? PTS_GROUP : PTS_SIGNAL, (sig << 8) | 0x7f, sig, 0, 0);
    int inj = L->cmd_sig;
    L->cmd_sig = 0;
    if (inj > 0) {
        L->inject_pass = inj;
        *out_sig = inj;
        return 1;
    } // deliver the tracer's signal, no re-trap
    *out_sig = 0;
    return 1; // suppressed
}

// ---- the traced-process syscall dispatcher (replaces service_local when g_pt->nactive > 0) ----
static void ptrace_service_traced(struct cpu *c) {
    struct pt_link *L = ptrace_lookup_self();
    if (!L) {
        service_local(c);
        return;
    } // someone else is traced, not us
    uint64_t rawnr = PT_RAWNR(c);
    // ATTACH/SEIZE/INTERRUPT initial stop: report a group-stop (SIGSTOP) at the next syscall boundary.
    if (L->pending_attach_stop) {
        L->pending_attach_stop = 0;
        ptrace_publish_regs(c, L, rawnr);
        ptrace_stop(c, L, PTS_GROUP, (19 << 8) | 0x7f, 19, 0, 0);
        if (!L->attached) {
            service_local(c);
            return;
        }
    }
    // syscall-ENTRY stop (only when the tracer armed PTRACE_SYSCALL)
    if (L->syscall_mode) {
        L->entry_nr = rawnr;
        ptrace_publish_regs(c, L, rawnr);
        int ssig = (L->options & PT_O_TRACESYSGOOD) ? (5 | 0x80) : 5;
        ptrace_stop(c, L, PTS_SYSCALL_ENTRY, (ssig << 8) | 0x7f, ssig, 0, 0);
    }
    service_local(c);
    // exec-stop: a successful execve redirected into the new image -> SIGTRAP (or PTRACE_EVENT_EXEC) stop.
    if (pt_is_execve(rawnr) && c->redirect) {
        ptrace_publish_regs(c, L, rawnr);
        if (L->options & PT_O_TRACEEXEC)
            ptrace_stop(c, L, PTS_EXEC, ((((PTEV_EXEC) << 8) | 5) << 8) | 0x7f, 5, PTEV_EXEC,
                        (unsigned long)L->tracee_pid);
        else
            ptrace_stop(c, L, PTS_EXEC, (5 << 8) | 0x7f, 5, 0, 0);
        return;
    }
    // syscall-EXIT stop (only if still armed after the entry stop's resume command)
    if (L->syscall_mode) {
        ptrace_publish_regs(c, L, L->entry_nr); // orig_rax = the entry number
        int ssig = (L->options & PT_O_TRACESYSGOOD) ? (5 | 0x80) : 5;
        ptrace_stop(c, L, PTS_SYSCALL_EXIT, (ssig << 8) | 0x7f, ssig, 0, 0);
    }
}

// ---- helpers used by proc.c wait4 / mem.c pvm ----
static int ptrace_any_tracee_of_self(void) {
    if (!g_pt || __atomic_load_n(&g_pt->nactive, __ATOMIC_RELAXED) == 0) return 0;
    int me = container_pid();
    for (int i = 0; i < PT_MAXLINK; i++)
        if (g_pt->link[i].used && g_pt->link[i].tracer_pid == me) return 1;
    return 0;
}

// wait filter: Linux wait4 pid arg -- <-1 pgid, -1 any, 0 own pgrp, >0 exact.
static int pt_wait_match(int wpid, int tracee) {
    return wpid == -1 || wpid == 0 || wpid == tracee;
}

// The tracer's wait4 pump. Returns 1 iff it produced a result (*out = pid or -errno, *status Linux-encoded).
static int ptrace_wait(struct cpu *c, pid_t wpid, int opts, struct rusage *ru, int *status, pid_t *out) {
    int me = container_pid();
    for (;;) {
        // 1) a reportable ptrace-stop of one of our tracees?
        for (int i = 0; i < PT_MAXLINK; i++) {
            struct pt_link *L = &g_pt->link[i];
            if (!L->used || L->tracer_pid != me) continue;
            if (!pt_wait_match((int)wpid, L->tracee_pid)) continue;
            if (__atomic_load_n(&L->stopstate, __ATOMIC_ACQUIRE) == 1 &&
                __atomic_load_n(&L->reported, __ATOMIC_ACQUIRE) == 0) {
                __atomic_store_n(&L->reported, 1, __ATOMIC_SEQ_CST);
                *status = L->waitstatus; // already Linux-encoded (WSTOPPED + TRACESYSGOOD/EVENT bits)
                *out = L->tracee_pid;
                return 1;
            }
        }
        // 2) a real child state change (a tracee that EXITED shows up here; reap + free its link).
        int st = 0;
        pid_t r = wait4((pid_t)wpid, &st, opts | WNOHANG, ru);
        if (r > 0) {
            // translate macOS status -> Linux (same as the plain wait4 path in proc.c)
            if ((st & 0x7f) != 0 && (st & 0x7f) != 0x7f)
                st = (st & ~0x7f) | (sig_m2l(st & 0x7f) & 0x7f);
            else if ((st & 0xff) == 0x7f)
                st = (st & ~0xff00) | ((sig_m2l((st >> 8) & 0xff) & 0xff) << 8);
            struct pt_link *L = pt_find_tracee(me, pt_gpid((int)r));
            if (L) pt_free(L);
            *status = st;
            *out = r;
            return 1;
        }
        if (r < 0 && errno == ECHILD) {
            // No real children. If we still hold an ATTACH'd (non-child) tracee, keep polling its stops;
            // else genuinely no one to wait for.
            int have = 0;
            for (int i = 0; i < PT_MAXLINK; i++)
                if (g_pt->link[i].used && g_pt->link[i].tracer_pid == me &&
                    pt_wait_match((int)wpid, g_pt->link[i].tracee_pid))
                    have = 1;
            if (!have) {
                *out = (pid_t)-ECHILD;
                return 1;
            }
        } else if (r < 0 && errno != 0 && errno != EINTR && errno != EAGAIN) {
            *out = (pid_t)-errno;
            return 1;
        }
        if (opts & WNOHANG) {
            *status = 0;
            *out = 0;
            return 1;
        } // nothing ready
        // block: poll ptrace stops + host children, honoring signal-driven restart/interrupt.
        if (__atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) {
            *out = (pid_t)-EINTR;
            return 1;
        }
        // A deliverable guest handler interrupts the wait with EINTR (unless SA_RESTART).
        if (errno == EINTR && !syscall_should_restart(c)) {
            *out = (pid_t)-EINTR;
            return 1;
        }
        pt_usleep(200);
    }
}

// cross-process process_vm_readv/writev against a stopped tracee (strace's string-arg reader).
static long ptrace_pvm(struct cpu *c, int is_write, pid_t rpid, const struct iovec *liov, unsigned long ln,
                       const struct iovec *riov, unsigned long rn) {
    (void)c;
    int me = container_pid();
    if (rpid == 0 || (int)rpid == me) return PT_PVM_LOCAL; // same process -> same-address-space memcpy
    struct pt_link *L = pt_find_tracee(me, pt_gpid((int)rpid));
    if (!L || __atomic_load_n(&L->stopstate, __ATOMIC_ACQUIRE) != 1) return -ESRCH;
    // Walk the local + remote iovec arrays in lockstep, moving bytes through the stopped tracee.
    unsigned long li = 0, riX = 0;
    size_t loff = 0, roff = 0;
    long total = 0;
    while (li < ln && riX < rn) {
        uint8_t *lb = (uint8_t *)liov[li].iov_base + loff;
        uint64_t la = liov[li].iov_len - loff;
        uint64_t raddr = (uint64_t)(uintptr_t)riov[riX].iov_base + roff;
        uint64_t rlen = riov[riX].iov_len - roff;
        uint64_t n = la < rlen ? la : rlen;
        if (n) {
            // is_write: local (our) -> remote (tracee); else remote (tracee) -> local (our).
            long r = pt_mem_xfer(L, raddr, lb, n, is_write ? 1 : 0);
            if (r < 0) return total ? total : r;
            total += r;
            if ((uint64_t)r < n) break;
        }
        loff += n;
        roff += n;
        if (loff >= liov[li].iov_len) {
            li++;
            loff = 0;
        }
        if (roff >= riov[riX].iov_len) {
            riX++;
            roff = 0;
        }
    }
    return total;
}

// =============================== the ptrace(2) syscall handler ====================================
// Returns 0 or -errno (rare.c stores it into G_RET). `c` is the CALLER's cpu.
static int svc_ptrace(struct cpu *c, uint64_t req, uint64_t pid, uint64_t addr, uint64_t data) {
    if (!g_pt) return -EPERM; // arena unavailable
    int me = container_pid();

    if (req == PTRACE_TRACEME) {
        struct pt_link *L = ptrace_lookup_self();
        if (L) return 0; // already traced
        int tracer = pt_gpid(getppid());
        L = pt_alloc(tracer, me);
        if (!L) return -ENOMEM;
        L->attached = 1;
        g_pt_seen_gen = ~0ull; // force our own cache to pick up the new link
        return 0;
    }

    int tpid = pt_gpid((int)pid);
    if (req == PTRACE_ATTACH || req == PTRACE_SEIZE) {
        if ((int)pid <= 0) return -ESRCH;
        if (pt_find_tracee(me, tpid)) return -EPERM; // already attached
        int hp = pt_hostpid(tpid);
        if (hp != me && kill(hp, 0) < 0 && errno == ESRCH) return -ESRCH;
        struct pt_link *L = pt_alloc(me, tpid);
        if (!L) return -ENOMEM;
        L->attached = 1;
        L->seized = (req == PTRACE_SEIZE);
        if (req == PTRACE_SEIZE)
            L->options = data; // SEIZE takes options in `data`
        else
            L->pending_attach_stop = 1; // ATTACH stops the tracee (SIGSTOP group-stop)
        return 0;
    }

    // All remaining requests operate on an existing tracee link.
    struct pt_link *L = pt_find_tracee(me, tpid);
    if (!L) return -ESRCH;
    int stopped = __atomic_load_n(&L->stopstate, __ATOMIC_ACQUIRE) == 1;

    switch (req) {
    case PTRACE_SETOPTIONS: L->options = data; return 0;
    case PTRACE_GETEVENTMSG:
        if (!host_range_mapped((uintptr_t)data, sizeof(unsigned long))) return -EFAULT;
        *(unsigned long *)(uintptr_t)data = L->eventmsg;
        return 0;
    case PTRACE_GETSIGINFO:
        if (!host_range_mapped((uintptr_t)data, 128)) return -EFAULT;
        memcpy((void *)(uintptr_t)data, (void *)L->siginfo, 128);
        return 0;
    case PTRACE_SETSIGINFO:
        if (!host_range_mapped((uintptr_t)data, 128)) return -EFAULT;
        memcpy((void *)L->siginfo, (void *)(uintptr_t)data, 128);
        return 0;
    case PTRACE_GETREGS:
        if (!stopped) return -ESRCH;
        if (!host_range_mapped((uintptr_t)data, (size_t)L->reglen)) return -EFAULT;
        memcpy((void *)(uintptr_t)data, (void *)L->regs, (size_t)L->reglen);
        return 0;
    case PTRACE_SETREGS:
        if (!stopped) return -ESRCH;
        if (!host_range_mapped((uintptr_t)data, (size_t)L->reglen)) return -EFAULT;
        memcpy((void *)L->regs, (void *)(uintptr_t)data, (size_t)L->reglen);
        L->regs_dirty = 1;
        return 0;
    case PTRACE_GETREGSET:
    case PTRACE_SETREGSET: {
        if (!stopped) return -ESRCH;
        if (addr != NT_PRSTATUS_) return -EIO; // NT_PRFPREG/NT_X86_XSTATE are staged (FP/vector batch)
        if (!host_range_mapped((uintptr_t)data, sizeof(struct iovec))) return -EFAULT;
        struct iovec *iov = (struct iovec *)(uintptr_t)data;
        size_t n = iov->iov_len < (size_t)L->reglen ? iov->iov_len : (size_t)L->reglen;
        if (!host_range_mapped((uintptr_t)iov->iov_base, n)) return -EFAULT;
        if (req == PTRACE_GETREGSET) {
            memcpy(iov->iov_base, (void *)L->regs, n);
            iov->iov_len = (size_t)L->reglen;
        } else {
            memcpy((void *)L->regs, iov->iov_base, n);
            L->regs_dirty = 1;
        }
        return 0;
    }
    case PTRACE_PEEKUSER: {
        // user-area read: registers live in the low PT_REGBYTES; return that word via *data (raw ABI).
        if (!host_range_mapped((uintptr_t)data, sizeof(uint64_t))) return -EFAULT;
        uint64_t off = addr, val = 0;
        if (off + 8 <= (uint64_t)L->reglen && (off & 7) == 0) val = L->regs[off / 8];
        *(uint64_t *)(uintptr_t)data = val;
        return 0;
    }
    case PTRACE_POKEUSER: {
        uint64_t off = addr;
        if (off + 8 <= (uint64_t)L->reglen && (off & 7) == 0) {
            L->regs[off / 8] = data;
            L->regs_dirty = 1;
        }
        return 0;
    }
    case PTRACE_PEEKTEXT:
    case PTRACE_PEEKDATA: {
        if (!stopped) return -ESRCH;
        if (!host_range_mapped((uintptr_t)data, sizeof(uint64_t))) return -EFAULT;
        uint64_t word = 0;
        long r = pt_mem_xfer(L, addr, (uint8_t *)&word, 8, 0);
        if (r < 0) return (int)r;
        *(uint64_t *)(uintptr_t)data = word; // raw ptrace stores the word at *data, returns 0
        return 0;
    }
    case PTRACE_POKETEXT:
    case PTRACE_POKEDATA: {
        if (!stopped) return -ESRCH;
        uint64_t word = data;
        long r = pt_mem_xfer(L, addr, (uint8_t *)&word, 8, 1);
        return r < 0 ? (int)r : 0;
    }
    case PTRACE_CONT:
    case PTRACE_SYSCALL:
    case PTRACE_SINGLESTEP:
    case PTRACE_LISTEN: {
        if (!stopped) return -ESRCH;
        L->cmd = (req == PTRACE_SYSCALL)      ? PTC_SYSCALL
                 : (req == PTRACE_SINGLESTEP) ? PTC_SINGLESTEP
                 : (req == PTRACE_LISTEN)     ? PTC_LISTEN
                                              : PTC_CONT;
        L->cmd_sig = (int)data; // signal to inject on resume (0 = none)
        __atomic_add_fetch(&L->cmd_seq, 1, __ATOMIC_RELEASE);
        return 0;
    }
    case PTRACE_INTERRUPT:
        L->pending_attach_stop = 1; // stops at the next syscall boundary
        if (stopped) {
            L->cmd = PTC_LISTEN;
            __atomic_add_fetch(&L->cmd_seq, 1, __ATOMIC_RELEASE);
        }
        return 0;
    case PTRACE_DETACH: {
        L->cmd = PTC_DETACH;
        L->cmd_sig = (int)data;
        L->attached = 0;
        __atomic_add_fetch(&L->cmd_seq, 1, __ATOMIC_RELEASE);
        if (!stopped)
            pt_free(L); // not stopped -> just drop the link (tracee already running free)
        else {
            // give the tracee a moment to observe the resume, then reclaim the slot
            for (int i = 0; i < 2000 && L->stopstate == 1; i++)
                pt_usleep(50);
            pt_free(L);
        }
        return 0;
    }
    case PTRACE_KILL: {
        L->cmd = PTC_KILL;
        __atomic_add_fetch(&L->cmd_seq, 1, __ATOMIC_RELEASE);
        kill(pt_hostpid(tpid), SIGKILL);
        return 0;
    }
    case PTRACE_GETFPREGS:
    case PTRACE_SETFPREGS: return -EIO; // staged: FP/vector user_fpregs_struct marshalling (next batch)
    default: return -EIO;               // unknown/unsupported request (honest error, not a fake success)
    }
}
