/* Included by event.c: unity-build access with bounded syscall handlers. */

static int svc_epoll_pwait(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                      uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
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
        uint64_t sm_set = 0;
        int have_mask = 0;
        if (a4) {
            if ((size_t)a5 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (guest_copy_from(&sm_set, a4, sizeof(sm_set)) != sizeof(sm_set)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            have_mask = 1;
        }
        // guest timeout ms (a3): <0 = infinite, 0 = poll, >0 = finite. Widen to nanoseconds for the
        // shared wait core (which epoll_pwait2 also drives with a sub-ms timespec).
        int32_t tmo_ms = (int32_t)a3;
        int64_t timeout_ns = tmo_ms < 0 ? -1 : (int64_t)tmo_ms * 1000000LL;
        svc_epoll_wait_common(c, (int)a0, a1, maxev, timeout_ns, have_mask, sm_set);
        break;
    }
    // epoll_pwait2(epfd, events, max, timeout(const struct timespec*), sigmask, sigsetsize)
    default: return 0;
    }
    return svc_done_host(c);
}

static int svc_epoll_pwait2(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                      uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 441: {
        // Same as epoll_pwait (case 22) except the timeout is a NANOSECOND-resolution struct timespec at
        // a3 (NULL = block forever) instead of an int millisecond count, so sub-ms waits are honored
        // exactly. tokio/glibc>=2.35 issue this when the kernel supports it; unimplemented it read as
        // ENOSYS and every wait failed. Argument order: a3=timeout*, a4=sigmask*, a5=sigsetsize.
        int maxev = (int)a2;
        // Linux rejects maxevents <= 0 with EINVAL before touching the timeout/sigmask.
        if (maxev <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (maxev > 256) maxev = 256;
        // NULL timeout -> infinite; otherwise read + validate the timespec (tv_nsec in [0,1e9), tv_sec>=0)
        // and fold it to a single nanosecond budget for the shared wait core.
        int64_t timeout_ns = -1;
        if (a3) {
            struct timespec to;
            if (guest_copy_from(&to, a3, sizeof(to)) != sizeof(to)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            if (to.tv_sec < 0 || to.tv_nsec < 0 || to.tv_nsec >= 1000000000L) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            timeout_ns = (int64_t)to.tv_sec * 1000000000LL + to.tv_nsec;
        }
        // sigmask (a4) + sigsetsize (a5): identical contract to epoll_pwait -- size must be 8, mask readable.
        uint64_t sm_set = 0;
        int have_mask = 0;
        if (a4) {
            if ((size_t)a5 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (guest_copy_from(&sm_set, a4, sizeof(sm_set)) != sizeof(sm_set)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            have_mask = 1;
        }
        svc_epoll_wait_common(c, (int)a0, a1, maxev, timeout_ns, have_mask, sm_set);
        break;
    }
    default: return 0;
    }
    return svc_done_host(c);
}
