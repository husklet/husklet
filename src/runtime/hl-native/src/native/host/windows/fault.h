#ifndef HL_HOST_WINDOWS_FAULT_H
#define HL_HOST_WINDOWS_FAULT_H

/*
 * Fault interception on a Windows host: the primitive underneath everything the
 * POSIX arms get from sigaction(SIGSEGV/SIGBUS/SIGILL/SIGFPE/SIGTRAP).
 *
 * The mechanism is one process-wide vectored exception handler (VEH), and the
 * choice is not a preference:
 *
 *   - __try/__except is unusable on this toolchain. It compiles under
 *     -fms-extensions and then segfaults; the breakage was localised to the
 *     CRT's __C_specific_handler scope-table glue, not to the emitted xdata and
 *     not to NT's dispatch. Do not reach for it, and do not conclude from it
 *     that table-based SEH is unavailable -- the .seh_handler directive and
 *     RtlAddFunctionTable both work.
 *   - a frame-scoped handler cannot cover the fault sites the engine actually
 *     has. Deliberate probe reads are issued from between translated blocks, and
 *     absolute-data fixups fire from anywhere; no per-scope mechanism spans
 *     that set. A VEH fires on every thread wherever the fault happened, which
 *     is exactly the property a POSIX signal handler has.
 *   - a fault taken inside JIT-emitted code reaches a VEH and resumes from it
 *     with no RtlAddFunctionTable registration at all, on plain RWX pages and on
 *     a dual-alias RW/RX mapping alike. That is structural: vectored handlers
 *     run before the frame walk begins, so unwind info only matters where the
 *     VEH declines. Registering a function table is still worth doing later, for
 *     debuggers and crash reporters -- it is not needed to intercept a fault.
 *
 * Resuming is EXCEPTION_CONTINUE_EXECUTION with a mutated CONTEXT, which is the
 * same operation as returning from a POSIX handler with a mutated ucontext_t --
 * and simpler, because with no signal mask on this host there is no mask restore
 * owed and no hand-rolled sigreturn.
 *
 * What this header does NOT do: classify guest memory, deliver guest signals, or
 * know anything about a guest at all. It hands a classified fault and a mutable
 * register file to one installed callback and does what the callback says.
 */

#include <stdint.h>

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Fault class. The values are the Linux signal numbers the class maps onto, so a
 * consumer that already speaks in guest signal numbers needs no second table.
 */
typedef enum hl_windows_fault_kind {
    HL_WINDOWS_FAULT_NONE = 0,
    HL_WINDOWS_FAULT_ILL = 4,
    HL_WINDOWS_FAULT_TRAP = 5,
    HL_WINDOWS_FAULT_BUS = 7,
    HL_WINDOWS_FAULT_FPE = 8,
    HL_WINDOWS_FAULT_SEGV = 11
} hl_windows_fault_kind;

/*
 * si_code analogue, numerically the Linux value for the pair (kind, code) so it
 * can be handed straight to a guest-signal builder. Windows does not report
 * MAPERR vs ACCERR -- both arrive as EXCEPTION_ACCESS_VIOLATION with identical
 * ExceptionInformation -- so that one distinction is recovered by asking
 * VirtualQuery whether the faulting address is MEM_FREE. That is a real kernel
 * call on the fault path, and it is why the field is filled in here once rather
 * than by every consumer.
 */
enum {
    HL_WINDOWS_FAULT_CODE_NONE = 0,
    /* SEGV */
    HL_WINDOWS_FAULT_CODE_MAPERR = 1,
    HL_WINDOWS_FAULT_CODE_ACCERR = 2,
    /* BUS */
    HL_WINDOWS_FAULT_CODE_ADRALN = 1,
    HL_WINDOWS_FAULT_CODE_ADRERR = 2,
    /* ILL */
    HL_WINDOWS_FAULT_CODE_ILLOPC = 1,
    HL_WINDOWS_FAULT_CODE_PRVOPC = 5,
    /* FPE */
    HL_WINDOWS_FAULT_CODE_INTDIV = 1,
    HL_WINDOWS_FAULT_CODE_INTOVF = 2,
    HL_WINDOWS_FAULT_CODE_FLTDIV = 3,
    HL_WINDOWS_FAULT_CODE_FLTOVF = 4,
    HL_WINDOWS_FAULT_CODE_FLTUND = 5,
    HL_WINDOWS_FAULT_CODE_FLTRES = 6,
    HL_WINDOWS_FAULT_CODE_FLTINV = 7,
    /* TRAP */
    HL_WINDOWS_FAULT_CODE_BRKPT = 1,
    HL_WINDOWS_FAULT_CODE_TRACE = 2
};

/*
 * How the faulting instruction touched the address. This is strictly more than
 * POSIX hands a handler: si_addr says where, never whether. Sites that today
 * infer write-ness from "a read would have been legal under this protection, so
 * it must have been a write" can stop inferring on this host.
 */
enum { HL_WINDOWS_FAULT_ACCESS_READ = 0, HL_WINDOWS_FAULT_ACCESS_WRITE = 1, HL_WINDOWS_FAULT_ACCESS_EXECUTE = 2 };

enum {
    /* address / access are meaningful. Set for SEGV and BUS, clear otherwise. */
    HL_WINDOWS_FAULT_HAS_ADDRESS = 1u << 0,
    /* NT refuses to continue this exception; returning RESUME would raise
     * EXCEPTION_NONCONTINUABLE_EXCEPTION. The dispatcher enforces this even if a
     * handler asks to resume anyway. */
    HL_WINDOWS_FAULT_NONCONTINUABLE = 1u << 1
};

typedef struct hl_windows_fault {
    uint32_t kind;   /* hl_windows_fault_kind */
    uint32_t code;   /* HL_WINDOWS_FAULT_CODE_* */
    uint32_t access; /* HL_WINDOWS_FAULT_ACCESS_* */
    uint32_t flags;  /* HL_WINDOWS_FAULT_HAS_ADDRESS | ... */
    uint64_t address;
    /* EXCEPTION_IN_PAGE_ERROR carries the underlying NTSTATUS of the failed
     * backing-store read in a third information word; zero for every other
     * exception. It is the only way to tell a genuine I/O error on a mapped view
     * from any other kind of bus fault. */
    uint64_t nt_status;
    uint32_t exception_code; /* the raw Win32 code, for anything not modelled above */
    uint32_t reserved;
    /* The live register file. Mutating it and returning RESUME is the whole of
     * "return from the handler with a modified ucontext". */
    CONTEXT *context;
    EXCEPTION_RECORD *record;
} hl_windows_fault;

enum {
    HL_WINDOWS_FAULT_DECLINE = 0, /* -> EXCEPTION_CONTINUE_SEARCH */
    HL_WINDOWS_FAULT_RESUME = 1   /* -> EXCEPTION_CONTINUE_EXECUTION */
};

/*
 * The one installed callback. It runs on the faulting thread, synchronously,
 * from the faulting instruction. Three rules, and the reasons differ from the
 * POSIX ones:
 *
 *   1. Take no engine lock. Not because a handler can interrupt a lock holder --
 *      a vectored handler cannot, it is only ever entered synchronously -- but
 *      because the faulting instruction may itself have been inside a lock this
 *      callback would then re-enter on the same thread.
 *   2. Allocate nothing and log nothing. Both take CRT locks, which is rule 1
 *      again by a different route.
 *   3. Call no API whose first call could load a DLL. A fault can be taken while
 *      the loader lock is held, and the first call through a delay-loaded thunk
 *      resolves it. Resolve at init, use here.
 *
 * Kernel VM calls are safe: VirtualAlloc/VirtualProtect/VirtualQuery take the
 * address-space lock inside the kernel, and a thread that was executing
 * user-mode code cannot already hold it.
 */
typedef int (*hl_windows_fault_handler)(hl_windows_fault *fault, void *context);

/* Install the process-wide handler ahead of any a loaded DLL adds later.
 * Returns 1 on success, 0 if one is already installed or registration failed.
 * Not a stack: exactly one owner, installed once at engine init. */
int hl_windows_fault_install(hl_windows_fault_handler handler, void *context);

/* Unregister. Returns 1 if a handler was installed and is now gone. Must not
 * race a fault on another thread; this is an init/shutdown operation. */
int hl_windows_fault_remove(void);

/* Exceptions the VEH was dispatched for, and those an installed handler resumed
 * from. Diagnostics and tests only -- in particular, "the counter did not move"
 * is the only way to observe that a fault never reached user mode at all. */
void hl_windows_fault_counters(uint64_t *out_seen, uint64_t *out_resumed);

/* --- context accessors ------------------------------------------------------
 * The engine-wide register matrix lives elsewhere and types the fault context as
 * the CONTEXT record a vectored handler receives; these are the few accessors
 * this primitive needs on its own, spelled so that adding the matrix cell later
 * does not have to move them. */

static inline uint64_t hl_windows_fault_pc(const hl_windows_fault *fault) {
    return (uint64_t)fault->context->Rip;
}

static inline void hl_windows_fault_set_pc(const hl_windows_fault *fault, uint64_t pc) {
    fault->context->Rip = (DWORD64)pc;
}

static inline uint64_t hl_windows_fault_sp(const hl_windows_fault *fault) {
    return (uint64_t)fault->context->Rsp;
}

/*
 * Rax..R15 then Rip are consecutive DWORD64 members of CONTEXT in x86
 * register-encoding order, so the flat "base + register number" idiom the POSIX
 * arms use over ucontext gregs survives verbatim, with the natural register
 * numbers as indices. fault.c asserts the layout at compile time rather than
 * trusting this comment.
 */
static inline uint64_t *hl_windows_fault_gprs(const hl_windows_fault *fault) {
    return (uint64_t *)(void *)&fault->context->Rax;
}

/* EFlags is a DWORD sited BEFORE Rax and cannot join that index space; there is
 * deliberately no register number for it. */
static inline uint32_t hl_windows_fault_eflags(const hl_windows_fault *fault) {
    return (uint32_t)fault->context->EFlags;
}

/* xmm0..15, contiguous inside the legacy FXSAVE image. Never NULL -- unlike the
 * optional fpregs pointer on a POSIX host -- but only meaningful, and only
 * restored on resume, when ContextFlags carries CONTEXT_FLOATING_POINT. The
 * dispatcher declines any exception whose context lacks it, so a handler that
 * runs at all may rely on this. The ymm/zmm upper lanes are reachable too, via
 * LocateXStateFeature on fault->context; they are not wrapped here because
 * writing them additionally requires committing the XSTATE feature mask. */
static inline void *hl_windows_fault_xmm(const hl_windows_fault *fault) {
    return (void *)fault->context->FltSave.XmmRegisters;
}

/* --- the setjmp replacement --------------------------------------------------
 *
 * longjmp out of a fault handler is forbidden on this host, unconditionally. The
 * Win64 ABI implements setjmp/longjmp over SEH: longjmp calls RtlUnwindEx to
 * unwind the frames between the jump and its target, and from inside a vectored
 * handler that means unwinding through the kernel's exception dispatcher while
 * the kernel still believes dispatch is in progress. That is not a supported
 * operation, and it is the sort of thing that works on one Windows build and not
 * the next.
 *
 * The replacement is the ten words a mask-less sigsetjmp would have stored -- a
 * resume label, SP, and the callee-saved set -- restored into the fault context.
 * No unwinder is involved at any point, which is the entire reason to prefer it,
 * and it is cheaper than the sigsetjmp it stands in for.
 *
 * Two obligations a POSIX pad does not have:
 *
 *   - anything live across the arm point must survive the resume. The macro
 *     therefore clobbers every volatile register, so the compiler is forced to
 *     keep such values in memory or in a callee-saved register -- and the pad
 *     restores the callee-saved set. Without that clobber list a discriminator
 *     the compiler happened to hold in a volatile register comes back garbage
 *     after a resume, and the classic symptom is a pad that re-arms and
 *     re-faults forever.
 *   - the pad must be re-armed at every entry to the guarded region, exactly as
 *     sigsetjmp is, because the stored rsp names this invocation's frame.
 */
typedef struct hl_windows_fault_pad {
    uint64_t rip;
    uint64_t rsp;
    uint64_t rbp;
    uint64_t rbx;
    uint64_t rsi;
    uint64_t rdi;
    uint64_t r12;
    uint64_t r13;
    uint64_t r14;
    uint64_t r15;
} hl_windows_fault_pad;

#define HL_WINDOWS_FAULT_PAD_ARM(pad)                                                                                  \
    __asm__ __volatile__("leaq 1f(%%rip), %%rax\n\t"                                                                   \
                         "movq %%rax, 0(%0)\n\t"                                                                       \
                         "movq %%rsp, 8(%0)\n\t"                                                                       \
                         "movq %%rbp, 16(%0)\n\t"                                                                      \
                         "movq %%rbx, 24(%0)\n\t"                                                                      \
                         "movq %%rsi, 32(%0)\n\t"                                                                      \
                         "movq %%rdi, 40(%0)\n\t"                                                                      \
                         "movq %%r12, 48(%0)\n\t"                                                                      \
                         "movq %%r13, 56(%0)\n\t"                                                                      \
                         "movq %%r14, 64(%0)\n\t"                                                                      \
                         "movq %%r15, 72(%0)\n\t"                                                                      \
                         "1:\n"                                                                                        \
                         :                                                                                             \
                         : "r"(pad)                                                                                    \
                         : "rax", "rcx", "rdx", "r8", "r9", "r10", "r11", "xmm0", "xmm1", "xmm2", "xmm3", "xmm4",      \
                           "xmm5", "cc", "memory")

/* Point a register file at the pad's landing site. The CONTEXT form is the one a
 * fault handler wants; it is spelled over a bare CONTEXT * rather than over the
 * fault record because a consumer that already holds the native context -- an
 * engine resume path that takes `void *native_context` and knows what it is --
 * should not have to manufacture an hl_windows_fault to reach it. */
static inline void hl_windows_fault_pad_restore(CONTEXT *c, const hl_windows_fault_pad *pad) {
    c->Rip = (DWORD64)pad->rip;
    c->Rsp = (DWORD64)pad->rsp;
    c->Rbp = (DWORD64)pad->rbp;
    c->Rbx = (DWORD64)pad->rbx;
    c->Rsi = (DWORD64)pad->rsi;
    c->Rdi = (DWORD64)pad->rdi;
    c->R12 = (DWORD64)pad->r12;
    c->R13 = (DWORD64)pad->r13;
    c->R14 = (DWORD64)pad->r14;
    c->R15 = (DWORD64)pad->r15;
}

/* The caller returns HL_WINDOWS_FAULT_RESUME afterwards; execution continues
 * immediately after the arm macro, with the callee-saved set and the stack as
 * they were there. */
static inline void hl_windows_fault_pad_resume(const hl_windows_fault *fault, const hl_windows_fault_pad *pad) {
    hl_windows_fault_pad_restore(fault->context, pad);
}

/*
 * Enter the pad from ORDINARY code -- no fault, no handler, no CONTEXT to edit.
 *
 * This is the second of the two routes a sigsetjmp pad has, and on a POSIX host
 * both are siglongjmp. Here they must differ: the fault route edits a context
 * the kernel is about to reload, and this one has to move the machine itself.
 *
 * It is still NOT longjmp. Win64 longjmp is implemented over RtlUnwindEx, which
 * walks and unwinds the frames between here and the target, running whatever
 * unwind handlers they carry. The frames in between are translated blocks and
 * engine internals that own no cleanup and register no handlers, so every one of
 * those unwind steps is work with nothing to do -- and any one of them lacking
 * unwind data is a fatal RtlUnwindEx failure rather than a slow one. Restoring
 * the ten words directly is both cheaper and unconditional.
 *
 * The pad pointer is pinned in rax, which is the one register the restore
 * neither reads nor writes; rsp is loaded LAST, after every value has been read
 * out of the pad, because the pad may itself live on the stack being switched
 * away from. r11 carries the target -- volatile, so the callee-saved set the pad
 * restores is complete when the jump is taken.
 */
__attribute__((noreturn)) static inline void hl_windows_fault_pad_jump(const hl_windows_fault_pad *pad) {
    __asm__ __volatile__("movq 0(%0), %%r11\n\t"
                         "movq 16(%0), %%rbp\n\t"
                         "movq 24(%0), %%rbx\n\t"
                         "movq 32(%0), %%rsi\n\t"
                         "movq 40(%0), %%rdi\n\t"
                         "movq 48(%0), %%r12\n\t"
                         "movq 56(%0), %%r13\n\t"
                         "movq 64(%0), %%r14\n\t"
                         "movq 72(%0), %%r15\n\t"
                         "movq 8(%0), %%rsp\n\t"
                         "jmp *%%r11\n"
                         :
                         : "a"(pad)
                         : "memory");
    __builtin_unreachable();
}

/* --- the kernel-write hole ---------------------------------------------------
 *
 * A vectored handler sees faults taken by user-mode instructions and nothing
 * else. When the KERNEL touches an inaccessible user page on the caller's
 * behalf -- ReadFile into a PAGE_NOACCESS or not-yet-grown destination -- no
 * exception is raised anywhere: the call simply fails with ERROR_NOACCESS and
 * the handler is never entered. Measured: zero dispatches. This is a genuine
 * hole in any VEH-only fault design, and it is the long-unexplained shape of the
 * "git clone crashes" failure earlier Windows Linux-emulation work carried.
 *
 * It cannot be closed from inside the handler, because the handler never runs.
 * It is closed on the other side: make the pages good from user mode, where a
 * fault IS observable, before the range is handed to a kernel call. That is what
 * this does, and every host path that passes guest memory to the kernel as a
 * destination (or as a source) should call it first.
 *
 * for_write touches with an atomic OR of zero rather than a store. It is a real
 * write access -- so a write-protection fault is raised and the page is dirtied
 * -- and it cannot lose a concurrent update, because the value it writes is the
 * value it just read, indivisibly.
 *
 * Returns 1 when every page of the range was reachable afterwards, and 0 when a
 * page faulted and the installed handler declined to repair it: the caller then
 * reports the equivalent of EFAULT instead of issuing the call, which is what
 * the kernel would have done. Returns 1 for an empty range.
 *
 * Two limits, stated rather than hidden. The window between the probe and the
 * call is not atomic -- another thread can unmap in between, and then the call
 * fails as it does today. And a kernel write the engine did not issue (an APC
 * completing into a buffer, say) is not covered by anything here.
 */
int hl_windows_fault_probe(uint64_t address, uint64_t size, int for_write);

#ifdef __cplusplus
}
#endif

#endif
