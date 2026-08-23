/* Included by event.c: unity-build access with bounded syscall handlers. */

static int svc_signalfd4(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                         uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
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
        uint64_t lm = 0;
        if (a1 && guest_copy_from(&lm, a1, sizeof(lm)) != sizeof(lm)) {
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
            if (!((int)a0 >= 0 && (int)a0 < HL_NFD && g_sigfd_slot[(int)a0])) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            sslot = g_sigfd_slot[(int)a0] - 1;
        }
        // sigset bit (signo-1) -> g_pending bit signo
        uint64_t pm = lm;
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
            if (fds[0] >= 0 && fds[0] < HL_NFD) g_sigfd_slot[fds[0]] = (uint8_t)(sslot + 1);
        }
        // Linux signalfd(fd != -1, mask): UPDATE replaces this OFD's mask EXACTLY (a narrowed mask drops the
        // signals it removed). A fresh create sets the new OFD's mask. Masks never cross between OFDs.
        g_sfd[sslot].mask = pm;
        sfd_refresh_slot(sslot);
        for (int s = 1; s <= 64; s++)
            // make sure the host delivers them
            if ((pm & (UINT64_C(1) << (s - 1))) && !sig_is_sync(s) && !sig_host_is_engine_control(sig_l2m(s))) {
                struct sigaction sa;
                memset(&sa, 0, sizeof sa);
                sa.sa_handler = host_sigh;
                sa.sa_flags = SA_ONSTACK;
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
    default: return 0;
    }
    return svc_done_host(c);
}

static int svc_timerfd_create(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                              uint64_t a4, uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
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
            if (r < HL_NFD) {
                g_timerfd[r] = 1;
                g_epoll_family_seen = 1;
                g_tfd_clock[r] = (int)a0; // remember the clockid for TFD_TIMER_ABSTIME conversion
            }
            if (a1 & 0x800) fcntl(r, F_SETFL, O_NONBLOCK); // TFD_NONBLOCK
            // macOS kqueue() defaults FD_CLOEXEC SET; Linux timerfd_create(...,0) leaves it CLEAR. Set it
            // exactly per TFD_CLOEXEC (clearing the kqueue default otherwise) so a timerfd created without
            // the flag survives exec instead of being swept by hl's close-on-exec pass.
            fcntl(r, F_SETFD, (a1 & 0x80000) ? FD_CLOEXEC : 0);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // timerfd_settime(fd, flags, new, old)
    default: return 0;
    }
    return svc_done_host(c);
}

static int svc_timerfd_settime(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                               uint64_t a4, uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
    case 86: {
        struct kevent kv;
        uint64_t new_value[4];
        uint64_t old_value[4] = {0};
        // timerfd_settime(2) error surface, in Linux order (LTP timerfd_settime01).
        // (1) EFAULT: new_value must be a readable itimerspec (the kernel copy_from_user's it first).
        if (guest_copy_from(new_value, a2, sizeof(new_value)) != sizeof(new_value)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) EINVAL: only TFD_TIMER_ABSTIME(1) and TFD_TIMER_CANCEL_ON_SET(2) are valid flag bits.
        if ((int)a1 & ~(int)(1 | 2)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint64_t iv_s = 0, iv_n = 0, vl_s = 0, vl_n = 0;
        memcpy(&iv_s, new_value + 0, 8);
        memcpy(&iv_n, new_value + 1, 8);
        memcpy(&vl_s, new_value + 2, 8);
        memcpy(&vl_n, new_value + 3, 8);
        // (3) EINVAL: itimerspec tv_nsec must be in [0,1e9) and tv_sec non-negative (itimerspec64_valid).
        if (iv_n >= 1000000000ull || vl_n >= 1000000000ull || (int64_t)iv_s < 0 || (int64_t)vl_s < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (4) EBADF if fd is not an open descriptor; EINVAL if it is open but not a timerfd (e.g. a plain
        // file). Our timerfds are engine-tracked kqueues (< HL_NFD, g_timerfd set); a larger valid fd is left to
        // the best-effort path below.
        {
            int fd = (int)a0;
            if (fcntl(fd, F_GETFD) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (fd >= 0 && fd < HL_NFD && !g_timerfd[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // (5) EFAULT: a non-NULL old_value must be writable -- the kernel reports the previous setting there.
        if (a3 && guest_accessible_prefix(a3, sizeof(old_value), PROT_WRITE) != sizeof(old_value)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Report the PREVIOUS setting into old_value before re-arming (remaining it_value + it_interval),
        // mirroring timerfd_gettime's math against the stashed deadline.
        if (a3) {
            int ofd = (int)a0;
            int64_t odl = (ofd >= 0 && ofd < HL_NFD) ? g_tfd_deadline[ofd] : 0;
            int64_t oiv = (ofd >= 0 && ofd < HL_NFD) ? g_tfd_interval[ofd] : 0;
            if (odl > 0) {
                struct timespec onow;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &onow);
                int64_t onow_ns = (int64_t)onow.tv_sec * 1000000000LL + onow.tv_nsec;
                int64_t orem = odl - onow_ns;
                if (orem < 0 && oiv > 0) orem += ((-orem) / oiv + 1) * oiv;
                if (orem < 0) orem = 0;
                old_value[0] = (uint64_t)(oiv / 1000000000LL);
                old_value[1] = (uint64_t)(oiv % 1000000000LL);
                old_value[2] = (uint64_t)(orem / 1000000000LL);
                old_value[3] = (uint64_t)(orem % 1000000000LL);
            }
            if (guest_copy_to(a3, old_value, sizeof(old_value)) != sizeof(old_value)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        int64_t interval_ns = (int64_t)(iv_s * 1000000000ull + iv_n);
        int64_t value_ns = (int64_t)(vl_s * 1000000000ull + vl_n);
        // itimerspec.it_value==0 disarms (regardless of it_interval), same as Linux.
        if (value_ns <= 0) {
            EV_SET(&kv, 1, EVFILT_TIMER, EV_DELETE, 0, 0, NULL);
            kevent((int)a0, &kv, 1, NULL, 0, NULL);
            if ((int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_tfd_deadline[(int)a0] = 0;
                g_tfd_interval[(int)a0] = 0;
                g_tfd_first_oneshot[(int)a0] = 0;
            }
            G_RET(c) = 0;
            break;
            // disarm
        }
        struct timespec now;
        hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
        int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
        // TFD_TIMER_ABSTIME (flags bit 1): it_value is an ABSOLUTE deadline expressed in the TIMER'S OWN
        // clock. The kqueue EVFILT_TIMER delay is always RELATIVE, so convert by subtracting "now" IN THAT
        // SAME CLOCK -- a CLOCK_REALTIME timerfd's absolute deadline is a realtime epoch value, and
        // subtracting CLOCK_MONOTONIC from it made the delay ~decades (a near-future realtime deadline never
        // fired). A past deadline fires asap (0).
        int64_t first_delay;
        if ((int)a1 & 1) {
            int clkid = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_tfd_clock[(int)a0] : 1;
            // Linux CLOCK_REALTIME(0)/REALTIME_ALARM(8) are wall-clock; everything else is monotonic-scale.
            struct timespec tnow;
            int service_clock =
                (clkid == 0 || clkid == 8) ? HL_PRODUCTION_CLOCK_REALTIME : HL_PRODUCTION_CLOCK_MONOTONIC;
            hl_production_clock_gettime(effective_host_services(), service_clock, &tnow);
            int64_t tnow_ns = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec;
            first_delay = value_ns - tnow_ns;
        } else {
            first_delay = value_ns;
        }
        if (first_delay < 0) first_delay = 0;
        // Record the absolute next-expiry deadline + interval so timerfd_gettime can report the remaining time.
        if ((int)a0 >= 0 && (int)a0 < HL_NFD) {
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
        if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_tfd_first_oneshot[(int)a0] = first_distinct ? 1 : 0;
        uint16_t fl = EV_ADD | ((periodic && !first_distinct) ? 0 : EV_ONESHOT);
        int64_t arm_ns = (periodic && !first_distinct) ? interval_ns : first_delay;
        EV_SET(&kv, 1, EVFILT_TIMER, fl, NOTE_NSECONDS | NOTE_CRITICAL, arm_ns, NULL);
        G_RET(c) = kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    default: return 0;
    }
    return svc_done_host(c);
}

static int svc_timerfd_gettime(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                               uint64_t a4, uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
    case 87: {
        uint64_t current_value[4] = {0};
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
            if (fd >= 0 && fd < HL_NFD && !g_timerfd[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        if (a1) {
            if (guest_accessible_prefix(a1, sizeof(current_value), PROT_WRITE) != sizeof(current_value)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            int fd = (int)a0;
            int64_t deadline = (fd >= 0 && fd < HL_NFD) ? g_tfd_deadline[fd] : 0;
            int64_t interval = (fd >= 0 && fd < HL_NFD) ? g_tfd_interval[fd] : 0;
            if (deadline > 0) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                int64_t rem = deadline - now_ns;
                if (rem < 0 && interval > 0) rem += ((-rem) / interval + 1) * interval; // next periodic tick
                if (rem < 0) rem = 0;
                current_value[0] = (uint64_t)(interval / 1000000000LL);
                current_value[1] = (uint64_t)(interval % 1000000000LL);
                current_value[2] = (uint64_t)(rem / 1000000000LL);
                current_value[3] = (uint64_t)(rem % 1000000000LL);
            }
            if (guest_copy_to(a1, current_value, sizeof(current_value)) != sizeof(current_value)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return svc_done_host(c);
}
