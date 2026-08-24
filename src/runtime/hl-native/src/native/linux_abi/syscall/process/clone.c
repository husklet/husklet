// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
#include "../../../host/process.h"

static int clone_has_namespace_flags(uint64_t flags) {
    const uint64_t namespace_flags = 0x00020000ull | // CLONE_NEWNS
                                     0x02000000ull | // CLONE_NEWCGROUP
                                     0x04000000ull | // CLONE_NEWUTS
                                     0x08000000ull | // CLONE_NEWIPC
                                     0x10000000ull | // CLONE_NEWUSER
                                     0x20000000ull | // CLONE_NEWPID
                                     0x40000000ull;  // CLONE_NEWNET
    return (flags & namespace_flags) != 0;
}

static int svc_proc_220(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 220: {
        // Dynamic Linux namespaces are not yet modeled by the embedded ABI.
        // Silently accepting these bits is observably wrong: callers publish
        // namespace-specific state after a successful clone even though the
        // child remains in the original namespace. Return EINVAL, the kernel
        // contract for unsupported clone flags, so capability probes can use
        // their documented fallback.
        if (clone_has_namespace_flags(a0)) {
            fork_diagnostic_emit(c, nr, a0, "namespace-flags", EINVAL, -1, NULL);
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // CLONE_THREAD: stack arg IS the top
        if (a0 & 0x10000) {
            int64_t result = spawn_thread(c, a0, a1, a3, a2, a4);
            if (result < 0) fork_diagnostic_emit(c, nr, a0, "thread-spawn", (int)-result, -1, NULL);
            G_RET(c) = (uint64_t)result;
            break;
        }
        // cgroup pids.max also gates a FORKED PROCESS: a forked child is a new container task, so a fork
        // past the limit must fail EAGAIN exactly as clone(CLONE_THREAD) does (the container-wide count is
        // one shared budget across the process tree). Previously only in-process threads were gated.
        int pids_total = g_pids_max ? acct_pids_total() : -1;
        if (g_pids_max && pids_total >= g_pids_max) {
            fork_diagnostic_emit(c, nr, a0, "pids-limit", EAGAIN, pids_total, NULL);
            G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
            break;
        }
        int share_fs = (a0 & 0x00000200ull) != 0; // CLONE_FS
        int fs_status = share_fs ? guest_fs_share() : 0;
        if (fs_status != 0) {
            fork_diagnostic_emit(c, nr, a0, "share-fs", -fs_status, pids_total, NULL);
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
            fork_diagnostic_emit(c, nr, a0, "snapshot-prepare", -bound_status, pids_total, &bound_fork);
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        int vfork_pipe[2] = {-1, -1};
        int vfork_ack[2] = {-1, -1};
        int is_vfork = (a0 & 0x4000) != 0;
        // The host implements a guest vfork with process-private COW memory. On
        // child _exit we import that memory to preserve Linux's shared-address-
        // space behavior, but only before the process has acquired threading
        // authority. g_ever_threaded is published before pthread_create, so
        // unlike a live-registry count it also covers a peer in its registration
        // window, and its atomic acquire cannot race or observe a stale zero.
        // Importing a fork-time snapshot
        // into a live multithreaded parent rolls allocator and application state
        // backward (a failed posix_spawn followed by malloc hit glibc's heap
        // consistency abort). The only defined multithreaded vfork-child actions
        // are exec/_exit, neither of which requires child writes to be published.
        int import_vfork_exit_memory = is_vfork && !atomic_load_explicit(&g_ever_threaded, memory_order_acquire);
        if (is_vfork && (pipe(vfork_pipe) != 0 || pipe(vfork_ack) != 0)) {
            int pipe_error = errno;
            fork_diagnostic_emit(c, nr, a0, "vfork-pipe", pipe_error, pids_total, &bound_fork);
            fork_diagnostic_close_pair(vfork_pipe);
            fork_diagnostic_close_pair(vfork_ack);
            bound_fork_complete(&bound_fork, 0, -1);
            G_RET(c) = (uint64_t)(int64_t)(-pipe_error);
            break;
        }
        if (is_vfork) {
            (void)fcntl(vfork_pipe[0], F_SETFD, FD_CLOEXEC);
            (void)fcntl(vfork_pipe[1], F_SETFD, FD_CLOEXEC);
        }
        int runtime_source_tid = cpu_tid(c);
        if (!hl_target_task_event(c, HL_TASK_EVENT_PREPARE_FORK, 0, (uint64_t)runtime_source_tid, 0)) {
            fork_diagnostic_emit(c, nr, a0, "task-prepare", EAGAIN, pids_total, &bound_fork);
            (void)bound_fork_complete(&bound_fork, 0, -1);
            fork_diagnostic_close_pair(vfork_pipe);
            fork_diagnostic_close_pair(vfork_ack);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        int guest_child_pid = hl_linux_pidmap_is_active(&g_pidmap) ? (int)hl_linux_pidmap_allocate_guest(&g_pidmap) : 0;
        if (hl_linux_pidmap_is_active(&g_pidmap) && guest_child_pid <= 0) {
            fork_diagnostic_emit(c, nr, a0, "pid-allocate", EAGAIN, pids_total, &bound_fork);
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            (void)bound_fork_complete(&bound_fork, 0, -1);
            fork_diagnostic_close_pair(vfork_pipe);
            fork_diagnostic_close_pair(vfork_ack);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        pid_t pid = hl_host_process_clone_current();
        int fork_error = errno;
        if (pid < 0) fork_diagnostic_emit(c, nr, a0, "host-fork", fork_error, pids_total, &bound_fork);
        guest_child_pid =
            pid < 0 ? -1 : restore_process_identity_publish(guest_child_pid, pid == 0 ? (int)getpid() : (int)pid);
        if (pid >= 0 && guest_child_pid <= 0) {
            if (pid == 0) _exit(127);
            fork_diagnostic_emit(c, nr, a0, "identity-publish", EAGAIN, pids_total, &bound_fork);
            kill(pid, SIGKILL);
            while (waitpid(pid, NULL, 0) < 0 && errno == EINTR) {}
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            (void)bound_fork_complete(&bound_fork, 0, -1);
            fork_diagnostic_close_pair(vfork_pipe);
            fork_diagnostic_close_pair(vfork_ack);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        if (is_vfork) {
            if (pid == 0) {
                fork_diagnostic_close_descriptor(&vfork_pipe[0]);
                fork_diagnostic_close_descriptor(&vfork_ack[1]);
                g_vfork_release_fd = vfork_pipe[1];
                g_vfork_ack_fd = vfork_ack[0];
                vfork_pipe[1] = -1;
                vfork_ack[0] = -1;
            } else {
                fork_diagnostic_close_descriptor(&vfork_pipe[1]);
                fork_diagnostic_close_descriptor(&vfork_ack[0]);
            }
        }
        bound_status = bound_fork_complete(&bound_fork, pid == 0, pid == 0 ? (int)getpid() : (int)pid);
        if (bound_status != 0) {
            fork_diagnostic_emit(c, nr, a0, "snapshot-complete", -bound_status, pids_total, &bound_fork);
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            if (pid == 0) _exit(127);
            if (pid > 0) {
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
            }
            fork_diagnostic_close_pair(vfork_pipe);
            fork_diagnostic_close_pair(vfork_ack);
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
                fork_diagnostic_emit(c, nr, a0, "task-publish", EAGAIN, pids_total, &bound_fork);
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
                (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, source_tid, 0);
                fork_diagnostic_close_pair(vfork_pipe);
                fork_diagnostic_close_pair(vfork_ack);
                G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
                break;
            }
        }
        if (pid == 0) {
            if (hl_linux_pidmap_is_active(&g_pidmap)) {
                g_self_gpid = guest_child_pid;
                // The child's parent is THIS process, not the one recorded for it. Clearing the inherited
                // value sends getppid() back through the live map (identity.c), which is both correct here
                // and correct after the parent is reaped and the host reparents us out of the container.
                g_self_gppid = -1;
            }
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
                int tid = guest_child_pid;
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
            host_pid_register_child((int)pid); // host registry: register the child NOW (parent-side, race-
                                               // free) so a kill/pidfd membership check can never ESRCH it before
                                               // it runs its own proc_reg_after_fork publish
            acct_child_born((int)pid);         // register the child's OWN task slot (container-wide pids.current)
        }
        if (pid > 0 && is_vfork) {
            unsigned char committed;
            ssize_t received;
            do
                received = read(vfork_pipe[0], &committed, sizeof committed);
            while (received < 0 && errno == EINTR);
            if (received == 1 && committed == 2) {
                if (import_vfork_exit_memory) vfork_import_guest_memory(pid);
                // The child waits for this acknowledgement before exiting so
                // process_vm_readv could inspect it. Release it even when a live
                // peer makes importing the snapshot unsafe.
                while (write(vfork_ack[1], &committed, sizeof committed) < 0 && errno == EINTR) {}
            }
            fork_diagnostic_close_descriptor(&vfork_pipe[0]);
            fork_diagnostic_close_descriptor(&vfork_ack[1]);
        } else if (pid < 0 && is_vfork) {
            fork_diagnostic_close_pair(vfork_pipe);
            fork_diagnostic_close_pair(vfork_ack);
        }
        // CLONE_PARENT_SETTID(0x00100000): store the child's tid (its pid) into the PARENT's *ptid (a2).
        // Mutually exclusive with CLONE_PIDFD (which also uses the ptid slot), so it never clobbers a pidfd.
        if (pid > 0 && (a0 & 0x00100000) && !(a0 & 0x1000) && a2) {
            int parent_tid = guest_child_pid;
            if (guest_copy_to(a2, &parent_tid, sizeof parent_tid) != sizeof parent_tid) {
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
            }
        }
        // parent: pid, child: 0
        G_RET(c) = pid < 0 ? (uint64_t)(-errno) : (uint64_t)(pid == 0 ? 0 : guest_child_pid);
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
    if (dirfd >= 0 && dirfd < HL_NFD && g_fdpath[dirfd][0] && g_fdpath_guest[dirfd]) {
        snprintf(path, capacity, "%s", g_fdpath[dirfd]);
        return 0;
    }
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
    int owned_descriptor = -1;
    int error;
    // Linux validates the complete flag word before resolving either the pathname or dirfd. The only
    // accepted bits are AT_SYMLINK_NOFOLLOW and AT_EMPTY_PATH; silently ignoring another bit can execute
    // a program after the caller's execveat was required to fail with EINVAL.
    if ((int)a4 & ~(0x100 | 0x1000))
        error = -EINVAL;
    else if (source && !source[0] && ((int)a4 & 0x1000)) {
        // AT_EMPTY_PATH names the open file description, not the path which happened to open it. Pin that
        // description before pathname bookkeeping: the guest may have unlinked or replaced the directory
        // entry, and reopening g_fdpath would then validate and execute a different image. The exec request
        // owns this duplicate on success and failure; keeping it non-CLOEXEC also lets it survive the manual
        // close-on-exec sweep until the immutable exec image has finished loading.
        owned_descriptor = bound_exec_descriptor((int)a0);
        error = owned_descriptor < 0 ? -errno : execveat_empty_path((int)a0, path, sizeof path);
    } else if (!source || !source[0])
        error = -ENOENT;
    else
        error = execveat_named_path((int)a0, source, (int)a4, path, sizeof path);
    if (error != 0) {
        if (owned_descriptor >= 0) close(owned_descriptor);
        G_RET(c) = (uint64_t)(int64_t)error;
        return 1;
    }
    return svc_proc_exec(c, path, a2, a3, owned_descriptor);
}

static int clone3_decode(const uint8_t *imported, size_t size, uint64_t arguments[11]) {
    if (size < 64) return -EINVAL;
    if (size > 4096) return -E2BIG;
    memset(arguments, 0, 11 * sizeof *arguments);
    size_t known = size < 11 * sizeof *arguments ? size : 11 * sizeof *arguments;
    memcpy(arguments, imported, known);
    for (size_t index = 11 * sizeof *arguments; index < size; ++index)
        if (imported[index] != 0) return -E2BIG;
    if (arguments[8] != 0 || arguments[9] != 0 || arguments[10] != 0 ||
        (arguments[0] & UINT64_C(0x200000000)) != 0)
        return -EOPNOTSUPP;
    return 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int g_clone3_test_block_fork;
static int g_clone3_test_fork_attempts;
#endif

static int svc_proc_435(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 435: {
        // clone3(clone_args*, size): a hostile/buggy guest can pass a bad args pointer or a junk size;
        // validate BEFORE any fork so a versioned field we cannot honour never creates a child.
        if (a1 < 64 || a1 > 4096) {
            int size_error = a1 < 64 ? EINVAL : E2BIG;
            fork_diagnostic_emit(c, nr, 0, "clone3-size", size_error, -1, NULL);
            G_RET(c) = (uint64_t)(int64_t)-size_error;
            break;
        }
        uint8_t imported[4096];
        if (guest_copy_from(imported, a0, (size_t)a1) != (ssize_t)a1) {
            fork_diagnostic_emit(c, nr, 0, "clone3-arguments", EFAULT, -1, NULL);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t ca[11];
        int extension = clone3_decode(imported, (size_t)a1, ca);
        if (extension == -E2BIG) {
            fork_diagnostic_emit(c, nr, ca[0], "clone3-future-fields", E2BIG, -1, NULL);
            G_RET(c) = (uint64_t)(int64_t)(-E2BIG);
            break;
        }
        // clone_args v1/v2 request PID selection and cgroup placement. Silently ignoring either reports a
        // successful child with a different identity/placement than Linux. This runtime cannot honour those
        // capabilities yet, so fail before any task/fd/accounting publication. EOPNOTSUPP is a documented
        // CLONE_INTO_CGROUP result and is a fail-loud capability verdict for both extensions.
        if (extension != 0) {
            fork_diagnostic_emit(c, nr, ca[0], "clone3-extended-fields", EOPNOTSUPP, -1, NULL);
            G_RET(c) = (uint64_t)(int64_t)(-EOPNOTSUPP);
            break;
        }
#if defined(HL_NATIVE_TEST_HOOKS)
        /* A refusal test arms this boundary rather than allowing its deliberate mutation to fork. */
        if (g_clone3_test_block_fork) {
            g_clone3_test_fork_attempts++;
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
#endif
        uint64_t flags = ca[0];
        const uint64_t namespace_flags = 0x00020000ull | 0x02000000ull | 0x04000000ull | 0x08000000ull | 0x10000000ull |
                                         0x20000000ull | 0x40000000ull;
        if (flags & namespace_flags) {
            fork_diagnostic_emit(c, nr, flags, "namespace-flags", EINVAL, -1, NULL);
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // CLONE_THREAD: sp = stack + stack_size
        if (flags & 0x10000) {
            int64_t result = spawn_thread(c, flags, ca[5] + ca[6], ca[7], ca[3], ca[2]);
            if (result < 0) fork_diagnostic_emit(c, nr, flags, "thread-spawn", (int)-result, -1, NULL);
            G_RET(c) = (uint64_t)result;
            break;
        }
        // cgroup pids.max gates a clone3 forked PROCESS too (see case 220): fork past the limit -> EAGAIN.
        int pids_total = g_pids_max ? acct_pids_total() : -1;
        if (g_pids_max && pids_total >= g_pids_max) {
            fork_diagnostic_emit(c, nr, flags, "pids-limit", EAGAIN, pids_total, NULL);
            G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
            break;
        }
        int share_fs = (flags & 0x00000200ull) != 0; // CLONE_FS
        int fs_status = share_fs ? guest_fs_share() : 0;
        if (fs_status != 0) {
            fork_diagnostic_emit(c, nr, flags, "share-fs", -fs_status, pids_total, NULL);
            G_RET(c) = (uint64_t)(int64_t)fs_status;
            break;
        }
        sigexit_init(); // shared signal-death relay must exist in the parent before fork (see case 220)
        bound_fork_state bound_fork;
        int bound_status = bound_fork_prepare(&bound_fork);
        if (bound_status != 0) {
            fork_diagnostic_emit(c, nr, flags, "snapshot-prepare", -bound_status, pids_total, &bound_fork);
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        int runtime_source_tid = cpu_tid(c);
        if (!hl_target_task_event(c, HL_TASK_EVENT_PREPARE_FORK, 0, (uint64_t)runtime_source_tid, 0)) {
            (void)bound_fork_complete(&bound_fork, 0, -1);
            fork_diagnostic_emit(c, nr, flags, "task-prepare", EAGAIN, pids_total, &bound_fork);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        int guest_child_pid = hl_linux_pidmap_is_active(&g_pidmap) ? (int)hl_linux_pidmap_allocate_guest(&g_pidmap) : 0;
        if (hl_linux_pidmap_is_active(&g_pidmap) && guest_child_pid <= 0) {
            fork_diagnostic_emit(c, nr, flags, "pid-allocate", EAGAIN, pids_total, &bound_fork);
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            (void)bound_fork_complete(&bound_fork, 0, -1);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        pid_t pid = hl_host_process_clone_current();
        int fork_error = errno;
        if (pid < 0) fork_diagnostic_emit(c, nr, flags, "host-fork", fork_error, pids_total, &bound_fork);
        guest_child_pid =
            pid < 0 ? -1 : restore_process_identity_publish(guest_child_pid, pid == 0 ? (int)getpid() : (int)pid);
        if (pid >= 0 && guest_child_pid <= 0) {
            if (pid == 0) _exit(127);
            fork_diagnostic_emit(c, nr, flags, "identity-publish", EAGAIN, pids_total, &bound_fork);
            kill(pid, SIGKILL);
            while (waitpid(pid, NULL, 0) < 0 && errno == EINTR) {}
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            (void)bound_fork_complete(&bound_fork, 0, -1);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        bound_status = bound_fork_complete(&bound_fork, pid == 0, pid == 0 ? (int)getpid() : (int)pid);
        if (bound_status != 0) {
            fork_diagnostic_emit(c, nr, flags, "snapshot-complete", -bound_status, pids_total, &bound_fork);
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
                fork_diagnostic_emit(c, nr, flags, "task-publish", EAGAIN, pids_total, &bound_fork);
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
                (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
                G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
                break;
            }
        }
        errno = fork_error;
        // child: the same shared engine reset as the clone/fork site above (cache re-alias / §B shadow /
        // path caches / kqueues / fork-unsafe locks). clone3 historically lacked the W^X re-assert and the
        // DIR*-cache drop the clone site had; the shared helper closes that drift.
        if (pid == 0) {
            if (hl_linux_pidmap_is_active(&g_pidmap)) {
                g_self_gpid = guest_child_pid;
                // The child's parent is THIS process, not the one recorded for it. Clearing the inherited
                // value sends getppid() back through the live map (identity.c), which is both correct here
                // and correct after the parent is reaped and the host reparents us out of the container.
                g_self_gppid = -1;
            }
            guest_fs_after_fork(share_fs);
            if ((flags & 0x100) && ca[5]) G_SP(c) = ca[5] + ca[6];
            fork_child_hooks(c);
            // clone_args: child_tid = ca[2]. CLONE_CHILD_SETTID stores the child's tid there; CLONE_CHILD_
            // CLEARTID remembers it so exit zeroes it + wakes a joiner (mirrors case 220).
            if ((flags & 0x01000000) && ca[2]) {
                int tid = guest_child_pid;
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
            host_pid_register_child((int)pid); // host registry: parent-side registration (see case 220)
            acct_child_born((int)pid);         // register the child's OWN task slot (container-wide pids.current)
        }
        // clone_args: parent_tid = ca[3]. CLONE_PARENT_SETTID stores the child's tid (pid) into the PARENT's
        // parent_tid (a distinct field from pidfd in clone3, so it never conflicts with CLONE_PIDFD).
        if (pid > 0 && (flags & 0x00100000) && ca[3]) {
            int tid = guest_child_pid;
            if (guest_copy_to(ca[3], &tid, sizeof tid) != sizeof tid) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = pid < 0 ? (uint64_t)(-errno) : (uint64_t)(pid == 0 ? 0 : guest_child_pid);
        break;
    }
    default: return 0;
    }
    return 1;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int clone3_extended_args_test(enum hl_linux_guest_isa isa, uint64_t raw_number, uint32_t scenario) {
    if (hl_linux_syscall_number(isa, raw_number) != 435) return 10;
    uint8_t bytes[96] = {0};
    size_t size = scenario == 1 ? 64 : scenario == 2 ? 80 : scenario <= 6 ? 88 : scenario <= 8 ? 96 : 63;
    uint64_t one = 1;
    if (scenario == 4 || scenario == 11) memcpy(bytes + 64, &one, sizeof one);
    if (scenario == 5 || scenario == 12) memcpy(bytes + 72, &one, sizeof one);
    if (scenario == 6 || scenario == 13) memcpy(bytes + 80, &one, sizeof one);
    if (scenario == 8) bytes[95] = 1;
    if (scenario == 14) bytes[95] = 1;
    if (scenario == 15) memcpy(bytes, &(uint64_t){UINT64_C(0x200000000)}, sizeof(uint64_t));
    uint64_t arguments[11];
    int result = clone3_decode(bytes, size, arguments);
    if (scenario <= 3) return result == 0 ? 0 : 11;
    if (scenario >= 4 && scenario <= 6) return result == -EOPNOTSUPP ? 0 : 12;
    if (scenario == 7) return result == 0 ? 0 : 13;
    if (scenario == 8) return result == -E2BIG ? 0 : 14;
    if (scenario == 9) return result == -EINVAL ? 0 : 15;

    struct cpu fixture = {0};
    uint64_t pointer = scenario == 10 ? 1 : (uint64_t)(uintptr_t)bytes;
    size_t actual_size = scenario == 10 ? 64 : scenario == 14 ? 96 : scenario == 16 ? 4097 : 88;
    g_clone3_test_fork_attempts = 0;
    g_clone3_test_block_fork = scenario >= 11 && scenario <= 15;
    (void)svc_proc_435(&fixture, 435, pointer, actual_size, 0, 0, 0, 0);
    g_clone3_test_block_fork = 0;
    int64_t actual = (int64_t)G_RET(&fixture);
    if (scenario == 10) return actual == -EFAULT ? 0 : 16;
    if (scenario >= 11 && scenario <= 13)
        return actual == -EOPNOTSUPP && g_clone3_test_fork_attempts == 0 ? 0 : 17;
    if (scenario == 14) return actual == -E2BIG && g_clone3_test_fork_attempts == 0 ? 0 : 18;
    if (scenario == 15) return actual == -EOPNOTSUPP && g_clone3_test_fork_attempts == 0 ? 0 : 19;
    if (scenario == 16) return actual == -E2BIG ? 0 : 20;
    return 21;
}

#if !defined(_WIN32)
static pthread_mutex_t g_clone3_extended_args_fault_handler_lock = PTHREAD_MUTEX_INITIALIZER;

static int clone3_extended_args_test_guarded(enum hl_linux_guest_isa isa, uint64_t raw_number, uint32_t scenario,
                                             void (*handler)(int, siginfo_t *, void *)) {
    struct sigaction action = {0}, previous_segv, previous_bus;
    pthread_mutex_lock(&g_clone3_extended_args_fault_handler_lock);
    action.sa_flags = SA_SIGINFO;
    action.sa_sigaction = handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, &previous_segv) != 0) {
        pthread_mutex_unlock(&g_clone3_extended_args_fault_handler_lock);
        return 22;
    }
    if (sigaction(SIGBUS, &action, &previous_bus) != 0) {
        (void)sigaction(SIGSEGV, &previous_segv, NULL);
        pthread_mutex_unlock(&g_clone3_extended_args_fault_handler_lock);
        return 23;
    }
    int result = clone3_extended_args_test(isa, raw_number, scenario);
    int bus_status = sigaction(SIGBUS, &previous_bus, NULL);
    int segv_status = sigaction(SIGSEGV, &previous_segv, NULL);
    pthread_mutex_unlock(&g_clone3_extended_args_fault_handler_lock);
    return bus_status == 0 && segv_status == 0 ? result : 24;
}
#endif
#endif
