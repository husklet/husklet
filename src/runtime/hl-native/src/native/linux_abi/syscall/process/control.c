// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int svc_proc_118(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 118: {
        int pid = (int)a0;
        if (!a1 || pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // do_sched_setscheduler: !param||pid<0
        int prio;
        if (guest_copy_from(&prio, a1, sizeof prio) != sizeof prio) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        int lo, hi;
        if (sched_prio_band(g_sched_policy, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (prio < lo || prio > hi) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        g_sched_prio = prio;
        G_RET(c) = 0;
        break;
    }
    // sched_setscheduler(pid, policy, param)
    default: return 0;
    }
    return 1;
}

static int svc_proc_119(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 119: {
        int pid = (int)a0, policy = (int)a1;
        if (!a2 || pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // !param || pid<0
        int prio;
        if (guest_copy_from(&prio, a2, sizeof prio) != sizeof prio) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        } // copy_from_user(param)
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        } // find_process_by_pid
        int base = policy & ~HL_SCHED_RESET_ON_FORK, lo, hi;
        if (sched_prio_band(base, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // unknown policy
        if (prio < lo || prio > hi) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // priority out of band
        // Real-time classes (SCHED_FIFO=1 / SCHED_RR=2) need CAP_SYS_NICE or a nonzero RLIMIT_RTPRIO.
        // The container runs unprivileged, so the kernel rejects them with EPERM after arg validation --
        // otherwise a latency-sensitive probe believes RT scheduling was installed when nothing changed.
        if (base == 1 || base == 2) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        }
        g_sched_policy = base;
        g_sched_prio = prio;
        G_RET(c) = 0;
        break;
    }
    // sched_getscheduler(pid)
    default: return 0;
    }
    return 1;
}

static int svc_proc_120(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 120: {
        int pid = (int)a0;
        if (pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        G_RET(c) = (uint64_t)g_sched_policy;
        break;
    }
    // sched_getparam(pid, param)
    default: return 0;
    }
    return 1;
}

static int svc_proc_121(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 121: {
        int pid = (int)a0;
        if (!a1 || pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // kernel: !param || pid<0
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        if (guest_copy_to(a1, &g_sched_prio, sizeof g_sched_prio) != sizeof g_sched_prio) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    // sched_get_priority_max(policy)
    default: return 0;
    }
    return 1;
}

static int svc_proc_155(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 155: {
        // Map the guest's view of the init (pid 1) to its real host pid, then query. Linux getpgid fails
        // ONLY with ESRCH (no process with that pid) -- never EPERM/EINVAL. The old handler returned the
        // raw -1 on failure, which svc_done then misread as -EPERM (errno "1"): getpgid02's -99/unused_pid
        // wrongly reported EPERM instead of ESRCH. Force ESRCH for any lookup failure.
        pid_t pid = ((pid_t)a0 == 1 && g_init_hostpid) ? g_init_hostpid : (pid_t)a0;
        pid_t r = getpgid(pid);
        if (r < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (g_init_hostpid && r == g_init_hostpid) r = 1;
        G_RET(c) = (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_156(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 156: {
        // getsid: same contract as getpgid above -- fails only with ESRCH for a pid that names no process
        // (getsid02's unused_pid), so map a raw -1 to ESRCH rather than let svc_done coin it into EPERM.
        pid_t pid = ((pid_t)a0 == 1 && g_init_hostpid) ? g_init_hostpid : (pid_t)a0;
        pid_t r = getsid(pid);
        if (r < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (g_init_hostpid && r == g_init_hostpid) r = 1;
        G_RET(c) = (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_158(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 158: {
        // getgroups(size, list): in rootfs mode report the IMAGE-DERIVED supplementary set runc computes
        // (parsed at container_init -- alpine root -> 0 0 1 2 3 4 6 10 11 20 26 27; ubuntu -> 0), which a
        // guest setgroups(2) may later replace (apt/gosu drop). size==0 queries the count; size<count is
        // -EINVAL; a bad list pointer is -EFAULT. This matches getgroups(2) exactly and stays byte-consistent
        // with the /proc/self/status Groups: line (both read g_groups). Bare mode (unparsed) keeps the prior
        // behavior below: the container egid when a USER-ns gid is set, else the real host set.
        if (g_groups_parsed) {
            int cnt = g_ngroups;
            if ((int)a0 == 0) {
                G_RET(c) = (uint64_t)cnt;
                break;
            } // size 0 -> just the count
            if ((int)a0 < cnt) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (a1) {
                size_t bytes = (size_t)cnt * sizeof(gid_t);
                if (guest_copy_to(a1, g_groups, bytes) != (ssize_t)bytes) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
            }
            G_RET(c) = (uint64_t)cnt;
            break;
        }
        if (g_gid >= 0) {
            // getgroups -> [effective gid]. Tracking the overlay's egid means apt's drop to _apt's group
            // is reflected here too (it setgroups(1,&_apt_gid) right before switching).
            gid_t egid = (gid_t)cred_egid();
            if ((int)a0 >= 1 && a1 && guest_copy_to(a1, &egid, sizeof egid) != sizeof egid) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            G_RET(c) = 1;
            break;
        }
        int count = (int)a0;
        gid_t *groups = count > 0 ? calloc((size_t)count, sizeof *groups) : NULL;
        if (count > 0 && !groups) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        int r = getgroups(count, groups);
        if (r >= 0 && count > 0 && a1 &&
            guest_copy_to(a1, groups, (size_t)r * sizeof *groups) != (ssize_t)((size_t)r * sizeof *groups)) {
            free(groups);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        free(groups);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // setgroups(size, list): a privileged guest replaces its supplementary set (apt setgroups(1,&_apt_gid)
    // before dropping to _apt; gosu clears groups before switching user). In rootfs mode record it so
    // getgroups(2) + /proc/self/status Groups: reflect the guest's current view; size 0 clears the set. Bare
    // mode (unparsed) keeps the historical no-op-succeed. size out of range -> -EINVAL; bad list -> -EFAULT.
    default: return 0;
    }
    return 1;
}

static int svc_proc_159(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 159: {
        if (!g_groups_parsed) {
            G_RET(c) = 0;
            break;
        }
        long ng = (long)a0;
        if (ng < 0 || ng > HL_NGROUPS_MAX) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (ng > 0) {
            if (!a1 ||
                guest_copy_from(g_groups, a1, (size_t)ng * sizeof(gid_t)) != (ssize_t)((size_t)ng * sizeof(gid_t))) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        g_ngroups = (int)ng;
        G_RET(c) = 0;
        break;
    }
    // getrusage(who, *usage) -- a1 is the buffer, not a0!
    default: return 0;
    }
    return 1;
}

static int svc_proc_165(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 165: {
        struct rusage ru;
        // Linux validates `who` FIRST: only RUSAGE_SELF(0), RUSAGE_CHILDREN(-1) and RUSAGE_THREAD(1) are
        // legal; anything else is -EINVAL BEFORE the buffer is touched (LTP getrusage02 passes who=-2). The
        // old handler mapped every non-(-1) value to SELF and always "succeeded".
        int who_g = (int)a0;
        if (who_g != 0 && who_g != -1 && who_g != 1) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // RUSAGE_THREAD(1) -> SELF (macOS has no per-thread rusage; SELF is the closest faithful account).
        int who = (who_g == -1) ? RUSAGE_CHILDREN : RUSAGE_SELF;
        // The 144-byte struct rusage is written directly by the engine (not via a host syscall), so a
        // bad/unmapped pointer must return -EFAULT here rather than fault the engine (access_ok).
        // NULL is NOT exempt: Linux always copy_to_user()s the buffer, so getrusage(RUSAGE_SELF, NULL)
        // is -EFAULT. The old `if (a1)` guard silently skipped the copy-out and returned success.
        if (guest_accessible_prefix(a1, 144, HL_LOGICAL_VMA_WRITE) != 144) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint8_t linux_ru[144] = {0};
        if (getrusage(who, &ru) == 0) rusage_to_linux(linux_ru, &ru);
        if (guest_copy_to(a1, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    // prctl(option,...)
    default: return 0;
    }
    return 1;
}

static int proc_prctl_name(struct cpu *c, uint64_t option, uint64_t arg) {
    if ((int)option == 15) {
        char name[16] = {0};
        for (size_t i = 0; i < sizeof name; ++i) {
            if (guest_copy_from(name + i, arg + i, 1) != 1) {
                G_RET(c) = (uint64_t)(-EFAULT);
                return 1;
            }
            if (!name[i]) break;
        }
        name[15] = 0;
        snprintf(g_procname, sizeof g_procname, "%.15s", name);
        set_guest_comm_name(g_procname, c->tid == 0);
        if (c->tid == 0) proc_reg_publish_comm();
        G_RET(c) = 0;
        return 1;
    }
    if ((int)option != 16) return 0;
    char name[16] = {0};
    if (g_procname[0])
        snprintf(name, sizeof name, "%s", g_procname);
    else
        proc_comm(name, sizeof name);
    G_RET(c) = guest_copy_to(arg, name, sizeof name) == sizeof name ? 0 : (uint64_t)(-EFAULT);
    return 1;
}

static int proc_prctl_ambient(struct cpu *c, uint64_t option, uint64_t subop, uint64_t cap,
                              uint64_t arg4, uint64_t arg5) {
    if ((int)option != 47) return 0;
    if (subop == 4) {
        G_RET(c) = (cap || arg4 || arg5) ? (uint64_t)(-EINVAL) : 0;
        return 1;
    }
    if (arg4 || arg5 || (subop != 1 && subop != 2 && subop != 3) || cap > 40) {
        G_RET(c) = (uint64_t)(-EINVAL);
        return 1;
    }
    G_RET(c) = subop == 2 ? (uint64_t)(-EPERM) : 0;
    return 1;
}

static int proc_prctl_capability(struct cpu *c, uint64_t option, uint64_t arg) {
    switch ((int)option) {
    case 23:
        G_RET(c) = arg > 40 ? (uint64_t)(-EINVAL) : (uint64_t)((g_cap_bnd >> arg) & 1ull);
        return 1;
    case 24:
        if (!(g_cap_eff & (1ull << CAP_SETPCAP)))
            G_RET(c) = (uint64_t)(-EPERM);
        else if (arg > 40)
            G_RET(c) = (uint64_t)(-EINVAL);
        else {
            g_cap_bnd &= ~(1ull << arg);
            G_RET(c) = 0;
        }
        return 1;
    case 27: G_RET(c) = (uint64_t)(unsigned)g_securebits; return 1;
    case 28:
        if (!(g_cap_eff & (1ull << CAP_SETPCAP)))
            G_RET(c) = (uint64_t)(-EPERM);
        else {
            g_securebits = (int)arg;
            G_RET(c) = 0;
        }
        return 1;
    default: return 0;
    }
}

static int proc_prctl_mce(struct cpu *c, uint64_t option, uint64_t operation, uint64_t policy,
                          uint64_t arg4, uint64_t arg5) {
    if ((int)option == 34) {
        G_RET(c) = (operation || policy || arg4 || arg5) ? (uint64_t)(-EINVAL) : (uint64_t)(unsigned)g_mce_kill;
        return 1;
    }
    if ((int)option != 33) return 0;
    if (operation == 0 && !(policy || arg4 || arg5)) {
        g_mce_kill = 2;
        G_RET(c) = 0;
    } else if (operation == 1 && !(arg4 || arg5) && policy <= 2) {
        g_mce_kill = (int)policy;
        G_RET(c) = 0;
    } else
        G_RET(c) = (uint64_t)(-EINVAL);
    return 1;
}

static int svc_proc_167(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 167: {
        if (proc_prctl_name(c, a0, a1)) break;
        // PR_SET_TIMERSLACK(29)/PR_GET_TIMERSLACK(30): the per-process timer slack (ns) round-trips. SET with
        // arg2==0 resets to the default (Linux copies the process's default_timer_slack_ns); GET returns the
        // current value as the syscall return. Previously both fell through to the generic no-op/EINVAL
        // switch, so GET never reported what SET stored.
        if ((int)a0 == 29) {
            g_timerslack = a1 ? (unsigned long)a1 : 50000UL;
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 30) {
            G_RET(c) = (uint64_t)g_timerslack;
            break;
        }
        // PR_SET_KEEPCAPS(8)/PR_GET_KEEPCAPS(7) drive the CAP_SETID retention model -- setpriv arms
        // KEEPCAPS so its post-uid-drop capset can re-raise CAP_SETGID (see cred_uid_changed/capset).
        if ((int)a0 == 8) {
            g_keepcaps = (a1 != 0);
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 7) {
            G_RET(c) = (uint64_t)g_keepcaps;
            break;
        }
        // PR_SET_PDEATHSIG(1)/PR_GET_PDEATHSIG(2): the parent-death signal round-trips (no real delivery
        // under the JIT, but the value the guest set is reported back). arg2 must be 0 (clear) or a valid
        // signal number 1..64; anything else is -EINVAL (LTP prctl02 PR_SET_PDEATHSIG/ULONG_MAX).
        if ((int)a0 == 1) {
            if (a1 > 64) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_pdeathsig = (int)a1;
            // Forward to the host prctl on Linux: each guest process is a real host process whose parent is
            // the guest parent's host process, so the host kernel delivers the parent-death signal when that
            // parent dies -- exactly like the PR_SET_CHILD_SUBREAPER forward below. Without this the guest's
            // pdeathsig never fired and a child blocked in sigwait() hung forever.
#if defined(__linux__) && defined(PR_SET_PDEATHSIG)
            (void)prctl(PR_SET_PDEATHSIG, (unsigned long)(int)a1, 0UL, 0UL, 0UL);
#endif
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 2) {
            if (guest_copy_to(a1, &g_pdeathsig, sizeof g_pdeathsig) != sizeof g_pdeathsig) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = 0;
            break;
        }
        // PR_GET_DUMPABLE(3)/PR_SET_DUMPABLE(4): the dumpable flag round-trips. SET accepts ONLY
        // SUID_DUMP_DISABLE(0) and SUID_DUMP_USER(1); any other value (incl. 2 = the internal
        // SUID_DUMP_ROOT, which is not settable from userspace) is -EINVAL (LTP prctl02 PR_SET_DUMPABLE/2).
        if ((int)a0 == 3) {
            G_RET(c) = (uint64_t)(unsigned)g_dumpable;
            break;
        }
        if ((int)a0 == 4) {
            if (a1 > 1) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_dumpable = (int)a1;
            G_RET(c) = 0;
            break;
        }
        // PR_SET_NO_NEW_PRIVS(38)/PR_GET_NO_NEW_PRIVS(39): sticky once set; SET requires arg2==1.
        if ((int)a0 == 38) {
            if (a1 != 1 || a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_nnp = 1;
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 39) {
            if (a1 || a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)g_nnp;
            break;
        }
        // PR_SET_CHILD_SUBREAPER(36)/PR_GET_CHILD_SUBREAPER(37): the subreaper flag round-trips. SET stores
        // arg2 as a boolean; GET writes it through the int* in arg2 (LTP prctl03). NOTE: only the flag itself
        // round-trips here -- the ACTUAL reparenting of an orphaned descendant onto a subreaper is a
        // process-tree feature hl's 1:1 host-fork model does not implement (an orphaned guest grandchild is
        // reparented by the host kernel, not routed back to the guest subreaper), so prctl03's reparent/
        // SIGCHLD/wait subtests are a known process-model gap, out of this syscall layer's scope.
        if ((int)a0 == 36) {
            g_subreaper = (a1 != 0);
            // hl runs each guest task as a real host process, so orphan reparenting is a host-kernel
            // decision. Forward the flag to the host prctl on Linux: the host kernel then reparents an
            // orphaned guest grandchild onto THIS engine process (the marked subreaper) instead of host
            // init, so the guest's own waitpid(-1) harvests it -- matching PR_SET_CHILD_SUBREAPER. The
            // guest-visible flag still round-trips via g_subreaper for PR_GET below.
#if defined(__linux__) && defined(PR_SET_CHILD_SUBREAPER)
            (void)prctl(PR_SET_CHILD_SUBREAPER, a1 != 0 ? 1UL : 0UL, 0UL, 0UL, 0UL);
#endif
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 37) {
            if (guest_copy_to(a1, &g_subreaper, sizeof g_subreaper) != sizeof g_subreaper) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = 0;
            break;
        }
        // PR_GET_THP_DISABLE(42)/PR_SET_THP_DISABLE(41): the per-process transparent-hugepage opt-out flag
        // round-trips. GET rejects any nonzero unused arg; SET treats arg2 as a boolean and rejects nonzero
        // arg3/arg4/arg5 (LTP prctl02 PR_{GET,SET}_THP_DISABLE). Modeling it (rather than the old blanket
        // EINVAL) makes the feature probe succeed so its dependent LTP subtests run, matching real Linux.
        if ((int)a0 == 42) {
            if (a1 || a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)g_thp_disable;
            break;
        }
        if ((int)a0 == 41) {
            if (a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_thp_disable = (a1 != 0);
            G_RET(c) = 0;
            break;
        }
        // PR_CAP_AMBIENT(47): the ambient capability set is empty in this all-root container, so RAISE/LOWER
        // are accepted no-ops and IS_SET always reports "not set"; the value of this handler is matching
        // Linux's argument validation exactly (LTP prctl02 PR_CAP_AMBIENT/*). Sub-command in arg2:
        //   4=CLEAR_ALL (arg3/4/5 must be 0), 2=RAISE, 3=LOWER, 1=IS_SET (arg3=cap, must be <= CAP_LAST_CAP;
        //   arg4/5 must be 0). Any other sub-command is -EINVAL.
        if (proc_prctl_ambient(c, a0, a1, a2, a3, a4)) break;
        // PR_GET_SPECULATION_CTRL(52): report a plausible speculation-control status. arg3/arg4/arg5 must be
        // 0 (LTP prctl02 PR_GET_SPECULATION_CTRL/arg-nonzero -> EINVAL); the feature must NOT report EINVAL
        // for the all-zero probe, or its dependent subtests would be skipped where real Linux runs them.
        if ((int)a0 == 52) {
            if (a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = 2; // PR_SPEC_PRCTL is off, mitigation not forced: PR_SPEC_ENABLE
            break;
        }
        // PR_CAPBSET_READ(23): "is capability arg2 in this task's BOUNDING set?" capsh --print / getpcaps
        // probe every cap this way to render the mask; it MUST agree with /proc/self/status CapBnd. Returns 1
        // if present, 0 if absent, -EINVAL for a cap index past CAP_LAST_CAP (40). The docker default holds
        // exactly the 14 bits of HL_CAP_DEFAULT (g_cap_bnd), so e.g. CAP_SYS_ADMIN(21) reads 0.
        if (proc_prctl_capability(c, a0, a1)) break;
        // PR_GET_SECCOMP(21): the docker default seccomp profile is always applied, so real docker reports
        // filter mode (2) here AND as Seccomp:2 in /proc/self/status. Match it (unfiltered Linux returns 0);
        // software that gates behaviour on being sandboxed reads this. arg2..5 are ignored by the kernel.
        if ((int)a0 == 21) {
            const char *baseline = hl_option_get("HL_SECCOMP_BASELINE");
            G_RET(c) = t_seccomp_mode != 0 ? t_seccomp_mode
                                           : (baseline != NULL && strcmp(baseline, "disabled") == 0 ? 0 : 2);
            break;
        }
        // PR_SET_SECCOMP(22): the legacy entry point for seccomp, ENFORCED like the seccomp(2) syscall
        // (rare.c case 277) via os/linux/seccomp.c. arg2 is the SECCOMP_MODE_* (STRICT=1, FILTER=2 -- note
        // these differ from seccomp(2)'s op numbers); FILTER takes the struct sock_fprog* in arg3 (a2).
        if ((int)a0 == 22) {
            if (a1 == 1 /*SECCOMP_MODE_STRICT*/)
                G_RET(c) = (uint64_t)(int64_t)seccomp_set_strict();
            else if (a1 == 2 /*SECCOMP_MODE_FILTER*/)
                G_RET(c) = (uint64_t)(int64_t)seccomp_install_filter(a2, 0);
            else
                G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // PR_SET_PTRACER (0x59616d61, "Yama"): a process may allow a specific helper pid to ptrace it
        // when Linux's Yama LSM is present. hl has no Yama policy to enforce, so accept the request as a no-op.
        if ((int)a0 == 1499557217) {
            G_RET(c) = 0;
            break;
        }
        // PR_SET_SECUREBITS(28) and PR_CAPBSET_DROP(24) require CAP_SETPCAP in the effective set; without it
        // the kernel returns -EPERM before any further validation (LTP prctl02 drops CAP_SETPCAP first). With
        // the cap held (the container default) they succeed for a well-formed argument.
        // PR_GET_SECUREBITS(27): report the current securebits flags (0 in a default container). The kernel
        // ignores arg2..5 here, so no argument validation -- capsh/libcap read this and it must agree with what
        // PR_SET_SECUREBITS stored. Previously fell through to the generic switch -> -EINVAL (a query that
        // always succeeds on real Linux).
        // PR_TASK_PERF_EVENTS_DISABLE(31)/PR_TASK_PERF_EVENTS_ENABLE(32): toggle this task's perf events.
        // Always succeeds on real Linux (the remaining args are ignored, no capability required); with no perf
        // subsystem to gate there is nothing to enforce, so mirror the kernel's unconditional success rather
        // than the old blanket -EINVAL, which made a benign query look like an unsupported operation.
        if ((int)a0 == 31 || (int)a0 == 32) {
            G_RET(c) = 0;
            break;
        }
        // PR_MCE_KILL(33)/PR_MCE_KILL_GET(34): the per-process machine-check early/late kill policy. GET (34)
        // returns the stored policy (LATE=0 / EARLY=1 / DEFAULT=2) and rejects any nonzero arg2..5. SET (33)
        // takes a sub-op in arg2: PR_MCE_KILL_CLEAR(0) resets to DEFAULT (arg3..5 must be 0);
        // PR_MCE_KILL_SET(1) takes a policy in arg3 (EARLY=1 / LATE=0 / DEFAULT=2, arg4/5 must be 0). Any other
        // shape is -EINVAL (LTP prctl02). The policy is advisory (there is no guest-visible MCE delivery here)
        // but round-tripping it makes the feature probe succeed exactly as on Linux instead of -EINVAL.
        if (proc_prctl_mce(c, a0, a1, a2, a3, a4)) break;
        // PR_SET_MM(35): rewrite fields of this task's mm layout (start_brk, brk, arg/env bounds, ...). The
        // kernel gates the WHOLE option on CAP_SYS_RESOURCE and returns -EPERM before validating the sub-op
        // when it is absent. The container's default cap set (HL_CAP_DEFAULT) does NOT include
        // CAP_SYS_RESOURCE, so every PR_SET_MM here is -EPERM -- matching native (both an unprivileged task
        // and a container root without the cap). Previously this fell into the no-op list and returned 0,
        // silently claiming to relayout the mm while doing nothing.
        if ((int)a0 == 35) {
            G_RET(c) = (g_cap_eff & (1ull << CAP_SYS_RESOURCE)) ? (uint64_t)(-EINVAL) : (uint64_t)(-EPERM);
            break;
        }
        // 0 for known no-ops; EINVAL for unknown (kernel does)
        switch ((int)a0) {
        case 15:
        case 53:
        case 55:
        // NAME/SECCOMP/TIMERSLACK/THP/SPECCTRL...
        case 59: G_RET(c) = 0; break;
        // EINVAL -- so feature probes (e.g. magic "AUXV") fail as on Linux
        default: G_RET(c) = (uint64_t)(-22); break;
        }
        break;
    }
    // Standalone C fallback. Product execution selects the Rust-owned process identity before service().
    // getpid (PID ns: init -> 1)
    default: return 0;
    }
    return 1;
}
