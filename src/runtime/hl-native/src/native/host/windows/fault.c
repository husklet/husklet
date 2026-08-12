/*
 * The Windows fault-interception primitive: one process-wide vectored exception
 * handler, an exception -> fault classifier, and the two ways out of a handler
 * (resume in place, or unwind to a pad).
 *
 * The whole file runs on the fault path, so the rules of the callback contract
 * apply to it first: no allocation, no CRT, no lock, no logging, and no call
 * that could resolve a delay-loaded import. Everything it calls is a statically
 * bound kernel32 export or a compiler intrinsic.
 */
#include "fault.h"

#include <stddef.h>

#if !defined(__x86_64__) && !defined(_M_X64)
#error "src/host/windows/fault.c models the x86-64 register file only"
#endif

/*
 * The CONTEXT layout the accessors in fault.h assume, asserted rather than
 * trusted: Rax..R15 then Rip are consecutive DWORD64 members in x86
 * register-encoding order, so a flat "base + register number" index works.
 */
_Static_assert(offsetof(CONTEXT, Rsp) - offsetof(CONTEXT, Rax) == 4 * 8,
               "Win32 x64 CONTEXT Rsp is not at register-encoding index 4");
_Static_assert(offsetof(CONTEXT, R15) - offsetof(CONTEXT, Rax) == 15 * 8,
               "Win32 x64 CONTEXT GPRs are not a contiguous encoding-ordered block");
_Static_assert(offsetof(CONTEXT, Rip) - offsetof(CONTEXT, Rax) == 16 * 8, "Win32 x64 CONTEXT Rip does not follow R15");
_Static_assert(sizeof(((CONTEXT *)0)->FltSave.XmmRegisters) == 16 * 16, "Win32 x64 CONTEXT xmm area is not 16 x 128b");
_Static_assert(sizeof(hl_windows_fault_pad) == 80, "the fault pad is not the ten words the arm macro writes");

#define HL_WINDOWS_FAULT_PAGE UINT64_C(4096)

/* Exception codes mingw's headers do not name. */
#define HL_WINDOWS_EXCEPTION_CPP_THROW 0xE06D7363u

/* ExceptionInformation[0] on an access violation / in-page error. */
enum { HL_WINDOWS_AV_READ = 0, HL_WINDOWS_AV_WRITE = 1, HL_WINDOWS_AV_EXECUTE = 8 };

/* --- installed state ---------------------------------------------------------
 * Read on the fault path, written only at engine init and shutdown. The handler
 * pointer is the publication gate: the context is stored first and the handler
 * last, and the VEH is not registered until both are in place, so a dispatch
 * that sees a handler necessarily sees its context. */
static hl_windows_fault_handler g_handler;
static void *g_handler_context;
static PVOID g_registration;
static uint64_t g_seen;
static uint64_t g_resumed;

/* --- the probe's per-thread landing pad --------------------------------------
 * Thread-local because the pad names one thread's stack frame. This compiles to
 * a native TLS access -- a TEB load and two indexed loads, no call and no
 * allocation -- which is what makes reading it from inside the handler legal.
 * The per-thread block itself is materialised by the loader at thread start, not
 * on first touch, so even a thread that never probes can be inspected safely. */
typedef struct hl_windows_probe_state {
    hl_windows_fault_pad pad;
    uintptr_t low;
    uintptr_t high;
    uint32_t active;
    uint32_t failed;
} hl_windows_probe_state;

static _Thread_local hl_windows_probe_state g_probe;

/* --- classification ---------------------------------------------------------- */

/*
 * MAPERR vs ACCERR is the one thing POSIX reports that Windows does not: nothing
 * is unmapped is reported as unmapped, both arrive as EXCEPTION_ACCESS_VIOLATION
 * with identical information words. VirtualQuery recovers it -- MEM_FREE means
 * there is no mapping at that address -- at the cost of a kernel call on the
 * fault path. That call is legal here (the address-space lock lives in the
 * kernel and a user-mode thread cannot already hold it) and it is the same trade
 * a Mach-based host makes.
 */
static uint32_t hl_windows_fault_segv_code(uint64_t address) {
    MEMORY_BASIC_INFORMATION info;
    void *page = (void *)(uintptr_t)address;
    if (address > UINTPTR_MAX) return HL_WINDOWS_FAULT_CODE_MAPERR;
    if (VirtualQuery(page, &info, sizeof(info)) != sizeof(info)) return HL_WINDOWS_FAULT_CODE_MAPERR;
    if (info.State == MEM_FREE) return HL_WINDOWS_FAULT_CODE_MAPERR;
    /* Reserved-but-uncommitted is address space with no page behind it, which is
     * what an unmapped hole means to a guest even though NT calls it claimed. */
    if (info.State == MEM_RESERVE) return HL_WINDOWS_FAULT_CODE_MAPERR;
    return HL_WINDOWS_FAULT_CODE_ACCERR;
}

static uint32_t hl_windows_fault_access_from(ULONG_PTR word) {
    if (word == HL_WINDOWS_AV_WRITE) return HL_WINDOWS_FAULT_ACCESS_WRITE;
    if (word == HL_WINDOWS_AV_EXECUTE) return HL_WINDOWS_FAULT_ACCESS_EXECUTE;
    return HL_WINDOWS_FAULT_ACCESS_READ;
}

/* Fill `out` from the record. kind stays HL_WINDOWS_FAULT_NONE for anything the
 * engine does not model, and the caller declines on that without having touched
 * the context. */
static void hl_windows_fault_classify(const EXCEPTION_RECORD *record, hl_windows_fault *out) {
    const DWORD code = record->ExceptionCode;
    out->exception_code = (uint32_t)code;
    if ((record->ExceptionFlags & EXCEPTION_NONCONTINUABLE) != 0) out->flags |= HL_WINDOWS_FAULT_NONCONTINUABLE;
    switch (code) {
    case EXCEPTION_ACCESS_VIOLATION:
        if (record->NumberParameters < 2) return;
        out->kind = HL_WINDOWS_FAULT_SEGV;
        out->access = hl_windows_fault_access_from(record->ExceptionInformation[0]);
        out->address = (uint64_t)record->ExceptionInformation[1];
        out->flags |= HL_WINDOWS_FAULT_HAS_ADDRESS;
        out->code = hl_windows_fault_segv_code(out->address);
        return;
    case EXCEPTION_IN_PAGE_ERROR:
        /* A genuine backing-store I/O failure on a mapped view, and only that.
         * This host cannot fault past the end of a file at all -- a view cannot
         * exceed its section and a section cannot exceed the file it was made
         * over -- so past-EOF SIGBUS never arrives here and must come from the
         * engine's own ledger instead. */
        if (record->NumberParameters < 2) return;
        out->kind = HL_WINDOWS_FAULT_BUS;
        out->code = HL_WINDOWS_FAULT_CODE_ADRERR;
        out->access = hl_windows_fault_access_from(record->ExceptionInformation[0]);
        out->address = (uint64_t)record->ExceptionInformation[1];
        out->flags |= HL_WINDOWS_FAULT_HAS_ADDRESS;
        if (record->NumberParameters >= 3) out->nt_status = (uint64_t)record->ExceptionInformation[2];
        return;
    case EXCEPTION_DATATYPE_MISALIGNMENT:
        /* No information words: the address is not reported for this one. */
        out->kind = HL_WINDOWS_FAULT_BUS;
        out->code = HL_WINDOWS_FAULT_CODE_ADRALN;
        return;
    case EXCEPTION_ILLEGAL_INSTRUCTION:
        out->kind = HL_WINDOWS_FAULT_ILL;
        out->code = HL_WINDOWS_FAULT_CODE_ILLOPC;
        return;
    case EXCEPTION_PRIV_INSTRUCTION:
        out->kind = HL_WINDOWS_FAULT_ILL;
        out->code = HL_WINDOWS_FAULT_CODE_PRVOPC;
        return;
    case EXCEPTION_INT_DIVIDE_BY_ZERO:
        out->kind = HL_WINDOWS_FAULT_FPE;
        out->code = HL_WINDOWS_FAULT_CODE_INTDIV;
        return;
    case EXCEPTION_INT_OVERFLOW:
        out->kind = HL_WINDOWS_FAULT_FPE;
        out->code = HL_WINDOWS_FAULT_CODE_INTOVF;
        return;
    /* The float classes are only reachable with FP exceptions unmasked, which
     * the engine does not do; they are classified anyway so that an embedder
     * that unmasks them does not fall off the end into a decline. */
    case EXCEPTION_FLT_DIVIDE_BY_ZERO:
        out->kind = HL_WINDOWS_FAULT_FPE;
        out->code = HL_WINDOWS_FAULT_CODE_FLTDIV;
        return;
    case EXCEPTION_FLT_OVERFLOW:
        out->kind = HL_WINDOWS_FAULT_FPE;
        out->code = HL_WINDOWS_FAULT_CODE_FLTOVF;
        return;
    case EXCEPTION_FLT_UNDERFLOW:
        out->kind = HL_WINDOWS_FAULT_FPE;
        out->code = HL_WINDOWS_FAULT_CODE_FLTUND;
        return;
    case EXCEPTION_FLT_INEXACT_RESULT:
        out->kind = HL_WINDOWS_FAULT_FPE;
        out->code = HL_WINDOWS_FAULT_CODE_FLTRES;
        return;
    case EXCEPTION_FLT_INVALID_OPERATION:
    case EXCEPTION_FLT_DENORMAL_OPERAND:
    case EXCEPTION_FLT_STACK_CHECK:
        out->kind = HL_WINDOWS_FAULT_FPE;
        out->code = HL_WINDOWS_FAULT_CODE_FLTINV;
        return;
    case EXCEPTION_BREAKPOINT:
        out->kind = HL_WINDOWS_FAULT_TRAP;
        out->code = HL_WINDOWS_FAULT_CODE_BRKPT;
        return;
    case EXCEPTION_SINGLE_STEP:
        out->kind = HL_WINDOWS_FAULT_TRAP;
        out->code = HL_WINDOWS_FAULT_CODE_TRACE;
        return;
    default: return;
    }
}

/* --- the handler ------------------------------------------------------------- */

/*
 * Everything the engine does not own leaves here before anything is touched.
 *
 *   - informational severity (top nibble 4) is every debugger and tooling
 *     notification there is: OutputDebugString, the thread-naming exception, the
 *     debugger's Ctrl-C. Classifying those would turn a debugger session into a
 *     fault path.
 *   - a C++ throw is a control-flow mechanism belonging to whichever library
 *     raised it.
 *   - a stack overflow is the ENGINE's own stack, never a guest fault, and it
 *     must leave before the classifiers see it. There is no sigaltstack analogue
 *     on this host, the guard page has already been consumed by the time a
 *     handler runs, and the honest behaviour for an engine bug is to die with a
 *     diagnostic. Letting it reach a lazy-page grower would turn an engine bug
 *     into silent memory corruption.
 *
 * Not filterable, and stated because it is invisible otherwise: if SP points
 * into inaccessible memory when a fault occurs, the kernel cannot push the
 * exception frame and kills the thread with no handler invoked at all. The rule
 * is "writable memory below SP", which is broader than stack overflow.
 */
static int hl_windows_fault_is_foreign(DWORD code) {
    if ((code & 0xF0000000u) == 0x40000000u) return 1;
    if (code == HL_WINDOWS_EXCEPTION_CPP_THROW) return 1;
    if (code == (DWORD)EXCEPTION_STACK_OVERFLOW) return 1;
    return 0;
}

static LONG CALLBACK hl_windows_veh(EXCEPTION_POINTERS *pointers) {
    hl_windows_fault fault;
    hl_windows_fault_handler handler;
    EXCEPTION_RECORD *record;
    CONTEXT *context;
    DWORD saved_error;
    int verdict;

    if (pointers == NULL) return EXCEPTION_CONTINUE_SEARCH;
    record = pointers->ExceptionRecord;
    context = pointers->ContextRecord;
    if (record == NULL || context == NULL) return EXCEPTION_CONTINUE_SEARCH;
    if (hl_windows_fault_is_foreign(record->ExceptionCode)) return EXCEPTION_CONTINUE_SEARCH;

    /* The thread's last error is a TEB field the faulting instruction stream may
     * be about to read, and every call below overwrites it. POSIX handlers owe
     * the same debt for errno and mostly do not pay it; here it is paid. */
    saved_error = GetLastError();

    /* ContextFlags gates what NtContinue actually restores. Measured on this
     * host at first-chance dispatch it is 0x0010005f -- every flag including
     * XSTATE -- but the value is not contractual and the failure it would cause
     * is silent: a fault path that reads and rewrites vector state would resume
     * a guest with garbage registers. One test, on a path that already costs
     * microseconds. */
    if ((context->ContextFlags & CONTEXT_CONTROL) != CONTEXT_CONTROL ||
        (context->ContextFlags & CONTEXT_INTEGER) != CONTEXT_INTEGER ||
        (context->ContextFlags & CONTEXT_FLOATING_POINT) != CONTEXT_FLOATING_POINT) {
        SetLastError(saved_error);
        return EXCEPTION_CONTINUE_SEARCH;
    }

    (void)__atomic_fetch_add(&g_seen, 1u, __ATOMIC_RELAXED);

    fault.kind = HL_WINDOWS_FAULT_NONE;
    fault.code = HL_WINDOWS_FAULT_CODE_NONE;
    fault.access = HL_WINDOWS_FAULT_ACCESS_READ;
    fault.flags = 0;
    fault.address = 0;
    fault.nt_status = 0;
    fault.exception_code = 0;
    fault.reserved = 0;
    fault.context = context;
    fault.record = record;
    hl_windows_fault_classify(record, &fault);
    if (fault.kind == HL_WINDOWS_FAULT_NONE) {
        SetLastError(saved_error);
        return EXCEPTION_CONTINUE_SEARCH;
    }

    /* A deliberate probe of the faulting address is answered BEFORE the engine
     * classifier runs, never after it.
     *
     * The ordering is the whole point and it is the POSIX arm's: every signal
     * handler on the run path calls the probe hook first, ahead of the non-PIE
     * fixup and the lazy zero-page grower, so that a probe fault can never be
     * mis-served as a lazy mapping -- which would turn a guest EFAULT into a
     * bogus success -- and never reaches guest-signal delivery. Testing the
     * window after a decline instead only looks equivalent: the engine's
     * classifier does not HAVE a decline for a fault it cannot serve. It
     * terminates the guest or re-raises, so a probe of an unmapped address --
     * exactly what an mprotect ENOMEM check or a syscall's EFAULT check issues
     * -- killed the process before the window was ever consulted.
     *
     * Only this thread's own window can claim a fault, and only for an address
     * inside it, so a genuine guest fault at an unrelated address is untouched. */
    if (g_probe.active != 0 && (fault.flags & HL_WINDOWS_FAULT_HAS_ADDRESS) != 0 &&
        (fault.flags & HL_WINDOWS_FAULT_NONCONTINUABLE) == 0 &&
        (fault.kind == HL_WINDOWS_FAULT_SEGV || fault.kind == HL_WINDOWS_FAULT_BUS) &&
        fault.address >= (uint64_t)g_probe.low && fault.address < (uint64_t)g_probe.high) {
        g_probe.active = 0;
        g_probe.failed = 1;
        hl_windows_fault_pad_resume(&fault, &g_probe.pad);
        (void)__atomic_fetch_add(&g_resumed, 1u, __ATOMIC_RELAXED);
        SetLastError(saved_error);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    handler = __atomic_load_n(&g_handler, __ATOMIC_ACQUIRE);
    verdict = HL_WINDOWS_FAULT_DECLINE;
    if (handler != NULL) verdict = handler(&fault, g_handler_context);

    /* Continuing a noncontinuable exception raises
     * EXCEPTION_NONCONTINUABLE_EXCEPTION, so the refusal is enforced here rather
     * than trusted to every handler. */
    if (verdict == HL_WINDOWS_FAULT_RESUME && (fault.flags & HL_WINDOWS_FAULT_NONCONTINUABLE) == 0) {
        (void)__atomic_fetch_add(&g_resumed, 1u, __ATOMIC_RELAXED);
        SetLastError(saved_error);
        return EXCEPTION_CONTINUE_EXECUTION;
    }

    SetLastError(saved_error);
    return EXCEPTION_CONTINUE_SEARCH;
}

/* --- install / remove --------------------------------------------------------- */

int hl_windows_fault_install(hl_windows_fault_handler handler, void *context) {
    hl_windows_fault_handler expected = NULL;
    if (handler == NULL) return 0;
    /* One owner. A second install loses rather than silently replacing the
     * first, because a replaced classifier is a fault path nobody can find --
     * and it must lose without side effects, so the claim is taken before the
     * context is stored rather than after. */
    if (!__atomic_compare_exchange_n(&g_handler, &expected, handler, 0, __ATOMIC_RELEASE, __ATOMIC_RELAXED)) return 0;
    g_handler_context = context;
    /* First != 0 puts this ahead of any vectored handler a DLL loaded later
     * adds. Registration is what publishes the pair to other threads: no
     * dispatch can reach hl_windows_veh before this call returns. */
    g_registration = AddVectoredExceptionHandler(1u, hl_windows_veh);
    if (g_registration == NULL) {
        __atomic_store_n(&g_handler, (hl_windows_fault_handler)NULL, __ATOMIC_RELEASE);
        g_handler_context = NULL;
        return 0;
    }
    return 1;
}

int hl_windows_fault_remove(void) {
    PVOID registration = g_registration;
    if (registration == NULL) return 0;
    /* Unregister first, so no further dispatch can observe a cleared handler. */
    (void)RemoveVectoredExceptionHandler(registration);
    g_registration = NULL;
    __atomic_store_n(&g_handler, (hl_windows_fault_handler)NULL, __ATOMIC_RELEASE);
    g_handler_context = NULL;
    return 1;
}

void hl_windows_fault_counters(uint64_t *out_seen, uint64_t *out_resumed) {
    if (out_seen != NULL) *out_seen = __atomic_load_n(&g_seen, __ATOMIC_RELAXED);
    if (out_resumed != NULL) *out_resumed = __atomic_load_n(&g_resumed, __ATOMIC_RELAXED);
}

/* --- the user-mode probe ------------------------------------------------------ */

int hl_windows_fault_probe(uint64_t address, uint64_t size, int for_write) {
    uintptr_t low;
    uintptr_t high;
    uintptr_t page;
    if (size == 0) return 1;
    if (address == 0 || address > UINTPTR_MAX || size > (uint64_t)UINTPTR_MAX - address) return 0;
    low = (uintptr_t)address;
    high = low + (uintptr_t)size;

    g_probe.low = low;
    g_probe.high = high;
    g_probe.failed = 0;
    g_probe.active = 1;
    /* The arm has to precede the first touch and cannot be hoisted out of it:
     * everything after this point may be re-entered from the handler with the
     * volatile register file destroyed, which the macro's clobber list is what
     * makes safe. */
    HL_WINDOWS_FAULT_PAD_ARM(&g_probe.pad);
    if (g_probe.failed == 0) {
        for (page = low & ~(uintptr_t)(HL_WINDOWS_FAULT_PAGE - 1u); page < high; page += HL_WINDOWS_FAULT_PAGE) {
            /* Clamped so the touched byte is always inside the requested range;
             * it is in the same page either way, so the fault behaviour is
             * identical and the handler's range test stays exact. */
            volatile unsigned char *byte = (volatile unsigned char *)(page < low ? low : page);
            if (for_write) {
                /* A real write access -- it raises a write-protection fault and
                 * dirties the page -- that cannot lose a concurrent update,
                 * because the value written is the value read, indivisibly. */
                (void)__atomic_fetch_or(byte, 0, __ATOMIC_RELAXED);
            } else {
                (void)*byte;
            }
        }
    }
    g_probe.active = 0;
    return g_probe.failed == 0;
}
