static int host_addr_mapped(uintptr_t a) {
    return hl_host_address_mapped(a);
}

// per-thread ALTERNATE signal stack for the synchronous-fault guards. On the aarch64 frontend the
// host SP == the guest SP while a translated block runs, so a guest STACK OVERFLOW leaves no room for the
// kernel to push the SIGSEGV/SIGBUS guard's signal frame -- without an altstack the handler double-faults
// and the guest dies of a spurious SIGILL/SIGBUS instead of a clean, guard-delivered SIGSEGV. Installed
// once per thread (main + every guest thread) from run_guest, before any guest code executes, and torn down
// at run_guest exit. (x86 keeps host SP != guest SP, so its guards don't take SA_ONSTACK and never use it;
// the reservation is uncommitted there.)
#define HOST_ALTSTK_SZ (512u << 10)
static _Thread_local hl_host_memory_mapping g_altstk_mapping = {
    HL_HOST_MEMORY_MAPPING_ABI, sizeof(hl_host_memory_mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};

// Idempotent: (re)registers the alternate signal stack for THIS thread, allocating one on first use and
// reusing the existing region otherwise. The sigaltstack() registration is not reliably inherited across
// fork() on Apple Silicon (like the W^X/APRR state -- see fork_child_hooks), so the fork child re-arms via
// this same call with its COW-inherited region.
static void install_host_sigaltstack(void) {
    const hl_host_services *host = effective_host_services();
    hl_host_memory_mapping mapped = {
        HL_HOST_MEMORY_MAPPING_ABI, sizeof(hl_host_memory_mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};
    hl_host_memory_mapping *mapping = &g_altstk_mapping;
    int created = mapping->handle == HL_HOST_HANDLE_INVALID;
    if (created) {
        hl_host_result result;
        if (host == NULL || host->memory == NULL || host->memory->map_anonymous == NULL ||
            host->memory->release == NULL)
            return;
        result =
            host->memory->map_anonymous(host->context, 0, HOST_ALTSTK_SZ, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE,
                                        HL_HOST_MEMORY_PRIVATE, &mapped);
        if (result.status != HL_STATUS_OK || mapped.handle == HL_HOST_HANDLE_INVALID || mapped.address == 0 ||
            mapped.address > UINTPTR_MAX || mapped.mapped_size < HOST_ALTSTK_SZ) {
            if (mapped.handle != HL_HOST_HANDLE_INVALID) (void)host->memory->release(host->context, mapped.handle);
            return;
        }
        mapping = &mapped;
    }
    stack_t ss = {.ss_sp = (void *)(uintptr_t)mapping->address, .ss_size = HOST_ALTSTK_SZ, .ss_flags = 0};
    if (sigaltstack(&ss, NULL) != 0) {
        if (created) (void)host->memory->release(host->context, mapping->handle);
        return;
    }
    if (created) g_altstk_mapping = mapped;
}

static void uninstall_host_sigaltstack(void) {
    const hl_host_services *host = effective_host_services();
    hl_host_result released = {HL_STATUS_NOT_SUPPORTED, 0, 0, 0};
    if (g_altstk_mapping.handle == HL_HOST_HANDLE_INVALID) return;
    stack_t ss = {.ss_flags = SS_DISABLE};
    if (sigaltstack(&ss, NULL) != 0) return;
    if (host == NULL || host->memory == NULL || host->memory->release == NULL) return;
    /* A provider can report a transient teardown failure (including an injected first-call failure).
     * The stack is already disabled, so bounded immediate retries are safe; retain the handle unless a
     * release succeeds rather than silently forgetting provider-owned memory. */
    for (unsigned attempt = 0; attempt < 3 && released.status != HL_STATUS_OK; ++attempt)
        released = host->memory->release(host->context, g_altstk_mapping.handle);
    if (released.status == HL_STATUS_OK)
        g_altstk_mapping = (hl_host_memory_mapping){
            HL_HOST_MEMORY_MAPPING_ABI, sizeof(hl_host_memory_mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};
}

// Range form of host_addr_mapped: true iff every page spanning [a, a+len) is mapped. Used to validate a
// guest-supplied syscall buffer (a result struct to write, an argument struct to read) BEFORE dereferencing
// it, so a bad/garbage user pointer returns -EFAULT to the guest instead of faulting the engine (the
// kernel's access_ok() role). A zero length is vacuously OK; an address-space-wrapping range is rejected.
//
// PERF (sqlite/fcntl): the original implementation issued one mach_vm_region -- a full Mach
// message round-trip (~200ns+) -- PER PAGE PER CALL. `sample` showed ~97% of the engine-side overhead of the
// sqlite syscall mix (2 fcntl(F_SETLK) per query, each validating the guest flock*) inside
// host_range_mapped->mach_vm_region->mach_msg2_trap. Replace it with the kernel's own access_ok() idiom:
// a FAULT-GUARDED PROBE READ of each page under a per-thread sigsetjmp. Mapped pointer (the always case)
// = one L1 load per page, no syscall; unmapped pointer = the SIGSEGV/SIGBUS guard long-jumps back and we
// report 0 exactly as mach_vm_region did. Every fault handler checks
// hrm_fault_hook() FIRST (before non-PIE fixup / the x86 lazy zero-page mapper), so a probe fault can
// never be mis-served as a lazy mapping (which would flip an EFAULT into a bogus success), never burns
// lazy-map budget, and never reaches guest-signal delivery. PROT_NONE pages now probe as UNMAPPED ->
// -EFAULT, which is what a real Linux copy_from_user() returns (the old region query called them mapped
// and the later engine deref crashed) -- strictly closer to the oracle.
#include <setjmp.h>
#if defined(_WIN32)
// The host fault primitive already owns this exact operation, pad and all: it arms its own landing site,
// touches each page (with an atomic OR of zero when the caller asked about writes, so the access really is
// a write and still cannot lose a concurrent update), and reports whether every page was reachable. Its
// probe window is consulted by the vectored handler BEFORE the engine classifier runs, which is exactly the
// ordering the POSIX arm gets from calling hrm_fault_hook first -- a probe fault is the probe's answer and
// nothing else's, so it can never be mis-served as a lazy mapping or reach guest-signal delivery.
//
// Measured, and the reason that ordering is stated rather than assumed: consulting the window after a
// DECLINE instead is not equivalent, because the engine's classifier has no decline for a fault it cannot
// serve -- it terminates the guest. A probe of an unmapped page (an mprotect hole check, a syscall's EFAULT
// check) killed the process before the window was reached.
//
// So there is nothing left for a second implementation to do, and the two thread-locals and the fault hook
// below have no Windows arm at all. What the primitive additionally buys, which no POSIX arm can, is the
// kernel-write hole: a kernel touching an inaccessible user page on the caller's behalf raises no exception
// anywhere, so every host call that hands guest memory to the kernel has to make the pages good from user
// mode first, and that is this same probe.
#include "../../host/windows/fault.h"
#endif
static _Thread_local sigjmp_buf g_hrm_jb;                   // probe return point (valid while g_hrm_hi != 0)
static _Thread_local volatile uintptr_t g_hrm_lo, g_hrm_hi; // page range being probed; probing iff hi != 0

// Called FIRST by every SIGSEGV/SIGBUS handler on the run path: when the fault is this thread's own probe
// load, long-jump back to host_range_mapped ("unmapped"). The faulting signal was auto-blocked at handler
// entry and siglongjmp(.,0) does not restore masks, so unblock it here or the NEXT probe fault would be
// force-killed instead of caught. Returns 0 (fault not ours) in every other case.
static int hrm_fault_hook(siginfo_t *si) {
#if defined(_WIN32)
    // Never claims anything: the probe window lives inside the host fault primitive, and its dispatcher
    // tests it directly. A hook that answered here would be a second, stale copy of that window.
    (void)si;
    return 0;
#else
    if (!g_hrm_hi) return 0;
    uintptr_t va = (uintptr_t)(si ? si->si_addr : NULL);
    if (va < g_hrm_lo || va >= g_hrm_hi) return 0; // not the probe access -> normal fault handling
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, SIGSEGV);
    sigaddset(&s, SIGBUS);
    pthread_sigmask(SIG_UNBLOCK, &s, NULL);
    siglongjmp(g_hrm_jb, 1); // never returns
#endif
}

static int host_range_mapped(uintptr_t a, size_t len) {
    if (!len) return 1;
    /* User mappings live in the lower half of every supported host address
       space.  Reject a non-canonical/top-byte-corrupted guest pointer before
       the guarded load: some kernels canonicalize si_addr for such faults,
       placing it outside the armed probe window and bypassing hrm_fault_hook. */
    if (a > (uintptr_t)INTPTR_MAX || len > (size_t)((uintptr_t)INTPTR_MAX - a)) return 0;
    uintptr_t end = a + len;
    // A guest PROT_NONE mapping is physically R+W under hl (see the g_gna registry above), so the page
    // probe below would call it mapped; the kernel's copy_to/from_user faults it. Reject up front.
    if (gna_hit((uint64_t)a, (uint64_t)len) || hl_linux_bus_hit((uint64_t)a, (uint64_t)len)) return 0;
#if defined(_WIN32)
    // The registry rejections above are still ours -- they encode guest-visible protection this host's
    // page tables do not carry -- and everything below them is the host primitive's job.
    return hl_windows_fault_probe((uint64_t)a, (uint64_t)len, 0);
#else
    uintptr_t lo = a & ~(uintptr_t)0xfff;
    volatile int ok = 1;
    if (sigsetjmp(g_hrm_jb, 0)) {
        ok = 0; // a probe load faulted -> some page in the range is unmapped
    } else {
        g_hrm_lo = lo;
        g_hrm_hi = end;
        for (uintptr_t p = lo; p < end; p += 0x1000) {
            (void)*(volatile const uint8_t *)p;
            /* A file mapping can end in the middle of a guest page.  Darwin's
               VM region query and a probe at the page start both succeed, but
               bytes after EOF raise SIGBUS.  Probe the last covered byte too;
               mapped ranges cannot contain an interior hole, so the pair
               proves the complete page fragment is readable. */
            uintptr_t q = p + 0xfff;
            if (q >= end) q = end - 1;
            if (q != p) (void)*(volatile const uint8_t *)q;
        }
    }
    g_hrm_lo = 0;
    g_hrm_hi = 0; // probe window closed (hook inert again)
    return ok;
#endif
}

/* Prove that every guest-page fragment in a range accepts stores without ever
   storing into guest memory. Guest mappings and their read-only/PROT_NONE/EOF
   intervals are tracked when they are created or protected; the guarded READ
   probe catches unmapped and Darwin file-tail SIGBUS bytes. */
static size_t host_range_writable_prefix(uintptr_t a, size_t len);

static int host_range_writable(uintptr_t a, size_t len) {
    return host_range_writable_prefix(a, len) == len;
}

/* Return the exact writable prefix without performing a store.  Protection
   ledgers locate virtual denials; page-fragment probes locate absent mappings.
   Callers can therefore reject a complete store atomically and report the
   first guest byte that would have faulted. */
static size_t host_range_writable_prefix(uintptr_t a, size_t len) {
    if (!len) return 0;
    if (a > (uintptr_t)INTPTR_MAX || len > (size_t)((uintptr_t)INTPTR_MAX - a)) return 0;
    size_t available = len;
    uint64_t none = gna_prefix((uint64_t)a, (uint64_t)len);
    uint64_t readonly = gro_prefix((uint64_t)a, (uint64_t)len);
    if (none < available) available = (size_t)none;
    if (readonly < available) available = (size_t)readonly;
    uint64_t bus = hl_linux_bus_fault((uint64_t)a, (uint64_t)len);
    if (bus != 0) {
        size_t prefix = bus > (uint64_t)a ? (size_t)(bus - (uint64_t)a) : 0;
        if (prefix < available) available = prefix;
    }
#if defined(_WIN32)
    // A WRITE probe, not the read probe host_range_mapped issues. Two things follow from that on this host
    // and neither is available to the POSIX arm: a page that is mapped but not writable answers correctly
    // without consulting any registry, and the page is left present and dirty -- which is what closes the
    // kernel-write hole for the call this validation precedes, where a kernel store into a not-yet-good
    // page fails with no exception raised anywhere and no handler entered.
    if (available < len) return available;
    return hl_windows_fault_probe((uint64_t)a, (uint64_t)len, 1) ? len : 0;
#else
    size_t checked = 0;
    while (checked < available) {
        uintptr_t address = a + checked;
        size_t fragment = 4096u - (size_t)(address & 4095u);
        if (fragment > available - checked) fragment = available - checked;
        if (!host_range_mapped(address, fragment)) return checked;
        checked += fragment;
    }
    return available;
#endif
}

static void abs_from_rel(struct timespec *abs, const struct timespec *ts) {
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_REALTIME, abs);
    abs->tv_sec += ts->tv_sec;
    abs->tv_nsec += ts->tv_nsec;
    if (abs->tv_nsec >= 1000000000) {
        abs->tv_sec++;
        abs->tv_nsec -= 1000000000;
    }
}

// FUTEX_WAIT_BITSET (op 9) passes an ABSOLUTE deadline, not a relative duration: against
// CLOCK_REALTIME when FUTEX_CLOCK_REALTIME is set (e.g. glibc's pthread_cond_timedwait on a
// CLOCK_REALTIME condvar) and CLOCK_MONOTONIC otherwise. That clock flag is masked off before
// the syscall reaches us, so we recover the intended clock from the deadline itself: only one
// of the two clocks leaves a sane remaining time -- the other is off by the decades-wide gap
// between realtime (~now since 1970) and monotonic (~uptime), yielding a negative or absurdly
// large value. Fills `rel` with the remaining time until the deadline, clamped at zero.
static void futex_rel_from_abs(struct timespec *rel, const struct timespec *deadline) {
    struct timespec rt, mono;
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_REALTIME, &rt);
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &mono);
    int64_t drt = (int64_t)(deadline->tv_sec - rt.tv_sec) * 1000000000 + (deadline->tv_nsec - rt.tv_nsec);
    int64_t dmono = (int64_t)(deadline->tv_sec - mono.tv_sec) * 1000000000 + (deadline->tv_nsec - mono.tv_nsec);
    int64_t ns;
    if (drt < 0)
        ns = dmono; // deadline predates realtime "now" -> it must be a monotonic deadline
    else if (dmono < 0)
        ns = drt;
    else
        ns = drt < dmono ? drt : dmono; // both plausible: the true clock gives the smaller remainder
    if (ns < 0) ns = 0;
    rel->tv_sec = ns / 1000000000;
    rel->tv_nsec = ns % 1000000000;
}

// ---- interruptible waits: let a cross-thread tgkill wake a thread parked in futex_op ----
// A guest FUTEX_WAIT lands the host thread in an (otherwise uninterruptible) pthread_cond_wait. A signal
// aimed at that thread via tkill/tgkill only sets its cpu->tpending; the thread must then round-trip through
// the dispatcher for maybe_deliver_signal to run the guest handler. A thread spinning in translated code
// crosses that boundary continuously (Go's SIGURG stop-the-world preemption relies on exactly that), but a
// PARKED thread sits in cond_wait and never returns -- so the handler never runs. This bit Go's
// runtime.doAllThreadsSyscall (glibc/musl setuid/setgid across all OS threads): the coordinator tgkills each
// sibling M with realtime signal 33 and busy-waits (sched_yield) for its handler to perform the per-thread
// syscall and ack; a parked sibling never woke, so the coordinator spun forever (postgres/mysql/mariadb
// gosu/su-exec privilege-drop hung). Fix: publish the wait primitive so the signaler can wake the parked
// thread, and check for a deliverable thread-directed signal around the wait so it returns -EINTR (the guest
// retries the futex and the dispatcher delivers the handler first), exactly as a real futex is interrupted.

// A thread-directed signal is "actionable" for thread c iff it is pending and NOT blocked by c's mask: the
// dispatcher will then either run its guest handler or apply the default action, so the thread makes
// progress. A blocked pending signal must NOT interrupt a wait (it stays pending, as on Linux) -- otherwise
// the guest would re-wait, see it still pending, and spin returning EINTR forever.
static int thread_pending_test(const struct cpu *cpu, int signal);
static int signal_deliverable(const struct cpu *cpu, int signal);

static int cpu_has_actionable_tsig(const struct cpu *c) {
    uint64_t t = __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    if (!t && !thread_pending_test(c, 64)) return 0;
    for (int s = 1; s <= 64; s++)
        if (thread_pending_test(c, s) && signal_deliverable(c, s)) return 1;
    return 0;
}

// A futex/interruptible wait must abort (return -EINTR so the guest round-trips through the dispatcher) when
// either an actionable thread-directed signal arrives OR this thread has been flagged exited by a peer's
// execve teardown (thread_exit_others) -- in both cases the dispatcher must regain control.
static int cpu_wait_interrupted(const struct cpu *c) {
    return ckpt_pending() || __atomic_load_n(&c->exited, __ATOMIC_SEQ_CST) || cpu_has_actionable_tsig(c);
}

// This thread's slot in g_threg (set on register), so it can publish the primitive it is about to wait on
// without re-scanning the registry on the hot futex path.
static __thread int g_my_threg = -1;
// Publish/clear (defined after g_threg) the mutex+condvar this thread is blocked on. thread_wait_publish is
// called UNDER the wait's own mutex and BEFORE the tpending re-check, so the publish (store) is ordered
// ahead of that check (load): with the signaler's store-tpending-then-load-waitc, this seq_cst StoreLoad
// handshake guarantees at least one side observes the other -- no lost wakeup.
static void thread_wait_publish(pthread_mutex_t *m, pthread_cond_t *cnd);
static void thread_wait_clear(void);

// Task-state stamping (defined later in container/vfs.c, same translation unit). A guest that parks in a
// host blocking wait publishes 'S' (interruptible sleep) into the cross-process task-state table, and 'R'
// on wake, so a peer reading /proc/<pid>/stat (field 3) or /proc/<pid>/status (State:) sees the true state.
// FUTEX_WAIT is a blocking wait exactly like recv/read/epoll_wait (which already bracket their parks), so
// the actual pthread_cond_wait park below must be bracketed too -- otherwise a futex-blocked waiter is
// reported 'R' (running) instead of 'S', hiding blocked threads from monitors and deadlock diagnostics.
static inline void ts_wait_enter(void);
static inline void ts_wait_leave(void);

// Wake up to `n` waiters parked on uaddr's W5C bucket and report the number actually woken (capped at n).
// Factored out of the FUTEX_WAKE path so FUTEX_WAKE_OP wakes through the SAME buckets -- its wakes must
// reach real WAIT-parked waiters (in this process or, across a shared page, a forked peer). Mirrors the
// WAKE block exactly: PROF fast/slow split, lock+broadcast (the lock orders the guest's pre-syscall store
// to *uaddr ahead of an arriving waiter's under-lock value-check, so no wakeup is lost), and a count taken
// from the per-address slot (the number of waiters that will re-check their word and leave).
// `grant_all` selects EVERY matching waiter (returning only the first `n` as the woken count) for the
// REQUEUE family, whose parked peers have no secondary queue to be woken from later, so this
// approximation moves them by waking them all; plain FUTEX_WAKE(n) passes 0 and grants exactly `n`.
static int futex_wake_bucket(const void *key, int n, uint32_t match, int grant_all) {
    struct futex_bucket *b = fbk_of(key);
    if (g_prof) {
        if (atomic_load_explicit(&b->waiters, memory_order_relaxed))
            g_futex_wake_slow++;
        else
            g_futex_wake_fast++;
    }
    pthread_mutex_lock(&b->m);
    // FUTEX_WAKE_BITSET only wakes waiters whose bitset overlaps `match`; a plain FUTEX_WAKE passes ~0u.
    // If no parked waiter on this address can match, wake nobody (Linux does not disturb them).
    if (!fbk_match(b, futex_key(key), match)) {
        pthread_mutex_unlock(&b->m);
        return 0;
    }
    int registered = 0;
    int woke = fbk_wait_grant(b, futex_key(key), n, match, &registered);
    if (grant_all && registered) { // REQUEUE approximation: also release the peers left behind
        int r2 = 0;
        (void)fbk_wait_grant(b, futex_key(key), INT_MAX, match, &r2);
    }
    // PI and overflow fallback waiters do not occupy ordinary-wait slots;
    // their loops re-check ownership/value after a broadcast, so the old
    // bounded parked count remains correct for that exceptional path.
    if (!registered) {
        woke = fbk_parked(b, futex_key(key));
        if (woke > n) woke = n;
    }
    pthread_cond_broadcast(&b->c); // waiters re-check their own word; spurious wakes are legal
    pthread_mutex_unlock(&b->m);
    return woke;
}

// FUTEX_WAKE_OP arithmetic half (Linux-exact): atomically apply the val3-encoded op to *uaddr2 and report,
// via *do_wake2, whether the PRE-mutation value satisfies the encoded comparison (=> uaddr2 waiters are also
// woken by the caller). val3 layout: bits 28-31 op (bit 3 = FUTEX_OP_OPARG_SHIFT), 24-27 cmp, 12-23 oparg,
// 0-11 cmparg. Returns 0, or -EFAULT for an unmapped uaddr2 (kernel's copy semantics), -ENOSYS for an
// unknown op/cmp. Comparisons are signed, exactly as the kernel's futex_atomic_op_inuser.
static int futex_wake_op_apply(int *uaddr2, uint32_t val3, int *do_wake2) {
    unsigned enc = val3;
    int op2 = (enc >> 28) & 0xf;
    int cmp = (enc >> 24) & 0xf;
    int oparg = (enc >> 12) & 0xfff;
    int cmparg = enc & 0xfff;
    if (op2 & 8) { // FUTEX_OP_OPARG_SHIFT: oparg is a shift count (masked to 31, as the kernel does)
        op2 &= 7;
        oparg = 1 << (oparg & 31);
    }
    if (!uaddr2 || !host_addr_mapped((uintptr_t)uaddr2)) return -EFAULT;
    int oldval;
    switch (op2) {
    case 0: oldval = __atomic_exchange_n(uaddr2, oparg, __ATOMIC_SEQ_CST); break; // FUTEX_OP_SET
    case 1: oldval = __atomic_fetch_add(uaddr2, oparg, __ATOMIC_SEQ_CST); break;  // FUTEX_OP_ADD
    case 2: oldval = __atomic_fetch_or(uaddr2, oparg, __ATOMIC_SEQ_CST); break;   // FUTEX_OP_OR
    case 3: oldval = __atomic_fetch_and(uaddr2, ~oparg, __ATOMIC_SEQ_CST); break; // FUTEX_OP_ANDN
    case 4: oldval = __atomic_fetch_xor(uaddr2, oparg, __ATOMIC_SEQ_CST); break;  // FUTEX_OP_XOR
    default: return -ENOSYS;
    }
    int cond;
    switch (cmp) {
    case 0: cond = (oldval == cmparg); break; // FUTEX_OP_CMP_EQ
    case 1: cond = (oldval != cmparg); break; // FUTEX_OP_CMP_NE
    case 2: cond = (oldval < cmparg); break;  // FUTEX_OP_CMP_LT
    case 3: cond = (oldval <= cmparg); break; // FUTEX_OP_CMP_LE
    case 4: cond = (oldval > cmparg); break;  // FUTEX_OP_CMP_GT
    case 5: cond = (oldval >= cmparg); break; // FUTEX_OP_CMP_GE
    default: return -ENOSYS;
    }
    *do_wake2 = cond;
    return 0;
}

// FUTEX PI (priority-inheritance) mutex constants: the futex word holds the owner's guest TID in the low 30
// bits, plus FUTEX_WAITERS (contended -> userspace must trap into the kernel) and FUTEX_OWNER_DIED (robust:
// the owner exited still holding it). hl does not model priority BOOSTING (a latency/QoS property, not a
// correctness one), but it enforces real MUTUAL EXCLUSION and the exact futex-word contract glibc's userspace
// fast paths depend on -- so two threads can never both believe they own a PTHREAD_PRIO_INHERIT/robust mutex
// (the old return-0 fake-acquire silently let them into the critical section together -> data corruption).
#define HL_FUTEX_WAITERS 0x80000000u
#define HL_FUTEX_OWNER_DIED 0x40000000u
#define HL_FUTEX_TID_MASK 0x3fffffffu

static int cpu_tid(const struct cpu *c);

// Acquire the PI mutex at uaddr for this thread (block until free, unless `trylock`). On success writes
// this thread's TID -- OR'd with FUTEX_WAITERS when other threads remain queued on the same word -- into the
// futex word and returns 0, or -EOWNERDEAD when the prior owner died holding it (robust recovery). NEVER
// returns 0 without the word actually naming this thread as the owner. `mono`: the (absolute) timeout is on
// CLOCK_MONOTONIC (FUTEX_LOCK_PI2) rather than the FUTEX_LOCK_PI default of CLOCK_REALTIME. Interruptible
// (a thread-directed signal returns -EINTR, exactly as a real LOCK_PI is interrupted -> glibc retries).
static long futex_lock_pi(struct cpu *c, int *uaddr, const void *key, int trylock, const struct timespec *ts,
                          int mono) {
    if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
    int mytid = cpu_tid(c);
    struct futex_bucket *b = fbk_of(key);
    pthread_mutex_lock(&b->m);
    int parked = 0;
    long ret;
    for (;;) {
        int expect = __atomic_load_n(uaddr, __ATOMIC_SEQ_CST);
        uint32_t v = (uint32_t)expect;
        uint32_t owner = v & HL_FUTEX_TID_MASK;
        int others = fbk_parked(b, futex_key(key)) - (parked ? 1 : 0); // waiters left behind
        // A fresh arrival must not steal from a queued waiter: Linux hands the rt_mutex to the top waiter, so
        // an unlocker that immediately re-locks queues behind it. Stealing starved waiters out to ETIMEDOUT.
        // Only when the parked count is exact -- an overflowed bucket counts foreign addresses, and blocking
        // on a phantom waiter would park us with nobody left to wake us.
        int must_queue = !parked && others > 0 && !b->imprecise;
        if (owner == 0 && !must_queue) { // free (FUTEX_OWNER_DIED may still be set on a robust mutex)
            int nv = (int)((uint32_t)mytid | (v & HL_FUTEX_OWNER_DIED) | (others > 0 ? HL_FUTEX_WAITERS : 0));
            // Acquire atomically vs a racing userspace fast-path locker (cmpxchg 0->tid): if the word moved
            // underfoot, retry from the re-read instead of clobbering the new owner (double-ownership bug).
            if (!__atomic_compare_exchange_n(uaddr, &expect, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) continue;
            /*
             * PI owner death is reported in the futex word, not as a negative
             * FUTEX_LOCK_PI syscall result. glibc consumes OWNER_DIED after a
             * successful syscall and turns it into pthread EOWNERDEAD.
             */
            ret = 0;
            break;
        }
        if (owner == (uint32_t)mytid) {
            ret = -EDEADLK;
            break;
        } // this thread already owns it
        if (trylock) {
            ret = -EAGAIN;
            break;
        } // TRYLOCK_PI: contended -> fail, no block
        // Contended: set FUTEX_WAITERS so glibc's userspace unlock fast path traps to FUTEX_UNLOCK_PI and a
        // userspace lock fast path can't steal ahead of us. Do it as a CMPXCHG under b->m: if the owner just
        // released in userspace (word -> 0) our swap fails and we loop to re-read + acquire -- WITHOUT this
        // the stale store would resurrect a dead owner and every waiter would block forever (the deadlock).
        if (!__atomic_compare_exchange_n(uaddr, &expect, (int)(v | HL_FUTEX_WAITERS), 0, __ATOMIC_SEQ_CST,
                                         __ATOMIC_SEQ_CST))
            continue;
        if (!parked) {
            atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
            fbk_park(b, futex_key(key), ~0u); // PI-mutex waiter: matches any wake bitset
            parked = 1;
        }
        thread_wait_publish(&b->m, &b->c);
        if (cpu_wait_interrupted(c)) {
            thread_wait_clear();
            ret = -EINTR;
            break;
        }
        ts_wait_enter(); // 'S' while parked in FUTEX_LOCK_PI/PI2 (PI-mutex contention)
        int rc = 0;
        if (ts) {
            struct timespec abs, rel;
            if (mono) { // FUTEX_LOCK_PI2: absolute CLOCK_MONOTONIC deadline -> realtime abs for the condvar
                futex_rel_from_abs(&rel, ts);
                abs_from_rel(&abs, &rel);
            } else {
                abs = *ts; // FUTEX_LOCK_PI: already an absolute CLOCK_REALTIME deadline (the condvar's clock)
            }
            rc = pthread_cond_timedwait(&b->c, &b->m, &abs);
        } else {
            pthread_cond_wait(&b->c, &b->m);
        }
        ts_wait_leave();
        thread_wait_clear();
        if (cpu_wait_interrupted(c)) {
            ret = -EINTR;
            break;
        }
        if (rc == ETIMEDOUT) {
            ret = -ETIMEDOUT;
            break;
        }
        // otherwise loop and re-read the word (the releaser cleared the owner; race for ownership under b->m)
    }
    if (parked) {
        fbk_unpark(b, futex_key(key));
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
    }
    pthread_mutex_unlock(&b->m);
    return ret;
}

// Release the PI mutex at uaddr (FUTEX_UNLOCK_PI): only the owner may unlock (-EPERM otherwise). Hand off by
// clearing the owner TID; if waiters remain, keep FUTEX_WAITERS set (word = FUTEX_WAITERS, owner 0) so a
// userspace fast-path locker can't steal in ahead of a parked waiter, and broadcast -- the woken waiters
// re-contend for ownership under the bucket mutex in futex_lock_pi, so exactly one acquires. Returns 0.
static long futex_unlock_pi(struct cpu *c, int *uaddr, const void *key) {
    if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
    int mytid = cpu_tid(c);
    struct futex_bucket *b = fbk_of(key);
    pthread_mutex_lock(&b->m);
    uint32_t v = (uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST);
    if ((v & HL_FUTEX_TID_MASK) != (uint32_t)mytid) {
        pthread_mutex_unlock(&b->m);
        return -EPERM; // not the owner -- Linux rejects an UNLOCK_PI from a non-owner
    }
    int waiters = fbk_parked(b, futex_key(key));
    __atomic_store_n(uaddr, (int)(waiters > 0 ? HL_FUTEX_WAITERS : 0), __ATOMIC_SEQ_CST);
    if (waiters > 0) pthread_cond_broadcast(&b->c);
    pthread_mutex_unlock(&b->m);
    return 0;
}

// nr_wake2 is the raw 4th syscall arg (a3) reinterpreted as a count for FUTEX_WAKE_OP (WAIT ops use a3 as a
// timespec instead -- the two never overlap because op selects one interpretation); uaddr2 (a4) + val3 (a5)
// carry the WAKE_OP / REQUEUE second-address operands, and are ignored by the WAIT/plain-WAKE branches.
static long futex_op(struct cpu *c, int *uaddr, const void *key, int op, int private, int val,
                     const struct timespec *ts, int nr_wake2, int *uaddr2, const void *key2, uint32_t val3) {
    // Linux FUTEX_PRIVATE_FLAG promises that no other process can participate. Keep those high-frequency
    // pthread waits on process-private host mutexes/condvars; macOS process-shared pthread primitives are
    // substantially heavier and eventually fault/livelock under sustained condvar churn. Non-private ops
    // retain the MAP_SHARED table required by forked and independently-mapped shared futexes.
    g_fbk_active = private ? g_fbk_private : g_fbk;
    // FUTEX_WAIT_BITSET(9)/WAKE_BITSET(10) require a non-empty bitset: Linux rejects val3==0 with EINVAL
    // (a zero mask can match no waiter). The old shared WAIT/WAKE path ignored val3 entirely and accepted it.
    if ((op == 9 || op == 10) && val3 == 0) return -EINVAL;
    // PI-mutex ops use per-address ownership tracked in the (always-present) buckets, independent of the
    // legacy single-queue mode below, so dispatch them first. FUTEX_LOCK_PI=6, UNLOCK_PI=7, TRYLOCK_PI=8,
    // WAIT_REQUEUE_PI=11, CMP_REQUEUE_PI=12, LOCK_PI2=13.
    if (op == 6) return futex_lock_pi(c, uaddr, key, 0, ts, 0);
    if (op == 13) return futex_lock_pi(c, uaddr, key, 0, ts, 1);
    if (op == 8) return futex_lock_pi(c, uaddr, key, 1, NULL, 0);
    if (op == 7) return futex_unlock_pi(c, uaddr, key);
    if (op == 11) { // FUTEX_WAIT_REQUEUE_PI: wait on uaddr (while *uaddr==val), then acquire the PI mutex uaddr2.
        // Modern glibc (>=2.25) condvars no longer use requeue_pi, so this path is cold; implement it as a
        // plain WAIT followed by a LOCK_PI on uaddr2 -- semantically what pthread_cond_wait on a PI mutex
        // needs, and always CORRECT (a woken waiter re-acquires uaddr2 itself; see CMP_REQUEUE_PI below).
        if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
        struct futex_bucket *b = fbk_of(key);
        pthread_mutex_lock(&b->m);
        if ((uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != (uint32_t)val) {
            pthread_mutex_unlock(&b->m);
            return -EAGAIN;
        }
        atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
        fbk_park(b, futex_key(key), ~0u); // PI-mutex waiter: matches any wake bitset
        thread_wait_publish(&b->m, &b->c);
        long ret = 0;
        if (cpu_wait_interrupted(c)) {
            ret = -EINTR;
        } else {
            ts_wait_enter(); // 'S' while parked in FUTEX_WAIT_REQUEUE_PI
            int rc = 0;
            if (ts) {
                struct timespec abs, rel;
                futex_rel_from_abs(&rel, ts); // WAIT_REQUEUE_PI's timeout is an absolute deadline
                abs_from_rel(&abs, &rel);
                rc = pthread_cond_timedwait(&b->c, &b->m, &abs);
            } else {
                pthread_cond_wait(&b->c, &b->m);
            }
            ts_wait_leave();
            if (cpu_wait_interrupted(c))
                ret = -EINTR;
            else if (rc == ETIMEDOUT)
                ret = -ETIMEDOUT;
        }
        thread_wait_clear();
        fbk_unpark(b, futex_key(key));
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
        pthread_mutex_unlock(&b->m);
        if (ret < 0) return ret;                         // on error the caller does NOT own uaddr2 (kernel-exact)
        return futex_lock_pi(c, uaddr2, key2, 0, ts, 0); // woken -> acquire the target PI mutex before returning
    }
    if (op == 12) { // FUTEX_CMP_REQUEUE_PI: verify *uaddr==val3, wake uaddr waiters (they self-acquire uaddr2).
        // We don't physically move queues -- each woken WAIT_REQUEUE_PI waiter re-acquires uaddr2's PI lock on
        // its own, so waking them and letting them serialize there is correct (the requeue is only a
        // thundering-herd optimization). Returns the number woken.
        if (uaddr && host_addr_mapped((uintptr_t)uaddr) && (uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != val3)
            return -EAGAIN;
        // Wake up to `val` signalled + `nr_wake2` requeue-budget waiters in one broadcast; each self-acquires
        // uaddr2, so the physical requeue is unnecessary. (A single broadcast wakes all parked in the bucket.)
        long budget = (long)(val < 1 ? 1 : val) + (nr_wake2 > 0 ? nr_wake2 : 0);
        return futex_wake_bucket(key, budget > 0x7fffffff ? 0x7fffffff : (int)budget, ~0u, 1);
    }
    // ---- W5C per-address buckets ----
    struct futex_bucket *b = fbk_of(key);
    // FUTEX_WAIT / WAIT_BITSET: sleep while *uaddr == val
    if (op == 0 || op == 9) {
        if (g_prof) g_futex_wait_n++;
        pthread_mutex_lock(&b->m);
        // Count this bucket's sleepers (PROF only). The value-check below runs under b->m, and so
        // does every WAKE's broadcast, so the lock -- not this counter -- closes the lost-wakeup
        // window: we see the waker's new *uaddr here (and bail) or we cond_wait and it broadcasts.
        atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
        if (__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != val) {
            atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed);
            pthread_mutex_unlock(&b->m);
            return -EAGAIN;
        }
        // Publish, then re-check tpending (the StoreLoad handshake with thread_target_signal) so a
        // thread-directed signal arriving just before we sleep interrupts the wait instead of deadlocking.
        thread_wait_publish(&b->m, &b->c);
        if (cpu_wait_interrupted(c)) {
            thread_wait_clear();
            atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed);
            pthread_mutex_unlock(&b->m);
            return -EINTR;
        }
        // We are now about to actually park: record this waiter against its uaddr so a FUTEX_WAKE (this
        // process or, across a shared page, another) can report the true woken count. Kept in the SHARED
        // bucket, so a cross-fork waker sees it. Carry the wait bitset (op 9 = FUTEX_WAIT_BITSET's val3;
        // plain FUTEX_WAIT matches any) so FUTEX_WAKE_BITSET can skip a non-overlapping wake.
        fbk_park(b, futex_key(key), op == 9 ? val3 : ~0u);
        int wait_slot = fbk_wait_register(b, futex_key(key), op == 9 ? val3 : ~0u);
        ts_wait_enter(); // 'S' (sleeping) while parked in FUTEX_WAIT; peer /proc/<pid>/stat|status must not show 'R'
        int rc = 0;
        struct timespec abs;
        if (ts) {
            struct timespec rel;
            // op 9 (FUTEX_WAIT_BITSET): ts is an absolute deadline; op 0: it is relative.
            if (op == 9) futex_rel_from_abs(&rel, ts);
            abs_from_rel(&abs, op == 9 ? &rel : ts);
        }
        // FUTEX_WAKE(n) selects EXACTLY n waiters by setting their wgrant slot (fbk_wait_grant); the
        // pthread_cond_broadcast is only the transport that makes sleepers runnable. An unselected peer
        // that wakes must therefore re-park rather than return success -- otherwise a WAKE(1) releases
        // every waiter in the bucket. Loop until we are granted, time out, or a signal interrupts us. A
        // waiter that overflowed the exact-selection slots (wait_slot < 0) keeps the legacy re-check-word
        // wake so it can never be stranded.
        int intr = 0;
        for (;;) {
            rc = ts ? pthread_cond_timedwait(&b->c, &b->m, &abs) : pthread_cond_wait(&b->c, &b->m);
            if (cpu_wait_interrupted(c)) {
                intr = 1;
                break;
            }
            if (rc == ETIMEDOUT) break;
            if (wait_slot < 0) break;
            if (b->wgrant[wait_slot]) {
                b->wgrant[wait_slot] = 0;
                break;
            }
        }
        ts_wait_leave();
        thread_wait_clear();
        fbk_wait_unregister(b, wait_slot);
        fbk_unpark(b, futex_key(key));
        // fetch_sub returns the PREVIOUS value; == 1 means the bucket just fully drained -> a stale
        // `imprecise` flag (set by a past slot overflow) can be cleared so exact counting resumes.
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
        pthread_mutex_unlock(&b->m);
        if (intr) return -EINTR; // woken by a cross-thread signal -> guest retries; dispatcher delivers it
        // A pure-timeout wait must report -ETIMEDOUT so the guest stops re-waiting.
        return rc == ETIMEDOUT ? -ETIMEDOUT : 0;
    }
    // FUTEX_WAKE / WAKE_BITSET / REQUEUE / CMP_REQUEUE: wake the waiters on THIS address's bucket.
    // REQUEUE(3)/CMP_REQUEUE(4) ask to wake `val` waiters on uaddr and MOVE the rest onto a second
    // futex (uaddr2) to be woken later by its owner. musl's pthread_cond_broadcast issues exactly this
    // (wake 1, requeue the rest onto the mutex) -- so dropping it (the old "other ops -> return 0")
    // silently lost every broadcast wakeup and any joiner/cond waiter slept forever (node's V8 worker
    // threads never exit -> hang at process shutdown). We don't model the secondary queue; instead we
    // broadcast ALL waiters on uaddr. Waking is always safe -- a spuriously woken waiter re-checks its
    // word and re-waits if needed -- and the requeue target is only an optimization to avoid a
    // thundering herd, so broadcasting is correct, just less efficient under heavy contention.
    if (op == 1 || op == 10 || op == 3 || op == 4) {
        // A real FUTEX_WAKE returns the NUMBER of waiters actually woken (capped at the requested `val`),
        // NOT `val` itself. futex_wake_bucket takes the bucket mutex + broadcasts (the lock orders the
        // guest's pre-syscall store to *uaddr ahead of an arriving waiter's under-lock value-check, so no
        // wakeup is lost -- the old lock-free no-sleeper skip lost wakeups on ARM) and counts the waiters
        // parked on THIS uaddr: each re-checks its word, sees the store, and leaves -- exactly the woken
        // count. (Returning `val` broke LTP tst_checkpoint_wake's `waked += WAKE(INT_MAX)` -> fork04.)
        // FUTEX_WAKE_BITSET (op 10) wakes only waiters whose bitset overlaps val3; the others match any.
        // REQUEUE(3)/CMP_REQUEUE(4) have no modelled secondary queue, so wake every parked peer (grant_all);
        // plain WAKE(1)/WAKE_BITSET(10) must release EXACTLY `val` -- unselected peers re-park (see WAIT loop).
        int woke = futex_wake_bucket(key, val, op == 10 ? val3 : ~0u, op == 3 || op == 4);
        return woke;
    }
    if (op == 5) { // FUTEX_WAKE_OP: atomically mutate *uaddr2, wake uaddr waiters, conditionally uaddr2's.
        // glibc's pthread_cond_signal/broadcast issue this (bump the internal seq/counter at uaddr2 and wake
        // the condvar's futex at uaddr) -- the old "other ops -> return 0" reported success WITHOUT waking,
        // so every glibc condvar signal was silently dropped (the waiting thread remained blocked on the
        // an in-process helper thread's condvar -> an application stall).
        int do_wake2 = 0;
        int rc = futex_wake_op_apply(uaddr2, val3, &do_wake2);
        if (rc < 0) return rc; // -EFAULT (bad uaddr2) / -ENOSYS (unknown op|cmp): report to the guest as-is
        int woke = futex_wake_bucket(key, val, ~0u, 0);
        if (do_wake2) woke += futex_wake_bucket(key2, nr_wake2, ~0u, 0);
        return woke;
    }
    // A genuinely undefined command (the removed FUTEX_FD=2, or any value >= 14 that names no futex op) is
    // -ENOSYS on Linux, not a silent success -- the old fall-through masked capability probes. The PI ops
    // (6-8,11-13) that hl does not model are left as best-effort success above to avoid breaking a PI-mutex
    // fast-path fallback; only the truly-undefined range is rejected here.
    if (op == 2 || op >= 14) return -ENOSYS;
    // Any remaining op is unmodelled -- pretend success (baseline behavior).
    return 0;
}

/* Resolve and hold a guest teardown word while it is accessed. Logical VMAs
 * need a pin because their guest address is not necessarily a host mapping;
 * identity mappings still use the fault-guarded access probe. */
static int futex_teardown_pin(uint64_t address, size_t length, uint32_t protection, hl_logical_vma_pin *pin,
                              void **host) {
    memset(pin, 0, sizeof(*pin));
    int logical = hl_logical_vma_pin_data(address, length, protection, pin);
    if (logical < 0 || pin->contiguous < length) {
        hl_logical_vma_unpin(pin);
        return 0;
    }
    if (logical > 0) {
        *host = pin->host;
        return 1;
    }
    int accessible = (protection & HL_LOGICAL_VMA_WRITE) ? host_range_writable((uintptr_t)address, length)
                                                         : host_range_mapped((uintptr_t)address, length);
    if (!accessible) {
        hl_logical_vma_unpin(pin);
        return 0;
    }
    *host = (void *)(uintptr_t)address;
    return 1;
}

static void futex_wake_addr(uint64_t uaddr) {
    if (!uaddr) return;
    uaddr = nonpie_fold(uaddr); // clear_child_tid is stored in guest coordinates; this is its deref
    // CLONE_CHILD_CLEARTID: zero the word then wake joiners (pthread_join FUTEX_WAITs on this word). A
    // DETACHED guest thread (e.g. musl's __unmapself) munmaps its OWN stack -- which also holds the thread
    // descriptor with this CLEARTID word -- and only THEN issues the exit syscall, so by the time we run
    // here the word can already be unmapped. Linux's clear_child_tid uses put_user() and silently swallows
    // that fault; a raw store here would instead SIGSEGV/SIGBUS the whole process (the flaky rustc-at-exit
    // teardown crash). Skip the store+wake when the address is gone -- a detached thread has no joiner to
    // wake, and a joinable thread never unmaps its own stack so its ctid is always still live.
    hl_logical_vma_pin pin;
    void *host;
    if (!futex_teardown_pin(uaddr, sizeof(int), HL_LOGICAL_VMA_WRITE, &pin, &host)) return;
    __atomic_store_n((int *)host, 0, __ATOMIC_SEQ_CST);
    // libc implementations use both private and shared futex operations for
    // thread joins.  The kernel-generated clear-child-tid wake is not tagged
    // by the exiting guest syscall, so notify both key spaces.  The word is
    // already zero and waiters re-check it, making the second notification a
    // harmless spurious wake while avoiding a permanently parked exact waiter.
    struct futex_bucket *tables[] = {g_fbk_private, g_fbk};
    for (size_t i = 0; i < sizeof tables / sizeof tables[0]; ++i) {
        g_fbk_active = tables[i];
        struct futex_bucket *b = fbk_of(host);
        pthread_mutex_lock(&b->m);
        int registered = 0;
        (void)fbk_wait_grant(b, futex_key(host), INT_MAX, ~0u, &registered);
        pthread_cond_broadcast(&b->c);
        pthread_mutex_unlock(&b->m);
    }
    hl_logical_vma_unpin(&pin);
}

static volatile int g_next_tid = 1000;

// ---------------- live-thread registry (for thread-directed signals: tkill/tgkill) ----------------
// A tgkill()/tkill() names a specific guest tid; to deliver to THAT thread (and only it) we must map the
// tid back to its struct cpu and host pthread. Each thread (init + every spawned one) registers on entry
// to run_guest and unregisters when it leaves. Small fixed table guarded by a mutex; a lookup miss (target
// already gone, or table full) just drops the signal, exactly as Linux drops a tgkill to a dead tid.
#define THREAD_REG_MAX 4096

static struct {
    struct cpu *c;
    pthread_t th;
    // The mutex+condvar this thread is currently parked on in an interruptible futex wait (NULL if none), so
    // thread_target_signal can wake it out of pthread_cond_wait. waitc is the published flag (accessed via
    // __atomic); waitm points at a permanent (bucket / global) mutex, valid whenever waitc != NULL.
    pthread_cond_t *volatile waitc;
    pthread_mutex_t *waitm;
} g_threg[THREAD_REG_MAX];

// O(1) live-thread count, maintained under g_threg_m by thread_register/thread_unregister. Lets
// thread_after_fork() detect a SINGLE-THREADED parent (count <= 1) and skip the phantom-registry rebuild
// and the private-futex table reset -- both exist ONLY to repair state a vanished PEER thread could have
// left inconsistent across the guest fork (a held lock, a phantom tid entry, a stale parked-waiter slot).
// With no peer, none of those can occur: the inherited registry already holds only the calling thread and
// every private-futex bucket lock is unlocked with empty waiter slots, so the reset is pure overhead
// (~130us/fork: 256 bucket mutex+cond re-inits plus a 128KB registry memset). A threaded parent (count > 1)
// still takes the full reset. Correctness gate mirrors jit_after_fork's own single-threaded fast path.
static int g_threg_live;

static pthread_mutex_t g_threg_m = PTHREAD_MUTEX_INITIALIZER;

// fork() only clones the calling thread. Any process-PRIVATE engine mutex a dead peer held at the instant
// the guest forked is inherited LOCKED with no owner to release it, so the single-threaded child deadlocks
// the first time it takes that lock (the go/npm/cargo build hang). Reinitialise this module's
// private locks to a clean unlocked state in the child (the calling thread never holds one across a guest
// syscall, and no peer survives, so this is always safe). Only the PRIVATE futex table is reset; g_fbk lives
// in a PROCESS_SHARED MAP_SHARED page and must retain cross-fork FUTEX_WAKE/WAIT state (glibc process-shared
// semaphores). Called from the fork child path in proc.c.
