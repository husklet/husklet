// Extracted from service(): Event loop -- epoll/eventfd2/timerfd/signalfd4/inotify, emulated on macOS
// kqueue/pipes. Returns 1 if nr was handled, 0 otherwise. Included by service.c after service/net.c,
// before service() -- same TU scope (shares io.c/signal.c fd-redirection state).

// struct epoll_event has a DIFFERENT layout per guest arch: x86-64 forces __attribute__((packed)) so it
// is 12 bytes with `data` at offset 4; every other arch (aarch64/asm-generic) leaves it naturally aligned
// at 16 bytes (4 bytes pad after the u32 events, then `data` at offset 8). Derive both from the same
// G_O_DIRECTORY discriminator io.c uses, so epoll_ctl reads `data` and epoll_pwait writes the out-array at
// the stride/offset the guest's libc/runtime expects (Go's netpoller stores a pointer in `data`).
#if G_O_DIRECTORY == 0x10000
#define G_EPEV_SZ 12u   // x86-64 (packed)
#define G_EPEV_DOFF 4u
#else
#define G_EPEV_SZ 16u   // aarch64 / asm-generic
#define G_EPEV_DOFF 8u
#endif

// Edge-triggered "prime" on registration. Linux reports an fd that is ALREADY readable/writable at
// EPOLL_CTL_ADD/MOD time when it is registered EPOLLET -- the registration itself counts as the edge (this
// is how Go's netpoller learns about an accepted connection whose request bytes are already buffered). A
// macOS kqueue EV_CLEAR filter, by contrast, reports only a *subsequent* transition, so an already-ready fd
// is never delivered and a Go HTTP server accepts the connection but never responds. So when we arm an edge
// filter on a fd that currently polls ready, stash a synthetic readiness event here and deliver it on the
// next epoll_wait -- once (edge semantics). Tables are indexed by epoll fd (<1024); larger fds use the
// immediate path and simply don't get primed. Level-triggered fds need no prime (kqueue without EV_CLEAR
// already reports current readiness), so only EPOLLET arms reach here -- level semantics are untouched.
static struct kevent *g_ep_prime[1024];
static int g_ep_primen[1024], g_ep_primecap[1024];

static void ep_prime_push(int ep, uintptr_t ident, int16_t filt, void *udata) {
    if (ep < 0 || ep >= 1024) return;
    struct kevent *a = g_ep_prime[ep];
    for (int i = 0; i < g_ep_primen[ep]; i++)
        if (a[i].ident == ident && a[i].filter == filt) { a[i].udata = udata; return; }
    if (g_ep_primen[ep] >= g_ep_primecap[ep]) {
        int nc = g_ep_primecap[ep] ? g_ep_primecap[ep] * 2 : 8;
        struct kevent *na = realloc(a, (size_t)nc * sizeof *na);
        if (!na) return;
        g_ep_prime[ep] = na; g_ep_primecap[ep] = nc; a = na;
    }
    EV_SET(&a[g_ep_primen[ep]++], ident, filt, 0, 0, 0, udata);
}

// If `fd` currently polls ready for the direction `filt` covers, record a one-shot prime on `ep`.
static void ep_prime_if_ready(int ep, int fd, int16_t filt, void *udata) {
    if (ep < 0 || ep >= 1024 || fd < 0) return;
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
static uint8_t g_ep_wake_armed[1024];          // per epoll fd: EVFILT_USER wake knote installed on its kqueue
static pthread_mutex_t g_ep_mtx = PTHREAD_MUTEX_INITIALIZER;

// #396: per-epoll-instance registered-fd membership (lazily allocated 1024-bit bitmap indexed by the
// watched fd). kqueue silently accepts an EV_ADD of an already-armed filter and an EV_DELETE of an
// absent one, but Linux epoll_ctl returns EEXIST / ENOENT respectively, so track membership to serve
// those (plus EINVAL for adding the epoll fd to itself and EPERM for a regular file / directory). Only
// dd-tracked epoll fds (< 1024, g_epoll set) get this surface -- a dup'd/large epfd keeps the existing
// best-effort immediate path, so correct software's readiness path is byte-unchanged.
static uint8_t *g_ep_member[1024];
static int ep_mem_test(int ep, int fd) {
    if (ep < 0 || ep >= 1024 || fd < 0 || fd >= 1024 || !g_ep_member[ep]) return 0;
    return (g_ep_member[ep][fd >> 3] >> (fd & 7)) & 1;
}
static void ep_mem_set(int ep, int fd, int on) {
    if (ep < 0 || ep >= 1024 || fd < 0 || fd >= 1024) return;
    if (!g_ep_member[ep]) { if (!on) return; g_ep_member[ep] = calloc(1024 / 8, 1); if (!g_ep_member[ep]) return; }
    if (on) g_ep_member[ep][fd >> 3] |= (uint8_t)(1u << (fd & 7));
    else g_ep_member[ep][fd >> 3] &= (uint8_t) ~(1u << (fd & 7));
}
static void ep_mem_clear(int ep) {
    if (ep < 0 || ep >= 1024) return;
    if (g_ep_member[ep]) { free(g_ep_member[ep]); g_ep_member[ep] = NULL; }
}

// Capture g_threaded into the returned token so lock/unlock stay balanced even if a peer thread flips
// g_threaded (0->1 on its first clone) between the two calls. Single-threaded (token 0) takes no lock.
static inline int ep_lock(void) { int lk = g_threaded; if (lk) pthread_mutex_lock(&g_ep_mtx); return lk; }
static inline void ep_unlock(int lk) { if (lk) pthread_mutex_unlock(&g_ep_mtx); }

// Install the one-shot self-wake knote on `ep`'s kqueue (idempotent). EV_CLEAR: a NOTE_TRIGGER is
// auto-consumed on delivery, so a trigger raised while no peer is blocked simply makes that peer's next
// kevent() return immediately -- it re-scans primes and re-blocks, so no wakeup is ever lost.
static void ep_wake_arm(int ep) {
    if (ep < 0 || ep >= 1024 || g_ep_wake_armed[ep]) return;
    struct kevent kv;
    EV_SET(&kv, EP_WAKE_IDENT, EVFILT_USER, EV_ADD | EV_CLEAR, 0, 0, NULL);
    if (kevent(ep, &kv, 1, NULL, 0, NULL) == 0) g_ep_wake_armed[ep] = 1;
}

// Push the deferred changelist to the kernel now (so an fd registered/removed on this thread becomes
// visible to a peer M already blocked in kevent) and, when `wake` is set (interest was added/modified),
// NOTE_TRIGGER the wake knote so that blocked peer returns and re-scans primes for an already-ready fd.
// Caller holds g_ep_mtx. Only used when g_threaded, so the W3E batching still applies single-threaded.
static void ep_flush(int ep, int wake) {
    if (ep < 0 || ep >= 1024) return;
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
    for (int fd = 0; fd < 1024; fd++) {
        if (!(g_epoll[fd] || g_timerfd[fd] || g_inotify[fd])) continue;
        if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) continue; // still a live inherited fd -> leave it
        int kq = kqueue();
        if (kq < 0) continue;
        if (kq != fd) { dup2(kq, fd); close(kq); }
        // the fresh instance carries no registrations: drop this epoll fd's inherited (now-invalid) changelist
        // and prime buffer so a later epoll_ctl/epoll_wait re-arms against the new kqueue, not stale state.
        if (g_ep_chg[fd]) { free(g_ep_chg[fd]); g_ep_chg[fd] = NULL; }
        g_ep_chgn[fd] = g_ep_chgcap[fd] = 0;
        if (g_ep_prime[fd]) { free(g_ep_prime[fd]); g_ep_prime[fd] = NULL; }
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
    // fork() only clones the calling thread: if a peer M held g_ep_mtx (mid epoll_ctl/epoll_wait) at fork
    // time the child inherits it LOCKED with no owner, so its next svc_event ep_lock() deadlocks forever
    // (the go-build compile child hit exactly this after the g_jit_lock fix). The child is single-threaded
    // now, so reinitialising it to unlocked is always correct. (Same fork-unsafe-mutex class as g_jit_lock.)
    pthread_mutex_init(&g_ep_mtx, NULL);
}

static int svc_event(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    switch (nr) {
    // ===================== Event loop — epoll/eventfd/timerfd/signalfd/inotify (macOS kqueue) =====================
    // eventfd2(initval, flags) -> pipe
    case 19: {
        int fds[2];
        if (pipe(fds) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        if (a1 & 0x80000) {
            fcntl(fds[0], F_SETFD, FD_CLOEXEC);
            fcntl(fds[1], F_SETFD, FD_CLOEXEC);
            // EFD_CLOEXEC
        }
        if (a1 & 0x800) {
            fcntl(fds[0], F_SETFL, O_NONBLOCK);
            fcntl(fds[1], F_SETFL, O_NONBLOCK);
            // EFD_NONBLOCK
        }
        // writes to the eventfd go to fds[1]; the counter + sema-flag live alongside.
        if (fds[0] < 1024 && fds[1] < 1024) {
            g_eventfd_peer[fds[0]] = fds[1] + 1;
            g_eventfd_sema[fds[0]] = (a1 & 1) != 0; // EFD_SEMAPHORE
            g_eventfd_count[fds[0]] = a0;           // initval
            if (a0 > 0) {
                char b = 1;
                if (write(fds[1], &b, 1) < 0) {}
            } // make it readable
        }
        G_RET(c) = (uint64_t)fds[0];
        break;
    }
    case 20: {
        // epoll_create1(flags) -> kqueue. Only EPOLL_CLOEXEC (0x80000) is defined; any other bit is
        // rejected with EINVAL by the Linux kernel (LTP epoll_create1_01).
        if (a0 & ~0x80000u) { G_RET(c) = (uint64_t)(-EINVAL); break; }
        int r = kqueue();
        // EPOLL_CLOEXEC
        // macOS kqueue() defaults FD_CLOEXEC SET; Linux epoll_create1(0) leaves it CLEAR. Set it exactly
        // per the EPOLL_CLOEXEC flag (clear when absent) so epoll_create1_01's no-CLOEXEC assertion holds.
        if (r >= 0) fcntl(r, F_SETFD, (a0 & 0x80000) ? FD_CLOEXEC : 0);
        // a reused fd number must start with an empty prime buffer + no stale wake knote + no stale membership
        // (close() doesn't clear ours -- this is how an epoll fd's per-instance state is reset on reuse)
        if (r >= 0 && r < 1024) { g_ep_primen[r] = 0; g_ep_wake_armed[r] = 0; g_epoll[r] = 1; ep_mem_clear(r); }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // epoll_ctl(epfd, op, fd, event) -> kevent
    case 21: {
        int op = (int)a1, fd = (int)a2;
        uint32_t ev = 0;
        uint64_t data = (uint64_t)(unsigned)fd;
        if (a3) {
            ev = *(uint32_t *)a3;
            memcpy(&data, (void *)(a3 + G_EPEV_DOFF), 8);
            // struct epoll_event {u32 events; [pad;] u64 data} -- layout per guest arch (see G_EPEV_*)
        }
        // #396: epoll_ctl error surface (Linux return-value semantics kqueue does not enforce). Confined to a
        // dd-tracked epoll instance (< 1024, g_epoll set) watching an fd < 1024 -- these fire ONLY on inputs
        // that already error on Linux, so correct software's ADD/MOD/DEL readiness path is byte-unchanged.
        if ((int)a0 >= 0 && (int)a0 < 1024 && g_epoll[(int)a0] && fd >= 0 && fd < 1024) {
            int ep = (int)a0;
            if (fd == ep) { G_RET(c) = (uint64_t)(-EINVAL); break; }                    // can't watch the epoll fd itself
            int member = ep_mem_test(ep, fd);
            if (op == 1 && member) { G_RET(c) = (uint64_t)(-EEXIST); break; }            // ADD an already-registered fd
            if ((op == 2 || op == 3) && !member) { G_RET(c) = (uint64_t)(-ENOENT); break; } // MOD/DEL an absent fd
            if (op == 1) {
                struct stat st;
                if (fstat(fd, &st) == 0 && (S_ISREG(st.st_mode) || S_ISDIR(st.st_mode))) { G_RET(c) = (uint64_t)(-EPERM); break; }
            }
            if (op == 1 || op == 3 || op == 2) ep_mem_set(ep, fd, op != 2);             // commit membership
        }
        // op: 1=ADD 2=DEL 3=MOD ; EPOLLET=0x80000000 -> EV_CLEAR ; EPOLLONESHOT=0x40000000 -> EV_ONESHOT
        uint16_t xf = (uint16_t)((ev & 0x80000000u ? EV_CLEAR : 0) | (ev & 0x40000000u ? EV_ONESHOT : 0));
        int want_rd = (op != 2) && (ev & 0x1); // EPOLLIN
        int want_wr = (op != 2) && (ev & 0x4); // EPOLLOUT
        if (epopt_on() && (int)a0 >= 0 && (int)a0 < 1024 && fd >= 0 && fd < 1024) {
            // W3E fast path: track armed filters, defer the change to the next epoll_wait kevent().
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
        if (maxev > 256) maxev = 256;
        if (maxev < 0) maxev = 0;
        struct kevent kv[256];
        uint8_t *out = (uint8_t *)a1;
        int ep = (int)a0;
        int opt = epopt_on() && ep >= 0 && ep < 1024;
        int32_t tmo = (int32_t)a3; // guest timeout ms: <0 = infinite (must NEVER return 0), 0 = poll, >0 = finite
        // #252 regression fix: a cross-thread epoll_ctl fires the internal EVFILT_USER wake knote (#247), which
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
            if (deadline.tv_nsec >= 1000000000L) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000L; }
        }
        int oi = 0;
        for (;;) {
            struct timespec ts, *tp = NULL;
            if (tmo == 0) {
                ts.tv_sec = 0; ts.tv_nsec = 0; tp = &ts; // non-blocking poll
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
            if (lk) { ep_wake_arm(ep); ep_flush(ep, 0); }
            // A pending edge-prime means some fd is ready *now*; don't sleep waiting for a fresh kqueue edge
            // (a Go server's epoll_wait blocks with an infinite timeout) -- poll kqueue and merge the prime in.
            if (opt && g_ep_primen[ep] > 0) { ts.tv_sec = 0; ts.tv_nsec = 0; tp = &ts; }
            // W3E: submit the deferred changelist together with the wait in ONE kevent() syscall (single-threaded);
            // threaded already flushed it above and waits with no changelist.
            struct kevent *chg = (opt && !lk) ? g_ep_chg[ep] : NULL;
            int nchg = (opt && !lk) ? g_ep_chgn[ep] : 0;
            if (lk) ep_unlock(lk);
            int r;
            // #146: epoll_wait is never restarted by a handler -- re-wait only on a SPURIOUS EINTR (nothing to
            // deliver); the moment a guest handler is runnable we return -EINTR and let the dispatcher run it.
            // kevent applies the changelist before blocking, so a retry re-waits only (changes consumed -> none).
            do {
                r = kevent(ep, chg, nchg, kv, maxev, tp);
                ep_count();
                chg = NULL;
                nchg = 0;
            } while (r < 0 && svc_poll_retry(c));
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
                if (opt && kv[i].ident < 1024 && g_ep_os[kv[i].ident]) {
                    if (kv[i].filter == EVFILT_READ)
                        g_ep_rd[kv[i].ident] = 0;
                    else if (kv[i].filter == EVFILT_WRITE)
                        g_ep_wr[kv[i].ident] = 0;
                }
                oi++;
            }
            // Deliver edge-triggered primes that kqueue didn't surface (fds already ready at registration).
            // This is the #247 cross-thread-readiness delivery: a peer M that registered an already-ready fd
            // stashed a prime here, so a wake that carried no kqueue edge still hands the guest the ready fd.
            if (ep >= 0 && ep < 1024 && g_ep_primen[ep] > 0) {
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
                        if (ju == (uint64_t)pk->udata && (jev & pev)) { dup = 1; break; }
                    }
                    if (dup) continue;                                        // kqueue already reported it
                    if (oi >= maxev) { g_ep_prime[ep][kept++] = *pk; continue; } // no room -> keep for next wait
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
        break;
    }
    case 26: {
        // inotify_init1(flags) -> kqueue
        int r = kqueue();
        if (r >= 0) {
            if (r < 1024) g_inotify[r] = 1;
            if (a0 & 0x800) fcntl(r, F_SETFL, O_NONBLOCK);
            if (a0 & 0x80000) fcntl(r, F_SETFD, FD_CLOEXEC);
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
        if (wfd >= 0 && wfd < 1024 && stat(p, &dst) == 0 && S_ISDIR(dst.st_mode)) {
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
        // inotify_rm_watch(fd, wd)
        EV_SET(&kv, (int)a1, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
        kevent((int)a0, &kv, 1, NULL, 0, NULL);
        close((int)a1);
        G_RET(c) = 0;
        break;
    }
    case 72: { // pselect6(nfds, readfds, writefds, exceptfds, timeout(timespec), sigmask) -> pselect.
        // The Linux/macOS fd_set byte-layout is identical (bit N at byte N/8), so pass the sets through.
        int have_to = a4 != 0;
        if (have_to && !host_range_mapped((uintptr_t)a4, sizeof(struct timespec))) {
            G_RET(c) = (uint64_t)(-EFAULT); // faulty timeout pointer -> EFAULT, like the Linux kernel
            break;
        }
        // #396: a spurious EINTR (a signal dd hooks with host_sigh but the guest has BLOCKED or defaults to
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
            if (deadline.tv_nsec >= 1000000000L) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000L; }
        }
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
            r = pselect((int)a0, (fd_set *)a1, (fd_set *)a2, (fd_set *)a3, tsp, NULL);
            // #146: pselect is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry),
            // and then only for the time that remains (recomputed above), never the full budget again.
            if (r < 0 && svc_poll_retry(c)) continue;
            break;
        }
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
        // #290: skalibs iopause (s6-supervise et al.) hands a huge but FINITE relative deadline for an
        // idle-but-up service -- settimeout_infinite() makes the delta exactly tain_infinite_relative,
        // whose tv_sec = 2^61 = 2305843009213693952. On real Linux ppoll takes that timespec and blocks.
        // Here the naive (int)(tv_sec*1000) truncates: 2^61 * 1000 == 0 (mod 2^32), so tmo became 0 ->
        // poll returned immediately -> s6 saw a spurious timeout in the UP state and busy-looped printing
        // "can't happen: timeout while the service is up!". Clamp the conversion to [0, 0x7fffffff] ms.
        struct timespec *ts = (void *)a2;
        if (ts && !host_range_mapped((uintptr_t)a2, sizeof(struct timespec))) {
            G_RET(c) = (uint64_t)(-EFAULT); // faulty timeout pointer -> EFAULT, like the Linux kernel
            break;
        }
        int have_to = ts != NULL;
        // Full requested budget in ms, clamped (see #290 above).
        int tmo_full;
        if (!ts) tmo_full = -1;                                     // NULL timespec -> block forever
        else if (ts->tv_sec < 0) tmo_full = 0;                      // past/negative deadline -> don't block
        else if (ts->tv_sec >= 0x7fffffff / 1000) tmo_full = 0x7fffffff; // would overflow ms range -> cap (~24.8d)
        else {
            long long ms = (long long)ts->tv_sec * 1000 + ts->tv_nsec / 1000000;
            tmo_full = ms > 0x7fffffff ? 0x7fffffff : (int)ms;
        }
        // #396: like pselect (case 72), a spurious EINTR must re-block only for the REMAINING time, not the
        // full budget again -- otherwise a repeating hooked-but-blocked signal restarts the timeout forever
        // (the select02-class hang) and every finite wait overshoots. Capture the deadline once.
        struct timespec deadline = {0, 0};
        if (have_to && tmo_full > 0) {
            clock_gettime(CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += tmo_full / 1000;
            deadline.tv_nsec += (long)(tmo_full % 1000) * 1000000L;
            if (deadline.tv_nsec >= 1000000000L) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000L; }
        }
        int r;
        for (;;) {
            int tmo = tmo_full;
            if (have_to && tmo_full > 0) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t ms = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000LL + (deadline.tv_nsec - now.tv_nsec) / 1000000LL;
                tmo = ms < 0 ? 0 : (ms > 0x7fffffff ? 0x7fffffff : (int)ms);
            }
            r = poll(fds, (nfds_t)a1, tmo);
            // #146: poll/ppoll is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry).
            if (r < 0 && svc_poll_retry(c)) continue;
            break;
        }
        // Linux ppoll(2) writes the leftover time back into the timespec (glibc's ppoll wrapper hides it via
        // a local copy, so this is invisible to POSIX callers but correct for the raw syscall).
        if (r >= 0 && have_to) {
            struct timespec left = {0, 0};
            if (tmo_full > 0) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns > 0) { left.tv_sec = (time_t)(ns / 1000000000LL); left.tv_nsec = (long)(ns % 1000000000LL); }
            }
            *ts = left;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // signalfd4(fd, mask, sizemask, flags)
    case 74: {
        // sigset bit (signo-1) -> g_pending bit signo
        uint64_t lm = a1 ? *(uint64_t *)a1 : 0, pm = 0;
        for (int s = 1; s < 64; s++)
            if (lm & (1ull << (s - 1))) pm |= (1ull << s);
        if (g_sigfd_pipe[0] < 0 && pipe(g_sigfd_pipe) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        g_sigfd_mask |= pm;
        g_sigfd_read = g_sigfd_pipe[0];
        for (int s = 1; s < 64; s++)
            // make sure the host delivers them
            if ((pm & (1ull << s)) && !sig_is_sync(s)) {
                struct sigaction sa;
                memset(&sa, 0, sizeof sa);
                sa.sa_handler = host_sigh;
                sigaction(sig_l2m(s), &sa, NULL);
            }
        // SFD_CLOEXEC
        if (a3 & 0x80000) fcntl(g_sigfd_pipe[0], F_SETFD, FD_CLOEXEC);
        // SFD_NONBLOCK
        if (a3 & 0x800) fcntl(g_sigfd_pipe[0], F_SETFL, O_NONBLOCK);
        G_RET(c) = (uint64_t)g_sigfd_pipe[0];
        break;
    }
    case 85: {
        // timerfd_create(clockid, flags) -> kqueue
        int r = kqueue();
        if (r >= 0) {
            if (r < 1024) g_timerfd[r] = 1;
            if (a1 & 1) fcntl(r, F_SETFL, O_NONBLOCK);
            // TFD_NONBLOCK=1
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // timerfd_settime(fd, flags, new, old)
    case 86: {
        struct kevent kv;
        uint64_t iv_s = 0, iv_n = 0, vl_s = 0, vl_n = 0;
        if (a2) {
            memcpy(&iv_s, (void *)a2, 8);
            memcpy(&iv_n, (void *)(a2 + 8), 8);
            memcpy(&vl_s, (void *)(a2 + 16), 8);
            memcpy(&vl_n, (void *)(a2 + 24), 8);
        }
        // periodic uses it_interval
        int64_t period_ns = (iv_s || iv_n) ? (int64_t)(iv_s * 1000000000ull + iv_n)
                                           // one-shot uses it_value
                                           : (int64_t)(vl_s * 1000000000ull + vl_n);
        int64_t interval_ns = (int64_t)(iv_s * 1000000000ull + iv_n);
        int64_t value_ns = (int64_t)(vl_s * 1000000000ull + vl_n);
        // itimerspec.it_value==0 disarms (regardless of it_interval), same as Linux.
        if (value_ns <= 0 && period_ns <= 0) {
            EV_SET(&kv, 1, EVFILT_TIMER, EV_DELETE, 0, 0, NULL);
            kevent((int)a0, &kv, 1, NULL, 0, NULL);
            if ((int)a0 >= 0 && (int)a0 < 1024) { g_tfd_deadline[(int)a0] = 0; g_tfd_interval[(int)a0] = 0; }
            G_RET(c) = 0;
            break;
            // disarm
        }
        // Record the absolute next-expiry deadline + interval so timerfd_gettime can report the remaining
        // time. TFD_TIMER_ABSTIME (flags bit 1, a1&1) gives an absolute it_value on CLOCK_MONOTONIC; a
        // relative value is now+value. (The kevent below fires on the same delay either way.)
        if ((int)a0 >= 0 && (int)a0 < 1024) {
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
            int64_t first = value_ns > 0 ? value_ns : interval_ns;
            g_tfd_deadline[(int)a0] = ((int)a1 & 1) ? first : now_ns + first; // TFD_TIMER_ABSTIME=1
            g_tfd_interval[(int)a0] = interval_ns;
        }
        // no interval -> one-shot
        uint16_t fl = EV_ADD | ((iv_s || iv_n) ? 0 : EV_ONESHOT);
        EV_SET(&kv, 1, EVFILT_TIMER, fl, NOTE_NSECONDS, period_ns, NULL);
        G_RET(c) = kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 87: {
        // timerfd_gettime(fd, curr): report the remaining time to the next expiry (it_value) and the
        // interval (it_interval), computed from the deadline timerfd_settime stashed. A disarmed timer
        // (deadline 0) reports {0,0}; an expired periodic timer reports the time to its next tick.
        if (a1) {
            if (!host_range_mapped((uintptr_t)a1, 32)) { G_RET(c) = (uint64_t)(-EFAULT); break; }
            memset((void *)a1, 0, 32);
            int fd = (int)a0;
            int64_t deadline = (fd >= 0 && fd < 1024) ? g_tfd_deadline[fd] : 0;
            int64_t interval = (fd >= 0 && fd < 1024) ? g_tfd_interval[fd] : 0;
            if (deadline > 0) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                int64_t rem = deadline - now_ns;
                if (rem < 0 && interval > 0) rem += ((-rem) / interval + 1) * interval; // next periodic tick
                if (rem < 0) rem = 0;
                *(int64_t *)(a1 + 0) = interval / 1000000000LL;  // it_interval.tv_sec
                *(int64_t *)(a1 + 8) = interval % 1000000000LL;  // it_interval.tv_nsec
                *(int64_t *)(a1 + 16) = rem / 1000000000LL;      // it_value.tv_sec
                *(int64_t *)(a1 + 24) = rem % 1000000000LL;      // it_value.tv_nsec
            }
        }
        G_RET(c) = 0;
        break;
    }
    default:
        return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
