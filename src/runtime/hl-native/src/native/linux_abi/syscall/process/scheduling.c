// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int svc_proc_122(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 122: {
        size_t n = (size_t)a1;
        size_t copy = n < 128 ? n : 128;
        uint8_t mask[128] = {0};
        // Linux get_user_cpu_mask() copy_from_user()s min(len, cpumask_size) bytes FIRST -> a bad mask pointer
        // (with len>0) is -EFAULT before anything else. The old handler read the guest mask straight through in
        // hl_linux_affinity_set(), so an unmapped pointer SEGV'd the engine instead of returning EFAULT. len==0
        // copies nothing (no fault) and falls through to the empty-mask -EINVAL below.
        if (copy && guest_copy_from(mask, a2, copy) != (ssize_t)copy) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Then, like sched_getaffinity, the target task must exist: find_task_by_vpid() -> -ESRCH for a pid that
        // names no live task. The old handler skipped this and "succeeded" (returned 0) for any pid.
        if (sched_pid_live((int)(int32_t)a0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (!hl_linux_affinity_set(&g_affinity, mask, copy, linux_online_cpus())) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_123(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 123: {
        size_t n = (size_t)a1;
        // sched_getaffinity(pid,size,MASK=a2!) -- return the current mask (all online CPUs by default),
        // not just CPU 0, so CPU_COUNT() and tcmalloc's enumeration see the real width (mongod aborts).
        // Linux validates the cpusetsize FIRST: it must be a multiple of sizeof(long) AND wide enough to
        // hold every online CPU, else -EINVAL (LTP sched_getaffinity01). The old handler skipped this and
        // always "succeeded", so a deliberately-tiny cpusetsize wrongly returned 0.
        if ((n & (sizeof(unsigned long) - 1)) || n * 8 < (size_t)linux_online_cpus()) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // The target task must exist. Linux looks the pid up AFTER the size check and BEFORE the copy-out,
        // returning -ESRCH for a pid that names no live task (LTP sched_getaffinity01 uses an unused pid).
        // pid 0 == the caller; a live guest thread tid (glibc's pthread_getaffinity_np -> pd->tid, on the
        // JVM/Go bootstrap path) resolves via the registry, not a host kill() of an unrelated host pid.
        if (sched_pid_live((int)(int32_t)a0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        // The mask itself must be writable -> EFAULT on a bad pointer (matches Linux copy_to_user).
        // NULL is not exempt: the size check above already guarantees n >= sizeof(long), so Linux always
        // copies out and sched_getaffinity(0, sizeof(mask), NULL) is -EFAULT. The old `a2 &&` guard made a
        // NULL mask "succeed" (returning the byte count) while writing nothing.
        size_t copy = n < 128 ? n : 128;
        if (copy && guest_accessible_prefix(a2, copy, HL_LOGICAL_VMA_WRITE) != copy) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (copy && guest_copy_to(a2, hl_linux_affinity_get(&g_affinity, linux_online_cpus()), copy) != (ssize_t)copy) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Return the number of bytes the mask spans (glibc zeroes the remainder); 8 covers <=64 CPUs.
        G_RET(c) = copy < 8 ? (uint64_t)copy : 8;
        break;
    }
    // sched_yield
    default: return 0;
    }
    return 1;
}

static int svc_proc_124(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 124: G_RET(c) = 0; break;
    // ---- sched_setscheduler / sched_*param arg-validation family (LTP sched_*01..03). hl has no real
    // Linux scheduling classes, so these validate exactly like the kernel and record the requested
    // policy/priority for round-trip reads; the errno ORDER matches the kernel line-for-line.
    // sched_setparam(pid, param)
    default: return 0;
    }
    return 1;
}

static int svc_proc_125(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 125: {
        int lo, hi;
        if (sched_prio_band((int)a0, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        G_RET(c) = (uint64_t)hi;
        break;
    }
    // sched_get_priority_min(policy)
    default: return 0;
    }
    return 1;
}

static int svc_proc_126(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 126: {
        int lo, hi;
        if (sched_prio_band((int)a0, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        G_RET(c) = (uint64_t)lo;
        break;
    }
    // sched_rr_get_interval(pid, tp): report a nominal RR quantum (100ms). Validation order matches the
    // kernel: pid<0 -> EINVAL, missing task -> ESRCH, bad tp -> EFAULT.
    default: return 0;
    }
    return 1;
}

static int svc_proc_127(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 127: {
        int pid = (int)a0;
        if (pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        const uint64_t interval[2] = {0, 100000000};
        if (guest_copy_to(a1, interval, sizeof interval) != sizeof interval) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_140(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 140: {
        // setpriority(which, who, prio). Linux CLAMPS the resulting nice to [-20, 19]; macOS PRIO_MAX is
        // 20, so an unclamped host setpriority(...,>=20) leaves nice==20 and a following getpriority reads
        // 20 -- the nice02 off-by-one ("Process priority 20, expected 19"). Clamp to the Linux range first.
        // `which` is validated as on Linux (EINVAL); the priority set itself stays best-effort success (the
        // container is root, so a host EACCES/EPERM for lowering nice must not surface to a root guest).
        int which = (int)a0;
        if (which != PRIO_PROCESS && which != PRIO_PGRP && which != PRIO_USER) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int prio = (int)a2;
        if (prio > 19)
            prio = 19;
        else if (prio < -20)
            prio = -20;
        setpriority(which, (int)a1, prio);
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_141(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 141: {
        // getpriority(which, who) -> Linux raw kernel encoding (20 - nice). Linux validates `which` first
        // (EINVAL for anything but PRIO_PROCESS/PGRP/USER), then fails ESRCH when no process matches
        // (which,who) -- e.g. getpriority02's who==-1. macOS can report the wrong errno family here, so
        // enforce the Linux contract directly: bad which -> EINVAL, any other failure -> ESRCH.
        int which = (int)a0;
        if (which != PRIO_PROCESS && which != PRIO_PGRP && which != PRIO_USER) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        errno = 0;
        int r = getpriority(which, (int)a1);
        if (r == -1 && errno) { // a real -1 nice value keeps errno==0; only a genuine failure sets it
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        G_RET(c) = (uint64_t)(20 - r);
        break;
    }
    // setuid(uid): a privileged task sets real+eff+saved; an unprivileged one may only set euid to an id
    // it already holds. Honoured against the credential overlay so apt's _apt drop (and its "can't regain
    // root" check) behave as on Linux. (See cred_init above.)
    default: return 0;
    }
    return 1;
}

static int svc_proc_146(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 146: {
        cred_init();
        int u = (int)a0;
        if (!uid_permitted(u)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (g_cap_setid_eff) g_ruid = g_suid = u;
        g_euid = u;
        g_fsuid_ovr = -1;   // fsuid follows the new euid (POSIX) -> new files stamped with it
        cred_uid_changed(); // recompute CAP_SETID after the uid transition (drop vs keepcaps)
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = 0;
        break;
    }
    // setgid(gid): symmetric to setuid above.
    default: return 0;
    }
    return 1;
}

static int svc_proc_144(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 144: {
        cred_init();
        int gg = (int)a0;
        if (!gid_permitted(gg)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (g_cap_setid_eff) g_rgid = g_sgid = gg;
        g_egid = gg;
        g_fsgid_ovr = -1; // fsgid follows the new egid
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = 0;
        break;
    }
    // setresuid(ruid,euid,suid): each (uid_t)-1 leaves that id unchanged; every requested id must be
    // permitted (privileged, or already held). glibc's seteuid() arrives here as setresuid(-1,euid,-1).
    default: return 0;
    }
    return 1;
}

static int svc_proc_147(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 147: {
        cred_init();
        int r = (int)a0, e = (int)a1, s = (int)a2;
        if (!uid_permitted(r) || !uid_permitted(e) || !uid_permitted(s)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_ruid = r;
        if (e != -1) g_euid = e;
        if (s != -1) g_suid = s;
        g_fsuid_ovr = -1;   // fsuid follows euid
        cred_uid_changed(); // recompute CAP_SETID after the uid transition (drop vs keepcaps)
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = 0;
        break;
    }
    // setresgid(rgid,egid,sgid): symmetric. glibc's setegid() arrives here as setresgid(-1,egid,-1).
    default: return 0;
    }
    return 1;
}

static int svc_proc_149(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 149: {
        cred_init();
        int r = (int)a0, e = (int)a1, s = (int)a2;
        if (!gid_permitted(r) || !gid_permitted(e) || !gid_permitted(s)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_rgid = r;
        if (e != -1) g_egid = e;
        if (s != -1) g_sgid = s;
        g_fsgid_ovr = -1; // fsgid follows egid
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = 0;
        break;
    }
    // setreuid(ruid,euid): -1 leaves an id unchanged. The kernel moves saved-uid to the new euid whenever
    // the real uid is changed, or the euid is set to a value other than the previous real uid.
    default: return 0;
    }
    return 1;
}

static int svc_proc_145(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 145: {
        cred_init();
        int r = (int)a0, e = (int)a1, old_ruid = g_ruid;
        if (!uid_permitted(r) || !uid_permitted(e)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_ruid = r;
        if (e != -1) g_euid = e;
        if (r != -1 || (e != -1 && e != old_ruid)) g_suid = g_euid;
        g_fsuid_ovr = -1;   // fsuid follows euid
        cred_uid_changed(); // recompute CAP_SETID after the uid transition (drop vs keepcaps)
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = 0;
        break;
    }
    // setregid(rgid,egid): symmetric to setreuid. -1 leaves an id unchanged; saved-gid moves to the new
    // egid when the real gid is changed, or the egid is set to a value other than the previous real gid.
    default: return 0;
    }
    return 1;
}

static int svc_proc_143(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 143: {
        cred_init();
        int r = (int)a0, e = (int)a1, old_rgid = g_rgid;
        if (!gid_permitted(r) || !gid_permitted(e)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_rgid = r;
        if (e != -1) g_egid = e;
        if (r != -1 || (e != -1 && e != old_rgid)) g_sgid = g_egid;
        g_fsgid_ovr = -1; // fsgid follows egid
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = 0;
        break;
    }
    // setfsuid(fsuid) / setfsgid(fsgid): set only the FS id used for ownership checks (and, here, the id
    // that STAMPS newly-created files). Linux always returns the PREVIOUS fs id and never sets errno; the
    // change is honoured only for a permitted id. == euid/egid clears the override so it tracks the creds.
    default: return 0;
    }
    return 1;
}

static int svc_proc_151(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 151: {
        cred_init();
        int prev = newfile_uid(), u = (int)a0;
        if (u != -1 && uid_permitted(u)) {
            g_fsuid_ovr = (u == g_euid) ? -1 : u;
            if (prev == 0 && u != 0)
                g_cap_eff &= ~HL_CAP_FS_MASK;
            else if (prev != 0 && u == 0)
                g_cap_eff |= g_cap_prm & HL_CAP_FS_MASK;
        }
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = (uint64_t)(uint32_t)prev;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_152(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 152: {
        cred_init();
        int prev = newfile_gid(), g = (int)a0;
        if (g != -1 && gid_permitted(g)) g_fsgid_ovr = (g == g_egid) ? -1 : g;
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = (uint64_t)(uint32_t)prev;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_148(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 148: {
        // getresuid(r,e,s) -- report the overlay so a runtime drop is observed (apt verifies all three).
        // Linux faults the whole call if any output pointer is NULL/unwritable (EFAULT), writing none.
        cred_init();
        if (!a0 || !a1 || !a2 || guest_accessible_prefix(a0, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a1, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a2, 4, HL_LOGICAL_VMA_WRITE) != 4) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ids[3] = {(uint32_t)g_ruid, (uint32_t)g_euid, (uint32_t)g_suid};
        (void)guest_copy_to(a0, ids, 4);
        (void)guest_copy_to(a1, ids + 1, 4);
        (void)guest_copy_to(a2, ids + 2, 4);
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return 1;
}

static int svc_proc_150(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 150: {
        // getresgid(r,e,s) -- report the overlay (see getresuid above). NULL/unwritable pointer -> EFAULT.
        cred_init();
        if (!a0 || !a1 || !a2 || guest_accessible_prefix(a0, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a1, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a2, 4, HL_LOGICAL_VMA_WRITE) != 4) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ids[3] = {(uint32_t)g_rgid, (uint32_t)g_egid, (uint32_t)g_sgid};
        (void)guest_copy_to(a0, ids, 4);
        (void)guest_copy_to(a1, ids + 1, 4);
        (void)guest_copy_to(a2, ids + 2, 4);
        G_RET(c) = 0;
        break;
    }
    // setpgid -- bash job control. The container init has getpid()==1 (container_pid), so bash issues
    // setpgid(0, 1); forwarded verbatim that names launchd (host pid 1) -> EPERM ("initialize_job_control:
    // setpgid: Operation not permitted"). Map the faked PID1 self-reference to the host's own process, and
    // treat a residual EPERM as success -- a container is its own session, so guest process groups are virtual.
    default: return 0;
    }
    return 1;
}

static int svc_proc_154(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 154: {
        // Map the guest's view of the init (pid/pgid 1) to its real host pid/group, then do the REAL setpgid.
        // Children already carry real host pids, so they pass straight through and get real process groups.
        // EPERM is benign (the init is a session leader, already its own group leader) -> report success.
        // Linux validates the requested pgid >= 0 first (setpgid02 case 1: pgid < 0 -> EINVAL).
        if ((int)a1 < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int host_pid, host_pgid;
        if ((int)a0 == 0)
            host_pid = 0;
        else if (hl_linux_pidmap_host_checked(&g_pidmap, (int)a0, &host_pid) != 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (!hl_linux_pidmap_is_active(&g_pidmap) && (int)a0 == 1 && g_init_hostpid) host_pid = g_init_hostpid;
        if ((int)a1 == 0)
            host_pgid = 0;
        else if (hl_linux_pidmap_host_checked(&g_pgidmap, (int)a1, &host_pgid) != 0) {
            // Creating a process group names the target process as its new leader before that group has an
            // entry of its own. Resolve that one Linux-defined creation case through the process map; every
            // other unknown group remains EPERM. This matters after restore, where guest and host pids differ.
            int guest_pid = (int)a0 != 0 ? (int)a0 : container_pid();
            if ((int)a1 != guest_pid) {
                G_RET(c) = (uint64_t)(int64_t)(-EPERM);
                break;
            }
            host_pgid = host_pid != 0 ? host_pid : (int)getpid();
        }
        if (!hl_linux_pidmap_is_active(&g_pgidmap) && (int)a1 == 1 && g_init_hostpid) host_pgid = g_init_hostpid;
        pid_t pid = (pid_t)host_pid;
        pid_t pgid = (pid_t)host_pgid;
        int guest_process = (int)a0 != 0 ? (int)a0 : container_pid();
        int guest_group = (int)a1 != 0 ? (int)a1 : guest_process;
        int r = hl_linux_pidmap_is_active(&g_pgidmap)
                    ? hl_linux_identity_registry_setpgid(&g_pidmap, &g_pgidmap, guest_process, (int32_t)pid,
                                                         guest_group, (int32_t)pgid)
                    : setpgid(pid, pgid);
        if (r == 0) {
            pid_t group_host = getpgid(pid);
            if (!hl_linux_pidmap_is_active(&g_pgidmap) && guest_group > 0 && group_host > 0)
                (void)hl_linux_pidmap_add(&g_pgidmap, guest_group, (int)group_host);
            G_RET(c) = 0;
            break;
        }
        // EPERM is benign ONLY for bash's job-control self-move into the container init's own (virtual)
        // group -- setpgid(0, 1): the init is a session leader already its own group leader, so the host
        // rejects it but the container is its own session and guest groups are virtual. Gate the swallow on
        // the guest having named group 1; a genuine EPERM (setpgid02 case 3: joining a NONEXISTENT group)
        // must propagate, along with EINVAL/ESRCH (bad pgid / target that is neither caller nor its child).
        if (errno == EPERM && (pid_t)a1 == 1 && getpid() == g_init_hostpid) {
            G_RET(c) = 0;
            break;
        }
        G_RET(c) = (uint64_t)(int64_t)(-errno);
        break;
    }
    // getpgid / getsid -- translate the init's real host group/session id to the guest's pgid 1 so the guest's
    // identity is self-consistent (getpid 1 == getpgrp 1 == getsid 1). bash then sees itself as session+group
    // leader and initializes job control WITHOUT the setpgid EPERM / "cannot set terminal process group"
    // warning -- it enables job control cleanly, and the real terminal handoff works (see TIOCSPGRP above +
    // the rt_sigprocmask stop-signal mirroring).
    default: return 0;
    }
    return 1;
}
