/* Included by io.c: unity-build access with bounded I/O capability handlers. */

static int svc_read_timer(struct cpu *c, int rfd, uint64_t a1, uint64_t a2) {
    if (rfd < 0 || rfd >= HL_NFD || !g_timerfd[rfd]) return 0;
    do {
            // Linux needs an 8-byte buffer; a shorter read is EINVAL and must NOT drain the expiration
            // (checked before the kqueue drain so the pending tick survives an invalid short read).
            if (a2 < 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            int tslot = timerfd_slot(rfd);
            if (tslot < 0 || tslot >= HL_NFD) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (g_tfd_shared[rfd]) {
                struct timerfd_shared_state *shared = g_tfd_shared[rfd];
            timerfd_shared_retry:
                timerfd_shared_lock(shared);
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                uint64_t expirations = shared->pending;
                shared->pending = 0;
                if (shared->deadline > 0 && now_ns >= shared->deadline) {
                    if (shared->interval > 0) {
                        uint64_t elapsed = 1 + (uint64_t)((now_ns - shared->deadline) / shared->interval);
                        expirations += elapsed;
                        shared->deadline += (int64_t)elapsed * shared->interval;
                    } else {
                        expirations += 1;
                        shared->deadline = 0;
                    }
                }
                int64_t next = shared->deadline;
                int64_t interval = shared->interval;
                timerfd_shared_unlock(shared);
                struct kevent stale;
                struct timespec zero = {0, 0};
                (void)kevent(rfd, NULL, 0, &stale, 1, &zero);
                if (expirations != 0) {
                    if (!a1 || guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
                        G_RET(c) = (uint64_t)(-EFAULT);
                        break;
                    }
                    if (next > now_ns) {
                        struct kevent future;
                        EV_SET(&future, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL,
                               next - now_ns, NULL);
                        (void)kevent(rfd, &future, 1, NULL, 0, NULL);
                    }
                    g_tfd_deadline[tslot] = next;
                    g_tfd_interval[tslot] = interval;
                    if (guest_copy_to(a1, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations)) {
                        G_RET(c) = (uint64_t)(-EFAULT);
                        break;
                    }
                    G_RET(c) = 8;
                    break;
                }
                if (g_tfd_nb[rfd]) {
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
                struct kevent wake;
                int waited = kevent(rfd, NULL, 0, &wake, 1, NULL);
                if (waited > 0) goto timerfd_shared_retry;
                G_RET(c) = (uint64_t)(waited < 0 ? -errno : -EAGAIN);
                break;
            }
            if (g_tfd_nb[rfd] && g_tfd_deadline[tslot] == 0 && g_tfd_pending[tslot] == 0) {
                G_RET(c) = (uint64_t)(-EAGAIN);
                break;
            }
            if (g_tfd_pending[tslot] != 0) {
                if (!a1 || guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                uint64_t expirations = g_tfd_pending[tslot];
                g_tfd_pending[tslot] = 0;
                struct kevent pending_event;
                struct timespec zero = {0, 0};
                (void)kevent(rfd, NULL, 0, &pending_event, 1, &zero);
                if (g_tfd_interval[tslot] > 0 && g_tfd_deadline[tslot] > 0) {
                    struct timespec now;
                    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                    int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                    if (now_ns >= g_tfd_deadline[tslot]) {
                        uint64_t elapsed = 1 + (uint64_t)((now_ns - g_tfd_deadline[tslot]) / g_tfd_interval[tslot]);
                        expirations += elapsed;
                        g_tfd_deadline[tslot] += (int64_t)elapsed * g_tfd_interval[tslot];
                    }
                    struct kevent future;
                    int64_t delay = g_tfd_deadline[tslot] - now_ns;
                    if (delay < 0) delay = 0;
                    EV_SET(&future, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL, delay, NULL);
                    (void)kevent(rfd, &future, 1, NULL, 0, NULL);
                    g_tfd_first_oneshot[tslot] = 1;
                }
                if (guest_copy_to(a1, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                G_RET(c) = 8;
                break;
            }
            struct kevent kv;
            struct timespec zero = {0, 0};
            int nb = fcntl(rfd, F_GETFL) & O_NONBLOCK;
            int n = kevent(rfd, NULL, 0, &kv, 1, nb ? &zero : NULL);
            if (n <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(n < 0 ? -errno : -EAGAIN);
                break;
                // EAGAIN
            }
            if (a1 && a2 >= 8) {
                if (guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                /*
                 * EVFILT_TIMER may report several host wakeup quanta when a busy runner services an
                 * overdue EV_ONESHOT. Linux timerfd one-shots have exactly one expiration; only a
                 * periodic timer accumulates multiple expirations.
                 */
                uint64_t expirations = g_tfd_interval[tslot] == 0 ? UINT64_C(1) : (uint64_t)kv.data;
                /*
                 * A distinct first deadline is represented by an EV_ONESHOT.  kqueue can therefore
                 * only report the first expiry, while Linux accumulates every interval that elapsed
                 * before read(2).  Derive that count from the original deadline and keep the next
                 * deadline phase-aligned instead of restarting the period at read time.
                 */
                if (g_tfd_first_oneshot[tslot] && g_tfd_interval[tslot] > 0 && g_tfd_deadline[tslot] > 0) {
                    struct timespec tnow;
                    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &tnow);
                    int64_t now_ns = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec;
                    if (now_ns >= g_tfd_deadline[tslot])
                        expirations = 1 + (uint64_t)((now_ns - g_tfd_deadline[tslot]) / g_tfd_interval[tslot]);
                }
                if (guest_copy_to(a1, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
            }
            // A periodic timerfd whose first expiry (it_value) differed from its interval was armed as a
            // one-shot for that first tick (event.c case 86). Now that the first tick has been consumed,
            // re-arm the recurring periodic at the interval so subsequent expiries fire every it_interval.
            if (g_tfd_first_oneshot[tslot] && g_tfd_interval[tslot] > 0) {
                struct kevent rkv;
                struct timespec tnow;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &tnow);
                int64_t now_ns = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec;
                int64_t next = g_tfd_deadline[tslot];
                if (next <= now_ns) next += ((now_ns - next) / g_tfd_interval[tslot] + 1) * g_tfd_interval[tslot];
                int64_t delay = next - now_ns;
                EV_SET(&rkv, 1, EVFILT_TIMER, EV_ADD | EV_ONESHOT, NOTE_NSECONDS | NOTE_CRITICAL, delay, NULL);
                kevent(rfd, &rkv, 1, NULL, 0, NULL);
                g_tfd_deadline[tslot] = next;
            }
            G_RET(c) = 8;
            break;
    } while (0);
    return 1;
}

static int svc_read_eventfd(struct cpu *c, int rfd, uint64_t a1, uint64_t a2) {
    if (rfd < 0 || rfd >= HL_NFD || !g_eventfd_peer[rfd]) return 0;
    do {
            int eslot = eventfd_counter_slot(rfd);
            if (a2 < 8) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // a1 (the result counter) is written directly below; reject a bad/NULL pointer here, before any
            // side effect (counter reset / pipe drain). Linux read(eventfd, NULL, 8) is EFAULT, not a
            // silent 8-byte success, so a null pointer must fault too (not just an out-of-range one).
            if (!a1 || guest_accessible_prefix(a1, 8, HL_LOGICAL_VMA_WRITE) != 8) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            // The counter read/reset + pipe drain/re-signal is done atomically under g_eventfd_lock so it
            // never races a concurrent write() (which mutates the same counter+pipe pair) -- see the
            // _eventfd-atomicity_ note. The BLOCKING branch (count==0, not O_NONBLOCK) must wait for a
            // writer's byte OUTSIDE the lock (or it would deadlock the very writer that unblocks it), then
            // re-take the lock and re-check.
            pthread_mutex_lock(&g_eventfd_lock);
            while (g_eventfd_count[eslot] == 0) {
                if (eventfd_guest_nb(rfd)) {
                    // Guest asked for non-blocking. No flag toggle is needed either way (the toggle used to
                    // race a cross-process reader — the spurious-EAGAIN bug): where the read end is
                    // host-O_NONBLOCK there is nothing to toggle, and where it is not, the drain below takes
                    // a counted byte rather than probing for one. Drain any stale readiness byte so a
                    // level-triggered epoll won't report the fd ready-forever, then return EAGAIN. The
                    // counter is zero on this branch, so the pair's invariant says no byte is owed and a
                    // host with a blocking read end takes none.
                    eventfd_drain_readiness(rfd, 0);
                    pthread_mutex_unlock(&g_eventfd_lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    goto eventfd_read_done;
                }
                // Guest wants to block. Where the read end is O_NONBLOCK a raw read would EAGAIN rather than
                // wait, so the wait is poll()'s and the read that follows only consumes the byte. Where it is
                // NOT -- a host with no per-descriptor status-flag channel, which is also a host whose poll()
                // over a mixed descriptor set does not exist -- that same one-byte read IS the wait, and it
                // is the right one: it parks the thread on exactly the pipe a writer signals through.
                pthread_mutex_unlock(&g_eventfd_lock);
                if (g_eventfd_readend_nb) {
                    struct pollfd pf = {.fd = rfd, .events = POLLIN, .revents = 0};
                    poll(&pf, 1, -1); // block until a writer signals 0->positive
                }
                char b;
                if (read(rfd, &b, 1) < 0) {} // consume one readiness byte (non-blocking; EAGAIN is fine)
                pthread_mutex_lock(&g_eventfd_lock);
            }
            uint64_t v;
            if (g_eventfd_sema[rfd]) {
                v = 1;
                g_eventfd_count[eslot] -= 1;
            } // EFD_SEMAPHORE: one at a time
            else {
                v = g_eventfd_count[eslot];
                g_eventfd_count[eslot] = 0;
            }
            // re-sync the pipe to "counter > 0": drain it, then re-signal one byte if still positive. Drain
            // directly with no flag toggle either way (no cross-process race). This branch is reached only
            // with a positive counter, so exactly one byte is owed.
            eventfd_drain_readiness(rfd, 1);
            if (g_eventfd_count[eslot] > 0) {
                char b = 1;
                if (write(g_eventfd_peer[rfd] - 1, &b, 1) < 0) {}
            }
            pthread_mutex_unlock(&g_eventfd_lock);
            if (guest_copy_to(a1, &v, sizeof(v)) != (ssize_t)sizeof(v)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = 8;
        eventfd_read_done:
            break;
    } while (0);
    return 1;
}

static int svc_read_inotify(struct cpu *c, int rfd, uint64_t a1, uint64_t a2) {
#if defined(__linux__)
    if (!((rfd >= 0 && rfd < HL_NFD && g_inotify[rfd] && g_inotify_raw_pos[rfd] < g_inotify_raw_len[rfd]))) return 0;
            if (a2 < 16) {
                G_RET(c) = (uint64_t)(-EINVAL);
                return 1;
            }
            if (guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE) != (size_t)a2) {
                G_RET(c) = (uint64_t)(-EFAULT);
                return 1;
            }
            size_t available = g_inotify_raw_len[rfd] - g_inotify_raw_pos[rfd];
            size_t copied = available < (size_t)a2 ? available : (size_t)a2;
            if (guest_copy_to(a1, g_inotify_raw[rfd] + g_inotify_raw_pos[rfd], copied) != (ssize_t)copied) {
                G_RET(c) = (uint64_t)(-EFAULT);
                return 1;
            }
            g_inotify_raw_pos[rfd] += copied;
            if (g_inotify_raw_pos[rfd] == g_inotify_raw_len[rfd]) {
                free(g_inotify_raw[rfd]);
                g_inotify_raw[rfd] = NULL;
                g_inotify_raw_pos[rfd] = g_inotify_raw_len[rfd] = 0;
            }
            G_RET(c) = copied;
            return 1;
#else
    if (!((rfd >= 0 && rfd < 1024 && g_inotify[rfd]))) return 0;
            // Linux needs room for at least one struct inotify_event (16-byte header); a shorter buffer is
            // EINVAL and must NOT consume the queued event (checked before any kqueue drain / snapshot diff).
            if (a2 < 16) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                return 1;
            }
            // The whole [a1, a1+a2) buffer is written directly by the engine below; validate it up front so a
            // bad/unmapped pointer returns -EFAULT (without consuming events) instead of faulting the engine.
            if (guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE) != (size_t)a2) {
                G_RET(c) = (uint64_t)(-EFAULT);
                return 1;
            }
            uint8_t *out = malloc((size_t)a2);
            if (!out) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                return 1;
            }
            size_t off = 0;
            // First drain any queued rename events (IN_MOVED_FROM/IN_MOVED_TO) for this instance; the
            // snapshot diff below can only synthesize IN_CREATE/IN_DELETE, not paired moves.
            off += inomv_drain(rfd, out, (size_t)a2);
            for (int wd = 0; wd < 1024; wd++) {
                if (g_inotify_owner[wd] != rfd || !g_inotify_pending[wd]) continue;
                if (g_inotify_isdir[wd]) {
                    char *cur = dir_snapshot(g_inotify_wpath[wd]);
                    char *old = g_inotify_snap[wd];
                    for (int pass = 0; pass < 2; pass++) {
                        const char *src = pass == 0 ? cur : old, *other = pass == 0 ? old : cur;
                        uint32_t mask = pass == 0 ? 0x100u : 0x200u;
                        for (const char *p = src ? src : ""; *p;) {
                            const char *e = strchr(p, '\n');
                            size_t length = e ? (size_t)(e - p) : strlen(p);
                            if (length && !snap_has(other, p, length) && (g_inotify_mask[wd] & mask)) {
                                size_t nlen = (length + 1 + 15) & ~(size_t)15;
                                if (off + 16 + nlen > a2) break;
                                *(int32_t *)(out + off) = wd;
                                *(uint32_t *)(out + off + 4) = mask;
                                *(uint32_t *)(out + off + 8) = 0;
                                *(uint32_t *)(out + off + 12) = (uint32_t)nlen;
                                memcpy(out + off + 16, p, length);
                                memset(out + off + 16 + length, 0, nlen - length);
                                off += 16 + nlen;
                            }
                            p = e ? e + 1 : p + length;
                        }
                    }
                    free(old);
                    g_inotify_snap[wd] = cur;
                } else if (off + 16 <= a2) {
                    *(int32_t *)(out + off) = wd;
                    *(uint32_t *)(out + off + 4) = g_inotify_pending[wd] & g_inotify_mask[wd];
                    *(uint32_t *)(out + off + 8) = 0;
                    *(uint32_t *)(out + off + 12) = 0;
                    off += 16;
                }
                g_inotify_pending[wd] = 0;
            }
            struct kevent kv[32];
            struct timespec zero = {0, 0};
            int nb = g_inotify_nb[rfd];
            // If we already produced move events, poll the kqueue non-blocking so we never wait behind them.
            int n = kevent(rfd, NULL, 0, kv, 32, (nb || off > 0) ? &zero : NULL);
            if (n <= 0) {
                if (off > 0) {
                    ssize_t copied = guest_copy_to(a1, out, off);
                    G_RET(c) = copied == (ssize_t)off ? (uint64_t)off : (uint64_t)(-EFAULT);
                    free(out);
                    return 1;
                } // return the moves we already have
                G_RET(c) = (uint64_t)(int64_t)(n < 0 ? -errno : -EAGAIN);
                free(out);
                return 1;
            }
            for (int i = 0; i < n; i++) {
                int wd = (int)kv[i].ident;
                if (wd >= 0 && wd < 1024 && g_inotify_isdir[wd]) {
                    // directory watch: diff current entries against the snapshot -> IN_CREATE/IN_DELETE+name
                    char *cur = dir_snapshot(g_inotify_wpath[wd]);
                    char *old = g_inotify_snap[wd];
                    for (int pass = 0; pass < 2; pass++) { // pass 0 = created, pass 1 = deleted
                        const char *src = pass == 0 ? cur : old, *other = pass == 0 ? old : cur;
                        uint32_t mask = pass == 0 ? 0x100u : 0x200u; // IN_CREATE / IN_DELETE
                        for (const char *p = src ? src : ""; *p;) {
                            const char *e = strchr(p, '\n');
                            size_t l = e ? (size_t)(e - p) : strlen(p);
                            if (l && !snap_has(other, p, l) && (g_inotify_mask[wd] & mask)) {
                                size_t nlen = (l + 1 + 15) & ~(size_t)15; // padded name field
                                if (off + 16 + nlen > a2) break;
                                *(int32_t *)(out + off) = wd;
                                *(uint32_t *)(out + off + 4) = mask;
                                *(uint32_t *)(out + off + 8) = 0;               // cookie
                                *(uint32_t *)(out + off + 12) = (uint32_t)nlen; // len
                                memcpy(out + off + 16, p, l);
                                memset(out + off + 16 + l, 0, nlen - l);
                                off += 16 + nlen;
                            }
                            p = e ? e + 1 : p + l;
                        }
                    }
                    free(old);
                    g_inotify_snap[wd] = cur;
                } else {
                    if (off + 16 > a2) break;
                    uint32_t f = kv[i].fflags, m = 0;
                    if (f & (NOTE_WRITE | NOTE_EXTEND)) m |= 0x2; // IN_MODIFY
                    if (f & NOTE_ATTRIB) m |= 0x4;                // IN_ATTRIB
                    if (f & NOTE_DELETE) m |= 0x400;              // IN_DELETE_SELF
                    if (f & NOTE_RENAME) m |= 0x800;              // IN_MOVE_SELF
                    *(int32_t *)(out + off) = wd;
                    *(uint32_t *)(out + off + 4) = m;
                    *(uint32_t *)(out + off + 8) = 0;
                    *(uint32_t *)(out + off + 12) = 0;
                    off += 16;
                }
            }
            ssize_t copied = guest_copy_to(a1, out, off);
            G_RET(c) = copied == (ssize_t)off ? (uint64_t)off : (uint64_t)(-EFAULT);
            free(out);
            return 1;
#endif
    return 1;
}

static int svc_read(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 63: {
        int rfd = (int)a0;
        if (fcntl(rfd, F_GETFL) < 0 && errno == EBADF) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // tee(2) pushback: bytes a prior tee() peeked out of this pipe are re-served here first, in order.
        if (rfd >= 0 && rfd < HL_NFD && g_fd_pb_len[rfd]) {
            size_t want = (size_t)a2;
            size_t accessible = guest_accessible_prefix(a1, want, HL_LOGICAL_VMA_WRITE);
            if (want != 0 && accessible == 0) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            want = accessible;
            void *buffer = malloc(want == 0 ? 1 : want);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            size_t taken = pipe_pushback_take(rfd, buffer, want);
            ssize_t copied = guest_copy_to(a1, buffer, taken);
            free(buffer);
            G_RET(c) = copied == (ssize_t)taken ? taken : copied > 0 ? (uint64_t)copied : (uint64_t)(-EFAULT);
            break;
        }
        // AF_NETLINK socket read: busybox `ip` receives its RTNETLINK dump with read(2)/recvmsg;
        // drain our queued reply with the Linux MSG_PEEK/MSG_TRUNC semantics (see netns.c nl_recv).
        if (nl_is(rfd)) {
            size_t accessible = guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE);
            if (a2 && !accessible) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint8_t *buffer = malloc(accessible ? accessible : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            struct iovec iov = {buffer, accessible};
            int64_t result = nl_recv(rfd, &iov, 1, 0, NULL);
            if (result > 0) {
                size_t produced = (uint64_t)result < accessible ? (size_t)result : accessible;
                ssize_t copied = guest_copy_to(a1, buffer, produced);
                if (copied != (ssize_t)produced) result = copied > 0 ? copied : -EFAULT;
            }
            free(buffer);
            G_RET(c) = (uint64_t)result;
            break;
        }
        // RAM-backed scratch file: serve the read from memory. Unlike a host-fd read (whose kernel copyout
        // faults a bad buffer to EFAULT), this copies straight into the guest buffer, so a bad/unmapped
        // pointer must be validated here or the engine memcpy faults (access_ok).
        if (memf_get(rfd)) {
            size_t accessible = guest_accessible_prefix(a1, (size_t)a2, HL_LOGICAL_VMA_WRITE);
            if (a2 != 0 && accessible == 0) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            void *buffer = malloc(accessible == 0 ? 1 : accessible);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t r = memf_read_pos(g_memf[rfd], buffer, accessible);
            if (r > 0) {
                ssize_t copied = guest_copy_to(a1, buffer, (size_t)r);
                if (copied != r) r = copied > 0 ? copied : -EFAULT;
            }
            free(buffer);
            G_RET(c) = (uint64_t)r;
            break;
        }
        // signalfd read -> struct signalfd_siginfo. Each signalfd OFD has its own self-pipe; the fd number
        // (original OR a dup, both mapped by g_sigfd_slot) is the read end, so read straight from rfd.
        if (rfd >= 0 && rfd < HL_NFD && g_sigfd_slot[rfd]) {
            // Linux needs room for at least one struct signalfd_siginfo (128 bytes); a shorter buffer is
            // EINVAL and must NOT consume a pending signal (checked before draining the wake byte).
            if (a2 < 128) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            // Linux validates the destination before dequeuing the signal.  A
            // failed copyout must leave both the pending instance and the
            // signalfd readability intact so the caller can retry with a valid
            // buffer.
            if (guest_accessible_prefix(a1, 128, HL_LOGICAL_VMA_WRITE) != 128) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            char b;
            // drain one wake byte (one byte was written per queued instance -> keeps readability accurate)
            ssize_t pr = read(rfd, &b, 1);
            if (pr <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(pr < 0 ? -errno : -EAGAIN);
                break;
            }
            // The self-pipe only preserves INSERTION order, but signalfd drains in priority order (lowest
            // signo first, FIFO within a signo) carrying the queued siginfo (ssi_int / ssi_pid / ssi_code).
            // So ignore the byte's signo for SELECTION: scan this OFD's mask ascending for the lowest signo
            // with a queued instance and pop it. Only if nothing is queued (a host async-path wake that set
            // the bit with an empty queue) do we fall back to the byte's signo and the single-slot g_sig*.
            int sslot = g_sigfd_slot[rfd] - 1;
            uint64_t omask = g_sfd[sslot].mask;
            struct sigq_ent ent;
            int sig = 0, popped = 0;
            for (int s = 1; s < 64; s++)
                if ((omask & (1ull << s)) && sigq_pop(s, &ent)) {
                    sig = s;
                    popped = 1;
                    break;
                }
            if (!popped) {
                sig = (unsigned char)b;
                if (sig > 0 && sig < 64) __atomic_and_fetch(&g_pending, ~(1ull << (unsigned)sig), __ATOMIC_SEQ_CST);
            }
            if (a2 >= 128) {
                // a1 is a raw guest buffer we write directly -> EFAULT a bad pointer instead of faulting the engine
                uint8_t info[128] = {0};
                uint32_t signo = (uint32_t)sig, pid = (uint32_t)(popped ? ent.pid : g_sigpid[sig]);
                uint32_t uid = (uint32_t)(popped ? ent.uid : g_siguid[sig]);
                int32_t error = popped ? ent.error : g_sigerror[sig];
                int32_t code = popped ? ent.code : g_sigcode[sig];
                memcpy(info, &signo, sizeof(signo));
                memcpy(info + 4, &error, sizeof(error));
                memcpy(info + 8, &code, sizeof(code));
                memcpy(info + 12, &pid, sizeof(pid));
                memcpy(info + 16, &uid, sizeof(uid));
                uint64_t val = popped ? ent.value : g_sigval[sig];
                int32_t integer = (int32_t)val;
                memcpy(info + 44, &integer, sizeof(integer));
                memcpy(info + 48, &val, sizeof(val));
                if (guest_copy_to(a1, info, sizeof(info)) != (ssize_t)sizeof(info)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                if (!popped) {
                    g_sigerror[sig] = 0;
                    g_sigcode[sig] = 0;
                    g_sigval[sig] = 0;
                    g_sigpid[sig] = 0;
                    g_siguid[sig] = 0;
                }
            }
            G_RET(c) = 128;
            break;
        }
        // inotify read -> struct inotify_event[]
        if (svc_read_inotify(c, rfd, a1, a2)) break;
        // timerfd read -> drain timer, return count
        if (svc_read_timer(c, rfd, a1, a2)) break;
        // eventfd read: return the accumulated counter, reset it, drain the readiness pipe
        if (svc_read_eventfd(c, rfd, a1, a2)) break;
        // /proc/<pid>/pagemap (vfs.c backs it with an empty seekable fd; g_pagemap_fd marks it): synthesize
        // one 64-bit entry per page with the PRESENT bit (63) set. The guest lseek'd to vaddr/pagesize*8 and
        // reads sequentially; advance the real fd offset so its position tracks what we "read" (LTP mmap12).
        if (rfd >= 0 && rfd < HL_NFD && g_pagemap_fd[rfd]) {
            size_t want = (size_t)a2 & ~(size_t)7; // whole 8-byte pagemap entries only
            if (want == 0) {
                G_RET(c) = 0;
                break;
            }
            if (guest_accessible_prefix(a1, want, HL_LOGICAL_VMA_WRITE) != want) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint64_t entries[512];
            size_t done = 0;
            for (size_t i = 0; i < sizeof(entries) / sizeof(entries[0]); ++i)
                entries[i] = UINT64_C(1) << 63;
            while (done < want) {
                size_t chunk = want - done < sizeof(entries) ? want - done : sizeof(entries);
                if (guest_copy_to(a1 + done, entries, chunk) != (ssize_t)chunk) {
                    G_RET(c) = done != 0 ? done : (uint64_t)(-EFAULT);
                    goto pagemap_done;
                }
                done += chunk;
            }
            lseek(rfd, (off_t)want, SEEK_CUR);
            G_RET(c) = (uint64_t)want;
        pagemap_done:
            break;
        }
        // SA_RESTART: a blocking read interrupted by a signal whose guest handler asked for restart is
        // resumed in place (the dispatcher runs the handler after the read finally returns); a handler
        // WITHOUT SA_RESTART lets EINTR through. (Well-behaved programs block in poll/select/epoll -- which
        // always return EINTR -- and only read when ready, so this never defers a needed handler.)
        ssize_t r;
        ts_wait_enter(); // 'S' while a read may block (pipe/socket/tty; a ready/regular fd returns at once)
        do {
            r = guest_fd_read(rfd, a1, (size_t)a2, 0, 0);
        } while (r < 0 && SVC_EINTR_RESTART(c));
        ts_wait_leave();
        // /dev/tty (or /dev/console) tty semantics: a controlling terminal has no EOF-from-emptiness, so a
        // NONBLOCKING read that came back with 0 bytes ("no input") must be EAGAIN, never EOF -- otherwise
        // readline/TUI/event-loop code reads the 0 as terminal closure and tears the terminal down. hl may
        // back /dev/tty with a host device (or /dev/null for console) that returns 0 when empty; remap it.
        if (r == 0 && a2 > 0 && rfd >= 0 && rfd < HL_NFD && g_devtty[rfd]) {
            int fl = fcntl(rfd, F_GETFL);
            if (fl >= 0 && (fl & O_NONBLOCK)) {
                r = -1;
                errno = EAGAIN;
            }
        }
        // SEQPACKET/O_DIRECT-pipe EOF over a DGRAM backing: a peer-closed read reports ECONNRESET, but the
        // emulated endpoint must return 0 (EOF) like the Linux original. (See netns.c / case 199 / pipe2.)
        if (r < 0 && errno == ECONNRESET && seq_is(rfd)) r = 0;
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static void svc_write_host(struct cpu *c, int descriptor, uint64_t address, uint64_t count) {
    hl_fdcache_fd_evict(descriptor);
    int64_t allowed = fsize_gate(c, descriptor, -1, count);
    if (allowed < 0) {
        G_RET(c) = (uint64_t)allowed;
        return;
    }
    ssize_t result;
    do {
        result = guest_fd_write(descriptor, address, (size_t)allowed, 0, 0);
    } while (result < 0 && SVC_EINTR_RESTART(c));
    G_RET(c) = result < 0 ? (uint64_t)(-errno) : (uint64_t)result;
    svc_sigpipe_on_epipe(c, (int64_t)G_RET(c));
}

static int svc_write_sealed(struct cpu *c, int descriptor) {
    if (descriptor < 0 || descriptor >= HL_NFD || !(memfd_seals_fd(descriptor) & 0x8)) return 0;
    G_RET(c) = (uint64_t)(-EPERM);
    return 1;
}

static int svc_write(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 64: {
        int wfd = (int)a0;
        if (fcntl(wfd, F_GETFL) < 0 && errno == EBADF) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // eventfd write is hoisted to the very top of the write handler: an eventfd fd is disjoint from every
        // fd type the checks below probe (oom_score_adj text file, memfd, netlink, dns, icmp, udp, memf), so
        // routing it here changes no behavior -- but it skips the memfd_seals_fd() probe further down, which
        // fstat(2)s the fd on EVERY write to detect a memfd (an eventfd is never cached as one, so it paid a
        // full host fstat per write). This is the eventfd write hot path's dominant non-syscall overhead.
        if (wfd >= 0 && wfd < HL_NFD && g_eventfd_peer[wfd]) {
            int eslot = eventfd_counter_slot(wfd);
            // eventfd_write rejects any size other than exactly 8 (fs/eventfd.c uses count != sizeof).
            // This is asymmetric with the read path, which accepts count >= 8, so a 9- or 16-byte write
            // must NOT be admitted (and must not add to the counter) the way a 9/16-byte read is served.
            if (a2 != 8) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // a1 is a raw guest pointer we read directly -> validate before the deref (covers NULL too)
            uint64_t add;
            if (guest_copy_from(&add, a1, sizeof(add)) != (ssize_t)sizeof(add)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            if (add == 0xffffffffffffffffULL) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Counter bump + pipe re-signal held together under g_eventfd_lock so a concurrent read()'s
            // drain (or a peer write) can never strand the pipe readable-with-count-0 / empty-with-count>0
            // (the event-loop spin / lost-wakeup root cause -- see the _eventfd-atomicity_ note in vfs.c).
            pthread_mutex_lock(&g_eventfd_lock);
            // Linux caps the counter at ULLONG_MAX-1 (0xfffffffffffffffe). A write that would overflow that
            // maximum does NOT wrap: a nonblocking eventfd returns EAGAIN and leaves the counter unchanged
            // (a blocking one sleeps until a reader makes room -- an extreme edge hl does not model, so it
            // also returns EAGAIN rather than silently wrapping the counter to zero and losing wake state).
            if (add > 0xfffffffffffffffeULL - g_eventfd_count[eslot]) {
                pthread_mutex_unlock(&g_eventfd_lock);
                G_RET(c) = (uint64_t)(-EAGAIN);
                break;
            }
            // Captured before the bump: it is the drain's "is a readiness byte outstanding" predicate, and
            // after the bump the counter can no longer answer that question.
            int was_signalled = g_eventfd_count[eslot] > 0;
            g_eventfd_count[eslot] += add;
            // Linux wakes epoll edge-triggered waiters on EVERY write, not just the 0->positive transition.
            // A waker eventfd that is never drained (mio/tokio's cross-thread wakeup) would otherwise lose
            // its 2nd and later wakeups: the backing pipe already holds a byte, so an EV_CLEAR kqueue filter
            // never re-fires and a blocked epoll_wait hangs forever. Drain the pipe to exactly one fresh byte
            // so each write produces a new readable edge, bounded even when the reader never keeps up.
            if (add > 0) {
                // Drain to exactly one fresh byte with no flag toggle. The old toggle mutated the
                // cross-process-shared fd flags and a concurrent reader in another process observed the
                // transient O_NONBLOCK -> spurious EAGAIN.
                eventfd_drain_readiness(wfd, was_signalled);
                char b = 1;
                if (write(g_eventfd_peer[wfd] - 1, &b, 1) < 0) {}
            }
            pthread_mutex_unlock(&g_eventfd_lock);
            G_RET(c) = 8;
            break;
        }
        // /proc/self/oom_score_adj is guest-visible process state, not a
        // writable view of the host process. Parse the complete decimal value
        // and update the synthetic file so reads through this open description
        // and through later opens agree.
        if (wfd >= 0 && wfd < HL_NFD && !strcmp(g_proc_text_desc[wfd], "self:oom_score_adj")) {
            char value[32];
            if (!a2 || a2 >= 32 || guest_copy_from(value, a1, (size_t)a2) != (ssize_t)a2) {
                G_RET(c) = (uint64_t)(a2 ? -EFAULT : 0);
                break;
            }
            value[a2] = 0;
            char *end = NULL;
            errno = 0;
            long parsed = strtol(value, &end, 10);
            while (end && (*end == '\n' || *end == ' ' || *end == '\t'))
                end++;
            if (errno || !end || end == value || *end || parsed < -1000 || parsed > 1000) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            char rendered[32];
            int rendered_size = snprintf(rendered, sizeof rendered, "%ld\n", parsed);
            int replaced = proc_text_replace(wfd, rendered, (size_t)rendered_size);
            if (replaced != 0) {
                G_RET(c) = (uint64_t)replaced;
                break;
            }
            g_proc_oom_score_adj = (int)parsed;
            G_RET(c) = a2;
            break;
        }
        // /proc/self/comm is WRITABLE on Linux: it renames the task exactly as prctl(PR_SET_NAME) does,
        // truncating to TASK_COMM_LEN-1 (15) characters and dropping one trailing newline, and the new name
        // is immediately visible through prctl(PR_GET_NAME) and /proc/self/{comm,status:Name,stat}. Without
        // this the write landed in the synthetic backing file and the task name never changed.
        if (wfd >= 0 && wfd < HL_NFD && !strcmp(g_proc_text_desc[wfd], "self:comm")) {
            if (!a2) {
                int replaced = proc_text_replace(wfd, "\n", 1);
                if (replaced != 0) {
                    G_RET(c) = (uint64_t)replaced;
                    break;
                }
                g_procname[0] = 0;
                set_guest_comm_name("", c->tid == 0);
                if (c->tid == 0) proc_reg_publish_comm();
                G_RET(c) = 0;
                break;
            }
            char name[16];
            size_t take = (size_t)a2 < sizeof name - 1 ? (size_t)a2 : sizeof name - 1;
            if (guest_copy_from(name, a1, take) != (ssize_t)take) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            name[take] = 0;
            char normalized[sizeof g_procname];
            snprintf(normalized, sizeof normalized, "%.15s", name);
            char rendered[32];
            int rendered_size = snprintf(rendered, sizeof rendered, "%s\n", normalized);
            int replaced = proc_text_replace(wfd, rendered, (size_t)rendered_size);
            if (replaced != 0) {
                G_RET(c) = (uint64_t)replaced;
                break;
            }
            memcpy(g_procname, normalized, sizeof g_procname);
            set_guest_comm_name(g_procname, c->tid == 0);
            if (c->tid == 0) proc_reg_publish_comm();
            G_RET(c) = a2; // Linux consumes the whole write even when it truncated the stored name
            break;
        }
        if (svc_write_sealed(c, wfd)) break;
        // AF_NETLINK socket write: busybox `ip` (libbb) sends its RTM_GET* dump request via
        // write(2), NOT sendto/sendmsg -- so the netlink responder (which only hooked send*) never saw
        // it, no dump was queued, and the follow-up recvmsg blocked forever ("container stuck Up").
        // Route it to nl_send so the dump is synthesized exactly as for the send* path.
        if (nl_is(wfd)) {
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            G_RET(c) = copied == (ssize_t)a2 ? (uint64_t)nl_send(wfd, buffer, (size_t)a2)
                                             : (uint64_t)(copied > 0 ? copied : -EFAULT);
            free(buffer);
            break;
        }
        // Container DNS: a query write(2)'d on a DNS socket (TCP DNS via write, or a connected-UDP write) is
        // parsed + answered by the host resolver (net.c/netns.c dns_send); nothing reaches the wire.
        if (wfd >= 0 && wfd < HL_NFD && g_dns_sock[wfd]) {
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            G_RET(c) = copied == (ssize_t)a2 ? (uint64_t)dns_send(wfd, buffer, (size_t)a2, g_sock_stream[wfd])
                                             : (uint64_t)(copied > 0 ? copied : -EFAULT);
            free(buffer);
            break;
        }
        if (wfd >= 0 && wfd < HL_NFD && g_icmp_kind[wfd]) {
            int64_t result;
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            if (copied != (ssize_t)a2) {
                free(buffer);
                G_RET(c) = (uint64_t)(copied > 0 ? copied : -EFAULT);
                break;
            }
            if (icmp_try_send(wfd, buffer, (size_t)a2, NULL, 0, &result)) {
                free(buffer);
                G_RET(c) = (uint64_t)result;
                break;
            }
            free(buffer);
        }
        {
            uint8_t *buffer = malloc(a2 ? (size_t)a2 : 1);
            if (!buffer) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)a2);
            if (copied != (ssize_t)a2) {
                free(buffer);
                // This udp-switch probe bounces the source for EVERY write, ahead of any look at the fd, but
                // Linux resolves the descriptor first (fdget_pos then FMODE_WRITE, both EBADF) -- so
                // write(fd_opened_O_RDONLY, NULL, 1) must be EBADF, not the EFAULT this used to report.
                G_RET(c) = (uint64_t)(copied > 0 ? copied : (guest_fd_rejects(wfd, 0) ? -EBADF : -EFAULT));
                break;
            }
            struct iovec vector = {buffer, (size_t)a2};
            int64_t result;
            if (udp_switch_write(wfd, &vector, 1, &result)) {
                free(buffer);
                G_RET(c) = (uint64_t)result;
                break;
            }
            free(buffer);
        }
        // Validate RAM-backed writes because a host-fd kernel copyin would fault
        // a bad pointer to EFAULT; this engine memcpy would instead crash) --, access_ok.
        if (memf_get(wfd) && memf_room_or_spill(wfd, (off_t)g_memf[wfd]->pos + (off_t)a2)) {
            int64_t allowed = memf_fsize_gate(c, (off_t)g_memf[wfd]->pos, a2); // RLIMIT_FSIZE -> SIGXFSZ/EFBIG
            if (allowed < 0) {
                G_RET(c) = (uint64_t)allowed;
                break;
            }
            void *buffer = malloc(allowed == 0 ? 1 : (size_t)allowed);
            if (buffer == NULL) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            ssize_t copied = guest_copy_from(buffer, a1, (size_t)allowed);
            if (allowed != 0 && copied <= 0) {
                free(buffer);
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            ssize_t r = memf_write_pos(g_memf[wfd], buffer, copied > 0 ? (size_t)copied : 0);
            free(buffer);
            G_RET(c) = (uint64_t)r;
            break;
        }
        svc_write_host(c, wfd, a1, a2);
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}
