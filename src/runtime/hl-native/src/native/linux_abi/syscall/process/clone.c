// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int svc_proc_220(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 220: {
        // Dynamic Linux namespaces are not yet modeled by the embedded ABI.
        // Silently accepting these bits is observably wrong: callers publish
        // namespace-specific state after a successful clone even though the
        // child remains in the original namespace. Return EINVAL, the kernel
        // contract for unsupported clone flags, so capability probes can use
        // their documented fallback.
        const uint64_t namespace_flags = 0x00020000ull | // CLONE_NEWNS
                                         0x02000000ull | // CLONE_NEWCGROUP
                                         0x04000000ull | // CLONE_NEWUTS
                                         0x08000000ull | // CLONE_NEWIPC
                                         0x10000000ull | // CLONE_NEWUSER
                                         0x20000000ull | // CLONE_NEWPID
                                         0x40000000ull;  // CLONE_NEWNET
        if (a0 & namespace_flags) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // CLONE_THREAD: stack arg IS the top
        if (a0 & 0x10000) {
            G_RET(c) = (uint64_t)spawn_thread(c, a0, a1, a3, a2, a4);
            break;
        }
        // cgroup pids.max also gates a FORKED PROCESS: a forked child is a new container task, so a fork
        // past the limit must fail EAGAIN exactly as clone(CLONE_THREAD) does (the container-wide count is
        // one shared budget across the process tree). Previously only in-process threads were gated.
        if (g_pids_max && acct_pids_total() >= g_pids_max) {
            G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
            break;
        }
        int share_fs = (a0 & 0x00000200ull) != 0; // CLONE_FS
        int fs_status = share_fs ? guest_fs_share() : 0;
        if (fs_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)fs_status;
            break;
        }
        // fork/vfork: COW copy; child continues. Flush RAM-backed scratch into the real (shared) fds so
        // parent and child see one coherent file via the inherited description, exactly as POSIX requires
        // (the heap-resident buffers would otherwise COW-diverge while the fd stays shared).
        memf_materialize_all();
        sigexit_init(); // create the shared guest-signal-death relay in the PARENT before forking, so
                        // this child (and its descendants) inherit the same MAP_SHARED page it may die into.
        bound_fork_state bound_fork;
        int bound_status = bound_fork_prepare(&bound_fork);
        if (bound_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        int vfork_pipe[2] = {-1, -1};
        int vfork_ack[2] = {-1, -1};
        int is_vfork = (a0 & 0x4000) != 0;
        if (is_vfork && (pipe(vfork_pipe) != 0 || pipe(vfork_ack) != 0)) {
            bound_fork_complete(&bound_fork, 0, -1);
            G_RET(c) = (uint64_t)(int64_t)(-errno);
            break;
        }
        if (is_vfork) {
            (void)fcntl(vfork_pipe[0], F_SETFD, FD_CLOEXEC);
            (void)fcntl(vfork_pipe[1], F_SETFD, FD_CLOEXEC);
        }
        int runtime_source_tid = cpu_tid(c);
        if (!hl_target_task_event(c, HL_TASK_EVENT_PREPARE_FORK, 0, (uint64_t)runtime_source_tid, 0)) {
            (void)bound_fork_complete(&bound_fork, 0, -1);
            if (is_vfork) {
                close(vfork_pipe[0]);
                close(vfork_pipe[1]);
            }
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        pid_t pid = fork();
        int fork_error = errno;
        if (is_vfork) {
            if (pid == 0) {
                close(vfork_pipe[0]);
                close(vfork_ack[1]);
                g_vfork_release_fd = vfork_pipe[1];
                g_vfork_ack_fd = vfork_ack[0];
            } else {
                close(vfork_pipe[1]);
                close(vfork_ack[0]);
            }
        }
        bound_status = bound_fork_complete(&bound_fork, pid == 0, pid == 0 ? (int)getpid() : (int)pid);
        if (bound_status != 0) {
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            if (pid == 0) _exit(127);
            if (pid > 0) {
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
            }
            if (is_vfork && vfork_pipe[0] >= 0) close(vfork_pipe[0]);
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        if (pid < 0) (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
        errno = fork_error;
        if (pid >= 0) {
            uint64_t child_pid = (uint64_t)(pid == 0 ? getpid() : pid);
            uint64_t source_tid = (uint64_t)runtime_source_tid;
            if (!hl_target_task_event(c, HL_TASK_EVENT_FORK_PROCESS, child_pid, source_tid, pid == 0)) {
                if (pid == 0) _exit(127);
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
                G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
                break;
            }
        }
        if (pid == 0) {
            guest_fs_after_fork(share_fs);
            // clone(child_stack): Linux resumes the child on the supplied stack regardless of CLONE_VM.
            // glibc seeds its clone trampoline (fn ptr + arg) there before the syscall. Restricting this to
            // CLONE_VM made an ordinary process clone resume on the parent's stack: the child then popped a
            // return address as its callback and returned into the caller with cleared callee-saved state.
            // a1==0 is the fork-compatible form and keeps the inherited stack.
            if (a1) G_SP(c) = a1;
            fork_child_hooks(c); // shared child-side engine reset (cache re-alias, caches, kqueues, locks)
            // CLONE_CHILD_SETTID(0x01000000): store the child's own tid (== its pid for a process clone) into
            // the child's *ctid (a4). CLONE_CHILD_CLEARTID(0x00200000): remember ctid so thread/process exit
            // zeroes it and FUTEX_WAKEs a joiner. The old code ignored both, so pthread/runtime handshakes
            // that read the child tid from these slots saw stale memory.
            if ((a0 & 0x01000000) && a4) {
                int tid = (int)getpid();
                (void)guest_copy_to(a4, &tid, sizeof tid);
            }
            if (a0 & 0x00200000) c->ctid = a4;
        }
        // CLONE_PIDFD(0x1000): the kernel stores a pidfd for the new child at the address in `parent_tid`
        // (a2, the aarch64 clone slot). Mint a host pollable process watch through pidfd_make; modern runtimes
        // (Go/Rust/glibc posix_spawn) then epoll_wait/poll THAT fd to reap the
        // compiler child they just forked. Without it the guest's pidfd storage keeps its stale value (Go
        // seeds it 0 -> fd 0 = stdin) and the wait blocks forever at 0% CPU -- the go/npm/cargo build hang.
        if (pid > 0 && (a0 & 0x1000) && a2) {
            int pfd = pidfd_make(pid);
            if (pfd >= 0 && guest_copy_to(a2, &pfd, sizeof pfd) != sizeof pfd) {
                close(pfd);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (pid > 0) { // parent side of a successful fork: count it for /proc/stat processes + pids.current
            atomic_fetch_add(&g_forks_since_boot, 1);
            proc_reg_mark_child((int)pid); // guest-pid namespace: register the child NOW (parent-side, race-
                                           // free) so a kill/pidfd membership check can never ESRCH it before
                                           // it runs its own proc_reg_after_fork publish
            acct_child_born((int)pid);     // register the child's OWN task slot (container-wide pids.current)
        }
        if (pid > 0 && is_vfork) {
            unsigned char committed;
            ssize_t received;
            do
                received = read(vfork_pipe[0], &committed, sizeof committed);
            while (received < 0 && errno == EINTR);
            if (received == 1 && committed == 2) {
                vfork_import_guest_memory(pid);
                while (write(vfork_ack[1], &committed, sizeof committed) < 0 && errno == EINTR) {}
            }
            close(vfork_pipe[0]);
            close(vfork_ack[1]);
        } else if (pid < 0 && is_vfork) {
            close(vfork_pipe[0]);
            close(vfork_ack[1]);
        }
        // CLONE_PARENT_SETTID(0x00100000): store the child's tid (its pid) into the PARENT's *ptid (a2).
        // Mutually exclusive with CLONE_PIDFD (which also uses the ptid slot), so it never clobbers a pidfd.
        if (pid > 0 && (a0 & 0x00100000) && !(a0 & 0x1000) && a2) {
            int parent_tid = (int)pid;
            if (guest_copy_to(a2, &parent_tid, sizeof parent_tid) != sizeof parent_tid) {
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
            }
        }
        // parent: pid, child: 0
        G_RET(c) = pid < 0 ? (uint64_t)(-errno) : (uint64_t)pid;
        // A fork/vfork that was normalized to clone repurposed the guest's arg registers; put them back so
        // the syscall preserves every GPR but rax, as the real kernel does (no-op for a genuine clone).
        G_FORK_PRESERVE(c);
        break;
    }
    // execveat(dirfd, path, argv, envp, flags) -- canonical 281 (x86 322 maps here via sysmap). Resolve
    // (dirfd, path, flags) to a guest-absolute exec path, shift the args into execve positions, and fall
    // through to the shared case-221 body (glibc fexecve() and Rust/Go re-exec helpers use this).
    default: return 0;
    }
    return 1;
}

static int execveat_empty_path(int dirfd, char *path, size_t capacity) {
    char host[4200];
    if (dirfd >= 0 && dirfd < HL_NFD && g_fdpath[dirfd][0])
        snprintf(host, sizeof host, "%s", g_fdpath[dirfd]);
    else if (dirfd < 0 || hl_native_fd_path(dirfd, host, sizeof host) != 0 || !host[0])
        return dirfd < 0 ? -EBADF : -EACCES;
    if (!g_rootfs) {
        snprintf(path, capacity, "%s", host);
        return 0;
    }
    char guest[4200];
    int mapped = guest_from_host_raw(host, guest, sizeof guest);
    if (mapped <= 0) return mapped < 0 ? mapped : -EACCES;
    snprintf(path, capacity, "%s", guest);
    return 0;
}

static int execveat_named_path(int dirfd, const char *source, int flags, char *path, size_t capacity) {
    if (flags & 0x100) {
        char buffer[4200];
        struct stat status;
        const char *local = atpath(dirfd, source, buffer, sizeof buffer, 1);
        if (fstatat(ATFD(dirfd), local, &status, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(status.st_mode)) return -ELOOP;
    }
    if (source[0] == '/' || (!g_rootfs && dirfd == -100)) {
        snprintf(path, capacity, "%s", source);
        return 0;
    }
    if (g_rootfs) {
        abs_guest(dirfd, source, path, capacity);
        return 0;
    }
    char host[4200];
    if (dirfd >= 0 && dirfd < HL_NFD && g_fdpath[dirfd][0])
        snprintf(host, sizeof host, "%s", g_fdpath[dirfd]);
    else if (hl_native_fd_path(dirfd, host, sizeof host) != 0)
        return -EBADF;
    return path_join(path, capacity, host, source) == 0 ? 0 : -ENAMETOOLONG;
}

static int svc_proc_281(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    (void)nr;
    (void)a5;
    static char path[4200];
    const char *source = (const char *)a1;
    int error;
    if (source && !source[0] && ((int)a4 & 0x1000))
        error = execveat_empty_path((int)a0, path, sizeof path);
    else if (!source || !source[0])
        error = -ENOENT;
    else
        error = execveat_named_path((int)a0, source, (int)a4, path, sizeof path);
    if (error != 0) {
        G_RET(c) = (uint64_t)(int64_t)error;
        return 1;
    }
    return svc_proc_221(c, 221, (uint64_t)(uintptr_t)path, a2, a3, 0, 0, 0);
}

static int svc_proc_435(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 435: {
        // clone3(clone_args*, size): a hostile/buggy guest can pass a bad args pointer or a junk size;
        // validate BEFORE any deref so it returns an errno instead of faulting the engine. -EINVAL if size
        // is below the VER0 clone_args (we read only its first 64 bytes) or implausibly large; -EFAULT if
        // the args struct isn't mapped.
        if (a1 < 64 || a1 > 4096) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint64_t ca[8];
        if (guest_copy_from(ca, a0, sizeof ca) != sizeof ca) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t flags = ca[0];
        const uint64_t namespace_flags = 0x00020000ull | 0x02000000ull | 0x04000000ull | 0x08000000ull | 0x10000000ull |
                                         0x20000000ull | 0x40000000ull;
        if (flags & namespace_flags) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // CLONE_THREAD: sp = stack + stack_size
        if (flags & 0x10000) {
            G_RET(c) = (uint64_t)spawn_thread(c, flags, ca[5] + ca[6], ca[7], ca[3], ca[2]);
            break;
        }
        // cgroup pids.max gates a clone3 forked PROCESS too (see case 220): fork past the limit -> EAGAIN.
        if (g_pids_max && acct_pids_total() >= g_pids_max) {
            G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
            break;
        }
        int share_fs = (flags & 0x00000200ull) != 0; // CLONE_FS
        int fs_status = share_fs ? guest_fs_share() : 0;
        if (fs_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)fs_status;
            break;
        }
        sigexit_init(); // shared signal-death relay must exist in the parent before fork (see case 220)
        bound_fork_state bound_fork;
        int bound_status = bound_fork_prepare(&bound_fork);
        if (bound_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        int runtime_source_tid = cpu_tid(c);
        if (!hl_target_task_event(c, HL_TASK_EVENT_PREPARE_FORK, 0, (uint64_t)runtime_source_tid, 0)) {
            (void)bound_fork_complete(&bound_fork, 0, -1);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        pid_t pid = fork();
        int fork_error = errno;
        bound_status = bound_fork_complete(&bound_fork, pid == 0, pid == 0 ? (int)getpid() : (int)pid);
        if (bound_status != 0) {
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            if (pid == 0) _exit(127);
            if (pid > 0) {
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
            }
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        if (pid < 0)
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
        else {
            uint64_t child_pid = (uint64_t)(pid == 0 ? getpid() : pid);
            if (!hl_target_task_event(c, HL_TASK_EVENT_FORK_PROCESS, child_pid, (uint64_t)runtime_source_tid,
                                      pid == 0)) {
                if (pid == 0) _exit(127);
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
                G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
                break;
            }
        }
        errno = fork_error;
        // child: the same shared engine reset as the clone/fork site above (cache re-alias / §B shadow /
        // path caches / kqueues / fork-unsafe locks). clone3 historically lacked the W^X re-assert and the
        // DIR*-cache drop the clone site had; the shared helper closes that drift.
        if (pid == 0) {
            guest_fs_after_fork(share_fs);
            if ((flags & 0x100) && ca[5]) G_SP(c) = ca[5] + ca[6];
            fork_child_hooks(c);
            // clone_args: child_tid = ca[2]. CLONE_CHILD_SETTID stores the child's tid there; CLONE_CHILD_
            // CLEARTID remembers it so exit zeroes it + wakes a joiner (mirrors case 220).
            if ((flags & 0x01000000) && ca[2]) {
                int tid = (int)getpid();
                (void)guest_copy_to(ca[2], &tid, sizeof tid);
            }
            if (flags & 0x00200000) c->ctid = ca[2];
        }
        // CLONE_PIDFD: clone3 stores the child pidfd via the `pidfd` field (clone_args[1]); back it the same
        // way as case 220 so a clone3-based spawn (newer glibc/runtimes) can epoll_wait/poll it to reap.
        if (pid > 0 && (flags & 0x1000) && ca[1]) {
            int pfd = pidfd_make(pid);
            if (pfd >= 0 && guest_copy_to(ca[1], &pfd, sizeof pfd) != sizeof pfd) {
                close(pfd);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (pid > 0) { // parent side of a successful clone3 fork: count it (see case 220)
            atomic_fetch_add(&g_forks_since_boot, 1);
            proc_reg_mark_child((int)pid); // guest-pid namespace: parent-side registration (see case 220)
            acct_child_born((int)pid);     // register the child's OWN task slot (container-wide pids.current)
        }
        // clone_args: parent_tid = ca[3]. CLONE_PARENT_SETTID stores the child's tid (pid) into the PARENT's
        // parent_tid (a distinct field from pidfd in clone3, so it never conflicts with CLONE_PIDFD).
        if (pid > 0 && (flags & 0x00100000) && ca[3]) {
            int tid = (int)pid;
            if (guest_copy_to(ca[3], &tid, sizeof tid) != sizeof tid) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = pid < 0 ? (uint64_t)(-errno) : (uint64_t)pid;
        break;
    }
    default: return 0;
    }
    return 1;
}
