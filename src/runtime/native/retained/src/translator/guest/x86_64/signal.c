#ifndef _GNU_SOURCE
#define _GNU_SOURCE 1
#endif

#include "frame.h"

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "cpu.h"
#include "../../../host/native_context.h"

// translator/guest/x86_64/signal.c -- the x86-64 Linux rt_sigframe (per-arch register layout). os/linux/
// signal.c drives delivery + owns g_sigact/SIGRETURN_PC/g_pending; these build/restore the frame.

// x86-64 sigcontext gregs index -> guest cpu->r[] index (r8..r15,rdi,rsi,rbp,rbx,rdx,rax,rcx,rsp; then rip,eflags)
static const int GREG2R[16] = {8, 9, 10, 11, 12, 13, 14, 15, 7, 6, 5, 3, 2, 0, 1, 4}; // gregs[0..15]

uint64_t hl_x86_signal_nzcv_to_eflags(uint64_t nz) {
    uint64_t f = 0x2; // bit1 reserved (always 1)
    if (!((nz >> 29) & 1)) f |= 1u << 0;
    if ((nz >> 30) & 1) f |= 1u << 6; // CF (stored inverted), ZF
    if ((nz >> 31) & 1) f |= 1u << 7;
    if ((nz >> 28) & 1) f |= 1u << 11; // SF, OF
    return f;
}

uint64_t hl_x86_signal_eflags_to_nzcv(uint64_t f) {
    uint64_t nz = 0;
    if (!(f & 1)) nz |= 1u << 29;
    if (f & (1u << 6)) nz |= 1u << 30; // CF (invert), ZF
    if (f & (1u << 7)) nz |= 1u << 31;
    if (f & (1u << 11)) nz |= 1u << 28; // SF, OF
    return nz;
}

#define SA_ONSTACK_L 0x08000000u
#define SS_DISABLE_L 2u

void hl_x86_signal_build(struct cpu *c, int sig, const hl_x86_signal_state *state) {
    // SA_ONSTACK: build the frame on the alternate signal stack, not the interrupted (guest) stack. Runtimes
    // that manage their own threads install handlers with SA_ONSTACK + a per-thread signal stack and then
    // VERIFY the handler ran on it -- Go's adjustSignalStack treats a handler that ran off its gsignal stack
    // as a signal on a foreign (cgo) thread and calls needm(), which in a no-cgo program spins forever in
    // lockextra waiting for an "extra M" that is never created (the go-build/go-run hang). Mirror the kernel:
    // switch to the alt stack only when SA_ONSTACK is set, an alt stack exists, and we are not ALREADY on it
    // (a nested handler keeps growing the same stack), so the interrupted RSP saved below stays truthful.
    uint64_t base = c->r[4];
    if ((state->flags & SA_ONSTACK_L) && c->alt_sp && !(c->alt_flags & SS_DISABLE_L) &&
        !(c->r[4] >= c->alt_sp && c->r[4] < c->alt_sp + c->alt_size))
        base = c->alt_sp + c->alt_size;                             // alt stack top; the frame grows down from here
    uint64_t sp = (base - 2048) & ~15ull;                           // 16-aligned frame base; uc lives here
    uint64_t uc = sp, mc = uc + 40, info = uc + 512, xs = uc + 768; // ucontext / mcontext(gregs) / siginfo / xmm save
    memset((void *)sp, 0, 2048);
    // uc_stack: expose the configured sigaltstack (ss_sp@16, ss_flags@24, ss_size@32) so a handler or crash
    // reporter can discover the active stack (Linux fills this from the task's sigaltstack). SS_DISABLE when
    // none is configured; SS_ONSTACK(1) when this handler runs on the alt stack. Was left zero.
    *(uint64_t *)(uc + 16) = c->alt_sp;   // ss_sp
    *(uint64_t *)(uc + 32) = c->alt_size; // ss_size
    *(int *)(uc + 24) = (!c->alt_sp || (c->alt_flags & SS_DISABLE_L)) ? (int)SS_DISABLE_L
                        : (base != c->r[4])                           ? 1 /*SS_ONSTACK*/
                                                                      : 0;
    for (size_t i = 0; i < 16; i++)
        *(uint64_t *)(mc + i * 8) = c->r[GREG2R[i]]; // gregs[0..15]
    *(uint64_t *)(mc + 16 * 8) = c->rip;             // gregs[16] = RIP
    *(uint64_t *)(mc + 17 * 8) = hl_x86_signal_nzcv_to_eflags(c->nzcv) | ((c->df & 1) << 10);
    *(uint64_t *)(uc + 296) = c->sigmask;  // uc_sigmask (restored on sigreturn)
    memcpy((void *)xs, c->v, sizeof c->v); // preserve guest xmm across the handler
    // Preserve the EXTENDED vector + x87 state too: the xmm area above holds only the low 128 bits, so a
    // handler that touches YMM/ZMM upper lanes or the x87 stack would otherwise leave the interrupted state
    // corrupted on sigreturn (proven: an AVX handler zeroed the interrupted ymm UPPER lanes). Stash it in the
    // frame's free tail, just past the 256-byte xmm area (well below the handler's rsp = sp-8, so untouched).
    uint64_t xe = xs + sizeof c->v;                      // extended-state save area, inside the 2048B frame
    memcpy((void *)(xe + 0), c->vhi, sizeof c->vhi);     // ymm/zmm bits[128:256) of regs 0..15
    memcpy((void *)(xe + 256), c->kreg, sizeof c->kreg); // AVX-512 opmask k0..k7
    memcpy((void *)(xe + 320), c->st, sizeof c->st);     // x87 register stack (double model)
    *(uint64_t *)(xe + 384) = c->fptop;
    *(uint64_t *)(xe + 392) = c->fpsw;
    *(uint64_t *)(xe + 400) = c->fpcw;
    *(int *)(info + 0) = sig;                   // siginfo.si_signo
    *(int *)(info + 4) = *state->error;         // si_errno (seccomp trap data; otherwise zero)
    *(int *)(info + 8) = *state->code;          // si_code (SI_QUEUE for sigqueue, else 0)
    *(uint64_t *)(info + 16) = *state->address; // si_addr (synchronous fault address; 0 for async)
    *(uint64_t *)(info + 24) = *state->value;   // si_value
    // SA_SIGINFO sender identity for a kill/tgkill signal: _kill/_rt union overlays si_addr@16 -> si_pid@16,
    // si_uid@20 (async kill has si_addr==0, so this fills those 8 bytes).
    if (*state->pid) {
        *(int *)(info + 16) = *state->pid;
        *(int *)(info + 20) = *state->uid;
    }
    *state->error = 0;
    *state->code = 0;
    *state->value = 0;
    *state->address = 0;
    *state->pid = 0;
    *state->uid = 0; // consumed
    uint64_t rsp = sp - 8;
    *(uint64_t *)rsp = state->sigreturn_pc; // pushed return address
    c->r[7] = (uint64_t)sig;
    c->r[6] = info;
    c->r[2] = uc; // handler(signo, siginfo*, ucontext*) in rdi,rsi,rdx
    c->r[4] = rsp;
    c->rip = state->handler;
    c->sigmask |= state->mask;
    if (!(state->flags & 0x40000000)) c->sigmask |= (1ull << (sig - 1)); // SA_NODEFER off -> block this signal
    if (state->trace)
        fprintf(stderr, "[sig] deliver %d handler=%llx rsp=%llx\n", sig, (unsigned long long)c->rip,
                (unsigned long long)rsp);
}

void hl_x86_signal_restore(struct cpu *c) {
    uint64_t uc = c->r[4], mc = uc + 40, xs = uc + 768; // after the handler's ret, rsp == uc
    for (size_t i = 0; i < 16; i++)
        c->r[GREG2R[i]] = *(uint64_t *)(mc + i * 8);
    c->rip = *(uint64_t *)(mc + 16 * 8);
    c->nzcv = hl_x86_signal_eflags_to_nzcv(*(uint64_t *)(mc + 17 * 8));
    c->df = (*(uint64_t *)(mc + 17 * 8) >> 10) & 1; // restore DF a handler may have changed
    c->sigmask = *(uint64_t *)(uc + 296);
    memcpy(c->v, (void *)xs, sizeof c->v);
    // Restore the extended vector + x87 state saved by build_signal_frame (see there), so the interrupted
    // YMM/ZMM upper lanes, opmasks, and x87 stack survive a handler that clobbered them.
    uint64_t xe = xs + sizeof c->v;
    memcpy(c->vhi, (void *)(xe + 0), sizeof c->vhi);
    memcpy(c->kreg, (void *)(xe + 256), sizeof c->kreg);
    memcpy(c->st, (void *)(xe + 320), sizeof c->st);
    c->fptop = *(uint64_t *)(xe + 384);
    c->fpsw = *(uint64_t *)(xe + 392);
    c->fpcw = *(uint64_t *)(xe + 400);
}

// Synchronous-fault delivery support (driven by os/linux/signal.c's deliver_guest_fault; see the matching
// frontend/aarch64/sigframe.c for the model). In a translated x86 block the 16 guest GPRs rax..r15 live in
// host x0..x15 and xmm0..15 in host v0..v15, with cpu pinned in host x28. Reconstruct the guest GPR/xmm
// state from the host fault context (the deferred-flag NZCV is left as last spilled). block_return
// (frontend/x86_64/translate.c) unwinds the block back to the run_guest loop, which runs cpu->rip == handler.
// The only host-dependent code here (with the two functions after it): the frame builders above are pure
// guest ABI, this reads the AArch64 host register file.
int hl_x86_signal_capture(struct cpu *c, void *ucv, hl_x86_signal_cache_fn cache_contains, void *callback_context) {
#if defined(HL_HOST_HAS_A64_CONTEXT)
    ucontext_t *uc = (ucontext_t *)ucv;
    uint64_t hpc = (uint64_t)HL_HOST_UC_PC(uc);
    if (!cache_contains(callback_context, hpc)) return 0; // host PC outside the code cache -> a genuine engine fault
    uint64_t *X = HL_HOST_UC_REGS(uc);
    __uint128_t *V = HL_HOST_UC_VREGS(uc);
    if (V == NULL) return 0;
    for (int i = 0; i < 16; i++)
        c->r[i] = X[i];           // rax..r15 == host x0..x15
    memcpy(c->v, V, sizeof c->v); // xmm0..15 == host v0..v15
    return 1;
#else
    // Nothing to reconstruct from. Return the SAME 0 as an out-of-cache host PC: both callers read 0 as
    // "not a guest fault in a translated block" and re-raise, which is right when no block can run.
    (void)c;
    (void)ucv;
    (void)cache_contains;
    (void)callback_context;
    return 0;
#endif
}

void hl_x86_signal_resume(struct cpu *c, void *ucv, uintptr_t dispatcher_return) {
#if defined(HL_HOST_HAS_A64_CONTEXT)
    ucontext_t *uc = (ucontext_t *)ucv;
    uint64_t cpu_address = (uint64_t)c;
    memcpy(HL_HOST_UC_REGS(uc) + 28, &cpu_address, sizeof(cpu_address));
    HL_HOST_UC_PC(uc) = (uint64_t)dispatcher_return;
#else
    // Unreachable: the only caller runs after hl_x86_signal_capture returned 1. Empty, not abort --
    // aborting inside a signal handler turns a diagnosable "unsupported host" into a second fault.
    (void)c;
    (void)ucv;
    (void)dispatcher_return;
#endif
}

// recover a fast-clock GUARDED store fault (emit_fast_syscall's clock_gettime/gettimeofday inline
// path) as -EFAULT. Called FIRST by the run-path SIGSEGV/SIGBUS guard (jit86_lazyguard) -- before non-PIE
// fixup / SMC / lazy-map / guest-signal delivery -- so a bad guest RESULT pointer returns -EFAULT to the
// guest exactly like the slow (svc_time host_range_mapped) path: it must NOT lazy-map the store target
// (which would flip a correct EFAULT into a bogus success) and must never fault the engine. The emitted
// store armed the window: cpu->fastclk_ptr = the 16-byte guest buffer, cpu->fastclk_resume = the host PC of
// the in-block EFAULT tail (which does `msr nzcv,x17; b L_after`). Guest rax lives in host x0 at the fault,
// so we set mcontext x0 = -EFAULT and redirect the PC to that tail. Inert unless armed (resume != 0), so
// ordinary guest wild-pointer faults fall straight through to deliver_guest_fault as before.
int hl_x86_signal_fast_clock_fault(struct cpu *c, uintptr_t va, void *ucv) {
    if (!c || !c->fastclk_resume) return 0;
    // overflow-safe window test. LTP passes tv=(void*)-1 (fastclk_ptr near UINT64_MAX), where
    // `va >= fastclk_ptr + 16` wraps and wrongly rejects the fault -> SIGSEGV. Compare the unsigned
    // offset instead: an in-window fault has (va - fastclk_ptr) in [0,16); every other value (including
    // va < fastclk_ptr, which underflows to a huge number) is >= 16. Correct for both bounds and wrap.
    if ((uint64_t)(va - c->fastclk_ptr) >= 16) return 0; // fault outside the guarded 16B window
#if defined(HL_HOST_HAS_A64_CONTEXT)
    ucontext_t *uc = (ucontext_t *)ucv;
    uint64_t result = (uint64_t)(int64_t)(-EFAULT);
    memcpy(HL_HOST_UC_REGS(uc), &result, sizeof(result)); // guest rax = -EFAULT
    HL_HOST_UC_PC(uc) = c->fastclk_resume;                // resume at the in-block EFAULT tail
    c->fastclk_resume = 0;                                // window closed
    return 1;
#else
    // Dead: cpu->fastclk_resume is written only by the emitted ARM64 fast-clock arm, which s1_calibrate
    // leaves disabled, so the window test above already returned 0. "Not handled" beats faking an EFAULT.
    (void)ucv;
    return 0;
#endif
}

// Integer divide-by-zero (#DE) reaches the dispatcher as R_DIV/R_IDIV with divop==0. The host cannot
// synthesize a real SIGFPE here -- on Apple Silicon udiv/0 quietly returns 0 and FP /0 traps as SIGILL --
// so deliver it from C, mirroring deliver_guest_fault's queue-and-resume for a synchronous SIGSEGV/SIGBUS.
// If the guest installed a SIGFPE handler, queue it with the Linux #DE siginfo (si_code=FPE_INTDIV,
// si_addr = the faulting insn) and force it deliverable even if blocked; run_guest's maybe_deliver_signal
// then builds the rt_sigframe and runs the handler (which typically siglongjmps out and recovers). Returns
// 1 when queued (caller `continue`s the loop), 0 when no handler is installed (caller default-terminates).
// cpu->rip already holds the architectural #DE PC set by the emitted block exit.
int hl_x86_signal_raise_divide(struct cpu *c, const hl_x86_signal_queue *queue, int si_code) {
    if (queue->handler(queue->context, 8) <= 1) return 0; // SIG_DFL/SIG_IGN
    queue->codes[8] = si_code;                            // FPE_INTDIV / FPE_INTOVF
    queue->addresses[8] = c->rip;                         // si_addr = faulting instruction
    c->sigmask &= ~(1ull << 7); // a synchronous fault forces delivery even if SIGFPE was blocked
    c->reason = R_BRANCH;       // resume as a plain branch (no stale special-op handling)
    __atomic_or_fetch(queue->pending, 1ull << 8, __ATOMIC_SEQ_CST);
    return 1;
}

// int3 (#BP -> SIGTRAP) / UD2 (#UD -> SIGILL): the block exited with R_TRAP carrying (linux_signo |
// si_code<<8) in cpu->divop and the architectural PC in cpu->rip. Deliver the guest signal exactly like
// raise_guest_de: queue it + force it deliverable even if the guest blocked it, then resume as a plain
// branch so run_guest's maybe_deliver_signal builds the rt_sigframe and runs the handler (which typically
// siglongjmps out, or -- for UD2 -- advances rip and resumes). Returns 1 when queued, 0 when the guest
// has no handler (caller default-terminates with 128+signo, mirroring the #DE path).
int hl_x86_signal_raise_trap(struct cpu *c, const hl_x86_signal_queue *queue) {
    int sig = (int)(c->divop & 0xff);
    int code = (int)((c->divop >> 8) & 0xff);
    if (sig < 1 || sig > 64 || queue->handler(queue->context, sig) <= 1) return 0;
    queue->codes[sig] = code;
    queue->addresses[sig] = (sig == 4) ? c->rip : 0;
    c->sigmask &= ~(1ull << (sig - 1)); // a synchronous trap forces delivery even if blocked
    c->reason = R_BRANCH;
    __atomic_or_fetch(queue->pending, 1ull << sig, __ATOMIC_SEQ_CST);
    return 1;
}
