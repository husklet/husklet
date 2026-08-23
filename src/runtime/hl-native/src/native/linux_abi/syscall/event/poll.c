/* Included by event.c: unity-build access with bounded syscall handlers. */

#if !defined(__linux__)
// macOS poll(2) is ~7us per call WHENEVER IT REPORTS NOTHING READY and ~0.37us when it does; select(2)
// on the SAME descriptors costs 0.22us either way (measured on Darwin 26.3, arm64: pipe read end idle
// poll 7.37us / select 0.23us, ready pipe write end poll 0.36us). An idle guest event loop therefore paid
// ~7us of pure XNU cost per wakeup. Gate the host poll behind a select(2) probe over the same descriptors
// and the same wait: when select reports nothing, poll would report nothing too, so the expensive call is
// skipped entirely; when select reports readiness, poll runs with a zero timeout and takes its FAST path,
// so exact Linux revents (POLLHUP/POLLERR/POLLPRI/POLLRDNORM...) still come from poll and nothing about the
// reported bits changes.
//
// The gate is declined -- and the plain host poll used -- whenever select cannot express the request:
// a descriptor at or above FD_SETSIZE, an events mask with no read/write/priority bit (Linux still reports
// POLLHUP/POLLERR/POLLNVAL for events==0, which empty select sets cannot see), an events mask carrying a
// bit outside the representable group, or a select failure such as EBADF from a closed descriptor (that is
// exactly the POLLNVAL case, which only poll can report per-descriptor).
#define POLL_SELECT_READ (POLLIN | POLLRDNORM | POLLRDBAND)
#define POLL_SELECT_WRITE (POLLOUT | POLLWRNORM | POLLWRBAND)
#define POLL_SELECT_KNOWN (POLL_SELECT_READ | POLL_SELECT_WRITE | POLLPRI)

static int poll_select_gate_ready(struct pollfd *fds, nfds_t n, int timeout_ms, int *decided) {
    fd_set read_set, write_set, except_set;
    int highest = -1;
    nfds_t index;
    int rc;
    struct timeval tv;
    *decided = 0;
    if (fds == NULL) return 0;
    FD_ZERO(&read_set);
    FD_ZERO(&write_set);
    FD_ZERO(&except_set);
    for (index = 0; index < n; ++index) {
        int fd = fds[index].fd;
        short events = fds[index].events;
        if (fd < 0) continue;
        if (fd >= FD_SETSIZE) return 0;
        if ((events & POLL_SELECT_KNOWN) == 0 || (events & ~(short)POLL_SELECT_KNOWN) != 0) return 0;
        if (events & POLL_SELECT_READ) FD_SET(fd, &read_set);
        if (events & POLL_SELECT_WRITE) FD_SET(fd, &write_set);
        if (events & POLLPRI) FD_SET(fd, &except_set);
        if (fd > highest) highest = fd;
    }
    if (highest < 0) return 0; // nothing select could watch: let poll decide (nfds==0, all fds negative)
    tv.tv_sec = timeout_ms / 1000;
    tv.tv_usec = (timeout_ms % 1000) * 1000;
    rc = select(highest + 1, &read_set, &write_set, &except_set, timeout_ms < 0 ? NULL : &tv);
    if (rc < 0) {
        if (errno == EINTR) {
            *decided = 1; // a real interruption: the caller's EINTR handling must see it, not a re-poll
            return -1;
        }
        return 0; // EBADF and friends: fall through to poll so POLLNVAL is reported per descriptor
    }
    if (rc == 0) {
        for (index = 0; index < n; ++index)
            fds[index].revents = 0;
        *decided = 1;
        return 0;
    }
    return 1; // something is ready; the caller polls with a zero timeout and hits the host fast path
}

// Host wait for one poll scan. Semantically identical to poll(fds, n, timeout_ms).
static int poll_host_wait(struct pollfd *fds, nfds_t n, int timeout_ms) {
    for (;;) {
        int decided = 0;
        int gate = poll_select_gate_ready(fds, n, timeout_ms, &decided);
        int r;
        if (decided) return gate;
        if (gate <= 0) return poll(fds, n, timeout_ms); // gate declined: plain host poll, unchanged behaviour
        r = poll(fds, n, 0);
        // An infinite wait must never return 0. If the readiness select saw was consumed between the two
        // calls, wait again rather than reporting a timeout that cannot happen.
        if (r == 0 && timeout_ms < 0) continue;
        return r;
    }
}
#endif

static int svc_pselect6(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
    case 72: { // pselect6(nfds, readfds, writefds, exceptfds, timeout(timespec), sigmask) -> pselect.
        // The Linux/macOS fd_set byte-layout is identical (bit N at byte N/8), so pass the sets through.
        int have_to = a4 != 0;
        fd_set read_set, write_set, except_set;
        fd_set read_requested, write_requested, except_requested;
        fd_set *read_host = NULL, *write_host = NULL, *except_host = NULL;
        struct timespec timeout_value;
        // EFAULT on any inaccessible fd_set / timeout pointer -- incl. a PROT_NONE guard page (LTP's
        // tst_get_bad_addr), which host_range_mapped alone misses since hl force-maps guest anon host-RW.
        int selnfds = (int)a0;
        size_t nb = selnfds > 0 ? ((size_t)selnfds + 7) / 8 : 0;
        if (nb > sizeof(fd_set)) nb = sizeof(fd_set);
        if ((a1 && guest_copy_from(&read_set, a1, nb) != (ssize_t)nb) ||
            (a2 && guest_copy_from(&write_set, a2, nb) != (ssize_t)nb) ||
            (a3 && guest_copy_from(&except_set, a3, nb) != (ssize_t)nb) ||
            (have_to && guest_copy_from(&timeout_value, a4, sizeof(timeout_value)) != sizeof(timeout_value))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (a1) read_host = &read_set;
        if (a2) write_host = &write_set;
        if (a3) except_host = &except_set;
        if (a1) read_requested = read_set;
        if (a2) write_requested = write_set;
        if (a3) except_requested = except_set;
        // Linux rejects an out-of-range timeout nanoseconds field (tv_nsec < 0 or >= 1e9) with EINVAL
        // before waiting; hl must not treat it as a normal timeout and hide the caller bug.
        if (have_to) {
            long tns = timeout_value.tv_nsec;
            if (tns < 0 || tns >= 1000000000L) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            (void)checkpoint_resume_timeout(nr, &timeout_value);
        }
        // pselect6 6th arg (a5): pointer to { const sigset_t *ss; size_t ss_len; }. Resolve the guest sigset
        // address so the wait honours the temporary signal mask Linux swaps in atomically (see
        // poll_sigmask_enter). NULL a5 (or a NULL inner ss) = no temporary mask.
        uint64_t sm_set = 0, sm_saved = 0;
        int have_mask = 0;
        if (a5) {
            uint64_t pk[2];
            if (guest_copy_from(pk, a5, sizeof(pk)) != sizeof(pk)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            uint64_t ssp = pk[0], sslen = pk[1];
            if (ssp) {
                if (sslen != 8) {
                    G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                    break;
                }
                if (guest_copy_from(&sm_set, ssp, sizeof(sm_set)) != sizeof(sm_set)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                have_mask = 1;
            }
        }
        // a spurious EINTR (a signal hl hooks with host_sigh but the guest has BLOCKED or defaults to
        // ignore -- e.g. an LTP heartbeat, or SIGCHLD from a reaped child) interrupts the host pselect but
        // must NOT restart the FULL original timeout: that overshoots the deadline and, under a *repeating*
        // spurious wakeup, never reaches it -> select02 hangs (and every timing sample overshoots -> the
        // select01/poll02/pselect01 tst_timer FAILs). Capture a monotonic deadline once and re-block only
        // for the time that remains, exactly like epoll_pwait (case 22). Linux ALSO writes the leftover time
        // back into the timeout struct (both select(2) and the raw pselect6(2) syscall do), so mirror that.
        struct timespec deadline = {0, 0};
        if (have_to) {
            struct timespec ts = timeout_value;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += ts.tv_sec;
            deadline.tv_nsec += ts.tv_nsec;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
        }
        int sm_on = poll_sigmask_enter(c, have_mask, sm_set, &sm_saved);
        int r;
        for (;;) {
            struct timespec rem, *tsp = NULL;
            if (have_to) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns < 0) ns = 0;
                rem.tv_sec = (time_t)(ns / 1000000000LL);
                rem.tv_nsec = (long)(ns % 1000000000LL);
                tsp = &rem;
            }
            // pselect mutates its sets. Rebuild them before every retry, then
            // merge per-thread signalfd readiness into the host result.
            fd_set sfd_read;
            FD_ZERO(&sfd_read);
            int sfd_pre = a1 ? sfd_select_apply_for_cpu((int)a0, &read_requested, &sfd_read, c) : 0;
            if (a1) read_set = read_requested;
            if (a2) write_set = write_requested;
            if (a3) except_set = except_requested;
            ts_wait_enter();
            struct timespec immediate = {0, 0};
            r = pselect((int)a0, read_host, write_host, except_host, sfd_pre > 0 ? &immediate : tsp, NULL);
            ts_wait_leave(); // S while blocked (glibc pause on aarch64 lands in ppoll below; select here)
            if (r < 0 && errno == EINTR && sfd_any_ready_for_cpu(c)) r = 0;
            if (r >= 0 && a1) r += sfd_select_apply_for_cpu((int)a0, &read_requested, &read_set, c);
            // pselect is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry),
            // and then only for the time that remains (recomputed above), never the full budget again.
            if (r < 0 && svc_poll_retry(c)) continue;
            if (r < 0 && have_to) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                checkpoint_prepare_timeout(c, ns);
            }
            break;
        }
        if (sm_on) poll_sigmask_leave(c, sm_saved);
        if (r >= 0 && have_to) {
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
            if (ns < 0) ns = 0;
            timeout_value.tv_sec = (time_t)(ns / 1000000000LL);
            timeout_value.tv_nsec = (long)(ns % 1000000000LL);
        }
        int copy_fault = 0;
        if (r >= 0) {
            if ((a1 && guest_copy_to(a1, &read_set, nb) != (ssize_t)nb) ||
                (a2 && guest_copy_to(a2, &write_set, nb) != (ssize_t)nb) ||
                (a3 && guest_copy_to(a3, &except_set, nb) != (ssize_t)nb) ||
                (have_to && guest_copy_to(a4, &timeout_value, sizeof(timeout_value)) != sizeof(timeout_value)))
                copy_fault = 1;
        }
        G_RET(c) = copy_fault ? (uint64_t)(int64_t)(-EFAULT) : r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return svc_done_host(c);
}

static int svc_ppoll(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                     uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
    case 73: {
        struct pollfd *fds = NULL;
        struct timespec timeout_value;
        // ppoll -> poll. macOS has no ppoll, so collapse the timespec deadline into poll's int-ms timeout.
        // skalibs iopause (s6-supervise et al.) hands a huge but FINITE relative deadline for an
        // idle-but-up service -- settimeout_infinite() makes the delta exactly tain_infinite_relative,
        // whose tv_sec = 2^61 = 2305843009213693952. On real Linux ppoll takes that timespec and blocks.
        // Here the naive (int)(tv_sec*1000) truncates: 2^61 * 1000 == 0 (mod 2^32), so tmo became 0 ->
        // poll returned immediately -> s6 saw a spurious timeout in the UP state and busy-looped printing
        // "can't happen: timeout while the service is up!". Clamp the conversion to [0, 0x7fffffff] ms.
        struct timespec *ts = a2 ? &timeout_value : NULL;
        // EFAULT on an inaccessible pollfd array (a0, a1=nfds) or timeout (a2), PROT_NONE guard page too.
        if (a1 > SIZE_MAX / sizeof(struct pollfd)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        size_t fds_bytes = (size_t)a1 * sizeof(struct pollfd);
        fds = calloc(a1 ? (size_t)a1 : 1, sizeof(*fds));
        if (!fds) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if ((a0 && a1 && guest_copy_from(fds, a0, fds_bytes) != (ssize_t)fds_bytes) ||
            (ts && guest_copy_from(ts, a2, sizeof(*ts)) != sizeof(*ts))) {
            free(fds);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Linux rejects an out-of-range timeout nanoseconds field (tv_nsec < 0 or >= 1e9) with EINVAL.
        if (ts && (ts->tv_sec < 0 || ts->tv_nsec < 0 || ts->tv_nsec >= 1000000000L)) {
            free(fds);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (ts) (void)checkpoint_resume_timeout(nr, ts);
        // ppoll(fds, n, tmo, sigmask, sigsetsize): a3 is the guest sigset_t pointer, a4 its size. Apply the
        // temporary signal mask for the duration of the wait (Linux swaps it atomically); NULL a3 = no mask.
        uint64_t sm_set = 0, sm_saved = 0;
        int have_mask = 0;
        if (a3) {
            if ((size_t)a4 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                free(fds);
                break;
            }
            if (guest_copy_from(&sm_set, a3, sizeof(sm_set)) != sizeof(sm_set)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                free(fds);
                break;
            }
            have_mask = 1;
        }
        int have_to = ts != NULL;
        // like pselect (case 72), a spurious EINTR must re-block only for the REMAINING time, not the
        // full budget again -- otherwise a repeating hooked-but-blocked signal restarts the timeout forever
        // (the select02-class hang) and every finite wait overshoots. Capture the exact nanosecond deadline;
        // truncating ppoll's timeout to integer milliseconds made sub-ms waits return immediately and every
        // non-integral-ms wait finish early.
        struct timespec deadline = {0, 0};
        if (have_to && ts->tv_sec >= 0) {
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += ts->tv_sec;
            deadline.tv_nsec += ts->tv_nsec;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
        }
        int sm_on = poll_sigmask_enter(c, have_mask, sm_set, &sm_saved);
        int r;
        for (;;) {
            int signalfd_ready = sfd_poll_apply_for_cpu(fds, (nfds_t)a1, c);
            r = socket_poll_error_fixup(fds, (nfds_t)a1, 0);
            if (r > 0) break;
            struct timespec rem = {0, 0};
            if (have_to && ts->tv_sec >= 0) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns > 0) {
                    rem.tv_sec = (time_t)(ns / 1000000000LL);
                    rem.tv_nsec = (long)(ns % 1000000000LL);
                }
            }
            ts_wait_enter();
#if defined(__linux__)
            // Linux already provides the exact relative-timespec primitive.
            // Hand it the complete remaining budget.  An earlier attempt to
            // wake several milliseconds early and spin to the deadline burned
            // a scheduler quantum on every call; repeated timer waits were
            // consequently preempted for about 8ms and became much less
            // accurate than the host ppoll they were trying to improve.
            struct timespec immediate = {0, 0};
            r = ppoll(fds, (nfds_t)a1, signalfd_ready > 0 ? &immediate : have_to ? &rem : NULL, NULL);
#else
            // poll(2) only accepts milliseconds. Round UP so a finite wait
            // never returns before its Linux ppoll deadline.
            int tmo = -1;
            if (have_to) {
                int64_t ns = (int64_t)rem.tv_sec * 1000000000LL + rem.tv_nsec;
                int64_t ms = (ns + 999999LL) / 1000000LL;
                tmo = ms > 0x7fffffff ? 0x7fffffff : (int)ms;
            }
            r = poll_host_wait(fds, (nfds_t)a1, signalfd_ready > 0 ? 0 : tmo);
#endif
            ts_wait_leave(); // S while blocked (glibc pause on aarch64 -> ppoll)
            if (r < 0 && errno == EINTR && sfd_any_ready_for_cpu(c)) r = 0;
            r = socket_poll_error_fixup(fds, (nfds_t)a1, r);
            if (r >= 0) r += sfd_poll_apply_for_cpu(fds, (nfds_t)a1, c);
            if (r == 0 && have_to) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t left =
                    (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (left > 0) continue;
            }
            // poll/ppoll is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry).
            if (r < 0 && svc_poll_retry(c)) continue;
            if (r < 0 && have_to) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                checkpoint_prepare_timeout(c, ns);
            }
            break;
        }
        r = eventfd_poll_writable_fixup(fds, (nfds_t)a1, r);
        if (sm_on) poll_sigmask_leave(c, sm_saved);
        // Linux ppoll(2) writes the leftover time back into the timespec (glibc's ppoll wrapper hides it via
        // a local copy, so this is invisible to POSIX callers but correct for the raw syscall).
        if (r >= 0 && have_to) {
            struct timespec left = {0, 0};
            if (ts->tv_sec >= 0) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns > 0) {
                    left.tv_sec = (time_t)(ns / 1000000000LL);
                    left.tv_nsec = (long)(ns % 1000000000LL);
                }
            }
            *ts = left;
        }
        int copy_fault = r >= 0 && a0 && a1 && guest_copy_to(a0, fds, fds_bytes) != (ssize_t)fds_bytes;
        if (r >= 0 && have_to && guest_copy_to(a2, ts, sizeof(*ts)) != sizeof(*ts)) copy_fault = 1;
        free(fds);
        G_RET(c) = copy_fault ? (uint64_t)(int64_t)(-EFAULT) : r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // signalfd4(fd, mask, sizemask, flags)
    default: return 0;
    }
    return svc_done_host(c);
}
