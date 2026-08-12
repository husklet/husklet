/* Included by event.c: unity-build access with bounded syscall handlers. */

static int svc_eventfd2(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                      uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 19: {
        // Validate `flags` exactly as Linux (fs/eventfd.c): only EFD_SEMAPHORE(1) | EFD_NONBLOCK(O_NONBLOCK
        // 0x800) | EFD_CLOEXEC(O_CLOEXEC 0x80000) are defined; any other bit -> EINVAL. IPC runtimes
        // EventFDNotifier::KernelSupported() probes eventfd2(0, ~0) and PCHECKs it FAILS with EINVAL/ENOSYS/
        // EPERM; without this the probe succeeded and the caller aborted.
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
        // Record whether the host honoured it. A host with no per-descriptor status-flag channel refuses,
        // and the drain/wait paths in io.c must know: their "read until it stops returning bytes" idiom
        // terminates only on a non-blocking read end, and on a blocking one the first drain of an empty
        // pipe never returns. See g_eventfd_readend_nb.
        g_eventfd_readend_nb = fcntl(fds[0], F_SETFL, O_NONBLOCK) == 0;
        // writes to the eventfd go to fds[1]; the counter + sema-flag live alongside.
        // Defensive: the accumulating-counter arena is bound once per process (eventfd_count_init, from a
        // cold hl_run_linux_guest or the fork-server prewarm parent). If it is somehow still NULL, indexing
        // it would SIGSEGV the process (fatal in a resident fork-server parent) -- report ENOMEM instead.
        if (!g_eventfd_count) {
            close(fds[0]);
            close(peer);
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        if (fds[0] < 0 || fds[0] >= HL_NFD) {
            // No per-fd tracking slot for a host fd past the table: the eventfd would be untrackable AND the
            // dup'd `peer` write end (not stored in g_eventfd_peer) would leak. Close both and report EMFILE,
            // matching the fd-past-cap convention used across io.c/fs.c (host fd table is far larger).
            close(fds[0]);
            close(peer);
            G_RET(c) = (uint64_t)(int64_t)(-EMFILE);
            break;
        }
        g_eventfd_peer[fds[0]] = peer + 1;
        g_eventfd_cslot[fds[0]] = fds[0] + 1;
        g_eventfd_sema[fds[0]] = (a1 & 1) != 0;          // EFD_SEMAPHORE
        eventfd_guest_nb_set(fds[0], (a1 & 0x800) != 0); // OFD-shared guest non-blocking intent
        g_eventfd_count[fds[0]] = a0;                    // initval
        g_eventfd_refs[fds[0]] = 1;                      // one alias (this fd); dup() bumps it (fd_carry_virt)
        if (a0 > 0) {
            char b = 1;
            if (write(peer, &b, 1) < 0) {}
        } // make it readable
        G_RET(c) = (uint64_t)fds[0];
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_epoll_create1(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                      uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
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
        if (r >= 0 && r < HL_NFD) {
            g_ep_provider_generations[r] = ep_provider_next(g_ep_provider_generations[r]);
            ep_object_retire_endpoint(r);
            g_ep_primen[r] = 0;
            g_ep_wake_armed[r] = 0;
            g_epoll[r] = 1;
            g_ep_cslot[r] = (uint16_t)(r + 1);
            g_epoll_family_seen = 1;
            ep_native_retire_epoll(r);
            ep_mem_clear(r);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // epoll_ctl(epfd, op, fd, event) -> kevent
    default: return 0;
    }
    return svc_done(c);
}

static int svc_epoll_ctl(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                      uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 21: {
        int op = (int)a1, fd = (int)a2, epfd = (int)a0;
        uint8_t event[G_EPEV_SZ];
        int registry_ep = epoll_slot(epfd);
        uint32_t ev = 0;
        uint64_t data = (uint64_t)(unsigned)fd;
        // (extends): epoll_ctl(2) full error surface, in the kernel's exact ORDER (LTP
        // epoll_ctl02). kqueue silently accepts bad ops/fds and never faults on a NULL event, so enforce
        // each Linux return explicitly. Every check below fires ONLY on input that already errors on Linux,
        // so a well-formed ADD/MOD/DEL is behaviourally unchanged (it costs one extra fstat for the EBADF/
        // EPERM probe -- the ADD path already did that fstat).
        // (1) EFAULT: ADD(1)/MOD(3) -- any op that "has an event" (op != DEL) -- dereference `event`.
        if (op != 2 && (!a3 || guest_copy_from(event, a3, G_EPEV_SZ) != G_EPEV_SZ)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) EBADF: epfd must be an open fd. A engine-tracked epoll (g_epoll set) is known-valid -> only an
        // untracked epfd is probed (a dup'd/large epoll fd keeps the best-effort immediate path).
        if (!(epfd >= 0 && epfd < HL_NFD && g_epoll[epfd]) && fcntl(epfd, F_GETFD) == -1) {
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
            if (op == 2 && guest_copy_from(event, a3, G_EPEV_SZ) != G_EPEV_SZ) memset(event, 0, sizeof(event));
            memcpy(&ev, event, sizeof(ev));
            memcpy(&data, event + G_EPEV_DOFF, 8);
            // struct epoll_event {u32 events; [pad;] u64 data} -- layout per guest arch (see G_EPEV_*)
        }
        // EPOLLEXCLUSIVE (1<<28) may be specified only at EPOLL_CTL_ADD. Linux (fs/eventpoll.c) rejects it in
        // an EPOLL_CTL_MOD event, and rejects any EPOLL_CTL_MOD of a registration that was ADDed exclusive,
        // both with EINVAL. Checked before the membership ENOENT probe to match the kernel's error order.
        if (op == 3 && ((ev & 0x10000000u) || (epfd >= 0 && epfd < HL_NFD && g_epoll[epfd] && fd >= 0 && fd < HL_NFD &&
                                               (g_ep_events[fd] & 0x10000000u)))) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (7/8/9) EEXIST (ADD an already-registered fd) / ENOENT (MOD|DEL an absent fd) on a engine-tracked epoll
        // instance (membership bitmap). Confined to fd < HL_NFD, matching the readiness path below.
        if (epfd >= 0 && epfd < HL_NFD && g_epoll[epfd] && registry_ep >= 0 && registry_ep < HL_NFD && fd >= 0 &&
            fd < HL_NFD) {
            int ep = registry_ep;
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
        if (epfd >= 0 && epfd < HL_NFD && g_epoll[epfd] && registry_ep >= 0 && registry_ep < HL_NFD && fd >= 0 &&
            fd < HL_NFD) {
            if (op == 2) { // DEL
                g_ep_owner[fd] = 0;
                g_ep_events[fd] = 0;
                g_ep_udata[fd] = 0;
            } else { // ADD / MOD
                g_ep_owner[fd] = registry_ep + 1;
                g_ep_events[fd] = ev;
                g_ep_udata[fd] = data;
            }
            if (ep_native_set(registry_ep, fd, op, ev, data) != 0) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
        }
        // op: 1=ADD 2=DEL 3=MOD ; EPOLLET=0x80000000 -> EV_CLEAR ; EPOLLONESHOT=0x40000000 -> EV_ONESHOT
        uint16_t xf = (uint16_t)((ev & 0x80000000u ? EV_CLEAR : 0) | (ev & 0x40000000u ? EV_ONESHOT : 0));
        int want_rd = (op != 2) && (ev & 0x1); // EPOLLIN
        int want_wr = (op != 2) && (ev & 0x4); // EPOLLOUT
        if (epopt_on() && (int)a0 >= 0 && (int)a0 < HL_NFD && !g_ep_dupd[(int)a0] && fd >= 0 && fd < HL_NFD) {
            // W3E fast path: track armed filters, defer the change to the next epoll_wait kevent(). A dup'd
            // instance is excluded (g_ep_dupd) so its interest is submitted immediately to the shared kqueue.
            int ep = (int)a0;
            int lk = ep_lock();
            if (want_rd) {
                ep_push(ep, fd, EVFILT_READ, EV_ADD | xf, (void *)data);
                g_ep_rd[fd] = 1;
                if (xf & EV_CLEAR)
                    ep_prime_if_ready(ep, fd, EVFILT_READ, (void *)data);
                else if (lk)
                    ep_submit_ready_level(ep, fd, EVFILT_READ, xf, (void *)data);
            } else if (g_ep_rd[fd]) {
                ep_push(ep, fd, EVFILT_READ, EV_DELETE, (void *)data);
                g_ep_rd[fd] = 0;
            }
            if (want_wr) {
                ep_push(ep, fd, EVFILT_WRITE, EV_ADD | xf, (void *)data);
                g_ep_wr[fd] = 1;
                if (xf & EV_CLEAR)
                    ep_prime_if_ready(ep, fd, EVFILT_WRITE, (void *)data);
                else if (lk)
                    ep_submit_ready_level(ep, fd, EVFILT_WRITE, xf, (void *)data);
            } else if (g_ep_wr[fd]) {
                ep_push(ep, fd, EVFILT_WRITE, EV_DELETE, (void *)data);
                g_ep_wr[fd] = 0;
            }
            g_ep_os[fd] = (op != 2 && (ev & 0x40000000u)) ? 1 : 0;
            // Nested epoll: arm member knotes eagerly instead of deferring, since a nested inner epoll is
            // never epoll_wait'd by the guest to consume its changelist. Case A: we just added an inner epoll
            // fd into this outer -> flush the inner (fd) so it starts reporting its members' readiness. Case B:
            // this instance is itself watched by an outer (g_ep_owner) -> flush its own members now.
            if (op != 2 && fd != ep && fd >= 0 && fd < HL_NFD && g_epoll[fd]) ep_submit_changes(fd);
            if (g_ep_owner[ep]) ep_submit_changes(ep);
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
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && fd >= 0 && fd < HL_NFD) {
            g_ep_rd[fd] = want_rd ? 1 : 0;
            g_ep_wr[fd] = want_wr ? 1 : 0;
            g_ep_os[fd] = (op != 2 && (ev & UINT32_C(0x40000000))) ? 1 : 0;
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
    default: return 0;
    }
    return svc_done(c);
}
