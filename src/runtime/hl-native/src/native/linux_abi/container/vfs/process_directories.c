static unsigned long long self_rss_bytes(void) {
    unsigned long long charged = (unsigned long long)atomic_load(&g_mem_charged);
    struct hl_procinfo process;
    unsigned long long resident = hl_get_procinfo((int)getpid(), &process) ? process.rss : 0;
    return resident > charged ? resident : charged;
}

// Host boot epoch (seconds) -- the base for /proc/<pid> starttime and /proc/uptime. Cached.
static long host_btime(void) {
    static long bt = 0;
    if (bt) return bt;
    hl_host_system_info info;
    bt = hl_host_system_read(&info, NULL, 0) && info.boot_time_seconds <= LONG_MAX ? (long)info.boot_time_seconds
                                                                                   : time(NULL);
    return bt;
}

// Aggregate host CPU jiffies (user, system, idle, nice) -- monotonically increasing, so htop/top meters move.
static void host_cpu_ticks(unsigned long long t[4]) {
    hl_host_system_info info;
    if (hl_host_system_read(&info, NULL, 0)) {
        t[0] = info.aggregate.user;
        t[1] = info.aggregate.system;
        t[2] = info.aggregate.idle;
        t[3] = info.aggregate.nice;
    } else {
        t[0] = t[1] = t[2] = t[3] = 0;
    }
}

// Real host memory picture (kB): total from hw.memsize, free/available/cached from the Mach VM stats.
static void host_mem(unsigned long long *total, unsigned long long *fre, unsigned long long *avail,
                     unsigned long long *cached) {
    hl_host_system_info info;
    *total = 0;
    if (hl_host_system_read(&info, NULL, 0)) {
        *total = info.memory_total / 1024;
        *fre = info.memory_free / 1024;
        *avail = info.memory_available / 1024;
        *cached = info.memory_cached / 1024;
    } else {
        *fre = *avail = *total / 4;
        *cached = 0;
    }
}

// Count the live container processes (registry entries whose pid is still alive).
static int proc_reg_count(void) {
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    if (!d) return 1;
    int n = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
        if (kill(atoi(e->d_name), 0) == 0 || errno != ESRCH) n++;
    }
    closedir(d);
    return n ? n : 1;
}

// /sys/fs/cgroup/cgroup.procs (and cgroup.threads) membership: the container is ONE cgroup, so this must
// list EVERY guest process -- the init AND every forked child -- not just container_pid(). The process
// registry already tracks that set cross-process (each engine process, incl. every fork child, publishes
// a file named by its host pid; see proc_reg_publish/after_fork), so enumerate it and map each host pid
// to its guest pid (init_hostpid -> 1). `with_threads` additionally appends THIS process's extra guest
// thread tids for cgroup.threads (a peer's threads aren't enumerable from here, so it lists their main
// task -- exactly like /proc/<pid>/task for a peer). Self is always included (the registry may lag our
// own just-published entry). Returns the byte length written.
static int cgroup_procs_text(char *buf, size_t n, int with_threads) {
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    int o = 0, me = (int)getpid(), have_self = 0;
    DIR *d = opendir(dir);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d)) && (size_t)o < n - 16) {
            if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
            int host = atoi(e->d_name);
            if (host <= 0) continue;
            if (host != me && kill(host, 0) != 0 && errno == ESRCH) continue; // stale registry entry
            if (host == me) have_self = 1;
            int gp = guest_pid_from_host(host);
            if (gp <= 0) continue;
            o += snprintf(buf + o, n - (size_t)o, "%d\n", gp);
        }
        closedir(d);
    }
    if (!have_self && (size_t)o < n - 16) o += snprintf(buf + o, n - (size_t)o, "%d\n", container_pid());
    if (with_threads && (size_t)o < n - 16) {
        int tids[256];
        int self_gp = container_pid();
        int nt = thread_tid_list(tids, 256, me);
        for (int i = 0; i < nt && (size_t)o < n - 16; i++)
            if (tids[i] != me && tids[i] != self_gp) // the main thread was already listed as our pid
                o += snprintf(buf + o, n - (size_t)o, "%d\n", tids[i]);
    }
    if (o == 0) o = snprintf(buf, n, "%d\n", container_pid());
    return o;
}

// /sys/fs/cgroup/memory.current aggregate across the whole container. Under a memory.max cap the
// per-process anon CHARGE is tracked (bounded, matches enforcement) -> sum the shared accounting slots.
// With no cap the charge model is inert, so fall back to the REAL resident size of every live container
// process (host process stats) -- what a native cgroup reports, and what makes a forked child's allocation visible
// to a parent reading memory.current. Cross-process either way (was a single engine process's local value).
static unsigned long long cgroup_mem_current(void) {
    if (g_mem_max) return acct_mem_total();
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    unsigned long long total = 0;
    int me = (int)getpid(), saw_self = 0;
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
            int host = atoi(e->d_name);
            if (host <= 0) continue;
            if (host == me) {
                total += self_rss_bytes();
                saw_self = 1;
                continue;
            }
            if (kill(host, 0) != 0 && errno == ESRCH) continue; // stale registry entry
            struct hl_procinfo pi;
            if (hl_get_procinfo(host, &pi)) total += pi.rss;
        }
        closedir(d);
    }
    if (!saw_self) total += self_rss_bytes(); // registry may lag our own publish
    return total;
}

// Parse "/proc/<digits>/<leaf>" for ANY pid (unlike proc_self_leaf, which matches only our own). Returns
// the <leaf> and fills *pid, or NULL.
static const char *proc_any_leaf(const char *rp, int *pid) {
    if (strncmp(rp, "/proc/", 6)) return NULL;
    const char *q = rp + 6;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9' && i < 15)
        i++;
    if (i == 0 || q[i] != '/') return NULL;
    char num[16];
    memcpy(num, q, (size_t)i);
    num[i] = 0;
    *pid = atoi(num);
    return q + i + 1;
}

// Is `host` inside OUR process tree? Walks the host ppid chain looking for this process or the container
// init. A daemonized descendant (setsid) leaves our session, so the same-session fallback below cannot see
// it: /proc/<pid>/* for a double-forked grandchild that reparented onto us read back ENOENT, and a
// supervisor comparing that ppid against getppid() saw them disagree. Bounded hops; a chain that leaves our
// tree climbs to the host init instead, so an unrelated pid is still rejected.
static int proc_pid_descendant(int host) {
    int self = (int)getpid();
    for (int hop = 0; hop < 32 && host > 1; hop++) {
        struct hl_procinfo pi;
        if (!hl_get_procinfo(host, &pi)) return 0;
        if (pi.ppid_host == self || (g_init_hostpid && pi.ppid_host == g_init_hostpid)) return 1;
        if (pi.ppid_host <= 1 || pi.ppid_host == host) return 0;
        host = pi.ppid_host;
    }
    return 0;
}

static int host_pid_registered_checked(int host);

// Is guest pid `gp` a live member of this container? Fills *hostout with its host pid (gp==1 -> init).
static int guest_pid_member_checked(int guest, int *hostout) {
    int host = (guest == 1 && g_init_hostpid) ? g_init_hostpid : guest;
    if (hl_linux_pidmap_is_active(&g_pidmap)) {
        if (hl_linux_pidmap_host_checked(&g_pidmap, guest, &host) != 0) return 0;
        *hostout = host;
        return host_pid_registered_checked(host);
    }
    *hostout = host;
    if (host == (int)getpid()) return 1;
    if (host <= 0) return 0;
    char dir[80], path[128];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, host);
    if (access(path, F_OK) == 0 && !(kill(host, 0) != 0 && errno == ESRCH)) return 1;
    if (kill(host, 0) != 0) return 0;
    // Outside restored typed mode, tolerate a lagging marker only for a process whose host ancestry proves
    // container ownership. Host sessions are shared by unrelated engines and are never membership authority.
    return proc_pid_descendant(host);
}

// Translate a host identity obtained from host process metadata into the guest namespace, then apply the
// same membership policy as a guest /proc lookup. Keeping this separate prevents a host PID that happens to
// equal a restored guest PID from being accepted through a try-both fallback.
static int host_pid_member_checked(int host, int *guestout) {
    int guest = host;
    if (hl_linux_pidmap_is_active(&g_pidmap) && hl_linux_pidmap_guest_checked(&g_pidmap, host, &guest) != 0) return 0;
    int resolved;
    if (!guest_pid_member_checked(guest, &resolved) || resolved != host) return 0;
    if (guestout) *guestout = guest;
    return 1;
}

// Does `rp` name a /proc/<pid>/... path for a pid other than this process? Such a path must never reach the
// host /proc, whether or not the pid is a container member: a bare run read the HOST's pid 1 (systemd)
// through /proc/1/{cmdline,status,stat} because the peer synthesis declined those leaves and the open fell
// through, and a MEMBER peer's host /proc describes the engine process running that guest, not the guest.
// Every leaf the peer synthesis does serve is answered before this. fs.c calls it after the /proc synth.
static int proc_pid_not_self(const char *rp) {
    if (!rp) return 0;
    int pid = 0;
    if (!proc_any_leaf(rp, &pid) || pid <= 0) return 0;
    return pid != (int)getpid() && pid != container_pid();
}

// The container's namespace magic-link target for <name> ("net" -> "net:[<inode>]"), or -1 if <name>
// is not a known namespace. A container is a SINGLE namespace set, so self and every peer process share
// one inode per namespace. The inode MUST equal the one a stat() of the same ns file reports (synth_stat
// follows the magic link to the engine's REAL host nsfs node), or lsns/nsenter -- which compare the
// readlink text against the st_ino -- see the link and the file as different namespaces. On a Linux host
// the engine process already lives in the guest's namespace set, so its own /proc/self/ns/<name> readlink
// IS that authoritative, stable "<name>:[<inode>]" string (and correctly renders pid_for_children ->
// "pid:[...]"). Read it directly; fall back to the initial-namespace constants only when the host does not
// expose it (e.g. the macOS build), keeping a well-formed link. Writes the string into `out`, returns len.
static int ns_link_target(const char *name, char *out, size_t cap) {
    static const struct {
        const char *nm;  // guest ns-dir entry name
        const char *tgt; // link target namespace name (pid_for_children -> "pid")
        unsigned ino;    // initial-namespace fallback inode
    } NS[] = {{"cgroup", "cgroup", 4026531835u},
              {"ipc", "ipc", 4026531839u},
              {"mnt", "mnt", 4026531841u},
              {"net", "net", 4026531840u},
              {"pid", "pid", 4026531836u},
              {"pid_for_children", "pid", 4026531836u},
              {"time", "time", 4026531834u},
              {"time_for_children", "time", 4026531834u},
              {"user", "user", 4026531837u},
              {"uts", "uts", 4026531838u},
              {0, 0, 0}};

    for (int i = 0; NS[i].nm; i++) {
        if (strcmp(name, NS[i].nm)) continue;
        char hp[64], link[64];
        snprintf(hp, sizeof hp, "/proc/self/ns/%s", NS[i].nm);
        ssize_t r = readlink(hp, link, sizeof link - 1);
        // Accept only a well-formed "<tgt>:[<digits>]" host answer; anything else uses the fallback so a
        // partial/odd host read never yields a malformed link.
        if (r > 0 && (size_t)r < sizeof link) {
            link[r] = 0;
            size_t tl = strlen(NS[i].tgt);
            if (!strncmp(link, NS[i].tgt, tl) && link[tl] == ':' && link[tl + 1] == '[' && link[r - 1] == ']')
                return snprintf(out, cap, "%s", link);
        }
        return snprintf(out, cap, "%s:[%u]", NS[i].tgt, NS[i].ino);
    }
    return -1;
}

// Clone/setns namespace type corresponding to a procfs namespace leaf. The two *_for_children
// links name the namespace type they would place a child into, so setns validates them as PID/TIME.
static unsigned ns_clone_flag(const char *name) {
    if (!strcmp(name, "cgroup")) return 0x02000000u;
    if (!strcmp(name, "ipc")) return 0x08000000u;
    if (!strcmp(name, "mnt")) return 0x00020000u;
    if (!strcmp(name, "net")) return 0x40000000u;
    if (!strcmp(name, "pid") || !strcmp(name, "pid_for_children")) return 0x20000000u;
    if (!strcmp(name, "time") || !strcmp(name, "time_for_children")) return 0x00000080u;
    if (!strcmp(name, "user")) return 0x10000000u;
    if (!strcmp(name, "uts")) return 0x04000000u;
    return 0;
}

// ================= guest-pid namespace (kill/pidfd host-authority containment) =================
// hl runs every guest process as a real host (macOS) process, and historically used the host pid 1:1 as
// the guest pid. That let a guest kill(2)/pidfd_send_signal an ARBITRARY same-user HOST pid -- a sibling
// engine (another container), the launcher, or any of the hl user's processes -- because the target was
// resolved straight to the host with no namespace boundary. The per-container process REGISTRY (proc_reg_*,
// keyed by HL_NETNS/HL_HOSTNAME so every engine process of one guest agrees and two guests never
// collide) is that boundary: a host pid belongs to this container iff it published a `<dir>/<hostpid>`
// record. The signal syscalls resolve the guest target to a host pid and then require membership here,
// turning "any host pid" into "only a process inside THIS container" (a non-member -> ESRCH), exactly like
// a real PID namespace. A member that is a genuine peer stays reachable, so legitimate cross-guest-process
// signalling (the case rare.c pidfd + kill(-pgid) rely on) is preserved.

// STRICT host-pid membership for the security boundary (kill/pidfd reject). Unlike the guest /proc lookup (which
// tolerates registry lag with a permissive same-session fallback for /proc DISPLAY -- too loose here, since
// sibling engines share our host session), this demands a published registry record AND a live process, so
// a pid outside the container, or a stale marker whose pid is gone, is NOT a member. Self and the container
// init are always members. Every fork publishes the child's marker in the PARENT before it returns (see
// proc_reg_mark_child), so a just-forked descendant is a member the instant its pid exists (no fork race).
static int host_pid_registered_checked(int h) {
    if (h <= 0) return 0;
    if (h == (int)getpid() || (g_init_hostpid && h == g_init_hostpid)) return 1;
    char dir[80], path[128];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, h);
    if (access(path, F_OK) != 0) return 0;       // no record in THIS container's registry -> not a member
    return !(kill(h, 0) != 0 && errno == ESRCH); // reject a stale marker whose process is already gone
}

// Resolve a GUEST pid to its container-local host pid and require membership. gp==1 -> the init. Returns 1
// and fills *hostout when gp names a process inside this container; 0 (leaving *hostout resolved) otherwise.
static int guest_pid_registered_checked(int gp, int *hostout) __attribute__((unused));

static int guest_pid_registered_checked(int gp, int *hostout) {
    int host = (gp == 1 && g_init_hostpid) ? g_init_hostpid : gp;
    if (hl_linux_pidmap_is_active(&g_pidmap) && hl_linux_pidmap_host_checked(&g_pidmap, gp, &host) != 0) return 0;
    if (hostout) *hostout = host;
    return host_pid_registered_checked(host);
}

// Publish a fresh child's membership marker from the PARENT, synchronously at fork, so the child is a
// registry member before the parent can return and signal it (the child's own proc_reg_after_fork later
// replaces this empty marker with its full comm/argv via an atomic rename). Cheap (one create); only in
// container mode. Closes the fork-window race where a strict membership check would wrongly ESRCH a
// legitimate just-forked descendant that had not yet run its own publish.
static void host_pid_register_child(int hostpid) {
    launch_reg_publish(hostpid, 0);
    if (!g_init_hostpid || hostpid <= 0) return;
    char dir[80], path[144];
    proc_reg_key(dir, sizeof dir);
    hl_compat_mkdir(dir, 0777);
    snprintf(path, sizeof path, "%s/%d", dir, hostpid);
    // EXCL: never clobber the child's real record.
    (void)hl_host_file_exclusive(&g_jit_services, path, 0644);
    {
        hl_host_process_info process;
        char birth[32];
        if (hl_host_process_read(hostpid, &process)) {
            int size = snprintf(birth, sizeof birth, "%llu\n", (unsigned long long)process.start_time_ns);
            snprintf(path, sizeof path, "%s/b%d", dir, hostpid);
            if (size > 0) (void)hl_host_file_store(&g_jit_services, path, 0600, birth, (size_t)size);
        }
    }
}

// Drop a reaped child's registry records from the PARENT at wait4/waitid time. A child that exits cleanly
// unlinks its own record, but one killed by a signal (SIGKILL) never runs that cleanup -- and a host pid
// cannot be reused until it is reaped, so removing the marker exactly at reap keeps a recycled pid from
// inheriting stale in-container membership. Idempotent (unlink of an absent path is a no-op).
static void host_pid_unregister_reaped(int hostpid) {
    char launch_dir[80], launch_path[160];
    if (hostpid > 0 && launch_reg_key(launch_dir, sizeof launch_dir)) {
        snprintf(launch_path, sizeof launch_path, "%s/b%d", launch_dir, hostpid);
        (void)unlink(launch_path);
    }
    if (!g_init_hostpid || hostpid <= 0) return;
    char dir[80], path[144];
    proc_reg_key(dir, sizeof dir);
    snprintf(path, sizeof path, "%s/%d", dir, hostpid);
    unlink(path);
    snprintf(path, sizeof path, "%s/x%d", dir, hostpid);
    unlink(path);
    snprintf(path, sizeof path, "%s/b%d", dir, hostpid);
    unlink(path);
}

// kill(0,sig) / own-process-group delivery, contained to this engine's container. Linux kill(0,sig) signals
// every process in the CALLER's process group; hl forwards setpgid to the host so the host process group
// MIRRORS the guest's, but the engine shares its host group/session with the launcher + sibling engines --
// so a raw kill(-getpgrp()) would escape the container. Instead enumerate the container registry and signal
// each MEMBER whose host process-group == want_hpgid, skipping self (the caller delivers to itself via
// raise_guest_signal). `msig` is the already-macOS-translated signo. Returns the number of peers signalled.
static int container_group_kill(int want_hpgid, int msig, int self_hpid) {
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    if (!d) return 0;
    int n = 0;
    struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] < '0' || e->d_name[0] > '9') continue; // pid records only (skip the x<pid> exe recs)
        int h = atoi(e->d_name);
        if (h <= 0 || h == self_hpid) continue;
        struct hl_procinfo pi;
        if (!hl_get_procinfo(h, &pi)) continue;   // dead/unknown host pid -> skip
        if (pi.pgid_host != want_hpgid) continue; // not in the caller's process group
        if (kill(h, msig) == 0) n++;
    }
    closedir(d);
    return n;
}

// /proc/<pid>/stat for a peer -- the 52-field line with GUEST pid/ppid and REAL rss/cpu/state/starttime.
static int proc_stat_pid_text(char *b, size_t n, int gp, int host) {
    struct hl_procinfo pi;
    int ok = hl_get_procinfo(host, &pi);
    char comm[32], cmd[4096];
    int cl;
    if (!proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl))
        snprintf(comm, sizeof comm, "%.15s", ok ? pi.hostcomm : "proc");
    char state = ok ? pi.state : 'S';
    // pbi_status can't distinguish a running task from one asleep in a blocking wait (BSD p_stat is SRUN
    // for both). Prefer the guest's own published run state when it has one; keep pbi authoritative for the
    // states it CAN report faithfully -- 'Z' (zombie, post-exit) and 'T' (SIGSTOP/traced host-suspended).
    int ov = ts_lookup(host);
    if (ov && state != 'Z' && state != 'T') state = (char)ov;
    int ppid = 0;
    if (gp != 1 && ok) {
        int hp;
        if (pi.ppid_host == g_init_hostpid)
            ppid = 1;
        else if (host_pid_member_checked(pi.ppid_host, &hp))
            ppid = hp;
    }
    int pgrp = ok ? guest_pgid_from_host(pi.pgid_host) : gp;
    if (pgrp <= 0) pgrp = gp;
    // Field 6 (session): the peer's real host session id (init's session -> guest 1), NOT its own pid. The
    // old code printed gp (the pid), so getsid() and /proc/<pid>/stat disagreed for a normal child.
    int hsid = (int)getsid(host);
    int psess = guest_sid_from_host(hsid);
    if (psess <= 0) psess = gp;
    int tty_device = 0;
    if (ok && pi.tty_host > 0)
        tty_device = (int)hl_linux_device_make(hl_host_device_major((uint64_t)pi.tty_host),
                                               hl_host_device_minor((uint64_t)pi.tty_host));
    int foreground_group = ok ? pi.tpgid_host : -1;
    if (foreground_group > 0) foreground_group = guest_pgid_from_host(foreground_group);
    if (foreground_group == 0) foreground_group = -1;
    long hz = sysconf(_SC_CLK_TCK);
    if (hz <= 0) hz = 100;
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long long utime = ok ? pi.utime_ns * (unsigned long long)hz / 1000000000ULL : 0;
    unsigned long long stime = ok ? pi.stime_ns * (unsigned long long)hz / 1000000000ULL : 0;
    unsigned long rss_pg = ok ? (unsigned long)(pi.rss / pgsz) : 0;
    // The host virtual size is the whole DBT process (code cache + big anon reservations) -> tens of GB,
    // which makes top's VSZ/%VSZ nonsensical. Report a bounded, believable footprint (rss + a modest
    // overhead) instead; there is no visibility into a PEER's true guest vsize from another process.
    unsigned long long vsize = (unsigned long long)rss_pg * pgsz + (128ULL << 20);
    long long since = ok ? (long long)pi.start_sec - host_btime() : 0;
    unsigned long long start_ticks = since > 0 ? (unsigned long long)since * (unsigned long long)hz : 0;
    int nthreads = 1; // Peer /proc/<pid>/task currently exposes one synthetic task.
    return snprintf(b, n,
                    // Field 38 (exit_signal, SIGCHLD=17) sat at 39 here -- the same one-too-many zero after
                    // field 25 that proc_stat_text carried, shifting every field from 26 up by one.
                    "%d (%s) %c %d %d %d %d %d 4194560 0 0 0 0 %llu %llu 0 0 20 0 %d 0 %llu %llu %lu "
                    "18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
                    gp, comm, state, ppid, pgrp, psess, tty_device, foreground_group, utime, stime, nthreads,
                    start_ticks, vsize, rss_pg);
}

// /proc/<pid>/status for a peer -- the key:value form with GUEST Pid/PPid and REAL VmRSS.
static int proc_status_pid_text(char *b, size_t n, int gp, int host) {
    struct hl_procinfo pi;
    int ok = hl_get_procinfo(host, &pi);
    char comm[32], cmd[4096];
    int cl;
    if (!proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl))
        snprintf(comm, sizeof comm, "%.15s", ok ? pi.hostcomm : "proc");
    int ppid = 0;
    if (gp != 1 && ok) {
        int hp;
        if (pi.ppid_host == g_init_hostpid)
            ppid = 1;
        else if (host_pid_member_checked(pi.ppid_host, &hp))
            ppid = hp;
    }
    unsigned long rss = ok ? (unsigned long)(pi.rss / 1024) : 0;
    unsigned long vsz = rss + (128UL << 10); // bounded footprint, not the huge host DBT vsize (see stat text)
    char state = ok ? pi.state : 'S';        // same run-state override as proc_stat_pid_text (see there)
    int ov = ts_lookup(host);
    if (ov && state != 'Z' && state != 'T') state = (char)ov;
    const char *state_name = "unknown";
    switch (state) {
    case 'R': state_name = "running"; break;
    case 'S': state_name = "sleeping"; break;
    case 'D': state_name = "disk sleep"; break;
    case 'T': state_name = "stopped"; break;
    case 'Z': state_name = "zombie"; break;
    default: break;
    }
    char groups[512]; // peers carry the same container supplementary set (image-derived, see self)
    groups_status_str(groups, sizeof groups);
    char cpumask[40], cpulist[24];
    cpus_allowed_strs(cpumask, sizeof cpumask, cpulist, sizeof cpulist);
    return snprintf(
        b, n,
        "Name:\t%s\nUmask:\t0022\nState:\t%c (%s)\nTgid:\t%d\nNgid:\t0\nPid:\t%d\nPPid:\t%d\n"
        "TracerPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nFDSize:\t256\nGroups:\t%s\n"
        "VmPeak:\t%8lu kB\nVmSize:\t%8lu kB\nVmLck:\t       0 kB\nVmHWM:\t%8lu kB\nVmRSS:\t%8lu kB\n"
        "VmData:\t%8lu kB\nVmStk:\t     132 kB\nVmExe:\t     512 kB\nVmLib:\t    2048 kB\nVmPTE:\t      32 kB\n"
        "VmSwap:\t       0 kB\nThreads:\t%d\nSigQ:\t0/31000\nSigPnd:\t0000000000000000\n"
        "SigBlk:\t0000000000000000\nSigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
        // Peer processes carry the same docker default cap set (see proc_status_text). We don't
        // track a peer's live effective/nnp, so report the container default.
        "CapInh:\t0000000000000000\nCapPrm:\t%016llx\nCapEff:\t%016llx\nCapBnd:\t%016llx\n"
        "CapAmb:\t0000000000000000\nNoNewPrivs:\t0\nSeccomp:\t2\nSeccomp_filters:\t1\n"
        "Speculation_Store_Bypass:\tvulnerable\nSpeculationIndirectBranch:\tunknown\n"
        "Cpus_allowed:\t%s\nCpus_allowed_list:\t%s\nvoluntary_ctxt_switches:\t1\n"
        "nonvoluntary_ctxt_switches:\t0\n",
        comm, state, state_name, gp, gp, ppid, groups, vsz, vsz, rss, rss, rss, 1, (unsigned long long)HL_CAP_DEFAULT,
        (unsigned long long)HL_CAP_DEFAULT, (unsigned long long)HL_CAP_DEFAULT, cpumask, cpulist);
}

// /proc/<pid>/cmdline for a peer -- the published NUL-separated argv (fallback: the comm).
static int proc_cmdline_pid_text(char *b, size_t n, int host) {
    char comm[32], cmd[4096];
    int cl;
    if (proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl) && cl > 0) {
        int L = cl > (int)n ? (int)n : cl;
        memcpy(b, cmd, (size_t)L);
        if (L == 0 || b[L - 1] != 0) {
            if (L < (int)n)
                b[L++] = 0;
            else
                b[L - 1] = 0;
        }
        return L;
    }
    struct hl_procinfo pi;
    const char *c = hl_get_procinfo(host, &pi) ? pi.hostcomm : "proc";
    int L = (int)strlen(c);
    if (L + 1 > (int)n) L = (int)n - 1;
    memcpy(b, c, (size_t)L);
    b[L] = 0;
    return L + 1;
}

// /proc/<pid>/comm for a peer.
static int proc_comm_pid_text(char *b, size_t n, int host) {
    char comm[32], cmd[4096];
    int cl;
    if (!proc_reg_read(host, comm, sizeof comm, cmd, sizeof cmd, &cl)) {
        struct hl_procinfo pi;
        snprintf(comm, sizeof comm, "%.15s", hl_get_procinfo(host, &pi) ? pi.hostcomm : "proc");
    }
    return snprintf(b, n, "%s\n", comm);
}

// /proc/[pid]/statm -- the 7-field page-count line (size resident shared text lib data dt). htop's
// MEM% column reads `resident` from HERE (not status VmRSS), so it must be present and non-zero.
static int proc_statm_common(char *b, size_t n, unsigned long size_pg, unsigned long rss_pg) {
    return snprintf(b, n, "%lu %lu %lu 1 0 %lu 0\n", size_pg, rss_pg, rss_pg / 2, size_pg);
}

static int proc_statm_text(char *b, size_t n) { // our own pid
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long long vm_rss, vm_vsize;
    self_vm_statm_bytes(&vm_rss, &vm_vsize);
    unsigned long rss_pg = (unsigned long)(vm_rss / pgsz);
    unsigned long size_pg = (unsigned long)(vm_vsize / pgsz);
    if (size_pg < rss_pg) size_pg = rss_pg;
    return proc_statm_common(b, n, size_pg, rss_pg);
}

static int proc_statm_pid_text(char *b, size_t n, int host) { // a peer -- real host-backed RSS
    struct hl_procinfo pi;
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long rss_pg = hl_get_procinfo(host, &pi) ? (unsigned long)(pi.rss / pgsz) : 0;
    unsigned long overhead_pg = (unsigned long)((128ULL << 20) / pgsz);
    return proc_statm_common(b, n, rss_pg + overhead_pg, rss_pg);
}

// Register a materialized proc temp dir (fd + host temp path for reaping) AND tag the fd's GUEST /proc
// path in g_fdpath. The tag is the key trick: a RELATIVE openat/readlink against this dir fd (htop uses
// openat(pid_dirfd,"stat"/"task"/...) exclusively) then resolves via abs_guest back to the /proc path,
// so it re-enters this same synthesis instead of hitting the real (empty) temp entry. abs_guest strips
// g_rootfs_canon, so we store "<canon><guestpath>".
static void proc_dir_register(int fd, const char *tmpl, const char *guestpath) {
    for (int i = 0; i < 64; i++)
        if (!g_procfd_dirs[i].path[0]) {
            g_procfd_dirs[i].fd = fd;
            snprintf(g_procfd_dirs[i].path, sizeof g_procfd_dirs[i].path, "%s", tmpl);
            break;
        }
    if (fd >= 0 && fd < 1024 && path_concat(g_fdpath[fd], sizeof g_fdpath[fd], g_rootfs_canon, guestpath) != 0)
        g_fdpath[fd][0] = 0; // unrepresentable tags fail closed in relative-atpath handling
}

// Materialize a /proc/<gp> (or task/<tid>) directory as a temp dir of placeholder entries so
// opendir/getdents works and htop can descend; the CONTENT of each entry is served live on the
// (re-intercepted) relative open by proc_open. `guestpath` is the /proc path this dir represents;
// with_task adds the "task" subdir entry (omitted for a task/<tid> dir, which never nests another).
static int proc_leaf_dir_open(const char *guestpath, int with_task) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-proc-pidXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    // The per-pid file set. Direct open/stat serve every name here (proc_open), so listing them makes
    // readdir-based discovery agree with direct probing (mountinfo/limits/environ/smaps/pagemap/io were
    // openable but hidden from `ls /proc/self`).
    static const char *const files[] = {"stat",          "statm",        "status",     "cmdline",   "comm",   "maps",
                                        "oom_score_adj", "oom_adj",      "oom_score",  "mountinfo", "limits", "environ",
                                        "smaps",         "pagemap",      "io",         "mounts",    "cgroup", "auxv",
                                        "numa_maps",     "smaps_rollup", "mountstats", "syscall",   0};
    for (int i = 0; files[i]; i++) {
        char p[64];
        snprintf(p, sizeof p, "%s/%s", tmpl, files[i]);
        int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (f >= 0) close(f);
    }
    if (with_task) {
        char p[64];
        snprintf(p, sizeof p, "%s/task", tmpl);
        hl_compat_mkdir(p, 0555);
        snprintf(p, sizeof p, "%s/fd", tmpl);
        hl_compat_mkdir(p, 0555); // placeholder: an open of /proc/<pid>/fd re-enters the synthesis (proc_fd_dir_open)
        snprintf(p, sizeof p, "%s/map_files", tmpl);
        hl_compat_mkdir(p, 0555); // ditto -> proc_map_files_dir_open
    }
    // Magic-link placeholders (exe/cwd/root) so getdents lists them with d_type DT_LNK, like Linux. Every
    // ACCESS to them goes by path or by (tagged dirfd, relative) and is intercepted -- readlink/stat/open
    // of /proc/<pid>/{exe,cwd,root} are served by proc_self_exe / the root|cwd synthesis in fs.c;
    // the inert "." target exists only so a host-side follow can never dangle out of the temp dir.
    static const char *const links[] = {"exe", "cwd", "root", 0};
    for (int i = 0; links[i]; i++) {
        char p[64];
        snprintf(p, sizeof p, "%s/%s", tmpl, links[i]);
        if (symlink_idempotent(".", p) != 0) {
            procfd_dir_rm(tmpl);
            return -1;
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, guestpath);
    return fd;
}

// Materialize /proc/<gp>/task -- a dir whose sole entry is the main thread tid (== gp for the common
// single-threaded case; enough for htop to count the process). Returns the fd or -1.
static int proc_task_dir_open(int gp) {
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-proc-taskXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    char p[64];
    snprintf(p, sizeof p, "%s/%d", tmpl, gp);
    hl_compat_mkdir(p, 0555); // the main thread tid (== pid)
    // For OUR OWN process, enumerate every live guest thread's tid so a /proc/self/task walk sees them all
    // (thread enumerators, profilers, debuggers). Peer processes keep just the main entry (no cross-process
    // thread registry yet).
    if (gp == (int)getpid() || gp == container_pid()) {
        int tids[256];
        int nt = thread_tid_list(tids, 256, gp);
        for (int i = 0; i < nt; i++) {
            if (tids[i] == gp) continue; // main already created
            snprintf(p, sizeof p, "%s/%d", tmpl, tids[i]);
            hl_compat_mkdir(p, 0555);
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    char gpath[48];
    snprintf(gpath, sizeof gpath, "/proc/%d/task", gp);
    proc_dir_register(fd, tmpl, gpath);
    return fd;
}

// Rewrite a leading /proc/self/ or /proc/thread-self/ (WITH a tail) to /proc/<our-pid>/ so the
// numeric-pid synth (proc_dir_try_open, the synth_stat task-dir block) resolves the CALLER's own
// subtrees -- e.g. /proc/self/task, /proc/self/task/<tid>. Bare /proc/self (the magic symlink) is
// left untouched (it stays a symlink). Returns `out` on rewrite, else the original `rp` unchanged.
static const char *proc_deself(const char *rp, char *out, size_t osz) {
    if (!rp) return rp;
    const char *tail = NULL;
    if (!strncmp(rp, "/proc/self/", 11))
        tail = rp + 10; // keep the leading '/'
    else if (!strncmp(rp, "/proc/thread-self/", 18))
        tail = rp + 17;
    if (!tail) return rp;
    snprintf(out, osz, "/proc/%d%s", container_pid(), tail);
    return out;
}

static int proc_task_tid_visible(int pid, int tid) {
    if (tid <= 0) return 0;
    int is_self = (pid == (int)getpid() || pid == container_pid());
    if (is_self) return tid == pid || thread_tid_alive(tid);
    return tid == pid; // Peer thread registry is not cross-process yet.
}

// If `rp` is a /proc/<pid> DIRECTORY path (the pid dir, its task/ dir, or a task/<tid>/ dir) for a live
// container pid, materialize it and return the fd. Returns -1 on error, or -2 if `rp` is not such a
// directory (a per-pid FILE like stat/status -> the caller falls through to proc_open). fs.c calls this.
static int proc_dir_try_open(const char *rp) {
    char dsb[4200];
    rp = proc_deself(rp, dsb, sizeof dsb); // /proc/self/task -> /proc/<cpid>/task
    if (!rp || strncmp(rp, "/proc/", 6)) return -2;
    const char *q = rp + 6;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9' && i < 15)
        i++;
    if (i == 0) return -2;
    char num[16];
    memcpy(num, q, (size_t)i);
    num[i] = 0;
    int pid = atoi(num), host;
    if (pid != (int)getpid() && pid != container_pid() && pid != 1 && !guest_pid_member_checked(pid, &host)) return -2;
    const char *rest = q + i; // "" | "/task" | "/task/<tid>" | "/task/<tid>/<leaf>" | "/<leaf>"
    if (rest[0] == 0 || (rest[0] == '/' && rest[1] == 0)) {
        char gpath[32];
        snprintf(gpath, sizeof gpath, "/proc/%d", pid);
        return proc_leaf_dir_open(gpath, 1);
    }
    if (!strncmp(rest, "/task", 5) && (rest[5] == 0 || (rest[5] == '/' && rest[6] == 0)))
        return proc_task_dir_open(pid);
    // map_files/ for OUR OWN pid: one "<start>-<end>" symlink per file-backed VMA. A peer's is left
    // unsynthesized rather than passed through -- the host directory is the ENGINE's mapping list.
    if (!strncmp(rest, "/map_files", 10) && (rest[10] == 0 || (rest[10] == '/' && rest[11] == 0)))
        return (pid == (int)getpid() || pid == container_pid()) ? proc_map_files_dir_open() : -1;
    if (!strncmp(rest, "/task/", 6)) {
        const char *t = rest + 6;
        int j = 0;
        while (t[j] >= '0' && t[j] <= '9')
            j++;
        if (j > 0 && (t[j] == 0 || (t[j] == '/' && t[j + 1] == 0))) {
            int tid = atoi(t);
            if (!proc_task_tid_visible(pid, tid)) return -2;
            char gpath[48];
            snprintf(gpath, sizeof gpath, "/proc/%d/task/%d", pid, tid);
            return proc_leaf_dir_open(gpath, 0);
        }
    }
    return -2; // a per-pid FILE -> proc_open serves it
}

// Materialize /proc as a real temp directory of entries (static files + one numeric name per live
// container process) so the guest's ordinary opendir/getdents enumerates it. Entries are empty regular
// files -- ps/top/htop identify pids by digit-name and then open /proc/<pid>/stat BY PATH (served by
// proc_open), so the entry type is irrelevant; empty files keep cleanup trivial (procfd_dir_rm). The
// dir is reaped when the guest closes the fd (shared g_procfd_dirs machinery). Returns the fd or -1.
static int proc_root_dir_open(void) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-proc-rootXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    // ONLY names proc_open()/synth_stat actually serve -- listing an unserved name makes `ls /proc` stat it
    // and print "No such file or directory". "self" is the magic symlink (handled in synth_stat).
    static const char *const st[] = {"meminfo", "stat",   "cpuinfo", "uptime",  "loadavg",
                                     "version", "mounts", "self",    "cmdline", "filesystems",
                                     "swaps",   "vmstat", "modules", "devices", 0};
    for (int i = 0; st[i]; i++) {
        char p[96];
        snprintf(p, sizeof p, "%s/%s", tmpl, st[i]);
        int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
        if (f >= 0) close(f);
    }
    char dir[80];
    proc_reg_key(dir, sizeof dir);
    DIR *d = opendir(dir);
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] < '0' || e->d_name[0] > '9') continue;
            int host = atoi(e->d_name);
            if (kill(host, 0) != 0 && errno == ESRCH) { // dead -> prune the stale registry record
                char rp[352];
                if (path_join(rp, sizeof rp, dir, e->d_name) == 0) unlink(rp);
                continue;
            }
            int guest = guest_pid_from_host(host);
            if (guest <= 0) continue;
            char p[96];
            snprintf(p, sizeof p, "%s/%d", tmpl, guest);
            hl_compat_mkdir(p, 0555); // a real (empty) subdir: getdents reports DT_DIR, and htop opens /proc/<pid>
        }
        closedir(d);
    }
    { // always list ourselves (our registry write may have lagged the first `ps`)
        char p[96];
        snprintf(p, sizeof p, "%s/%d", tmpl, container_pid());
        hl_compat_mkdir(p, 0555);
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, "/proc"); // tag the fd's guest path so relative opens re-enter /proc synth
    return fd;
}

// materialize a /sys/class/net directory as a real temp dir the guest's opendir/getdents can
// walk. The class dir lists the two interfaces (lo, eth0) as subdirs; an interface dir lists its
// attribute files. FILE content is served live via proc_open on the (re-intercepted) relative/absolute
// open. Returns the fd, -1 on error, or -2 if `gp` is not a sysfs-net directory we synthesize.
static int sysnet_hidden(const char *gp) {
    static const char prefix[] = "/sys/class/net/eth0";
    return net_isolate() && gp != NULL && strncmp(gp, prefix, sizeof(prefix) - 1) == 0 &&
           (gp[sizeof(prefix) - 1] == 0 || gp[sizeof(prefix) - 1] == '/');
}

static int sysnet_dir_open(const char *gp) {
    if (!gp || strncmp(gp, "/sys/class/net", 14)) return -2;
    const char *r = gp + 14;
    const char *const *entries;
    // --network none: loopback-only, so /sys/class/net lists just `lo` (no eth0).
    static const char *const ifaces[] = {"lo", "eth0", 0};
    static const char *const ifaces_lo[] = {"lo", 0};
    static const char *const attrs[] = {
        "address", "addr_len", "broadcast",    "flags", "mtu",    "operstate",       "type",       "carrier",
        "ifindex", "iflink",   "tx_queue_len", "speed", "duplex", "carrier_changes", "statistics", 0};
    // per-net_device statistics counters (fixed kernel set) node_exporter/ifstat read directly from sysfs.
    static const char *const stats[] = {
        "collisions",       "multicast",           "rx_bytes",       "rx_compressed",    "rx_crc_errors",
        "rx_dropped",       "rx_errors",           "rx_fifo_errors", "rx_frame_errors",  "rx_length_errors",
        "rx_missed_errors", "rx_nohandler",        "rx_over_errors", "rx_packets",       "tx_aborted_errors",
        "tx_bytes",         "tx_carrier_errors",   "tx_compressed",  "tx_dropped",       "tx_errors",
        "tx_fifo_errors",   "tx_heartbeat_errors", "tx_packets",     "tx_window_errors", 0};
    int as_dirs; // class dir -> iface subdirs; iface dir -> attribute files
    if (r[0] == 0 || (r[0] == '/' && r[1] == 0)) {
        entries = net_isolate() ? ifaces_lo : ifaces;
        as_dirs = 1;
    } else if (r[0] == '/' && (!strcmp(r + 1, "lo") || (!net_isolate() && !strcmp(r + 1, "eth0")))) {
        entries = attrs;
        as_dirs = 0;
    } else if (r[0] == '/' &&
               (!strcmp(r + 1, "lo/statistics") || (!net_isolate() && !strcmp(r + 1, "eth0/statistics")))) {
        entries = stats; // the statistics/ subdir: one counter file per entry
        as_dirs = 0;
    } else
        return -2;
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-netXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int i = 0; entries[i]; i++) {
        char p[96];
        snprintf(p, sizeof p, "%s/%s", tmpl, entries[i]);
        if (as_dirs || !strcmp(entries[i], "statistics")) // statistics/ is a subdir even within an iface dir
            hl_compat_mkdir(p, 0555);
        else {
            int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
            if (f >= 0) close(f);
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    char gpath[64];
    snprintf(gpath, sizeof gpath, "/sys/class/net%s", (r[0] == '/') ? r : "");
    proc_dir_register(fd, tmpl, gpath); // tag guest path so a relative reopen re-enters this synth
    return fd;
}

// materialize the CPU-topology sysfs DIRECTORY so getdents enumerates one cpuN subdir per online
// CPU. htop's LinuxMachine_updateCPUcount opendir()s /sys/devices/system/cpu, counts the cpuN subdirs
// (reading each cpuN/online to mark it active), and -- crucially -- when it finds NO cpuN dir it early-
// returns keeping its built-in default of ONE CPU. macOS has no /sys, and hl previously served only the
// online/possible/present FILES (absolute-path reads), never the directory, so htop's opendir hit the
// (missing) host /sys and htop showed 1 CPU on a many-core host. glibc __get_nprocs_conf and tcmalloc
// NumPossibleCPUs likewise count these cpuN dirs. Two shapes:
//   - base "/sys/devices/system/cpu": a temp dir holding cpu0..cpu(N-1) as real SUBDIRS (htop only
//     accepts DT_DIR/DT_UNKNOWN entries) plus the online/possible/present placeholder files (so a plain
//     readdir sees them too -- their CONTENT is still served by the absolute-path synth in fs.c).
//   - a "/sys/devices/system/cpu/cpuN" leaf: an EMPTY temp dir. htop opens it O_DIRECTORY|O_PATH and then
//     openat(cpuN,"online") -> ENOENT (res<1) which htop counts as active -- exactly the real-Linux shape
//     (cpuN has no per-cpu `online` file). The dir must OPEN successfully or htop `continue`s past the CPU.
// Returns the fd, -1 on error, or -2 if `gp` is not the cpu-topology dir / a cpuN subdir we synthesize.
static int syscpu_dir_open(const char *gp) {
    if (!gp || strncmp(gp, "/sys/devices/system/cpu", 23)) return -2;
    const char *r = gp + 23;
    int is_base = (r[0] == 0 || (r[0] == '/' && r[1] == 0));
    int cpuN = -1;
    if (!is_base) {
        if (r[0] != '/' || strncmp(r + 1, "cpu", 3)) return -2; // not a /sys/devices/system/cpu/cpuN leaf
        const char *d = r + 4;
        if (*d < '0' || *d > '9') return -2;
        cpuN = 0;
        for (; *d >= '0' && *d <= '9'; d++)
            cpuN = cpuN * 10 + (*d - '0');
        if (*d != 0) return -2; // trailing junk (cpufreq/cpuidle/... are files/dirs, not our cpuN synth)
    }
    int nc = container_online_cpus();                    // host online count, docker --cpus capped (state.c)
    if (!is_base && (cpuN < 0 || cpuN >= nc)) return -2; // an out-of-range cpuN: not one we advertise
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-cpu-dirXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    char gpath[48];
    if (is_base) {
        for (int i = 0; i < nc; i++) {
            char p[96];
            snprintf(p, sizeof p, "%s/cpu%d", tmpl, i);
            hl_compat_mkdir(p, 0555); // real SUBDIR: getdents reports DT_DIR so htop counts it
        }
        static const char *const files[] = {"online", "possible", "present", "offline", 0};
        for (int i = 0; files[i]; i++) {
            char p[96];
            snprintf(p, sizeof p, "%s/%s", tmpl, files[i]);
            int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
            if (f >= 0) close(f); // content served on the absolute-path open (fs.c), not from this placeholder
        }
        snprintf(gpath, sizeof gpath, "/sys/devices/system/cpu");
    } else {
        snprintf(gpath, sizeof gpath, "/sys/devices/system/cpu/cpu%d", cpuN); // empty dir (no `online` leaf)
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, gpath); // tag guest path so a relative openat(cpuN)/readfileat re-enters synth
    return fd;
}

// Materialize an arbitrary synthetic directory as a temp dir of placeholder entries so opendir/getdents
// enumerate `names`; the CONTENT/target of each entry is served live on the (re-intercepted) open /
// readlink by proc_open / the fs.c readlink synth. kind: 0 = regular-file placeholders, 1 = symlink
// placeholders (namespace/fd magic links), 2 = subdir placeholders. `guestpath` tags the fd so a relative
// reopen re-enters the synth. Returns the fd, or -1 on error.
static int synth_names_dir_open(const char *guestpath, const char *const *names, int kind) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-sys-dirXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    for (int i = 0; names[i]; i++) {
        char p[160];
        snprintf(p, sizeof p, "%s/%s", tmpl, names[i]);
        if (kind == 2)
            hl_compat_mkdir(p, 0555);
        else if (kind == 1) {
            if (symlink_idempotent(".", p) != 0) {
                procfd_dir_rm(tmpl);
                return -1;
            }
        } else {
            int f = open(p, O_WRONLY | O_CREAT | O_TRUNC, 0444);
            if (f >= 0) close(f);
        }
    }
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, guestpath);
    return fd;
}

// If `gp` is one of the synthetic non-pid directories we enumerate (/proc/net, /proc/[self|pid]/ns,
// /sys/fs/cgroup, /sys/class/block, /sys/block, a cpuN/topology dir), materialize + return its fd; -2 if
// `gp` is not such a directory (caller falls through). Peer/self ns share the same name set.
// Predicate form (no materialization side effect): is `gp` one of the synthetic directories above? Used by
// synth_stat so a tool that stats the dir before opening it sees it as present.
static int synth_misc_dir_is(const char *gp) {
    if (!gp) return 0;
    if (!strcmp(gp, "/proc/net") || !strcmp(gp, "/proc/net/")) return 1;
    if (!strcmp(gp, "/proc/tty") || !strcmp(gp, "/proc/tty/")) return 1;
    if (!strcmp(gp, "/sys/fs/cgroup") || !strcmp(gp, "/sys/fs/cgroup/")) return 1;
    if (!strcmp(gp, "/sys/class/block") || !strcmp(gp, "/sys/class/block/")) return 1;
    if (!strcmp(gp, "/sys/block") || !strcmp(gp, "/sys/block/")) return 1;
    {
        char dsb[4200];
        const char *rp = proc_deself(gp, dsb, sizeof dsb);
        const char *q = rp && !strncmp(rp, "/proc/", 6) ? rp + 6 : NULL;
        if (q) {
            int i = 0;
            while (q[i] >= '0' && q[i] <= '9')
                i++;
            if (i > 0 && (!strcmp(q + i, "/ns") || !strcmp(q + i, "/ns/"))) return 1;
            if (i > 0 && (!strcmp(q + i, "/fdinfo") || !strcmp(q + i, "/fdinfo/"))) return 1;
        }
    }
    if (!strncmp(gp, "/sys/devices/system/cpu/cpu", 27)) {
        const char *d = gp + 27;
        if (*d >= '0' && *d <= '9') {
            while (*d >= '0' && *d <= '9')
                d++;
            if (!strcmp(d, "/topology") || !strcmp(d, "/topology/")) return 1;
        }
    }
    return 0;
}

static int synth_proc_fd_dir_is(const char *gp) {
    if (!gp) return 0;
    char dsb[4200];
    const char *rp = proc_deself(gp, dsb, sizeof dsb);
    const char *q = rp && !strncmp(rp, "/proc/", 6) ? rp + 6 : NULL;
    if (!q) return 0;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9')
        i++;
    if (!i) return 0;
    return !strcmp(q + i, "/fd") || !strcmp(q + i, "/fd/") || !strcmp(q + i, "/fdinfo") || !strcmp(q + i, "/fdinfo/");
}

static int synth_misc_dir_open(const char *gp) {
    if (!gp) return -2;
    if (!strcmp(gp, "/dev/fd") || !strcmp(gp, "/dev/fd/")) return proc_fd_dir_open(); // /dev/fd == /proc/self/fd
    // /proc/net: direct leaves (tcp/dev/unix/…) exist but the dir must enumerate them too.
    if (!strcmp(gp, "/proc/net") || !strcmp(gp, "/proc/net/")) {
        static const char *const net[] = {"tcp",       "tcp6",       "udp",  "udp6",  "unix",    "dev",
                                          "route",     "if_inet6",   "snmp", "snmp6", "netstat", "sockstat",
                                          "sockstat6", "ipv6_route", "arp",  "igmp",  0};
        return synth_names_dir_open("/proc/net", net, 0);
    }
    // /proc/tty: tty discovery tools (agetty, `ls /proc/tty`) walk this before reading drivers.
    if (!strcmp(gp, "/proc/tty") || !strcmp(gp, "/proc/tty/")) {
        static const char *const tty[] = {"drivers", "ldiscs", 0};
        return synth_names_dir_open("/proc/tty", tty, 0);
    }
    // /proc/[self|<pid>]/ns: enumerate the namespace magic links (readlink served in fs.c).
    {
        char dsb[4200];
        const char *rp = proc_deself(gp, dsb, sizeof dsb);
        const char *q = rp && !strncmp(rp, "/proc/", 6) ? rp + 6 : NULL;
        if (q) {
            int i = 0;
            while (q[i] >= '0' && q[i] <= '9')
                i++;
            if (i > 0 && (!strcmp(q + i, "/fd") || !strcmp(q + i, "/fd/"))) {
                int guest = atoi(q);
                return guest == (int)getpid() ? proc_fd_dir_open() : proc_fd_dir_pid_open(guest, guest);
            }
            if (i > 0 && (!strcmp(q + i, "/ns") || !strcmp(q + i, "/ns/"))) {
                static const char *const ns[] = {
                    "cgroup", "ipc", "mnt", "net", "pid", "pid_for_children", "time", "time_for_children",
                    "user",   "uts", 0};
                return synth_names_dir_open(rp, ns, 1);
            }
            if (i > 0 && (!strcmp(q + i, "/fdinfo") || !strcmp(q + i, "/fdinfo/"))) return proc_fdinfo_dir_open(rp);
        }
    }
    // /sys/fs/cgroup root: advertised in mountinfo, so a directory walk of the hierarchy must list it.
    if (!strcmp(gp, "/sys/fs/cgroup") || !strcmp(gp, "/sys/fs/cgroup/")) {
        static const char *const cg[] = {"cgroup.controllers",
                                         "cgroup.subtree_control",
                                         "cgroup.type",
                                         "cgroup.procs",
                                         "cgroup.threads",
                                         "cgroup.events",
                                         "cgroup.stat",
                                         "cgroup.max.depth",
                                         "cgroup.max.descendants",
                                         "cpu.max",
                                         "cpu.stat",
                                         "cpu.weight",
                                         "cpuset.cpus",
                                         "cpuset.mems",
                                         "cpuset.cpus.effective",
                                         "cpuset.mems.effective",
                                         "memory.max",
                                         "memory.min",
                                         "memory.low",
                                         "memory.high",
                                         "memory.current",
                                         "memory.peak",
                                         "memory.events",
                                         "memory.stat",
                                         "memory.swap.max",
                                         "memory.swap.current",
                                         "memory.oom.group",
                                         "pids.max",
                                         "pids.current",
                                         "pids.peak",
                                         "pids.events",
                                         "io.max",
                                         "io.stat",
                                         "io.weight",
                                         0};
        return synth_names_dir_open("/sys/fs/cgroup", cg, 0);
    }
    // /sys/class/block + /sys/block: storage sysfs (lsblk/installers). No real block devices are backed,
    // but the directories must EXIST and be enumerable (Linux exposes them inside containers).
    if (!strcmp(gp, "/sys/class/block") || !strcmp(gp, "/sys/class/block/") || !strcmp(gp, "/sys/block") ||
        !strcmp(gp, "/sys/block/")) {
        static const char *const empty[] = {0};
        return synth_names_dir_open(gp, empty, 2);
    }
    // /sys/devices/system/cpu/cpuN/topology: lscpu enumerates this dir before opening the leaves.
    if (!strncmp(gp, "/sys/devices/system/cpu/cpu", 27)) {
        const char *d = gp + 27;
        if (*d >= '0' && *d <= '9') {
            while (*d >= '0' && *d <= '9')
                d++;
            if (!strcmp(d, "/topology") || !strcmp(d, "/topology/")) {
                static const char *const topo[] = {"core_id",
                                                   "physical_package_id",
                                                   "cluster_id",
                                                   "thread_siblings",
                                                   "thread_siblings_list",
                                                   "core_siblings",
                                                   "core_siblings_list",
                                                   "core_cpus",
                                                   "core_cpus_list",
                                                   "package_cpus",
                                                   "package_cpus_list",
                                                   0};
                return synth_names_dir_open(gp, topo, 0);
            }
        }
    }
    return -2;
}

// Format a Linux cpumask hex string (as /sys topology mask files print it): zero-padded groups of up to 32
// bits, most-significant group first, comma-separated. `all` -> every online CPU set; else just bit `bit`.
// `ndig` is the low-group width the kernel pads to for this machine (DIV_ROUND_UP(nc,4)); e.g. nc=18 -> 5.
