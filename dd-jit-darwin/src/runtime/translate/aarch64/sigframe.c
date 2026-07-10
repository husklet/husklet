// frontend/aarch64/sigframe.c -- the aarch64 Linux rt_sigframe (per-arch: register layout differs from
// x86_64). os/linux/signal.c drives delivery (pending/mask/translate) and calls these to build/restore.

// Linux aarch64 rt_sigframe: siginfo(128) then ucontext{flags,link,stack(24),
// sigmask@40,...,mcontext@168}; sigcontext{fault,regs[31]@8,sp@256,pc@264,
// pstate@272,reserved@280}. We stash the guest V-regs in the reserved area.
// Linux sigaltstack: SA_ONSTACK asks the kernel to run the handler on the alternate signal stack;
// ss_flags SS_DISABLE(2) means no alt stack is configured.
#define SA_ONSTACK_L 0x08000000u
#define SS_DISABLE_L 2u

static void build_signal_frame(struct cpu *c, int sig) {
    if (g_trace)
        fprintf(stderr, "[sig] deliver %d sp=%llx handler=%llx\n", sig, (unsigned long long)c->sp,
                (unsigned long long)g_sigact[sig].handler);
    // SA_ONSTACK: build the frame on the alternate signal stack, not the interrupted (guest) stack. Runtimes
    // that manage their own threads install handlers with SA_ONSTACK + a per-thread signal stack and then
    // VERIFY the handler ran on it -- Go's adjustSignalStack treats a handler that ran off its gsignal stack
    // as a signal on a foreign (cgo) thread and calls needm(), which in a no-cgo program spins forever in
    // lockextra waiting for an "extra M" that is never created (the go-build/go-run hang). Mirror the kernel:
    // switch to the alt stack only when SA_ONSTACK is set, an alt stack exists, and we are not ALREADY on it
    // (a nested handler keeps growing the same stack), so the interrupted SP saved below stays truthful.
    uint64_t base = c->sp;
    if ((g_sigact[sig].flags & SA_ONSTACK_L) && c->alt_sp && !(c->alt_flags & SS_DISABLE_L) &&
        !(c->sp >= c->alt_sp && c->sp < c->alt_sp + c->alt_size))
        base = c->alt_sp + c->alt_size; // alt stack top; the frame grows down from here
    uint64_t frame = (base - 4688) & ~15ull;
    uint8_t *f = (uint8_t *)frame;
    memset(f, 0, 4688);
    // siginfo.si_signo
    *(int *)(f + 0) = sig;
    *(int *)(f + 8) = g_sigcode[sig];       // si_code (SI_QUEUE for sigqueue, else 0)
    *(uint64_t *)(f + 16) = g_sigaddr[sig]; // si_addr (synchronous fault address; 0 for async)
    *(uint64_t *)(f + 24) = g_sigval[sig];  // si_value (sigqueue's sival_int/ptr)
    // SA_SIGINFO sender identity for a kill/tgkill-delivered signal: the _kill/_rt union overlays si_addr at
    // offset 16 -> si_pid@16, si_uid@20 (async kill has si_addr==0, so this simply fills those 8 bytes).
    if (g_sigpid[sig]) {
        *(int *)(f + 16) = g_sigpid[sig];
        *(int *)(f + 20) = g_siguid[sig];
    }
    g_sigcode[sig] = 0;
    g_sigval[sig] = 0;
    g_sigaddr[sig] = 0;
    g_sigpid[sig] = 0;
    g_siguid[sig] = 0; // consumed
    // uc_mcontext sits at uc+176, NOT uc+168: mcontext_t is 16-byte aligned, so the kernel/glibc pad 8 bytes
    // after the 128-byte uc_sigmask (which ends at 168). The old mc=168 placed EVERY sigcontext field -- the
    // saved regs, sp, pc, pstate, and the __reserved FPSIMD area -- 8 bytes early, so a handler reading
    // uc_mcontext (Go's async-preempt pc, a crash reporter's registers, the FPSIMD record) saw shifted
    // garbage and could not locate the SIMD context. Match the real frame exactly.
    uint64_t uc = frame + 128, mc = uc + 176;
    // uc_stack: expose the configured sigaltstack (ss_sp@16, ss_flags@24, ss_size@32) so a handler or crash
    // reporter can discover the active stack -- Linux fills this from the task's sigaltstack. SS_DISABLE when
    // none is configured; SS_ONSTACK(1) when this handler is being delivered on the alt stack. Was left zero.
    *(uint64_t *)(uc + 16) = c->alt_sp;   // ss_sp
    *(uint64_t *)(uc + 32) = c->alt_size; // ss_size
    *(int *)(uc + 24) = (!c->alt_sp || (c->alt_flags & SS_DISABLE_L)) ? (int)SS_DISABLE_L
                        : (base != c->sp)                             ? 1 /*SS_ONSTACK*/
                                                                      : 0;
    // uc_sigmask (signal mask to restore)
    *(uint64_t *)(uc + 40) = c->sigmask;
    for (int i = 0; i < 31; i++)
        *(uint64_t *)(mc + 8 + i * 8) = c->x[i];
    *(uint64_t *)(mc + 256) = c->sp;
    // The interrupted PC is GUEST-VISIBLE: the handler reads it (and Go looks it up in pclntab / rewrites it
    // for async-preempt). A non-PIE ET_EXEC runs c->pc biased HIGH, but its pclntab is keyed on the LOW link
    // vaddr -- a HIGH pc is "unknown pc" to the guest runtime. Hand over the UN-BIASED (low) pc; do_sigreturn
    // reads it back and the dispatcher re-biases low->high on resume. pcrel_base is identity for PIE.
    *(uint64_t *)(mc + 264) = pcrel_base(c->pc);
    *(uint64_t *)(mc + 272) = c->nzcv;
    // preserve NEON across the handler. Linux wraps it in a struct fpsimd_context inside uc_mcontext.
    // __reserved, tagged with an _aarch64_ctx{magic=FPSIMD_MAGIC, size} header so handlers and unwind/crash
    // tooling can locate the SIMD state (they scan the reserved chain for FPSIMD_MAGIC). The old code dumped
    // raw NEON bytes with no header, so the record was invisible. Layout: head(8) + fpsr(4) + fpcr(4) +
    // vregs[32] (512) = 528, then a null _aarch64_ctx terminator ends the chain. __reserved is 16-aligned
    // within the mcontext, so it starts at mc+288 (the 280-byte sigcontext head rounded up to 16).
    uint8_t *rsv = (uint8_t *)(mc + 288);
    *(uint32_t *)(rsv + 0) = 0x46508001u;    // FPSIMD_MAGIC
    *(uint32_t *)(rsv + 4) = 528u;           // sizeof(struct fpsimd_context)
    *(uint32_t *)(rsv + 8) = 0;              // fpsr (not separately modelled)
    *(uint32_t *)(rsv + 12) = 0;             // fpcr
    memcpy(rsv + 16, c->v, sizeof c->v);     // vregs[32]
    *(uint32_t *)(rsv + 16 + sizeof c->v + 0) = 0; // terminator magic = 0
    *(uint32_t *)(rsv + 16 + sizeof c->v + 4) = 0; // terminator size  = 0
    c->x[0] = (uint64_t)sig;
    c->x[1] = frame;
    // handler(signo, siginfo*, ucontext*)
    c->x[2] = uc;
    // return address -> sigreturn
    c->x[30] = SIGRETURN_PC;
    c->sp = frame;
    c->pc = g_sigact[sig].handler;
    c->sigmask |= g_sigact[sig].mask;
    // SA_NODEFER (sigset_t bit N-1)
    if (!(g_sigact[sig].flags & 0x40000000)) c->sigmask |= (1ull << (sig - 1));
}

static void do_sigreturn(struct cpu *c) {
    uint64_t frame = c->sp, uc = frame + 128, mc = uc + 176; // mcontext at uc+176 (see build_signal_frame)
    for (int i = 0; i < 31; i++)
        c->x[i] = *(uint64_t *)(mc + 8 + i * 8);
    c->sp = *(uint64_t *)(mc + 256);
    c->pc = *(uint64_t *)(mc + 264);
    c->nzcv = *(uint64_t *)(mc + 272);
    // vregs live in the fpsimd_context after its 16-byte header (magic/size/fpsr/fpcr), at __reserved (mc+288).
    memcpy(c->v, (void *)(mc + 288 + 16), sizeof c->v);
    c->sigmask = *(uint64_t *)(uc + 40);
}

// Synchronous-fault delivery support (driven by os/linux/signal.c's deliver_guest_fault). In a translated
// aarch64 block all NON-stolen guest GPRs live in the matching host x-register, and the guest SP/flags/V
// state is the live host SP/NZCV/V state; the engine-stolen regs (x16/x17/x18/x28/x30) are kept in cpu->x[]
// at every instruction boundary. So reconstruct the guest state by copying the host fault context back into
// cpu, leaving the stolen regs untouched. block_return (jit/dispatch.c, included later) unwinds a block back
// to the dispatcher: it restores the host callee-saved state run_block saved at block entry and returns to
// the run_guest loop, which then sees cpu->pc == handler and runs it.
static void block_return(void);

static int sigframe_capture_fault(struct cpu *c, void *ucv) {
    ucontext_t *uc = (ucontext_t *)ucv;
    uint64_t hpc = (uint64_t)uc->uc_mcontext->__ss.__pc;
    extern int jit_pc_in_retained_cache(uint64_t pc);
    if (!jit_pc_in_retained_cache(hpc)) return 0; // host PC outside all retained code caches -> engine fault
    uint64_t *X = uc->uc_mcontext->__ss.__x; // __x[0..28], then fp=X[29] lr=X[30] sp=X[31]
    for (int r = 0; r <= 30; r++)
        if (!is_stolen(r)) c->x[r] = X[r];
    c->sp = X[31];
    c->nzcv = uc->uc_mcontext->__ss.__cpsr;
    memcpy(c->v, uc->uc_mcontext->__ns.__v, sizeof c->v);
    return 1;
}

static void sigframe_resume_dispatch(struct cpu *c, void *ucv) {
    ucontext_t *uc = (ucontext_t *)ucv;
    uc->uc_mcontext->__ss.__x[0] = (uint64_t)c; // block_return reads &cpu from x0
    uc->uc_mcontext->__ss.__pc = (uint64_t)block_return;
}
