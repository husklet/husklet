// Extracted from service(): Event loop -- epoll/eventfd2/timerfd/signalfd4/inotify, emulated on macOS
// kqueue/pipes. Returns 1 if nr was handled, 0 otherwise. Included by service.c after service/net.c,
// before service() -- same TU scope (shares io.c/signal.c fd-redirection state).

// struct epoll_event has a DIFFERENT layout per guest arch: x86-64 forces __attribute__((packed)) so it
// is 12 bytes with `data` at offset 4; every other arch (aarch64/asm-generic) leaves it naturally aligned
// at 16 bytes (4 bytes pad after the u32 events, then `data` at offset 8). Derive both from the same
// G_O_DIRECTORY discriminator io.c uses, so epoll_ctl reads `data` and epoll_pwait writes the out-array at
// the stride/offset the guest's libc/runtime expects (Go's netpoller stores a pointer in `data`).
#if G_O_DIRECTORY == 0x10000
#define G_EPEV_SZ 12u // x86-64 (packed)
#define G_EPEV_DOFF 4u
#else
#define G_EPEV_SZ 16u // aarch64 / asm-generic
#define G_EPEV_DOFF 8u
#endif

// Edge-triggered "prime" on registration. Linux reports an fd that is ALREADY readable/writable at
// EPOLL_CTL_ADD/MOD time when it is registered EPOLLET -- the registration itself counts as the edge (this
// is how Go's netpoller learns about an accepted connection whose request bytes are already buffered). A
// macOS kqueue EV_CLEAR filter, by contrast, reports only a *subsequent* transition, so an already-ready fd
// is never delivered and a Go HTTP server accepts the connection but never responds. So when we arm an edge
// filter on a fd that currently polls ready, stash a synthetic readiness event here and deliver it on the
// next epoll_wait -- once (edge semantics). Tables are indexed by epoll fd (<DD_NFD); larger fds use the
// immediate path and simply don't get primed. Level-triggered fds need no prime (kqueue without EV_CLEAR
// already reports current readiness), so only EPOLLET arms reach here -- level semantics are untouched.
static struct kevent *g_ep_prime[DD_NFD];
static int g_ep_primen[DD_NFD], g_ep_primecap[DD_NFD];

static void ep_prime_push(int ep, uintptr_t ident, int16_t filt, void *udata) {
    if (ep < 0 || ep >= DD_NFD) return;
    struct kevent *a = g_ep_prime[ep];
    for (int i = 0; i < g_ep_primen[ep]; i++)
        if (a[i].ident == ident && a[i].filter == filt) {
            a[i].udata = udata;
            return;
        }
    if (g_ep_primen[ep] >= g_ep_primecap[ep]) {
        int nc = g_ep_primecap[ep] ? g_ep_primecap[ep] * 2 : 8;
        struct kevent *na = realloc(a, (size_t)nc * sizeof *na);
        if (!na) return;
        g_ep_prime[ep] = na;
        g_ep_primecap[ep] = nc;
        a = na;
    }
    EV_SET(&a[g_ep_primen[ep]++], ident, filt, 0, 0, 0, udata);
}

// If `fd` currently polls ready for the direction `filt` covers, record a one-shot prime on `ep`.
static void ep_prime_if_ready(int ep, int fd, int16_t filt, void *udata) {
    if (ep < 0 || ep >= DD_NFD || fd < 0) return;
    short want = (filt == EVFILT_READ) ? POLLIN : POLLOUT;
    struct pollfd pfd = {.fd = fd, .events = want, .revents = 0};
    if (poll(&pfd, 1, 0) > 0 && (pfd.revents & (want | POLLHUP | POLLERR)))
        ep_prime_push(ep, (uintptr_t)fd, filt, udata);
}

// --- cross-thread readiness wakeup (EVFILT_USER) --------------------------------------------------
// A Go netpoller (and node's worker-thread pool) shares ONE epoll instance across several OS threads
// (Go Ms): one M blocks in epoll_wait while ANOTHER M accepts a connection and registers it on the same
// instance (epoll_ctl). That connection usually already has its request bytes buffered, so on Linux the
// EPOLLET registration edge wakes the blocked epoll_wait at once. Two things defeat that emulation here:
// (1) the W3E fast path DEFERS the kevent registration to the next epoll_wait on the SAME thread, so a
// peer M already blocked in kevent() never sees it; (2) an already-ready fd armed EV_CLEAR produces no
// kqueue edge, so its readiness is stashed in g_ep_prime and only consulted when THIS thread next waits.
// Either way the readiness is stranded on the registering thread and the connection is accepted but never
// serviced. Fix: give every epoll kqueue an EVFILT_USER "wake" knote; when a thread registers interest
// while the process is multi-threaded, flush the pending changelist to the kernel (so the fd is visible
// to a blocked peer) and NOTE_TRIGGER the knote (so the peer returns from kevent and re-scans primes).
// A single mutex serializes the W3E per-instance state (changelist/prime/armed maps) whenever guest
// threads exist; the single-threaded path is untouched (g_threaded == 0 -> no lock, no wake, no change).
#define EP_WAKE_IDENT ((uintptr_t)0x7fffffe0u) // EVFILT_USER ident, disjoint from any real fd number
static uint8_t g_ep_wake_armed[DD_NFD];          // per epoll fd: EVFILT_USER wake knote installed on its kqueue
static pthread_mutex_t g_ep_mtx = PTHREAD_MUTEX_INITIALIZER;
// Wall 7 trace focus: sticky per-epoll flag set the first time an AF_UNIX/eventfd (a Mojo channel/wake
// fd) is registered on this instance, so epoll_wait tracing is confined to the renderer's Mojo IO-pump
// epoll and not the timer/signal epolls. Touched only under DD_WALL7_TRACE.
static uint8_t g_ep_has_mojo[DD_NFD];

// per-epoll-instance registered-fd membership (lazily allocated DD_NFD-bit bitmap indexed by the
// watched fd -- the bitmap must span the SAME index range as the fd < DD_NFD guard on ep_mem_test/
// ep_mem_set and the sibling [DD_NFD] interest tables; a Chromium renderer registers hundreds of fds,
// so the watched-fd number routinely exceeds 1024 and any narrower bitmap would be indexed out of
// bounds -- a heap overflow whose corrupted/garbage membership bit spuriously returns EEXIST and drops
// the real registration, stranding that fd's readiness (the load-dependent renderer node-connect stall).
// watched fd). kqueue silently accepts an EV_ADD of an already-armed filter and an EV_DELETE of an
// absent one, but Linux epoll_ctl returns EEXIST / ENOENT respectively, so track membership to serve
// those (plus EINVAL for adding the epoll fd to itself and EPERM for a regular file / directory). Only
// dd-tracked epoll fds (< DD_NFD, g_epoll set) get this surface -- a dup'd/large epfd keeps the existing
// best-effort immediate path, so correct software's readiness path is byte-unchanged.
// A guest that shares ONE epoll instance across threads (Go's netpoller, node's worker pool) issues
// concurrent epoll_ctl from different threads, so the membership bitmap is touched cross-thread: the byte
// RMW and the lazy alloc are therefore ATOMIC. A plain `byte |= bit` / `byte &= ~bit` is a read-modify-write
// that loses a concurrent update to a DIFFERENT bit in the SAME byte (fds 8k..8k+7 share one byte) -- e.g. a
// waiter's DEL(fd X) clear racing a peer's ADD(fd Z) set would resurrect X's stale membership bit, so when
// fd X's number is later reused a fresh EPOLL_CTL_ADD wrongly returns EEXIST (Linux never does: its
// epoll_ctl is internally serialized and close() auto-removes). Atomic OR/AND on the byte + a CAS-installed
// bitmap close that race without a lock (the single-threaded path is unchanged: uncontended atomics).
static uint8_t *g_ep_member[DD_NFD];

static int ep_mem_test(int ep, int fd) {
    if (ep < 0 || ep >= DD_NFD || fd < 0 || fd >= DD_NFD) return 0;
    uint8_t *m = __atomic_load_n(&g_ep_member[ep], __ATOMIC_ACQUIRE);
    if (!m) return 0;
    return (__atomic_load_n(&m[fd >> 3], __ATOMIC_SEQ_CST) >> (fd & 7)) & 1;
}

static void ep_mem_set(int ep, int fd, int on) {
    if (ep < 0 || ep >= DD_NFD || fd < 0 || fd >= DD_NFD) return;
    uint8_t *m = __atomic_load_n(&g_ep_member[ep], __ATOMIC_ACQUIRE);
    if (!m) {
        if (!on) return;
        uint8_t *nm = calloc(DD_NFD / 8, 1);
        if (!nm) return;
        uint8_t *expect = NULL;
        // publish atomically; if a peer installed one first, adopt theirs and free ours (the bit RMW below
        // then lands on the single winning array, so no membership bit is stranded on a discarded buffer).
        if (__atomic_compare_exchange_n(&g_ep_member[ep], &expect, nm, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE))
            m = nm;
        else {
            free(nm);
            m = expect;
        }
    }
    uint8_t bit = (uint8_t)(1u << (fd & 7));
    if (on)
        __atomic_fetch_or(&m[fd >> 3], bit, __ATOMIC_SEQ_CST);
    else
        __atomic_fetch_and(&m[fd >> 3], (uint8_t)~bit, __ATOMIC_SEQ_CST);
}

static void ep_mem_clear(int ep) {
    if (ep < 0 || ep >= DD_NFD) return;
    if (g_ep_member[ep]) {
        free(g_ep_member[ep]);
        g_ep_member[ep] = NULL;
    }
}

// Re-arm one watched fd's kqueue knotes on epoll instance `ep`, from its recorded interest (events+udata).
// Shared by the fork-child rebuild and the close-with-surviving-dup re-home. `ident` is the fd whose knote
// is (re)armed on the kqueue; `slot` is the interest-table fd the events/udata come from (they differ only
// when re-homing a closed fd onto a surviving alias). Returns the armed read/write direction bits.
static void ep_rearm_from_interest(int ep, int ident, int slot) {
    uint32_t ev = g_ep_events[slot];
    uint16_t xf = (uint16_t)((ev & 0x80000000u ? EV_CLEAR : 0) | (ev & 0x40000000u ? EV_ONESHOT : 0));
    void *ud = (void *)g_ep_udata[slot];
    struct kevent kv[2];
    int n = 0;
    if (ev & 0x1) { // EPOLLIN
        EV_SET(&kv[n++], ident, EVFILT_READ, EV_ADD | xf, 0, 0, ud);
    }
    if (ev & 0x4) { // EPOLLOUT
        EV_SET(&kv[n++], ident, EVFILT_WRITE, EV_ADD | xf, 0, 0, ud);
    }
    for (int i = 0; i < n; i++) {
        kevent(ep, &kv[i], 1, NULL, 0, NULL);
        ep_count();
    }
}

// A watched fd is being closed. If a dup keeps its OPEN FILE DESCRIPTION alive, Linux keeps the epoll
// registration (readiness must persist), but the macOS kqueue knote dies with the fd NUMBER. Re-home the
// registration onto a surviving alias of the same OFD so readiness continues to be reported with the same
// udata. Called from fd_reset_emul BEFORE the interest table + ofd id are cleared, and before the real
// close(). No-op unless the closing fd is both watched (g_ep_owner) and has a dup alias (g_ofd_id).
static void ep_close_rehome(int fd) {
    if (fd < 0 || fd >= DD_NFD || !g_ep_owner[fd] || !g_ofd_id[fd]) return;
    int ep = g_ep_owner[fd] - 1;
    if (ep < 0 || ep >= DD_NFD || !g_epoll[ep] || fcntl(ep, F_GETFD) == -1) return; // epoll instance gone
    int y = ofd_surviving_alias(fd);
    if (y < 0 || y >= DD_NFD || y == fd) return; // last OFD reference is closing -> let the knote die
    if (g_ep_owner[y]) return;                   // the alias is already a watched fd of its own -> don't clobber
    ep_rearm_from_interest(ep, y, fd);           // arm the surviving alias with the closing fd's events+udata
    g_ep_owner[y] = ep + 1;
    g_ep_events[y] = g_ep_events[fd];
    g_ep_udata[y] = g_ep_udata[fd];
    g_ep_rd[y] = g_ep_rd[fd];
    g_ep_wr[y] = g_ep_wr[fd];
    g_ep_os[y] = g_ep_os[fd];
    ep_mem_set(ep, y, 1);
    ep_mem_set(ep, fd, 0);
}

// Capture g_threaded into the returned token so lock/unlock stay balanced even if a peer thread flips
// g_threaded (0->1 on its first clone) between the two calls. Single-threaded (token 0) takes no lock.
static inline int ep_lock(void) {
    int lk = g_threaded;
    if (lk) pthread_mutex_lock(&g_ep_mtx);
    return lk;
}

static inline void ep_unlock(int lk) {
    if (lk) pthread_mutex_unlock(&g_ep_mtx);
}

// Install the one-shot self-wake knote on `ep`'s kqueue (idempotent). EV_CLEAR: a NOTE_TRIGGER is
// auto-consumed on delivery, so a trigger raised while no peer is blocked simply makes that peer's next
// kevent() return immediately -- it re-scans primes and re-blocks, so no wakeup is ever lost.
static void ep_wake_arm(int ep) {
    if (ep < 0 || ep >= DD_NFD || g_ep_wake_armed[ep]) return;
    struct kevent kv;
    EV_SET(&kv, EP_WAKE_IDENT, EVFILT_USER, EV_ADD | EV_CLEAR, 0, 0, NULL);
    if (kevent(ep, &kv, 1, NULL, 0, NULL) == 0) g_ep_wake_armed[ep] = 1;
}

// Push the deferred changelist to the kernel now (so an fd registered/removed on this thread becomes
// visible to a peer M already blocked in kevent) and, when `wake` is set (interest was added/modified),
// NOTE_TRIGGER the wake knote so that blocked peer returns and re-scans primes for an already-ready fd.
// Caller holds g_ep_mtx. Only used when g_threaded, so the W3E batching still applies single-threaded.
static void ep_flush(int ep, int wake) {
    if (ep < 0 || ep >= DD_NFD) return;
    if (g_ep_chgn[ep] > 0) {
        kevent(ep, g_ep_chg[ep], g_ep_chgn[ep], NULL, 0, NULL); // registrations only; ignore EV_ERROR echoes
        ep_count();
        g_ep_chgn[ep] = 0;
    }
    if (!wake) return;
    ep_wake_arm(ep);
    struct kevent trig;
    EV_SET(&trig, EP_WAKE_IDENT, EVFILT_USER, 0, NOTE_TRIGGER, 0, NULL);
    kevent(ep, &trig, 1, NULL, 0, NULL);
    ep_count();
}

// macOS does NOT inherit kqueue() descriptors across fork(2) (unlike Linux epoll/timer/inotify fds, which
// are), so every epoll/timerfd/inotify fd the engine emulates with a kqueue is DEAD in a freshly forked
// child. A guest that then closes or re-arms it sees EBADF -- e.g. Ruby's post-fork timer-thread reset
// close()s its inherited epoll fd, hits EBADF, reports "[ASYNC BUG] close event_fd" and aborts the child.
// Rebuild a fresh kqueue at each such fd NUMBER so the descriptor is valid again; the (empty) instance
// matches the re-init every runtime does post-fork, and the guest re-registers its own interest. Only fds
// that are actually dead are rebuilt -- a stale marker on an fd the parent closed and reused for a live
// (inherited) file leaves that file untouched. Called from the fork child in proc.c, before the guest runs.
static void kqueue_rebuild_after_fork(void) {
    for (int fd = 0; fd < DD_NFD; fd++) {
        if (!(g_epoll[fd] || g_timerfd[fd] || g_inotify[fd])) continue;
        if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) continue; // still a live inherited fd -> leave it
        int kq = kqueue();
        if (kq < 0) continue;
        if (kq != fd) {
            dup2(kq, fd);
            close(kq);
        }
        // timerfd: Linux children INHERIT the armed timer. The deadline/interval survive the fork (COW BSS),
        // so re-arm the EVFILT_TIMER on the fresh kqueue from them (converting the absolute monotonic
        // deadline back to a relative first delay), rather than leaving the child's timer disarmed.
        if (g_timerfd[fd] && g_tfd_deadline[fd] > 0) {
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
            int64_t iv = g_tfd_interval[fd];
            int64_t delay = g_tfd_deadline[fd] - now_ns;
            if (delay < 0) delay = (iv > 0) ? (iv - ((-delay) % iv)) : 0;
            struct kevent kv;
            // A periodic timer still pending its distinct first tick (one-shot-first) inherits that pending
            // one-shot: re-arm one-shot at the remaining first delay; the child's read() then re-arms periodic.
            int one = (iv <= 0) || g_tfd_first_oneshot[fd];
            uint16_t flg = EV_ADD | (one ? EV_ONESHOT : 0);
            int64_t arm = one ? delay : iv;
            if (arm < 0) arm = 0;
            EV_SET(&kv, 1, EVFILT_TIMER, flg, NOTE_NSECONDS, arm, NULL);
            kevent(fd, &kv, 1, NULL, 0, NULL);
        }
        // inotify: Linux children inherit the instance AND its watches. The watch fds (O_EVTONLY opens) are
        // ordinary fds that survive the fork, so re-register each one's EVFILT_VNODE on the rebuilt kqueue and
        // re-apply O_NONBLOCK (the fresh kqueue is blocking by default -> an inherited nonblock read could hang).
        if (g_inotify[fd]) {
            if (g_inotify_nb[fd]) fcntl(fd, F_SETFL, O_NONBLOCK);
            for (int w = 0; w < 1024; w++) {
                if (g_inotify_owner[w] != fd) continue;
                if (fcntl(w, F_GETFD) == -1) continue; // the watch fd itself must still be open
                struct kevent wkv;
                EV_SET(&wkv, w, EVFILT_VNODE, EV_ADD | EV_CLEAR,
                       NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND, 0, (void *)(intptr_t)w);
                kevent(fd, &wkv, 1, NULL, 0, NULL);
            }
        }
        // the fresh instance carries no registrations: drop this epoll fd's inherited (now-invalid) changelist
        // and prime buffer so a later epoll_ctl/epoll_wait re-arms against the new kqueue, not stale state.
        if (g_ep_chg[fd]) {
            free(g_ep_chg[fd]);
            g_ep_chg[fd] = NULL;
        }
        g_ep_chgn[fd] = g_ep_chgcap[fd] = 0;
        if (g_ep_prime[fd]) {
            free(g_ep_prime[fd]);
            g_ep_prime[fd] = NULL;
        }
        g_ep_primen[fd] = g_ep_primecap[fd] = 0;
        ep_mem_clear(fd); // the rebuilt kqueue carries no registrations -> drop stale membership too
    }
    // every kqueue was rebuilt empty -> no watched fd is armed on any instance anymore (the armed map is
    // per-watched-fd and shared across epoll instances, so clear it wholesale to match the fresh kqueues).
    memset(g_ep_rd, 0, sizeof g_ep_rd);
    memset(g_ep_wr, 0, sizeof g_ep_wr);
    memset(g_ep_os, 0, sizeof g_ep_os);
    // the rebuilt kqueues carry no EVFILT_USER wake knote either -> re-arm lazily on next epoll op
    memset(g_ep_wake_armed, 0, sizeof g_ep_wake_armed);
    // Linux children INHERIT the parent's epoll interest list. The interest table (fd -> owner+events+udata)
    // survived the fork via COW, and the watched fds are ordinary inherited descriptors, so re-arm every
    // recorded registration on its owning (rebuilt, empty) epoll kqueue and restore the armed maps + the
    // membership we just cleared. A child that epoll_waits WITHOUT re-registering now sees inherited events
    // (the timerfd/inotify halves are re-armed in the rebuild loop above).
    for (int fd = 0; fd < DD_NFD; fd++) {
        if (!g_ep_owner[fd]) continue;
        int ep = g_ep_owner[fd] - 1;
        int drop = (ep < 0 || ep >= DD_NFD || !g_epoll[ep] || fcntl(ep, F_GETFD) == -1 || fcntl(fd, F_GETFD) == -1);
        if (drop) { // owner epoll or watched fd did not survive into the child -> drop the stale entry
            g_ep_owner[fd] = 0;
            g_ep_events[fd] = 0;
            g_ep_udata[fd] = 0;
            continue;
        }
        ep_rearm_from_interest(ep, fd, fd);
        uint32_t ev = g_ep_events[fd];
        g_ep_rd[fd] = (ev & 0x1) ? 1 : 0;
        g_ep_wr[fd] = (ev & 0x4) ? 1 : 0;
        g_ep_os[fd] = (ev & 0x40000000u) ? 1 : 0;
        ep_mem_set(ep, fd, 1);
    }
    // fork() only clones the calling thread: if a peer M held g_ep_mtx (mid epoll_ctl/epoll_wait) at fork
    // time the child inherits it LOCKED with no owner, so its next svc_event ep_lock() deadlocks forever
    // (the go-build compile child hit exactly this after the g_jit_lock fix). The child is single-threaded
    // now, so reinitialising it to unlocked is always correct. (Same fork-unsafe-mutex class as g_jit_lock.)
    pthread_mutex_init(&g_ep_mtx, NULL);
}

// pselect6/ppoll/epoll_pwait install a temporary signal mask for the duration of the wait (Linux swaps the
// blocked mask atomically so a caller can unblock a signal exactly while it waits). dd's wait loops gate on
// c->sigmask via svc_poll_retry, so installing the guest mask into c->sigmask for the wait makes an
// unblocked signal interrupt the host poll/select/kevent -- previously the mask was ignored and a
// signal-driven wait slept the full timeout. `smptr` is the guest sigset_t address (bit signo-1), or 0 for
// "no temporary mask". Returns 1 if a mask was installed (previous mask stored in *saved).
static int poll_sigmask_enter(struct cpu *c, uint64_t smptr, uint64_t *saved) {
    if (!smptr) return 0;
    uint64_t nm = *(uint64_t *)smptr;
    nm &= ~((1ull << (9 - 1)) | (1ull << (19 - 1))); // SIGKILL/SIGSTOP can never be blocked
    *saved = c->sigmask;
    c->sigmask = nm;
    return 1;
}
// Restore the pre-wait mask. A signal that became deliverable under the temporary mask but is blocked under
// the restored mask must still run its handler (Linux delivers it during the wait, then restores the mask on
// return) -- force exactly those bits via g_force_deliver, mirroring rt_sigsuspend (signal.c case 133).
static void poll_sigmask_leave(struct cpu *c, uint64_t saved) {
    uint64_t temp = c->sigmask;
    uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    for (int s = 1; s <= 64; s++) {
        uint64_t bit = 1ull << s;
        if (!(p & bit)) continue;
        if (temp & (1ull << (s - 1))) continue; // was blocked during the wait -> not delivered
        if ((saved & (1ull << (s - 1))) && g_sigact[s].handler > 1)
            g_force_deliver |= bit; // blocked again on restore, but Linux already delivered it -> force it
    }
    c->sigmask = saved;
}

static int svc_event(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                     uint64_t a5) {
    switch (nr) {
    // ===================== Event loop — epoll/eventfd/timerfd/signalfd/inotify (macOS kqueue) =====================
    // eventfd2(initval, flags) -> pipe
    case 19: {
        // Validate `flags` exactly as Linux (fs/eventfd.c): only EFD_SEMAPHORE(1) | EFD_NONBLOCK(O_NONBLOCK
        // 0x800) | EFD_CLOEXEC(O_CLOEXEC 0x80000) are defined; any other bit -> EINVAL. Chromium's Mojo
        // EventFDNotifier::KernelSupported() probes eventfd2(0, ~0) and PCHECKs it FAILS with EINVAL/ENOSYS/
        // EPERM (channel_linux.cc); without this the probe SUCCEEDED and the browser aborted.
        if ((unsigned)a1 & ~(unsigned)(1u | 0x800u | 0x80000u)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int fds[2];
        if (pipe(fds) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        int peer = fds[1];
        int hi = fcntl(peer, F_DUPFD, 1 << 20);
        if (hi < 0) hi = fcntl(peer, F_DUPFD, 64);
        if (hi >= 0) {
            close(peer);
            peer = hi;
        }
        if (a1 & 0x80000) {
            fcntl(fds[0], F_SETFD, FD_CLOEXEC);
            fcntl(peer, F_SETFD, FD_CLOEXEC);
            // EFD_CLOEXEC
        }
        // Keep the read end PERMANENTLY O_NONBLOCK at the host level so the counter/pipe drains in io.c
        // never toggle the (cross-process-shared) fd flags. The guest's real EFD_NONBLOCK intent is tracked
        // in g_eventfd_gnb and honoured by the read path (poll() when the guest wants to block). See the
        // g_eventfd_gnb note in vfs.c.
        fcntl(fds[0], F_SETFL, O_NONBLOCK);
        // writes to the eventfd go to fds[1]; the counter + sema-flag live alongside.
        if (fds[0] >= 0 && fds[0] < DD_NFD) {
            g_eventfd_peer[fds[0]] = peer + 1;
            g_eventfd_cslot[fds[0]] = fds[0] + 1;
            g_eventfd_sema[fds[0]] = (a1 & 1) != 0; // EFD_SEMAPHORE
            g_eventfd_gnb[fds[0]] = (a1 & 0x800) != 0; // EFD_NONBLOCK -> guest wants non-blocking reads
            g_eventfd_count[fds[0]] = a0;           // initval
            g_eventfd_refs[fds[0]] = 1;             // one alias (this fd); dup() bumps it (fd_carry_virt)
            if (a0 > 0) {
                char b = 1;
                if (write(peer, &b, 1) < 0) {}
            } // make it readable
        }
        G_RET(c) = (uint64_t)fds[0];
        break;
    }
    case 20: {
        // epoll_create1(flags) -> kqueue. Only EPOLL_CLOEXEC (0x80000) is defined; any other bit is
        // rejected with EINVAL by the Linux kernel (LTP epoll_create1_01).
        if (a0 & ~0x80000u) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int r = kqueue();
        // EPOLL_CLOEXEC
        // macOS kqueue() defaults FD_CLOEXEC SET; Linux epoll_create1(0) leaves it CLEAR. Set it exactly
        // per the EPOLL_CLOEXEC flag (clear when absent) so epoll_create1_01's no-CLOEXEC assertion holds.
        if (r >= 0) fcntl(r, F_SETFD, (a0 & 0x80000) ? FD_CLOEXEC : 0);
        // a reused fd number must start with an empty prime buffer + no stale wake knote + no stale membership
        // (close() doesn't clear ours -- this is how an epoll fd's per-instance state is reset on reuse)
        if (r >= 0 && r < DD_NFD) {
            g_ep_primen[r] = 0;
            g_ep_wake_armed[r] = 0;
            g_epoll[r] = 1;
            ep_mem_clear(r);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // epoll_ctl(epfd, op, fd, event) -> kevent
    case 21: {
        int op = (int)a1, fd = (int)a2, epfd = (int)a0;
        uint32_t ev = 0;
        uint64_t data = (uint64_t)(unsigned)fd;
        // (extends): epoll_ctl(2) full error surface, in the kernel's exact ORDER (LTP
        // epoll_ctl02). kqueue silently accepts bad ops/fds and never faults on a NULL event, so enforce
        // each Linux return explicitly. Every check below fires ONLY on input that already errors on Linux,
        // so a well-formed ADD/MOD/DEL is behaviourally unchanged (it costs one extra fstat for the EBADF/
        // EPERM probe -- the ADD path already did that fstat).
        // (1) EFAULT: ADD(1)/MOD(3) -- any op that "has an event" (op != DEL) -- dereference `event`.
        if (op != 2 && (!a3 || guest_bad_ptr((uintptr_t)a3, G_EPEV_SZ))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) EBADF: epfd must be an open fd. A dd-tracked epoll (g_epoll set) is known-valid -> only an
        // untracked epfd is probed (a dup'd/large epoll fd keeps the best-effort immediate path).
        if (!(epfd >= 0 && epfd < DD_NFD && g_epoll[epfd]) && fcntl(epfd, F_GETFD) == -1) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // (3) EINVAL: cannot add the epoll fd to itself. Checked before the fd fstat -- epfd is a kqueue,
        // which is a valid pollable fd, so this avoids relying on fstat's shape for a kqueue.
        if (fd == epfd) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (4) EBADF if fd is not open; (5) EPERM if it is open but cannot be polled (a regular file /
        // directory is not epoll-watchable). One fstat serves both -- gated to ADD only (as the path
        // was) so the hot MOD/DEL rearm path (Go's EPOLLONESHOT netpoller) stays fstat-free; a MOD/DEL of a
        // bad/unregistered fd still resolves correctly via the ENOENT membership check below.
        if (op == 1) {
            struct stat st;
            if (fstat(fd, &st) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (S_ISREG(st.st_mode) || S_ISDIR(st.st_mode)) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
        }
        // (6) EINVAL: op must be ADD/DEL/MOD.
        if (op != 1 && op != 2 && op != 3) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (a3) {
            ev = *(uint32_t *)a3;
            memcpy(&data, (void *)(a3 + G_EPEV_DOFF), 8);
            // struct epoll_event {u32 events; [pad;] u64 data} -- layout per guest arch (see G_EPEV_*)
        }
        // (7/8/9) EEXIST (ADD an already-registered fd) / ENOENT (MOD|DEL an absent fd) on a dd-tracked epoll
        // instance (membership bitmap). Confined to fd < DD_NFD, matching the readiness path below.
        if (epfd >= 0 && epfd < DD_NFD && g_epoll[epfd] && fd >= 0 && fd < DD_NFD) {
            int ep = epfd;
            int member = ep_mem_test(ep, fd);
            if (op == 1 && member) {
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            } // ADD an already-registered fd
            if ((op == 2 || op == 3) && !member) {
                G_RET(c) = (uint64_t)(-ENOENT);
                break;
            } // MOD/DEL an absent fd
            ep_mem_set(ep, fd, op != 2); // commit membership
        }
        // Record this registration in the per-instance interest table (fd -> owner + events + udata) so it
        // survives fork (re-armed on the rebuilt child kqueue) and a watched-fd close whose OFD lives on via a
        // dup (re-homed onto the surviving alias). DEL drops the entry. Confined to in-range epfd/fd, matching
        // the readiness path; a couple of fd-indexed stores, so the epoll_ctl hot path is essentially unchanged.
        if (epfd >= 0 && epfd < DD_NFD && g_epoll[epfd] && fd >= 0 && fd < DD_NFD) {
            if (op == 2) { // DEL
                g_ep_owner[fd] = 0;
                g_ep_events[fd] = 0;
                g_ep_udata[fd] = 0;
            } else { // ADD / MOD
                g_ep_owner[fd] = epfd + 1;
                g_ep_events[fd] = ev;
                g_ep_udata[fd] = data;
            }
        }
        if (w7_on() && fd >= 0 && fd < DD_NFD && (g_sock_fam[fd] == AF_UNIX || g_eventfd_peer[fd])) {
            if (epfd >= 0 && epfd < DD_NFD && op != 2) g_ep_has_mojo[epfd] = 1;
            w7_trace("epoll_ctl %s epfd=%d fd=%d ev=0x%x%s%s kind=%s", op == 1 ? "ADD" : op == 2 ? "DEL" : "MOD",
                     epfd, fd, ev, (ev & 0x1) ? " IN" : "", (ev & 0x80000000u) ? " ET" : "",
                     g_eventfd_peer[fd] ? "eventfd" : "unix");
        }
        // op: 1=ADD 2=DEL 3=MOD ; EPOLLET=0x80000000 -> EV_CLEAR ; EPOLLONESHOT=0x40000000 -> EV_ONESHOT
        uint16_t xf = (uint16_t)((ev & 0x80000000u ? EV_CLEAR : 0) | (ev & 0x40000000u ? EV_ONESHOT : 0));
        int want_rd = (op != 2) && (ev & 0x1); // EPOLLIN
        int want_wr = (op != 2) && (ev & 0x4); // EPOLLOUT
        if (epopt_on() && (int)a0 >= 0 && (int)a0 < DD_NFD && !g_ep_dupd[(int)a0] && fd >= 0 && fd < DD_NFD) {
            // W3E fast path: track armed filters, defer the change to the next epoll_wait kevent(). A dup'd
            // instance is excluded (g_ep_dupd) so its interest is submitted immediately to the shared kqueue.
            int ep = (int)a0;
            int lk = ep_lock();
            if (want_rd) {
                ep_push(ep, fd, EVFILT_READ, EV_ADD | xf, (void *)data);
                g_ep_rd[fd] = 1;
                if (xf & EV_CLEAR) ep_prime_if_ready(ep, fd, EVFILT_READ, (void *)data);
            } else if (g_ep_rd[fd]) {
                ep_push(ep, fd, EVFILT_READ, EV_DELETE, (void *)data);
                g_ep_rd[fd] = 0;
            }
            if (want_wr) {
                ep_push(ep, fd, EVFILT_WRITE, EV_ADD | xf, (void *)data);
                g_ep_wr[fd] = 1;
                if (xf & EV_CLEAR) ep_prime_if_ready(ep, fd, EVFILT_WRITE, (void *)data);
            } else if (g_ep_wr[fd]) {
                ep_push(ep, fd, EVFILT_WRITE, EV_DELETE, (void *)data);
                g_ep_wr[fd] = 0;
            }
            g_ep_os[fd] = (op != 2 && (ev & 0x40000000u)) ? 1 : 0;
            // Multi-threaded guest: a peer M may be blocked in epoll_wait on this instance right now, so the
            // deferred registration/prime must reach it -- flush the changelist to the kernel and (when we
            // added/modified interest) wake the peer to re-scan primes. No-op single-threaded, where the same
            // thread issues the next epoll_wait and consumes the changelist itself.
            if (lk) ep_flush(ep, op != 2);
            ep_unlock(lk);
            G_RET(c) = 0;
            break;
        }
        // ---- original immediate path (NOEPOLLOPT=1 or fd/epfd out of range) ----
        struct kevent kv[2];
        int n = 0;
        uint16_t base = (op == 2) ? EV_DELETE : EV_ADD;
        if (op == 2 || (ev & 0x1)) {
            EV_SET(&kv[n], fd, EVFILT_READ, base | xf, 0, 0, (void *)data);
            n++;
            // EPOLLIN
        }
        if (op == 2 || (ev & 0x4)) {
            EV_SET(&kv[n], fd, EVFILT_WRITE, base | xf, 0, 0, (void *)data);
            n++;
            // EPOLLOUT
        }
        for (int i = 0; i < n; i++) {
            // per-filter so DEL of an absent one is ignored
            kevent((int)a0, &kv[i], 1, NULL, 0, NULL);
            ep_count();
        }
        // EPOLLET: prime an already-ready fd so its initial readiness is reported (see g_ep_prime).
        if ((xf & EV_CLEAR) && op != 2) {
            if (want_rd) ep_prime_if_ready((int)a0, fd, EVFILT_READ, (void *)data);
            if (want_wr) ep_prime_if_ready((int)a0, fd, EVFILT_WRITE, (void *)data);
        }
        G_RET(c) = 0;
        break;
    }
    // epoll_pwait(epfd, events, max, timeout_ms, sigmask)
    case 22: {
        int maxev = (int)a2;
        // Linux rejects maxevents <= 0 with EINVAL before waiting; do not clamp it to a poll.
        if (maxev <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (maxev > 256) maxev = 256;
        // epoll_pwait(epfd, events, max, tmo, sigmask, sigsetsize): a4 is the guest sigset_t pointer, a5 its
        // size. Apply the temporary signal mask for the wait (Linux swaps it atomically); NULL a4 = no mask.
        uint64_t sm_set = 0, sm_saved = 0;
        if (a4) {
            if ((size_t)a5 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (guest_bad_ptr(a4, 8)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            sm_set = a4;
        }
        struct kevent kv[256];
        uint8_t *out = (uint8_t *)a1;
        int ep = (int)a0;
        // A dup'd instance opts out of the deferred-changelist machinery (its interest was submitted straight
        // to the shared kqueue), so it just blocks on the kqueue like the immediate path.
        int opt = epopt_on() && ep >= 0 && ep < DD_NFD && !g_ep_dupd[ep];
        int32_t tmo = (int32_t)a3; // guest timeout ms: <0 = infinite (must NEVER return 0), 0 = poll, >0 = finite
        // regression fix: a cross-thread epoll_ctl fires the internal EVFILT_USER wake knote, which
        // returns us from kevent() with ONLY that nudge and no guest event -> oi==0. On real Linux epoll_wait
        // with an infinite timeout NEVER returns 0 (libuv asserts timeout!=-1 on a 0-return and node aborts),
        // and a finite wait must re-block for the REMAINING budget, not the full timeout again. So we loop:
        // capture a monotonic deadline at entry and, whenever we produced no guest event but the guest still
        // wants to block, re-enter kevent for the time that remains. (The old "safety net" was gated on nchg>0,
        // so a BARE cross-thread wake -- which carries no changelist on this thread -- fell through and returned
        // 0.) Each re-block genuinely sleeps in kevent (the EVFILT_USER knote is EV_CLEAR, already consumed) --
        // no busy spin. Single-threaded semantics are preserved: kevent(NULL) never returns 0, and a 0/finite
        // poll behaves as before.
        struct timespec deadline = {0, 0};
        if (tmo > 0) {
            clock_gettime(CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += tmo / 1000;
            deadline.tv_nsec += (long)(tmo % 1000) * 1000000L;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
        }
        int oi = 0;
        int sm_on = poll_sigmask_enter(c, sm_set, &sm_saved);
        for (;;) {
            struct timespec ts, *tp = NULL;
            if (tmo == 0) {
                ts.tv_sec = 0;
                ts.tv_nsec = 0;
                tp = &ts; // non-blocking poll
            } else if (tmo > 0) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t rem = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (rem < 0) rem = 0;
                ts.tv_sec = (time_t)(rem / 1000000000LL);
                ts.tv_nsec = (long)(rem % 1000000000LL);
                tp = &ts;
            } // tmo < 0 -> tp stays NULL (block forever)
            // Multi-threaded guest: serialize against peer Ms doing epoll_ctl on this instance. Arm the wake
            // knote and push any deferred changelist to the kernel BEFORE we block, so a peer's registration is
            // kernel-visible to us and its NOTE_TRIGGER can wake us. We then block on a pure wait (no changelist)
            // with the lock released, so epoll_ctl on another M is never blocked behind our sleep. Single-threaded
            // (lk == 0) keeps the classic one-syscall ctl+wait batching, unchanged.
            int lk = opt ? ep_lock() : 0;
            if (lk) {
                ep_wake_arm(ep);
                ep_flush(ep, 0);
            }
            // A pending edge-prime means some fd is ready *now*; don't sleep waiting for a fresh kqueue edge
            // (a Go server's epoll_wait blocks with an infinite timeout) -- poll kqueue and merge the prime in.
            if (opt && g_ep_primen[ep] > 0) {
                ts.tv_sec = 0;
                ts.tv_nsec = 0;
                tp = &ts;
            }
            // W3E: submit the deferred changelist together with the wait in ONE kevent() syscall (single-threaded);
            // threaded already flushed it above and waits with no changelist.
            struct kevent *chg = (opt && !lk) ? g_ep_chg[ep] : NULL;
            int nchg = (opt && !lk) ? g_ep_chgn[ep] : 0;
            if (lk) ep_unlock(lk);
            int r;
            // epoll_wait is never restarted by a handler -- re-wait only on a SPURIOUS EINTR (nothing to
            // deliver); the moment a guest handler is runnable we return -EINTR and let the dispatcher run it.
            // kevent applies the changelist before blocking, so a retry re-waits only (changes consumed -> none).
            if (w7_on() && ep >= 0 && ep < DD_NFD && g_ep_has_mojo[ep] && tp == NULL)
                w7_trace("epoll_wait BLOCK epfd=%d tmo=inf nchg=%d", ep, nchg); // dormant if no matching WAKE follows
            ts_wait_enter();                                                    // 'S' while blocked in epoll_wait/epoll_pwait
            do {
                r = kevent(ep, chg, nchg, kv, maxev, tp);
                ep_count();
                chg = NULL;
                nchg = 0;
            } while (r < 0 && svc_poll_retry(c));
            ts_wait_leave();
            if (w7_on() && ep >= 0 && ep < DD_NFD && g_ep_has_mojo[ep] && r > 0) {
                char ids[240];
                int p = 0;
                for (int i = 0; i < r && p < (int)sizeof ids - 28; i++) {
                    const char *fn = kv[i].filter == EVFILT_READ    ? "R"
                                     : kv[i].filter == EVFILT_WRITE  ? "W"
                                     : kv[i].filter == EVFILT_USER   ? "U"
                                                                     : "?";
                    p += snprintf(ids + p, sizeof ids - (size_t)p, " %d%s%s", (int)kv[i].ident, fn,
                                  (kv[i].flags & EV_EOF) ? "!EOF" : "");
                }
                w7_trace("epoll_wait WAKE epfd=%d r=%d fds=%s", ep, r, ids);
            }
            if (opt && !lk) g_ep_chgn[ep] = 0; // consumed (threaded flushed it under the lock already)
            if (r < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            lk = opt ? ep_lock() : 0; // re-acquire to guard the armed-map updates + prime scan below
            oi = 0;
            for (int i = 0; i < r && oi < maxev; i++) {
                // The EVFILT_USER self-wake knote is an internal cross-thread nudge, not a guest event -- drop it.
                if (kv[i].filter == EVFILT_USER) continue;
                // An EV_ERROR entry is a *changelist* processing result (errno in .data), NOT a readiness
                // event. With correct armed-state tracking these do not occur; skip them if they do.
                if (opt && (kv[i].flags & EV_ERROR)) continue;
                uint32_t ev = (kv[i].filter == EVFILT_READ) ? 0x1u : (kv[i].filter == EVFILT_WRITE) ? 0x4u : 0u;
                // EPOLLHUP
                if (kv[i].flags & EV_EOF) ev |= 0x10u;
                // EPOLLERR (immediate-path semantics preserved when opt is off)
                if (!opt && (kv[i].flags & EV_ERROR)) ev |= 0x8u;
                *(uint32_t *)(out + (size_t)oi * G_EPEV_SZ) = ev;
                memcpy(out + (size_t)oi * G_EPEV_SZ + G_EPEV_DOFF, &kv[i].udata, 8);
                // EPOLLONESHOT: the kernel auto-removed this registration; keep our armed map in sync.
                if (opt && kv[i].ident < DD_NFD && g_ep_os[kv[i].ident]) {
                    if (kv[i].filter == EVFILT_READ)
                        g_ep_rd[kv[i].ident] = 0;
                    else if (kv[i].filter == EVFILT_WRITE)
                        g_ep_wr[kv[i].ident] = 0;
                }
                oi++;
            }
            // Deliver edge-triggered primes that kqueue didn't surface (fds already ready at registration).
            // This is the cross-thread-readiness delivery: a peer M that registered an already-ready fd
            // stashed a prime here, so a wake that carried no kqueue edge still hands the guest the ready fd.
            if (ep >= 0 && ep < DD_NFD && g_ep_primen[ep] > 0) {
                int kept = 0;
                for (int i = 0; i < g_ep_primen[ep]; i++) {
                    struct kevent *pk = &g_ep_prime[ep][i];
                    uint32_t pev = (pk->filter == EVFILT_READ) ? 0x1u : 0x4u;
                    int dup = 0;
                    for (int j = 0; j < oi; j++) {
                        uint32_t jev;
                        uint64_t ju;
                        memcpy(&jev, out + (size_t)j * G_EPEV_SZ, 4);
                        memcpy(&ju, out + (size_t)j * G_EPEV_SZ + G_EPEV_DOFF, 8);
                        if (ju == (uint64_t)pk->udata && (jev & pev)) {
                            dup = 1;
                            break;
                        }
                    }
                    if (dup) continue; // kqueue already reported it
                    if (oi >= maxev) {
                        g_ep_prime[ep][kept++] = *pk;
                        continue;
                    } // no room -> keep for next wait
                    *(uint32_t *)(out + (size_t)oi * G_EPEV_SZ) = pev;
                    memcpy(out + (size_t)oi * G_EPEV_SZ + G_EPEV_DOFF, &pk->udata, 8);
                    oi++;
                }
                g_ep_primen[ep] = kept;
            }
            ep_unlock(lk);
            // Re-block instead of returning a spurious 0. A bare cross-thread wake (or a changelist that only
            // produced EV_ERROR echoes) leaves oi==0 while the guest still asked to block. tmo<0: always loop
            // (epoll_wait(-1) must never return 0). tmo>0: loop until the monotonic deadline elapses, at which
            // point the remaining-time recompute yields a 0 timeout and the next kevent returns a genuine 0.
            // tmo==0: returning 0 is correct (non-blocking poll) -- never loop.
            if (oi == 0 && tmo != 0) {
                if (tmo < 0) continue;
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t rem = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (rem > 0) continue;
            }
            G_RET(c) = (uint64_t)oi;
            break;
        }
        if (sm_on) poll_sigmask_leave(c, sm_saved);
        break;
    }
    case 26: {
        // inotify_init1(flags) -> kqueue. Only IN_NONBLOCK(0x800) and IN_CLOEXEC(0x80000) are defined;
        // Linux rejects any other flag bit with EINVAL, so a bad-flag probe must not read as supported.
        if ((int)a0 & ~(0x800 | 0x80000)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int r = kqueue();
        if (r >= 0) {
            if (r < DD_NFD) {
                g_inotify[r] = 1;
                g_inotify_nb[r] = (a0 & 0x800) ? 1 : 0; // remember IN_NONBLOCK for the fork-child kqueue rebuild
            }
            if (a0 & 0x800) fcntl(r, F_SETFL, O_NONBLOCK);
            // macOS kqueue() defaults FD_CLOEXEC SET; Linux inotify_init1(0) leaves it CLEAR. Set it exactly
            // per IN_CLOEXEC (clearing the kqueue default otherwise) so an inotify fd created without the
            // flag survives exec instead of being swept by dd's close-on-exec pass.
            fcntl(r, F_SETFD, (a0 & 0x80000) ? FD_CLOEXEC : 0);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // inotify_add_watch(fd, path, mask) -- kqueue EVFILT_VNODE
    case 27: {
        char pb[4200];
        // confined (realpath gate)
        const char *p = atpath(-100, (const char *)a1, pb, sizeof pb, 0);
        int wfd = open(p, O_EVTONLY);
        if (wfd < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        struct kevent kv;
        EV_SET(&kv, wfd, EVFILT_VNODE, EV_ADD | EV_CLEAR,
               NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND, 0, (void *)(intptr_t)wfd);
        if (kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0) {
            int e = errno;
            close(wfd);
            G_RET(c) = (uint64_t)(-(int64_t)e);
            break;
        }
        // a directory watch: remember the path + a snapshot so read() can diff into IN_CREATE/IN_DELETE+name
        struct stat dst;
        if (wfd >= 0 && wfd < DD_NFD && stat(p, &dst) == 0 && S_ISDIR(dst.st_mode)) {
            snprintf(g_inotify_wpath[wfd], sizeof g_inotify_wpath[wfd], "%s", p);
            free(g_inotify_snap[wfd]);
            g_inotify_snap[wfd] = dir_snapshot(p);
            g_inotify_owner[wfd] = (int)a0; // the inotify instance this watch belongs to (for the move queue)
        }
        G_RET(c) = (uint64_t)wfd;
        break;
        // watch descriptor = the watched fd
    }
    case 28: {
        struct kevent kv;
        // inotify_rm_watch(fd, wd). The wd is the watched fd; deleting the EVFILT_VNODE knote from THIS
        // inotify kqueue is the source of truth for "is this a real watch of this instance". If the knote
        // is not registered here (bad/foreign wd), kevent fails ENOENT -- Linux returns EINVAL and leaves
        // the fd alone, so we must NOT close(wd) or we would silently destroy an unrelated guest fd.
        EV_SET(&kv, (int)a1, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
        if (kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        close((int)a1);
        G_RET(c) = 0;
        break;
    }
    case 72: { // pselect6(nfds, readfds, writefds, exceptfds, timeout(timespec), sigmask) -> pselect.
        // The Linux/macOS fd_set byte-layout is identical (bit N at byte N/8), so pass the sets through.
        int have_to = a4 != 0;
        // EFAULT on any inaccessible fd_set / timeout pointer -- incl. a PROT_NONE guard page (LTP's
        // tst_get_bad_addr), which host_range_mapped alone misses since dd force-maps guest anon host-RW.
        int selnfds = (int)a0;
        size_t nb = selnfds > 0 ? ((size_t)selnfds + 7) / 8 : 0;
        if (nb > sizeof(fd_set)) nb = sizeof(fd_set);
        if ((a1 && guest_bad_ptr(a1, nb)) || (a2 && guest_bad_ptr(a2, nb)) || (a3 && guest_bad_ptr(a3, nb)) ||
            (have_to && guest_bad_ptr(a4, sizeof(struct timespec)))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Linux rejects an out-of-range timeout nanoseconds field (tv_nsec < 0 or >= 1e9) with EINVAL
        // before waiting; dd must not treat it as a normal timeout and hide the caller bug.
        if (have_to) {
            long tns = ((const struct timespec *)a4)->tv_nsec;
            if (tns < 0 || tns >= 1000000000L) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // pselect6 6th arg (a5): pointer to { const sigset_t *ss; size_t ss_len; }. Resolve the guest sigset
        // address so the wait honours the temporary signal mask Linux swaps in atomically (see
        // poll_sigmask_enter). NULL a5 (or a NULL inner ss) = no temporary mask.
        uint64_t sm_set = 0, sm_saved = 0;
        if (a5) {
            if (guest_bad_ptr(a5, 16)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            const uint64_t *pk = (const uint64_t *)a5;
            uint64_t ssp = pk[0], sslen = pk[1];
            if (ssp) {
                if (sslen != 8) {
                    G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                    break;
                }
                if (guest_bad_ptr(ssp, 8)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                sm_set = ssp;
            }
        }
        // a spurious EINTR (a signal dd hooks with host_sigh but the guest has BLOCKED or defaults to
        // ignore -- e.g. an LTP heartbeat, or SIGCHLD from a reaped child) interrupts the host pselect but
        // must NOT restart the FULL original timeout: that overshoots the deadline and, under a *repeating*
        // spurious wakeup, never reaches it -> select02 hangs (and every timing sample overshoots -> the
        // select01/poll02/pselect01 tst_timer FAILs). Capture a monotonic deadline once and re-block only
        // for the time that remains, exactly like epoll_pwait (case 22). Linux ALSO writes the leftover time
        // back into the timeout struct (both select(2) and the raw pselect6(2) syscall do), so mirror that.
        struct timespec deadline = {0, 0};
        if (have_to) {
            struct timespec ts = *(struct timespec *)a4;
            clock_gettime(CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += ts.tv_sec;
            deadline.tv_nsec += ts.tv_nsec;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
        }
        int sm_on = poll_sigmask_enter(c, sm_set, &sm_saved);
        int r;
        for (;;) {
            struct timespec rem, *tsp = NULL;
            if (have_to) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns < 0) ns = 0;
                rem.tv_sec = (time_t)(ns / 1000000000LL);
                rem.tv_nsec = (long)(ns % 1000000000LL);
                tsp = &rem;
            }
            ts_wait_enter();
            r = pselect((int)a0, (fd_set *)a1, (fd_set *)a2, (fd_set *)a3, tsp, NULL);
            ts_wait_leave(); // S while blocked (glibc pause on aarch64 lands in ppoll below; select here)
            // pselect is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry),
            // and then only for the time that remains (recomputed above), never the full budget again.
            if (r < 0 && svc_poll_retry(c)) continue;
            break;
        }
        if (sm_on) poll_sigmask_leave(c, sm_saved);
        if (r >= 0 && have_to) {
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
            if (ns < 0) ns = 0;
            ((struct timespec *)a4)->tv_sec = (time_t)(ns / 1000000000LL);
            ((struct timespec *)a4)->tv_nsec = (long)(ns % 1000000000LL);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 73: {
        struct pollfd *fds = (void *)a0;
        // ppoll -> poll. macOS has no ppoll, so collapse the timespec deadline into poll's int-ms timeout.
        // skalibs iopause (s6-supervise et al.) hands a huge but FINITE relative deadline for an
        // idle-but-up service -- settimeout_infinite() makes the delta exactly tain_infinite_relative,
        // whose tv_sec = 2^61 = 2305843009213693952. On real Linux ppoll takes that timespec and blocks.
        // Here the naive (int)(tv_sec*1000) truncates: 2^61 * 1000 == 0 (mod 2^32), so tmo became 0 ->
        // poll returned immediately -> s6 saw a spurious timeout in the UP state and busy-looped printing
        // "can't happen: timeout while the service is up!". Clamp the conversion to [0, 0x7fffffff] ms.
        struct timespec *ts = (void *)a2;
        // EFAULT on an inaccessible pollfd array (a0, a1=nfds) or timeout (a2), PROT_NONE guard page too.
        if ((a0 && a1 && guest_bad_ptr(a0, (size_t)a1 * sizeof(struct pollfd))) ||
            (ts && guest_bad_ptr(a2, sizeof(struct timespec)))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Linux rejects an out-of-range timeout nanoseconds field (tv_nsec < 0 or >= 1e9) with EINVAL.
        if (ts && (ts->tv_nsec < 0 || ts->tv_nsec >= 1000000000L)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // ppoll(fds, n, tmo, sigmask, sigsetsize): a3 is the guest sigset_t pointer, a4 its size. Apply the
        // temporary signal mask for the duration of the wait (Linux swaps it atomically); NULL a3 = no mask.
        uint64_t sm_set = 0, sm_saved = 0;
        if (a3) {
            if ((size_t)a4 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (guest_bad_ptr(a3, 8)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            sm_set = a3;
        }
        int have_to = ts != NULL;
        // Full requested budget in ms, clamped (see above).
        int tmo_full;
        if (!ts)
            tmo_full = -1; // NULL timespec -> block forever
        else if (ts->tv_sec < 0)
            tmo_full = 0; // past/negative deadline -> don't block
        else if (ts->tv_sec >= 0x7fffffff / 1000)
            tmo_full = 0x7fffffff; // would overflow ms range -> cap (~24.8d)
        else {
            long long ms = (long long)ts->tv_sec * 1000 + ts->tv_nsec / 1000000;
            tmo_full = ms > 0x7fffffff ? 0x7fffffff : (int)ms;
        }
        // like pselect (case 72), a spurious EINTR must re-block only for the REMAINING time, not the
        // full budget again -- otherwise a repeating hooked-but-blocked signal restarts the timeout forever
        // (the select02-class hang) and every finite wait overshoots. Capture the deadline once.
        struct timespec deadline = {0, 0};
        if (have_to && tmo_full > 0) {
            clock_gettime(CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += tmo_full / 1000;
            deadline.tv_nsec += (long)(tmo_full % 1000) * 1000000L;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
        }
        int sm_on = poll_sigmask_enter(c, sm_set, &sm_saved);
        int r;
        for (;;) {
            int tmo = tmo_full;
            if (have_to && tmo_full > 0) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t ms =
                    (int64_t)(deadline.tv_sec - now.tv_sec) * 1000LL + (deadline.tv_nsec - now.tv_nsec) / 1000000LL;
                tmo = ms < 0 ? 0 : (ms > 0x7fffffff ? 0x7fffffff : (int)ms);
            }
            ts_wait_enter();
            r = poll(fds, (nfds_t)a1, tmo);
            ts_wait_leave(); // S while blocked (glibc pause on aarch64 -> ppoll)
            // poll/ppoll is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry).
            if (r < 0 && svc_poll_retry(c)) continue;
            break;
        }
        if (sm_on) poll_sigmask_leave(c, sm_saved);
        // Linux ppoll(2) writes the leftover time back into the timespec (glibc's ppoll wrapper hides it via
        // a local copy, so this is invisible to POSIX callers but correct for the raw syscall).
        if (r >= 0 && have_to) {
            struct timespec left = {0, 0};
            if (tmo_full > 0) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns > 0) {
                    left.tv_sec = (time_t)(ns / 1000000000LL);
                    left.tv_nsec = (long)(ns % 1000000000LL);
                }
            }
            *ts = left;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // signalfd4(fd, mask, sizemask, flags)
    case 74: {
        // signalfd4(2) error surface, in Linux order (LTP signalfd02).
        // (1) EINVAL: the only valid flag bits are SFD_CLOEXEC(0x80000) and SFD_NONBLOCK(0x800).
        if ((int)a3 & ~(int)(0x80000 | 0x800)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (1b) EINVAL: the kernel requires sizemask == sizeof(sigset_t) (8 on the 64-bit ABI). A zero or
        // otherwise wrong sizemask is rejected BEFORE the mask is read (LTP signalfd4_01).
        if ((size_t)a2 != 8) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (1c) EFAULT: a non-null but inaccessible mask pointer must return EFAULT, never fault the engine.
        if (a1 && guest_bad_ptr(a1, 8)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) fd == -1 creates a new signalfd; otherwise it must reference an EXISTING signalfd -- EBADF if
        // it is not an open fd at all, EINVAL if it is open but not one of our signalfds (each signalfd OFD is
        // an independent self-pipe in g_sfd[], tracked by fd number in g_sigfd_slot).
        int sslot = -1;
        if ((int)a0 != -1) {
            if (fcntl((int)a0, F_GETFD) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            // The signalfd read end OR any dup of it (g_sigfd_slot) updates the SAME OFD; Linux accepts a mask
            // update on a dup'd signalfd, so resolve the fd number to its pool slot rather than the original.
            if (!((int)a0 >= 0 && (int)a0 < DD_NFD && g_sigfd_slot[(int)a0])) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            sslot = g_sigfd_slot[(int)a0] - 1;
        }
        // sigset bit (signo-1) -> g_pending bit signo
        uint64_t lm = a1 ? *(uint64_t *)a1 : 0, pm = 0;
        for (int s = 1; s < 64; s++)
            if (lm & (1ull << (s - 1))) pm |= (1ull << s);
        // Create: allocate an INDEPENDENT OFD (its own self-pipe + mask). The read end is the guest's signalfd;
        // the write end is engine-private (relocated out of the guest's low fd range, poked on delivery).
        if (sslot < 0) {
            sslot = sfd_alloc();
            if (sslot < 0) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            int fds[2];
            if (pipe(fds) < 0) {
                g_sfd[sslot].refs = 0; // release the slot
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int wr = fcntl(fds[1], F_DUPFD, 1 << 20); // move the write end clear of the guest's low fds
            if (wr < 0) wr = fcntl(fds[1], F_DUPFD, 64);
            if (wr >= 0) {
                close(fds[1]);
                fds[1] = wr;
            }
            g_sfd[sslot].rd = fds[0];
            g_sfd[sslot].wr = fds[1];
            if (fds[0] >= 0 && fds[0] < DD_NFD) g_sigfd_slot[fds[0]] = (uint8_t)(sslot + 1);
        }
        // Linux signalfd(fd != -1, mask): UPDATE replaces this OFD's mask EXACTLY (a narrowed mask drops the
        // signals it removed). A fresh create sets the new OFD's mask. Masks never cross between OFDs.
        g_sfd[sslot].mask = pm;
        for (int s = 1; s < 64; s++)
            // make sure the host delivers them
            if ((pm & (1ull << s)) && !sig_is_sync(s)) {
                struct sigaction sa;
                memset(&sa, 0, sizeof sa);
                sa.sa_handler = host_sigh;
                sigaction(sig_l2m(s), &sa, NULL);
            }
        int srd = g_sfd[sslot].rd;
        // SFD_CLOEXEC
        if (a3 & 0x80000) fcntl(srd, F_SETFD, FD_CLOEXEC);
        // SFD_NONBLOCK
        if (a3 & 0x800) fcntl(srd, F_SETFL, O_NONBLOCK);
        // An UPDATE (fd != -1) returns the SAME fd the caller passed (Linux), including a dup alias; a fresh
        // create returns this OFD's read end.
        G_RET(c) = (int)a0 != -1 ? a0 : (uint64_t)srd;
        break;
    }
    case 85: {
        // timerfd_create(clockid, flags) -> kqueue. validate args per Linux (LTP timerfd_create01).
        // Only these clocks back a timerfd: REALTIME(0), MONOTONIC(1), BOOTTIME(7) and the ALARM pair
        // (REALTIME_ALARM=8 / BOOTTIME_ALARM=9); anything else (e.g. -1) is -EINVAL. The only valid flag
        // bits are TFD_NONBLOCK(O_NONBLOCK=0x800) and TFD_CLOEXEC(O_CLOEXEC=0x80000); any other bit
        // (e.g. flags=-1) is -EINVAL. (The old code both accepted every clock/flag AND read NONBLOCK from
        // the wrong bit -- 0x1 instead of 0x800 -- so a TFD_NONBLOCK timerfd was left blocking.)
        int clk = (int)a0;
        if (clk != 0 && clk != 1 && clk != 7 && clk != 8 && clk != 9) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if ((int)a1 & ~(int)(0x800 | 0x80000)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int r = kqueue();
        if (r >= 0) {
            if (r < DD_NFD) {
                g_timerfd[r] = 1;
                g_tfd_clock[r] = (int)a0; // remember the clockid for TFD_TIMER_ABSTIME conversion
            }
            if (a1 & 0x800) fcntl(r, F_SETFL, O_NONBLOCK); // TFD_NONBLOCK
            // macOS kqueue() defaults FD_CLOEXEC SET; Linux timerfd_create(...,0) leaves it CLEAR. Set it
            // exactly per TFD_CLOEXEC (clearing the kqueue default otherwise) so a timerfd created without
            // the flag survives exec instead of being swept by dd's close-on-exec pass.
            fcntl(r, F_SETFD, (a1 & 0x80000) ? FD_CLOEXEC : 0);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // timerfd_settime(fd, flags, new, old)
    case 86: {
        struct kevent kv;
        // timerfd_settime(2) error surface, in Linux order (LTP timerfd_settime01).
        // (1) EFAULT: new_value must be a readable itimerspec (the kernel copy_from_user's it first).
        if (guest_bad_ptr((uintptr_t)a2, 32)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) EINVAL: only TFD_TIMER_ABSTIME(1) and TFD_TIMER_CANCEL_ON_SET(2) are valid flag bits.
        if ((int)a1 & ~(int)(1 | 2)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint64_t iv_s = 0, iv_n = 0, vl_s = 0, vl_n = 0;
        memcpy(&iv_s, (void *)a2, 8);
        memcpy(&iv_n, (void *)(a2 + 8), 8);
        memcpy(&vl_s, (void *)(a2 + 16), 8);
        memcpy(&vl_n, (void *)(a2 + 24), 8);
        // (3) EINVAL: itimerspec tv_nsec must be in [0,1e9) and tv_sec non-negative (itimerspec64_valid).
        if (iv_n >= 1000000000ull || vl_n >= 1000000000ull || (int64_t)iv_s < 0 || (int64_t)vl_s < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (4) EBADF if fd is not an open descriptor; EINVAL if it is open but not a timerfd (e.g. a plain
        // file). Our timerfds are dd-tracked kqueues (< DD_NFD, g_timerfd set); a larger valid fd is left to
        // the best-effort path below.
        {
            int fd = (int)a0;
            if (fcntl(fd, F_GETFD) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (fd >= 0 && fd < DD_NFD && !g_timerfd[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // (5) EFAULT: a non-NULL old_value must be writable -- the kernel reports the previous setting there.
        if (a3 && guest_bad_ptr((uintptr_t)a3, 32)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Report the PREVIOUS setting into old_value before re-arming (remaining it_value + it_interval),
        // mirroring timerfd_gettime's math against the stashed deadline.
        if (a3) {
            memset((void *)a3, 0, 32);
            int ofd = (int)a0;
            int64_t odl = (ofd >= 0 && ofd < DD_NFD) ? g_tfd_deadline[ofd] : 0;
            int64_t oiv = (ofd >= 0 && ofd < DD_NFD) ? g_tfd_interval[ofd] : 0;
            if (odl > 0) {
                struct timespec onow;
                clock_gettime(CLOCK_MONOTONIC, &onow);
                int64_t onow_ns = (int64_t)onow.tv_sec * 1000000000LL + onow.tv_nsec;
                int64_t orem = odl - onow_ns;
                if (orem < 0 && oiv > 0) orem += ((-orem) / oiv + 1) * oiv;
                if (orem < 0) orem = 0;
                *(int64_t *)(a3 + 0) = oiv / 1000000000LL;
                *(int64_t *)(a3 + 8) = oiv % 1000000000LL;
                *(int64_t *)(a3 + 16) = orem / 1000000000LL;
                *(int64_t *)(a3 + 24) = orem % 1000000000LL;
            }
        }
        int64_t interval_ns = (int64_t)(iv_s * 1000000000ull + iv_n);
        int64_t value_ns = (int64_t)(vl_s * 1000000000ull + vl_n);
        // itimerspec.it_value==0 disarms (regardless of it_interval), same as Linux.
        if (value_ns <= 0) {
            EV_SET(&kv, 1, EVFILT_TIMER, EV_DELETE, 0, 0, NULL);
            kevent((int)a0, &kv, 1, NULL, 0, NULL);
            if ((int)a0 >= 0 && (int)a0 < DD_NFD) {
                g_tfd_deadline[(int)a0] = 0;
                g_tfd_interval[(int)a0] = 0;
                g_tfd_first_oneshot[(int)a0] = 0;
            }
            G_RET(c) = 0;
            break;
            // disarm
        }
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
        // TFD_TIMER_ABSTIME (flags bit 1): it_value is an ABSOLUTE deadline expressed in the TIMER'S OWN
        // clock. The kqueue EVFILT_TIMER delay is always RELATIVE, so convert by subtracting "now" IN THAT
        // SAME CLOCK -- a CLOCK_REALTIME timerfd's absolute deadline is a realtime epoch value, and
        // subtracting CLOCK_MONOTONIC from it made the delay ~decades (a near-future realtime deadline never
        // fired). A past deadline fires asap (0).
        int64_t first_delay;
        if ((int)a1 & 1) {
            int clkid = ((int)a0 >= 0 && (int)a0 < DD_NFD) ? g_tfd_clock[(int)a0] : 1;
            // Linux CLOCK_REALTIME(0)/REALTIME_ALARM(8) are wall-clock; everything else is monotonic-scale.
            clockid_t tc = (clkid == 0 || clkid == 8) ? CLOCK_REALTIME : CLOCK_MONOTONIC;
            struct timespec tnow;
            clock_gettime(tc, &tnow);
            int64_t tnow_ns = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec;
            first_delay = value_ns - tnow_ns;
        } else {
            first_delay = value_ns;
        }
        if (first_delay < 0) first_delay = 0;
        // Record the absolute next-expiry deadline + interval so timerfd_gettime can report the remaining time.
        if ((int)a0 >= 0 && (int)a0 < DD_NFD) {
            g_tfd_deadline[(int)a0] = now_ns + first_delay;
            g_tfd_interval[(int)a0] = interval_ns;
        }
        // Arm the kqueue. kqueue's EVFILT_TIMER can't express "first at it_value, then every it_interval" in
        // one entry (a recurring EV_ADD fires FIRST only after a full period). Cases:
        //   - one-shot (interval==0): EV_ONESHOT at the relative first delay.
        //   - periodic whose first delay == interval: a plain recurring EV_ADD at the interval (exact, no drift).
        //   - periodic whose first delay DIFFERS from interval: honor Linux by arming a ONE-SHOT at the first
        //     delay and flagging it_first_oneshot; the read() drain (io.c) re-arms the recurring periodic at
        //     the interval once that first tick is consumed. Without this the first expiry was wrongly delayed
        //     to a full interval (a periodic timerfd with a short it_value + long it_interval never fired early).
        int periodic = (iv_s || iv_n);
        int first_distinct = periodic && (first_delay != interval_ns);
        if ((int)a0 >= 0 && (int)a0 < DD_NFD) g_tfd_first_oneshot[(int)a0] = first_distinct ? 1 : 0;
        uint16_t fl = EV_ADD | ((periodic && !first_distinct) ? 0 : EV_ONESHOT);
        int64_t arm_ns = (periodic && !first_distinct) ? interval_ns : first_delay;
        EV_SET(&kv, 1, EVFILT_TIMER, fl, NOTE_NSECONDS, arm_ns, NULL);
        G_RET(c) = kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 87: {
        // timerfd_gettime(fd, curr): report the remaining time to the next expiry (it_value) and the
        // interval (it_interval), computed from the deadline timerfd_settime stashed. A disarmed timer
        // (deadline 0) reports {0,0}; an expired periodic timer reports the time to its next tick.
        // validate the fd FIRST (Linux order) -- EBADF if not open, EINVAL if open but not a timerfd,
        // and only then EFAULT on a bad curr pointer (LTP timerfd_gettime01).
        {
            int fd = (int)a0;
            if (fcntl(fd, F_GETFD) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (fd >= 0 && fd < DD_NFD && !g_timerfd[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        if (a1) {
            if (guest_bad_ptr((uintptr_t)a1, 32)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            memset((void *)a1, 0, 32);
            int fd = (int)a0;
            int64_t deadline = (fd >= 0 && fd < DD_NFD) ? g_tfd_deadline[fd] : 0;
            int64_t interval = (fd >= 0 && fd < DD_NFD) ? g_tfd_interval[fd] : 0;
            if (deadline > 0) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                int64_t rem = deadline - now_ns;
                if (rem < 0 && interval > 0) rem += ((-rem) / interval + 1) * interval; // next periodic tick
                if (rem < 0) rem = 0;
                *(int64_t *)(a1 + 0) = interval / 1000000000LL; // it_interval.tv_sec
                *(int64_t *)(a1 + 8) = interval % 1000000000LL; // it_interval.tv_nsec
                *(int64_t *)(a1 + 16) = rem / 1000000000LL;     // it_value.tv_sec
                *(int64_t *)(a1 + 24) = rem % 1000000000LL;     // it_value.tv_nsec
            }
        }
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
