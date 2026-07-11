// Extracted from service(): the long tail of uncommon/low-traffic syscalls + Docker seccomp-profile
// parity -- bpf/userfaultfd/io_uring/ptrace (-EPERM like the default profile), flock, mq_*, sched_*,
// {get,set}itimer, cap{get,set}, adjtimex, fs id setters, getcpu, readahead, etc. Returns 1 if nr was
// handled, 0 otherwise. Included by service.c AFTER its local helpers (svc_adjtimex/pidfd_*/mq_* it
// calls) and before service() -- same TU scope.

// ---- POSIX message-queue helpers (mq_timed{send,receive} timeout + blocking; backing state in dispatch.c)
#define DD_SI_MESGQ (-3) // Linux si_code SI_MESGQ: an mq_notify(SIGEV_SIGNAL) delivery (what the guest expects)

// Validate an mq_timed{send,receive} abs_timeout argument (a4/a5 depending on the op). Mirrors the kernel
// wrapper's prepare_timeout, which runs BEFORE the fd lookup: EFAULT for an unreadable pointer, EINVAL for
// tv_nsec outside [0,1e9) or tv_sec < 0. A NULL pointer means "block indefinitely" (*have_dl = 0). The
// deadline is against CLOCK_REALTIME (struct __kernel_timespec: two 8-byte longs on LP64).
// EFAULT via gna_hit (the guest's PROT_NONE registry — what LTP's tst_get_bad_addr installs), NOT the
// host_range_mapped probe: that probe reports a valid non-PIE static buffer (a `-static` LTP binary's .bss)
// as unmapped, which would wrongly EFAULT a legitimate on-stack/.bss timespec.
static int mq_check_timeout(uint64_t p, struct timespec *dl, int *have_dl) {
    *have_dl = 0;
    if (!p) return 0;
    if (gna_hit(p, 16)) return -EFAULT;
    const long *ts = (const long *)(uintptr_t)p;
    long sec = ts[0], nsec = ts[1];
    if (nsec < 0 || nsec >= 1000000000L || sec < 0) return -EINVAL;
    dl->tv_sec = sec;
    dl->tv_nsec = nsec;
    *have_dl = 1;
    return 0;
}

// One blocking step for a full/empty queue with O_NONBLOCK clear. Returns 0 to retry the queue op (after a
// short sleep, so a concurrent thread's drain/fill is observed), -ETIMEDOUT once the absolute CLOCK_REALTIME
// deadline has passed, or -EINTR if a signal interrupted the sleep (matches the kernel's EINTR on a blocked
// mq op). have_dl==0 => no deadline => poll indefinitely (faithful to a NULL abs_timeout; in the truly stuck
// single-thread case it blocks exactly as Linux does, since nothing else can change the queue).
static int mq_block_wait(int have_dl, const struct timespec *dl) {
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    struct timespec slice = {0, 2 * 1000 * 1000}; // 2ms poll granularity
    if (have_dl) {
        long ds = dl->tv_sec - now.tv_sec;
        long dn = dl->tv_nsec - now.tv_nsec;
        if (dn < 0) {
            dn += 1000000000L;
            ds--;
        }
        if (ds < 0 || (ds == 0 && dn <= 0)) return -ETIMEDOUT; // deadline reached/passed
        if (ds == 0 && dn < slice.tv_nsec) slice.tv_nsec = dn; // never overshoot the deadline
    }
    if (nanosleep(&slice, NULL) < 0 && errno == EINTR) return -EINTR;
    return 0;
}

static int svc_rare(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    switch (nr) {
    // ===================== seccomp / sandboxing parity =====================
    // Docker's default seccomp profile BLOCKS these with EPERM (they need elevated caps the container
    // lacks); real Linux returns -EPERM, so a probe sees "Operation not permitted". We don't emulate
    // the feature -- replicate the blocked-syscall result so guests that probe for it agree with Linux.
    case 280: // bpf(2)            -- needs CAP_BPF/CAP_SYS_ADMIN
    case 282: // userfaultfd(2)    -- blocked by default profile
    // fanotify_init(262)/fanotify_mark(263): the whole API needs CAP_SYS_ADMIN, which the container lacks.
    // Real Linux returns EPERM (the feature is PRESENT but unprivileged) -- NOT ENOSYS -- so a probe agrees
    // with the kernel. (The qemu-x86_64 oracle lacks the syscall and reports ENOSYS, so the x86 differential
    // treats this as an oracle artifact; the aarch64 native oracle gives EPERM and matches.)
    case 262: // fanotify_init(flags, event_f_flags)
    case 263: // fanotify_mark(fd, flags, mask, dirfd, path)
        G_RET(c) = (uint64_t)(-EPERM);
        break;
    // io_uring: we don't implement it. Return ENOSYS ("absent") not EPERM ("present but blocked"),
    // else runtime probers read EPERM as retryable and retry/hang. All three entry points agree.
    case 425: // io_uring_setup(2)
    case 426: // io_uring_enter(2)
    case 427: // io_uring_register(2)
        G_RET(c) = (uint64_t)(-ENOSYS);
        break;
    // seccomp(2): ENFORCED. We store the guest's classic-BPF program(s) and run a small cBPF interpreter
    // against a real struct seccomp_data on every syscall (os/linux/seccomp.c, gated in service()), honouring
    // the program's return action (ALLOW/ERRNO/KILL_PROCESS/KILL_THREAD/TRAP/TRACE/LOG). SET_MODE_STRICT is
    // likewise enforced (only read/write/exit/rt_sigreturn). Filters are per-thread, stacked, inherited across
    // fork and preserved across dd's in-process execve -- matching Linux's SECCOMP_FILTER semantics.
    case 277: { // seccomp(op, flags, args)
        unsigned op = (unsigned)a0;
        if (op == DD_SECCOMP_SET_MODE_FILTER) {
            G_RET(c) = (uint64_t)(int64_t)seccomp_install_filter(a2, (uint32_t)a1);
        } else if (op == DD_SECCOMP_SET_MODE_STRICT) {
            // strict takes no flags/args (SECCOMP_SET_MODE_STRICT): both must be zero, else -EINVAL.
            G_RET(c) = (a1 || a2) ? (uint64_t)(-EINVAL) : (uint64_t)(int64_t)seccomp_set_strict();
        } else {
            G_RET(c) = (uint64_t)(-EINVAL);
        }
        break;
    }
    // ptrace(2): real in-dd tracer/tracee coordination. dd emulates the ptrace relationship
    // BETWEEN two guest processes (both run translated under dd) over a shared arena keyed on guest pids;
    // see os/linux/syscall/ptrace.c. svc_ptrace sets G_RET (0 / -errno) itself.
    case 117: // ptrace(request, pid, addr, data)
        G_RET(c) = (uint64_t)(int64_t)svc_ptrace(c, a0, a1, a2, a3);
        break;

    case 279: { // memfd_create(name, flags) -> an anonymous file: a tmpfile, unlinked immediately
        // Validate `flags` exactly as Linux (mm/memfd.c): any unknown bit -> EINVAL. Chromium's Mojo
        // ChannelLinux::KernelSupportsUpgradeRequirements() probes memfd_create("", ~0) and PCHECKs the
        // call FAILS with EINVAL/ENOSYS/EPERM; without this the probe SUCCEEDED (fd>=0) and the browser
        // process aborted at channel_linux.cc (Check failed). MFD_CLOEXEC=1 MFD_ALLOW_SEALING=2
        // MFD_HUGETLB=4 MFD_NOEXEC_SEAL=8 MFD_EXEC=16; with MFD_HUGETLB the huge-size log2 bits (0x3f<<26)
        // are also permitted. (mkstemp ignores `name`, so no name-length check is needed here.)
        {
            unsigned mfd_flags = (unsigned)a1;
            unsigned mfd_known = 0x1fu; // CLOEXEC|ALLOW_SEALING|HUGETLB|NOEXEC_SEAL|EXEC
            if (mfd_flags & 4u) mfd_known |= (0x3fu << 26); // MFD_HUGETLB -> size-log2 bits are valid
            if (mfd_flags & ~mfd_known) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
        }
        char tn[] = "/tmp/.ddmemfdXXXXXX";
        int fd = mkstemp(tn);
        if (fd >= 0) {
            unlink(tn);
            if (a1 & 1) fcntl(fd, F_SETFD, FD_CLOEXEC); // MFD_CLOEXEC
            // Track it as a memfd so F_ADD_SEALS/F_GET_SEALS (io.c fcntl) and the F_SEAL_WRITE write-guard
            // apply. Without MFD_ALLOW_SEALING (2) the file is born F_SEAL_SEAL'd -> later F_ADD_SEALS EPERMs,
            // exactly as on Linux.
            if (fd < DD_NFD) {
                g_memfd_is[fd] = 1;
                g_memfd_seal[fd] = (a1 & 2) ? 0 : 0x1 /*F_SEAL_SEAL*/;
                memfd_reg_set_fd(fd, g_memfd_seal[fd]);
            }
        }
        G_RET(c) = fd < 0 ? (uint64_t)(-errno) : (uint64_t)fd;
        break;
    }
    // flock(fd, op): BSD whole-file advisory lock. Serviced on a private companion file (see dd_flock in
    // helpers.c) so it stays INDEPENDENT of fcntl POSIX record locks -- on macOS both would otherwise share
    // one per-vnode lock list and spuriously conflict with each other. Linux LOCK_SH/EX/UN/NB match the host.
    case 32: G_RET(c) = dd_flock((int)a0, (int)a1) < 0 ? (uint64_t)(-errno) : 0; break;
    // close_range(first, last, flags): close every fd in [first,last]. CLOSE_RANGE_CLOEXEC(4) sets
    // FD_CLOEXEC instead of closing; CLOSE_RANGE_UNSHARE(2) (file-table unshare) is a no-op here.
    case 436: {
        unsigned first = (unsigned)a0, last = (unsigned)a1;
        int flags = (int)a2;
        // Linux rejects unknown flag bits with EINVAL (only CLOSE_RANGE_UNSHARE=2 and CLOSE_RANGE_CLOEXEC=4
        // are defined). Validate before touching any fd so an invalid contract never closes/cloexecs fds.
        if (flags & ~(int)(2 | 4)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (first > last) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        long maxfd = sysconf(_SC_OPEN_MAX);
        if (maxfd <= 0 || maxfd > 65536) maxfd = 65536;
        if (last >= (unsigned)maxfd) last = (unsigned)maxfd - 1;
        if (!(flags & 4)) engine_fd_vacate_range(first, last); // relocate engine fds out of the actual-close range
        for (unsigned fd = first; fd <= last; fd++) {
            if (flags & 4) { // CLOSE_RANGE_CLOEXEC
                int fl = fcntl((int)fd, F_GETFD);
                if (fl >= 0) fcntl((int)fd, F_SETFD, fl | FD_CLOEXEC);
            } else {
                if (exec_fd_is_engine((int)fd)) continue; // never close a (relocated) live engine fd
                fd_reset_emul((int)fd); // drop dd's emulation tables so a reused fd number isn't misrouted
                close((int)fd);
            }
        }
        G_RET(c) = 0;
        break;
    }
    // pidfd_open(pid, flags): no macOS pidfd -> back it with a real fd and record the target pid.
    case 434: {
        pid_t pid = (pid_t)a0;
        // Linux validates flags before the pid: only PIDFD_NONBLOCK(0x800, O_NONBLOCK) is defined; any
        // other bit is EINVAL. Without this a probe with a bogus flag gets a usable fd it should not.
        if ((unsigned)a1 & ~(unsigned)0x800u) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (pid <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        // guest-pid namespace: `pid` is a GUEST pid. Resolve it to a container-local HOST pid and require
        // that it name a process INSIDE this container -- a non-member is ESRCH, so a guest can no longer
        // pidfd_open an arbitrary same-user host pid (a sibling engine / the launcher) and then signal it.
        // Store the resolved HOST pid so pidfd_send_signal / waitid(P_PIDFD) target the right process
        // (in particular guest pid 1 -> the init's host pid, not host pid 1 = launchd). Bare (non-container)
        // mode keeps the historical host-pid existence probe.
        pid_t hpid;
        if (pid == container_pid()) {
            hpid = (pid_t)getpid();
        } else if (g_init_hostpid) {
            int h;
            if (!container_gpid_member((int)pid, &h)) {
                G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
                break;
            }
            hpid = (pid_t)h;
        } else {
            if (kill(pid, 0) < 0 && errno == ESRCH) {
                G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
                break;
            }
            hpid = pid;
        }
        int fd = pidfd_make(hpid); // kqueue EVFILT_PROC/NOTE_EXIT so poll/epoll on it wakes on the target's exit
        if (fd < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        G_RET(c) = (uint64_t)fd;
        break;
    }
    // pidfd_send_signal(pidfd, sig, siginfo, flags): resolve the pidfd back to its pid, then deliver.
    // sig 0 is the existence check. Self/own-pgrp signals raise into the guest (mirrors kill, case 129).
    case 424: {
        pid_t pid;
        // Reject unknown flag bits (Linux >=6.9 defines PIDFD_SIGNAL_THREAD=1 / _THREAD_GROUP=2 /
        // _PROCESS_GROUP=4; older kernels require 0). Anything outside that set is EINVAL, validated before
        // delivery so a bad-flags probe never signals the target.
        if ((unsigned)a3 & ~(unsigned)(1 | 2 | 4)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (pidfd_lookup((int)a0, &pid) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        int sig = (int)a1;
        // pidfd_open now stores the resolved HOST pid, so self is our own host pid (not container_pid()).
        if (pid == (int)getpid() || pid <= 0) {
            if (sig != 0) raise_guest_signal(c, sig);
            G_RET(c) = 0;
        } else {
            // guest-pid namespace: reject a pidfd whose target is no longer a live member of this container
            // -> ESRCH (matches a real pidfd to an exited/departed process, and closes the same host-pid
            // authority leak as kill, case 129 -- the pidfd could otherwise deliver to an arbitrary host pid).
            if (g_init_hostpid && !container_host_member((int)pid)) {
                G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
                break;
            }
            // Cross-process: translate Linux->macOS signo (the target dd engine listens on the macOS number;
            // see kill, case 129). Untranslated, a divergent signal (SIGUSR1/2, SIGURG, ...) is lost.
            G_RET(c) = kill(pid, sig_l2m(sig)) < 0 ? (uint64_t)(-errno) : 0;
        }
        break;
    }
    // mq_open(name, oflag, mode, attr): find-or-create the named queue, hand back a real fd bound to it.
    // (glibc already rejects a name that lacks a leading '/' or is empty with EINVAL before the syscall,
    // so the raw-syscall path only has to police ENAMETOOLONG, EFAULT, ENOENT, EEXIST, ENOSPC + a bad attr.)
    case 180: {
        const char *name = (const char *)a0;
        int oflag = (int)a1;
        const long *at = (const long *)a3; // struct mq_attr: {flags, maxmsg, msgsize, curmsgs, ...}
        if (!name) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // ENAMETOOLONG: the name COMPONENT (after the leading '/') must be <= NAME_MAX (255). (glibc already
        // rejects an empty/no-leading-slash name and copies the name in, so the raw path only bounds length.)
        size_t nl = strnlen(name, sizeof g_mqq[0].name + 2);
        size_t comp = (nl && name[0] == '/') ? nl - 1 : nl;
        if (comp > 255) {
            G_RET(c) = (uint64_t)(int64_t)(-ENAMETOOLONG);
            break;
        }
        int qi = mq_find(name);
        if (qi < 0) {
            if (!(oflag & 0x40)) { // O_CREAT
                G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
                break;
            }
            // When creating with an explicit attr, mq_maxmsg and mq_msgsize must both be > 0 else EINVAL
            // (Linux validates the attr only on the create path; an existing queue ignores it).
            if (at && (at[1] <= 0 || at[2] <= 0)) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            for (int i = 0; i < MQ_MAXQ; i++)
                if (!g_mqq[i].used) {
                    qi = i;
                    break;
                }
            if (qi < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-ENOSPC);
                break;
            }
            memset(&g_mqq[qi], 0, sizeof g_mqq[qi]);
            g_mqq[qi].used = 1;
            snprintf(g_mqq[qi].name, sizeof g_mqq[qi].name, "%s", name);
            g_mqq[qi].maxmsg = (at && at[1] > 0) ? at[1] : 10;
            g_mqq[qi].msgsize = (at && at[2] > 0) ? at[2] : 8192;
            if (g_mqq[qi].maxmsg > MQ_MAXMSG) g_mqq[qi].maxmsg = MQ_MAXMSG;
        } else if ((oflag & 0x40) && (oflag & 0x80)) { // O_CREAT | O_EXCL
            G_RET(c) = (uint64_t)(int64_t)(-EEXIST);
            break;
        }
        int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        g_mqq[qi].refs++;
        mq_bind(fd, qi);
        mq_fd_setnb(fd, (oflag & MQ_O_NONBLOCK) != 0); // O_NONBLOCK is a per-descriptor mq_flag
        G_RET(c) = (uint64_t)fd;
        break;
    }
    // mq_unlink(name): mark removed; freed once the last descriptor is gone.
    case 181: {
        int qi = mq_find((const char *)a0);
        if (qi < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
            break;
        }
        g_mqq[qi].unlinked = 1;
        mq_maybe_free(qi);
        G_RET(c) = 0;
        break;
    }
    // mq_timedsend(mqdes, msg, len, prio, abs_timeout): insert highest-priority-first, FIFO within a prio.
    // Errno order mirrors the kernel: abs_timeout EFAULT/EINVAL (validated first, in the wrapper), then prio
    // EINVAL, EBADF, EMSGSIZE, msg-buffer EFAULT, then full-queue handling: O_NONBLOCK -> EAGAIN, else block
    // until space or the abs_timeout expires (-> ETIMEDOUT) / a signal (-> EINTR). Real cross-PROCESS blocking
    // is not emulated (queues aren't shared across fork); a full queue with a blocking descriptor and a NULL
    // timeout therefore blocks forever exactly as Linux would when nothing can drain it.
    case 182: {
        struct timespec dl;
        int have_dl;
        int terr = mq_check_timeout(a4, &dl, &have_dl);
        if (terr) {
            G_RET(c) = (uint64_t)(int64_t)terr;
            break;
        }
        unsigned prio = (unsigned)a3;
        if (prio >= 32768) { // MQ_PRIO_MAX
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int qi = mq_qof((int)a0);
        if (qi < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        struct mq_queue *q = &g_mqq[qi];
        size_t len = (size_t)a2;
        if ((long)len > q->msgsize) {
            G_RET(c) = (uint64_t)(int64_t)(-EMSGSIZE);
            break;
        }
        if (len && gna_hit(a1, len)) { // the kernel copies the message in before enqueue (gna_hit: PIE-safe EFAULT)
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        int nb = mq_fd_nonblock((int)a0);
        int werr = 0;
        while (q->n >= q->maxmsg) { // full: block (unless O_NONBLOCK)
            if (nb) {
                werr = -EAGAIN;
                break;
            }
            werr = mq_block_wait(have_dl, &dl); // 0 retry, -ETIMEDOUT, -EINTR
            if (werr) break;
        }
        if (werr) {
            G_RET(c) = (uint64_t)(int64_t)werr;
            break;
        }
        char *buf = malloc(len ? len : 1);
        if (!buf) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        memcpy(buf, (const void *)a1, len);
        int was_empty = (q->n == 0);
        int pos = q->n;
        while (pos > 0 && q->msg[pos - 1].prio < prio) {
            q->msg[pos] = q->msg[pos - 1];
            pos--;
        }
        q->msg[pos].prio = prio;
        q->msg[pos].len = len;
        q->msg[pos].data = buf;
        q->n++;
        // mq_notify one-shot: a message arriving on a previously-EMPTY queue with a registered notification
        // fires it (SIGEV_SIGNAL raises the signal with an SI_MESGQ siginfo; SIGEV_NONE/THREAD just consume
        // the registration). Real Linux suppresses this if a process is blocked in mq_receive on the queue;
        // the single-process emulation tracks no such blocked receiver, so it always fires on the edge.
        if (was_empty && q->notify_set) {
            if (q->notify_notify == 0 /*SIGEV_SIGNAL*/ && q->notify_signo >= 1 && q->notify_signo <= 64) {
                g_sigcode[q->notify_signo] = DD_SI_MESGQ;
                g_sigval[q->notify_signo] = q->notify_val;
                g_sigpid[q->notify_signo] = container_pid(); // si_pid: the sender (this process, self-notify)
                g_siguid[q->notify_signo] = cuid();          // si_uid
                raise_guest_signal(c, q->notify_signo);
            }
            q->notify_set = 0;
        }
        G_RET(c) = 0;
        break;
    }
    // mq_timedreceive(mqdes, msg, len, prio*, abs_timeout): pop the head (highest priority, oldest first).
    // Same errno order as mq_timedsend: abs_timeout EFAULT/EINVAL, EBADF, EMSGSIZE (buffer < mq_msgsize),
    // then empty-queue handling: O_NONBLOCK -> EAGAIN, else block until a message / deadline / signal.
    case 183: {
        struct timespec dl;
        int have_dl;
        int terr = mq_check_timeout(a4, &dl, &have_dl);
        if (terr) {
            G_RET(c) = (uint64_t)(int64_t)terr;
            break;
        }
        int qi = mq_qof((int)a0);
        if (qi < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        struct mq_queue *q = &g_mqq[qi];
        if ((long)(size_t)a2 < q->msgsize) {
            G_RET(c) = (uint64_t)(int64_t)(-EMSGSIZE);
            break;
        }
        int nb = mq_fd_nonblock((int)a0);
        int werr = 0;
        while (q->n == 0) { // empty: block (unless O_NONBLOCK)
            if (nb) {
                werr = -EAGAIN;
                break;
            }
            werr = mq_block_wait(have_dl, &dl);
            if (werr) break;
        }
        if (werr) {
            G_RET(c) = (uint64_t)(int64_t)werr;
            break;
        }
        struct mq_qmsg m = q->msg[0];
        if (m.len && gna_hit(a1, m.len)) { // don't fault the engine on an unwritable dest; keep the msg
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a3 && gna_hit(a3, sizeof(unsigned))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        for (int j = 1; j < q->n; j++)
            q->msg[j - 1] = q->msg[j];
        q->n--;
        memcpy((void *)a1, m.data, m.len);
        if (a3) *(unsigned *)a3 = m.prio;
        free(m.data);
        G_RET(c) = (uint64_t)m.len;
        break;
    }
    // mq_notify(mqdes, sevp): register/unregister the single one-shot notification the queue fires on its
    // empty->non-empty edge (delivered in mq_timedsend). Errno order matches the kernel: for a non-NULL sevp,
    // EFAULT (unreadable sigevent) then EINVAL (sigev_notify not SIGEV_SIGNAL/NONE/THREAD, or SIGEV_SIGNAL
    // with an invalid signo) are checked BEFORE the fd -> EBADF -> EBUSY (already registered). A NULL sevp
    // removes this process's registration (always 0). SIGEV_THREAD is accepted like the kernel but its glibc
    // helper-thread/netlink callback is not driven in-process, so only the registration/EBUSY semantics hold
    // for it (SIGEV_SIGNAL and SIGEV_NONE are fully emulated).
    case 184: {
        const uint8_t *sev = (const uint8_t *)a1;
        if (sev) {
            if (gna_hit(a1, 16)) { // struct sigevent: sigev_value[8], sigev_signo[4], sigev_notify[4] (PIE-safe)
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            int notify = *(const int *)(sev + 12); // sigev_notify
            if (notify != 0 /*SIGEV_SIGNAL*/ && notify != 1 /*SIGEV_NONE*/ && notify != 2 /*SIGEV_THREAD*/) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            int signo = *(const int *)(sev + 8); // sigev_signo
            if (notify == 0 && (signo < 1 || signo > 64)) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            int qi = mq_qof((int)a0);
            if (qi < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            struct mq_queue *q = &g_mqq[qi];
            if (q->notify_set) {
                G_RET(c) = (uint64_t)(int64_t)(-EBUSY);
                break;
            }
            q->notify_set = 1;
            q->notify_notify = notify;
            q->notify_signo = signo;
            q->notify_val = *(const uint64_t *)(sev + 0); // sigev_value
            q->notify_pid = container_pid();
            G_RET(c) = 0;
        } else {
            // Unregister: drop this process's registration if it owns it (matches the kernel's remove path,
            // which is a no-op returning 0 when nothing is registered).
            int qi = mq_qof((int)a0);
            if (qi < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            g_mqq[qi].notify_set = 0;
            G_RET(c) = 0;
        }
        break;
    }
    // mq_getsetattr(mqdes, newattr, oldattr): report mq_flags(O_NONBLOCK)/maxmsg/msgsize/curmsgs into oldattr
    // (the state BEFORE any change), then, if newattr is set, apply it -- only O_NONBLOCK is settable (the
    // kernel masks mq_flags to O_NONBLOCK and ignores the rest), so this is the mq analogue of F_SETFL.
    case 185: {
        if ((a1 && gna_hit(a1, 32)) || (a2 && gna_hit(a2, 32))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        int qi = mq_qof((int)a0);
        if (qi < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        struct mq_queue *q = &g_mqq[qi];
        if (a2) { // oldattr: current flags/geometry (reported before applying newattr)
            long *o = (long *)a2;
            o[0] = mq_fd_nonblock((int)a0) ? MQ_O_NONBLOCK : 0;
            o[1] = q->maxmsg;
            o[2] = q->msgsize;
            o[3] = q->n;
        }
        if (a1) { // newattr: only mq_flags' O_NONBLOCK bit is honoured
            long nf = ((const long *)a1)[0];
            mq_fd_setnb((int)a0, (nf & MQ_O_NONBLOCK) != 0);
        }
        G_RET(c) = 0;
        break;
    }
    // setsid(): new session / process-group leader
    case 157: {
        pid_t s = setsid();
        G_RET(c) = s < 0 ? (uint64_t)(-errno) : (uint64_t)s;
        break;
    }
    // scheduling: stub with sane SCHED_OTHER values (real-time priorities aren't offered)
    case 118:                      // sched_setparam
    case 119: G_RET(c) = 0; break; // sched_setscheduler -> ok (ignored)
    case 120: G_RET(c) = 0; break; // sched_getscheduler -> SCHED_OTHER(0)
    case 121:
        if (a1) *(int *)a1 = 0;
        G_RET(c) = 0;
        break;                                                 // sched_getparam -> priority 0
    case 125: G_RET(c) = (a0 == 1 || a0 == 2) ? 99 : 0; break; // sched_get_priority_max: FIFO/RR=99 else 0
    case 126: G_RET(c) = (a0 == 1 || a0 == 2) ? 1 : 0; break;  // sched_get_priority_min: FIFO/RR=1 else 0
    case 127:                                                  // sched_rr_get_interval -> a nominal 100ms slice
        if (a1) {
            ((struct timespec *)a1)->tv_sec = 0;
            ((struct timespec *)a1)->tv_nsec = 100000000L;
        }
        G_RET(c) = 0;
        break;
    // mlockall/munlockall. macOS has no mlockall(2), but it DOES have mlock(2), so we wire the pages for real
    // over the guest's tracked mappings instead of only tracking state: MCL_CURRENT host-mlocks every current
    // guest range (mlk_wire_current); MCL_FUTURE arms g_mlock_future so each fresh mmap (mem.c case 222) is
    // wired on creation. The lock STATE stays reported via /proc VmLck:/smaps Locked: (g_mlock_all; LTP
    // munlockall01). `flags` is validated exactly as Linux (mm/mlock.c): flags==0, any unknown bit, or
    // MCL_ONFAULT without MCL_CURRENT|MCL_FUTURE -> EINVAL. (MCL_CURRENT=1 MCL_FUTURE=2 MCL_ONFAULT=4.)
    // RESIDUAL: a range the host mlock refuses (RLIMIT_MEMLOCK) is left pageable and the call still succeeds
    // (Linux would ENOMEM) -- best-effort wiring with honest per-range residency; see syscall-compat.md.
    case 230: {
        unsigned f = (unsigned)a0, known = 1u | 2u | 4u;
        if (f == 0 || (f & ~known) || ((f & 4u) && !(f & (1u | 2u)))) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // RLIMIT_MEMLOCK (container is unprivileged, no CAP_IPC_LOCK): a soft limit of 0 refuses any lock
        // (can_do_mlock -> EPERM); with MCL_CURRENT, wiring the whole address space past the limit -> ENOMEM.
        int rl = mlk_rlimit_gate_all();
        if (rl < 0 && ((f & 1u) || rl == -EPERM)) { // ENOMEM only bites MCL_CURRENT; EPERM bites either
            G_RET(c) = (uint64_t)(int64_t)rl;
            break;
        }
        if (f & 1u) mlk_wire_current();  // MCL_CURRENT: wire every existing mapping resident now
        if (f & 2u) g_mlock_future = 1;  // MCL_FUTURE: wire mappings created from here on (mem.c case 222)
        g_mlock_all = 1;
        G_RET(c) = 0;
        break;
    }
    case 231:
        mlk_unwire_all(); // drop the real host wiring before clearing the tracked state
        mlk_reset();
        G_RET(c) = 0;
        break;
    // NUMA memory-policy syscalls (mbind/{set,get}_mempolicy/migrate_pages/move_pages). The host is a
    // single NUMA node and these are advisory placement hints, so accept them as permissive no-ops --
    // e.g. R/OpenBLAS calls mbind(2) on its large matrix buffers at startup. (arm64-normalized numbers;
    // x86-64 237/238/239/256/279 are mapped to these by sysmap.h.)
    case 235: G_RET(c) = 0; break; // mbind          -> success, no-op
    case 237: G_RET(c) = 0; break; // set_mempolicy  -> success, no-op
    case 238: G_RET(c) = 0; break; // migrate_pages  -> success, no-op (single NUMA node, nothing to move)
    // move_pages(pid, count, pages, nodes, status, flags). Single NUMA node here, so nothing ever migrates
    // -- but returning 0 while leaving status[] UNTOUCHED makes a NUMA-introspection guest (numactl --show,
    // libnuma) read an uninitialized buffer. QUERY mode (nodes==NULL) must fill status[] with each page's
    // current node; a real migration request (nodes!=NULL) still writes the resulting node into status[] if
    // one was given. Report node 0 for a mapped/present page, -ENOENT for one not present -- exactly Linux.
    case 239: {
        unsigned long count = (unsigned long)a1;
        void **pages = (void **)a2;
        int *status = (int *)a4;
        if (count == 0) {
            G_RET(c) = 0;
            break;
        }
        if (count > SIZE_MAX / sizeof(void *)) { // guard the size math against a bogus count
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (!pages || !host_range_mapped((uintptr_t)pages, count * sizeof(void *)) ||
            (status && !host_range_mapped((uintptr_t)status, count * sizeof(int)))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (status)
            for (unsigned long i = 0; i < count; i++)
                status[i] = host_addr_mapped((uintptr_t)pages[i]) ? 0 : -ENOENT;
        G_RET(c) = 0;
        break;
    }
    case 450:
        G_RET(c) = 0;
        break; // set_mempolicy_home_node -> success, no-op (same NUMA-hint family)
    // get_mempolicy(mode*, nodemask, maxnode, addr, flags): report the default policy. If the guest
    // passed a mode pointer, write MPOL_DEFAULT(0) -- but validate it first (host_addr_mapped, thread.c)
    // so a bad pointer returns -EFAULT to the guest rather than faulting the engine.
    case 236: {
        int *mode = (int *)a0;
        if (mode) {
            if (!host_addr_mapped((uintptr_t)mode)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            *mode = 0; // MPOL_DEFAULT
        }
        G_RET(c) = 0;
        break;
    }
    // getitimer/setitimer: wrap the host (ITIMER_* + struct itimerval layouts match Linux<->macOS)
    case 102: G_RET(c) = getitimer((int)a0, (struct itimerval *)a1) < 0 ? (uint64_t)(-errno) : 0; break;
    case 103:
        G_RET(c) =
            setitimer((int)a0, (const struct itimerval *)a1, (struct itimerval *)a2) < 0 ? (uint64_t)(-errno) : 0;
        break;
    case 112: G_RET(c) = (uint64_t)(-1); break; // clock_settime: container has no CAP_SYS_TIME -> EPERM
    case 143: G_RET(c) = setregid((gid_t)a0, (gid_t)a1) < 0 ? (uint64_t)(-errno) : 0; break; // setregid
    case 151: G_RET(c) = (uint64_t)cuid(); break; // setfsuid -> previous fsuid (container uid)
    case 152:
        G_RET(c) = (uint64_t)cgid();
        break; // setfsgid -> previous fsgid
    // getcpu(cpu, node, tcache): report a CPU from the guest's affinity set (LTP getcpu01 pins to one
    // CPU and expects it back), node 0 (single NUMA node). A cpu/node pointer outside the address space
    // -> EFAULT (LTP getcpu02; guest_bad_ptr also catches a PROT_NONE tst_get_bad_addr page).
    case 168:
        if ((a0 && guest_bad_ptr(a0, sizeof(unsigned))) || (a1 && guest_bad_ptr(a1, sizeof(unsigned)))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a0) *(unsigned *)a0 = affinity_first_cpu();
        if (a1) *(unsigned *)a1 = 0;
        G_RET(c) = 0;
        break;
    case 213: G_RET(c) = 0; break; // readahead: advisory, no-op
    case 274:
        G_RET(c) = 0;
        break; // sched_setattr -> ok (ignored)
    // preadv2/pwritev2: flags (a5) ignored; offset in a3 (pos_high a4 is 0 on LP64)
    case 286: {
        if (memf_get((int)a0)) {
            ssize_t r = memf_preadv(g_memf[(int)a0], (const struct iovec *)a1, (int)a2, (off_t)a3, 0);
            G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
            break;
        }
        ssize_t r = preadv((int)a0, (const struct iovec *)a1, (int)a2, (off_t)a3);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 287: {
        if ((int)a0 >= 0 && (int)a0 < DD_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (memf_get((int)a0)) {
            const struct iovec *iv = (const struct iovec *)a1;
            off_t end = (off_t)a3;
            for (int i = 0; i < (int)a2; i++)
                end += iv[i].iov_len;
            if (memf_room_or_spill((int)a0, end)) {
                ssize_t r = memf_pwritev(g_memf[(int)a0], iv, (int)a2, (off_t)a3, 0);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
                break;
            }
        }
        ssize_t r = pwritev((int)a0, (const struct iovec *)a1, (int)a2, (off_t)a3);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // waitid(idtype, id, infop, options): host waitid into a macOS siginfo, then hand-build the guest's
    // Linux siginfo (layout + signal/status numbers differ). idtype P_ALL/P_PID/P_PGID (0/1/2) match.
    case 95: {
        siginfo_t si;
        memset(&si, 0, sizeof si);
        // Linux validates waitid() arguments BEFORE any child lookup (waitid02), in this order:
        //   1. reject option bits outside WNOHANG|WNOWAIT|WEXITED|WSTOPPED|WCONTINUED -> EINVAL;
        //   2. require at least one of WEXITED|WSTOPPED|WCONTINUED -> else EINVAL (a bare WNOHANG, as
        //      waitid02 passes, is invalid; the host would instead report ECHILD after finding no child);
        //   3. reject an idtype outside P_ALL/P_PID/P_PGID/P_PIDFD (0/1/2/3) -> EINVAL;
        //   4. a non-NULL infop that isn't writable -> EFAULT (the kernel copies the siginfo out).
        {
            int lo = (int)a3;
            if (lo & ~(1 | 2 | 4 | 8 | 0x01000000)) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (!(lo & (2 | 4 | 8))) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            int id0 = (int)a0;
            if (id0 != 0 && id0 != 1 && id0 != 2 && id0 != 3) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (a2 && !host_range_mapped((uintptr_t)a2, 128)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            // Raw waitid(2) also copies out arg 5 (struct rusage *) when non-NULL; a bad pointer is EFAULT.
            if (a4 && !host_range_mapped((uintptr_t)a4, 144)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        // P_PIDFD (Linux idtype 3): macOS has no pidfd, so resolve the emulated pidfd back to its pid and
        // wait on P_PID. Go's os.(*Process).pidfdWait reaps a CLONE_PIDFD child exactly this way.
        int idt = (int)a0;
        id_t idv = (id_t)a1;
        if (idt == 3) {
            pid_t tp;
            if (pidfd_lookup((int)a1, &tp) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            idt = P_PID;
            idv = (id_t)tp;
        }
        int lopt = (int)a3, mopt = 0;
        // Linux wait-option bits -> macOS bits (only WNOHANG/WEXITED share a value)
        if (lopt & 0x00000001) mopt |= WNOHANG;    // WNOHANG
        if (lopt & 0x00000002) mopt |= WSTOPPED;   // Linux WSTOPPED(2) -> macOS WSTOPPED
        if (lopt & 0x00000004) mopt |= WEXITED;    // WEXITED
        if (lopt & 0x00000008) mopt |= WCONTINUED; // Linux WCONTINUED(8) -> macOS WCONTINUED
        if (lopt & 0x01000000) mopt |= WNOWAIT;    // Linux WNOWAIT -> macOS WNOWAIT
        int r = waitid((idtype_t)idt, idv, &si, mopt);
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        uint8_t *gi = (uint8_t *)a2;
        if (gi) {
            // Linux siginfo_t is 128 bytes; zero it (also the WNOHANG "no child" case -> si_pid stays 0)
            memset(gi, 0, 128);
            if (si.si_pid != 0) {
                int code = si.si_code, status = si.si_status;
                int gsig, gcore;
                // the child relayed a guest signal death (it _exitd after recording its Linux signo in
                // the shared table because no faithful fatal host mapping exists). The host reports CLD_EXITED;
                // rebuild the CLD_KILLED/CLD_DUMPED siginfo. WNOWAIT (does not reap) must NOT consume the slot.
                if (code == CLD_EXITED && sigexit_lookup((int)si.si_pid, &gsig, &gcore, !(lopt & 0x01000000))) {
                    code = gcore ? CLD_DUMPED : CLD_KILLED;
                    status = gsig;
                }
                // si_status carries a signal number for kill/dump/stop/cont -> translate macOS->Linux
                else if (code == CLD_KILLED || code == CLD_DUMPED || code == CLD_STOPPED || code == CLD_CONTINUED)
                    status = sig_m2l(status);
                // Linux reports CLD_DUMPED (not CLD_KILLED) when a core-dumping signal killed the child with
                // cores enabled. macOS rarely dumps the host child, so synthesize it the way wait4 encodes
                // WCOREDUMP: core-dumping signal AND the guest's RLIMIT_CORE soft limit > 0. (status is now the
                // translated Linux signo.)
                if (code == CLD_KILLED && sig_coredumps(status) && svc_core_rlimit_cur() > 0) code = CLD_DUMPED;
                *(int *)(gi + 0) = 17;   // si_signo = Linux SIGCHLD
                *(int *)(gi + 4) = 0;    // si_errno
                *(int *)(gi + 8) = code; // si_code (CLD_* values match Linux<->macOS)
                *(int *)(gi + 16) = (int)si.si_pid;
                *(int *)(gi + 20) = (int)si.si_uid;
                *(int *)(gi + 24) = status; // si_status
            }
        }
        // guest-pid namespace: on an ACTUAL reap of a TERMINATED child (not a WNOWAIT peek, not a stop/
        // continue report), drop its container-registry record -- see wait4 (case 260) -- so a signal-killed
        // child leaves no stale membership marker a recycled host pid could inherit.
        if (si.si_pid != 0 && !(lopt & 0x01000000) &&
            (si.si_code == CLD_EXITED || si.si_code == CLD_KILLED || si.si_code == CLD_DUMPED))
            proc_reg_reap((int)si.si_pid);
        // Raw waitid(2) fills arg 5 (struct rusage *) when non-NULL (glibc's wrapper passes NULL, but the
        // raw syscall and some runtimes use it). macOS waitid has no rusage variant, so report the reaped
        // child's accounting best-effort from RUSAGE_CHILDREN in the guest's Linux layout -- leaving the
        // buffer untouched exposed sentinel garbage (a wild ru_maxrss). Only on an actual reap (si_pid set).
        if (a4 && si.si_pid != 0) {
            struct rusage cru;
            if (getrusage(RUSAGE_CHILDREN, &cru) == 0)
                rusage_to_linux((uint8_t *)a4, &cru);
            else
                memset((void *)a4, 0, 144);
        }
        G_RET(c) = 0;
        break;
    }
    // truncate(path, length): resolve the guest path through the overlay (same helper execve uses), then
    // truncate by host path. Evict the stat cache so the new size is observed.
    case 45: {
        if (jail_ro_at(-100, (const char *)a0)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        char pb[4200];
        const char *p = xresolve_overlay((const char *)a0, pb, sizeof pb);
        int r = truncate(p, (off_t)a1);
        if (r >= 0) mc_evict(p);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // getrlimit(resource, rlim) / setrlimit(resource, rlim): alias prlimit64 (case 261, svc_fill_rlimit).
    // RLIMIT_STACK(3) reports 8MB, RLIMIT_NOFILE(7) a finite fd cap, everything else unlimited; setrlimit is
    // accepted (no-op).
    case 163:
        if (a1) svc_fill_rlimit((int)a0, (uint64_t *)a1);
        G_RET(c) = 0;
        break;
    case 164: {
        // setrlimit(resource, rlim): apply into the same store getrlimit/prlimit64 read, so a direct
        // setrlimit(2) (not funneled through prlimit64/case 261 by glibc) also takes effect -- e.g. a guest
        // raising RLIMIT_CORE to enable cores must have wait4/waitid report WCOREDUMP afterwards.
        int res = (int)a0;
        const uint64_t *nl = (const uint64_t *)a1;
        if (nl && res >= 0 && res < DD_RLIM_MAX) {
            g_ulimit[res].set = 1;
            g_ulimit[res].cur = nl[0];
            g_ulimit[res].max = nl[1];
        }
        G_RET(c) = 0;
        break; // setrlimit -> accepted
    }

    // adjtimex(2)/clock_adjtime(2): read-only query fills struct timex + TIME_OK; setting -> EPERM.
    case 266: { // clock_adjtime(clk_id, timex)
        int r = svc_adjtimex((uint8_t *)a1);
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
        break;
    }
    case 171: { // adjtimex(timex)
        int r = svc_adjtimex((uint8_t *)a0);
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
        break;
    }
    // sched_getattr(pid, attr, size, flags): report a SCHED_OTHER profile. Zero the caller's struct, then
    // fill size + sched_policy=SCHED_OTHER(0); nice/priority stay 0. (sched_setattr is case 274, ignored.)
    case 275: {
        if (a1) {
            size_t sz = (size_t)a2;
            if (sz == 0 || sz > 48) sz = 48; // kernel struct sched_attr is 48+ bytes; cap to a sane size
            memset((void *)a1, 0, sz);
            *(uint32_t *)(a1 + 0) = (uint32_t)sz; // sched_attr.size
            *(uint32_t *)(a1 + 4) = 0;            // sched_attr.sched_policy = SCHED_OTHER
        }
        G_RET(c) = 0;
        break;
    }
    // mlock2(addr, len, flags): like mlock (228) -- track the range so /proc reports it locked. Only
    // MLOCK_ONFAULT(1) is a valid flag (Linux -> EINVAL otherwise), and RLIMIT_MEMLOCK is honored the same
    // as mlock (soft limit 0 -> EPERM, exceeding -> ENOMEM; container is unprivileged, no CAP_IPC_LOCK).
    case 284: {
        if ((unsigned)a2 & ~1u) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int rl = mlk_rlimit_gate(a0, (uint64_t)a1);
        if (rl < 0) {
            G_RET(c) = (uint64_t)(int64_t)rl;
            break;
        }
        mlk_add(a0, (uint64_t)a1);
        G_RET(c) = 0;
        break;
    }
    // rt_tgsigqueueinfo(tgid, tid, sig, siginfo): thread-targeted sibling of rt_sigqueueinfo (case 138).
    // Carry si_code + si_value to the guest handler's siginfo, then raise the signal to the guest.
    case 240: {
        int sig = (int)a2;
        if (sig >= 1 && sig <= 64 && a3) {
            g_sigcode[sig] = *(int *)(a3 + 8);      // siginfo.si_code
            g_sigval[sig] = *(uint64_t *)(a3 + 24); // siginfo.si_value (sival_int/ptr)
        }
        raise_guest_signal(c, sig);
        G_RET(c) = 0;
        break;
    }

    // x86-only `time` (x86 nr 201, no aarch64 equivalent) — glibc has no vDSO here and issues the raw
    // syscall on a hot path (redis server-cron / per-command clock). Serve it directly: seconds since epoch,
    // optionally written through the result pointer in a0. Without this it spams the unhandled-syscall log
    // on every call (a per-op fprintf + ENOSYS), which both breaks the guest's clock and tanks throughput.
#ifdef CANON_X86ONLY
    case (CANON_X86ONLY | 201): { // time (x86-only nr 201; no aarch64 equivalent) -- redis hot clock path
        time_t t = time(NULL);
        if (a0) *(int64_t *)a0 = (int64_t)t;
        G_RET(c) = (uint64_t)(int64_t)t;
        break;
    }
#endif
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
