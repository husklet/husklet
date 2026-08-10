// Extracted from service(): the long tail of uncommon/low-traffic syscalls + Docker seccomp-profile
// parity -- bpf/userfaultfd/io_uring/ptrace (-EPERM like the default profile), flock, mq_*, sched_*,
// {get,set}itimer, cap{get,set}, adjtimex, fs id setters, getcpu, readahead, etc. Returns 1 if nr was
// handled, 0 otherwise. Included by service.c AFTER its local helpers (svc_adjtimex/pidfd_*/mq_* it
// calls) and before service() -- same TU scope.

// ---- POSIX message-queue helpers (mq_timed{send,receive} timeout + blocking; backing state in dispatch.c)
#define HL_SI_MESGQ (-3) // Linux si_code SI_MESGQ: an mq_notify(SIGEV_SIGNAL) delivery (what the guest expects)

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
    /* The guest's struct timespec is two 64-bit words on every guest this engine
       targets, so the buffer it is copied into must be 64-bit wide REGARDLESS of
       what the host calls `long`. Spelled `long` this read 8 bytes instead of 16
       on an LLP64 host and every deadline came back as garbage. */
    int64_t ts[2];
    if (guest_copy_from(ts, p, sizeof(ts)) != sizeof(ts)) return -EFAULT;
    int64_t sec = ts[0], nsec = ts[1];
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
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_REALTIME, &now);
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
    // io_uring is blocked by Docker's default seccomp profile. Report the same EPERM policy result
    // for every entry point rather than claiming that the Linux API is absent with ENOSYS.
    case 425: // io_uring_setup(2)
    case 426: // io_uring_enter(2)
    case 427: // io_uring_register(2)
        G_RET(c) = (uint64_t)(-EPERM);
        break;
    // seccomp(2): ENFORCED. We store the guest's classic-BPF program(s) and run a small cBPF interpreter
    // against a real struct seccomp_data on every syscall (os/linux/seccomp.c, gated in service()), honouring
    // the program's return action (ALLOW/ERRNO/KILL_PROCESS/KILL_THREAD/TRAP/TRACE/LOG). SET_MODE_STRICT is
    // likewise enforced (only read/write/exit/rt_sigreturn). Filters are per-thread, stacked, inherited across
    // fork and preserved across hl's in-process execve -- matching Linux's SECCOMP_FILTER semantics.
    case 277: { // seccomp(op, flags, args)
        unsigned op = (unsigned)a0;
        if (op == HL_LINUX_SECCOMP_SET_MODE_FILTER) {
            G_RET(c) = (uint64_t)(int64_t)seccomp_install_filter(a2, (uint32_t)a1);
        } else if (op == HL_LINUX_SECCOMP_SET_MODE_STRICT) {
            // strict takes no flags/args (SECCOMP_SET_MODE_STRICT): both must be zero, else -EINVAL.
            G_RET(c) = (a1 || a2) ? (uint64_t)(-EINVAL) : (uint64_t)(int64_t)seccomp_set_strict();
        } else if (op == 2 /*SECCOMP_GET_ACTION_AVAIL*/) {
            G_RET(c) = (uint64_t)(int64_t)seccomp_get_action_avail(a1, a2);
        } else if (op == 3 /*SECCOMP_GET_NOTIF_SIZES*/) {
            G_RET(c) = (uint64_t)(int64_t)seccomp_get_notif_sizes(a1, a2);
        } else {
            G_RET(c) = (uint64_t)(-EINVAL);
        }
        break;
    }
    // ptrace(2): real in-hl tracer/tracee coordination. hl emulates the ptrace relationship
    // BETWEEN two guest processes (both run translated under hl) over a shared arena keyed on guest pids;
    // see os/linux/syscall/ptrace.c. svc_ptrace sets G_RET (0 / -errno) itself.
    case 117: // ptrace(request, pid, addr, data)
        G_RET(c) = (uint64_t)(int64_t)svc_ptrace(c, a0, a1, a2, a3);
        break;

    case 279: { // memfd_create(name, flags) -> an anonymous file
        // Validate `flags` exactly as Linux (mm/memfd.c): any unknown bit -> EINVAL. Shared-memory IPC
        // ChannelLinux::KernelSupportsUpgradeRequirements() probes memfd_create("", ~0) and PCHECKs the
        // call FAILS with EINVAL/ENOSYS/EPERM; without this the probe SUCCEEDED (fd>=0) and the caller
        // process aborted at channel_linux.cc (Check failed). MFD_CLOEXEC=1 MFD_ALLOW_SEALING=2
        // MFD_HUGETLB=4 MFD_NOEXEC_SEAL=8 MFD_EXEC=16; with MFD_HUGETLB the huge-size log2 bits (0x3f<<26)
        // are also permitted. (mkstemp ignores `name`, so no name-length check is needed here.)
        {
            unsigned mfd_flags = (unsigned)a1;
            unsigned mfd_known = 0x1fu;                     // CLOEXEC|ALLOW_SEALING|HUGETLB|NOEXEC_SEAL|EXEC
            if (mfd_flags & 4u) mfd_known |= (0x3fu << 26); // MFD_HUGETLB -> size-log2 bits are valid
            if (mfd_flags & ~mfd_known) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
        }
        int fd;
#if defined(__linux__)
        /*
         * A sparse unlinked file is not equivalent to memfd storage:
         * ftruncate can succeed on a full /tmp and the first MAP_SHARED write
         * then raises SIGBUS. Large dual-mapped JIT arenas such as BEAM's hit
         * exactly that failure. Use the host's anonymous shmem object so guest
         * memfd allocation is independent of filesystem capacity.
         */
        fd = memfd_create("hl-guest", MFD_CLOEXEC);
#else
        char tn[] = "/tmp/.hl-memfdXXXXXX";
        fd = mkstemp(tn);
        if (fd >= 0) unlink(tn);
#endif
        if (fd >= 0) {
            if (a1 & 1) fcntl(fd, F_SETFD, FD_CLOEXEC); // MFD_CLOEXEC
            // Track it as a memfd so F_ADD_SEALS/F_GET_SEALS (io.c fcntl) and the F_SEAL_WRITE write-guard
            // apply. Without MFD_ALLOW_SEALING (2) the file is born F_SEAL_SEAL'd -> later F_ADD_SEALS EPERMs,
            // exactly as on Linux.
            if (fd < HL_NFD) {
                g_memfd_is[fd] = 1;
                g_memfd_seal[fd] = (a1 & 2) ? 0 : 0x1 /*F_SEAL_SEAL*/;
                memfd_reg_set_fd(fd, g_memfd_seal[fd]);
            }
            if (proc_fdvis_publish_native_fd(fd) != 0) {
                if (fd < HL_NFD) {
                    g_memfd_is[fd] = 0;
                    g_memfd_seal[fd] = 0;
                }
                close(fd);
                fd = -1;
                errno = EMFILE;
            }
        }
        G_RET(c) = fd < 0 ? (uint64_t)(-errno) : (uint64_t)fd;
        break;
    }
    // flock(fd, op): BSD whole-file advisory lock. Serviced on a private companion file (see hl_flock in
    // helpers.c) so it stays INDEPENDENT of fcntl POSIX record locks -- on macOS both would otherwise share
    // one per-vnode lock list and spuriously conflict with each other. Linux LOCK_SH/EX/UN/NB match the host.
    case 32: G_RET(c) = hl_flock((int)a0, (int)a1) < 0 ? (uint64_t)(-errno) : 0; break;
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
        size_t open_count = 0;
        hl_host_process_fd *open_fds = NULL;
        int enumerated = hl_host_process_fds((int64_t)getpid(), NULL, 0, &open_count);
        if (enumerated && open_count != 0) {
            size_t capacity = open_count;
            open_fds = calloc(capacity, sizeof *open_fds);
            if (open_fds == NULL || !hl_host_process_fds((int64_t)getpid(), open_fds, capacity, &open_count)) {
                free(open_fds);
                open_fds = NULL;
                enumerated = 0;
            } else if (open_count > capacity) {
                capacity = open_count;
                hl_host_process_fd *larger = realloc(open_fds, capacity * sizeof *open_fds);
                if (larger == NULL || !hl_host_process_fds((int64_t)getpid(), larger, capacity, &open_count)) {
                    free(larger == NULL ? open_fds : larger);
                    open_fds = NULL;
                    enumerated = 0;
                } else {
                    open_fds = larger;
                    if (open_count > capacity) open_count = capacity;
                }
            }
        }
        size_t visits = enumerated ? open_count : (size_t)last - first + 1;
        for (size_t index = 0; index < visits; ++index) {
            unsigned fd = enumerated ? (unsigned)open_fds[index].descriptor : first + (unsigned)index;
            if (fd < first || fd > last) continue;
            hl_linux_fd_snapshot bound;
            if (g_linux_box != NULL && hl_linux_fd_snapshot_get(g_linux_box, fd, &bound) == HL_STATUS_OK) {
                if (flags & 4) {
                    (void)hl_linux_fcntl(g_linux_box, fd, HL_LINUX_F_SETFD, HL_LINUX_FD_CLOEXEC);
                    (void)fcntl((int)fd, F_SETFD, FD_CLOEXEC);
                } else {
                    (void)hl_linux_close(g_linux_box, fd);
                    proc_fdvis_close((int)fd);
                    close((int)fd);
                }
                continue;
            }
            if (flags & 4) { // CLOSE_RANGE_CLOEXEC
                int fl = fcntl((int)fd, F_GETFD);
                if (fl >= 0) fcntl((int)fd, F_SETFD, fl | FD_CLOEXEC);
            } else {
                if (exec_fd_is_engine((int)fd)) continue; // never close a (relocated) live engine fd
                fd_reset_emul((int)fd); // drop hl's emulation tables so a reused fd number isn't misrouted
                proc_fdvis_close((int)fd);
                close((int)fd);
            }
        }
        free(open_fds);
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
        int fd = pidfd_make(hpid); // host pollable process watch; poll/epoll wakes when the target exits
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
            // Cross-process: translate Linux->macOS signo (the target hl engine listens on the macOS number;
            // see kill, case 129). Untranslated, a divergent signal (SIGUSR1/2, SIGURG, ...) is lost.
            G_RET(c) = kill(pid, sig_l2m(sig)) < 0 ? (uint64_t)(-errno) : 0;
        }
        break;
    }
    // pidfd_getfd(pidfd, targetfd, flags): duplicate targetfd out of the process the pidfd refers to
    // (container managers/debuggers pull a listening socket or log fd out of a child this way). A pidfd we
    // minted (case 434) is a REAL host pidfd on Linux, and the engine keeps guest fd numbers equal to their
    // backing host fd numbers, so forward straight to the host syscall: the kernel does the real
    // ptrace-scope permission check, so success or EPERM (yama) matches native for the caller's privilege.
    // flags must be 0 (Linux rejects any other value with EINVAL); an fd we never minted -> EBADF (same as
    // pidfd_send_signal). Without this the syscall fell through to ENOSYS, diverging from a kernel where a
    // same-user self/child getfd succeeds.
    case 438: {
#ifdef SYS_pidfd_getfd
        if (a2 != 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        pid_t pid;
        if (pidfd_lookup((int)a0, &pid) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        long r = syscall(SYS_pidfd_getfd, (int)a0, (int)a1, 0u);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
#else
        G_RET(c) = (uint64_t)(int64_t)(-ENOSYS);
#endif
        break;
    }
    // mq_open(name, oflag, mode, attr): find-or-create the named queue, hand back a real fd bound to it.
    // (glibc already rejects a name that lacks a leading '/' or is empty with EINVAL before the syscall,
    // so the raw-syscall path only has to police ENAMETOOLONG, EFAULT, ENOENT, EEXIST, ENOSPC + a bad attr.)
    case 180: {
#if defined(__linux__)
        // Host-passthrough: a real POSIX mqueue is a kernel object — pollable via poll/select, shared
        // across fork by name, and its mq_notify is delivered by the kernel's own signal machinery. The
        // returned descriptor IS a real host mqd (guest fd == host fd, as with pipes/sockets), so it flows
        // through the existing fd table and the kernel enforces access mode, RLIMIT_MSGQUEUE, geometry, and
        // the ENOENT/EEXIST/ENAMETOOLONG/EINVAL errno order natively — identical to the native oracle. The
        // #else in-process broker below backs the macOS build (Darwin has no POSIX mqueue kernel object).
        char name[258];
        int copied_name = guest_copy_string(name, sizeof(name), a0);
        if (copied_name < 0) {
            G_RET(c) = (uint64_t)(int64_t)copied_name;
            break;
        }
        long attr[8], *attrp = NULL;
        if (a3) {
            if (guest_copy_from(attr, a3, sizeof(attr)) != (ssize_t)sizeof(attr)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            attrp = attr;
        }
        long r = syscall(SYS_mq_open, name, (int)a1, (unsigned)a2, attrp);
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : (uint64_t)r;
        break;
#else
        char name_storage[258];
        int name_result = guest_copy_string(name_storage, sizeof(name_storage), a0);
        if (name_result < 0) {
            G_RET(c) = (uint64_t)(int64_t)name_result;
            break;
        }
        const char *name = name_storage;
        int oflag = (int)a1;
        /* struct mq_attr is four 64-bit words in the GUEST's ABI. Same LLP64 trap
           as mq_check_timeout above: spelled `long` this copied 16 bytes of a
           32-byte structure, so mq_maxmsg read back as the high half of mq_flags
           (zero) and every create with an explicit attr returned EINVAL. */
        int64_t attr_storage[4];
        const int64_t *at = NULL; // struct mq_attr: {flags, maxmsg, msgsize, curmsgs, ...}
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
            if (a3) {
                if (guest_copy_from(attr_storage, a3, sizeof(attr_storage)) != sizeof(attr_storage)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                at = attr_storage;
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
        int fd = open(HL_LINUX_HOST_NULL_DEVICE, O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        if (mq_bind(fd, qi) != 0) {
            close(fd);
            G_RET(c) = (uint64_t)(int64_t)(-EMFILE);
            break;
        }
        g_mqq[qi].refs++;
        g_mqfd_amode[fd] = (uint8_t)(oflag & O_ACCMODE); // remember O_RDONLY/O_WRONLY/O_RDWR for send/recv EBADF
        mq_fd_setnb(fd, (oflag & MQ_O_NONBLOCK) != 0);   // O_NONBLOCK is a per-descriptor mq_flag
        G_RET(c) = (uint64_t)fd;
        break;
#endif
    }
    // mq_unlink(name): mark removed; freed once the last descriptor is gone.
    case 181: {
#if defined(__linux__)
        char name[258];
        int copied_name = guest_copy_string(name, sizeof(name), a0);
        if (copied_name < 0) {
            G_RET(c) = (uint64_t)(int64_t)copied_name;
            break;
        }
        long r = syscall(SYS_mq_unlink, name); // host object: ENOENT enforced by the kernel
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : 0;
        break;
#else
        char name[258];
        int name_result = guest_copy_string(name, sizeof(name), a0);
        if (name_result < 0) {
            G_RET(c) = (uint64_t)(int64_t)name_result;
            break;
        }
        int qi = mq_find(name);
        if (qi < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOENT);
            break;
        }
        g_mqq[qi].unlinked = 1;
        mq_maybe_free(qi);
        G_RET(c) = 0;
        break;
#endif
    }
    // mq_timedsend(mqdes, msg, len, prio, abs_timeout): insert highest-priority-first, FIFO within a prio.
    // Errno order mirrors the kernel: abs_timeout EFAULT/EINVAL (validated first, in the wrapper), then prio
    // EINVAL, EBADF, EMSGSIZE, msg-buffer EFAULT, then full-queue handling: O_NONBLOCK -> EAGAIN, else block
    // until space or the abs_timeout expires (-> ETIMEDOUT) / a signal (-> EINTR). Real cross-PROCESS blocking
    // is not emulated (queues aren't shared across fork); a full queue with a blocking descriptor and a NULL
    // timeout therefore blocks forever exactly as Linux would when nothing can drain it.
    case 182: {
#if defined(__linux__)
        // Host-passthrough: the kernel enforces prio<MQ_PRIO_MAX, EBADF (fd + access mode), EMSGSIZE, the
        // msg-buffer EFAULT, then full-queue O_NONBLOCK->EAGAIN / block-until-space-or-abs_timeout
        // (ETIMEDOUT) / EINTR — the exact native semantics, and a blocked send now wakes when ANY process
        // sharing the named queue drains it (real cross-process blocking, not the single-process broker).
        struct timespec timeout, *timeoutp = NULL;
        if (a4) {
            if (guest_copy_from(&timeout, a4, sizeof(timeout)) != (ssize_t)sizeof(timeout)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            timeoutp = &timeout;
        }
        if ((unsigned)a3 >= 32768) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        long attr[8] = {0};
        if (syscall(SYS_mq_getsetattr, (int)a0, NULL, attr) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-errno);
            break;
        }
        if (attr[2] >= 0 && a2 > (uint64_t)attr[2]) {
            G_RET(c) = (uint64_t)(int64_t)(-EMSGSIZE);
            break;
        }
        char *message = malloc(a2 ? (size_t)a2 : 1);
        if (!message) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        if (a2 && guest_copy_from(message, a1, (size_t)a2) != (ssize_t)a2) {
            free(message);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        long r = syscall(SYS_mq_timedsend, (int)a0, message, (size_t)a2, (unsigned)a3, timeoutp);
        free(message);
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : 0;
        break;
#else
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
        if (!mq_fd_canwrite((int)a0)) { // send on a descriptor opened O_RDONLY -> EBADF (before EMSGSIZE)
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        struct mq_queue *q = &g_mqq[qi];
        size_t len = (size_t)a2;
        if ((long)len > q->msgsize) {
            G_RET(c) = (uint64_t)(int64_t)(-EMSGSIZE);
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
        if (len && guest_copy_from(buf, a1, len) != (ssize_t)len) {
            free(buf);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
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
                g_sigcode[q->notify_signo] = HL_SI_MESGQ;
                g_sigval[q->notify_signo] = q->notify_val;
                g_sigpid[q->notify_signo] = container_pid(); // si_pid: the sender (this process, self-notify)
                g_siguid[q->notify_signo] = cuid();          // si_uid
                raise_guest_signal(c, q->notify_signo);
            }
            q->notify_set = 0;
        }
        G_RET(c) = 0;
        break;
#endif
    }
    // mq_timedreceive(mqdes, msg, len, prio*, abs_timeout): pop the head (highest priority, oldest first).
    // Same errno order as mq_timedsend: abs_timeout EFAULT/EINVAL, EBADF, EMSGSIZE (buffer < mq_msgsize),
    // then empty-queue handling: O_NONBLOCK -> EAGAIN, else block until a message / deadline / signal.
    case 183: {
#if defined(__linux__)
        // Host-passthrough: the kernel pops the highest-priority-oldest message, writes it + the priority,
        // and enforces EBADF (fd + access mode), EMSGSIZE (buffer < mq_msgsize), then empty-queue
        // O_NONBLOCK->EAGAIN / block-until-message-or-abs_timeout (ETIMEDOUT) / EINTR. A blocked receive now
        // wakes when ANY process sharing the named queue sends (the cross-process fix).
        struct timespec timeout, *timeoutp = NULL;
        if (a4) {
            if (guest_copy_from(&timeout, a4, sizeof(timeout)) != (ssize_t)sizeof(timeout)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            timeoutp = &timeout;
        }
        long attr[8] = {0};
        size_t capacity = (size_t)a2;
        if (syscall(SYS_mq_getsetattr, (int)a0, NULL, attr) == 0 && attr[2] >= 0 && capacity > (size_t)attr[2])
            capacity = (size_t)attr[2];
        char *message = malloc(capacity ? capacity : 1);
        if (!message) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        unsigned priority = 0;
        long r = syscall(SYS_mq_timedreceive, (int)a0, message, (size_t)a2, a3 ? &priority : NULL, timeoutp);
        if (r >= 0 && ((r && guest_copy_to(a1, message, (size_t)r) != r) ||
                       (a3 && guest_copy_to(a3, &priority, sizeof(priority)) != (ssize_t)sizeof(priority))))
            r = -1, errno = EFAULT;
        free(message);
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : (uint64_t)r;
        break;
#else
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
        if (!mq_fd_canread((int)a0)) { // receive on a descriptor opened O_WRONLY -> EBADF (before EMSGSIZE)
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
        if (m.len && guest_accessible_prefix(a1, m.len, PROT_WRITE) != m.len) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a3 && guest_accessible_prefix(a3, sizeof(unsigned), PROT_WRITE) != sizeof(unsigned)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        for (int j = 1; j < q->n; j++)
            q->msg[j - 1] = q->msg[j];
        q->n--;
        if ((m.len && guest_copy_to(a1, m.data, m.len) != (ssize_t)m.len) ||
            (a3 && guest_copy_to(a3, &m.prio, sizeof(m.prio)) != sizeof(m.prio))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        free(m.data);
        G_RET(c) = (uint64_t)m.len;
        break;
#endif
    }
    // mq_notify(mqdes, sevp): register/unregister the single one-shot notification the queue fires on its
    // empty->non-empty edge (delivered in mq_timedsend). Errno order matches the kernel: for a non-NULL sevp,
    // EFAULT (unreadable sigevent) then EINVAL (sigev_notify not SIGEV_SIGNAL/NONE/THREAD, or SIGEV_SIGNAL
    // with an invalid signo) are checked BEFORE the fd -> EBADF -> EBUSY (already registered). A NULL sevp
    // removes this process's registration (always 0). SIGEV_THREAD is accepted like the kernel but its glibc
    // helper-thread/netlink callback is not driven in-process, so only the registration/EBUSY semantics hold
    // for it (SIGEV_SIGNAL and SIGEV_NONE are fully emulated).
    case 184: {
#if defined(__linux__)
        // Host-passthrough: the kernel owns the one-shot registration and delivers the notification. For
        // SIGEV_SIGNAL it raises the guest's chosen signal with a real SI_MESGQ siginfo carrying the
        // registered sigev_value (the engine's host-signal interception routes it into the guest with
        // si_code/si_value intact); SIGEV_NONE registers-only. This is the empty->non-empty edge fired by
        // the kernel — cross-process and race-correct — and unblocks ipc_mq_notify. A NULL sevp
        // unregisters. SIGEV_THREAD is glibc-rewritten in the GUEST before this syscall (netlink-socket
        // delivery), so only its registration/EBUSY errno semantics are guaranteed here; the callback
        // thread's netlink plumbing is not separately driven (documented, matches the SIGEV_SIGNAL/NONE
        // fidelity the test asserts). Access-mode/EFAULT/EINVAL/EBADF/EBUSY are enforced by the kernel.
        unsigned char event[64];
        if (a1 && guest_copy_from(event, a1, sizeof event) != sizeof event) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        long r = syscall(SYS_mq_notify, (int)a0, a1 ? event : NULL);
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : 0;
        break;
#else
        uint8_t event[16];
        const uint8_t *sev = a1 ? event : NULL;
        if (sev) {
            if (guest_copy_from(event, a1, sizeof(event)) != sizeof(event)) {
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
#endif
    }
    // mq_getsetattr(mqdes, newattr, oldattr): report mq_flags(O_NONBLOCK)/maxmsg/msgsize/curmsgs into oldattr
    // (the state BEFORE any change), then, if newattr is set, apply it -- only O_NONBLOCK is settable (the
    // kernel masks mq_flags to O_NONBLOCK and ignores the rest), so this is the mq analogue of F_SETFL.
    case 185: {
#if defined(__linux__)
        // Host-passthrough: the kernel reports the current mq_flags(O_NONBLOCK)/maxmsg/msgsize/curmsgs into
        // oldattr and, if newattr is set, applies only the O_NONBLOCK bit (the mq analogue of F_SETFL) —
        // exact native geometry and EBADF/EFAULT semantics.
        long new_attribute[4], old_attribute[4];
        if (a1 && guest_copy_from(new_attribute, a1, sizeof new_attribute) != sizeof new_attribute) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        long r = syscall(SYS_mq_getsetattr, (int)a0, a1 ? new_attribute : NULL, a2 ? old_attribute : NULL);
        if (r >= 0 && a2 && guest_copy_to(a2, old_attribute, sizeof old_attribute) != sizeof old_attribute) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : 0;
        break;
#else
        /* Guest struct mq_attr, four 64-bit words -- see the note in case 180. */
        int64_t new_attribute[4], old_attribute[4];
        if ((a1 && guest_copy_from(new_attribute, a1, sizeof(new_attribute)) != sizeof(new_attribute)) ||
            (a2 && guest_accessible_prefix(a2, sizeof(old_attribute), PROT_WRITE) != sizeof(old_attribute))) {
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
            int64_t *o = old_attribute;
            o[0] = mq_fd_nonblock((int)a0) ? MQ_O_NONBLOCK : 0;
            o[1] = q->maxmsg;
            o[2] = q->msgsize;
            o[3] = q->n;
        }
        if (a1) { // newattr: only mq_flags' O_NONBLOCK bit is honoured
            int64_t nf = new_attribute[0];
            mq_fd_setnb((int)a0, (nf & MQ_O_NONBLOCK) != 0);
        }
        if (a2 && guest_copy_to(a2, old_attribute, sizeof(old_attribute)) != sizeof(old_attribute)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
#endif
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
    case 121: {
        int priority = 0;
        if (guest_copy_to(a1, &priority, sizeof priority) != sizeof priority) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        G_RET(c) = 0;
        break; // sched_getparam -> priority 0
    }
    case 125: G_RET(c) = (a0 == 1 || a0 == 2) ? 99 : 0; break; // sched_get_priority_max: FIFO/RR=99 else 0
    case 126: G_RET(c) = (a0 == 1 || a0 == 2) ? 1 : 0; break;  // sched_get_priority_min: FIFO/RR=1 else 0
    case 127: {                                                // sched_rr_get_interval -> a nominal 100ms slice
        struct timespec interval = {.tv_nsec = 100000000L};
        if (guest_copy_to(a1, &interval, sizeof interval) != sizeof interval) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        G_RET(c) = 0;
        break;
    }
    // mlockall/munlockall. macOS has no mlockall(2), but it DOES have mlock(2), so we wire the pages for real
    // over the guest's tracked mappings instead of only tracking state: MCL_CURRENT host-mlocks every current
    // guest range (hl_gmap_lock_wire_current); MCL_FUTURE arms the registry so each fresh mmap (mem.c case 222) is
    // wired on creation. The lock STATE stays reported via /proc VmLck:/smaps Locked: (LTP
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
        int rl = hl_gmap_lock_limit_all();
        if (rl < 0 && ((f & 1u) || rl == -EPERM)) { // ENOMEM only bites MCL_CURRENT; EPERM bites either
            G_RET(c) = (uint64_t)(int64_t)rl;
            break;
        }
        if (f & 1u) hl_gmap_lock_wire_current(); // MCL_CURRENT: wire every existing mapping resident now
        hl_gmap_lock_all((f & 2u) != 0);
        G_RET(c) = 0;
        break;
    }
    case 231:
        hl_gmap_lock_unwire_all(); // drop the real host wiring before clearing the tracked state
        hl_gmap_lock_reset();
        G_RET(c) = 0;
        break;
    // NUMA memory-policy syscalls (mbind/{set,get}_mempolicy/migrate_pages/move_pages). The host is a
    // single NUMA node and these are advisory placement hints, so accept them as permissive no-ops --
    // e.g. R/OpenBLAS calls mbind(2) on its large matrix buffers at startup. (arm64-normalized numbers;
    // x86-64 237/238/239/256/279 are mapped to these by sysmap.h.)
    case 235: G_RET(c) = 0; break; // mbind          -> success, no-op
    case 237: G_RET(c) = 0; break; // set_mempolicy  -> success, no-op
    case 238:
        G_RET(c) = 0;
        break; // migrate_pages  -> success, no-op (single NUMA node, nothing to move)
    // move_pages(pid, count, pages, nodes, status, flags). Single NUMA node here, so nothing ever migrates
    // -- but returning 0 while leaving status[] UNTOUCHED makes a NUMA-introspection guest (numactl --show,
    // libnuma) read an uninitialized buffer. QUERY mode (nodes==NULL) must fill status[] with each page's
    // current node; a real migration request (nodes!=NULL) still writes the resulting node into status[] if
    // one was given. Report node 0 for a mapped/present page, -ENOENT for one not present -- exactly Linux.
    case 239: {
        unsigned long count = (unsigned long)a1;
        if (count == 0) {
            G_RET(c) = 0;
            break;
        }
        if (count > SIZE_MAX / sizeof(void *)) { // guard the size math against a bogus count
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        void **pages = malloc(count * sizeof *pages);
        int *nodes = a3 ? malloc(count * sizeof *nodes) : NULL;
        int *status = a4 ? malloc(count * sizeof *status) : NULL;
        if (!pages || (a3 && !nodes) || (a4 && !status)) {
            free(pages);
            free(nodes);
            free(status);
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        if (guest_copy_from(pages, a2, count * sizeof *pages) != (ssize_t)(count * sizeof *pages) ||
            (nodes && guest_copy_from(nodes, a3, count * sizeof *nodes) != (ssize_t)(count * sizeof *nodes))) {
            free(pages);
            free(nodes);
            free(status);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (status)
            for (unsigned long i = 0; i < count; i++)
                status[i] =
                    guest_accessible_prefix((uint64_t)(uintptr_t)pages[i], 1, HL_LOGICAL_VMA_READ) == 1 ? 0 : -ENOENT;
        if (status && guest_copy_to(a4, status, count * sizeof *status) != (ssize_t)(count * sizeof *status)) {
            free(pages);
            free(nodes);
            free(status);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        free(pages);
        free(nodes);
        free(status);
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
        if (a0) {
            // Validate the WHOLE 4-byte int, not just its first page: the store below is a full int write, so
            // a mode pointer straddling a page boundary into an unmapped page (host_addr_mapped only probes the
            // start) would fault the engine instead of returning -EFAULT. Mirror the sibling getcpu (case 168),
            // which range-checks its cpu/node out-pointers.
            int mode = 0;
            if (guest_copy_to(a0, &mode, sizeof mode) != sizeof mode) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = 0;
        break;
    }
    // getitimer/setitimer: wrap the host (ITIMER_* + struct itimerval layouts match Linux<->macOS)
    case 102: {
        struct itimerval value;
        if (getitimer((int)a0, &value) < 0)
            G_RET(c) = (uint64_t)(-errno);
        else
            G_RET(c) = guest_copy_to(a1, &value, sizeof value) == sizeof value ? 0 : (uint64_t)(int64_t)-EFAULT;
        break;
    }
    case 103: {
        struct itimerval value, old;
        if (guest_copy_from(&value, a1, sizeof value) != sizeof value) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        int result = setitimer((int)a0, &value, a2 ? &old : NULL);
        if (result < 0)
            G_RET(c) = (uint64_t)(-errno);
        else if (a2 && guest_copy_to(a2, &old, sizeof old) != sizeof old)
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
        else
            G_RET(c) = 0;
        break;
    }
    // clock_settime: validate the clock id BEFORE the privilege check, as Linux does. An unknown or
    // non-settable clock id (e.g. CLOCK_MONOTONIC) is -EINVAL; only the settable wall clocks
    // CLOCK_REALTIME(0)/CLOCK_TAI(11) reach the CAP_SYS_TIME gate the container lacks -> -EPERM.
    case 112:
        G_RET(c) = ((int)a0 == 0 || (int)a0 == 11) ? (uint64_t)(int64_t)(-EPERM) : (uint64_t)(int64_t)(-EINVAL);
        break;
    case 143: G_RET(c) = setregid((gid_t)a0, (gid_t)a1) < 0 ? (uint64_t)(-errno) : 0; break; // setregid
    case 151: G_RET(c) = (uint64_t)cuid(); break; // setfsuid -> previous fsuid (container uid)
    case 152:
        G_RET(c) = (uint64_t)cgid();
        break; // setfsgid -> previous fsgid
    // getcpu(cpu, node, tcache): report a CPU from the guest's affinity set (LTP getcpu01 pins to one
    // CPU and expects it back), node 0 (single NUMA node). A cpu/node pointer outside the address space
    // -> EFAULT (LTP getcpu02; guest_bad_ptr also catches a PROT_NONE tst_get_bad_addr page).
    case 168: {
        unsigned cpu = hl_linux_affinity_first(&g_affinity, linux_online_cpus()), node = 0;
        if ((a0 && guest_copy_to(a0, &cpu, sizeof cpu) != sizeof cpu) ||
            (a1 && guest_copy_to(a1, &node, sizeof node) != sizeof node)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    case 213: { // readahead: advisory, but Linux still validates the descriptor and offset
        if ((int)a0 < 0 || fcntl((int)a0, F_GETFD) < 0)
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
        else if ((int64_t)a1 < 0)
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
        else
            G_RET(c) = 0;
        break;
    }
    // sched_setattr(pid, attr, flags): validate exactly like the kernel and RECORD the requested
    // policy/priority so sched_getattr (case 275) and sched_getscheduler (proc.c case 120) round-trip it.
    // The old handler blanket-returned success, so sched_getattr kept reporting SCHED_OTHER after a policy
    // change and a real-time policy was "accepted" here even though sched_setscheduler (case 119) rejects it
    // -- the two entry points disagreed. flags (a2) must be 0 (no sched_setattr flags are defined).
    case 274: {
        if (!a1 || a2) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        unsigned char attr[48];
        if (guest_copy_from(attr, a1, sizeof attr) != sizeof attr) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t size;
        memcpy(&size, attr, sizeof size);
        if (size == 0) size = 48; // 0 selects the VER0 struct size, as in the kernel
        if (size < 48) {          // struct sched_attr is 48 bytes; a smaller one is malformed
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t policy;
        uint64_t sflags;
        memcpy(&policy, attr + 4, sizeof policy);
        memcpy(&sflags, attr + 8, sizeof sflags);
        int base = (int)policy & ~HL_SCHED_RESET_ON_FORK, lo, hi;
        if (sched_prio_band(base, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int32_t prio;
        memcpy(&prio, attr + 20, sizeof prio);
        if (prio < lo || prio > hi) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // Real-time classes need a privilege the container's host process lacks -- reject them the same way
        // sched_setscheduler does, so a latency probe never believes RT scheduling was installed via setattr.
        if (base == 1 || base == 2) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        }
        g_sched_policy = base;
        g_sched_prio = prio;
        // sched_nice at +16 (unless SCHED_FLAG_KEEP_PARAMS 0x10). Best-effort apply like setpriority
        // (proc.c case 140): the root container may lower niceness, so clamp to Linux [-20,19] without a
        // can_nice EPERM. This keeps sched_getattr's reported nice in sync with getpriority.
        if (!(sflags & 0x10)) {
            int32_t nice;
            memcpy(&nice, attr + 16, sizeof nice);
            if (nice > 19)
                nice = 19;
            else if (nice < -20)
                nice = -20;
            setpriority(PRIO_PROCESS, 0, nice);
        }
        G_RET(c) = 0;
        break;
    }
    // preadv2/pwritev2: offset in a3 (pos_high a4 is 0 on LP64), RWF_* flags in a5. The flags are
    // semantic requirements (RWF_APPEND/DSYNC/SYNC/NOWAIT/HIPRI), not hints: silently dropping them
    // makes RWF_APPEND write at the supplied offset instead of the end. Honor them via the host
    // preadv2/pwritev2 (Linux host); the macOS build lacks them, so reject any flag there.
    case 286: {
        if (memf_get((int)a0)) {
            if (a5) {
                G_RET(c) = (uint64_t)(int64_t)(-EOPNOTSUPP);
                break;
            }
            memf_materialize((int)a0);
        }
#if !defined(__linux__)
        if (a5) {
            G_RET(c) = (uint64_t)(int64_t)(-EOPNOTSUPP);
            break;
        }
#endif
        ssize_t r = guest_fd_vector_flags((int)a0, a1, (size_t)a2, (off_t)a3, 1, 1, (int)a5);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 287: {
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && (memfd_seals_fd((int)a0) & 0x8)) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        } // F_SEAL_WRITE
        if (memf_get((int)a0)) {
            struct iovec imported[GUEST_IOV_STACK_MAX];
            if (a2 > GUEST_IOV_STACK_MAX || guest_iov_import(a1, (size_t)a2, imported) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(a2 > GUEST_IOV_STACK_MAX ? -EINVAL : -EFAULT);
                break;
            }
            uint64_t total = 0;
            for (size_t i = 0; i < (size_t)a2; ++i) {
                if (imported[i].iov_len > UINT64_MAX - total) {
                    total = UINT64_MAX;
                    break;
                }
                total += imported[i].iov_len;
            }
            if (memf_fsize_gate(c, (off_t)a3, total) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFBIG);
                break;
            }
            if (a5) {
                G_RET(c) = (uint64_t)(int64_t)(-EOPNOTSUPP);
                break;
            }
            memf_materialize((int)a0);
        }
#if !defined(__linux__)
        // macOS lacks pwritev2. RWF_APPEND (0x10) is a semantic requirement: ignore the supplied offset
        // and land the write at end-of-file. Emulate it via fstat + pwritev at st_size; reject any other
        // RWF_* flag we cannot honor. (Linux keeps the native pwritev2 path above.)
        off_t hl_off = (off_t)a3;
        unsigned hl_rwf = (unsigned)a5;
        if (hl_rwf & 0x10u) { // RWF_APPEND
            struct stat hl_st;
            if (fstat((int)a0, &hl_st) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            hl_off = hl_st.st_size;
            hl_rwf &= ~0x10u;
        }
        if (hl_rwf) {
            G_RET(c) = (uint64_t)(int64_t)(-EOPNOTSUPP);
            break;
        }
        ssize_t r = guest_fd_vector_flags((int)a0, a1, (size_t)a2, hl_off, 1, 0, 0);
#else
        ssize_t r = guest_fd_vector_flags((int)a0, a1, (size_t)a2, (off_t)a3, 1, 0, (int)a5);
#endif
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
            if (a2 && guest_accessible_prefix(a2, 128, HL_LOGICAL_VMA_WRITE) != 128) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            // Raw waitid(2) also copies out arg 5 (struct rusage *) when non-NULL; a bad pointer is EFAULT.
            if (a4 && guest_accessible_prefix(a4, 144, HL_LOGICAL_VMA_WRITE) != 144) {
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
        uint8_t linux_siginfo[128] = {0};
        uint8_t *gi = linux_siginfo;
        if (a2) {
            // Linux siginfo_t is 128 bytes; zero it (also the WNOHANG "no child" case -> si_pid stays 0)
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
            if (guest_copy_to(a2, linux_siginfo, sizeof linux_siginfo) != sizeof linux_siginfo) {
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
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
            uint8_t linux_ru[144] = {0};
            if (getrusage(RUSAGE_CHILDREN, &cru) == 0) rusage_to_linux(linux_ru, &cru);
            if (guest_copy_to(a4, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = 0;
        break;
    }
    // truncate(path, length): resolve the guest path through the overlay (same helper execve uses), then
    // truncate by host path. Evict the stat cache so the new size is observed.
    case 45: {
        char guest_path[4200];
        int imported = guest_copy_string(guest_path, sizeof guest_path, a0);
        if (imported < 0) {
            G_RET(c) = (uint64_t)(int64_t)imported;
            break;
        }
        if (jail_ro_at(-100, guest_path)) {
            G_RET(c) = (uint64_t)(int64_t)(-EROFS);
            break;
        }
        char pb[4200];
        const char *p = xresolve_overlay(guest_path, pb, sizeof pb);
        // RLIMIT_FSIZE: a truncate whose target length exceeds the soft file-size limit raises SIGXFSZ and
        // returns -EFBIG. Linux resolves the path FIRST (a missing file is ENOENT with no signal), so only
        // gate once the target is known to exist. No-op for an infinite limit (the common case).
        {
            uint64_t fslim = guest_fsize_cur();
            struct stat tst;
            if (fslim != ~UINT64_C(0) && a1 > fslim && stat(p, &tst) == 0) {
                raise_guest_signal(c, 25); // SIGXFSZ
                G_RET(c) = (uint64_t)(int64_t)(-EFBIG);
                break;
            }
        }
        int r = truncate(p, (off_t)a1);
        if (r >= 0) hl_fdcache_metadata_evict(p);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    // getrlimit(resource, rlim) / setrlimit(resource, rlim): alias prlimit64 (case 261, svc_fill_rlimit).
    // RLIMIT_STACK(3) reports 8MB, RLIMIT_NOFILE(7) a finite fd cap, everything else unlimited; setrlimit is
    // accepted (no-op).
    case 163:
        if ((int)a0 < 0 || (int)a0 >= HL_LIMIT_COUNT) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
        } else {
            uint64_t limits[2];
            svc_fill_rlimit((int)a0, limits);
            G_RET(c) = guest_copy_to(a1, limits, sizeof limits) == sizeof limits ? 0 : (uint64_t)(int64_t)(-EFAULT);
        }
        break;
    case 164: {
        // setrlimit(resource, rlim): apply into the same store getrlimit/prlimit64 read, so a direct
        // setrlimit(2) (not funneled through prlimit64/case 261 by glibc) also takes effect -- e.g. a guest
        // raising RLIMIT_CORE to enable cores must have wait4/waitid report WCOREDUMP afterwards.
        int res = (int)a0;
        if (res < 0 || res >= HL_LIMIT_COUNT) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint64_t nl[2];
        if (guest_copy_from(nl, a1, sizeof nl) != sizeof nl) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (nl[0] != ~UINT64_C(0) && nl[1] != ~UINT64_C(0) && nl[0] > nl[1]) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        hl_limit_table_set(&g_limits, res, nl[0], nl[1]);
        G_RET(c) = 0;
        break; // setrlimit -> accepted
    }

    // adjtimex(2)/clock_adjtime(2): read-only query fills struct timex + TIME_OK; setting -> EPERM.
    case 266: { // clock_adjtime(clk_id, timex)
        // Validate the clock id first: an unknown/dynamic id is -EINVAL on Linux, not a silent read.
        if ((int)a0 < 0 || (int)a0 > 11) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint8_t timex[96];
        if (guest_copy_from(timex, a1, sizeof timex) != sizeof timex) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        int r = svc_adjtimex(timex);
        if (r >= 0 && guest_copy_to(a1, timex, sizeof timex) != sizeof timex) r = -EFAULT;
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
        break;
    }
    case 171: { // adjtimex(timex)
        uint8_t timex[96];
        if (guest_copy_from(timex, a0, sizeof timex) != sizeof timex) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        int r = svc_adjtimex(timex);
        if (r >= 0 && guest_copy_to(a0, timex, sizeof timex) != sizeof timex) r = -EFAULT;
        G_RET(c) = r < 0 ? (uint64_t)(int64_t)r : (uint64_t)r;
        break;
    }
    // sched_getattr(pid, attr, size, flags): report the task's LIVE scheduling profile so it agrees with
    // sched_getscheduler/sched_getparam. The old handler hardcoded SCHED_OTHER + nice 0, so sched_getattr
    // disagreed with sched_getscheduler after any policy change (and ignored the process nice). Zero the
    // caller's struct, then fill size + the recorded policy, the live nice, and the recorded priority.
    case 275: {
        if (a1) {
            size_t sz = (size_t)a2;
            if (sz == 0 || sz > 48) sz = 48; // kernel struct sched_attr is 48+ bytes; cap to a sane size
            // The struct is written straight into the guest buffer below; a bad/unmapped attr pointer must
            // EFAULT like the kernel's copy_to_user, not fault the engine on the memset/field stores.
            unsigned char attr[48] = {0};
            if (sz >= 4) memcpy(attr, &(uint32_t){(uint32_t)sz}, 4);
            if (sz >= 8) memcpy(attr + 4, &(uint32_t){(uint32_t)g_sched_policy}, 4);
            if (sz >= 20) {
                errno = 0;
                int nv = getpriority(PRIO_PROCESS, 0); // sched_nice = the live process nice value
                if (nv == -1 && errno) nv = 0;
                memcpy(attr + 16, &nv, sizeof(int32_t));
            }
            if (sz >= 24) memcpy(attr + 20, &(uint32_t){(uint32_t)g_sched_prio}, 4);
            if (guest_copy_to(a1, attr, sz) != (ssize_t)sz) {
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
            }
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
        int rl = hl_gmap_lock_limit_range(a0, (uint64_t)a1);
        if (rl < 0) {
            G_RET(c) = (uint64_t)(int64_t)rl;
            break;
        }
        hl_gmap_lock_add(a0, (uint64_t)a1);
        G_RET(c) = 0;
        break;
    }
    // rt_tgsigqueueinfo(tgid, tid, sig, siginfo): thread-targeted sibling of rt_sigqueueinfo (case 138).
    // Carry si_code + si_value to the guest handler's siginfo, then raise the signal to the guest.
    case 240: {
        int sig = (int)a2;
        // The kernel copy_from_user()s the whole siginfo up front, so a bad (or NULL) user pointer is
        // -EFAULT, never a fault in the engine -- guard the direct deref below to match (as case 138 does).
        uint8_t siginfo[128];
        if (guest_copy_from(siginfo, a3, sizeof siginfo) != sizeof siginfo) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (sig >= 1 && sig <= 64 && a3) {
            memcpy(&g_sigcode[sig], siginfo + 8, sizeof g_sigcode[sig]);
            memcpy(&g_sigval[sig], siginfo + 24, sizeof g_sigval[sig]);
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
        int64_t seconds = (int64_t)t;
        if (a0 && guest_copy_to(a0, &seconds, sizeof seconds) != sizeof seconds) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        G_RET(c) = (uint64_t)(int64_t)t;
        break;
    }
#endif
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
