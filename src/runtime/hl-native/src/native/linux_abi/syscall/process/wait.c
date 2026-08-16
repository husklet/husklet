// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
#if defined(__APPLE__)
#include <sys/event.h>

static pid_t ckpt_interruptible_wait4(pid_t pid, int *status, int options, struct rusage *usage) {
    if (g_ckpt_trigger == NULL || (options & WNOHANG) != 0) return wait4(pid, status, options, usage);
    int queue = kqueue();
    if (queue < 0) return wait4(pid, status, options, usage);
    struct kevent changes[2];
    EV_SET(&changes[0], SIGCHLD, EVFILT_SIGNAL, EV_ADD, 0, 0, NULL);
    EV_SET(&changes[1], THREAD_INT_SIG, EVFILT_SIGNAL, EV_ADD, 0, 0, NULL);
    if (kevent(queue, changes, 2, NULL, 0, NULL) < 0) {
        int saved = errno;
        close(queue);
        errno = saved;
        return wait4(pid, status, options, usage);
    }
    pid_t result;
    for (;;) {
        result = wait4(pid, status, options | WNOHANG, usage);
        if (result != 0) break;
        if (ckpt_pending()) {
            errno = EINTR;
            result = -1;
            break;
        }
        struct kevent event;
        if (kevent(queue, NULL, 0, &event, 1, NULL) < 0) {
            result = -1;
            break;
        }
    }
    int saved = errno;
    close(queue);
    errno = saved;
    return result;
}
#else
static pid_t ckpt_interruptible_wait4(pid_t pid, int *status, int options, struct rusage *usage) {
    return wait4(pid, status, options, usage);
}
#endif

static int svc_proc_260(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 260: {
        int st = 0;
        int status_efault = 0;
        pid_t r;
        struct rusage ruloc;
        memset(&ruloc, 0, sizeof ruloc);
        // Linux validates the option bits BEFORE any child lookup: anything outside
        // WNOHANG|WUNTRACED|WCONTINUED|__WNOTHREAD|__WALL|__WCLONE is -EINVAL (waitpid04 case 3 passes
        // options 0xffffffff and expects EINVAL, not the ECHILD a permissive host wait4 returns). macOS
        // ignores unknown bits, so gate here rather than trust the host.
        if ((int)a2 & ~(int)0xE000000B) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // waitpid(INT_MIN, ...) is the one negative pid Linux answers with ESRCH rather than ECHILD:
        // pid < -1 means "any child in process group -pid", and -INT_MIN overflows, so the kernel
        // special-cases it to -ESRCH (waitpid04 case 4). Any other invalid pgroup is a normal ECHILD.
        if ((int)a0 == INT_MIN) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (a3 && guest_accessible_prefix(a3, 144, HL_LOGICAL_VMA_WRITE) != 144) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // checkpoint restore: a wait targeting a specific checkpoint-time guest pid must name the live host
        // pid the tree was re-forked with (identity no-op on a normal launch when the pid map is empty).
        if (hl_linux_pidmap_count(&g_pidmap) != 0 && (int)a0 > 0)
            a0 = (uint64_t)(unsigned)hl_linux_pidmap_host(&g_pidmap, (int)a0);
        else if (hl_linux_pidmap_count(&g_pidmap) != 0 && (int)a0 < -1)
            a0 = (uint64_t)(int64_t)(-hl_linux_pidmap_host(&g_pidmap, -(int)a0));
        // when ptrace is already in use in this session (a tracee link exists -> nactive>0) route the
        // wait through the ptrace pump, which surfaces tracee ptrace-stops (Linux-encoded) AND real child
        // exits and tears a link down when its tracee dies. For the ENTIRE non-ptrace matrix nactive is 0,
        // so this predicate is false. Returns 1 when it produced a result (r/st Linux-encoded).
        if (ptrace_wait_active()) {
            pid_t pr;
            int handled = ptrace_wait(c, (pid_t)(int)a0, (int)a2, a3 ? &ruloc : NULL, &st, &pr);
            if (handled) {
                if (pr < 0) {
                    G_RET(c) = (uint64_t)(int64_t)pr;
                    break;
                } // -errno / -EINTR
                if (a1 && guest_copy_to(a1, &st, sizeof st) != sizeof st) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                if (a3) {
                    uint8_t linux_ru[144];
                    rusage_to_linux(linux_ru, &ruloc);
                    if (guest_copy_to(a3, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        break;
                    }
                }
                G_RET(c) = (uint64_t)pr;
                break;
            }
        }
        // tracer-wait race guard (see ptrace.c pt_wait_arm): a child may PTRACE_TRACEME + stop AFTER
        // this parent already blocked here (the classic strace ordering: parent waitpid()s before the child
        // traces itself, so nactive was 0 at entry and we take this plain path). To let the tracee's stop
        // SIGCHLD interrupt us so we can reroute, arm a benign SIGCHLD handler ONLY around the BLOCKING
        // wait4 (a2 without WNOHANG), and ONLY if the guest has no SIGCHLD handler of its own; it is
        // restored the instant the wait returns. This touches NOTHING outside this one blocking wait4 --
        // no other syscall is affected, the guest's waitpid never returns a spurious EINTR (the do/while
        // retries), and a guest that never calls wait4 is never armed. pt_wait_arm returns 0 (no-op) for a
        // WNOHANG wait, a guest with its own SIGCHLD handler, or if the ptrace arena is absent.
        // Translate Linux wait4 options to the host's (they DIVERGE): Linux WCONTINUED is 0x8, but that value
        // is macOS WSTOPPED -- passing the raw bits made a WCONTINUED wait miss continued children and mis-
        // encode the following status. Only WNOHANG/WUNTRACED share a value; the __W* thread-selection bits
        // have no host form and are dropped. rusage goes into a LOCAL host struct and is converted to the
        // guest's Linux layout after the reap (a raw host rusage buffer would leave Darwin byte-scale values
        // in the Linux ru_maxrss/... fields).
        int mopt = 0;
        if ((int)a2 & 1) mopt |= WNOHANG;
        if ((int)a2 & 2) mopt |= WUNTRACED;
        if ((int)a2 & 8) mopt |= WCONTINUED;
        struct sigaction pt_saved;
        int pt_armed = ((int)a2 & 1 /*WNOHANG*/) ? 0 : pt_wait_arm(&pt_saved);
        // SA_RESTART: a wait interrupted by a handler that asked to restart (e.g. a SIGCHLD reaper, or
        // gcc's driver) must transparently retry instead of failing the guest with EINTR.
        ts_wait_enter(); // 'S' while blocked waiting on a child (WNOHANG returns immediately, harmless)
        do {
            r = ckpt_interruptible_wait4((pid_t)(int)a0, &st, mopt, a3 ? &ruloc : NULL);
            // Reroute to the ptrace pump if the interrupt was a tracee of ours stopping (we became a tracer
            // while blocked). Gated on nactive>0 -> the non-ptrace matrix never enters this branch.
            if (r < 0 && errno == EINTR && ptrace_wait_active() && ptrace_any_tracee_of_self()) {
                pid_t pr;
                if (ptrace_wait(c, (pid_t)(int)a0, (int)a2, a3 ? &ruloc : NULL, &st, &pr)) {
                    pt_wait_disarm(pt_armed, &pt_saved);
                    ts_wait_leave();
                    if (pr < 0) {
                        G_RET(c) = (uint64_t)(int64_t)pr;
                        goto wait_done;
                    }
                    if (a1 && guest_copy_to(a1, &st, sizeof st) != sizeof st) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        goto wait_done;
                    }
                    if (a3) {
                        uint8_t linux_ru[144];
                        rusage_to_linux(linux_ru, &ruloc);
                        if (guest_copy_to(a3, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
                            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                            goto wait_done;
                        }
                    }
                    G_RET(c) = (uint64_t)pr;
                    goto wait_done;
                }
            }
        } while (r < 0 && SVC_EINTR_RESTART(c));
        pt_wait_disarm(pt_armed, &pt_saved);
        ts_wait_leave();
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // WIFSIGNALED: macOS termsig -> Linux, and encode WCOREDUMP (0x80) exactly as Linux does. The host
        // child dies from a real host signal (signal.c default action), but macOS almost never writes a core
        // for it (cores off by default) and the guest's setrlimit(RLIMIT_CORE) is not applied to the host, so
        // the host status usually lacks 0x80. Synthesize the bit from (core-dumping signal AND the guest's
        // RLIMIT_CORE soft limit > 0) -- the Linux rule -- while still honoring the host's own core flag if it
        // did dump. Non-core signals (SIGKILL/SIGTERM/...) or rlim_cur==0 => no bit (WCOREDUMP false).
        int rawsig = st & 0x7f;
        // WIFCONTINUED: macOS encodes a continued child as a "stopped" status whose stop-signal is SIGCONT
        // (low byte 0x7f, high byte 19); Linux uses the sentinel status 0xffff. Check this BEFORE the stopped
        // branch below, which would otherwise mistranslate it as a stop.
        if ((st & 0xff) == 0x7f && ((st >> 8) & 0xff) == SIGCONT) {
            int ignored_stop;
            (void)sigstop_lookup(r, &ignored_stop, 1);
            st = 0xffff;
        } else if (rawsig != 0 && rawsig != 0x7f) {
#if defined(__APPLE__)
            // A raw macOS SIGBUS from a translated child is the host's bad-address/protection fault;
            // Linux reports it as SIGSEGV. Intentional guest SIGBUS deaths use the shared sigexit relay.
            int lsig = rawsig == SIGBUS ? 11 : sig_m2l(rawsig) & 0x7f;
#else
            int lsig = sig_m2l(rawsig) & 0x7f;
#endif
            int core = sig_coredumps(lsig) && (((st & 0x80) != 0) || svc_core_rlimit_cur() > 0);
            st = (st & ~0xff) | lsig | (core ? 0x80 : 0);
        }
        // WIFSTOPPED: macOS stopsig -> Linux
        else if ((st & 0xff) == 0x7f) {
            int guest_stop;
            int stop = sigstop_lookup(r, &guest_stop, 0) ? guest_stop : sig_m2l((st >> 8) & 0xff);
            st = (st & ~0xff00) | ((stop & 0xff) << 8);
        }
        // WIFEXITED from the host, but the child may have relayed a guest signal death: a fatal-default
        // signal with no faithful fatal host mapping is delivered by the child _exit()ing after recording its
        // Linux signo in the shared table. Reconstruct the SIGNALED status here. A genuine guest _exit(n)
        // recorded nothing, so it is left as WIFEXITED(n).
        else if ((st & 0x7f) == 0) {
            int gsig, gcore;
            if (sigexit_lookup(r, &gsig, &gcore, 1)) st = (gsig & 0x7f) | (gcore ? 0x80 : 0);
        }
        // Fill the guest's Linux-layout rusage from the reaped child's host accounting (kilobyte-scaled).
        if (a3 && r > 0) {
            uint8_t linux_ru[144];
            rusage_to_linux(linux_ru, &ruloc);
            if (guest_copy_to(a3, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
                status_efault = 1;
                goto wait_reap_bookkeeping;
            }
        }
        // status copy_to_user: a non-NULL but unwritable status pointer is -EFAULT, exactly as the kernel's
        // put_user(status, stat_addr) after the child is already reaped (native wait4 releases the zombie THEN
        // faults, leaving nothing to re-reap -- verified on aarch64). Guard the direct guest write so a bad
        // pointer returns EFAULT instead of faulting the engine; the reap-side bookkeeping below still runs.
        status_efault = a1 && guest_copy_to(a1, &st, sizeof st) != sizeof st;
    wait_reap_bookkeeping:
        // guest-pid namespace: a reaped child that TERMINATED (exited or signalled -- not merely stopped
        // 0x7f / continued 0xffff) leaves the pid table; drop its container-registry record here so a
        // signal-killed child (which never ran its own exit cleanup) can't leave a stale membership marker
        // that a recycled host pid could inherit. Use the host pid `r` before the restore remap below.
        if (r > 0 && (st & 0xff) != 0x7f && st != 0xffff) proc_reg_reap((int)r);
        // checkpoint restore: report the reaped child under the guest pid the checkpoint recorded, and drop
        // its translation once it is reaped so a future host pid can never alias it (no-op on normal launch).
        if (hl_linux_pidmap_count(&g_pidmap) != 0 && r > 0) {
            int gp = hl_linux_pidmap_guest(&g_pidmap, (int)r);
            if (gp != (int)r) {
                if (((st & 0x7f) == 0) || (((st & 0x7f) != 0x7f) && ((st & 0x7f) != 0)))
                    (void)hl_linux_pidmap_remove_host(&g_pidmap, (int)r);
                r = (pid_t)gp;
            }
        }
        int runtime_reap_fault =
            r > 0 && (st & 0xff) != 0x7f && st != 0xffff &&
            !hl_target_task_event(c, HL_TASK_EVENT_REAP_PROCESS, (uint64_t)r,
                                  (uint64_t)(c->tid ? c->tid : container_pid()), (uint64_t)(uint32_t)st);
        G_RET(c) = status_efault        ? (uint64_t)(int64_t)(-EFAULT)
                   : runtime_reap_fault ? (uint64_t)(int64_t)(-EIO)
                                        : (uint64_t)r;
    wait_done:; // the EINTR reroute jumps here (G_RET + *status already set)
        break;
    }
    default: return 0;
    }
    return 1;
}
static int svc_proc_261(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 261: {
        // prlimit64(pid, resource, NEW, OLD): report the CURRENT limit into OLD first (so a combined
        // get+set returns the pre-change value), THEN apply NEW into the per-resource store so a later
        // get reflects it. glibc's getrlimit/setrlimit/prlimit all funnel through this syscall, so the
        // store (g_limits, also seeded by docker --ulimit) is the single source of truth. without
        // applying NEW, setrlimit "succeeded" but the value never took -- the next getrlimit saw the old.
        int res = (int)a1;
        // Linux validates BEFORE touching the limits: the task lookup runs first (a negative or dead target
        // pid -> ESRCH), then the resource number is range-checked (>= RLIM_NLIMITS(16) -> EINVAL). Without
        // these hl reports success for dead pids and unsupported resources, so probes see them as valid.
        if ((int)a0 < 0 || sched_pid_live((int)a0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (res < 0 || res >= HL_LIMIT_COUNT) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint64_t new_limit[2], old_limit[2];
        if ((a2 && guest_copy_from(new_limit, a2, sizeof(new_limit)) != sizeof(new_limit)) ||
            (a3 && guest_accessible_prefix(a3, sizeof(old_limit), PROT_WRITE) != sizeof(old_limit))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a3) {
            svc_fill_rlimit(res, old_limit);
            if (guest_copy_to(a3, old_limit, sizeof(old_limit)) != sizeof(old_limit)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (a2) {
            const uint64_t *nl = new_limit;
            uint64_t ncur = nl[0], nmax = nl[1];
            // Linux: soft may not exceed hard -> EINVAL (RLIM_INFINITY == ~0 is the max, so it never trips).
            if (ncur != ~0ull && nmax != ~0ull && ncur > nmax) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (res == 7) {
                uint32_t guest_limit = hl_engine_guest_fd_limit();
                uint64_t ceiling = guest_limit > 0 ? guest_limit : 20480;
                if (nmax == ~0ull || nmax > ceiling || ncur == ~0ull || ncur > ceiling) {
                    G_RET(c) = (uint64_t)(int64_t)(-EPERM);
                    break;
                }
            }
            hl_limit_table_set(&g_limits, res, ncur, nmax);
        }
        G_RET(c) = 0;
        break;
    }
    // clone3(clone_args*, size)
    default: return 0;
    }
    return 1;
}
